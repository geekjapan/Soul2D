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
spec = importlib.util.spec_from_file_location('native_parameter_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
KEY = HELPER.load('native_parameter_can3', 'tools/verify-can3-key-structure.py')


def main():
    parser = argparse.ArgumentParser(description='Verify native parameter filter over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-native-parameter-filter'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), KEY.FIXTURE, native,
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md', ROOT / '.cursor/skills/operate-soul2d/SKILL.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status':'FAIL', 'scope':'Native name-or-ID filter, validation, format boundaries and lifecycle',
              'sourceHashes':hashes, 'binaries':binary_hashes, 'nativeGui':'NOT_RUN', 'realAcpAgent':'NOT_RUN',
              'vendorAcceptance':'NOT_RUN', 'cleanup':[], 'phases':[]}
    proc = None
    try:
        inx = run / 'source.inx'
        HELPER.CORE.fixture(inx)
        can3 = run / 'source.can3'
        can3.write_bytes(KEY.caff_pack(KEY.FIXTURE.read_bytes()))
        originals = {p:p.read_bytes() for p in [inx, can3, native]}
        env = HELPER.HARNESS.editor_env(run)
        settings = run / 'config/settings.json'
        for phase in ['headless', 'inx', 'cmo3', 'restarted', 'can3', 'attached']:
            path = can3 if phase in ['can3', 'attached'] else native if phase in ['cmo3', 'restarted'] else inx
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(path)]
            with (run / (phase+'-wire.jsonl')).open('w') as wire, (run / (phase+'.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                if phase == 'attached':
                    peer.op('live2d_model_attach', path=str(native))
                    assert peer.snapshot()['nativePlayback']['parameters']
                baseline = HELPER.document(peer.snapshot())
                fresh = HELPER.state(peer)
                saved = settings.read_bytes()
                if phase == 'headless':
                    schema = next(t for t in peer.rpc('tools/list', {})['tools'] if t['name']=='editor_native_parameters_set')['inputSchema']
                    assert schema == {'type':'object', 'properties':{'filter':{'type':'string'}}, 'required':['filter'], 'additionalProperties':False}
                    response = peer.rpc('tools/call', {'name':'editor_native_parameters_set', 'arguments':{'filter':'test'}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'editor_native_parameters_set needs the editor: this host has no window'
                    assert 'nativeParameterPanel' not in fresh
                elif phase == 'inx':
                    assert fresh['nativeParameterPanel'] is None
                    response = peer.rpc('tools/call', {'name':'editor_native_parameters_set', 'arguments':{'filter':'bad'}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'the native parameter panel is available only for CMO3 and CAN3 documents'
                    assert HELPER.state(peer) == fresh
                else:
                    assert fresh['nativeParameterPanel'] == {'filter':''}
                    if phase == 'can3':
                        assert not peer.snapshot()['nativePlayback']['parameters']
                    for value in ['tEsT', '', '顔', '  ']:
                        changed = peer.op('editor_native_parameters_set', filter=value)
                        assert changed['nativeParameterPanel'] == {'filter':value}
                        assert HELPER.state(peer)['nativeParameterPanel'] == {'filter':value}
                        assert changed['parameterPanel'] is None
                    for arguments in [{}, {'filter':None}, {'filter':1}, {'filter':True}, {'filter':'bad','extra':True}]:
                        response = peer.rpc('tools/call', {'name':'editor_native_parameters_set', 'arguments':arguments})
                        assert response['isError'] is True
                        assert HELPER.state(peer)['nativeParameterPanel'] == {'filter':'  '}
                    assert HELPER.document(peer.snapshot()) == baseline
                    assert settings.read_bytes() == saved
                    peer.op('open', path=str(inx))
                    assert HELPER.state(peer)['nativeParameterPanel'] is None
                    peer.op('open', path=str(path))
                    assert HELPER.state(peer)['nativeParameterPanel'] == {'filter':'  '}
                    peer.op('editor_native_parameters_set', filter='retained until restart')
                if phase in ['headless', 'inx']:
                    assert HELPER.document(peer.snapshot()) == baseline
                    assert settings.read_bytes() == saved
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert all(p.read_bytes()==data for p,data in originals.items())
        result.update(status='PASS', originalFiles={str(p):HELPER.digest(p) for p in originals})
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if proc is not None:
                result['cleanup'].append(HELPER.stop(proc))
            assert all(not row['running'] for row in result['cleanup'])
            assert {str(p.relative_to(ROOT)):HELPER.digest(p) for p in sorted(paths)} == hashes
            assert {str(p):HELPER.digest(p) for p in binaries} == binary_hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
            print(run)


if __name__ == '__main__':
    main()
