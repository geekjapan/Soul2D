#!/usr/bin/env python3
import argparse
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('parameter_proof', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
KEY = HELPER.load('parameter_can3_fixture', 'tools/verify-can3-key-structure.py')


def fixture(path):
    model, tail = HELPER.CORE.fixture(path)
    model['param'] = [{'uuid':10,'name':'Blink','is_vec2':False,'min':[0,0],'max':[1,0],
                       'defaults':[0,0],'axis_points':[[0,1],[0]],'merge_mode':'Additive','bindings':[]}]
    for name in ['Idle','Walk']:
        model['animations'][name] = {'length':60,'timestep':1/60,'additive':False,'animationWeight':1,
                                     'leadIn':-1,'leadOut':-1,'lanes':[], 'bookmarks':[
                                         {'label':'Other','x':0,'y':0,'parameters':[]},
                                         {'label':'Eye','x':1,'y':2,'parameters':[10]}]}
    raw = json.dumps(model, separators=(',',':')).encode()
    path.write_bytes(b'TRNSRTS\0' + struct.pack('>I',len(raw)) + raw + tail)


def main():
    parser = argparse.ArgumentParser(description='Verify INX Parameters filter, clip choice and transient bookmarks over production MCP; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(),args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-parameter-filter'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-',dir=evidence))
    native = ROOT / 'rust/tests/fixtures/physics3-editor.cmo3'
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), KEY.FIXTURE, native,
        ROOT / 'rust/Cargo.toml',ROOT / 'rust/Cargo.lock',ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md',ROOT / 'doc/gui-operation-inventory.md',ROOT / '.cursor/skills/operate-soul2d/SKILL.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)):HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p):HELPER.digest(p) for p in binaries}
    result = {'status':'FAIL','scope':'INX panel filter, selected clip, resolved bookmarks, validation and lifecycle with Outline hidden',
              'sourceHashes':hashes,'binaries':binary_hashes,'nativeGui':'NOT_RUN','realAcpAgent':'NOT_RUN',
              'vendorAcceptance':'NOT_RUN','cleanup':[],'phases':[]}
    proc = None
    try:
        source = run / 'source.inx'
        fixture(source)
        can3 = run / 'source.can3'
        can3.write_bytes(KEY.caff_pack(KEY.FIXTURE.read_bytes()))
        originals = {p:p.read_bytes() for p in [source,can3,native]}
        env = HELPER.HARNESS.editor_env(run)
        settings = run / 'config/settings.json'
        stored = json.loads(settings.read_text())
        stored['Outline.visible'] = False
        settings.write_text(json.dumps(stored))
        for phase in ['headless','first','restarted','cmo3','can3']:
            path = {'cmo3':native,'can3':can3}.get(phase,source)
            command = [str(binaries[1]),'--mcp'] if phase == 'headless' else [str(binaries[0]),'--mcp','--open',str(path)]
            with (run / (phase+'-wire.jsonl')).open('w') as wire, (run / (phase+'.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr,text=True,env=env)
                peer = HELPER.HARNESS.Mcp(proc,wire)
                peer.rpc('initialize',{})
                baseline = HELPER.document(peer.snapshot())
                fresh = HELPER.state(peer)
                if phase == 'headless':
                    tools = peer.rpc('tools/list',{})['tools']
                    schema = next(t for t in tools if t['name']=='editor_parameters_set')['inputSchema']
                    assert set(schema['properties']) == {'filter','animation','bookmark'} and schema['additionalProperties'] is False
                    response = peer.rpc('tools/call',{'name':'editor_parameters_set','arguments':{'filter':'Blink'}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'editor_parameters_set needs the editor: this host has no window'
                    assert 'parameterPanel' not in fresh
                elif phase in ['cmo3','can3']:
                    assert fresh['parameterPanel'] is None
                    response = peer.rpc('tools/call',{'name':'editor_parameters_set','arguments':{'filter':'bad'}})
                    assert response['isError'] is True
                    assert response['content'][0]['text'] == 'the Parameters panel is available only for INX documents'
                    assert HELPER.state(peer) == fresh
                else:
                    assert fresh['panels']['outline'] is False
                    assert fresh['parameterPanel'] == {'filter':'','animation':'Idle','bookmark':None}
                    saved = settings.read_bytes()
                    peer.op('animation_seek',name='Idle',frame=2)
                    preview = peer.snapshot()['animation']
                    baseline = HELPER.document(peer.snapshot())
                    chosen = peer.op('editor_parameters_set',filter='bLi',animation='Walk',bookmark={'labelIndex':1})
                    assert chosen['parameterPanel'] == {'filter':'bLi','animation':'Walk','bookmark':{
                        'animation':'Walk','label':'Eye','point':[1,2],'parameters':[10]}}
                    assert peer.snapshot()['animation'] == preview
                    assert HELPER.state(peer)['parameterPanel'] == chosen['parameterPanel']
                    part = peer.op('editor_parameters_set',bookmark={'part':2,'point':[-3,4]})
                    assert part['parameterPanel'] == {'filter':'bLi','animation':'Walk','bookmark':{
                        'animation':'Walk','label':'Card','point':[-3,4],'parameters':[]}}
                    for arguments in [{},{'filter':None},{'animation':1},{'filter':'bad','animation':'Missing'},
                                      {'filter':'bad','animation':'Idle','bookmark':{'labelIndex':99}},
                                      {'bookmark':True},{'bookmark':{'part':2}},
                                      {'bookmark':{'part':1,'point':[0,0]}},{'bookmark':{'part':99,'point':[0,0]}},
                                      {'bookmark':{'part':2,'point':[1.1e9,0]}},
                                      {'bookmark':{'part':2,'point':[0,0],'labelIndex':1}},
                                      {'bookmark':{'labelIndex':1,'parameters':[10]}},{'filter':'bad','extra':True}]:
                        response = peer.rpc('tools/call',{'name':'editor_parameters_set','arguments':arguments})
                        assert response['isError'] is True
                        assert HELPER.state(peer)['parameterPanel'] == part['parameterPanel']
                    peer.op('editor_parameters_set',bookmark=None)
                    assert HELPER.state(peer)['parameterPanel'] == {'filter':'bLi','animation':'Walk','bookmark':None}
                    peer.op('editor_parameters_set',bookmark={'labelIndex':1})
                    assert HELPER.document(peer.snapshot()) == baseline and settings.read_bytes() == saved
                    copied = HELPER.state(peer)['parameterPanel']
                    peer.op('animation_bookmark_delete',name='Walk',index=1)
                    assert HELPER.state(peer)['parameterPanel'] == copied
                    peer.op('animation_delete',name='Walk')
                    assert HELPER.state(peer)['parameterPanel'] == {'filter':'bLi','animation':'Idle','bookmark':None}
                    peer.op('save',path=str(run / (phase+'-changed.inx')))
                    peer.op('new')
                    assert HELPER.state(peer)['parameterPanel'] == {'filter':'bLi','animation':None,'bookmark':None}
                if phase in ['headless','cmo3','can3']:
                    assert HELPER.document(peer.snapshot()) == baseline
                result['phases'].append(phase)
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert all(p.read_bytes()==data for p,data in originals.items())
        result.update(status='PASS',originalFiles={str(p):HELPER.digest(p) for p in originals})
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
            result.update(status='FAIL',cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result,indent=2)+'\n')
            print(run)


if __name__ == '__main__':
    main()
