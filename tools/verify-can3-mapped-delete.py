#!/usr/bin/env python3
import argparse
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('mapped_delete_parent', ROOT / 'tools/verify-can3-mapped-parent.py')
PARENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PARENT)
TRACKS, KEY, CURVES = PARENT.TRACKS, PARENT.KEY, PARENT.CURVES
UNDERSCORE = CURVES.load('mapped_delete_keys', 'verify-can3-underscore-keys.py')
PARTS = CURVES.load('mapped_delete_parts', 'verify-can3-part-curves.py')


def proof_helpers():
    return TRACKS.proof_helpers() | {Path(m.__file__).resolve() for m in [
        PARENT, UNDERSCORE, UNDERSCORE.LEGACY, UNDERSCORE.MODERN, PARTS]} | {Path(__file__).resolve()}


def deleted_xml(original, ids, curve):
    nodes = TRACKS.xml_nodes(original)
    definitions = TRACKS.definitions(nodes)

    def resolve(node):
        for _ in range(65):
            if 'xs.ref' not in node.attrs:
                return node
            node = definitions[node.attrs['xs.ref']]
        raise AssertionError('reference cycle or depth')

    def field(node, name, optional=False):
        for _ in range(65):
            matches = [n for n in node.children if n.attrs.get('xs.n') == name]
            assert len(matches) <= 1
            if matches:
                return matches[0]
            supers = [n for n in node.children if n.attrs.get('xs.n') == 'super']
            assert len(supers) <= 1
            if not supers and optional:
                return None
            assert supers, name
            node = resolve(supers[0])
        raise AssertionError('inheritance depth')

    tracks = [n for n in definitions.values() if n.tag == TRACKS.TRACK
              and resolve(field(n, 'guid')).attrs.get('uuid') == ids['trackGuid']]
    assert len(tracks) == 1
    effect = resolve(field(tracks[0], 'partsVisibleEffect'))
    mapping = field(effect, 'attrMap')
    key = curve.get('mapKey', 'live2DPartsOpacity:' + curve.get('legacyMapPartId', curve.get('partId', '')))
    entries = [n for n in mapping.children if resolve(field(n, 'key')).attrs.get('idstr') == key]
    assert len(entries) == 1
    entry = entries[0]
    attribute = resolve(field(entry, 'value'))
    assert resolve(field(attribute, 'id')).attrs['idstr'] == ids['curveId']
    members = field(effect, 'attrList', optional=True)
    owned_members = [] if members is None else [n for n in members.children
        if n.attrs.get('xs.ref') == attribute.attrs['xs.id']]
    assert len(owned_members) <= 1
    changes = [(entry.start, entry.end, b''), (attribute.start, attribute.end, b'')]
    for member in owned_members:
        changes.append((member.start, member.end, b''))
    for node, delta in [(mapping, 1), (members, len(owned_members))]:
        if delta:
            start, end = TRACKS.count_span(original, node)
            changes.append((start, end, TRACKS.count_value(original[start:end], len(node.children) - delta)))
    return TRACKS.patch(original, changes)


def selection(model, key=None, legacy=False):
    family = 'legacyPartOpacityCurves' if legacy else 'underscorePartOpacityCurves'
    rows = [(s, t, c) for s in model['scenes'] for t in s['tracks'] for c in t[family]
            if legacy or c['mapKey'] == key]
    assert rows
    scene, track, curve = rows[0]
    ids = {'sceneGuid': scene['guid'], 'trackGuid': track['guid'], 'curveId': curve['id']}
    if not legacy:
        ids['partOpacityMapKey'] = key
    return ids, family, curve


def delete(host, label, ids, family, curve):
    _, before = KEY.state(host, label + '-before')
    expected = copy.deepcopy(before)
    scene = next(s for s in expected['scenes'] if s['guid'] == ids['sceneGuid'])
    track = next(t for t in scene['tracks'] if t['guid'] == ids['trackGuid'])
    track[family].remove(curve)
    track['skippedLegacyPartOpacityCount'] = len(track['legacyPartOpacityCurves']) + len(track['underscorePartOpacityCurves'])
    _, after = KEY.edit(host, label, 'live2d_animation_curve_delete', **ids)
    assert after == expected
    host.call(label + '-undo', 'undo', {})
    assert KEY.state(host, label + '-undone')[1] == before
    host.call(label + '-redo', 'redo', {})
    assert KEY.state(host, label + '-redone')[1] == expected
    return expected


def main():
    parser = argparse.ArgumentParser(description='Verify exact mapped CAN3 curve deletion on generated CAFF and pinned native copies.')
    for name in ['core', 'njc', 'rice-can3', 'ren-can3', 'hiyori-can3']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    native = {'rice': args.rice_can3.resolve(), 'ren': args.ren_can3.resolve(), 'hiyori': args.hiyori_can3.resolve()}
    for name in ['rice', 'ren']:
        assert (native[name].stat().st_size, PARENT.digest(native[name])) == UNDERSCORE.PINS[name]
    assert PARENT.digest(native['hiyori']) == UNDERSCORE.LEGACY.NATIVE_SHA256
    evidence = ROOT / 'build/verification/issue145-mapped-delete'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | proof_helpers() | set(PARENT.FIXTURES) | {
        ROOT / 'rust/tests/core.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): PARENT.digest(p) for p in sorted(paths)}
    inputs = {str(p): PARENT.digest(p) for p in [core, njc, *native.values()]}
    result = {'status': 'FAIL', 'sourceHashes': hashes, 'inputHashes': inputs, 'cases': [], 'refusals': [],
              'nativeGui': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN', 'generatedInputHashes': {}}
    host = None
    try:
        host = CURVES.DRIVER.Host(run, core, njc, CURVES.DRIVER.free_port())
        host.wait_ready()
        tools = {row['name']: row for row in host.tools()}
        manifest = {row['id']: row for row in json.loads((ROOT / 'doc/commands.manifest.json').read_text())['commands']}
        name = 'live2d_animation_curve_delete'
        assert tools[name]['inputSchema'] == manifest[name]['inputSchema'] == CURVES.EXPECTED_SCHEMAS[name]
        cmo3 = run / 'parts.cmo3'
        cmo3.write_bytes(KEY.caff_pack(PARTS.cmo3_xml().encode()))
        result['generatedInputHashes'][str(cmo3)] = PARENT.digest(cmo3)
        cases = [(name, KEY.caff_pack(xml), None, None) for name, xml in PARENT.fixtures().items()
                 if name in ['underscore-no-members', 'underscore-all-members']]
        cases.append(('mixed-unsaved-modern-add', KEY.caff_pack(PARENT.fixtures()['underscore-all-members']), None, None))
        absent = PARENT.fixtures()['underscore-no-members']
        registered = absent.replace(b'<i xs.n="start">', b"<CMvEffectManager xs.n='effectManager'><array xs.n='effectList' count='1' type='ICMvEffect'><CMvEffect_Live2DPartsVisible xs.ref='parts-effect'/></array></CMvEffectManager><i xs.n=\"start\">")
        flat = absent.replace(b'<ICMvEffect xs.n="super">', b'').replace(b'</ICMvEffect>', b'')
        for label, xml in [('missing', absent), ('registered-missing', registered), ('flat-missing', flat)]:
            cases.append(('mixed-unsaved-' + label, KEY.caff_pack(xml), None, None))
        cases += [(label, native[name].read_bytes(), key, name) for label, name, key in [
            ('rice25', 'rice', 'live2DPartsOpacity_Part25'), ('rice27', 'rice', 'live2DPartsOpacity_Part27'),
            ('rice-options', 'rice', 'live2DPartsOpacity_Part3'), ('ren-no-member', 'ren', 'live2DPartsOpacity_PartSketch0'),
            ('hiyori-legacy', 'hiyori', None)]]
        for label, original, native_key, native_name in cases:
            source, saved = run / (label + '.can3'), run / (label + '-saved.can3')
            source.write_bytes(original)
            result['generatedInputHashes'][str(source)] = PARENT.digest(source)
            host.call(label + '-open', 'open', {'path': str(source)})
            _, initial = KEY.state(host, label + '-initial')
            expected_xml = KEY.decode_entries(original)[1]
            mixed = label.startswith('mixed-unsaved-')
            if mixed:
                for part in ['PartNewA', 'PartNewB']:
                    KEY.edit(host, label + '-add-' + part, 'live2d_animation_curve_add', sceneGuid='scene-guid',
                             trackGuid='track-guid', modelPath=str(cmo3), partId=part)
            requests = [(native_key, native_name == 'hiyori')] if native_name else [
                ('live2DPartsOpacity_PartArmB', False), (None, True)] + ([] if mixed else [('live2DPartsOpacity_PartArmA', False)])
            model = initial
            for index, (key, legacy) in enumerate(requests):
                _, model = KEY.state(host, label + '-select-' + str(index))
                ids, family, curve = selection(model, key, legacy)
                expected_xml = deleted_xml(expected_xml, ids, curve)
                model = delete(host, label + '-delete-' + str(index), ids, family, curve)
            data = CURVES.save_and_reopen(host, label, saved, model, original)
            if mixed:
                added = model['scenes'][0]['tracks'][0]['partOpacityCurves']
                assert added == [{'id': 'live2DPartsOpacity_' + part, 'partId': part, 'partGuid': guid,
                    'sourcePart': {'type': 'CPartId', 'value': part}, 'range': [0.0, 1.0],
                    'points': [], 'curveTypes': []} for part, guid in [('PartNewA', 'new-a'), ('PartNewB', 'new-b')]]
                stripped = KEY.decode_entries(data)[1]
                for curve in added:
                    stripped = deleted_xml(stripped, {'trackGuid': 'track-guid', 'curveId': curve['id']}, curve)
                if label.endswith('missing'):
                    lists = [n for n in TRACKS.xml_nodes(stripped) if n.attrs.get('type') == 'ICMvAttr']
                    assert len(lists) == 1 and not lists[0].children and lists[0].attrs['count'] == '0'
                    stripped = TRACKS.patch(stripped, [(lists[0].start, lists[0].end, b'')])
                assert stripped == expected_xml, 'XML outside mapped removals, modern additions and count tokens changed'
            else:
                assert KEY.decode_entries(data)[1] == expected_xml, 'XML outside deletion nodes/counts changed'
            assert source.read_bytes() == original
            result['cases'].append({'name': label, 'nativeSource': native_name, 'deletions': len(requests)})
        fixture = UNDERSCORE.FIXTURE.read_bytes()
        source = run / 'selector-refusals.can3'
        source.write_bytes(KEY.caff_pack(fixture))
        result['generatedInputHashes'][str(source)] = PARENT.digest(source)
        host.call('selectors-open', 'open', {'path': str(source)})
        base = {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid', 'curveId': 'live2DPartsOpacity_PartArmA'}
        for label, extra, message in [
            ('ambiguous', {}, 'ambiguous'), ('null', {'partOpacityMapKey': None}, 'nonempty'),
            ('empty', {'partOpacityMapKey': ''}, 'nonempty'), ('type', {'partOpacityMapKey': 7}, 'nonempty'),
            ('wrong', {'partOpacityMapKey': 'missing'}, 'not found'),
            ('legacy-no-fallback', {'curveId': 'live2DPartsOpacity:legacy-attribute', 'partOpacityMapKey': 'live2DPartsOpacity_PartArmB'}, 'not found'),
            ('stale', {'partOpacityMapKey': 'live2DPartsOpacity_PartArmB', 'expectedRevision': 0}, 'revision conflict'),
        ]:
            KEY.refuse(host, label, name, {**base, **extra}, message)
            result['refusals'].append(label)
        for label, xml, message in [
            ('outside', fixture.replace(b'</shared>', b"<Opaque xs.ref='part-b'/></shared>"), 'outside its owner'),
            ('nested', fixture.replace(b'keep-part-b', b"<Opaque xs.id='nested-owned'/>"), 'nested shared'),
        ]:
            source = run / (label + '.can3')
            source.write_bytes(KEY.caff_pack(xml))
            result['generatedInputHashes'][str(source)] = PARENT.digest(source)
            host.call(label + '-open', 'open', {'path': str(source)})
            KEY.refuse(host, label, name, {**base, 'partOpacityMapKey': 'live2DPartsOpacity_PartArmB'}, message)
            result['refusals'].append(label)
        for label, xml, message in [
            ('missing-shared-owner', absent.replace(b'<ICMvEffect xs.n="super">',
                b"<ICMvEffect xs.n='super' xs.id='nested-super'>"), 'nested shared'),
            ('missing-unknown-owner', absent.replace(b'<ICMvEffect xs.n="super">',
                b'<FutureEffect xs.n="super">').replace(b'</ICMvEffect>', b'</FutureEffect>'), 'owner is unsupported'),
        ]:
            source = run / (label + '.can3')
            source.write_bytes(KEY.caff_pack(xml))
            result['generatedInputHashes'][str(source)] = PARENT.digest(source)
            host.call(label + '-open', 'open', {'path': str(source)})
            KEY.refuse(host, label, 'live2d_animation_curve_add',
                       {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid', 'modelPath': str(cmo3), 'partId': 'PartNewA'}, message)
            result['refusals'].append(label)
        result['status'] = 'PASS'
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode, 'running': host.child.poll() is None}
                assert not result['cleanup']['running']
            assert {str(p): PARENT.digest(p) for p in [core, njc, *native.values()]} == inputs
            assert {str(p.relative_to(ROOT)): PARENT.digest(p) for p in sorted(paths)} == hashes
            assert all(PARENT.digest(Path(p)) == h for p, h in result['generatedInputHashes'].items())
        except BaseException:
            result['status'] = 'FAIL'
            result['cleanupError'] = traceback.format_exc()
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(run)


if __name__ == '__main__':
    main()
