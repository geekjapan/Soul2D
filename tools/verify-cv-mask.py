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

CENTER_BG = [1, 1, 1, 1, 0, 1, 1, 1, 1]
BORDERED_SQUARE_7 = [
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
]
LINE_5X3 = [0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0]
GRID_3 = [255] * 9
DISCONNECTED_6X3 = [
    0, 0, 0, 0, 0, 0,
    255, 255, 255, 0, 255, 255,
    0, 0, 0, 0, 0, 0,
]
RECT_5 = [
    0, 0, 0, 0, 0,
    0, 255, 255, 255, 0,
    0, 255, 255, 255, 0,
    0, 255, 255, 255, 0,
    0, 0, 0, 0, 0,
]
RING_5 = [
    0, 0, 0, 0, 0,
    0, 255, 255, 255, 0,
    0, 255, 0, 255, 0,
    0, 255, 255, 255, 0,
    0, 0, 0, 0, 0,
]


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

    distance = peer.request('cv.analyze', method='distance', size=[3, 3], pixels=CENTER_BG)
    assert distance['method'] == 'distance' and distance['size'] == [3, 3]
    assert distance['hasBackground'] is True
    assert distance['distances'][0] == 1.4142135381698608, distance['distances']
    assert distance['distances'][1] == 1.0 and distance['distances'][4] == 0.0
    assert distance['nearest'] == [[1, 1]] * 9
    checks.append('distance.centerBg')

    distance = peer.request('cv.analyze', method='distance', size=[3, 2], pixels=[0] * 6)
    assert distance['distances'] == [0.0] * 6
    assert distance['nearest'] == [[0, 0], [1, 0], [2, 0], [0, 1], [1, 1], [2, 1]]
    distance = peer.request('cv.analyze', method='distance', size=[3, 2], pixels=[1] * 6)
    assert distance['hasBackground'] is False
    assert distance['distances'] is None and distance['nearest'] is None
    distance = peer.request('cv.analyze', method='distance', size=[3, 1], pixels=[0, 1, 0])
    assert distance['nearest'][1] == [0, 0]
    distance = peer.request('cv.analyze', method='distance', size=[1, 3], pixels=[0, 1, 0])
    assert distance['nearest'][1] == [0, 0]
    checks.append('distance.tiesAndNoBackground')

    thinned = peer.request('cv.analyze', method='thin', size=[7, 7],
                           pixels=BORDERED_SQUARE_7)
    expected = [0] * 49
    expected[24] = 255
    assert thinned['pixels'] == expected, thinned['pixels']
    thinned = peer.request('cv.analyze', method='thin', size=[3, 3], pixels=GRID_3)
    assert thinned['pixels'] == GRID_3
    checks.append('thin.literals')

    path = peer.request('cv.analyze', method='path', size=[3, 3], pixels=[0] * 9)
    assert path['path'] == []
    path = peer.request('cv.analyze', method='path', size=[3, 3],
                        pixels=[0, 0, 0, 0, 255, 0, 0, 0, 0])
    assert path['path'] == [[1, 1]], path['path']
    path = peer.request('cv.analyze', method='path', size=[5, 3], pixels=LINE_5X3)
    assert path['path'] == [[4, 1], [3, 1], [2, 1], [1, 1], [0, 1]], path['path']
    path = peer.request('cv.analyze', method='path', size=[3, 3], pixels=GRID_3)
    assert path['path'] == [[2, 0], [1, 0], [0, 0]], path['path']
    path = peer.request('cv.analyze', method='path', size=[6, 3],
                        pixels=DISCONNECTED_6X3)
    assert path['path'] == [[2, 1], [1, 1], [0, 1]], path['path']
    checks.append('path.correctedLiterals')

    for name, pixels in [('rect', RECT_5), ('ring', RING_5)]:
        found = peer.request('cv.analyze', method='contours', size=[5, 5], pixels=pixels)
        assert found['contours'] == [[[1, 1], [3, 1], [3, 3], [1, 3], [1, 1]]], \
            (name, found['contours'])
        assert found['hierarchy'] == [[-1, -1, -1, -1]]
    found = peer.request('cv.analyze', method='contours', size=[5, 5], pixels=RECT_5,
                         approximation='None')
    assert found['contours'] == [[[1, 1], [2, 1], [3, 1], [3, 2], [3, 3],
                                  [2, 3], [1, 3], [1, 2], [1, 1]]]
    for approximation in ['Tc89L1', 'Tc89Kcos']:
        found = peer.request('cv.analyze', method='contours', size=[5, 5],
                             pixels=RECT_5, approximation=approximation)
        assert found['contours'] == [[[1, 1], [3, 3], [1, 1]]], approximation
    for mode in ['External', 'List', 'Ccomp', 'Tree']:
        found = peer.request('cv.analyze', method='contours', size=[5, 5],
                             pixels=RECT_5, mode=mode)
        assert len(found['hierarchy']) == len(found['contours']) == 1, mode
        for index in found['hierarchy'][0]:
            assert index == -1 or 0 <= index < len(found['contours'])
    found = peer.request('cv.analyze', method='contours', size=[3, 3],
                         pixels=[0, 0, 0, 0, 255, 0, 0, 0, 0])
    assert found['contours'] == [[[1, 1]]]
    found = peer.request('cv.analyze', method='contours', size=[4, 3], pixels=[0] * 12)
    assert found['contours'] == [] and found['hierarchy'] == []
    checks.append('contours.modesAndApproximations')

    rejected = [
        {'method': 'distance'},
        {'method': 'warp', 'size': [1, 1], 'pixels': [0]},
        {'method': 'distance', 'size': [0, 1], 'pixels': [0]},
        {'method': 'distance', 'size': [2, 2], 'pixels': [0, 0, 0]},
        {'method': 'distance', 'size': [2, 2], 'pixels': [0, 0, 0, 256]},
        {'method': 'distance', 'size': [2, 2], 'pixels': [0, 0, 0, -1]},
        {'method': 'distance', 'size': [257, 256], 'pixels': [0] * 65792},
        {'method': 'distance', 'size': [1, 1], 'pixels': [0], 'mode': 'external'},
        {'method': 'distance', 'size': [1, 1], 'pixels': [0], 'approximation': 'fast'},
        {'method': 'distance', 'size': [1, 1], 'pixels': [0], 'mode': 3},
    ]
    for index, args in enumerate(rejected):
        peer.request('cv.analyze', success=False, **args)
        assert peer.request('snapshot') == before, f'rejected query {index} changed state'
    checks.append('rejects.snapshotUnchanged')

    assert peer.request('snapshot') == before
    return {'label': label, 'checks': checks}


def verify_mcp(peer, label):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'cv-verifier', 'version': '1'}})
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    schema = tools['cv_analyze']['inputSchema']
    assert schema['required'] == ['method', 'size', 'pixels']
    assert schema['properties']['method'] == {'type': 'string',
        'enum': ['distance', 'thin', 'path', 'contours']}
    assert schema['properties']['size'] == {'type': 'array', 'minItems': 2,
        'maxItems': 2, 'items': {'type': 'integer', 'minimum': 1, 'maximum': 65536}}
    assert schema['properties']['pixels'] == {'type': 'array', 'minItems': 1,
        'maxItems': 65536, 'items': {'type': 'integer', 'minimum': 0, 'maximum': 255}}
    assert schema['properties']['mode']['enum'] == ['External', 'List', 'Ccomp', 'Tree']
    assert schema['properties']['approximation']['enum'] == \
        ['None', 'Simple', 'Tc89L1', 'Tc89Kcos']
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
    run = Path(tempfile.mkdtemp(prefix='cv-mask-', dir=ROOT / 'out/verification'))
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
