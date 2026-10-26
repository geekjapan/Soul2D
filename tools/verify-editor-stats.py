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


def preference(peer):
    listing = peer.op('editor_settings_get')
    return next(row for row in listing['settings'] if row['key'] == 'NerdStats')


def main():
    parser = argparse.ArgumentParser(description='Verify Stats for Nerds over production MCP and settings restart; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-stats'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status': 'FAIL', 'scope': 'NerdStats preference, window readback, typed refusals and persistence restart',
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
                    for name, arguments in [('editor_settings_get', {}),
                                            ('editor_settings_set', {'key': 'NerdStats', 'value': True})]:
                        response = peer.rpc('tools/call', {'name': name, 'arguments': arguments})
                        assert response['isError'] is True
                        assert f'{name} needs the editor: this host has no window' in response['content'][0]['text']
                else:
                    initial = phase == 'restarted'
                    assert preference(peer) == {'key': 'NerdStats', 'label': 'Stats for Nerds', 'type': 'boolean',
                                                'value': initial, 'writable': True}
                    assert HELPER.state(peer)['windows']['nerdStats'] is initial
                    for visible in [True, False, True]:
                        response = peer.op('editor_settings_set', key='NerdStats', value=visible)
                        assert response['saved'] is True and response['setting']['value'] is visible
                        assert preference(peer)['value'] is visible
                        assert HELPER.state(peer)['windows']['nerdStats'] is visible
                        assert json.loads((run / 'config/settings.json').read_text())['NerdStats'] is visible
                        assert HELPER.document(peer.snapshot()) == baseline
                    settings = (run / 'config/settings.json').read_bytes()
                    for arguments in [{'key': 'NerdStats', 'value': value} for value in [None, 1, 'true', {}]] + [
                            {'key': 'NerdStats', 'value': False, 'extra': True},
                            {'key': 'unknown', 'value': True}, {'key': 'MCP.Port', 'value': 1234}]:
                        response = peer.rpc('tools/call', {'name': 'editor_settings_set', 'arguments': arguments})
                        assert response['isError'] is True
                        assert HELPER.state(peer)['windows']['nerdStats'] is True
                        assert (run / 'config/settings.json').read_bytes() == settings
                        assert HELPER.document(peer.snapshot()) == baseline
                assert HELPER.document(peer.snapshot()) == baseline
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert source.read_bytes() == original
        result.update(status='PASS', sourceSha256=HELPER.digest(source), settingsRestart='PASS')
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
