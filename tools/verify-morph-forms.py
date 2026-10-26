#!/usr/bin/env python3
"""Check additive source-space morph geometry against supplied CMO3 XML."""
import bisect
import json
from pathlib import Path
import sys
import tempfile
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
import importlib.util
spec = importlib.util.spec_from_file_location('primary_forms_oracle', ROOT / 'tools/verify-primary-forms.py')
PRIMARY = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PRIMARY)
WEIGHTS, CMO, CAFF, CORE = PRIMARY.WEIGHTS, PRIMARY.CMO, PRIMARY.CAFF, PRIMARY.CORE
OP = 'live2d.authoring.morph_forms'
TOOL = 'live2d_authoring_morph_forms'


def stored_sns_targets(source):
    primary = source['morphTargets']
    if not primary:
        return source, []
    targets, forms, provenance, selected = [], [], [], set()
    for extension in source['extendedInterpolations']:
        generated = extension['morphTargets']
        active = False
        for setting in (generated or {}).get('settings', []):
            endpoints = [t for t in primary['targets'] if t['parameterGuid'] == setting['parameterGuid']]
            if not endpoints or setting['interpolation'] == 'LINEAR':
                continue
            assert setting['interpolation'] == 'SNS_CURVE' and setting['parameterGuid'] not in selected
            selected.add(setting['parameterGuid'])
            keys = sorted([0, *[t['keyValue'] for t in endpoints]])
            inserted = [t for t in generated['targets'] if t['parameterGuid'] == setting['parameterGuid']]
            assert len(inserted) == setting['insertPointCount'] * (len(keys)-1)
            assert all(sum(a < t['keyValue'] < b for t in inserted) == setting['insertPointCount']
                       for a, b in zip(keys, keys[1:]))
            for target in inserted:
                representations = [f for f in extension['morphForms'] if f['guid'] == target['formGuid']]
                assert target['formAvailable'] and representations
                assert all(f.get('vertexCountMatchesSource') is not False for f in representations)
                assert not any(f['guid'] == target['formGuid'] for f in source['forms'])
                targets.append(target)
                forms.extend(representations)
            active = True
        if active:
            provenance.append(extension['guid'])
    return {**source, 'forms': source['forms'] + forms,
            'morphTargets': {**primary, 'targets': primary['targets'] + targets}}, provenance


def expected_response(scene, values=None, guid=None):
    values = values or {}
    parameters = {p['id']: p for p in scene['parameters']}
    response = PRIMARY.expected_response(scene, values, guid)
    response['format'] = 'live2d.authoring.morph-forms.v1'
    owners = {s['guid']: s for s in scene['sources']}
    for row in response['sources']:
        source, provenance = stored_sns_targets(owners[row['guid']])
        row.update(baseFormGuid=None, morphWeights=[])
        if provenance:
            row['extendedMorphExtensions'] = provenance
        morph = source['morphTargets']
        if morph is None or not morph['targets']:
            continue
        default = WEIGHTS.expected_source(source, parameters, {})['weights']
        assert len(default) == 1 and default[0]['weight'] == 1.0
        row['baseFormGuid'] = default[0]['formGuid']
        base = {f['coordinateType']: f for f in source['forms']
                if f['guid'] == row['baseFormGuid']}
        groups = {}
        for target in morph['targets']:
            assert target['formGuid'] not in {c['formGuid'] for c in source['grid']['cells']}
            groups.setdefault(target['parameterId'], []).append(target)
            target_forms = [f for f in source['forms'] if f['guid'] == target['formGuid']]
            forms = {f['coordinateType']: f for f in target_forms}
            assert len(forms) == len(target_forms) and set(forms) == set(base)
            for coordinate in base:
                neutral, delta = base[coordinate], forms[coordinate]
                assert 0 <= neutral['opacity'] <= 1 and 0 <= delta['opacity'] <= 1
                for field in ('drawOrder', 'multiplyColor', 'screenColor'):
                    assert neutral[field] == delta[field]
                kind = neutral['data']['type']
                assert kind == delta['data']['type']
                if kind in ('ArtMesh', 'Warp'):
                    assert len(neutral['data']['positions']) == len(delta['data']['positions'])
                elif kind == 'Rotation':
                    for field in ('scale', 'reflectX', 'reflectY'):
                        assert neutral['data'][field] == delta['data'][field]
        limits = {}
        curves = {}
        for point in morph['blendWeightConstraints']:
            curves.setdefault((point['morphTargetParameterId'], point['constraintParameterId']), []).append(point)
        for (target_id, controller_id), points in curves.items():
            controller = parameters[controller_id]
            points = sorted(points, key=lambda p: p['constraintParameterValue'])
            knots = [p['constraintParameterValue'] for p in points]
            assert len(knots) >= 2 and len(set(knots)) == len(knots) and not controller['repeat']
            assert knots[0] == controller['minimum'] and knots[-1] == controller['maximum']
            assert all(0 <= p['blendWeight'] <= 1 for p in points)
            sample = values.get(controller_id, controller['default'])
            assert knots[0] <= sample <= knots[-1]
            index = bisect.bisect_left(knots, sample)
            if knots[index] == sample:
                limit = points[index]['blendWeight']
            else:
                left, right = points[index-1:index+1]
                fraction = (sample-knots[index-1])/(knots[index]-knots[index-1])
                limit = (1-fraction)*left['blendWeight'] + fraction*right['blendWeight']
            assert target_id != controller_id
            limits[target_id] = min(limits.get(target_id, 1), limit)
        # Canonical parameter/key order is independent of serialized target order.
        for parameter_id in sorted(groups):
            parameter = parameters[parameter_id]
            assert parameter['parameterType'] == 'MORPH_TARGET'
            assert parameter['minimum'] <= 0 <= parameter['maximum'] and not parameter['repeat']
            targets = {t['keyValue']: t for t in groups[parameter_id]}
            assert len(targets) == len(groups[parameter_id]) and 0 not in targets
            sample = values.get(parameter_id, parameter['default'])
            assert parameter['minimum'] <= sample <= parameter['maximum']
            keys = sorted([0, *targets])
            if sample <= keys[0]:
                active = [(keys[0], 1.0)]
            elif sample >= keys[-1]:
                active = [(keys[-1], 1.0)]
            else:
                right = bisect.bisect_left(keys, sample)
                if keys[right] == sample:
                    active = [(sample, 1.0)]
                else:
                    low, high = keys[right-1:right+1]
                    active = [(low, (high-sample)/(high-low)),
                              (high, (sample-low)/(high-low))]
            for key, weight in active:
                weight *= limits.get(parameter_id, 1)
                if key == 0 or weight == 0:
                    continue
                target = targets[key]
                row['morphWeights'].append({'parameterId': parameter_id,
                    'keyValue': key, 'formGuid': target['formGuid'], 'weight': weight})
                forms = {f['coordinateType']: f for f in source['forms']
                         if f['guid'] == target['formGuid']}
                for result in row['evaluatedForms']:
                    neutral, delta = base[result['coordinateType']], forms[result['coordinateType']]
                    result['opacity'] += weight*(delta['opacity']-neutral['opacity'])
                    kind = neutral['data']['type']
                    fields = ('positions',) if kind in ('ArtMesh', 'Warp') else (
                        ('angle', 'origin') if kind == 'Rotation' else (
                            ('intensity',) if kind == 'Glue' else ()))
                    for field in fields:
                        start, end = neutral['data'][field], delta['data'][field]
                        if isinstance(start, list):
                            assert len(start) == len(end) == len(result['data'][field])
                            result['data'][field] = [x + weight*(b-a) for x,a,b in
                                zip(result['data'][field], start, end)]
                        else:
                            result['data'][field] += weight*(end-start)
        for form in row['evaluatedForms']:
            form['opacity'] = max(0, min(1, form['opacity']))
    return response


def refusal_fixtures(path, scene, probe, run):
    entries, xml = CAFF.decode(path.read_bytes())
    cases = []
    for label in ('inactive-scalar-target', 'default-not-stored-key', 'target-overlaps-normal-grid'):
        graph = CMO.XMLGraph(xml)
        if label == 'inactive-scalar-target':
            target = probe['morphTargets']['targets'][0]
            node = next(n for n in graph.root.iter('CArtMeshForm')
                        if n.get('xs.ref') is None and graph.guid(n, 'guid') == target['formGuid'])
            draw_order = graph.required(node, 'drawOrder')
            for child in list(node):
                if child.get('xs.n') == 'drawOrder':
                    node.remove(child)
            replacement = ET.Element(draw_order.tag, {'xs.n': 'drawOrder'})
            replacement.text = str(int(draw_order.text) + 1)
            node.append(replacement)
        elif label == 'default-not-stored-key':
            node = next(n for n in graph.root.iter('CParameterSource')
                        if n.get('xs.ref') is None and graph.field(n, 'id') is not None
                        and graph.required(n, 'id').get('idstr') == 'ParamBodyAngleX')
            graph.required(node, 'defaultValue').text = '5'
        else:
            node = next(n for n in graph.root.iter('KeyFormMorphTarget')
                        if n.get('xs.ref') is None and graph.guid(n, 'keyformGuid')
                        == probe['morphTargets']['targets'][0]['formGuid'])
            parameters = {p['id']: p for p in scene['parameters']}
            neutral = WEIGHTS.expected_source(probe, parameters, {})['weights'][0]['formGuid']
            guid_tag = graph.required(node, 'keyformGuid').tag
            for child in list(node):
                if child.get('xs.n') == 'keyformGuid':
                    node.remove(child)
            node.append(ET.Element(guid_tag, {'xs.n': 'keyformGuid', 'uuid': neutral}))
        rewritten = ET.tostring(graph.root, encoding='utf-8')
        altered = CMO.read_scene(rewritten, {e[0][0] for e in entries})
        try:
            expected_response(altered, {}, probe['guid'])
        except (AssertionError, ValueError):
            pass
        else:
            raise AssertionError('Oracle accepted refusal fixture ' + label)
        destination = run / (label + '.cmo3')
        destination.write_bytes(CMO.repack_main_xml(path.read_bytes(), entries, rewritten))
        cases.append((label, WEIGHTS.request_args(destination, {}, probe['guid'])))
    return cases


def main():
    inputs = Path(sys.argv[1]) if len(sys.argv) > 1 else WEIGHTS.DEFAULT_INPUTS
    run = Path(tempfile.mkdtemp(prefix='morph-forms-', dir=ROOT / 'build/verification'))
    library = ROOT / 'out/rust/debug' / ('libsoul2d_core.dylib' if sys.platform == 'darwin' else 'libsoul2d_core.so')
    binary = ROOT / 'out/rust/debug/soul2d-core'
    result = {'status': 'FAIL', 'verifiedResponses': [], 'negativeControls': [], 'evidence': str(run)}
    core = None
    try:
        paths = sorted(inputs.rglob('*.cmo3'))
        assert len(paths) == 4
        hashes = {str(p): PRIMARY.sha(p) for p in paths}
        scenes = {}
        for path in paths:
            entries, xml = CAFF.decode(path.read_bytes())
            scenes[path] = CMO.read_scene(xml, {e[0][0] for e in entries})
        ren = next(p for p in paths if p.name == 'ren_t01.cmo3')
        scene = scenes[ren]
        owners = [s for s in scene['sources'] if s['morphTargets'] and s['morphTargets']['targets']]
        assert len(owners) == 45 and sum(len(s['morphTargets']['targets']) for s in owners) == 118
        probe = next(s for s in owners if s['id'] == 'ArtMesh74')
        literal = expected_response(scene, {'ParamBodyAngleX2': 5}, probe['guid'])
        assert abs(literal['sources'][0]['evaluatedForms'][0]['data']['positions'][0] - 0.470703545) < 1e-12
        core = CORE.Core(library)
        active = run / 'active.inx'
        CORE.fixture(active)
        core.request('open', path=str(active))
        core.request('node.rename', id=2, name='Preserve active INX during morph queries')
        snapshot = core.request('snapshot')
        cases = []
        for path in paths:
            cases.append((path, 'all-defaults', {}, None))
        for owner in owners:
            for target in owner['morphTargets']['targets']:
                for fraction in (0.5, 1.0):
                    cases.append((ren, owner['id'] + ':' + str(target['keyValue']) + ':' + str(fraction),
                                  {target['parameterId']: target['keyValue']*fraction}, owner['guid']))
            combined = {t['parameterId']: t['keyValue']*0.5 for t in owner['morphTargets']['targets']}
            cases.append((ren, owner['id'] + ':combined', combined, owner['guid']))
            axis = next((a for a in owner['grid']['axes'] if len(a['keys']) > 1), None)
            if axis:
                varied = {**combined, axis['parameterId']: sum(axis['keys'][:2])/2}
                cases.append((ren, owner['id'] + ':normal-and-morph', varied, owner['guid']))
        zundamon = next(p for p in paths if 'zundamon' in str(p))
        zun_scene = scenes[zundamon]
        zun_parameters = {p['id']: p for p in zun_scene['parameters']}
        sns_owner = next(s for s in zun_scene['sources'] if s['id'] == 'Warp71')
        sns_literal = expected_response(zun_scene, {'ParamHairX2': -0.3333333}, sns_owner['guid'])['sources'][0]
        assert sns_literal['evaluatedForms'][0]['data']['positions'][80:82] == [-37.851868, 333.72885]
        assert sns_literal['morphWeights'][0]['formGuid'] == '54e50872-ce83-4130-ab72-9b0be2a714f8'
        for parameter in ('ParamHairX2', 'ParamHairY2'):
            inserted = sorted(t['keyValue'] for t in sns_owner['extendedInterpolations'][0]['morphTargets']['targets']
                              if t['parameterId'] == parameter)
            keys = sorted([-1, 0, 1, *inserted])
            for sample in sorted(set(keys + [(a+b)/2 for a,b in zip(keys, keys[1:])])):
                cases.append((zundamon, 'Warp71:SNS:' + parameter + ':' + str(sample),
                              {parameter: sample}, sns_owner['guid']))
        zun_owners = [s for s in zun_scene['sources'] if s['morphTargets'] and s['morphTargets']['targets']]
        assert len(zun_owners) == 118 and sum(len(s['morphTargets']['targets']) for s in zun_owners) == 416
        for owner in zun_owners:
            targets = owner['morphTargets']['targets']
            for target in targets:
                for fraction in (0.5, 1.0):
                    parameter = zun_parameters[target['parameterId']]
                    sample = target['keyValue']*fraction
                    if not parameter['minimum'] <= sample <= parameter['maximum']:
                        continue
                    cases.append((zundamon, owner['id'] + ':' + target['parameterId'] + ':' + str(fraction),
                        {target['parameterId']: sample}, owner['guid']))
            cases.append((zundamon, owner['id'] + ':combined',
                {t['parameterId']: max(zun_parameters[t['parameterId']]['minimum'],
                    min(zun_parameters[t['parameterId']]['maximum'], t['keyValue']*0.5)) for t in targets}, owner['guid']))
            curves = {}
            for point in owner['morphTargets']['blendWeightConstraints']:
                curves.setdefault((point['morphTargetParameterId'], point['constraintParameterId']), set()).add(point['constraintParameterValue'])
            for (target_id, controller_id), knots in curves.items():
                keys = sorted(knots)
                samples = sorted(set(keys + [(a+b)/2 for a,b in zip(keys, keys[1:])]))
                target = next(t for t in targets if t['parameterId'] == target_id)
                for sample in samples:
                    cases.append((zundamon, owner['id'] + ':' + target_id + ':limit:' + controller_id + ':' + str(sample),
                        {target_id: target['keyValue'], controller_id: sample}, owner['guid']))
        arm = next(s for s in zun_owners if s['id'] == 'ArtMesh196')
        arm_values = {'ParamArmUpperL':1, 'ParamArmCross':0.5, 'ParamArmLowerL':0.5, 'ParamArmJawL':0.5}
        arm_literal = expected_response(zun_scene, arm_values, arm['guid'])['sources'][0]
        arm_weight = next(w for w in arm_literal['morphWeights'] if w['parameterId'] == 'ParamArmUpperL')
        assert arm_weight['weight'] == 0.5
        assert arm_literal['evaluatedForms'][0]['opacity'] == 0.5
        cases.append((zundamon, 'ArtMesh196:three-controller-literal', arm_values, arm['guid']))
        plan = []
        for index, (path, label, values, guid) in enumerate(cases, 1):
            args = WEIGHTS.request_args(path, values, guid)
            expected = expected_response(scenes[path], values, guid)
            actual = WEIGHTS.ffi_request(core, {'op': OP, **args})
            assert actual['succeeded'], actual
            PRIMARY.compare_response(actual['result'], expected)
            value, _ = WEIGHTS.jsonl_request(binary, {'op': OP, **args})
            PRIMARY.compare_response(value, expected)
            plan.append((args, expected, label))
            result['verifiedResponses'].append({'case': label, 'transports': ['ffi', 'jsonl']})
            if index % 64 == 0:
                print(f'FFI/JSONL complete: {index}/{len(cases)} cases', flush=True)
        for offset in range(0, len(plan), 64):
            batch = plan[offset:offset+64]
            replies, schema, _ = WEIGHTS.mcp_requests(binary, [p[0] for p in batch], TOOL)
            assert len(replies) == len(batch)
            result['mcpSchema'] = schema
            for reply, (_, expected, label) in zip(replies, batch):
                assert reply['succeeded'], reply
                PRIMARY.compare_response(reply['result'], expected)
                result['verifiedResponses'].append({'case': label, 'transports': ['stdio-mcp']})
            print(f'MCP complete: {offset + len(batch)}/{len(plan)} cases', flush=True)
        negative = [('outside-morph-domain', WEIGHTS.request_args(ren, {'ParamBodyAngleX2': 100}, probe['guid'])),
                    ('unknown-parameter', WEIGHTS.request_args(ren, {'missing': 0}, probe['guid'])),
                    ('nonnumeric-value', WEIGHTS.request_args(ren, {'ParamBodyAngleX2': '5'}, probe['guid'])),
                    ('nonfinite-value', WEIGHTS.request_args(ren, {'ParamBodyAngleX2': WEIGHTS.RAW_NUMBER}, probe['guid'])),
                    ('wrong-values', WEIGHTS.request_args(ren, [], probe['guid'])),
                    ('unknown-source', WEIGHTS.request_args(ren, source_guid='missing'))]
        negative.extend(refusal_fixtures(ren, scene, probe, run))
        entries, xml = CAFF.decode(zundamon.read_bytes())
        for label in ('sns-missing-form', 'sns-duplicate-key', 'sns-incomplete-keys', 'sns-stale-topology', 'sns-unknown-mode'):
            graph = CMO.XMLGraph(xml)
            extension = next(n for n in graph.root.iter('CExtendedInterpolationExtension')
                             if n.get('xs.ref') is None and graph.guid(n, 'guid')
                             == sns_owner['extendedInterpolations'][0]['guid'])
            generated = graph.required(extension, '_extendedKeyformMorphTargetSet')
            targets = graph.named_items(generated, '_morphTargets')
            if label == 'sns-missing-form':
                tag = graph.required(targets[0], 'keyformGuid').tag
                for child in list(targets[0]):
                    if child.get('xs.n') == 'keyformGuid':
                        targets[0].remove(child)
                targets[0].append(ET.Element(tag, {'xs.n': 'keyformGuid', 'uuid': 'missing-generated-form'}))
            elif label == 'sns-duplicate-key':
                graph.required(targets[1], 'keyValue').text = graph.required(targets[0], 'keyValue').text
            elif label == 'sns-stale-topology':
                form = next(f for f in graph.named_items(extension, 'morphTargetExtendedKeyforms')
                            if graph.guid(f, 'guid') == graph.guid(targets[0], 'keyformGuid'))
                positions = graph.required(form, 'positions')
                scalars = positions.text.split()[:-2]
                positions.text = ' '.join(scalars)
                positions.set('count', str(len(scalars)))
            else:
                setting = graph.required(graph.named_items(generated, 'extendedInterpolationSettings')[0], 'value')
                if label == 'sns-incomplete-keys':
                    graph.required(setting, 'insertPointCount').text = '6'
                else:
                    graph.required(setting, 'type').set('v', 'UNKNOWN')
            rewritten = ET.tostring(graph.root, encoding='utf-8')
            altered = CMO.read_scene(rewritten, {entry[0][0] for entry in entries})
            try:
                expected_response(altered, {}, sns_owner['guid'])
            except (AssertionError, ValueError):
                pass
            else:
                raise AssertionError('Oracle accepted SNS refusal fixture ' + label)
            destination = run / (label + '.cmo3')
            destination.write_bytes(CMO.repack_main_xml(zundamon.read_bytes(), entries, rewritten))
            negative.append((label, WEIGHTS.request_args(destination, {}, sns_owner['guid'])))
        for label in ('constraint-weight-outside-domain', 'constraint-duplicate-key', 'constraint-partial-range'):
            graph = CMO.XMLGraph(xml)
            node = next(n for n in graph.root.iter('MorphTargetBlendWeightConstraint') if n.get('xs.ref') is None)
            if label == 'constraint-weight-outside-domain':
                graph.required(node, 'blendWeight').text = '2'
            elif label == 'constraint-partial-range':
                graph.required(node, 'constraintParameterValue').text = '0.25'
            else:
                parent = next(n for n in graph.root.iter() if node in list(n))
                parent.append(ET.fromstring(ET.tostring(node)))
                parent.set('count', str(len(parent)))
            rewritten = ET.tostring(graph.root, encoding='utf-8')
            destination = run / (label + '.cmo3')
            destination.write_bytes(CMO.repack_main_xml(zundamon.read_bytes(), entries, rewritten))
            negative.append((label, WEIGHTS.request_args(destination)))
        for label, args in negative:
            assert not WEIGHTS.ffi_request(core, {'op': OP, **args})['succeeded']
            WEIGHTS.jsonl_request(binary, {'op': OP, **args}, success=False)
            result['negativeControls'].append(label)
        replies, _, _ = WEIGHTS.mcp_requests(binary, [args for _,args in negative], TOOL)
        assert len(replies) == len(negative) and all(not r['succeeded'] for r in replies)
        assert core.request('snapshot') == snapshot
        assert hashes == {str(p): PRIMARY.sha(p) for p in paths}
        # Exercise the same query without a path against the native session.
        core.request('open', path=str(ren))
        native_snapshot = core.request('snapshot')
        actual = core.request(OP, sourceGuid=probe['guid'], parameterValues={'ParamBodyAngleX2': 5})
        PRIMARY.compare_response(actual, literal)
        assert core.request('snapshot') == native_snapshot
        result.update(status='PASS', originalSourceHashes=hashes, activeInxSnapshotPreserved=True,
                      nativeSnapshotPreserved=True, sourceBinaryHashes=PRIMARY.source_manifest(library, binary),
                      queryCases=len(plan), transportComparisons=len(plan)*3,
                      rejectionComparisons=len(negative)*3,
                      literalArtMesh74Positive5X=0.470703545)
        result['literalArtMesh196WeightAndOpacity'] = 0.5
        result['literalWarp71SNSPoint40'] = [-37.851868, 333.72885]
        result['sourceBinaryHashes']['tools/verify-morph-forms.py'] = PRIMARY.sha(Path(__file__).resolve())
        result['scope'] = 'Source-space geometry; not deformer-transformed or vendor-rendered output.'
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
