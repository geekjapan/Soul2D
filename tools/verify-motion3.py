#!/usr/bin/env python3
"""Verify real Motion3 curve editing through Rust FFI and stdio MCP, without a Cubism SDK."""
import argparse
import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class Mcp:
    def __init__(self, run):
        self.stderr = (run / 'mcp.stderr').open('w')
        self.wire = (run / 'mcp.jsonl').open('w')
        self.child = subprocess.Popen([str(ROOT / 'out/rust/debug/soul2d-core'), '--mcp'],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, text=True)
        self.sequence = 0

    def initialize(self):
        self.rpc('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                 'clientInfo': {'name': 'motion3-verifier', 'version': '1'}})
        tools = {tool['name'] for tool in self.rpc('tools/list', {})['tools']}
        assert {'motion3_import', 'motion3_list', 'motion3_read', 'motion3_sample',
                'motion3_point_set', 'motion3_export'} <= tools

    def rpc(self, method, params):
        self.sequence += 1
        message = {'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}
        self.wire.write(json.dumps({'request': message}) + '\n')
        self.child.stdin.write(json.dumps(message) + '\n')
        self.child.stdin.flush()
        response = json.loads(self.child.stdout.readline())
        self.wire.write(json.dumps({'response': response}) + '\n')
        assert response['id'] == self.sequence and 'error' not in response, response
        return response['result']

    def request(self, op, success=True, **args):
        result = self.rpc('tools/call', {'name': op.replace('.', '_'), 'arguments': args})
        assert not result.get('isError', False) if success else result.get('isError', False), (op, result)
        if not success:
            return None
        content = result.get('structuredContent') or json.loads(result['content'][0]['text'])
        image = next((item for item in result.get('content', []) if item.get('type') == 'image'), None)
        return {**content, 'png': image['data']} if image else content

    def close(self):
        self.child.stdin.close()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()
        assert self.child.returncode == 0
        self.wire.close()
        self.stderr.close()


# Independent 80-digit Decimal curve oracle; these interior samples are not resampled frame keys.
ORACLES = [
    ('zundamon_ja/runtime/motions/mtnBody_think3.motion3.json', 2, 1.35, -29.959144618833276323902),
    ('zundamon_ja/runtime/motions/mtnBody_think3.motion3.json', 2, 1.4, -29.987384429937868919562),
    ('hiyori_ja/hiyori_free/runtime/motion/hiyori_m01.motion3.json', 1, 0.35, 2.684898004531097798372),
    ('ren_ja/runtime/motions/mtn_01.motion3.json', 1, 0.8, 4.173125),
    ('ren_ja/runtime/motions/mtn_02.motion3.json', 57, 3.8, 0.19),
    ('ren_ja/runtime/motions/mtn_03.motion3.json', 67, 0.7915, 0.495),
    ('ren_ja/runtime/motions/mtn_03.motion3.json', 67, 1.49, 0.582352941176470588235),
    ('hiyori_ja/hiyori_free/runtime/motion/hiyori_m01.motion3.json', 21, 2.35, 1.0),
]


def verify(peer, label, fixtures, run, helper):
    source = run / f'{label}-source.inx'
    model, tail = helper.fixture(source)
    peer.request('open', path=str(source))
    expected = copy.deepcopy(model)
    expected['live2d'] = {'motions': {}}
    files = sorted(fixtures.rglob('*.motion3.json'))
    assert len(files) == 44, 'This acceptance targets the four supplied sample models (44 motions)'
    for file in files:
        name = file.relative_to(fixtures).as_posix()
        motion = json.loads(file.read_text())
        peer.request('motion3.import', name=name, path=str(file))
        assert peer.request('motion3.read', name=name) == motion
        expected['live2d']['motions'][name] = motion
        before = peer.request('snapshot')
        for seconds in [0, motion['Meta']['Duration'] / 3, motion['Meta']['Duration'] + 1]:
            sampled = peer.request('motion3.sample', name=name, time=seconds)
            assert len(sampled['curves']) == len(motion['Curves'])
            assert [(item['target'], item['id']) for item in sampled['curves']] == [
                (curve['Target'], curve['Id']) for curve in motion['Curves']]
        assert peer.request('snapshot') == before
    assert len(peer.request('motion3.list')['motions']) == 44
    for name, curve, seconds, value in ORACLES:
        sampled = peer.request('motion3.sample', name=name, time=seconds)
        assert abs(sampled['curves'][curve]['value'] - value) <= 1e-10, (label, name, curve, seconds, sampled)
    name = ORACLES[0][0]
    before = peer.request('snapshot')
    peer.request('motion3.import', name=name, path=str(fixtures / name), success=False)
    peer.request('motion3.point.set', name=name, curve=2, point=0, time=-1, success=False)
    assert peer.request('snapshot') == before
    peer.request('motion3.point.set', name=name, curve=2, point=1, value=-25.5)
    expected['live2d']['motions'][name]['Curves'][2]['Segments'][4] = -25.5
    assert peer.request('motion3.read', name=name) == expected['live2d']['motions'][name]
    after = peer.request('snapshot')
    peer.request('undo')
    assert peer.request('snapshot')['model'] == before['model']
    peer.request('redo')
    assert peer.request('snapshot')['model'] == after['model']
    saved, exported = run / f'{label}-saved.inx', run / f'{label}-edited.motion3.json'
    before_export = peer.request('snapshot')
    peer.request('motion3.export', name=name, path=str(exported))
    assert json.loads(exported.read_text()) == expected['live2d']['motions'][name]
    peer.request('motion3.export', name=name, path=str(exported), success=False)
    peer.request('motion3.export', name=name, path=str(fixtures / name), success=False)
    assert peer.request('snapshot') == before_export
    peer.request('save', path=str(saved))
    saved_model, saved_tail = helper.split_container(saved.read_bytes())
    assert saved_model == expected and saved_tail == tail
    peer.request('open', path=str(saved))
    assert peer.request('snapshot')['model'] == expected
    peer.request('motion3.import', name='Export reimport', path=str(exported))
    assert peer.request('motion3.read', name='Export reimport') == expected['live2d']['motions'][name]
    rendered = peer.request('render', width=32, height=32, bounds=[0, 0, 8, 8])
    png = base64.b64decode(rendered['png'], validate=True)
    width, height, pixels = helper.decode_rgba(png)
    assert (width, height) == (32, 32) and pixels[(16 * width + 16) * 4:(16 * width + 16) * 4 + 4] == bytes([255, 0, 0, 255])
    (run / f'{label}-inx.png').write_bytes(png)
    assert helper.split_container(source.read_bytes()) == (model, tail)
    return {'passed': True, 'motions': len(files), 'interiorOracles': len(ORACLES),
            'limitations': ['Raw motion curves only; no Cubism runtime pose, fade/loop/blend or MOC/CMO authoring']}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('fixtures', type=Path, help='Verified copies of the supplied Live2D sample archives')
    args = parser.parse_args()
    fixtures = args.fixtures.resolve()
    files = sorted(fixtures.rglob('*.motion3.json'))
    assert len(files) == 44
    hashes = {str(file): hashlib.sha256(file.read_bytes()).hexdigest() for file in files}
    run = Path(tempfile.mkdtemp(prefix='motion3-', dir=ROOT / 'build/verification'))
    print(run, flush=True)
    helper = module('core_fixture', ROOT / 'tools/verify-rust-core.py')
    suffix = 'libsoul2d_core.dylib' if sys.platform == 'darwin' else 'libsoul2d_core.so'
    artifacts = [ROOT / 'out/rust/debug' / suffix, ROOT / 'out/rust/debug/soul2d-core']
    provenance = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in artifacts}
    core = helper.Core(artifacts[0])
    results = {}
    try:
        results['ffi'] = verify(core, 'ffi', fixtures, run, helper)
    finally:
        core.close()
    peer = Mcp(run)
    try:
        peer.initialize()
        results['mcp'] = verify(peer, 'mcp', fixtures, run, helper)
    finally:
        peer.close()
    assert all(hashlib.sha256(Path(file).read_bytes()).hexdigest() == digest for file, digest in hashes.items())
    results['sourceHashes'] = hashes
    assert all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest for path, digest in provenance.items())
    results['artifactHashes'] = provenance
    results['passed'] = True
    (run / 'result.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'passed': True, 'ffi': results['ffi'], 'mcp': results['mcp']}), flush=True)


if __name__ == '__main__':
    main()
