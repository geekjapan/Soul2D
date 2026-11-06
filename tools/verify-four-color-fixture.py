#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('fixture_oracle', ROOT / 'tools/verify-caff-authoring.py')
ORACLE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ORACLE)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if not __debug__:
        raise RuntimeError('verification requires Python assertions; do not use -O')
    parser = argparse.ArgumentParser(description='Check the fixed synthetic CMO3 producer through its actual CLI.')
    parser.add_argument('--example', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    example = args.example.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=args.out.resolve()))
    paths = [Path(__file__).resolve(), ROOT / 'tools/verify-caff-authoring.py', ROOT / 'tools/verify-rust-core.py',
             ROOT / 'rust/examples/four-color-cmo3.rs', ROOT / 'rust/tests/common/authoring_fixture.rs',
             ROOT / 'rust/tests/core.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock']
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in paths}
    report = {'status': 'FAIL', 'scope': 'fixed owned four-color CMO3 generation; not GUI or performance acceptance',
              'sourceHashes': hashes, 'binarySha256': digest(example), 'cases': [], 'gui': 'NOT_RUN',
              'performance': 'NOT_RUN', 'vendor': 'NOT_RUN'}
    try:
        report['gitHead'] = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, check=True,
                                           capture_output=True, encoding='utf-8', timeout=5).stdout.strip()

        def invoke(name, arguments, success):
            result = subprocess.run([str(example), *map(str, arguments)], cwd=run, capture_output=True,
                                    encoding='utf-8', timeout=30)
            report['cases'].append(dict(name=name, arguments=list(map(str, arguments)), exit=result.returncode,
                                        stdout=result.stdout, stderr=result.stderr))
            assert (result.returncode == 0) is success, report['cases'][-1]

        invoke('missing-argument', [], False)
        invoke('extra-argument', [run / 'unused.cmo3', 'extra'], False)
        assert not list(run.iterdir()), 'invalid arguments created output'
        first, second = run / '容量-日本語.cmo3', run / 'copy-彩色.cmo3'
        invoke('first', [first], True)
        original = first.read_bytes()
        invoke('existing-file', [first], False)
        assert first.read_bytes() == original
        invoke('second', [second], True)
        assert second.read_bytes() == original
        sentinel = run / 'sentinel.cmo3'
        sentinel.write_bytes(b'preserve this existing file\0\xff')
        invoke('sentinel', [sentinel], False)
        assert sentinel.read_bytes() == b'preserve this existing file\0\xff'
        directory = run / 'existing-directory'
        directory.mkdir()
        invoke('existing-directory', [directory], False)
        assert directory.is_dir() and not list(directory.iterdir())
        if os.name == 'posix':
            alias = run / 'existing-symlink.cmo3'
            alias.symlink_to(sentinel.name)
            invoke('existing-symlink', [alias], False)
            assert alias.is_symlink() and sentinel.read_bytes() == b'preserve this existing file\0\xff'
            report['symlinkRefusal'] = 'PASS'
        else:
            report['symlinkRefusal'] = 'NOT_RUN (Windows symlink creation privilege not assumed)'
        invoke('missing-parent', [run / 'absent-parent' / 'fixture.cmo3'], False)
        assert not (run / 'absent-parent').exists()
        entries, xml = ORACLE.decode(original)
        payloads = {entry[0][0]: entry[-1] for entry in entries}
        names = ['mesh.png', 'tex0.png', 'tex1.png', 'tex2.png']
        assert len(entries) == 6 and set(payloads) == {'main.xml', 'opaque.bin', *names}
        assert payloads['opaque.bin'] == bytes([0, 1, 2, 255])
        colors = [(220, 40, 20, 255), (20, 190, 40, 255), (30, 50, 210, 255), (190, 40, 180, 255)]
        tree = ET.fromstring(xml)
        textures = {node.get('xs.id'): node for node in tree.iter('GTexture2D') if node.get('xs.id')}
        meshes = {node.get('xs.id') for node in tree.iter('CArtMeshSource') if node.get('xs.id')}
        assert set(textures) == {f'texture{i}' for i in range(4)} and meshes == {f'mesh{i}' for i in range(4)}
        for tag, prefix in [('CArtMeshGuid', 'mesh-guid'), ('CFormGuid', 'form-guid'), ('GTextureGuid', 'texture-guid')]:
            assert {node.get('uuid') for node in tree.iter(tag)} == {f'{prefix}-{i}' for i in range(4)}
        for i, (name, color) in enumerate(zip(names, colors)):
            resource = textures[f'texture{i}'].find('CImageResource')
            assert (resource.get('width'), resource.get('height'), resource.find('file').get('path')) == ('2048', '2048', name)
            width, height, rgba = ORACLE.CORE.decode_rgba(payloads[name])
            assert (width, height) == (2048, 2048) and rgba == bytes(color) * (width * height)
        assert all(digest(ROOT / path) == expected for path, expected in hashes.items())
        assert digest(example) == report['binarySha256'] and first.read_bytes() == second.read_bytes() == original
        report.update(status='PASS', fixtureSha256=digest(first), fixtureBytes=len(original), textures=4,
                      textureSize=[2048, 2048], opaquePreserved=True)
    except BaseException:
        report['error'] = traceback.format_exc()
        raise
    finally:
        (run / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(f"{report['status']}: {run}")


if __name__ == '__main__':
    main()
