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

PACK = {'size': [4, 4], 'rectangles': [[2, 2], [2, 2], [2, 2], [2, 2], [2, 2]]}
PLACEMENTS = [[0, 0, 2, 2], [0, 2, 2, 2], [2, 0, 2, 2], [2, 2, 2, 2], None]


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
    before = peer.request('snapshot')
    planned = peer.request('atlas.pack', **PACK)
    assert planned['size'] == [4, 4]
    assert planned['placements'] == PLACEMENTS, planned['placements']
    assert planned['occupancy'] == 1.0
    rotated = peer.request('atlas.pack', size=[2, 3], rectangles=[[3, 2]])
    assert rotated['placements'] == [None]
    peer.request('atlas.pack', size=[4, 4], rectangles=[[2, 2], 'bad'], success=False)
    peer.request('atlas.pack', size=[0, 4], rectangles=[[2, 2]], success=False)
    peer.request('atlas.pack', size=[4, 4], rectangles=[], success=False)
    assert peer.request('snapshot') == before
    return {'label': label, 'placements': PLACEMENTS, 'occupancy': 1.0}


def verify_mcp(peer, label):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'atlas-verifier', 'version': '1'}})
    result = verify(peer, label)
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    schema = tools['atlas_pack']['inputSchema']
    assert schema['required'] == ['size', 'rectangles']
    assert schema['properties']['size'] == {'type': 'array', 'minItems': 2, 'maxItems': 2,
        'items': {'type': 'integer', 'minimum': 1, 'maximum': 32768}}
    assert schema['properties']['rectangles'] == {'type': 'array', 'minItems': 1,
        'maxItems': 4096, 'items': {'type': 'array', 'minItems': 2, 'maxItems': 2,
        'items': {'type': 'integer', 'minimum': 1, 'maximum': 4294967295}}}
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
    run = Path(tempfile.mkdtemp(prefix='atlas-pack-', dir=ROOT / 'out/verification'))
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
