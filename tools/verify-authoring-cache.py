#!/usr/bin/env python3
"""Check current-scene queries through real native edit and history transitions."""
import json
import copy
from pathlib import Path
import sys
import tempfile
import subprocess

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import importlib.util

spec = importlib.util.spec_from_file_location('primary', ROOT / 'tools/verify-primary-forms.py')
PRIMARY = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PRIMARY)
MORPH = PRIMARY.import_tool('morph', ROOT / 'tools/verify-morph-forms.py')
CORE, CAFF, CMO = PRIMARY.CORE, PRIMARY.CAFF, PRIMARY.CMO


def read_scene(path):
    entries, xml = CAFF.decode(path.read_bytes())
    return CMO.read_scene(xml, {e[0][0] for e in entries})


def motion3_document(parameter, value):
    return {'Version': 3, 'Meta': {'Duration': 2, 'Fps': 30, 'Loop': True,
        'AreBeziersRestricted': False, 'CurveCount': 1, 'TotalSegmentCount': 2,
        'TotalPointCount': 3, 'UserDataCount': 0, 'TotalUserDataSize': 0},
        'Curves': [{'Target': 'Parameter', 'Id': parameter['id'],
            'Segments': [0, parameter['default'], 0, 1, value, 0, 2, parameter['default']]}],
        'UserData': []}


def runtime_transports(source, parameter, value, library, binary, run):
    motion_path = run / (source.stem + '-preview.motion3.json')
    motion_path.write_text(json.dumps(motion3_document(parameter, value)))
    requests = [
        {'op': 'open', 'path': str(source)},
        {'op': 'live2d.parameter.set', 'parameterId': parameter['id'], 'value': value},
        {'op': 'snapshot'},
        {'op': 'live2d.authoring.linear_weights'},
        {'op': 'live2d.parameter.set', 'parameterId': parameter['id'], 'value': parameter['maximum'] + 1},
        {'op': 'snapshot'},
        {'op': 'open', 'path': str(source)},
        {'op': 'snapshot'},
    ]
    requests += [
        {'op': 'motion3.import', 'name': 'preview', 'path': str(motion_path)},
        {'op': 'motion3.list'},
        {'op': 'motion3.read', 'name': 'preview'},
        {'op': 'motion3.pose', 'name': 'preview', 'time': 0.5},
        {'op': 'motion3.seek', 'name': 'preview', 'time': 0.5},
        {'op': 'live2d.authoring.linear_weights'},
        {'op': 'motion3.advance', 'dt': 2.25, 'loop': True},
        {'op': 'live2d.authoring.primary_forms'},
        {'op': 'live2d.authoring.morph_forms'},
        {'op': 'motion3.reset'},
        {'op': 'open', 'path': str(source)},
        {'op': 'snapshot'},
    ]
    core = CORE.Core(library)
    try:
        expected = [core.request(request['op'], success=index != 4,
                    **{key: value for key, value in request.items() if key != 'op'})
                    for index, request in enumerate(requests)]
    finally:
        core.close()
    wire = ''.join(json.dumps(request) + '\n' for request in requests)
    process = subprocess.run([str(binary)], input=wire, text=True, capture_output=True, timeout=120)
    assert process.returncode == 0, process.stderr
    rows = [json.loads(line) for line in process.stdout.splitlines()]
    assert len(rows) == len(requests)
    for index, response in enumerate(rows):
        assert response['succeeded'] == (index != 4), response
        assert response.get('result') == expected[index]
    (run / (source.stem + '-runtime-jsonl.txt')).write_text(wire + process.stdout)
    messages = [
        {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
            'protocolVersion': '2024-11-05', 'capabilities': {},
            'clientInfo': {'name': 'native-runtime-verifier', 'version': '1'}}},
        {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
        {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'},
    ]
    for index, request in enumerate(requests):
        messages.append({'jsonrpc': '2.0', 'id': index + 3, 'method': 'tools/call', 'params': {
            'name': request['op'].replace('.', '_'),
            'arguments': {key: value for key, value in request.items() if key != 'op'}}})
    wire = ''.join(json.dumps(message) + '\n' for message in messages)
    process = subprocess.run([str(binary), '--mcp'], input=wire, text=True,
                             capture_output=True, timeout=120)
    assert process.returncode == 0, process.stderr
    responses = {row['id']: row for row in map(json.loads, process.stdout.splitlines()) if 'id' in row}
    tool = next(tool for tool in responses[2]['result']['tools'] if tool['name'] == 'live2d_parameter_set')
    assert tool['inputSchema']['required'] == ['parameterId', 'value']
    for index in range(len(requests)):
        response = responses[index + 3]['result']
        assert bool(response.get('isError')) == (index == 4), response
        if index != 4:
            actual = response.get('structuredContent')
            if actual is None:
                actual = json.loads(response['content'][0]['text'])
            assert actual == expected[index]
    (run / (source.stem + '-runtime-mcp.txt')).write_text(wire + process.stdout)


def main():
    inputs = Path(sys.argv[1]) if len(sys.argv) > 1 else PRIMARY.DEFAULT_INPUTS
    run = Path(tempfile.mkdtemp(prefix='authoring-cache-', dir=ROOT / 'build/verification'))
    library = ROOT / 'out/rust/debug' / ('libsoul2d_core.dylib' if sys.platform == 'darwin' else 'libsoul2d_core.so')
    binary = ROOT / 'out/rust/debug/soul2d-core'
    result = {'status': 'FAIL', 'cases': [], 'queryComparisons': 0, 'evidence': str(run)}
    core = None
    try:
        paths = sorted(inputs.rglob('*.cmo3'))
        assert len(paths) == 4
        original_hashes = {str(p): PRIMARY.sha(p) for p in paths}
        core = CORE.Core(library)
        for index, source in enumerate(paths):
            original = read_scene(source)
            parameters = {p['id']: p for p in original['parameters']}
            candidates = []
            for owner in original['sources']:
                if owner['data']['type'] != 'ArtMesh' or not owner['grid']:
                    continue
                if owner['morphTargets'] and (owner['morphTargets']['targets'] or
                                             owner['morphTargets']['blendWeightConstraints']):
                    continue
                weights = PRIMARY.WEIGHTS.expected_source(owner, parameters, {})['weights']
                if len(weights) != 1 or weights[0]['weight'] != 1:
                    continue
                form = next(f for f in owner['forms'] if f['guid'] == weights[0]['formGuid'])
                if form['opacity'] is not None and form['opacity'] != 0.375:
                    candidates.append((owner, form))
            assert candidates, 'No supported exact-default ArtMesh in ' + str(source)
            owner, form = candidates[0]
            identity = {'sourceGuid': owner['guid'], 'formGuid': form['guid'],
                        'coordinateType': form['coordinateType']}
            vertex_scene = copy.deepcopy(original)
            edited_form = next(f for s in vertex_scene['sources'] if s['guid'] == owner['guid']
                               for f in s['forms'] if f['guid'] == form['guid']
                               and f['coordinateType'] == form['coordinateType'])
            x, y = form['data']['positions'][:2]
            position = [x + 0.01, y]
            edited_form['data']['positions'][:2] = position
            opacity_scene = copy.deepcopy(vertex_scene)
            edited_form = next(f for s in opacity_scene['sources'] if s['guid'] == owner['guid']
                               for f in s['forms'] if f['guid'] == form['guid']
                               and f['coordinateType'] == form['coordinateType'])
            edited_form['opacity'] = 0.375
            core.request('open', path=str(source))
            destination = run / (str(index) + '.cmo3')
            samples = [{}]
            runtime_owner = next((candidate for candidate in original['sources']
                if candidate['grid'] and any(len(a['keys']) > 1 for a in candidate['grid']['axes'])
                and not ((candidate.get('morphTargets') or {}).get('targets') or
                         (candidate.get('morphTargets') or {}).get('blendWeightConstraints'))), None)
            assert runtime_owner, 'No bound runtime parameter owner: ' + str(source)
            axis = next((a for a in runtime_owner['grid']['axes'] if len(a['keys']) > 1), None)
            assert axis, 'No runtime parameter probe on selected owner: ' + str(source)
            if axis:
                samples.append({axis['parameterId']: sum(axis['keys'][:2]) / 2})
                parameter = parameters[axis['parameterId']]
                value = next(key for key in axis['keys'] if key != parameter['default'])
                runtime = {parameter['id']: value}
                before = core.request('snapshot')
                after = core.request('live2d.parameter.set', parameterId=parameter['id'], value=value)
                expected_snapshot = copy.deepcopy(before)
                expected_snapshot['values'][parameter['id']] = value
                assert after == expected_snapshot
                for op, oracle in (
                    ('live2d.authoring.linear_weights', PRIMARY.WEIGHTS.expected_response),
                    ('live2d.authoring.primary_forms', PRIMARY.expected_response),
                    ('live2d.authoring.morph_forms', MORPH.expected_response)):
                    PRIMARY.compare_response(core.request(op, sourceGuid=runtime_owner['guid']),
                                             oracle(original, runtime, runtime_owner['guid']))
                    PRIMARY.compare_response(core.request(op, sourceGuid=runtime_owner['guid'],
                        parameterValues={parameter['id']: parameter['default']}),
                        oracle(original, {}, runtime_owner['guid']))
                    PRIMARY.compare_response(core.request(op, path=str(source), sourceGuid=runtime_owner['guid']),
                                             oracle(original, {}, runtime_owner['guid']))
                    result['queryComparisons'] += 3
                core.request('live2d.parameter.set', parameterId=parameter['id'],
                             value=parameter['maximum'] + 1, success=False)
                assert core.request('snapshot') == after
                runtime_saved = run / ('runtime-' + str(index) + '.cmo3')
                core.request('save', path=str(runtime_saved))
                assert runtime_saved.read_bytes() == source.read_bytes()
                core.request('live2d.parameter.set', parameterId=parameter['id'], value=parameter['default'])
                assert core.request('snapshot') == before
                result.setdefault('runtimeCases', []).append({'model': source.name,
                    'parameterId': parameter['id'], 'value': value, 'sourceGuid': runtime_owner['guid']})
                runtime_transports(source, parameter, value, library, binary, run)
                motion_path = run / (source.stem + '-preview.motion3.json')
                core.request('motion3.import', name='preview', path=str(motion_path))
                imported = core.request('snapshot')
                motion_frames = []
                for time in (0, 0.25, 0.5, 0.75, 1, 1.5, 2):
                    fraction = time if time <= 1 else 2 - time
                    frame_value = parameter['default'] + fraction * (value - parameter['default'])
                    frame = core.request('motion3.seek', name='preview', time=time)
                    assert frame['values'][parameter['id']] == frame_value
                    for key in ('revision', 'modified', 'canUndo', 'canRedo', 'model'):
                        assert frame[key] == imported[key]
                    for op, oracle in (
                        ('live2d.authoring.linear_weights', PRIMARY.WEIGHTS.expected_response),
                        ('live2d.authoring.primary_forms', PRIMARY.expected_response),
                        ('live2d.authoring.morph_forms', MORPH.expected_response)):
                        PRIMARY.compare_response(core.request(op, sourceGuid=runtime_owner['guid']),
                            oracle(original, {parameter['id']: frame_value}, runtime_owner['guid']))
                        result['queryComparisons'] += 1
                    motion_frames.append({'time': time, 'value': frame_value})
                core.request('motion3.reset')
                assert core.request('snapshot')['values'] == before['values']
                core.request('open', path=str(source))
                assert core.request('snapshot')['motion3Library'] == {}
                result.setdefault('motion3Cases', []).append({'model': source.name,
                    'parameterId': parameter['id'], 'sourceGuid': runtime_owner['guid'], 'frames': motion_frames})
            stages = [('initial', original), ('vertex', vertex_scene), ('opacity', opacity_scene),
                      ('undo', vertex_scene), ('undo', original), ('redo', vertex_scene),
                      ('redo', opacity_scene), ('noop', opacity_scene), ('stale', opacity_scene)]
            for stage, expected_scene in stages:
                before = core.request('snapshot')
                revision = before['revision']
                if stage == 'vertex':
                    core.request('live2d.form.set_vertex', **identity, vertexIndex=0,
                                 position=position, expectedRevision=revision)
                elif stage in ('opacity', 'noop', 'stale'):
                    core.request('live2d.form.set_opacity', **identity, opacity=0.375,
                                 expectedRevision=0 if stage == 'stale' else revision,
                                 success=stage != 'stale')
                elif stage in ('undo', 'redo'):
                    core.request(stage)
                after = core.request('snapshot')
                PRIMARY.compare_response(after['model'], expected_scene)
                if stage in ('initial', 'noop', 'stale'):
                    assert after == before, stage + ' changed session state'
                else:
                    assert after['revision'] == revision + 1, stage + ' did not advance one revision'
                    assert after['model'] != before['model'], stage + ' did not change model'
                core.request('save', path=str(destination))
                current = read_scene(destination)
                PRIMARY.compare_response(current, expected_scene)
                snapshot = core.request('snapshot')
                for op, oracle in (
                    ('live2d.authoring.linear_weights', PRIMARY.WEIGHTS.expected_response),
                    ('live2d.authoring.primary_forms', PRIMARY.expected_response),
                    ('live2d.authoring.morph_forms', MORPH.expected_response)):
                    for values in samples:
                        arguments = {'sourceGuid': owner['guid'], 'parameterValues': values}
                        expected = oracle(current, values, owner['guid'])
                        for _ in range(2):
                            PRIMARY.compare_response(core.request(op, **arguments), expected)
                            result['queryComparisons'] += 1
                        PRIMARY.compare_response(core.request(op, path=str(destination), **arguments), expected)
                        PRIMARY.compare_response(core.request(op, path=str(source), **arguments),
                                                 oracle(original, values, owner['guid']))
                        result['queryComparisons'] += 2
                if source.name == 'ren_t01.cmo3':
                    probe = next(s for s in original['sources'] if s['id'] == 'ArtMesh74')
                    values = {'ParamBodyAngleX2': 5}
                    expected = MORPH.expected_response(expected_scene, values, probe['guid'])
                    for _ in range(2):
                        PRIMARY.compare_response(core.request('live2d.authoring.morph_forms',
                            sourceGuid=probe['guid'], parameterValues=values), expected)
                        result['queryComparisons'] += 1
                assert core.request('snapshot') == snapshot
                result['cases'].append({'model': source.name, 'stage': stage,
                                        'sourceGuid': owner['guid'], 'samples': len(samples)})
            core.request('open', path=str(source))
            PRIMARY.compare_response(core.request('live2d.authoring.primary_forms', sourceGuid=owner['guid']),
                                     PRIMARY.expected_response(original, {}, owner['guid']))
            core.request('new')
            core.request('live2d.authoring.primary_forms', success=False)
            print('Native cache history complete: ' + source.name, flush=True)
        assert len(result.get('runtimeCases', [])) == 4, 'Runtime checks must cover all four models'
        assert len(result.get('motion3Cases', [])) == 4, 'Motion3 checks must cover all four models'
        assert original_hashes == {str(p): PRIMARY.sha(p) for p in paths}
        result.update(status='PASS', originalSourceHashes=original_hashes,
                      sourceBinaryHashes=PRIMARY.source_manifest(library, binary),
                      scope='Current source-space scene queries; no rendered-pose or performance claim.')
        result['sourceBinaryHashes']['tools/verify-authoring-cache.py'] = PRIMARY.sha(Path(__file__).resolve())
        result['sourceBinaryHashes']['tools/verify-morph-forms.py'] = PRIMARY.sha(ROOT / 'tools/verify-morph-forms.py')
    except Exception as error:
        result['error'] = repr(error)
    finally:
        if core:
            core.close()
        (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'status': result['status'], 'evidence': str(run), 'error': result.get('error')}))
    return 0 if result['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
