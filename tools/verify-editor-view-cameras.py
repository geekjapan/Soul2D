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

spec = importlib.util.spec_from_file_location('camera_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
KEY = HELPER.load('camera_can3_fixture', 'tools/verify-can3-key-structure.py')


def main():
    parser = argparse.ArgumentParser(description='Verify per-view camera transitions and Same display position over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-view-cameras'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), KEY.FIXTURE, native,
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/SKILL.md', ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status':'FAIL', 'scope':'Per-view camera transitions, sync, ID reuse, replacement, restart, native-format and headless boundaries',
              'sourceHashes':hashes, 'binaries':binary_hashes, 'nativeGui':'NOT_RUN', 'realAcpAgent':'NOT_RUN',
              'vendorAcceptance':'NOT_RUN', 'cleanup':[], 'phases':[]}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        can3 = run / 'source.can3'
        can3.write_bytes(KEY.caff_pack(KEY.FIXTURE.read_bytes()))
        originals = {p:p.read_bytes() for p in [source,can3,native]}
        env = HELPER.HARNESS.editor_env(run)
        for phase in ['headless','first','restarted','cmo3','can3']:
            path = {'cmo3':native,'can3':can3}.get(phase,source)
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(path)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, env=env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = HELPER.document(peer.snapshot())
                fresh = HELPER.state(peer)
                if phase == 'headless':
                    tools = peer.rpc('tools/list', {})['tools']
                    schema = next(t for t in tools if t['name'] == 'editor_camera_sync')['inputSchema']
                    assert schema['properties'] == {} and schema['additionalProperties'] is False
                    response = peer.rpc('tools/call', {'name':'editor_camera_sync','arguments':{}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'editor_camera_sync needs the editor: this host has no window'
                    assert 'viewCameras' not in fresh
                elif phase == 'can3':
                    assert fresh['viewCameras'] is None
                    response = peer.rpc('tools/call', {'name':'editor_camera_sync','arguments':{}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'view camera synchronization is available only for INX and CMO3 documents'
                    assert HELPER.state(peer) == fresh
                else:
                    saved = (run / 'config/settings.json').read_bytes()
                    initial_camera = {key:fresh['camera'][key] for key in ['centre','zoom']}
                    assert fresh['viewCameras'] == {'active':1,'views':[{'id':1,'camera':initial_camera}]}
                    assert fresh['camera']['mirror'] is False
                    peer.op('editor_camera_sync')
                    peer.op('editor_camera_set',centre=[10,-5],zoom=2,mirror=True)
                    peer.op('view_add')
                    peer.op('editor_camera_set',centre=[20,8],zoom=0.5)
                    peer.op('view_select',id=1)
                    answer = peer.op('editor_camera_set',zoom=3)
                    assert answer['camera']['centre'] == [10,-5]
                    assert answer['viewCameras']['views'][1]['camera'] == {'centre':[20,8],'zoom':0.5}
                    peer.op('view_add')
                    peer.op('view_select',id=2)
                    synced = peer.op('editor_camera_sync')
                    expected = {'centre':[20,8],'zoom':0.5}
                    assert synced['viewCameras'] == {'active':2,'views':[{'id':i,'camera':expected} for i in [1,2,3]]}
                    assert peer.op('editor_camera_sync')['viewCameras'] == synced['viewCameras']
                    response = peer.rpc('tools/call', {'name':'editor_camera_sync','arguments':{'all':True}})
                    assert response['isError'] is True
                    assert HELPER.state(peer)['viewCameras'] == synced['viewCameras']
                    for i in [1,2,3]:
                        peer.op('view_select',id=i)
                        camera = HELPER.state(peer)['camera']
                        assert camera['centre'] == expected['centre'] and camera['zoom'] == expected['zoom']
                        assert camera['mirror'] is True
                    peer.op('editor_camera_set',centre=[-40,60],zoom=4)
                    peer.op('view_remove',id=3)
                    peer.op('view_add')
                    reused = HELPER.state(peer)
                    assert reused['viewCameras']['active'] == 3
                    assert reused['viewCameras']['views'][2]['camera'] == expected
                    assert HELPER.document(peer.snapshot()) == baseline
                    assert (run / 'config/settings.json').read_bytes() == saved
                    peer.op('new')
                    replaced = HELPER.state(peer)
                    replacement_camera = {key:replaced['camera'][key] for key in ['centre','zoom']}
                    assert replaced['viewCameras'] == {'active':1,'views':[{'id':1,'camera':replacement_camera}]}
                    assert replaced['camera']['mirror'] is True
                if phase in ['headless','can3']:
                    assert HELPER.document(peer.snapshot()) == baseline
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert all(p.read_bytes() == data for p,data in originals.items())
        result.update(status='PASS', originalFiles={str(p):HELPER.digest(p) for p in originals})
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
