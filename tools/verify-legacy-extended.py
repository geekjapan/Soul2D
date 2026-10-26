#!/usr/bin/env python3
"""Verify historical and modern extended-form representations using a real CMO3."""
import argparse
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    source = args.source.resolve()
    original_hash = CMO.sha(source)
    original = source.read_bytes()
    entries, xml = CMO.CAFF.decode(original)
    textures = {entry[0][0] for entry in entries}
    expected = CMO.read_scene(xml, textures)
    graph = CMO.XMLGraph(xml)
    extensions = [node for node in graph.root.iter('CExtendedInterpolationExtension')
                  if node.get('xs.ref') is None and graph.field(node, 'extendedKeyforms') is not None]
    assert extensions, 'real source has no historical extended forms'
    for extension in extensions:
        graph.required(extension, 'extendedKeyforms').set('xs.n', 'normalExtendedKeyforms')
        CMO.ET.SubElement(extension, 'carray_list', {'xs.n': 'morphTargetExtendedKeyforms', 'count': '0'})
    modern_xml = CMO.ET.tostring(graph.root, encoding='utf-8')
    assert CMO.read_scene(modern_xml, textures) == expected
    run = Path(tempfile.mkdtemp(prefix='legacy-extended-', dir=ROOT / 'build/verification'))
    cases = [('historical', original, True),
             ('modern-equivalent', CMO.repack_main_xml(original, entries, modern_xml), True)]
    for name in ('mixed-lists', 'missing-modern-morph-list'):
        graph = CMO.XMLGraph(modern_xml)
        extension = next(n for n in graph.root.iter('CExtendedInterpolationExtension')
                         if n.get('xs.ref') is None)
        if name == 'mixed-lists':
            CMO.ET.SubElement(extension, 'carray_list', {'xs.n': 'extendedKeyforms', 'count': '0'})
        else:
            extension.remove(graph.required(extension, 'morphTargetExtendedKeyforms'))
        altered = CMO.ET.tostring(graph.root, encoding='utf-8')
        try:
            CMO.read_scene(altered, textures)
        except ValueError:
            pass
        else:
            raise AssertionError('oracle accepted ' + name)
        cases.append((name, CMO.repack_main_xml(original, entries, altered), False))
    for name, data, success in cases:
        path = run / (name + '.cmo3')
        path.write_bytes(data)
        actual, wire = CMO.jsonl_request(args.binary.resolve(), 'live2d.authoring.read', path)
        (run / (name + '.jsonl.log')).write_text(wire)
        assert actual['succeeded'] == success, (name, actual)
        if success:
            assert actual['result'] == expected, name
    saved = run / 'saved.cmo3'
    requests = [{'op': 'open', 'path': str(source)}, {'op': 'save', 'path': str(saved)},
                {'op': 'open', 'path': str(saved)},
                {'op': 'live2d.authoring.read', 'path': str(saved)}]
    process = subprocess.run([str(args.binary.resolve())],
        input=''.join(json.dumps(request) + '\n' for request in requests),
        text=True, capture_output=True, timeout=120, check=True)
    (run / 'save-reopen.jsonl.log').write_text(process.stdout + process.stderr)
    replies = [json.loads(line) for line in process.stdout.splitlines() if line.strip()]
    assert len(replies) == len(requests) and all(row['succeeded'] for row in replies), replies
    assert CMO.sha(saved) == original_hash and replies[-1]['result'] == expected
    assert CMO.sha(source) == original_hash
    (run / 'result.json').write_text(json.dumps({'status': 'PASS', 'source': str(source),
        'sha256': original_hash, 'extensions': len(extensions), 'cases': [c[0] for c in cases],
        'binarySha256': CMO.sha(args.binary), 'byteExactSaveReopen': True}, indent=2) + '\n')
    print(run)
    print('historical/modern equivalence and malformed-form refusals PASS')


if __name__ == '__main__':
    main()
