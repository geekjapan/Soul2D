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
spec = importlib.util.spec_from_file_location('modern_part_keys', ROOT / 'tools/verify-can3-part-opacity.py')
MODERN = importlib.util.module_from_spec(spec)
spec.loader.exec_module(MODERN)
KEY = MODERN.KEY
NATIVE_SHA256 = '3dc6b5d1fcccb653976c650a89d2a83c966e15a45ab40e9002f32955677e6c19'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def legacy_curve(model, ids):
    scene = next(s for s in model['scenes'] if s['guid'] == ids['sceneGuid'])
    track = next(t for t in scene['tracks'] if t['guid'] == ids['trackGuid'])
    return next(c for c in track['legacyPartOpacityCurves'] if c['id'] == ids['curveId'])


def xml_resolve(node, definitions):
    for _ in range(65):
        reference = node.get('xs.ref')
        if reference is None:
            return node
        node = definitions[reference]
    raise AssertionError('native XML reference cycle or depth')


def xml_field(node, name, definitions):
    for _ in range(65):
        node = xml_resolve(node, definitions)
        fields = [child for child in node if child.get('xs.n') == name]
        assert len(fields) <= 1
        if fields:
            return xml_resolve(fields[0], definitions)
        parents = [child for child in node if child.get('xs.n') == 'super']
        assert len(parents) <= 1
        if not parents:
            return None
        node = parents[0]
    raise AssertionError('native XML inheritance cycle or depth')


def exercise(host, label, ids, operations):
    for index, (name, fields) in enumerate(operations):
        before, old = KEY.state(host, f'{label}-{index}-before')
        _, model = KEY.edit(host, f'{label}-{index}-{name}', 'live2d_animation_' + name, **ids, **fields)
        host.call(f'{label}-{index}-undo', 'undo', {})
        assert KEY.state(host, f'{label}-{index}-undone')[1] == old
        host.call(f'{label}-{index}-redo', 'redo', {})
        assert KEY.state(host, f'{label}-{index}-redone')[1] == model
        KEY.refuse(host, f'{label}-{index}-stale', 'live2d_animation_' + name,
                   {**ids, **fields, 'expectedRevision': before['revision']}, 'revision conflict')
    return model


def main():
    parser = argparse.ArgumentParser(description='Verify legacy-colon PartOpacity keys with generated and pinned local CAN3 inputs.')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    parser.add_argument('--native-can3', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc, native = args.core.resolve(), args.njc.resolve(), args.native_can3.resolve()
    assert native.stat().st_size == 139956 and digest(native) == NATIVE_SHA256, 'unexpected native input'
    evidence = ROOT / 'build/verification/ani145-legacy-keys'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(MODERN.__file__), Path(KEY.__file__), Path(KEY.DRIVER.__file__),
        Path(KEY.CAFF.__file__), Path(KEY.CAFF.CORE.__file__), MODERN.FIXTURE,
        ROOT / 'rust/tests/core.rs', ROOT / 'rust/tests/editor.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}
    inputs = {str(p): digest(p) for p in (core, njc, native)}
    result = {'status': 'FAIL', 'scope': 'Existing legacy-colon PartOpacity keys only; no legacy curve CRUD or underscore-map edits',
              'sourceHashes': hashes, 'inputHashes': inputs, 'nativeGui': 'SKIP', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        host = KEY.DRIVER.Host(run, core, njc, KEY.DRIVER.free_port())
        host.wait_ready()
        result['hostPid'] = host.child.pid
        generated = run / 'generated.can3'
        xml = MODERN.FIXTURE.read_text()
        original = KEY.caff_pack(xml.encode())
        generated.write_bytes(original)
        host.call('open-generated', 'open', {'path': str(generated)})
        ids = {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid', 'curveId': 'live2DPartsOpacity:legacy-attribute'}
        model = exercise(host, 'generated', ids, [
            ('key_set', {'anchorFrame': 0, 'value': 0.25}),
            ('key_add', {'anchorFrame': 5, 'value': 0.6}),
            ('handle_set', {'anchorFrame': 5, 'side': 'next', 'frame': 6.5, 'value': 0.7}),
            ('curve_type_set', {'anchorFrame': 5, 'type': 'BEZIER'}),
            ('key_delete', {'anchorFrame': 0})])
        curve = legacy_curve(model, ids)
        assert curve['legacyMapPartId'] == 'legacy-map' and curve['legacyAttributeId'] == 'legacy-attribute'
        assert curve['sourcePart'] == {'type': 's', 'value': 'LegacySource'}
        assert [p['anchor']['frame'] for p in curve['points']] == [5, 10]
        assert curve['points'][0]['anchor']['value'] == 0.6 and curve['curveTypes'] == ['BEZIER', 'LINEAR']
        KEY.refuse(host, 'legacy-curve-delete-selector', 'live2d_animation_curve_delete',
                   {**ids, 'partOpacityMapKey': 'live2DPartsOpacity_missing'}, 'not found')
        saved = run / 'generated-edited.can3'
        host.call('save-generated', 'save', {'path': str(saved)})
        MODERN.require_preservation(original, saved.read_bytes(), storage='part-legacy')
        host.call('reopen-generated', 'open', {'path': str(saved)})
        assert KEY.state(host, 'generated-persisted')[1] == model
        bad = run / 'outside-reference.can3'
        bad.write_bytes(KEY.caff_pack(xml.replace('</shared>', '<Opaque xs.ref="part-legacy"/></shared>').encode()))
        host.call('open-aliased', 'open', {'path': str(bad)})
        for name, fields in [('key_set', {'anchorFrame': 0, 'value': 0.4}),
                             ('key_add', {'anchorFrame': 5, 'value': 0.4}),
                             ('key_delete', {'anchorFrame': 0}),
                             ('handle_set', {'anchorFrame': 0, 'side': 'next', 'value': 0.4}),
                             ('curve_type_set', {'anchorFrame': 0, 'type': 'STEP'})]:
            KEY.refuse(host, 'outside-' + name, 'live2d_animation_' + name, {**ids, **fields}, 'outside its owner')
        host.call('open-native', 'open', {'path': str(native)})
        _, initial = KEY.state(host, 'native-initial')
        choices = [(scene, track, curve) for scene in initial['scenes'] for track in scene['tracks']
                   for curve in track['legacyPartOpacityCurves'] if not curve['points']]
        assert choices
        scene, track, original_curve = choices[0]
        ids = {'sceneGuid': scene['guid'], 'trackGuid': track['guid'], 'curveId': original_curve['id']}
        native_bytes = native.read_bytes()
        native_xml = KEY.decode_entries(native_bytes)[1].decode()
        tree = ET.fromstring(native_xml)
        definitions = {node.get('xs.id'): node for node in tree.iter() if node.get('xs.id')}
        track_storage = next(node.get('xs.id') for node in tree.iter('CMvTrack_Live2DModel_Source')
                             if node.get('xs.id') and xml_field(node, 'guid', definitions).get('uuid') == track['guid'])
        attributes = [node for node in tree.iter('CMvAttrF') if node.get('xs.id')
                      and xml_field(node, 'id', definitions).get('idstr') == original_curve['id']
                      and xml_field(node, 'track', definitions).get('xs.id') == track_storage]
        assert len(attributes) == 1
        storage = attributes[0].get('xs.id')
        model = exercise(host, 'native', ids, [
            ('key_add', {'anchorFrame': 0, 'value': 0.25}),
            ('key_add', {'anchorFrame': 10, 'value': 0.75}),
            ('key_set', {'anchorFrame': 0, 'value': 0.4}),
            ('handle_set', {'anchorFrame': 0, 'side': 'next', 'frame': 3.0, 'value': 0.5}),
            ('curve_type_set', {'anchorFrame': 0, 'type': 'BEZIER'}),
            ('key_delete', {'anchorFrame': 10})])
        edited = legacy_curve(model, ids)
        without_keys = dict(edited, points=original_curve['points'], curveTypes=original_curve['curveTypes'])
        assert without_keys == original_curve
        assert edited['points'][0]['anchor'] == {'frame': 0, 'value': 0.4} and edited['curveTypes'] == ['BEZIER']
        saved_native = run / 'native-edited.can3'
        host.call('save-native', 'save', {'path': str(saved_native)})
        MODERN.require_preservation(native_bytes, saved_native.read_bytes(), storage=storage, retained_points=())
        host.call('reopen-native', 'open', {'path': str(saved_native)})
        assert KEY.state(host, 'native-persisted')[1] == model
        assert generated.read_bytes() == original
        result.update(status='PASS', nativeSelection=ids, nativeStorage=storage,
                      nativeOriginalCurve=original_curve, nativeEditedCurve=edited)
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode,
                                     'running': host.child.poll() is None}
                assert not result['cleanup']['running']
            assert {str(p): digest(p) for p in (core, njc, native)} == inputs
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)} == hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
