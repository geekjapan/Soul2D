#!/usr/bin/env python3
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('viewport_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
KEY = HELPER.load('viewport_can3_fixture', 'tools/verify-can3-key-structure.py')
DEFAULT = {'vertices': True, 'bounds': True, 'orientation': True, 'background': [0, 0, 0], 'partSort': 'z_sort'}


def main():
    parser = argparse.ArgumentParser(description='Verify viewport overlays, RGB background and INX Part menu sorting over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-pick-sort'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), KEY.FIXTURE, native,
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/SKILL.md', ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status': 'FAIL', 'scope': 'Viewport overlays, RGB background and INX Part menu sorting, atomic refusals, replacement and restart',
              'sourceHashes': hashes, 'binaries': binary_hashes, 'nativeGui': 'NOT_RUN', 'realAcpAgent': 'NOT_RUN',
              'vendorAcceptance': 'NOT_RUN', 'cleanup': [], 'phases': []}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        can3 = run / 'source.can3'
        can3.write_bytes(KEY.caff_pack(KEY.FIXTURE.read_bytes()))
        originals = {p:p.read_bytes() for p in [source, can3, native]}
        env = HELPER.HARNESS.editor_env(run)
        for phase in ['headless', 'first', 'restarted', 'cmo3', 'can3']:
            path = {'cmo3':native, 'can3':can3}.get(phase, source)
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(path)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = HELPER.document(peer.snapshot())
                schema = next(t for t in peer.rpc('tools/list', {})['tools'] if t['name'] == 'editor_viewport_set')['inputSchema']
                assert schema['additionalProperties'] is False
                assert set(schema['properties']) == set(DEFAULT)
                assert schema['properties']['partSort']['enum'] == ['z_sort', 'size_sort']
                assert schema['properties']['background']['minItems'] == schema['properties']['background']['maxItems'] == 3
                if phase == 'headless':
                    assert 'viewport' not in HELPER.state(peer)
                    response = peer.rpc('tools/call', {'name': 'editor_viewport_set', 'arguments': {'partSort': 'size_sort'}})
                    assert response['isError'] is True
                    assert 'editor_viewport_set needs the editor: this host has no window' in response['content'][0]['text']
                elif phase in ['cmo3', 'can3']:
                    assert HELPER.state(peer)['viewport'] == {**DEFAULT, 'partSort':None}
                    settings = run / 'config/settings.json'
                    saved = settings.read_bytes()
                    before = HELPER.state(peer)
                    response = peer.rpc('tools/call', {'name':'editor_viewport_set', 'arguments':{'partSort':'size_sort', 'background':[255,0,0]}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'Part menu sorting is available only for INX documents'
                    assert HELPER.state(peer) == before
                    seen = peer.op('editor_viewport_set', background=[12,34,56])['viewport']
                    assert seen == {**DEFAULT, 'partSort':None, 'background':[12,34,56]}
                    assert settings.read_bytes() == saved
                else:
                    assert HELPER.state(peer)['viewport'] == DEFAULT
                    settings = run / 'config/settings.json'
                    saved = settings.read_bytes()
                    expected = {'vertices': False, 'bounds': False, 'orientation': False, 'background': [255, 0, 37], 'partSort':'size_sort'}
                    for update in [expected.copy(), {'bounds': True}, {'vertices': True}, {'orientation': True},
                                   {'background': [0, 0, 0]}, {'background': [12, 34, 56]},
                                   {'partSort':'z_sort'}, {'partSort':'size_sort'}]:
                        expected.update(update)
                        assert peer.op('editor_viewport_set', **update)['viewport'] == expected
                        assert HELPER.state(peer)['viewport'] == expected
                        assert HELPER.document(peer.snapshot()) == baseline
                    assert peer.op('editor_viewport_set', **expected)['viewport'] == expected
                    for arguments in [{}, {'vertices': None}, {'bounds': 0}, {'orientation': 'yes'},
                                      {'background': None}, {'background': [0, 0]}, {'background': [0, 0, 0, 255]},
                                      {'background': [-1, 0, 0]}, {'background': [0, 256, 0]},
                                      {'background': [0, 0, 0.5]}, {'background': [0, '0', 0]},
                                      {'vertices': False, 'background': [0, 0, 999]}, {'bounds': False, 'extra': True},
                                      {'partSort':None}, {'partSort':True}, {'partSort':'Size-Sort'},
                                      {'background':[255,0,0], 'partSort':'unknown'}]:
                        response = peer.rpc('tools/call', {'name': 'editor_viewport_set', 'arguments': arguments})
                        assert response['isError'] is True, response
                        assert HELPER.state(peer)['viewport'] == expected
                    assert settings.read_bytes() == saved, 'viewport changes are not settings writes'
                    assert HELPER.document(peer.snapshot()) == baseline
                    peer.op('open', path=str(native))
                    assert HELPER.state(peer)['viewport'] == {**expected, 'partSort':None}
                    peer.op('open', path=str(source))
                    assert HELPER.state(peer)['viewport'] == expected
                    baseline = HELPER.document(peer.snapshot())
                assert HELPER.document(peer.snapshot()) == baseline
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert all(p.read_bytes()==data for p,data in originals.items())
        result.update(status='PASS', originalFiles={str(p):HELPER.digest(p) for p in originals}, viewportOnlySessionRestart='PASS')
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if proc is not None:
                result['cleanup'].append(HELPER.stop(proc))
            assert all(not row['running'] for row in result['cleanup'])
            assert {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)} == hashes
            assert {str(p): HELPER.digest(p) for p in binaries} == binary_hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
