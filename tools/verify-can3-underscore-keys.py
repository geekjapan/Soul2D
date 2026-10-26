#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('legacy_keys', ROOT / 'tools/verify-can3-legacy-keys.py')
LEGACY = importlib.util.module_from_spec(spec)
spec.loader.exec_module(LEGACY)
MODERN, KEY = LEGACY.MODERN, LEGACY.KEY
FIXTURE = ROOT / 'rust/tests/fixtures/can3-underscore-opacity.xml'
PINS = {'rice': (95670, 'ad7ff25d60619b77944c0473403c2bb9a04d71be9adf84a95ed151b826042631'),
        'ren': (181386, 'e9de09c664a44d39954838cd95b4e6453f32b5a5f2f03af1b7b897be30869e1b')}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def choices(model, key):
    return [(scene, track, curve) for scene in model['scenes'] for track in scene['tracks']
            for curve in track['underscorePartOpacityCurves'] if curve['mapKey'] == key]


def curve(model, ids):
    rows = [(s, t, c) for s, t, c in choices(model, ids['partOpacityMapKey'])
            if s['guid'] == ids['sceneGuid'] and t['guid'] == ids['trackGuid'] and c['id'] == ids['curveId']]
    assert len(rows) == 1
    return rows[0][2]


def native_storage(data, ids):
    tree = ET.fromstring(KEY.decode_entries(data)[1])
    definitions = {n.get('xs.id'): n for n in tree.iter() if n.get('xs.id')}
    field = lambda n, name: LEGACY.xml_field(n, name, definitions)
    tracks = [n for n in tree.iter('CMvTrack_Live2DModel_Source') if n.get('xs.id')
              and field(n, 'guid').get('uuid') == ids['trackGuid']]
    assert len(tracks) == 1
    entries = [LEGACY.xml_resolve(n, definitions) for n in field(field(tracks[0], 'partsVisibleEffect'), 'attrMap')]
    values = [field(n, 'value') for n in entries if field(n, 'key').get('idstr') == ids['partOpacityMapKey']]
    assert len(values) == 1 and field(values[0], 'id').get('idstr') == ids['curveId']
    storage = values[0].get('xs.id')
    return storage, sum(n.get('xs.ref') == storage for n in tree.iter()), [n.get('xs.n') for n in field(values[0], 'optionParam')]


def main():
    parser = argparse.ArgumentParser(description='Verify exact underscore-map PartOpacity key edits on generated and pinned local CAN3 copies.')
    for name in ['core', 'njc', 'rice-can3', 'ren-can3']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    native = {'rice': args.rice_can3.resolve(), 'ren': args.ren_can3.resolve()}
    for name, path in native.items():
        assert (path.stat().st_size, digest(path)) == PINS[name], 'unexpected pinned native input'
    evidence = ROOT / 'build/verification/ani145-underscore-keys'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(LEGACY.__file__), Path(MODERN.__file__), Path(KEY.__file__),
        Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), Path(KEY.CAFF.CORE.__file__), FIXTURE,
        ROOT / 'rust/tests/core.rs', ROOT / 'rust/tests/editor.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}
    inputs = {str(p): digest(p) for p in [core, njc, *native.values()]}
    result = {'status': 'FAIL', 'scope': 'Existing underscore-map five raw key operations; no curve CRUD, playback arbitration or vendor acceptance',
              'sourceHashes': hashes, 'inputHashes': inputs, 'nativeGui': 'SKIP', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        host = KEY.DRIVER.Host(run, core, njc, KEY.DRIVER.free_port())
        host.wait_ready()
        result['hostPid'] = host.child.pid
        generated = run / 'generated.can3'
        original = KEY.caff_pack(FIXTURE.read_bytes())
        generated.write_bytes(original)
        host.call('open-generated', 'open', {'path': str(generated)})
        snapshot, initial = KEY.state(host, 'generated-initial')
        assert 'CAN3 PartOpacity binding is unavailable' in snapshot['nativePlayback']['warnings']
        rows = initial['scenes'][0]['tracks'][0]['underscorePartOpacityCurves']
        assert [c['mapKey'] for c in rows] == ['live2DPartsOpacity_PartArmA', 'live2DPartsOpacity_PartArmB']
        assert rows[0]['id'] == rows[1]['id'] == 'live2DPartsOpacity_PartArmA'
        assert rows[0]['partGuid'] == rows[1]['partGuid']
        tools = KEY.rpc(host, 'tools', 'tools', 'list')['tools']
        for name in ['key_set', 'key_add', 'key_delete', 'handle_set', 'curve_type_set']:
            schema = next(t for t in tools if t['name'] == 'live2d_animation_' + name)['inputSchema']
            assert schema['properties']['partOpacityMapKey'] == {'type': 'string', 'minLength': 1}
        ids = {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid', 'curveId': rows[0]['id']}
        KEY.refuse(host, 'bare-duplicate', 'live2d_animation_key_set', {**ids, 'anchorFrame': 0, 'value': 0.4}, 'ambiguous')
        for index, selector in enumerate(['', None, 7, 'missing', 'live2DPartsOpacity:PartArmB']):
            KEY.refuse(host, f'bad-selector-{index}', 'live2d_animation_key_set',
                       {**ids, 'partOpacityMapKey': selector, 'anchorFrame': 0, 'value': 0.4}, '')
        ids['partOpacityMapKey'] = rows[1]['mapKey']
        KEY.refuse(host, 'mismatched-id', 'live2d_animation_key_set',
                   {**ids, 'curveId': 'live2dParam_ParamTest', 'anchorFrame': 0, 'value': 0.4}, 'not found')
        model = LEGACY.exercise(host, 'generated', ids, [
            ('key_set', {'anchorFrame': 0, 'value': 0.25}), ('key_add', {'anchorFrame': 5, 'value': 0.6}),
            ('handle_set', {'anchorFrame': 5, 'side': 'next', 'frame': 6.5, 'value': 0.7}),
            ('curve_type_set', {'anchorFrame': 5, 'type': 'BEZIER'}), ('key_delete', {'anchorFrame': 0})])
        assert model['scenes'][0]['tracks'][0]['underscorePartOpacityCurves'][0] == rows[0]
        assert curve(model, ids)['points'][0]['anchor'] == {'frame': 5, 'value': 0.6}
        saved = run / 'generated-edited.can3'
        host.call('save-generated', 'save', {'path': str(saved)})
        MODERN.require_preservation(original, saved.read_bytes(), storage='part-b')
        host.call('reopen-generated', 'open', {'path': str(saved)})
        assert KEY.state(host, 'generated-persisted')[1] == model
        aliased = run / 'outside-reference.can3'
        aliased.write_bytes(KEY.caff_pack(FIXTURE.read_text().replace('</shared>', '<Opaque xs.ref="part-b"/></shared>').encode()))
        host.call('open-aliased', 'open', {'path': str(aliased)})
        for name, fields in [('key_set', {'anchorFrame': 0, 'value': 0.4}),
                             ('key_add', {'anchorFrame': 5, 'value': 0.4}),
                             ('key_delete', {'anchorFrame': 0}),
                             ('handle_set', {'anchorFrame': 0, 'side': 'next', 'value': 0.4}),
                             ('curve_type_set', {'anchorFrame': 0, 'type': 'STEP'})]:
            KEY.refuse(host, 'outside-' + name, 'live2d_animation_' + name, {**ids, **fields}, 'outside its owner')
        receipts = []
        for label, name, key in [('rice25', 'rice', 'live2DPartsOpacity_Part25'),
                                 ('rice27', 'rice', 'live2DPartsOpacity_Part27'),
                                 ('rice-extra-options', 'rice', 'live2DPartsOpacity_Part3'),
                                 ('ren-no-member', 'ren', 'live2DPartsOpacity_PartSketch0')]:
            host.call(label + '-open', 'open', {'path': str(native[name])})
            snapshot, initial = KEY.state(host, label + '-initial')
            assert 'CAN3 PartOpacity binding is unavailable' in snapshot['nativePlayback']['warnings']
            candidates = choices(initial, key)
            assert candidates
            scene, track, old_curve = candidates[0]
            assert old_curve['points'] == []
            ids = {'sceneGuid': scene['guid'], 'trackGuid': track['guid'], 'curveId': old_curve['id'], 'partOpacityMapKey': key}
            if label in ['rice25', 'rice27']:
                bare = dict(ids)
                del bare['partOpacityMapKey']
                KEY.refuse(host, label + '-bare', 'live2d_animation_key_add',
                           {**bare, 'anchorFrame': 0, 'value': 0.25}, 'ambiguous')
            data = native[name].read_bytes()
            storage, references, option_names = native_storage(data, ids)
            assert references == (2 if name == 'ren' else 3)
            if label == 'rice-extra-options':
                assert set(option_names) == {'KEY_PARTS_VISIBLE_ID', 'KEY_ATTR_FADE_IN', 'KEY_ATTR_FADE_OUT'}
                assert old_curve['sourcePart'] == {'type': 'CPartId', 'value': 'Part3'}
            model = LEGACY.exercise(host, label, ids, [
                ('key_add', {'anchorFrame': 0, 'value': 0.25}), ('key_add', {'anchorFrame': 10, 'value': 0.75}),
                ('key_set', {'anchorFrame': 0, 'value': 0.4}),
                ('handle_set', {'anchorFrame': 0, 'side': 'next', 'frame': 3, 'value': 0.5}),
                ('curve_type_set', {'anchorFrame': 0, 'type': 'BEZIER'}), ('key_delete', {'anchorFrame': 10})])
            edited = curve(model, ids)
            assert dict(edited, points=[], curveTypes=[]) == old_curve
            assert edited['points'][0]['anchor'] == {'frame': 0, 'value': 0.4}
            assert edited['curveTypes'] == ['BEZIER']
            saved = run / (label + '-edited.can3')
            host.call(label + '-save', 'save', {'path': str(saved)})
            MODERN.require_preservation(data, saved.read_bytes(), storage=storage, retained_points=())
            host.call(label + '-reopen', 'open', {'path': str(saved)})
            assert KEY.state(host, label + '-persisted')[1] == model
            receipts.append({'case': label, 'selection': ids, 'storage': storage, 'references': references,
                             'optionNames': option_names, 'originalCurve': old_curve, 'editedCurve': edited})
        assert generated.read_bytes() == original
        result.update(status='PASS', nativeCases=receipts)
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode, 'running': host.child.poll() is None}
                assert not result['cleanup']['running']
            assert {str(p): digest(p) for p in [core, njc, *native.values()]} == inputs
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)} == hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
