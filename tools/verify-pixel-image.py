#!/usr/bin/env python3
import argparse
import hashlib
import json
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import traceback

ROOT = Path(__file__).resolve().parents[1]

CENTER_SEED_3X3 = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    200, 50, 25, 255,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
]
SEED_5X1 = [100, 20, 10, 128] + [0] * 16
MIXED_3X1 = [200, 100, 50, 255, 0, 0, 0, 0, 100, 0, 0, 1]


class Peer:
    def __init__(self, run, name, args, binary):
        self.stderr = (run / f'{name}.stderr').open('w')
        self.wire = (run / f'{name}.jsonl').open('w')
        self.child = subprocess.Popen([str(binary)] + args, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=self.stderr, text=True)

    def send(self, message):
        self.wire.write(json.dumps({'request': message}) + '\n')
        self.wire.flush()
        self.child.stdin.write(json.dumps(message) + '\n')
        self.child.stdin.flush()
        ready, _, _ = select.select([self.child.stdout], [], [], 30)
        if not ready:
            raise TimeoutError(f'{self.child.args} did not respond within 30s')
        line = self.child.stdout.readline()
        if not line:
            raise RuntimeError(f'{self.child.args} closed stdout')
        response = json.loads(line)
        self.wire.write(json.dumps({'response': response}) + '\n')
        self.wire.flush()
        return response

    def close(self):
        try:
            self.child.stdin.close()
        except OSError:
            pass
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()
        assert self.child.returncode == 0, self.child.returncode
        self.wire.close()
        self.stderr.close()


class Jsonl(Peer):
    def __init__(self, run, binary):
        super().__init__(run, 'jsonl', [], binary)

    def request(self, op, success=True, **args):
        response = self.send({**args, 'op': op})
        if success:
            assert response.get('succeeded') is True, (op, response)
            return response['result']
        assert response.get('succeeded') is False, (op, response)
        return response


class Mcp(Peer):
    def __init__(self, run, binary):
        super().__init__(run, 'mcp', ['--mcp'], binary)
        self.sequence = 0

    def rpc(self, method, params):
        self.sequence += 1
        message = {'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}
        response = self.send(message)
        assert response['id'] == self.sequence and 'error' not in response, response
        return response['result']

    def request(self, op, success=True, **args):
        result = self.rpc('tools/call', {'name': op.replace('.', '_'), 'arguments': args})
        if success:
            assert not result.get('isError', False), (op, result)
            return result.get('structuredContent')
        assert result.get('isError', False) is True, (op, result)
        return result


def verify(peer, label):
    checks = []
    before = peer.request('snapshot')

    seen = peer.request('image.process', method='inspect', format='Rgb',
                        size=[2, 1], pixels=[1, 2, 3, 4, 5, 6])
    assert seen['method'] == 'inspect' and seen['size'] == [2, 1]
    assert seen['format'] == 'Rgb' and seen['bitDepth'] == 8
    assert seen['channels'] == 3 and seen['shape'] == [1, 2, 3]
    assert seen['empty'] is False
    assert seen['pixels'] == [1, 2, 3, 4, 5, 6]
    for fmt, channels in [('Mono', 1), ('Rgb', 3), ('Rgba', 4)]:
        seen = peer.request('image.process', method='inspect', format=fmt,
                            size=[1, 2], pixels=[7] * 2 * channels)
        assert seen['channels'] == channels and seen['shape'] == [2, 1, channels]
    seen = peer.request('image.process', method='inspect', format='Mono',
                        size=[0, 3], pixels=[])
    assert seen['empty'] is True and seen['shape'] == [3, 0, 1] and seen['pixels'] == []
    checks.append('inspect.metadata')

    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[3, 3], pixels=CENTER_SEED_3X3, iterations=1)
    for index in range(9):
        assert out['pixels'][index * 4:index * 4 + 3] == [200, 50, 25], index
        assert out['pixels'][index * 4 + 3] == (255 if index == 4 else 0)
    checks.append('bleed.centerSeed')

    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[5, 1], pixels=SEED_5X1, iterations=1)
    assert out['pixels'] == [100, 20, 10, 128, 100, 20, 10, 0] + [0] * 12, out['pixels']
    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[5, 1], pixels=SEED_5X1)
    assert out['pixels'] == [100, 20, 10, 128] + [100, 20, 10, 0] * 4
    checks.append('bleed.ringPerPass')

    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[3, 1], pixels=MIXED_3X1, iterations=1)
    assert out['pixels'] == [200, 100, 50, 255, 150, 50, 25, 0, 100, 0, 0, 1]
    checks.append('bleed.average')

    transparent = [9, 9, 9, 0, 7, 7, 7, 0]
    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[2, 1], pixels=transparent)
    assert out['pixels'] == transparent
    opaque = [1, 2, 3, 255, 4, 5, 6, 200]
    for args in [{}, {'iterations': 0}, {'iterations': -3}]:
        out = peer.request('image.process', method='bleed', format='Rgba',
                           size=[2, 1], pixels=opaque, **args)
        assert out['pixels'] == opaque
    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[0, 2], pixels=[])
    assert out['empty'] is True and out['pixels'] == []
    out = peer.request('image.process', method='bleed', format='Rgba',
                       size=[2, 1], pixels=SEED_5X1[:8], iterations=-2147483648)
    assert out['pixels'] == SEED_5X1[:8]
    checks.append('bleed.noChangeCases')

    rejected = [
        {'method': 'inspect'},
        {'method': 'warp', 'format': 'Rgba', 'size': [1, 1], 'pixels': [0, 0, 0, 0]},
        {'method': 'inspect', 'format': 'rgba', 'size': [1, 1], 'pixels': [0, 0, 0, 0]},
        {'method': 'inspect', 'format': 4, 'size': [1, 1], 'pixels': [0, 0, 0, 0]},
        {'method': 'inspect', 'format': 'Rgba', 'size': [1, 1], 'pixels': [0, 0, 0]},
        {'method': 'inspect', 'format': 'Rgba', 'size': [1, 1], 'pixels': [0, 0, 0, 256]},
        {'method': 'inspect', 'format': 'Rgba', 'size': [1, 1], 'pixels': [0, 0, 0, -1]},
        {'method': 'inspect', 'format': 'Rgba', 'size': [-1, 1], 'pixels': []},
        {'method': 'inspect', 'format': 'Mono', 'size': [257, 256], 'pixels': [0] * 65792},
        {'method': 'inspect', 'format': 'Rgba', 'size': [1, 1],
         'pixels': [0, 0, 0, 0], 'iterations': 257},
        {'method': 'inspect', 'format': 'Rgba', 'size': [1, 1],
         'pixels': [0, 0, 0, 0], 'iterations': 1.5},
        {'method': 'inspect', 'format': 'Rgba', 'size': [2, 1],
         'pixels': SEED_5X1[:8], 'iterations': -2147483649},
        {'method': 'bleed', 'format': 'Rgba', 'size': [2, 1],
         'pixels': SEED_5X1[:8], 'iterations': -4294967295},
        {'method': 'bleed', 'format': 'Rgba', 'size': [2, 1],
         'pixels': SEED_5X1[:8], 'iterations': -9223372036854775808},
        {'method': 'bleed', 'format': 'Rgb', 'size': [1, 1], 'pixels': [0, 0, 0]},
        {'method': 'bleed', 'format': 'Mono', 'size': [1, 1], 'pixels': [0]},
    ]
    for index, args in enumerate(rejected):
        peer.request('image.process', success=False, **args)
        assert peer.request('snapshot') == before, \
            f'rejected query {index} changed state'
    checks.append('rejects.snapshotUnchanged')

    assert peer.request('snapshot') == before
    return {'label': label, 'checks': checks}


def verify_mcp(peer, label):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'pixel-verifier', 'version': '1'}})
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    schema = tools['image_process']['inputSchema']
    assert schema['required'] == ['method', 'format', 'size', 'pixels']
    assert schema['properties']['method'] == {'type': 'string',
        'enum': ['inspect', 'bleed']}
    assert schema['properties']['format'] == {'type': 'string',
        'enum': ['Mono', 'Rgb', 'Rgba']}
    assert schema['properties']['size'] == {'type': 'array', 'minItems': 2,
        'maxItems': 2, 'items': {'type': 'integer', 'minimum': 0, 'maximum': 65536}}
    assert schema['properties']['pixels'] == {'type': 'array', 'minItems': 0,
        'maxItems': 262144, 'items': {'type': 'integer', 'minimum': 0, 'maximum': 255}}
    assert schema['properties']['iterations'] == {'type': 'integer',
        'minimum': -2147483648, 'maximum': 256}
    result = verify(peer, label)
    read = peer.rpc('resources/read', {'uri': 'resource://vsoul/snapshot'})
    observed = json.loads(read['contents'][0]['text'])
    assert observed == peer.request('snapshot')
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    binary = args.binary.resolve()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    (ROOT / 'out/verification').mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='pixel-image-', dir=ROOT / 'out/verification'))
    print(run, flush=True)
    results = {'binary': str(binary), 'binarySha256': digest, 'run': str(run)}
    try:
        peer = Jsonl(run, binary)
        try:
            results['jsonl'] = verify(peer, 'jsonl')
        finally:
            peer.close()
        peer = Mcp(run, binary)
        try:
            results['mcp'] = verify_mcp(peer, 'mcp')
        finally:
            peer.close()
        results['passed'] = True
    except Exception:
        results['passed'] = False
        results['error'] = traceback.format_exc()
        (run / 'result.json').write_text(json.dumps(results, indent=2))
        print(json.dumps({'passed': False, 'run': str(run)}), flush=True)
        sys.exit(1)
    (run / 'result.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'passed': True, 'run': str(run)}), flush=True)


if __name__ == '__main__':
    main()
