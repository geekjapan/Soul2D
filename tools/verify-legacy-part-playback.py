#!/usr/bin/env python3
"""Verify exact-GUID legacy Part playback using local, explicitly modified CAN3 copies."""
import json
from pathlib import Path
import tempfile
import importlib.util

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('playback', ROOT / 'tools/verify-native-playback.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
CAN = m.CAN
inputs = CAN.CMO.DEFAULT_INPUTS / 'hiyori_ja/hiyori_pro'
model = inputs / 'hiyori_pro_t11.cmo3'
animation = inputs / 'hiyori_pro_t04.can3'
output = Path(tempfile.mkdtemp(prefix='legacy-part-playback-', dir=ROOT / 'build/verification'))
originals = {p: m.sha(p) for p in (model, animation)}
original = animation.read_bytes()
entries, xml = CAN.CMO.CAFF.decode(original)
part_guid = '153ca6e1-e049-4adc-acc7-d1124c9e19b0'


def scratch(name, opacity, keep_modern=False):
    g = CAN.CMO.XMLGraph(xml)
    a = g.resolve(list(g.root.find('main'))[0])
    selected = 0
    for scene in g.named_items(a, '_scenes'):
        for track in g.source_items(scene, 'trackSourceSet'):
            if track.tag != 'CMvTrack_Live2DModel_Source':
                continue
            effect = g.field(track, 'partsVisibleEffect')
            if effect is None or effect.tag == 'null':
                continue
            attr_map = g.required(effect, 'attrMap')
            for entry in list(attr_map):
                attr = g.required(entry, 'value')
                identity = g.required(attr, 'id').get('idstr', '')
                legacy = identity.startswith('live2DPartsOpacity:')
                chosen = legacy and g.text(scene, 'sceneName', '') == 'hiyori_m01' and identity.endswith(':Parts01ArmA001')
                if chosen:
                    g.required(attr, 'guid').set('uuid', part_guid)
                    points = g.named_items(g.required(attr, 'valueData'), 'points')
                    assert len(points) == 1
                    g.required(g.required(points[0], 'anchor'), 'doubleValue').text = str(opacity)
                    selected += 1
                elif legacy or (not keep_modern and identity == 'live2DPartsOpacity_PartArmA' and g.text(scene, 'sceneName', '') == 'hiyori_m01'):
                    attr_map.remove(entry)
            attr_map.set('count', str(len(attr_map)))
    assert selected == 1
    path = output / (name + '.can3')
    path.write_bytes(CAN.CMO.repack_main_xml(original, entries, CAN.CMO.ET.tostring(g.root, encoding='utf-8')))
    return path


bound = scratch('matched-legacy-030', .3)
opaque = scratch('matched-legacy-100', 1)
collision = scratch('modern-legacy-collision', .3, True)
report = {'originals': {str(p): sha for p, sha in originals.items()}, 'scratchContract':
    'One legacy GUID deliberately matches PartArmA; other legacy map entries removed; selected modern PartArmA removed except collision control. Raw legacy identities stay unchanged.', 'transports': []}
for mcp in (False, True):
    rpc = m.Rpc(ROOT / 'out/rust/release/soul2d-core', 120, mcp)
    try:
        raw = rpc.call('live2d.animation.read', path=str(animation))
        CAN.check_xml(raw, xml)
        resource = raw['resources'][0]['guid']
        original_binding = rpc.call('live2d.animation.bindings', path=str(animation), modelPath=str(model), resourceGuid=resource)
        assert len(original_binding['legacyPartBindings']) == 0
        assert len(original_binding['unboundLegacyPartCurves']) == 180
        result = {'transport': 'mcp' if mcp else 'jsonl', 'originalUnmatched': 180, 'variants': []}
        hashes = []
        for path, expected in ((bound, .3), (opaque, 1)):
            raw = rpc.call('live2d.animation.read', path=str(path))
            _, scratch_xml = CAN.CMO.CAFF.decode(path.read_bytes())
            CAN.check_xml(raw, scratch_xml)
            binding = rpc.call('live2d.animation.bindings', path=str(path), modelPath=str(model), resourceGuid=resource)
            assert len(binding['legacyPartBindings']) == 1 and not binding['unboundLegacyPartCurves']
            item = binding['legacyPartBindings'][0]
            assert item['partId'] == 'PartArmA' and item['partGuid'] == part_guid
            baseline = rpc.call('open', path=str(model))
            rpc.call('live2d.animation.attach', path=str(path), resourceGuid=resource)
            state = rpc.call('live2d.animation.seek', sceneGuid=item['sceneGuid'], frame=30.5)
            active = state['nativePlayback']['active']
            assert active['poseSignals']['PartArmA'] == expected
            assert active['partOpacities'][part_guid] == expected
            assert 'PartArmA' not in state['values']
            assert not any('legacy CAN3 Part' in w for w in state['nativePlayback']['warnings'])
            for key in ('model', 'revision', 'modified', 'canUndo', 'canRedo'):
                assert state[key] == baseline[key]
            render = m.render(rpc, output, f"{result['transport']}-{path.stem}")
            hashes.append(render['sha256'])
            rpc.call('live2d.animation.advance', dt=.2, loop=False)
            repeated = rpc.call('live2d.animation.seek', sceneGuid=item['sceneGuid'], frame=30.5)
            assert repeated['nativePlayback']['active'] == active
            assert m.render(rpc, output, f"{result['transport']}-{path.stem}-repeat")['sha256'] == render['sha256']
            saved = output / f"{result['transport']}-{path.stem}-saved.cmo3"
            rpc.call('save', path=str(saved))
            assert m.sha(saved) == originals[model]
            result['variants'].append({'path': str(path), 'binding': item, 'expectedOpacity': expected, 'render': render, 'repeatAndSave': 'PASS'})
        assert hashes[0] != hashes[1]
        baseline = rpc.call('open', path=str(model))
        rpc.call('live2d.animation.attach', path=str(collision), resourceGuid=resource)
        before = rpc.call('snapshot')
        try:
            rpc.call('live2d.animation.seek', sceneGuid=item['sceneGuid'], frame=30.5)
        except RuntimeError as error:
            assert 'same Part' in str(error), str(error)
        else:
            raise AssertionError('modern/legacy duplicate writer accepted')
        assert rpc.call('snapshot') == before
        result['collisionRejectedWithoutMutation'] = True
        report['transports'].append(result)
    finally:
        rpc.close()
assert all(m.sha(p) == sha for p, sha in originals.items())
report['status'] = 'PASS'
(output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print('PASS: original legacy GUIDs unbound; exact-GUID scratch opacity .3/1 changes rendering, repeat/history/save preserved; modern collision rejected. JSONL + MCP.')
print(output / 'result.json')
