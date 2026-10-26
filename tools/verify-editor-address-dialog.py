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
spec = importlib.util.spec_from_file_location('address_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
KEY = HELPER.load('address_can3_fixture', 'tools/verify-can3-key-structure.py')
TARGETS = ['configuration', 'online_documentation', 'soul2d_live_documentation', 'bug_report', 'feature_request']


def main():
    parser = argparse.ArgumentParser(description='Verify fixed address dialogs over production MCP; no native UI or OS clipboard verification.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-address-dialog'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), KEY.FIXTURE, native,
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'rust/tests/app_shell.rs', ROOT / 'rust/tests/last_gaps.rs',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/SKILL.md', ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status':'FAIL', 'scope':'Fixed address dialog targets, validation, unchanged document/settings, replacement and restart',
              'sourceHashes':hashes, 'binaries':binary_hashes, 'nativeGui':'NOT_RUN', 'osClipboard':'NOT_RUN',
              'realAcpAgent':'NOT_RUN', 'vendorAcceptance':'NOT_RUN', 'cleanup':[], 'phases':[]}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        can3 = run / 'source.can3'
        can3.write_bytes(KEY.caff_pack(KEY.FIXTURE.read_bytes()))
        originals = {p:p.read_bytes() for p in [source, can3, native]}
        env = HELPER.HARNESS.editor_env(run)
        settings = run / 'config/settings.json'
        for phase in ['headless', 'first', 'restarted', 'cmo3', 'can3']:
            path = {'cmo3':native, 'can3':can3}.get(phase, source)
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(path)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                schema = next(t for t in peer.rpc('tools/list', {})['tools'] if t['name'] == 'editor_address_dialog')['inputSchema']
                assert schema['additionalProperties'] is False
                assert set(schema['properties']) == {'action', 'target'}
                assert schema['required'] == ['action']
                assert schema['properties']['action']['enum'] == ['open', 'copy', 'close']
                assert schema['properties']['target']['enum'] == TARGETS
                if phase == 'headless':
                    assert 'windows' not in HELPER.state(peer)
                    for action in ['open', 'copy', 'close']:
                        arguments = {'action':action, **({'target':'configuration'} if action == 'open' else {})}
                        response = peer.rpc('tools/call', {'name':'editor_address_dialog', 'arguments':arguments})
                        assert response['isError'] is True
                        assert response['content'][0]['text'] == 'editor_address_dialog needs the editor: this host has no window'
                else:
                    assert HELPER.state(peer)['windows']['addressDialog'] is None
                    saved = settings.read_bytes()
                    baseline = HELPER.document(peer.snapshot())
                    response = peer.rpc('tools/call', {'name':'editor_address_dialog', 'arguments':{'action':'copy'}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'no address dialog is open'
                    for target in TARGETS:
                        expected = peer.op('editor_address_dialog', action='open', target=target)['windows']['addressDialog']
                        assert set(expected) == {'target','title','address'} and expected['target'] == target
                        assert expected['title'] and expected['address']
                        if target == 'configuration':
                            assert expected['address'] == str(settings.parent)
                        else:
                            assert expected['address'].startswith('https://')
                        assert HELPER.state(peer)['windows']['addressDialog'] == expected
                        assert peer.op('editor_address_dialog', action='copy')['windows']['addressDialog'] == expected
                        assert HELPER.document(peer.snapshot()) == baseline
                    before = HELPER.state(peer)
                    for arguments in [{}, [], {'action':None}, {'action':True}, {'action':'launch'},
                                      {'action':'open'}, {'action':'open','target':'unknown'}, {'action':'open','target':None},
                                      {'action':'open','target':1}, {'action':'copy','target':'bug_report'},
                                      {'action':'copy','target':None}, {'action':'close','target':None},
                                      {'action':'open','target':'bug_report','url':'https://example.com'},
                                      {'action':'copy','title':'Override'}, {'action':'close','path':'/tmp'}]:
                        response = peer.rpc('tools/call', {'name':'editor_address_dialog','arguments':arguments})
                        assert response['isError'] is True, response
                        assert HELPER.state(peer) == before
                    assert settings.read_bytes() == saved
                    assert HELPER.document(peer.snapshot()) == baseline
                    if phase == 'first':
                        for replacement in [native, can3, source]:
                            peer.op('open', path=str(replacement))
                            assert HELPER.state(peer)['windows']['addressDialog'] == expected
                        assert peer.op('editor_layout_reset')['windows']['addressDialog'] == expected
                    else:
                        for _ in range(2):
                            assert peer.op('editor_address_dialog', action='close')['windows']['addressDialog'] is None
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert all(p.read_bytes() == data for p,data in originals.items())
        result.update(status='PASS', originalFiles={str(p):HELPER.digest(p) for p in originals},
                      copyAcknowledgment='egui queue only; Rust tests inspect CopyText output')
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
