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

MODEL = {
    'meta': {'name': 'fixture', 'version': '1.0-alpha'},
    'nodes': {
        'uuid': 1, 'name': 'Fixture', 'type': 'Node', 'enabled': True,
        'zsort': 0.0,
        'transform': {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                      'scale': [1.0, 1.0]},
        'lockToRoot': False, 'pinToMesh': False,
        'children': [{
            'uuid': 9, 'name': 'Root', 'type': 'Node', 'enabled': True,
            'zsort': 0.0,
            'transform': {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                          'scale': [1.0, 1.0]},
            'lockToRoot': False, 'pinToMesh': False,
        }],
    },
    'param': [],
    'animations': {},
}


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


def root_name(snapshot):
    return snapshot['model']['nodes']['children'][0]['name']


def verify(peer, run, label):
    checks = []
    peer.request('new', model=MODEL)
    before = peer.request('snapshot')

    summary = peer.request('history.summary')
    assert summary == {
        'position': 0, 'count': 0, 'limit': 100,
        'revision': before['revision'],
        'states': [{'index': 0, 'label': '', 'current': True, 'applied': True}],
    }, summary
    assert before['history'] == summary
    assert peer.request('snapshot') == before
    checks.append('summary.initial')

    for name in ['First', 'Second', 'Third']:
        peer.request('node.rename', id=9, name=name)
    revision = peer.request('snapshot')['revision']
    summary = peer.request('history.summary')
    assert summary['position'] == 3 and summary['count'] == 3
    assert len(summary['states']) == 4
    assert summary['states'][3] == {'index': 3, 'label': 'node.rename', 'current': True, 'applied': True}
    assert summary['states'][0] == {'index': 0, 'label': '', 'current': False, 'applied': True}
    checks.append('summary.afterRenames')

    snapshot = peer.request('history.goto', index=0, expectedRevision=revision)
    assert root_name(snapshot) == 'Root'
    assert snapshot['canRedo'] is True and snapshot['canUndo'] is False
    assert snapshot['revision'] == revision + 1
    assert snapshot['history']['position'] == 0
    checks.append('goto.root')

    revision = snapshot['revision']
    snapshot = peer.request('history.goto', index=1, expectedRevision=revision)
    assert root_name(snapshot) == 'First'
    peer.request('node.rename', id=9, name='Fourth')
    summary = peer.request('history.summary')
    assert summary['position'] == 2 and summary['count'] == 2
    assert len(summary['states']) == 3
    assert peer.request('snapshot')['canRedo'] is False
    checks.append('goto.branchTruncation')

    revision = peer.request('snapshot')['revision']
    before = peer.request('snapshot')
    snapshot = peer.request('history.goto', index=2, expectedRevision=revision)
    assert snapshot == before
    checks.append('goto.sameIndexNoop')

    rejected = [
        {},
        {'index': 0},
        {'expectedRevision': revision},
        {'index': 0, 'expectedRevision': revision + 9},
        {'index': 99, 'expectedRevision': revision},
        {'index': -1, 'expectedRevision': revision},
        {'index': 0.5, 'expectedRevision': revision},
        {'index': '0', 'expectedRevision': revision},
        {'index': 0, 'expectedRevision': 'x'},
        {'index': 18446744073709551615, 'expectedRevision': revision},
    ]
    for index, args in enumerate(rejected):
        peer.request('history.goto', success=False, **args)
        assert peer.request('snapshot') == before, \
            f'rejected goto {index} changed state'
    checks.append('goto.rejectsSnapshotUnchanged')

    for index in range(1, 106):
        peer.request('node.rename', id=9, name=f'State{index}')
    summary = peer.request('history.summary')
    assert summary['count'] == 99 and summary['position'] == 99
    assert len(summary['states']) == 100
    snapshot = peer.request('history.goto', index=0,
                            expectedRevision=summary['revision'])
    assert root_name(snapshot) == 'State6'
    checks.append('eviction.oldestRetained')

    saved = run / f'saved-{label}.inx'
    peer.request('node.rename', id=9, name='SavedFirst')
    peer.request('save', path=str(saved))
    assert peer.request('snapshot')['modified'] is False
    peer.request('node.rename', id=9, name='SavedSecond')
    revision = peer.request('snapshot')['revision']
    summary = peer.request('history.summary')
    first_index = summary['count'] - 1
    snapshot = peer.request('history.goto', index=first_index,
                            expectedRevision=revision)
    assert root_name(snapshot) == 'SavedFirst'
    assert snapshot['modified'] is False
    snapshot = peer.request('history.goto', index=0,
                            expectedRevision=snapshot['revision'])
    assert snapshot['modified'] is True
    checks.append('goto.modifiedTracksSaved')

    reopened = peer.request('open', path=str(saved))
    assert reopened['history']['position'] == 0
    assert reopened['history']['count'] == 0
    assert len(reopened['history']['states']) == 1
    assert root_name(reopened) == 'SavedFirst'
    checks.append('open.rebuildsHistory')
    return {'label': label, 'checks': checks}


def verify_mcp(peer, run, label):
    peer.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'history-verifier', 'version': '1'}})
    tools = {tool['name']: tool for tool in peer.rpc('tools/list', {})['tools']}
    assert tools['history_summary']['inputSchema']['required'] == []
    schema = tools['history_goto']['inputSchema']
    assert schema['required'] == ['index', 'expectedRevision']
    assert schema['properties']['index'] == {'type': 'integer', 'minimum': 0}
    assert schema['properties']['expectedRevision'] == {'type': 'integer', 'minimum': 0}
    result = verify(peer, run, label)
    read = peer.rpc('resources/read', {'uri': 'resource://vsoul/snapshot'})
    observed = json.loads(read['contents'][0]['text'])
    assert observed == peer.request('snapshot')
    assert observed['history'] == peer.request('history.summary')
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    binary = args.binary.resolve()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    (ROOT / 'out/verification').mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='history-navigation-', dir=ROOT / 'out/verification'))
    print(run, flush=True)
    results = {'binary': str(binary), 'binarySha256': digest, 'run': str(run)}
    try:
        peer = Jsonl(run, binary)
        try:
            results['jsonl'] = verify(peer, run, 'jsonl')
        finally:
            peer.close()
        peer = Mcp(run, binary)
        try:
            results['mcp'] = verify_mcp(peer, run, 'mcp')
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
