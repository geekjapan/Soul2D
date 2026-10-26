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
spec = importlib.util.spec_from_file_location('preview_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)


def main():
    parser = argparse.ArgumentParser(description='Verify Guide Settings visibility and physics preview over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-preview-controls'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__), native,
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status':'FAIL', 'scope':'Guide window and physics preview, validation, restart, native-format limits and headless refusal',
              'sourceHashes':hashes, 'binaries':binary_hashes, 'nativeGui':'NOT_RUN', 'realAcpAgent':'NOT_RUN',
              'vendorAcceptance':'NOT_RUN', 'cleanup':[], 'phases':[]}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        original = source.read_bytes()
        env = HELPER.HARNESS.editor_env(run)
        for phase in ['headless','first','restarted','native']:
            path = native if phase == 'native' else source
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(path)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = HELPER.document(peer.snapshot())
                if phase == 'headless':
                    for name, arguments in [('editor_guides_set',{'open':True}),('editor_physics_preview_set',{'enabled':True})]:
                        response = peer.rpc('tools/call', {'name':name,'arguments':arguments})
                        assert response['isError'] is True
                        assert f'{name} needs the editor: this host has no window' in response['content'][0]['text']
                    assert 'physicsPreview' not in HELPER.state(peer)
                else:
                    available = phase != 'native'
                    fresh = HELPER.state(peer)
                    assert fresh['windows']['guideSettings'] is False
                    assert fresh['physicsPreview'] == {'enabled':False,'available':available}
                    saved = (run / 'config/settings.json').read_bytes()
                    for opened in [True,True,False,True]:
                        answer = peer.op('editor_guides_set', open=opened)
                        assert answer['windows']['guideSettings'] == opened
                        assert HELPER.state(peer)['windows']['guideSettings'] == opened
                        assert answer['guides'] == fresh['guides']
                    for arguments in [{'open':None},{'open':1},{'open':'yes'},
                                      {'open':False,'lines':[{'axis':'bad','position':0}]}, {'open':False,'extra':True}]:
                        response = peer.rpc('tools/call', {'name':'editor_guides_set','arguments':arguments})
                        assert response['isError'] is True
                        assert HELPER.state(peer)['windows']['guideSettings'] is True
                        assert HELPER.state(peer)['guides'] == fresh['guides']
                    if available:
                        for enabled in [True,True,False,True]:
                            answer = peer.op('editor_physics_preview_set', enabled=enabled)
                            assert answer['physicsPreview'] == {'enabled':enabled,'available':True}
                            assert HELPER.state(peer)['physicsPreview'] == answer['physicsPreview']
                    else:
                        response = peer.rpc('tools/call', {'name':'editor_physics_preview_set','arguments':{'enabled':True}})
                        assert response['isError'] is True
                        assert response['content'][0]['text'] == 'physics preview is available only for INX documents'
                        assert peer.op('editor_physics_preview_set', enabled=False)['physicsPreview'] == {'enabled':False,'available':False}
                    expected = HELPER.state(peer)['physicsPreview']
                    for arguments in [{},{'enabled':None},{'enabled':0},{'enabled':'yes'},{'enabled':False,'extra':True}]:
                        response = peer.rpc('tools/call', {'name':'editor_physics_preview_set','arguments':arguments})
                        assert response['isError'] is True
                        assert HELPER.state(peer)['physicsPreview'] == expected
                    assert (run / 'config/settings.json').read_bytes() == saved
                assert HELPER.document(peer.snapshot()) == baseline
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert source.read_bytes() == original
        result.update(status='PASS', sourceSha256=HELPER.digest(source))
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
            (run / 'result.json').write_text(json.dumps(result,indent=2)+'\n')
            print(run)


if __name__ == '__main__':
    main()
