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
        {'uuid': 41, 'name': 'Target', 'is_vec2': True, 'min': [-1.0, -1.0], 'max': [1.0, 1.0],
         'defaults': [0.0, 0.0], 'axis_points': [[0.0, 1.0], [0.0, 1.0]], 'merge_mode': 'Additive'},
    ],
    'animations': {
        'Idle': {'length': 12, 'timestep': 0.1, 'additive': False, 'animationWeight': 1.0,
                 'leadIn': -1, 'leadOut': -1, 'lanes': [
            {'uuid': 17, 'target': 1, 'interpolation': 'Linear', 'merge_mode': 'Forced',
             'keyframes': [{'frame': 0, 'value': 0.5, 'tension': 0.5},
                           {'frame': 4, 'value': 0.75, 'tension': 0.5}]},
            {'uuid': 17, 'target': 0, 'interpolation': 'Linear', 'merge_mode': 'Forced',
             'keyframes': [{'frame': 0, 'value': -0.25, 'tension': 0.5},
                           {'frame': 4, 'value': -0.5, 'tension': 0.5}]},
            {'uuid': 17, 'target': 0, 'interpolation': 'Linear', 'merge_mode': 'Forced',
             'keyframes': [{'frame': 4, 'value': 9.0, 'tension': 0.5}]},
            {'uuid': 41, 'target': 1, 'interpolation': 'Linear', 'merge_mode': 'Forced',
             'keyframes': [{'frame': 4, 'value': 0.9, 'tension': 0.5}]},
        ]},
        'Walk': {'length': 12, 'timestep': 0.1, 'additive': False, 'animationWeight': 1.0,
                 'leadIn': -1, 'leadOut': -1, 'lanes': [
            {'uuid': 41, 'target': 0, 'interpolation': 'Stepped', 'merge_mode': 'Additive',
             'keyframes': [{'frame': 6, 'value': 0.0, 'tension': 0.9}]},
            {'uuid': 41, 'target': 0, 'interpolation': 'Bezier', 'merge_mode': 'Weighted',
             'keyframes': [{'frame': 2, 'value': 5.0, 'tension': 0.1}]},
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
    snapshot = peer.request('snapshot')
    assert snapshot['animationClipboard'] == []

    copied = peer.request('animation.key.copy', name='Idle', parameter=41, frame=4)
    assert copied == {'count': 1, 'keys': [{'axis': 1, 'value': 0.9}]}, copied
    before = peer.request('snapshot')
    peer.request('animation.key.paste', name='Walk', parameter=23, frame=6, success=False)
    assert peer.request('snapshot') == before

    copied = peer.request('animation.key.copy', name='Idle', parameter=17, frame=0)
    assert copied == {'count': 2, 'keys': [{'axis': 1, 'value': 0.5},
                                          {'axis': 0, 'value': -0.25}]}, copied
    assert peer.request('snapshot')['animationClipboard'] == copied['keys']
    peer.request('animation.key.paste', name='Walk', parameter=23, frame=6)
    scalar = lanes(peer.request('snapshot'), 'Walk', 23)
    assert len(scalar) == 1 and scalar[0]['target'] == 0, scalar
    assert scalar[0]['keyframes'] == [{'frame': 6, 'value': -0.25, 'tension': 0.5}], scalar

    copied = peer.request('animation.key.copy', name='Idle', parameter=17, frame=4)
    assert copied == {'count': 3, 'keys': [{'axis': 1, 'value': 0.75},
                                          {'axis': 0, 'value': -0.5},
                                          {'axis': 0, 'value': 9.0}]}, copied
    pre_paste = peer.request('snapshot')
    peer.request('animation.key.paste', name='Walk', parameter=41, frame=6)
    pasted = peer.request('snapshot')
    assert pasted['revision'] == pre_paste['revision'] + 1
    target = lanes(pasted, 'Walk', 41)
    assert len(target) == 3, target
    assert target[0]['keyframes'] == [{'frame': 6, 'value': 9.0, 'tension': 0.9}], target[0]
    assert target[0]['interpolation'] == 'Stepped' and target[0]['merge_mode'] == 'Additive'
    assert target[1]['interpolation'] == 'Bezier' and target[1]['merge_mode'] == 'Weighted'
    assert target[1]['keyframes'] == [{'frame': 2, 'value': 5.0, 'tension': 0.1}]
    assert target[2]['target'] == 1 and target[2]['interpolation'] == 'Linear'
    assert target[2]['merge_mode'] == 'Forced'
    assert target[2]['keyframes'] == [{'frame': 6, 'value': 0.75, 'tension': 0.5}]

    peer.request('undo')
    assert peer.request('snapshot')['model'] == pre_paste['model']
    assert peer.request('snapshot')['canRedo'] is True
    peer.request('redo')
    assert peer.request('snapshot')['model'] == pasted['model']

    saved = run / f'{label}-saved.inx'
    peer.request('save', path=str(saved))
    peer.request('open', path=str(saved))
    reopened = peer.request('snapshot')
    assert reopened['model'] == pasted['model']
    assert reopened['animationClipboard'] == []
    peer.request('animation.key.paste', name='Walk', parameter=41, frame=6, success=False)
    return {'label': label, 'pastedRevision': pasted['revision'], 'savedKeys': len(target)}


def verify_mcp(peer, label, run):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'clipboard-verifier', 'version': '1'}})
    result = verify(peer, label, run)
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    frame = {'type': 'integer', 'minimum': 0, 'maximum': 2147483647}
    for name in ['animation_key_set', 'animation_key_delete',
                 'animation_key_copy', 'animation_key_paste']:
        assert tools[name]['inputSchema']['properties']['frame'] == frame, name
    for name in ['animation_key_copy', 'animation_key_paste']:
        assert tools[name]['inputSchema']['required'] == ['name', 'parameter', 'frame'], name
    read = peer.rpc('resources/read', {'uri': 'resource://vsoul/snapshot'})
    observed = json.loads(read['contents'][0]['text'])
    assert observed == peer.request('snapshot')
    assert observed['animationClipboard'] == []
    assert len(lanes(observed, 'Walk', 41)) == 3
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    binary = args.binary.resolve()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    (ROOT / 'out/verification').mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='animation-clipboard-', dir=ROOT / 'out/verification'))
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
