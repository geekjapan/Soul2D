#!/usr/bin/env python3
"""Check canonical form aliases, collision refusal and exact CMO3 save/reopen."""
import argparse
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('cmo', ROOT / 'tools/verify-cmo-source.py')
CMO = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CMO)


def alias_list(graph):
    for node in graph.root.iter():
        if node.tag != 'carray_list':
            continue
        rows = graph.items(node)
        if rows and rows[0].tag == 'CWarpDeformerForm' and len(set(rows)) < len(rows):
            return node
    raise AssertionError('real source has no repeated canonical warp form')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    source, binary = args.source.resolve(), args.binary.resolve()
    original = source.read_bytes()
    original_hash = CMO.sha(source)
    entries, xml = CMO.CAFF.decode(original)
    textures = {entry[0][0] for entry in entries}
    expected = CMO.read_scene(xml, textures)
    graph = CMO.XMLGraph(xml)
    rows = alias_list(graph)
    resolved = graph.items(rows)
    first, repeated = next((i, j) for j in range(len(resolved)) for i in range(j)
                           if resolved[i] is resolved[j])
    run = Path(tempfile.mkdtemp(prefix='form-aliases-', dir=ROOT / 'build/verification'))
    cases = [('original-alias', original, True)]
    for name in ('single-reference', 'transitive-alias', 'distinct-identity-collision', 'wrong-count'):
        graph = CMO.XMLGraph(xml)
        rows = alias_list(graph)
        if name == 'single-reference':
            rows.remove(rows[repeated])
            rows.set('count', str(len(rows)))
        elif name == 'transitive-alias':
            rows[first].set('xs.id', 'canonical-alias-proof')
            assert 'canonical-alias-proof' not in graph.ids
            rows[repeated].set('xs.ref', 'canonical-alias-proof')
        elif name == 'distinct-identity-collision':
            form = copy.deepcopy(graph.resolve(rows[first]))
            for node in form.iter():
                node.attrib.pop('xs.id', None)
            rows.remove(rows[repeated])
            rows.insert(repeated, form)
        else:
            rows.set('count', str(len(rows) + 1))
        altered = CMO.ET.tostring(graph.root, encoding='utf-8')
        success = name in ('single-reference', 'transitive-alias')
        try:
            result = CMO.read_scene(altered, textures)
        except ValueError:
            assert not success, name
        else:
            assert success and result == expected, name
        cases.append((name, CMO.repack_main_xml(original, entries, altered), success))
    for name, data, success in cases:
        path = run / (name + '.cmo3')
        path.write_bytes(data)
        actual, wire = CMO.jsonl_request(binary, 'live2d.authoring.read', path)
        (run / (name + '.jsonl.log')).write_text(wire)
        assert actual['succeeded'] == success, (name, actual)
        if success:
            assert actual['result'] == expected, name
    saved = run / 'saved.cmo3'
    requests = [{'op': 'open', 'path': str(source)}, {'op': 'save', 'path': str(saved)},
                {'op': 'open', 'path': str(saved)},
                {'op': 'live2d.authoring.read', 'path': str(saved)}]
    process = subprocess.run([str(binary)],
        input=''.join(json.dumps(request) + '\n' for request in requests),
        text=True, capture_output=True, timeout=120, check=True)
    (run / 'save-reopen.jsonl.log').write_text(process.stdout + process.stderr)
    replies = [json.loads(line) for line in process.stdout.splitlines() if line.strip()]
    assert len(replies) == len(requests) and all(row['succeeded'] for row in replies), replies
    assert CMO.sha(saved) == original_hash and replies[-1]['result'] == expected
    assert CMO.sha(source) == original_hash
    CMO.write_json(run / 'result.json', {'status': 'PASS', 'source': str(source),
        'sha256': original_hash, 'binarySha256': CMO.sha(binary),
        'cases': [case[0] for case in cases], 'byteExactSaveReopen': True})
    print(run)
    print('canonical alias equivalence, collision refusal and exact save/reopen PASS')


if __name__ == '__main__':
    main()
