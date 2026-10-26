#!/usr/bin/env python3
"""Audit native CMO3/CAN3 previews without redistributing the input assets.

The report distinguishes parsing, numeric timeline checks and approximate rendering.
It does not certify equivalence to the Cubism renderer. Identical inputs are tested
once by SHA-256; every original path remains in the report.
"""
import argparse
import base64
from collections import Counter
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools' / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


CORE = module('native_core_checks', 'verify-rust-core.py')
CAN = module('native_can_checks', 'verify-can3-source.py')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compatible_models(path, inputs, by_path, models, identities, part_identities=frozenset()):
    archive = path.parent.relative_to(inputs).parts[:2]
    return [candidate for candidate in by_path if candidate.suffix.lower() == '.cmo3'
        and candidate.parent.relative_to(inputs).parts[:2] == archive
        and by_path[candidate] in models
        and identities <= {(p['guid'], p['id']) for p in models[by_path[candidate]]['parameters']}
        and part_identities <= {(p['guid'], p['id']) for p in models[by_path[candidate]].get('sources', [])
            if p['data']['type'] == 'Part'}]


class Rpc:
    def __init__(self, binary, timeout, mcp=False):
        self.process = subprocess.Popen([str(binary)] + (['--mcp'] if mcp else []),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.lines = queue.Queue()
        self.timeout, self.mcp, self.sequence = timeout, mcp, 0

        def read():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(None)

        threading.Thread(target=read, daemon=True).start()
        if mcp:
            self.wire({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {
                'protocolVersion': '2024-11-05', 'capabilities': {},
                'clientInfo': {'name': 'native-playback-verifier', 'version': '1'}}})

    def wire(self, request):
        self.process.stdin.write(json.dumps(request, allow_nan=False) + '\n')
        self.process.stdin.flush()
        try:
            line = self.lines.get(timeout=self.timeout)
        except queue.Empty:
            self.close()
            raise RuntimeError('core request timed out') from None
        if line is None:
            raise RuntimeError('core exited before replying')
        return json.loads(line)

    def call(self, op, **arguments):
        if self.mcp:
            self.sequence += 1
            reply = self.wire({'jsonrpc': '2.0', 'id': self.sequence, 'method': 'tools/call',
                'params': {'name': op.replace('.', '_'), 'arguments': arguments}})
            result = reply.get('result', {})
            if reply.get('error') or result.get('isError'):
                raise RuntimeError(str(reply.get('error') or result.get('content')))
            value = result.get('structuredContent') or json.loads(result['content'][0]['text'])
            for item in result.get('content', []):
                if item.get('type') == 'image' and item.get('mimeType') == 'image/png':
                    value['png'] = item['data']
            return value
        reply = self.wire({'op': op, **arguments})
        if not reply.get('succeeded'):
            raise RuntimeError(reply.get('message', str(reply)))
        return reply['result']

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()


def cubic(a, b, c, d, t):
    return (1-t)**3*a + 3*(1-t)**2*t*b + 3*(1-t)*t*t*c + t**3*d


def sample_curve(curve, frame):
    pairs = sorted(zip(curve['points'], curve['curveTypes']), key=lambda pair: pair[0]['anchor']['frame'])
    if not pairs:
        return None
    if frame <= pairs[0][0]['anchor']['frame']:
        return pairs[0][0]['anchor']['value']
    for (left, kind), (right, _) in zip(pairs, pairs[1:]):
        a, d = left['anchor'], right['anchor']
        if frame == d['frame']:
            return d['value']
        if frame > d['frame']:
            continue
        if kind == 'LINEAR':
            t = (frame-a['frame'])/(d['frame']-a['frame'])
            return a['value'] + t*(d['value']-a['value'])
        if kind == 'STEP':
            return a['value']
        if kind not in ('SMOOTH', 'BEZIER'):
            raise ValueError(f'oracle does not implement curve type {kind!r}')
        b, c = left['next'], right['previous']
        if not a['frame'] <= b['frame'] <= c['frame'] <= d['frame']:
            raise ValueError('oracle rejects nonmonotonic Bezier time handles')
        lo, hi = 0.0, 1.0
        for _ in range(70):
            t = (lo+hi)/2
            if cubic(a['frame'], b['frame'], c['frame'], d['frame'], t) < frame:
                lo = t
            else:
                hi = t
        return cubic(a['value'], b['value'], c['value'], d['value'], (lo+hi)/2)
    return pairs[-1][0]['anchor']['value']


def sample_scene(scene, resource, parameters, frame):
    values = {p['id']: p['default'] for p in parameters}
    ranges = {p['id']: (p['minimum'], p['maximum']) for p in parameters}
    written = set()
    for track in scene['tracks']:
        if track['resourceGuid'] != resource or not track['start'] <= frame < track['start']+track['duration']:
            continue
        for curve in track['parameterCurves']:
            value = sample_curve(curve, frame-track['start']+track['internalOffset'])
            if value is None:
                continue
            key = curve['parameterId']
            if key in written:
                raise ValueError('oracle rejects overlapping parameter tracks')
            written.add(key)
            low, high = ranges[key]
            values[key] = min(high, max(low, value))
    return values


def render(rpc, output, name, bounds=None):
    if bounds is None:
        bounds = rpc.call('bounds')['bounds']
        assert len(bounds) == 4 and all(math.isfinite(v) for v in bounds)
    center = [(bounds[0]+bounds[2])/2, (bounds[1]+bounds[3])/2]
    half = max(bounds[2]-bounds[0], bounds[3]-bounds[1]) * 0.525
    assert half > 0
    square = [center[0]-half, center[1]-half, center[0]+half, center[1]+half]
    result = rpc.call('render', width=256, height=256, bounds=square)
    png = base64.b64decode(result['png'], validate=True)
    width, height, rgba = CORE.decode_rgba(png)
    assert (width, height) == (256, 256)
    visible = sum(alpha != 0 for alpha in rgba[3::4])
    assert visible, 'render has no visible pixels'
    (output / f'{name}.png').write_bytes(png)
    return {'status': 'preview', 'sha256': hashlib.sha256(png).hexdigest(),
        'visiblePixels': visible, 'bounds': bounds, 'warnings': result.get('warnings', [])}


def attempt(action):
    try:
        return action()
    except (RuntimeError, ValueError, KeyError, AssertionError, OSError) as error:
        return {'status': 'failed', 'error': str(error) or type(error).__name__}


def expressions(rpc, model, path, inputs, output, consumed):
    parameters = {parameter['id']: parameter for parameter in model['parameters']}
    defaults = {key: parameter['default'] for key, parameter in parameters.items()}
    before = rpc.call('snapshot')
    results = []
    rendered = False
    for expression_path in sorted(path.parent.rglob('*.exp3.json')):
        digest = sha(expression_path)
        consumed[expression_path] = digest
        entry = {'path': str(expression_path.relative_to(inputs)), 'sha256': digest,
            'association': 'under CMO3 directory; all parameter IDs must exist in this model'}
        results.append(entry)

        def apply():
            nonlocal rendered
            source = json.loads(expression_path.read_text())
            records = source['Parameters']
            unknown = sorted({item['Id'] for item in records} - parameters.keys())
            if unknown:
                return {'status': 'not_tested', 'reason': 'expression targets other model parameters',
                    'unknownParameters': unknown}
            name = f'audit-{len(results)}'
            rpc.call('live2d.expression.import', name=name, path=str(expression_path))
            expected = defaults.copy()
            for item in records:
                key, value, blend = item['Id'], item['Value'], item.get('Blend', 'Add')
                if blend == 'Add':
                    value += expected[key]
                elif blend == 'Multiply':
                    value *= expected[key]
                elif blend != 'Overwrite':
                    raise ValueError(f'unknown expression blend {blend!r}')
                expected[key] = min(parameters[key]['maximum'], max(parameters[key]['minimum'], value))
            rpc.call('live2d.expression.apply', name=name)
            actual = rpc.call('snapshot')
            assert actual['values'].keys() == expected.keys()
            error = max((abs(actual['values'][key]-value) for key, value in expected.items()), default=0)
            assert error < 1e-6, f'expression value mismatch: {error}'
            for key in ['model', 'revision', 'modified', 'canUndo', 'canRedo']:
                assert before[key] == actual[key], f'expression changed {key}'
            result = {'status': 'passed', 'maximumError': error,
                'changedParameters': [key for key, value in expected.items() if value != defaults[key]]}
            if not rendered and result['changedParameters']:
                result['render'] = attempt(lambda: render(rpc, output, f'expression-{sha(path)[:12]}-{digest[:12]}'))
                rendered = result['render']['status'] == 'preview'
            return result

        entry.update(attempt(apply))
        def reset():
            rpc.call('live2d.expression.reset')
            assert rpc.call('snapshot')['values'] == defaults, 'expression reset did not restore base values'
            return {'status': 'passed'}
        entry['reset'] = attempt(reset)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', type=Path)
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--timeout', type=float, default=120)
    parser.add_argument('--mcp', action='store_true', help='use actual stdio MCP for every operation')
    parser.add_argument('--parse-only', action='store_true')
    args = parser.parse_args()
    inputs = args.inputs.resolve()
    if args.output:
        output = args.output.resolve()
        output.mkdir(parents=True, exist_ok=False)
    else:
        parent = ROOT / 'build/verification'
        parent.mkdir(parents=True, exist_ok=True)
        output = Path(tempfile.mkdtemp(prefix='native-playback-', dir=parent))
    records, by_path, models, consumed = {}, {}, {}, {}
    files = sorted(path for path in inputs.rglob('*') if path.is_file())
    for path in files:
        if path.suffix.lower() not in ('.cmo3', '.can3'):
            continue
        digest = sha(path)
        record = records.setdefault(digest, {'sha256': digest, 'type': path.suffix.lower(), 'paths': []})
        record['paths'].append(str(path.relative_to(inputs)))
        by_path[path] = digest
    report = {'binarySha256': sha(args.binary), 'inputs': str(inputs),
        'mode': 'MCP' if args.mcp else 'JSONL', 'parseOnly': args.parse_only,
        'scope': 'Native CMO3/CAN3 parsing, preview rendering and parameter timelines; not Cubism equivalence',
        'expressionRenderPolicy': 'first successfully rendered expression changing default values per model',
        'fileTypes': dict(Counter(path.suffix.lower() for path in files)), 'records': list(records.values())}

    def checkpoint():
        (output / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')

    for digest, record in records.items():
        path = inputs / record['paths'][0]
        rpc = Rpc(args.binary, args.timeout, args.mcp)
        try:
            def opened():
                snapshot = rpc.call('open', path=str(path))
                model = snapshot['model']
                if record['type'] == '.can3':
                    _, xml = CAN.CMO.CAFF.decode(path.read_bytes())
                    CAN.check_xml(model, xml)
                else:
                    models[digest] = model
                return {'status': 'passed', 'format': snapshot['documentFormat']}
            record['read'] = attempt(opened)
            if record['read']['status'] == 'passed' and not args.parse_only:
                if record['type'] == '.cmo3':
                    record['render'] = attempt(lambda: render(rpc, output, digest[:16]))
                    record['expressions'] = expressions(rpc, models[digest], path, inputs, output, consumed)
                saved = output / (digest[:16] + record['type'])

                def roundtrip():
                    rpc.call('save', path=str(saved))
                    assert sha(saved) == digest, 'unmodified native save changed bytes'
                    saved.unlink()
                    return {'status': 'passed'}
                record['save'] = attempt(roundtrip)
        finally:
            rpc.close()
        print(record['type'], record['paths'][0], record['read'], record.get('render', {}).get('status'), flush=True)
        checkpoint()

    if not args.parse_only:
        for digest, record in records.items():
            if record['type'] != '.can3' or record['read']['status'] != 'passed':
                continue
            path = inputs / record['paths'][0]
            rpc = Rpc(args.binary, args.timeout, args.mcp)
            try:
                animation = rpc.call('open', path=str(path))['model']
                record['bindings'] = []
                for resource in animation['resources']:
                    binding = {'resourceGuid': resource['guid'], 'scenes': []}
                    record['bindings'].append(binding)
                    wanted = {resource['name'].replace('\\', '/').split('/')[-1],
                        resource['sourceFile'].replace('\\', '/').split('/')[-1]}
                    identities = {(curve['parameterGuid'], curve['parameterId'])
                        for scene in animation['scenes'] for track in scene['tracks']
                        if track['resourceGuid'] == resource['guid'] for curve in track['parameterCurves']}
                    part_identities = {(curve['partGuid'], curve['partId'])
                        for scene in animation['scenes'] for track in scene['tracks']
                        if track['resourceGuid'] == resource['guid'] for curve in track.get('partOpacityCurves', [])}
                    candidates = compatible_models(path, inputs, by_path, models, identities, part_identities)
                    named = [p for p in candidates if p.name in wanted or p.stem in wanted]
                    if named:
                        candidates = named
                    candidates.sort(key=lambda p: (-len(Path(os.path.commonpath([path.parent, p.parent])).parts), str(p)))
                    if not candidates:
                        binding.update(status='not_tested', reason='no exact GUID/ID-compatible CMO3 in this archive')
                        continue
                    if len({by_path[p] for p in candidates}) != 1:
                        binding.update(status='not_tested', reason='multiple GUID/ID-compatible CMO3 files; explicit mapping required',
                            candidates=[str(p.relative_to(inputs)) for p in candidates])
                        continue
                    model_path = candidates[0]
                    binding['model'] = str(model_path.relative_to(inputs))
                    binding['selection'] = 'exact source name and GUID/ID' if named else 'unique GUID/ID match in archive'
                    bound = attempt(lambda: rpc.call('live2d.model.attach', path=str(model_path), resourceGuid=resource['guid']))
                    if bound.get('status') == 'failed':
                        binding.update(bound)
                        continue
                    binding['status'] = 'bound'
                    parameters = models[by_path[model_path]]['parameters']
                    for index, scene in enumerate(animation['scenes']):
                        if not any(t['resourceGuid'] == resource['guid'] for t in scene['tracks']):
                            continue
                        entry = {'guid': scene['guid'], 'name': scene['name'], 'samples': []}
                        binding['scenes'].append(entry)
                        frames = sorted(set([0.0, max(0, scene['durationFrames']-1)/2, max(0, scene['durationFrames']-1)]))
                        bounds = None
                        for sample_index, frame in enumerate(frames):
                            sample = {'frame': frame}
                            entry['samples'].append(sample)

                            def seek():
                                expected = sample_scene(scene, resource['guid'], parameters, frame)
                                rpc.call('live2d.animation.seek', sceneGuid=scene['guid'], frame=frame)
                                actual = rpc.call('snapshot')['values']
                                assert actual.keys() == expected.keys(), 'parameter sets differ'
                                error = max((abs(actual[k]-v) for k, v in expected.items()), default=0)
                                assert error < 1e-5, f'numeric timeline mismatch: {error}'
                                return {'status': 'passed', 'maximumError': error}
                            sample['timeline'] = attempt(seek)
                            if sample['timeline']['status'] == 'passed':
                                sample['render'] = attempt(lambda: render(rpc, output,
                                    f'{digest[:12]}-{resource["guid"][:8]}-{index}-{sample_index}', bounds))
                                bounds = sample['render'].get('bounds', bounds)
                        checkpoint()
                print('CAN3 timelines', record['paths'][0], flush=True)
            finally:
                rpc.close()
    report['originalsUnchanged'] = all(sha(path) == digest for path, digest in (by_path | consumed).items())
    report['inputHashScope'] = 'all native CMO3/CAN3 inputs plus consumed expression files'
    assert report['originalsUnchanged'], 'original input was modified'
    checkpoint()
    print(output / 'result.json')


if __name__ == '__main__':
    main()
