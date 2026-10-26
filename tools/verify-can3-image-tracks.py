#!/usr/bin/env python3
import argparse,copy,hashlib,importlib.util,json,os,sys,tempfile,traceback
from pathlib import Path
sys.dont_write_bytecode=True
ROOT=next(p for p in Path(__file__).resolve().parents if (p/'rust/Cargo.toml').is_file())
spec=importlib.util.spec_from_file_location('image_tracks',ROOT/'tools/verify-can3-tracks.py')
T=importlib.util.module_from_spec(spec);sys.modules[spec.name]=T;spec.loader.exec_module(T)
KEY,CURVES=T.KEYS,T.CURVES
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def expected_xml(xml,native):
    nodes=T.xml_nodes(xml);defs=T.definitions(nodes)
    removed=({'#799','#800','#801',*(f'#{n}' for n in range(803,812))} if native else
        {'image-guid','image-track','image-visual','image-xy','image-opacity','image-frame'})
    assert len(removed)==(12 if native else 6) and removed<=defs.keys()
    track,guid=('#800','#799') if native else ('image-track','image-guid')
    members=[n for n in nodes if n.attrs=={'xs.ref':track} and n.parent.attrs.get('xs.n')=='_sources']
    assert len(members)==1 and len(members[0].parent.children)==3
    if native:
        children=[n for n in nodes if n.attrs=={'xs.ref':guid} and n.parent.attrs.get('xs.n')=='_childTrackGuids']
        assert len(children)==1 and len(children[0].parent.children)==2
        members+=children
    changes=[(defs[id].start,defs[id].end,b'') for id in removed]
    for member in members:
        start,end=T.count_span(xml,member.parent)
        changes.extend([(member.start,member.end,b''),(start,end,T.count_value(xml[start:end],len(member.parent.children)-1))])
    expected=T.patch(xml,changes);remaining=T.xml_nodes(expected);ids=T.definitions(remaining)
    assert not removed&ids.keys()
    assert ('#815' if native else 'image-resource-guid') in ids
    if native:assert '#802' in ids and '#0' in ids
    assert all(n.attrs['xs.ref'] in ids for n in remaining if 'xs.ref' in n.attrs)
    return expected,len(removed)
def main():
    assert __debug__
    parser=argparse.ArgumentParser(description='Verify exact CAN3 Image-track deletion; no vendor or native GUI acceptance.')
    parser.add_argument('--core',type=Path,required=True);parser.add_argument('--njc',type=Path,required=True)
    parser.add_argument('--native-can3',type=Path);args=parser.parse_args()
    unit=ROOT/'build/verification/issue145-image-track-delete';unit.mkdir(parents=True,exist_ok=True)
    run=Path(tempfile.mkdtemp(prefix='mcp-',dir=unit));host=None
    fixture=ROOT/'rust/tests/fixtures/can3-image-track.xml'
    paths=set((ROOT/'rust/src').rglob('*.rs'))|T.proof_helpers()|{Path(__file__).resolve(),fixture,ROOT/'rust/tests/core.rs',ROOT/'rust/Cargo.toml',ROOT/'rust/Cargo.lock'}
    sources={str(p.relative_to(ROOT)):digest(p) for p in paths}
    inputs={str(p.resolve()):digest(p.resolve()) for p in [args.core,args.njc]}
    if args.native_can3:
        native=args.native_can3.resolve();inputs[str(native)]=digest(native)
        assert inputs[str(native)]=='421b875c308c6e14fc9bc8cddaa6942a4a57c26356ea6402674d3c08915cb06a'
    result=dict(status='FAIL',scope='Exact Image deletion XML and nonXML CAFF preservation, resources and history; no native GUI/vendor acceptance',sourceHashes=sources,inputHashes=inputs,cases=[],generatedInputHashes={},savedArtifactHashes={})
    try:
        host=CURVES.DRIVER.Host(run,args.core.resolve(),args.njc.resolve(),CURVES.DRIVER.free_port());host.wait_ready()
        cases=[('authored',KEY.caff_pack(fixture.read_bytes()),False)]
        if args.native_can3:cases.append(('hiyori',native.read_bytes(),True))
        for label,original,is_native in cases:
            source,saved=run/(label+'.can3'),run/(label+'-saved.can3');source.write_bytes(original)
            result['generatedInputHashes'][str(source)]=digest(source)
            xml=KEY.decode_entries(original)[1];expected,count=expected_xml(xml,is_native)
            host.call(label+'-open','open',{'path':str(source)});before,model=KEY.state(host,label+'-before')
            scene_guid='3521e7dc-bc0a-447f-a650-f499252270d8' if is_native else 'scene-guid'
            track_guid='56c88abf-39e0-4ccd-959a-5257cb3a198c' if is_native else 'image-track-guid'
            changed=copy.deepcopy(model);scene=next(s for s in changed['scenes'] if s['guid']==scene_guid)
            assert [t['guid'] for t in scene['imageTracks']]==[track_guid];scene['imageTracks']=[]
            _,after=KEY.edit(host,label+'-delete','live2d_animation_track_delete',sceneGuid=scene_guid,trackGuid=track_guid)
            assert after==changed
            KEY.refuse(host,label+'-stale','live2d_animation_track_delete',{'sceneGuid':scene_guid,'trackGuid':track_guid,'expectedRevision':before['revision']},'revision conflict')
            host.call(label+'-undo','undo',{});assert KEY.state(host,label+'-undone')[1]==model
            undone=run/(label+'-undone.can3');host.call(label+'-save-undone','save',{'path':str(undone)})
            undone_data=undone.read_bytes();CURVES.caff_bytes_equal(original,undone_data)
            assert KEY.decode_entries(undone_data)[1]==xml,'undo did not restore original native XML'
            result['savedArtifactHashes'][str(undone)]=digest(undone)
            host.call(label+'-redo','redo',{});assert KEY.state(host,label+'-redone')[1]==changed
            data=CURVES.save_and_reopen(host,label,saved,changed,original)
            assert KEY.decode_entries(data)[1]==expected,'XML outside exact deletion closure/counts changed'
            assert source.read_bytes()==original
            result['savedArtifactHashes'][str(saved)]=digest(saved)
            result['cases'].append({'name':label,'removedDefinitions':count,'sceneGuid':scene_guid,'trackGuid':track_guid})
        result['status']='PASS'
    except BaseException:
        result['error']=traceback.format_exc();raise
    finally:
        try:
            if host:
                host.close();result['cleanup']={'pid':host.child.pid,'exit':host.child.returncode,'running':host.child.poll() is None}
                assert not result['cleanup']['running']
                try:os.kill(host.child.pid,0)
                except ProcessLookupError:pass
                else:raise AssertionError('owned host remains alive')
            for p,h in sources.items():assert digest(ROOT/p)==h
            for field in ['inputHashes','generatedInputHashes','savedArtifactHashes']:
                for p,h in result[field].items():assert digest(Path(p))==h
        except BaseException:
            result.update(status='FAIL',cleanupError=traceback.format_exc());raise
        finally:
            (run/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(run)
if __name__=='__main__':main()
