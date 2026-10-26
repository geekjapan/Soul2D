#!/usr/bin/env python3
"""Exercise every supplied constraint curve without masking by other controllers."""
import bisect
import importlib.util
import json
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('morph_oracle', ROOT / 'tools/verify-morph-forms.py')
MORPH = importlib.util.module_from_spec(spec)
spec.loader.exec_module(MORPH)
spec = importlib.util.spec_from_file_location('native_transport', ROOT / 'tools/verify-native-authoring-edit.py')
TRANSPORT = importlib.util.module_from_spec(spec)
spec.loader.exec_module(TRANSPORT)


def curve_value(points, sample):
    keys = [point['constraintParameterValue'] for point in points]
    right = bisect.bisect_left(keys, sample)
    if keys[right] == sample:
        return points[right]['blendWeight']
    a, b = points[right-1:right+1]
    fraction = (sample-keys[right-1])/(keys[right]-keys[right-1])
    return a['blendWeight']*(1-fraction) + b['blendWeight']*fraction


def plan(scene):
    cases = []
    curve_count = 0
    for owner in scene['sources']:
        morph = owner['morphTargets']
        if not morph or not morph['blendWeightConstraints']:
            continue
        groups = {}
        for point in morph['blendWeightConstraints']:
            groups.setdefault(point['morphTargetParameterId'], {}).setdefault(
                point['constraintParameterId'], []).append(point)
        for target_id, controllers in groups.items():
            target = next(t for t in morph['targets'] if t['parameterId'] == target_id)
            neutral_controllers = {}
            for controller_id, points in controllers.items():
                points.sort(key=lambda p: p['constraintParameterValue'])
                maximum = max(p['blendWeight'] for p in points)
                assert maximum == 1, 'Cannot independently activate this supplied constraint curve'
                neutral_controllers[controller_id] = next(
                    p['constraintParameterValue'] for p in points if p['blendWeight'] == maximum)
            for controller_id, points in controllers.items():
                curve_count += 1
                keys = [p['constraintParameterValue'] for p in points]
                samples = sorted(set(keys + [(a+b)/2 for a,b in zip(keys, keys[1:])]))
                for sample in samples:
                    values = {target_id: target['keyValue'], **neutral_controllers, controller_id: sample}
                    expected = MORPH.expected_response(scene, values, owner['guid'])
                    weight = next((w['weight'] for w in expected['sources'][0]['morphWeights']
                        if w['parameterId'] == target_id and w['keyValue'] == target['keyValue']), 0)
                    limit = curve_value(points, sample)
                    assert abs(weight-limit) < 1e-12, 'Another controller masks the selected curve'
                    cases.append(({'op': MORPH.OP, 'sourceGuid': owner['guid'], 'parameterValues': values},
                        expected, {'owner': owner['id'], 'target': target_id, 'controller': controller_id,
                            'sample': sample, 'expectedWeight': limit}))
    assert curve_count == 116
    # Literal controls distinguish minimum from product and exercise fractional and folded limits.
    for owner_id, values, target_id, literal in (
        ('ArtMesh196', {'ParamArmUpperL':1, 'ParamArmCross':0.5,
            'ParamArmLowerL':0.5, 'ParamArmJawL':0.5}, 'ParamArmUpperL', 0.5),
        ('Warp146', {'ParamPhysicsX':1, 'ParamPhysicsMotion':0.5}, 'ParamPhysicsX', 0.565),
        ('Warp42', {'ParamFaceAngry':0.9, 'ParamEyeType4':0, 'ParamEyeType5':0,
            'ParamEyeType6':0}, 'ParamEyeType7', 0.5),
    ):
        owner = next(s for s in scene['sources'] if s['id'] == owner_id)
        expected = MORPH.expected_response(scene, values, owner['guid'])
        weight = next(w['weight'] for w in expected['sources'][0]['morphWeights']
            if w['parameterId'] == target_id)
        assert abs(weight-literal) < 1e-12
        cases.append(({'op': MORPH.OP, 'sourceGuid': owner['guid'], 'parameterValues': values},
            expected, {'owner': owner_id, 'literalWeight': literal}))
    return cases


def main():
    inputs = Path(sys.argv[1]) if len(sys.argv) > 1 else MORPH.WEIGHTS.DEFAULT_INPUTS
    source = next(inputs.rglob('*zundamon*.cmo3'))
    run = Path(tempfile.mkdtemp(prefix='morph-constraints-', dir=ROOT / 'build/verification'))
    library = ROOT / 'out/rust/debug' / ('libsoul2d_core.dylib' if sys.platform == 'darwin' else 'libsoul2d_core.so')
    binary = ROOT / 'out/rust/debug/soul2d-core'
    result = {'status': 'FAIL', 'evidence': str(run)}
    core = None
    try:
        original_hash = MORPH.PRIMARY.sha(source)
        entries, xml = MORPH.CAFF.decode(source.read_bytes())
        scene = MORPH.CMO.read_scene(xml, {entry[0][0] for entry in entries})
        cases = plan(scene)
        requests = [{'op': 'open', 'path': str(source)}, *[c[0] for c in cases], {'op': 'snapshot'}]
        core = MORPH.CORE.Core(library)
        for transport in ('ffi', 'jsonl', 'stdio-mcp'):
            responses, wire = TRANSPORT.run_transport(binary, core, requests, transport)
            (run / f'{transport}.txt').write_text(wire)
            assert len(responses) == len(requests), transport
            assert all(r['succeeded'] for r in responses), transport
            assert responses[0]['result'] == responses[-1]['result'], 'Queries changed native session'
            for response, (_, expected, _) in zip(responses[1:-1], cases):
                MORPH.PRIMARY.compare_response(response['result'], expected)
            print(f'{transport}: {len(cases)} unmasked constraint cases PASS', flush=True)
        assert MORPH.PRIMARY.sha(source) == original_hash
        hashes = MORPH.PRIMARY.source_manifest(library, binary)
        for tool in (Path(__file__).resolve(), Path(MORPH.__file__).resolve(), Path(TRANSPORT.__file__).resolve()):
            hashes[str(tool.relative_to(ROOT))] = MORPH.PRIMARY.sha(tool)
        result.update(status='PASS', curves=116, cases=[c[2] for c in cases],
            queryComparisons=len(cases)*3, sourceHash=original_hash, sourceBinaryHashes=hashes,
            nativeSnapshotsPreserved=True,
            scope='Independent source-space constraint arithmetic; not final pose or vendor output.')
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
