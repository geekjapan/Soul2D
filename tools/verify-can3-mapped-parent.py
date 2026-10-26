#!/usr/bin/env python3
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('mapped_parent_tracks', ROOT / 'tools/verify-can3-tracks.py')
TRACKS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TRACKS
SPEC.loader.exec_module(TRACKS)
KEY, CURVES = TRACKS.KEYS, TRACKS.CURVES
FIXTURES = [ROOT / 'rust/tests/fixtures' / name for name in ['can3-part-opacity.xml', 'can3-underscore-opacity.xml']]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixtures():
    modern, underscore = [p.read_bytes() for p in FIXTURES]
    members = next(n for n in TRACKS.xml_nodes(underscore) if n.attrs.get('type') == 'ICMvAttr')
    absent = TRACKS.patch(underscore, [(members.start, members.end, b'')])
    complete = TRACKS.patch(underscore, [(members.start, members.end,
        b"<array xs.n='attrList' count='3' type='ICMvAttr'><CMvAttrF xs.ref='part-a'/><CMvAttrF xs.ref='part-b'/><CMvAttrF xs.ref='part-legacy'/></array>")])
    attribute = TRACKS.definitions(TRACKS.xml_nodes(modern))['part-a']
    added = modern[attribute.start:attribute.end].replace(b'part-a', b'part-modern')
    mixed = underscore.replace(b'</shared>', added + b'</shared>') \
        .replace(b'xs.n="attrMap" count="3">', b"xs.n='attrMap' count='4'><entry><CAttrId xs.n='key' idstr='live2DPartsOpacity:PartArmA'/><CMvAttrF xs.n='value' xs.ref='part-modern'/></entry>") \
        .replace(b'count="2" type="ICMvAttr">', b"count='3' type='ICMvAttr'><CMvAttrF xs.ref='part-modern'/>") \
        .replace(b'<i xs.n="start">', b"<CMvEffectManager xs.n='effectManager'><array xs.n='effectList' count='1' type='ICMvEffect'><CMvEffect_Live2DPartsVisible xs.ref='parts-effect'/></array></CMvEffectManager><i xs.n=\"start\">")
    retained = b"<Opaque xs.id='unrelated'><s>keep unrelated &amp; 0.1234500</s></Opaque>"
    return {name: xml.replace(b'</shared>', retained + b'</shared>') for name, xml in [
        ('modern-legacy', modern), ('underscore-no-members', absent),
        ('underscore-all-members', complete), ('mixed-registered', mixed)]}


def deleted_xml(original, scene):
    nodes = TRACKS.xml_nodes(original)
    definitions = TRACKS.definitions(nodes)
    removed = {'track', 'attr', 'parts-effect', 'part-a', 'part-b', 'part-legacy', 'part-modern'} & definitions.keys()
    if scene:
        removed.add('scene')
    identity, container = ('scene', '_scenes') if scene else ('track', '_sources')
    owners = [n for n in nodes if n.attrs == {'xs.ref': identity}
              and n.parent.attrs.get('xs.n') == container]
    assert len(owners) == 1
    owner = owners[0]
    assert len(owner.parent.children) == 1
    start, end = TRACKS.count_span(original, owner.parent)
    changes = [(owner.start, owner.end, b''), (start, end, TRACKS.count_value(original[start:end], 0))]
    changes.extend((definitions[id].start, definitions[id].end, b'') for id in removed)
    expected = TRACKS.patch(original, changes)
    remaining = TRACKS.xml_nodes(expected)
    ids = TRACKS.definitions(remaining)
    assert not removed & ids.keys() and 'unrelated' in ids and 'resource' in ids
    assert all(n.attrs['xs.ref'] in ids for n in remaining if 'xs.ref' in n.attrs)
    return expected


def native_ren_track_definitions(nodes, definitions):
    effects = {'#561', '#563', '#564', '#565', '#566'}
    removed = {'#162', '#161', *effects}
    for effect in effects:
        definition = definitions[effect]
        for node in nodes:
            if not definition.start < node.start < definition.end:
                continue
            if node.parent.attrs.get('xs.n') == 'attrList' or node.attrs.get('xs.n') == 'value':
                removed.add(node.attrs['xs.ref'])
    assert len(removed) == 147
    return removed


def native_ren_track(host, source, saved, original):
    xml = KEY.decode_entries(original)[1]
    nodes = TRACKS.xml_nodes(xml)
    definitions = TRACKS.definitions(nodes)
    removed = native_ren_track_definitions(nodes, definitions)
    owners = [node for node in nodes if node.attrs == {'xs.ref': '#162'}
              and node.parent.attrs.get('xs.n') == '_sources']
    group = definitions['#159']
    children = [node for node in nodes if node.attrs == {'xs.ref': '#161'}
                and node.parent.attrs.get('xs.n') == '_childTrackGuids'
                and group.start < node.start < group.end]
    assert len(owners) == len(children) == 1
    changes = [(definitions[id].start, definitions[id].end, b'') for id in removed]
    for member in [owners[0], children[0]]:
        assert len(member.parent.children) == (2 if member is owners[0] else 1)
        start, end = TRACKS.count_span(xml, member.parent)
        changes.extend([(member.start, member.end, b''),
                        (start, end, TRACKS.count_value(xml[start:end], len(member.parent.children) - 1))])
    expected_xml = TRACKS.patch(xml, changes)
    remaining = TRACKS.xml_nodes(expected_xml)
    retained = TRACKS.definitions(remaining)
    assert not removed.intersection(retained)
    assert all(node.attrs['xs.ref'] in retained for node in remaining if 'xs.ref' in node.attrs)
    host.call('native-ren-open', 'open', {'path': str(source)})
    before, model = KEY.state(host, 'native-ren-before')
    expected = copy.deepcopy(model)
    scene_guid = 'a46085c1-a235-447d-854d-60e0776241eb'
    track_guid = '3d8eaf7d-5a5c-4785-95fd-e82c9431cbf3'
    scene = next(scene for scene in expected['scenes'] if scene['guid'] == scene_guid)
    assert len(scene['tracks']) == 1 and scene['tracks'][0]['guid'] == track_guid
    scene['tracks'] = []
    _, after = KEY.edit(host, 'native-ren-delete', 'live2d_animation_track_delete',
                        sceneGuid=scene_guid, trackGuid=track_guid)
    assert after == expected
    KEY.refuse(host, 'native-ren-stale', 'live2d_animation_track_delete',
               {'sceneGuid': scene_guid, 'trackGuid': track_guid, 'expectedRevision': before['revision']}, 'revision conflict')
    host.call('native-ren-undo', 'undo', {})
    assert KEY.state(host, 'native-ren-undone')[1] == model
    host.call('native-ren-redo', 'redo', {})
    assert KEY.state(host, 'native-ren-redone')[1] == expected
    data = CURVES.save_and_reopen(host, 'native-ren', saved, expected, original)
    assert KEY.decode_entries(data)[1] == expected_xml, 'native XML outside exact deletion closure/counts changed'
    assert source.read_bytes() == original
    return {'status': 'PASS', 'sceneGuid': scene_guid, 'trackGuid': track_guid, 'removedDefinitions': len(removed),
            'savedSha256': digest(saved), 'originalSha256': digest(source)}


def native_ren_scene(host, source, saved, original):
    xml = KEY.decode_entries(original)[1]
    nodes = TRACKS.xml_nodes(xml)
    definitions = TRACKS.definitions(nodes)
    removed = native_ren_track_definitions(nodes, definitions) | {'#160', '#159'}
    assert len(removed) == 149
    owners = [node for node in nodes if node.attrs == {'xs.ref': '#160'}
              and node.parent.attrs.get('xs.n') == '_scenes']
    current = [node for node in nodes if node.attrs == {'xs.n': 'currentScene', 'xs.ref': '#160'}]
    assert len(owners) == len(current) == 1
    owner, current = owners[0], current[0]
    assert len(owner.parent.children) == 8
    index = owner.parent.children.index(owner)
    replacement = owner.parent.children[index + 1].attrs['xs.ref']
    assert replacement == '#568'
    start, end = TRACKS.count_span(xml, owner.parent)
    selected = xml[current.start:current.end]
    assert selected.count(b'xs.ref="#160"') == 1
    changes = [(definitions[id].start, definitions[id].end, b'') for id in removed]
    changes.extend([(owner.start, owner.end, b''), (start, end, TRACKS.count_value(xml[start:end], 7)),
                    (current.start, current.end, selected.replace(b'xs.ref="#160"', b'xs.ref="#568"'))])
    expected_xml = TRACKS.patch(xml, changes)
    remaining = TRACKS.xml_nodes(expected_xml)
    retained = TRACKS.definitions(remaining)
    assert not removed.intersection(retained)
    for id in ['#158', '#152', '#562', '#1524', '#157']:
        assert id in retained
    assert all(node.attrs['xs.ref'] in retained for node in remaining if 'xs.ref' in node.attrs)
    host.call('native-ren-scene-open', 'open', {'path': str(source)})
    before, model = KEY.state(host, 'native-ren-scene-before')
    expected = copy.deepcopy(model)
    scene_guid = 'a46085c1-a235-447d-854d-60e0776241eb'
    expected['scenes'] = [scene for scene in expected['scenes'] if scene['guid'] != scene_guid]
    assert len(model['scenes']) == 8 and len(expected['scenes']) == 7
    _, after = KEY.edit(host, 'native-ren-scene-delete', 'live2d_animation_scene_delete', sceneGuid=scene_guid)
    assert after == expected
    KEY.refuse(host, 'native-ren-scene-stale', 'live2d_animation_scene_delete',
               {'sceneGuid': scene_guid, 'expectedRevision': before['revision']}, 'revision conflict')
    host.call('native-ren-scene-undo', 'undo', {})
    assert KEY.state(host, 'native-ren-scene-undone')[1] == model
    host.call('native-ren-scene-redo', 'redo', {})
    assert KEY.state(host, 'native-ren-scene-redone')[1] == expected
    data = CURVES.save_and_reopen(host, 'native-ren-scene', saved, expected, original)
    assert KEY.decode_entries(data)[1] == expected_xml, 'native XML outside scene closure/count/currentScene changed'
    assert source.read_bytes() == original
    return {'status': 'PASS', 'sceneGuid': scene_guid, 'removedDefinitions': len(removed),
            'remainingScenes': 7, 'currentSceneSourceId': replacement,
            'savedSha256': digest(saved), 'originalSha256': digest(source)}


def main():
    parser = argparse.ArgumentParser(description='Verify owned parent deletion over HTTP MCP with generated CAFF and optional pinned native Ren.')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    parser.add_argument('--native-ren-can3', type=Path)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    evidence = ROOT / 'build/verification/issue145-mapped-parent'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | TRACKS.proof_helpers() | set(FIXTURES) | {
        Path(__file__).resolve(), ROOT / 'rust/tests/core.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}
    inputs = {str(p): digest(p) for p in (core, njc)}
    native = args.native_ren_can3.resolve() if args.native_ren_can3 else None
    if native is not None:
        inputs[str(native)] = digest(native)
        assert inputs[str(native)] == 'e9de09c664a44d39954838cd95b4e6453f32b5a5f2f03af1b7b897be30869e1b'
    result = {'status': 'FAIL', 'scope': 'Generated exact owned PartOpacity parent deletion; optional pinned Ren track and grouped scene deletion; no vendor acceptance',
              'sourceHashes': hashes, 'inputHashes': inputs, 'nativeGui': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN',
              'cases': [], 'refusals': [], 'generatedInputHashes': {}, 'nativeRenTrack': {'status': 'NOT_RUN'},
              'nativeRenScene': {'status': 'NOT_RUN'}}
    host = None
    try:
        host = CURVES.DRIVER.Host(run, core, njc, CURVES.DRIVER.free_port())
        host.wait_ready()
        for name, xml in fixtures().items():
            for scene in (False, True):
                label = name + ('-scene' if scene else '-track')
                original = KEY.caff_pack(xml)
                source, saved = run / (label + '.can3'), run / (label + '-saved.can3')
                source.write_bytes(original)
                result['generatedInputHashes'][str(source)] = digest(source)
                host.call(label + '-open', 'open', {'path': str(source)})
                before, model = KEY.state(host, label + '-initial')
                if name.startswith('underscore'):
                    curves = model['scenes'][0]['tracks'][0]['underscorePartOpacityCurves']
                    assert len(curves) == 2 and curves[0]['id'] == curves[1]['id']
                    assert curves[0]['mapKey'] != curves[1]['mapKey']
                    assert curves[0]['sourcePart'] != curves[1]['sourcePart']
                expected = copy.deepcopy(model)
                arguments = {'sceneGuid': 'scene-guid'}
                if scene:
                    expected['scenes'] = []
                    tool = 'live2d_animation_scene_delete'
                else:
                    expected['scenes'][0]['tracks'] = []
                    arguments['trackGuid'] = 'track-guid'
                    tool = 'live2d_animation_track_delete'
                _, after = KEY.edit(host, label, tool, **arguments)
                assert after == expected
                KEY.refuse(host, label + '-stale', tool, {**arguments, 'expectedRevision': before['revision']}, 'revision conflict')
                host.call(label + '-undo', 'undo', {})
                assert KEY.state(host, label + '-undone')[1] == model
                host.call(label + '-redo', 'redo', {})
                assert KEY.state(host, label + '-redone')[1] == expected
                data = CURVES.save_and_reopen(host, label, saved, expected, original)
                assert KEY.decode_entries(data)[1] == deleted_xml(xml, scene), 'XML outside deletion closure/count changed'
                assert source.read_bytes() == original
                result['cases'].append(label)
        base = fixtures()['underscore-all-members']
        for name, xml, message in [
            ('nested-id', base.replace(b'keep-part-a', b"<Opaque xs.id='unreferenced-nested'/>"), 'nested shared'),
            ('outside-ref', base.replace(b'</shared>', b"<Opaque xs.ref='part-a'/></shared>"), 'outside its owner'),
        ]:
            for scene in (False, True):
                label = name + ('-scene' if scene else '-track')
                source = run / (label + '.can3')
                source.write_bytes(KEY.caff_pack(xml))
                result['generatedInputHashes'][str(source)] = digest(source)
                host.call(label + '-open', 'open', {'path': str(source)})
                arguments = {'sceneGuid': 'scene-guid'}
                tool = 'live2d_animation_scene_delete'
                if not scene:
                    tool = 'live2d_animation_track_delete'
                    arguments['trackGuid'] = 'track-guid'
                KEY.refuse(host, label, tool, arguments, message)
                result['refusals'].append(label)
        if native is not None:
            source, saved = run / 'native-ren.can3', run / 'native-ren-saved.can3'
            original = native.read_bytes()
            source.write_bytes(original)
            result['generatedInputHashes'][str(source)] = digest(source)
            result['nativeRenTrack'] = native_ren_track(host, source, saved, original)
            result['nativeRenScene'] = native_ren_scene(host, source, run / 'native-ren-scene-saved.can3', original)
        result['savedArtifactHashes'] = {str(path): digest(path) for path in run.glob('*-saved.can3')}
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
            assert {p: digest(Path(p)) for p in inputs} == inputs
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)} == hashes
            assert all(digest(Path(p)) == h for p, h in result['generatedInputHashes'].items())
        except BaseException:
            result['status'] = 'FAIL'
            result['cleanupError'] = traceback.format_exc()
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(run)


if __name__ == '__main__':
    main()
