#!/usr/bin/env python3
"""Check the Glue source projection, the refused vertex edit and the unchanged save. Not Glue rendering."""
import importlib.util
import copy
import json
from pathlib import Path
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('playback', ROOT / 'tools/verify-native-playback.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
CMO = m.CAN.CMO
output = Path(tempfile.mkdtemp(prefix='glue-source-', dir=ROOT / 'build/verification'))
baseline = json.loads((ROOT / 'build/verification/native-resume-20261011/glue-source-baseline.json').read_text())
report = {'status': 'FAIL', 'contract': 'Glue source projection, refused vertex edit, unchanged save.', 'transports': []}
try:
    for mcp in (False, True):
        rpc = m.Rpc(ROOT / 'out/rust/release/soul2d-core', 120, mcp)
        checks = {'transport': 'mcp' if mcp else 'jsonl', 'models': [], 'rejectedControls': []}
        try:
            for item in baseline:
                path = Path(item['path'])
                assert m.sha(path) == item['sourceSha256']
                entries, xml = CMO.CAFF.decode(path.read_bytes())
                expected = CMO.read_scene(xml, {entry[0][0] for entry in entries})
                actual = rpc.call('live2d.authoring.read', path=str(path))
                assert actual == expected
                glues = [s for s in actual['sources'] if s['data']['type'] == 'Glue']
                assert glues
                opened = rpc.call('open', path=str(path))
                rendered = m.render(rpc, output, f"{checks['transport']}-{path.stem}")
                before = rpc.call('snapshot')
                glue = glues[0]
                try:
                    rpc.call('live2d.form.set_vertex', sourceGuid=glue['guid'],
                        formGuid=glue['forms'][0]['guid'], coordinateType='MODEL', vertexIndex=0,
                        position=[1, 2], expectedRevision=before['revision'])
                except RuntimeError as error:
                    assert 'Glue' in str(error) or 'vertex' in str(error), str(error)
                else:
                    raise AssertionError('Glue accepted vertex edit')
                assert rpc.call('snapshot') == before
                saved = output / f"{checks['transport']}-{path.stem}-saved.cmo3"
                rpc.call('save', path=str(saved))
                assert m.sha(saved) == item['sourceSha256']
                if path == Path(baseline[0]['path']):
                    edited = copy.deepcopy(actual)
                    mesh = next(s for s in edited['sources'] if s['data']['type'] == 'ArtMesh')
                    form = next(f for f in mesh['forms'] if f['data']['type'] == 'ArtMesh')
                    position = [form['data']['positions'][0] + .125, form['data']['positions'][1] - .25]
                    form['data']['positions'][:2] = position
                    state = rpc.call('snapshot')
                    rpc.call('live2d.form.set_vertex', sourceGuid=mesh['guid'], formGuid=form['guid'],
                        coordinateType=form['coordinateType'], vertexIndex=0, position=position,
                        expectedRevision=state['revision'])
                    assert rpc.call('live2d.authoring.read') == edited
                    edited_path = output / f"{checks['transport']}-edited-with-glue.cmo3"
                    rpc.call('save', path=str(edited_path))
                    rows, changed_xml = CMO.CAFF.decode(edited_path.read_bytes())
                    assert CMO.read_scene(changed_xml, {row[0][0] for row in rows}) == edited
                    rpc.call('undo')
                    assert rpc.call('live2d.authoring.read') == actual
                    rpc.call('redo')
                    assert rpc.call('live2d.authoring.read') == edited
                    rpc.call('undo')
                    restored_path = output / f"{checks['transport']}-undo-restored-with-glue.cmo3"
                    rpc.call('save', path=str(restored_path))
                    assert m.sha(restored_path) == item['sourceSha256']
                    checks['meshEditPreservesGlueAndHistory'] = 'PASS'
                checks['models'].append({'path': str(path), 'glueCount': len(glues),
                    'render': rendered, 'saveAndRejectedEdit': 'PASS'})
            path = Path(baseline[0]['path'])
            original = path.read_bytes()
            entries, xml = CMO.CAFF.decode(original)
            graph = CMO.XMLGraph(xml)
            glue = next(n for n in graph.root.iter('CGlueSource') if len(n))
            weights = graph.required(glue, 'weights')
            tokens = weights.text.split(); tokens[:2] = ['0.2', '0.3']; weights.text = ' '.join(tokens)
            form = graph.items(graph.required(glue, 'keyforms'))[0]
            graph.required(form, 'intensity').text = '1.5'
            positive = output / 'independent-weights-raw-intensity.cmo3'
            modified = CMO.ET.tostring(graph.root, encoding='utf-8')
            positive.write_bytes(CMO.repack_main_xml(original, entries, modified))
            expected = CMO.read_scene(modified, {entry[0][0] for entry in entries})
            actual = rpc.call('live2d.authoring.read', path=str(positive))
            assert actual == expected
            chosen = next(s for s in actual['sources'] if s['guid'] == graph.guid(glue, 'guid', required=True))
            assert chosen['data']['weights'][:2] == [.2, .3]
            evaluated = rpc.call('live2d.authoring.primary_forms', path=str(positive), sourceGuid=chosen['guid'])
            assert evaluated['sources'][0]['evaluatedForms'][0]['data'] == {'type': 'Glue', 'intensity': 1.5}
            checks['independentWeightsAndRawIntensity'] = 'PASS'
            for kind in ('uid-outside-u32', 'duplicate-uid', 'negative-bind', 'odd-bind',
                         'weight-range', 'weight-count', 'same-target', 'nonfinite-intensity'):
                graph = CMO.XMLGraph(xml)
                glue = next(n for n in graph.root.iter('CGlueSource') if len(n))
                bind = graph.required(glue, 'bindVertexUids')
                weights = graph.required(glue, 'weights')
                if kind == 'uid-outside-u32':  # an absent in-range UID is recorded as stale, not rejected
                    tokens = bind.text.split(); tokens[0] = '4294967296'; bind.text = ' '.join(tokens)
                elif kind == 'duplicate-uid':
                    mesh = next(n for n in graph.root.iter('CArtMeshSource') if len(n))
                    ext = next(n for n in graph.items(graph.required(mesh, '_extensions')) if n.tag == 'CEditableMeshExtension')
                    uids = graph.required(graph.required(ext, 'editableMesh'), 'pointUid')
                    tokens = uids.text.split(); tokens[1] = tokens[0]; uids.text = ' '.join(tokens)
                elif kind in ('negative-bind', 'odd-bind'):
                    tokens = bind.text.split()
                    if kind == 'negative-bind': tokens[0] = '-1'
                    else: tokens.pop(); bind.set('count', str(len(tokens)))
                    bind.text = ' '.join(tokens)
                elif kind in ('weight-range', 'weight-count'):
                    tokens = weights.text.split()
                    if kind == 'weight-range': tokens[0] = '1.5'
                    else: tokens.pop(); weights.set('count', str(len(tokens)))
                    weights.text = ' '.join(tokens)
                elif kind == 'same-target':
                    source_guids = [graph.guid(n, 'guid', required=True)
                        for n in graph.root.iter('CArtMeshSource') if len(n)]
                    a = graph.required(glue, 'targetArtMeshA_guid')
                    b = next(n for n in glue if n.get('xs.n') == 'targetArtMeshB_guid')
                    b.attrib.pop('xs.ref', None)
                    b.tag = 'CDrawableGuid'
                    b.set('uuid', a.get('uuid'))
                    assert graph.required(glue, 'targetArtMeshB_guid').get('uuid') == a.get('uuid')
                    assert source_guids == [graph.guid(n, 'guid', required=True)
                        for n in graph.root.iter('CArtMeshSource') if len(n)]
                else:
                    form = graph.items(graph.required(glue, 'keyforms'))[0]
                    graph.required(form, 'intensity').text = 'NaN'
                bad = output / f'{kind}.cmo3'
                bad.write_bytes(CMO.repack_main_xml(original, entries, CMO.ET.tostring(graph.root, encoding='utf-8')))
                before = rpc.call('snapshot')
                try:
                    rpc.call('live2d.authoring.read', path=str(bad))
                except RuntimeError as error:
                    checks['rejectedControls'].append({'kind': kind, 'error': str(error)})
                else:
                    raise AssertionError(f'accepted malformed Glue: {kind}')
                assert rpc.call('snapshot') == before
            assert all(m.sha(Path(row['path'])) == row['sourceSha256'] for row in baseline)
            report['transports'].append(checks)
        finally:
            rpc.close()
    report['status'] = 'PASS'
finally:
    (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print('PASS: Glue raw XML projection, unchanged save, unsupported vertex edits, eight malformed controls; JSONL + MCP.')
print(output / 'result.json')
