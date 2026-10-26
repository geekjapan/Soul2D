#!/usr/bin/env python3
"""Compare inventoried official CAN3 source reads with the XML oracle, not playback."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import zipfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('can3_source', ROOT / 'tools/verify-can3-source.py')
CAN = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CAN)


def write_report(output, report):
    (output / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')


def initial_report():
    return {'status': 'INCOMPLETE', 'scope': 'CAN3 source projection only, not playback',
            'files': [], 'missingArchives': [], 'invalidArchives': [], 'archiveErrors': {}}


def verify(inventory, output, binary):
    report = initial_report()
    output.mkdir(parents=True, exist_ok=True)
    write_report(output, report)
    cache = {}
    try:
        report['binarySha256'] = CAN.CMO.sha(binary)
        report['archiveCount'] = len(inventory['archives'])
        report['can3MembersExpected'] = sum(member['extension'] == '.can3'
            for row in inventory['archives'] for member in row.get('files', []))
        for row in inventory['archives']:
            route = row['route']
            if row['status'] == 'missing':
                report['missingArchives'].append(route)
                write_report(output, report)
                continue
            if row['status'] != 'inventoried_not_tested':
                report['invalidArchives'].append(route)
                report['archiveErrors'][route] = f"unexpected inventory status: {row['status']}"
                write_report(output, report)
                continue
            try:
                archive_path = Path(row['localPath'])
                if CAN.CMO.sha(archive_path) != row['sha256']:
                    raise ValueError('archive changed since inventory')
                with zipfile.ZipFile(archive_path) as archive:
                    models = []
                    for member in row['files']:
                        if member['extension'] != '.can3':
                            continue
                        model = archive.read(member['path'])
                        checksum = hashlib.sha256(model).hexdigest()
                        if checksum != member['sha256']:
                            raise ValueError(f"member changed since inventory: {member['path']}")
                        models.append((member, model, checksum))
            except Exception as error:
                report['invalidArchives'].append(route)
                report['archiveErrors'][route] = str(error)
                write_report(output, report)
                continue

            for member, model, checksum in models:
                result = {'archive': route, 'member': member['path'], 'sha256': checksum}
                if checksum not in cache:
                    path = output / (checksum + '.can3')
                    stage, wire = 'native_read', None
                    try:
                        path.write_bytes(model)
                        actual, wire = CAN.CMO.jsonl_request(
                            binary, 'live2d.animation.read', path.resolve())
                        if not actual.get('succeeded'):
                            raise ValueError(actual.get('message', 'native read failed'))
                        stage = 'xml_oracle'
                        _, xml = CAN.CMO.CAFF.decode(model)
                        stage = 'comparison'
                        scenes, parameter_points = CAN.check_xml(actual['result'], xml)
                        cache[checksum] = {'status': 'PASS', 'scenes': scenes,
                                           'parameterPoints': parameter_points}
                    except Exception as error:
                        if wire is not None:
                            (output / (checksum + '.jsonl.log')).write_text(wire)
                        cache[checksum] = {'status': 'FAIL', 'stage': stage, 'error': str(error)}
                result.update(cache[checksum])
                report['files'].append(result)
                print(f"{result['status']} {route} :: {member['path']}", flush=True)
            report['uniqueModels'] = len(cache)
            write_report(output, report)

        report['uniqueModels'] = len(cache)
        if any(row['status'] == 'FAIL' for row in report['files']) or report['invalidArchives']:
            report['status'] = 'FAIL'
        elif report['files'] and not report['missingArchives'] and len(report['files']) == report['can3MembersExpected']:
            report['status'] = 'PASS'
    except Exception as error:
        report['status'] = 'FAIL'
        report['error'] = str(error)
    write_report(output, report)
    return report


def self_test():
    import tempfile

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        binary = root / 'binary'
        binary.write_bytes(b'not invoked')
        output = root / 'output'
        output.mkdir()
        result_path = output / 'result.json'
        result_path.write_text('{"status":"PASS"}')
        report = verify({'archives': []}, output, binary)
        assert report['status'] == 'INCOMPLETE'
        assert json.loads(result_path.read_text())['status'] == 'INCOMPLETE'

        report = verify({'archives': [{'status': 'missing', 'route': 'missing.zip', 'files': []}]},
                        output, binary)
        assert report['status'] == 'INCOMPLETE' and report['missingArchives'] == ['missing.zip']

        archive = root / 'empty.zip'
        with zipfile.ZipFile(archive, 'w'):
            pass
        row = {'status': 'inventoried_not_tested', 'localPath': str(archive),
               'sha256': CAN.CMO.sha(archive), 'route': 'empty.zip', 'files': []}
        report = verify({'archives': [row]}, output, binary)
        assert report['status'] == 'INCOMPLETE' and not report['files']

        archive.write_bytes(b'changed')
        row['sha256'] = 'stale'
        result_path.write_text('{"status":"PASS"}')
        report = verify({'archives': [row]}, output, binary)
        assert report['status'] == 'FAIL' and report['invalidArchives'] == ['empty.zip']
        assert json.loads(result_path.read_text())['status'] == 'FAIL'

    proof = {'status': 'PASS', 'scope': 'focused harness self-test only, not full corpus',
             'cases': ['empty inventory overwrites stale PASS with INCOMPLETE',
                       'missing archive remains INCOMPLETE',
                       'empty archive remains INCOMPLETE',
                       'changed archive fails and overwrites stale PASS'],
             'scriptSha256': CAN.CMO.sha(Path(__file__))}
    proof_path = ROOT / 'build/verification/official-can3-source-harness-self-test.json'
    proof_path.parent.mkdir(parents=True, exist_ok=True)
    proof_path.write_text(json.dumps(proof, indent=2) + '\n')
    print('official CAN3 source verifier self-test PASS')


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
    write_report(args.output, initial_report())
    try:
        report = verify(json.loads(args.inventory.read_text()), args.output, args.binary.resolve())
    except Exception as error:
        report = initial_report()
        report.update(status='FAIL', error=str(error))
        write_report(args.output, report)
    print(report['status'])
    sys.exit(0 if report['status'] == 'PASS' else 1)


if __name__ == '__main__':
    main()
