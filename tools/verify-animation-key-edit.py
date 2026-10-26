#!/usr/bin/env python3
import argparse
import hashlib
import json
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import traceback

ROOT = Path(__file__).resolve().parents[1]

MODEL = {
    'nodes': {'uuid': 1, 'name': 'Root', 'type': 'Node'},
    'param': [
        {'uuid': 17, 'name': 'Move', 'is_vec2': True, 'min': [-1.0, -1.0], 'max': [1.0, 1.0],
         'defaults': [0.0, 0.0], 'axis_points': [[0.0, 1.0], [0.0, 1.0]], 'merge_mode': 'Additive'},
        {'uuid': 23, 'name': 'Blink', 'is_vec2': False, 'min': [-1.0, 0.0], 'max': [1.0, 1.0],
         'defaults': [0.0, 0.0], 'axis_points': [[0.0, 1.0], [0.0]], 'merge_mode': 'Additive'},
    ],
    'animations': {
        'Idle': {'length': 12, 'timestep': 0.1, 'additive': False, 'animationWeight': 1.0,
                 'leadIn': -1, 'leadOut': -1, 'lanes': [
            {'uuid': 17, 'target': 0, 'interpolation': 'Linear', 'merge_mode': 'Forced',
             'keyframes': [{'frame': 0, 'value': -0.25, 'tension': 0.5},
                           {'frame': 4, 'value': -0.5, 'tension': 0.5}]},
            {'uuid': 17, 'target': 1, 'interpolation': 'Stepped', 'merge_mode': 'Additive',
             'keyframes': [{'frame': 0, 'value': 0.5, 'tension': 0.9},
                           {'frame': 4, 'value': 0.75, 'tension': 0.9}]},
        ]},
    },
}


def inx(model):
    payload = json.dumps(model, separators=(',', ':')).encode()
    return b'TRNSRTS\0' + struct.pack('>I', len(payload)) + payload + b'TEX_SECT' + struct.pack('>I', 0)


def lanes(snapshot, animation, parameter):
    return [lane for lane in snapshot['model']['animations'][animation].get('lanes', [])
            if lane['uuid'] == parameter]


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


def verify(peer, label, run):
    source = run / f'{label}-source.inx'
    source.write_bytes(inx(MODEL))
    peer.request('open', path=str(source))
    before = peer.request('snapshot')
    revision = before['revision']

    peer.request('value.set', id=17, value=[0.125, 0.375])
    added = peer.request('animation.key.add', name='Idle', parameter=17, frame=2)
    assert added['revision'] == revision + 1, added
    snapshot = peer.request('snapshot')
    axis0 = lanes(snapshot, 'Idle', 17)[0]
    axis1 = lanes(snapshot, 'Idle', 17)[1]
    assert axis0['keyframes'] == [{'frame': 0, 'value': -0.25, 'tension': 0.5},
                                  {'frame': 2, 'value': 0.125, 'tension': 0.5},
                                  {'frame': 4, 'value': -0.5, 'tension': 0.5}], axis0
    assert axis1['keyframes'] == [{'frame': 0, 'value': 0.5, 'tension': 0.9},
                                  {'frame': 2, 'value': 0.375, 'tension': 0.5},
                                  {'frame': 4, 'value': 0.75, 'tension': 0.9}], axis1
    assert axis1['interpolation'] == 'Stepped' and axis1['merge_mode'] == 'Additive'

    peer.request('undo')
    assert len(lanes(peer.request('snapshot'), 'Idle', 17)[0]['keyframes']) == 2
    peer.request('redo')

    peer.request('value.set', id=23, value=[0.6, 0.0])
    peer.request('animation.key.add', name='Idle', parameter=23, frame=8)
    scalar = lanes(peer.request('snapshot'), 'Idle', 23)
    assert len(scalar) == 1 and scalar[0]['target'] == 0
    assert scalar[0]['keyframes'] == [{'frame': 8, 'value': 0.6, 'tension': 0.5}]

    copied = peer.request('animation.key.copy', name='Idle', parameter=17, frame=2)
    assert copied['keys'] == [{'axis': 0, 'value': 0.125}, {'axis': 1, 'value': 0.375}]
    clipboard = peer.request('snapshot')['animationClipboard']
    peer.request('animation.key.add', name='Idle', parameter=23, frame=3)
    assert peer.request('snapshot')['animationClipboard'] == clipboard

    peer.request('animation.key.paste', name='Idle', parameter=23, frame=6)
    pasted = lanes(peer.request('snapshot'), 'Idle', 23)[0]
    assert pasted['keyframes'] == [{'frame': 3, 'value': 0.6, 'tension': 0.5},
                                   {'frame': 6, 'value': 0.125, 'tension': 0.5},
                                   {'frame': 8, 'value': 0.6, 'tension': 0.5}], pasted

    for bad in ({'name': 'Missing', 'parameter': 17, 'frame': 2},
                {'name': 'Idle', 'parameter': 17, 'frame': 99},
                {'name': 'Idle', 'parameter': 999, 'frame': 2}):
        before = peer.request('snapshot')
        peer.request('animation.key.add', success=False, **bad)
        assert peer.request('snapshot') == before, bad

    source_bytes = source.read_bytes()
    saved = run / f'{label}-saved.inx'
    peer.request('save', path=str(saved))
    peer.request('open', path=str(saved))
    reopened = peer.request('snapshot')
    assert reopened['animationClipboard'] == []
    axis17 = {lane['target']: lane for lane in lanes(reopened, 'Idle', 17)}
    assert axis17[0]['keyframes'] == [{'frame': 0, 'value': -0.25, 'tension': 0.5},
                                      {'frame': 2, 'value': 0.125, 'tension': 0.5},
                                      {'frame': 4, 'value': -0.5, 'tension': 0.5}], axis17[0]
    assert axis17[1]['keyframes'] == [{'frame': 0, 'value': 0.5, 'tension': 0.9},
                                      {'frame': 2, 'value': 0.375, 'tension': 0.5},
                                      {'frame': 4, 'value': 0.75, 'tension': 0.9}], axis17[1]
    axis23 = lanes(reopened, 'Idle', 23)
    assert len(axis23) == 1
    assert axis23[0]['keyframes'] == [{'frame': 3, 'value': 0.6, 'tension': 0.5},
                                      {'frame': 6, 'value': 0.125, 'tension': 0.5},
                                      {'frame': 8, 'value': 0.6, 'tension': 0.5}], axis23[0]
    assert source.read_bytes() == source_bytes
    return {'label': label, 'revision': reopened['revision'],
            'sourceSha256': hashlib.sha256(source_bytes).hexdigest()}


def verify_mcp(peer, label, run):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'key-edit-verifier', 'version': '1'}})
    result = verify(peer, label, run)
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    schema = tools['animation_key_add']['inputSchema']
    assert schema['required'] == ['name', 'parameter', 'frame']
    assert schema['properties']['frame'] == {'type': 'integer', 'minimum': 0,
                                             'maximum': 2147483647}
    read = peer.rpc('resources/read', {'uri': 'resource://vsoul/snapshot'})
    observed = json.loads(read['contents'][0]['text'])
    assert observed == peer.request('snapshot')
    assert len(lanes(observed, 'Idle', 17)[0]['keyframes']) == 3
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    binary = args.binary.resolve()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    (ROOT / 'out/verification').mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='animation-key-edit-', dir=ROOT / 'out/verification'))
    print(run, flush=True)
    results = {'binary': str(binary), 'binarySha256': digest, 'run': str(run)}
    try:
        peer = Jsonl(run, binary)
        try:
            results['jsonl'] = verify(peer, 'jsonl', run)
        finally:
            peer.close()
        peer = Mcp(run, binary)
        try:
            results['mcp'] = verify_mcp(peer, 'mcp', run)
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
