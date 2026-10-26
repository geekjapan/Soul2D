#!/usr/bin/env python3
import argparse
import copy
import importlib.util
import json
import ntpath
from pathlib import Path
import sys
import tempfile
import traceback
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('native_curves_delete', ROOT / 'tools/verify-can3-mapped-delete.py')
DELETE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DELETE)
TRACKS, KEY, CURVES = DELETE.TRACKS, DELETE.KEY, DELETE.CURVES
LEGACY = DELETE.UNDERSCORE.LEGACY
PINS = {'can3': 'e9de09c664a44d39954838cd95b4e6453f32b5a5f2f03af1b7b897be30869e1b',
        'cmo3': '648e523b1c686a9af763ffbfe07f5f754df513e92f66271bbdc94eba7dd3e93f'}
PART = 'PartNose'
GUID = '7d647e30-30ef-4623-aeb8-ab5b20402cd3'
MAP_KEY = 'live2DPartsOpacity_PartSketch0'
SCENE = 'a46085c1-a235-447d-854d-60e0776241eb'
TRACK = '3d8eaf7d-5a5c-4785-95fd-e82c9431cbf3'
STORAGE = '#499'


def paired_parts(data):
    tree = ET.fromstring(KEY.decode_entries(data)[1])
    definitions = {n.get('xs.id'): n for n in tree.iter() if n.get('xs.id')}
    parts = {}
    for node in tree.iter('CPartSource'):
        if node.get('xs.ref'):
            continue
        part = LEGACY.xml_field(node, 'id', definitions).get('idstr')
        guid = LEGACY.xml_field(node, 'guid', definitions).get('uuid')
        assert part not in parts and part and guid
        parts[part] = guid
    assert parts[PART] == GUID
    return parts


def parts_storage(data, track_guid):
    nodes = TRACKS.xml_nodes(data)
    definitions = TRACKS.definitions(nodes)

    def resolve(node):
        for _ in range(65):
            if 'xs.ref' not in node.attrs:
                return node
            node = definitions[node.attrs['xs.ref']]
        raise AssertionError('reference depth')

    def field(node, name):
        for _ in range(65):
            node = resolve(node)
            matches = [n for n in node.children if n.attrs.get('xs.n') == name]
            assert len(matches) <= 1
            if matches:
                return resolve(matches[0])
            supers = [n for n in node.children if n.attrs.get('xs.n') == 'super']
            assert len(supers) <= 1
            if not supers:
                return None
            node = supers[0]
        raise AssertionError('inheritance depth')

    tracks = [n for n in definitions.values() if n.tag == TRACKS.TRACK
              and field(n, 'guid').attrs.get('uuid') == track_guid]
    assert len(tracks) == 1
    effect = field(tracks[0], 'partsVisibleEffect')
    return field(effect, 'attrMap'), field(effect, 'attrList')


def point(frame, value):
    return {'anchor': {'frame': frame, 'value': value}, 'corner': True,
            'previous': {'frame': float(frame), 'integerFrame': frame, 'value': value, 'positionOptimized': False},
            'next': {'frame': float(frame), 'integerFrame': frame, 'value': value, 'positionOptimized': False}}


def undo_addition(actual, original):
    mapping, _ = parts_storage(actual, TRACK)
    entries = [n for n in mapping.children if any(c.attrs.get('idstr') == 'live2DPartsOpacity:' + PART for c in n.children)]
    assert len(entries) == 1
    assert any(c.attrs.get('xs.ref') == STORAGE for c in entries[0].children)
    a, b = TRACKS.xml_nodes(original), TRACKS.xml_nodes(actual)
    old, new = TRACKS.definitions(a)[STORAGE], TRACKS.definitions(b)[STORAGE]
    assert sum(n.attrs.get('xs.ref') == STORAGE for n in b) == 3
    start, end = TRACKS.count_span(actual, mapping)
    changes = [(entries[0].start, entries[0].end, b''),
               (start, end, TRACKS.count_value(actual[start:end], len(mapping.children) - 1))]
    for name in ['points', 'curveTypes', 'curMin', 'curMax']:
        before = [n for n in a if old.start < n.start < old.end and n.attrs.get('xs.n') == name]
        after = [n for n in b if new.start < n.start < new.end and n.attrs.get('xs.n') == name]
        assert len(before) == len(after) == 1
        before, after = before[0], after[0]
        assert before.tag == after.tag
        if name in ['points', 'curveTypes']:
            assert {k: v for k, v in before.attrs.items() if k != 'count'} == {
                k: v for k, v in after.attrs.items() if k != 'count'}
            assert not before.children and before.attrs['count'] == '0'
            changes.append((after.start, after.end, original[before.start:before.end]))
        else:
            assert before.attrs == after.attrs and before.tag == 'd' and not before.children and not after.children
            changes.append((after.open_end, after.end - 4, original[before.open_end:before.end - 4]))
    return TRACKS.patch(actual, changes)


def main():
    parser = argparse.ArgumentParser(description='Verify modern CAN3 curve creation on an owned pinned Ren CAN3/CMO3 pair.')
    for name in ['core', 'njc', 'can3', 'cmo3']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc, can3, cmo3 = [getattr(args, name).resolve() for name in ['core', 'njc', 'can3', 'cmo3']]
    for name, path in [('can3', can3), ('cmo3', cmo3)]:
        assert DELETE.PARENT.digest(path) == PINS[name], 'unexpected pinned native input'
    original, model_bytes = can3.read_bytes(), cmo3.read_bytes()
    parts = paired_parts(model_bytes)
    evidence = ROOT / 'build/verification/issue145-native-curves'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | DELETE.proof_helpers() | {Path(__file__).resolve(),
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'rust/tests/core.rs',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): DELETE.PARENT.digest(p) for p in sorted(paths)}
    inputs = {str(p): DELETE.PARENT.digest(p) for p in [core, njc, can3, cmo3]}
    source, owned_model = run / 'ren_t01.can3', run / 'ren_t01.cmo3'
    source.write_bytes(original)
    owned_model.write_bytes(model_bytes)
    generated = {str(source): DELETE.PARENT.digest(source), str(owned_model): DELETE.PARENT.digest(owned_model)}
    result = {'status': 'FAIL', 'sourceHashes': hashes, 'inputHashes': inputs, 'generatedInputHashes': generated,
              'cases': [], 'nativeGui': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        host = CURVES.DRIVER.Host(run, core, njc, CURVES.DRIVER.free_port())
        host.wait_ready()
        for mixed in [False, True]:
            label = 'native-add-mapped-delete' if mixed else 'native-add'
            host.call(label + '-open', 'open', {'path': str(source)})
            snapshot, initial = KEY.state(host, label + '-initial')
            scene = next(s for s in initial['scenes'] if s['guid'] == SCENE)
            track = TRACKS.find_track(initial, SCENE, TRACK)
            old_curve = next(c for c in track['underscorePartOpacityCurves'] if c['mapKey'] == MAP_KEY)
            resource = next(r for r in initial['resources'] if r['guid'] == track['resourceGuid'])
            assert ntpath.basename(resource['sourceFile']) == 'ren_t01.cmo3'
            assert resource['guid'] == 'ee44f545-7c21-49f2-bb70-ef7dd320aa82'
            ids = {'sceneGuid': scene['guid'], 'trackGuid': track['guid']}
            mapping, members = parts_storage(KEY.decode_entries(original)[1], track['guid'])
            assert members.attrs['count'] == '51' and track['partOpacityCurves'] == []
            assert all(c['partGuid'] != GUID for field in ['partOpacityCurves', 'legacyPartOpacityCurves',
                       'underscorePartOpacityCurves'] for c in track[field])
            assert old_curve['partGuid'] not in parts.values()
            add = {**ids, 'modelPath': str(owned_model), 'partId': PART}
            _, added = KEY.edit(host, label + '-add', 'live2d_animation_curve_add', **add)
            expected = copy.deepcopy(initial)
            target = TRACKS.find_track(expected, scene['guid'], track['guid'])
            curve = {'id': 'live2DPartsOpacity_' + PART, 'partId': PART, 'partGuid': GUID,
                     'sourcePart': {'type': 'CPartId', 'value': PART}, 'range': [0.0, 1.0],
                     'points': [], 'curveTypes': []}
            target['partOpacityCurves'].append(curve)
            assert added == expected
            host.call(label + '-undo', 'undo', {})
            assert KEY.state(host, label + '-undone')[1] == initial
            host.call(label + '-redo', 'redo', {})
            assert KEY.state(host, label + '-redone')[1] == expected
            KEY.refuse(host, label + '-duplicate', 'live2d_animation_curve_add', add.copy(), 'already exists')
            KEY.refuse(host, label + '-stale', 'live2d_animation_curve_add',
                       {**add, 'expectedRevision': snapshot['revision']}, 'revision conflict')
            if not mixed:
                empty_path = run / 'native-add-empty.can3'
                host.call(label + '-save-empty', 'save', {'path': str(empty_path)})
                CURVES.caff_bytes_equal(original, empty_path.read_bytes())
                empty_xml, original_xml = KEY.decode_entries(empty_path.read_bytes())[1], KEY.decode_entries(original)[1]
                a, b = [TRACKS.definitions(TRACKS.xml_nodes(xml))[STORAGE] for xml in [original_xml, empty_xml]]
                assert original_xml[a.start:a.end] == empty_xml[b.start:b.end]
                assert undo_addition(empty_xml, original_xml) == original_xml
                _, cancelled = KEY.edit(host, label + '-cancel', 'live2d_animation_curve_delete',
                                        **ids, curveId=curve['id'])
                assert cancelled == initial
                cancelled_path = run / 'cancelled.can3'
                host.call(label + '-save-cancelled', 'save', {'path': str(cancelled_path)})
                assert cancelled_path.read_bytes() == original
                _, restored = KEY.edit(host, label + '-readd', 'live2d_animation_curve_add', **add)
                assert restored == expected
            for frame, value in [(0, 0.25), (10, 0.75)]:
                _, edited = KEY.edit(host, label + '-key-' + str(frame), 'live2d_animation_key_add',
                                     **ids, curveId=curve['id'], anchorFrame=frame, value=value)
                curve['points'].append(point(frame, value))
                curve['curveTypes'].append('LINEAR')
                assert edited == expected
            expected_xml = KEY.decode_entries(original)[1]
            if mixed:
                removal = {**ids, 'curveId': old_curve['id'], 'partOpacityMapKey': MAP_KEY}
                expected_xml = DELETE.deleted_xml(expected_xml, removal, old_curve)
                edited = DELETE.delete(host, label + '-mapped', removal, 'underscorePartOpacityCurves', old_curve)
                target['underscorePartOpacityCurves'].remove(old_curve)
                target['skippedLegacyPartOpacityCount'] -= 1
                assert edited == expected
            saved = run / (label + '-saved.can3')
            data = CURVES.save_and_reopen(host, label, saved, expected, original)
            actual_xml = KEY.decode_entries(data)[1]
            actual_map, actual_members = parts_storage(actual_xml, track['guid'])
            assert actual_map.attrs['count'] == str(len(mapping.children) + 1 - int(mixed))
            assert actual_members.attrs == members.attrs and len(actual_members.children) == 51
            assert actual_xml[actual_members.start:actual_members.end] == KEY.decode_entries(original)[1][members.start:members.end]
            stripped = undo_addition(actual_xml, KEY.decode_entries(original)[1])
            assert stripped == expected_xml, 'XML outside modern additions, selected mapped removal and counts changed'
            assert source.read_bytes() == original and owned_model.read_bytes() == model_bytes
            result['cases'].append({'name': label, 'selection': ids, 'resource': resource,
                                    'partId': PART, 'partGuid': GUID, 'sourceMemberList': 'present',
                                    'sourceStorage': STORAGE, 'savedMemberCount': 51,
                                    'mappedDeletion': mixed, 'refusals': 2})
        result['status'] = 'PASS'
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
            assert {str(p): DELETE.PARENT.digest(p) for p in [core, njc, can3, cmo3]} == inputs
            assert {str(p.relative_to(ROOT)): DELETE.PARENT.digest(p) for p in sorted(paths)} == hashes
            assert {str(p): DELETE.PARENT.digest(Path(p)) for p in generated} == generated
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
