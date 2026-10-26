#!/usr/bin/env python3
"""Compare every inventoried official CMO3 with the XML oracle and Rust JSONL reader."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import zipfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('cmo_oracle', ROOT / 'tools/verify-cmo-source.py')
CMO = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CMO)


def verify(inventory, output, binary):
    report = {'status': 'INCOMPLETE', 'scope': 'CMO3 source projection only',
              'files': [], 'missingArchives': [], 'invalidArchives': [], 'archiveErrors': {}}
    output.mkdir(parents=True, exist_ok=True)
    (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    report['binarySha256'] = CMO.sha(binary)
    cache = {}
    for row in inventory['archives']:
        if row['status'] == 'missing':
            report['missingArchives'].append(row['route'])
            continue
        if row['status'] != 'inventoried_not_tested':
            report['invalidArchives'].append(row['route'])
            continue
        archive_path = Path(row['localPath'])
        if CMO.sha(archive_path) != row['sha256']:
            report['invalidArchives'].append(row['route'])
            report['archiveErrors'][row['route']] = 'archive changed since inventory'
            continue
        with zipfile.ZipFile(archive_path) as archive:
            for member in row['files']:
                if member['extension'] != '.cmo3':
                    continue
                checksum = member['sha256']
                result = {'archive': row['route'], 'member': member['path'], 'sha256': checksum}
                if checksum not in cache:
                    model = archive.read(member['path'])
                    if hashlib.sha256(model).hexdigest() != checksum:
                        raise ValueError(f"member changed since inventory: {member['path']}")
                    path = output / (checksum + '.cmo3')
                    path.write_bytes(model)
                    stage = 'native_read'
                    try:
                        actual, wire = CMO.jsonl_request(binary, 'live2d.authoring.read', path.resolve())
                        if not actual.get('succeeded'):
                            (output / (checksum + '.jsonl.log')).write_text(wire)
                            raise ValueError(actual.get('message', 'native read failed'))
                        stage = 'xml_oracle'
                        entries, xml = CMO.CAFF.decode(model)
                        expected = CMO.read_scene(xml, {entry[0][0] for entry in entries})
                        stage = 'comparison'
                        if actual.get('result') != expected:
                            (output / (checksum + '.jsonl.log')).write_text(wire)
                            raise AssertionError('native source projection differs from XML oracle; see JSONL log')
                        cache[checksum] = {'status': 'PASS', 'sources': len(expected['sources'])}
                    except Exception as error:
                        cache[checksum] = {'status': 'FAIL', 'stage': stage, 'error': str(error)}
                result.update(cache[checksum])
                report['files'].append(result)
                print(f"{result['status']} {row['route']} :: {member['path']}", flush=True)
        report['uniqueModels'] = len(cache)
        (output / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    if any(row['status'] == 'FAIL' for row in report['files']) or report['invalidArchives']:
        report['status'] = 'FAIL'
    elif report['files'] and not report['missingArchives']:
        report['status'] = 'PASS'
    (output / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        binary = root / 'binary'
        binary.write_bytes(b'not invoked')
        archive = root / 'changed.zip'
        archive.write_bytes(b'changed')
        result = root / 'output/result.json'
        result.parent.mkdir()
        result.write_text('{"status":"PASS"}')
        report = verify({'archives': [{'status': 'inventoried_not_tested', 'localPath': str(archive),
                                      'sha256': 'stale', 'route': 'changed.zip'}]}, result.parent, binary)
        assert report['status'] == 'FAIL' and json.loads(result.read_text())['status'] == 'FAIL'
        report = verify({'archives': [{'status': 'missing', 'route': 'missing.zip'}]}, result.parent, binary)
        assert report['status'] == 'INCOMPLETE' and report['missingArchives'] == ['missing.zip']
        assert verify({'archives': []}, result.parent, binary)['status'] == 'INCOMPLETE'
    print('official CMO source verifier self-test PASS')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inventory', nargs='?', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.inventory is None or args.output is None:
        parser.error('inventory and --output are required')
    args.output.mkdir(parents=True, exist_ok=True)
    try:
        report = verify(json.loads(args.inventory.read_text()), args.output, args.binary.resolve())
    except Exception as error:
        report = {'status': 'FAIL', 'error': str(error)}
        (args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(report['status'])
    sys.exit(0 if report['status'] == 'PASS' else 1)


if __name__ == '__main__':
    main()
