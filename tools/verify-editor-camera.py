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
spec = importlib.util.spec_from_file_location('panel_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)


def camera(peer):
    value = HELPER.state(peer)['camera']
    return {key: value[key] for key in ['centre', 'zoom', 'mirror']}


def main():
    parser = argparse.ArgumentParser(description='Verify editor camera and Mirror View over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-camera'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status': 'FAIL', 'scope': 'Editor camera mirror, partial updates, refusals and session-only restart',
              'sourceHashes': hashes, 'binaries': binary_hashes, 'nativeGui': 'NOT_RUN',
              'realAcpAgent': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN', 'cleanup': []}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        original = source.read_bytes()
        env = HELPER.HARNESS.editor_env(run)
        for phase in ['headless', 'first', 'restarted']:
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(source)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = HELPER.document(peer.snapshot())
                if phase == 'headless':
                    tools = peer.rpc('tools/list', {})['tools']
                    schema = next(t for t in tools if t['name'] == 'editor_camera_set')['inputSchema']
                    assert schema['properties']['mirror'] == {'type': 'boolean'}
                    assert schema['additionalProperties'] is False
                    response = peer.rpc('tools/call', {'name': 'editor_camera_set', 'arguments': {'mirror': True}})
                    assert response['isError'] is True
                    assert 'editor_camera_set needs the editor: this host has no window' in response['content'][0]['text']
                else:
                    assert camera(peer)['mirror'] is False, 'Mirror View belongs to this window session only'
                    peer.op('editor_camera_set', centre=[10, -5], zoom=2, mirror=True)
                    expected = {'centre': [10, -5], 'zoom': 2, 'mirror': True}
                    assert camera(peer) == expected
                    for mirror in [False, True, True, False]:
                        answer = peer.op('editor_camera_set', mirror=mirror)
                        expected['mirror'] = mirror
                        assert {key: answer['camera'][key] for key in expected} == expected
                        assert camera(peer) == expected and HELPER.document(peer.snapshot()) == baseline
                    peer.op('editor_camera_set', mirror=True)
                    peer.op('editor_camera_set', zoom=0.5)
                    expected.update(zoom=0.5, mirror=True)
                    assert camera(peer) == expected
                    peer.op('editor_camera_set', centre=[-4, 6])
                    expected['centre'] = [-4, 6]
                    assert camera(peer) == expected
                    for arguments in [{}, {'mirror': None}, {'mirror': 1}, {'mirror': 'true'},
                                      {'mirror': False, 'zoom': 100}, {'mirror': False, 'centre': [1e12, 0]},
                                      {'mirror': False, 'centre': [0, 0], 'extra': True}]:
                        response = peer.rpc('tools/call', {'name': 'editor_camera_set', 'arguments': arguments})
                        assert response['isError'] is True
                        assert camera(peer) == expected and HELPER.document(peer.snapshot()) == baseline
                assert HELPER.document(peer.snapshot()) == baseline
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert source.read_bytes() == original
        result.update(status='PASS', sourceSha256=HELPER.digest(source), cameraOnlySessionRestart='PASS')
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
