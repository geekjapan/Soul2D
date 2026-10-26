#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import socket
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("part_key_proof", ROOT / "tools/verify-can3-key-structure.py")
KEY = importlib.util.module_from_spec(spec)
spec.loader.exec_module(KEY)
FIXTURE = ROOT / "rust/tests/fixtures/can3-part-opacity.xml"
IDS = {"sceneGuid": "scene-guid", "trackGuid": "track-guid", "curveId": "live2DPartsOpacity_PartArmA"}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def part(model):
    curves = model["scenes"][0]["tracks"][0]["partOpacityCurves"]
    return next(c for c in curves if c["id"] == IDS["curveId"])


def attribute(text, storage='part-a'):
    matches = list(re.finditer(r'<CMvAttrF\b(?=[^>]*\bxs.id="' + re.escape(storage) + r'")[^>]*>', text))
    assert len(matches) == 1, 'selected attribute storage is not unique'
    start = matches[0].start()
    end = text.index('</CMvAttrF>', start) + len('</CMvAttrF>')
    return start, end


def require_preservation(original, saved, storage='part-a', retained_points=((1, 1),)):
    a, old_xml = KEY.decode_entries(original)
    b, new_xml = KEY.decode_entries(saved)
    assert original[:58] == saved[:58] and original[-2:] == saved[-2:] == b'bc'
    assert [e[:4] for e in a] == [e[:4] for e in b]
    assert [e for e in a if e[0][0] != 'main.xml'] == [e for e in b if e[0][0] != 'main.xml']
    old, new = old_xml.decode(), new_xml.decode()
    x, y = attribute(old, storage)
    u, v = attribute(new, storage)
    assert old[:x] == new[:u] and old[y:] == new[v:], 'XML outside selected attribute changed'
    before, after = old[x:y], new[u:v]
    if retained_points:
        old_points = KEY.element_spans(before, 'points', 'CBezierPt')
        new_points = KEY.element_spans(after, 'points', 'CBezierPt')
        for old_index, new_index in retained_points:
            assert before[slice(*old_points[old_index])] == after[slice(*new_points[new_index])], 'retained original key changed'
    for tag, name in [('array', 'points'), ('carray_list', 'curveTypes')]:
        pattern = rf'<{tag}\b[^>]*xs.n="{name}"[^>]*(?:/>|>.*?</{tag}>)'
        old_array = re.search(pattern, before, re.S)
        new_array = re.search(pattern, after, re.S)
        assert old_array and new_array
        after = after[:new_array.start()] + old_array[0] + after[new_array.end():]
    for name in ['curMin', 'curMax']:
        pattern = rf'<d xs.n="{name}">([^<]*)</d>'
        old_cache, new_cache = re.search(pattern, before), re.search(pattern, after)
        assert old_cache and new_cache
        after = after[:new_cache.start(1)] + old_cache[1] + after[new_cache.end(1):]
    assert after == before, 'unrelated attribute XML changed'


def main():
    parser = argparse.ArgumentParser(description="Verify existing modern CAN3 PartOpacity key edits through owned MCP.")
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    evidence = ROOT / 'build/verification/ani145-parts'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    sources = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__),
        Path(KEY.CAFF.CORE.__file__),
        FIXTURE, ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/live2d-authoring.md', ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)}
    binaries = {str(p): digest(p) for p in (core, njc)}
    result = {'status': 'FAIL', 'scope': 'Generated modern PartOpacity native-source edits; no vendor or native GUI acceptance',
              'sourceHashes': hashes, 'binaries': binaries, 'nativeGui': 'SKIP', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        xml = FIXTURE.read_bytes()
        original = KEY.caff_pack(xml)
        source = run / 'source.can3'
        source.write_bytes(original)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        host = KEY.DRIVER.Host(run, core, njc, port)
        host.wait_ready()
        (run / 'instance.json').write_text(json.dumps({'pid': host.child.pid, 'port': port}) + '\n')
        host.call('open', 'open', {'path': str(source)})
        initial, initial_model = KEY.state(host, 'initial')
        tools = KEY.rpc(host, 'tools', 'tools', 'list')['tools']
        assert all(any(t['name'] == 'live2d_animation_' + name for t in tools)
                   for name in ['key_set', 'key_add', 'key_delete', 'handle_set', 'curve_type_set'])
        for label, name, fields in [
            ('set0', 'key_set', {'anchorFrame': 0, 'value': 0.25}),
            ('add5', 'key_add', {'anchorFrame': 5, 'value': 0.6}),
            ('handle5', 'handle_set', {'anchorFrame': 5, 'side': 'next', 'frame': 6.5, 'value': 0.7}),
            ('type5', 'curve_type_set', {'anchorFrame': 5, 'type': 'BEZIER'}),
            ('delete0', 'key_delete', {'anchorFrame': 0}),
        ]:
            snap, before = KEY.state(host, label + '-before')
            _, model = KEY.edit(host, label, 'live2d_animation_' + name, **IDS, **fields)
            without = json.loads(json.dumps(model))
            without['scenes'][0]['tracks'][0]['partOpacityCurves'][0] = before['scenes'][0]['tracks'][0]['partOpacityCurves'][0]
            assert without == before, label
            host.call(label + '-undo', 'undo', {})
            assert KEY.state(host, label + '-undone')[1] == before
            host.call(label + '-redo', 'redo', {})
            assert KEY.state(host, label + '-redone')[1] == model
        assert [p['anchor']['frame'] for p in part(model)['points']] == [5, 10]
        assert part(model)['curveTypes'] == ['BEZIER', 'LINEAR']
        assert part(model)['points'][0]['anchor']['value'] == 0.6
        assert part(model)['points'][0]['next'] == {'frame': 6.5, 'integerFrame': 5, 'value': 0.7, 'positionOptimized': False}
        KEY.refuse(host, 'stale', 'live2d_animation_key_set',
                   {**IDS, 'anchorFrame': 5, 'value': 0.4, 'expectedRevision': initial['revision']}, 'revision conflict')
        KEY.refuse(host, 'missing-legacy', 'live2d_animation_key_set',
                   {**IDS, 'curveId': 'live2DPartsOpacity:missing-legacy-attribute', 'anchorFrame': 0, 'value': 0.4}, 'not found')
        saved = run / 'saved.can3'
        host.call('save', 'save', {'path': str(saved)})
        require_preservation(original, saved.read_bytes())
        host.call('reopen', 'open', {'path': str(saved)})
        assert KEY.state(host, 'reopened')[1] == model
        assert source.read_bytes() == original
        bad = run / 'shared.can3'
        bad_xml = xml.decode().replace('</shared>', '<CMvAttrF xs.n="futureAlias" xs.ref="part-a"/></shared>')
        bad.write_bytes(KEY.caff_pack(bad_xml.encode()))
        host.call('open-shared', 'open', {'path': str(bad)})
        for name, fields in [('key_set', {'anchorFrame': 0, 'value': 0.4}),
                             ('key_add', {'anchorFrame': 5, 'value': 0.4}),
                             ('key_delete', {'anchorFrame': 0}),
                             ('handle_set', {'anchorFrame': 0, 'side': 'next', 'value': 0.4}),
                             ('curve_type_set', {'anchorFrame': 0, 'type': 'STEP'})]:
            KEY.refuse(host, 'shared-' + name, 'live2d_animation_' + name, {**IDS, **fields}, 'outside its owner')
        result.update(status='PASS', saved=str(saved), initialModelName=initial_model['name'])
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                (run / 'cleanup.json').write_text(json.dumps({'pid': host.child.pid, 'exit': host.child.returncode,
                                                             'running': host.child.poll() is None}) + '\n')
            assert {str(p): digest(p) for p in (core, njc)} == binaries
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)} == hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
