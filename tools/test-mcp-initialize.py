#!/usr/bin/env python3
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class InitializeTests(unittest.TestCase):
    def probe(self, mode, editor=False):
        peer = r'''
import json, sys
requests = [json.loads(line) for line in sys.stdin]
editor = len(requests) == 3
assert [r['method'] for r in requests] == (['initialize', 'resources/read', 'tools/call'] if editor else ['initialize'])
if editor:
    assert requests[1]['params']['uri'] == 'resource://soul2d/editor/state'
    assert requests[2]['params'] == {'name': 'editor_quit', 'arguments': {}}
replies = [{'jsonrpc': '2.0', 'id': 1, 'result': {'protocolVersion': '2024-11-05', 'serverInfo': {'name': 'soul2d-core', 'version': 'test'}}}]
if editor:
    state = {'editor': sys.argv[1] != 'wrong-state', 'document': {'path': None, 'modified': False}}
    replies += [{'jsonrpc': '2.0', 'id': 2, 'result': {'contents': [{'text': json.dumps(state)}]}},
                {'jsonrpc': '2.0', 'id': 3, 'result': {'isError': False, 'structuredContent': {'quit': {'state': 'accepted'}}}}]
if sys.argv[1] == 'missing-quit': replies.pop()
if sys.argv[1] == 'quit-error': replies[-1]['result']['isError'] = True
if sys.argv[1] == 'wrong-id': replies[0]['id'] = 8
if sys.argv[1] == 'noise': print('not JSON')
for reply in replies: print(json.dumps(reply))
sys.exit(1 if sys.argv[1] == 'exit-error' else 0)
'''
        with tempfile.TemporaryDirectory() as temporary:
            script = Path(temporary) / 'peer.py'
            script.write_text(peer, encoding='utf-8')
            command = [sys.executable, str(Path(__file__).with_name('verify-mcp-initialize.py'))]
            if editor:
                command.append('--editor')
            return subprocess.run(command + [sys.executable, str(script), mode], capture_output=True, text=True, timeout=10)

    def test_core(self):
        result = self.probe('pass')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_editor(self):
        result = self.probe('pass', editor=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_incomplete_or_invalid_editor(self):
        for mode in ('wrong-state', 'missing-quit', 'quit-error', 'wrong-id', 'noise', 'exit-error'):
            with self.subTest(mode=mode):
                self.assertNotEqual(self.probe(mode, editor=True).returncode, 0)


if __name__ == '__main__':
    unittest.main()
