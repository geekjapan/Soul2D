#!/usr/bin/env python3
import argparse
import importlib.util
import http.client
import json
import os
from pathlib import Path
import queue
import socket
import struct
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('acp_fixture', Path(__file__).with_name('acp-protocol-fixture.py'))
BASE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(BASE)


def write(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description='Owned deterministic ACP peer for host panel-approval checks; not an AI agent.')
    parser.add_argument('--run-dir', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'fixture requires assertions'
    instances = args.run_dir / 'instances'
    instances.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix=f'agent-{os.getpid()}-', dir=instances))
    (run / 'commands').mkdir()
    (run / 'results').mkdir()
    write(run / 'instance.json', {'pid': os.getpid(), 'fixture': 'deterministic ACP panel approval'})
    incoming = queue.Queue()

    def reader():
        for line in sys.stdin:
            incoming.put(json.loads(line))
        incoming.put(None)

    threading.Thread(target=reader, daemon=True).start()
    server = None
    active = None
    generation = 0
    seen = set()
    workers = []

    def rpc(command, receipt, endpoint, headers):
        request = command['request']
        try:
            if command.get('reset_after_review'):
                assert os.name == 'posix', 'reset fixture requires Unix socket linger layout'
                url = urllib.parse.urlparse(endpoint)
                peer = http.client.HTTPConnection(url.hostname, url.port, timeout=150)
                peer.request('POST', url.path or '/', body=json.dumps(request).encode(),
                             headers={'Content-Type': 'application/json', **headers})
                gate = receipt.with_suffix('.reset')
                deadline = time.monotonic() + 30
                while not gate.exists():
                    assert time.monotonic() < deadline, 'review reset gate timed out'
                    time.sleep(0.01)
                peer.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('ii', 1, 0))
                peer.close()
                write(receipt, {'request': request, 'resetSent': True})
                return
            peer = urllib.request.Request(endpoint, data=json.dumps(request).encode(),
                                          headers={'Content-Type': 'application/json', **headers}, method='POST')
            with urllib.request.urlopen(peer, timeout=150) as reply:
                body = reply.read()
                length = reply.headers.get('Content-Length')
                if reply.status == 204:
                    assert not body and (length is None or int(length) == 0), 'invalid bodyless HTTP response'
                else:
                    assert length is not None and int(length) == len(body), 'incomplete HTTP response framing'
                result = {'status': reply.status, 'response': json.loads(body) if body else None,
                          'contentLength': None if length is None else int(length), 'bodyLength': len(body), 'body': body.decode()}
            write(receipt, {'request': request, **result})
        except Exception as error:
            write(receipt, {'request': request, 'error': str(error)})

    while True:
        try:
            message = incoming.get(timeout=0.05)
        except queue.Empty:
            message = {}
        if message is None:
            return
        method = message.get('method')
        if method == 'initialize':
            BASE.send(BASE.init_result(message['id'], True))
        elif method == 'session/new':
            servers = message['params']['mcpServers']
            assert len(servers) == 1 and servers[0]['type'] == 'http' and servers[0]['name'] == 'soul2d'
            server = servers[0]
            url = urllib.parse.urlparse(server['url'])
            assert url.scheme == 'http' and url.hostname in ['127.0.0.1', '::1', 'localhost']
            headers = {entry['name']: entry['value'] for entry in server['headers']}
            assert any(key.lower() == 'authorization' and value.startswith('Bearer ') for key, value in headers.items())
            BASE.response(message, {'sessionId': 'owned-panel-approval-fixture'})
            write(run / 'session-ready.json', {'transport': 'http', 'tokenPresent': True, 'headersRecorded': False})
        elif method == 'session/prompt':
            assert active is None and server is not None
            active = message
            generation += 1
            BASE.send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {
                'sessionId': 'owned-panel-approval-fixture', 'update': {'sessionUpdate': 'agent_message_chunk',
                'content': {'type': 'text', 'text': 'Panel approval fixture is waiting for scripted MCP calls.'}}}})
            write(run / 'prompt-ready.json', {'generation': generation, 'active': True, 'promptId': message['id']})
        elif method == 'session/cancel':
            if active is not None:
                BASE.response(active, {'stopReason': 'cancelled'})
                active = None
            write(run / 'cancelled.json', {'generation': generation})
        elif method == 'shutdown':
            BASE.response(message, {})
        elif method == 'exit':
            return
        for file in sorted((run / 'commands').glob('*.json')):
            if file.name in seen:
                continue
            assert file.stem.isdecimal()
            command = json.loads(file.read_text())
            seen.add(file.name)
            if command.get('action') == 'finish':
                assert active is not None
                response = dict(active)
                response['id'] = command.get('response_id', active['id'])
                sent_response = {'jsonrpc': '2.0', 'id': response['id']}
                if 'error' in command:
                    sent_response['error'] = command['error']
                else:
                    sent_response['result'] = command.get('result', {'stopReason': 'end_turn'})
                BASE.send(sent_response)
                if response['id'] == active['id']:
                    active = None
                write(run / 'results' / file.name, {'finished': True, 'generation': generation, 'response': sent_response})
            elif command.get('action') == 'permission':
                assert active is not None
                BASE.send({'jsonrpc': '2.0', 'id': command.get('id', 'late-permission'),
                           'method': 'session/request_permission', 'params': {
                               'sessionId': 'owned-panel-approval-fixture',
                               'toolCall': {'toolCallId': 'late-tool', 'title': 'Late deterministic permission'},
                               'options': [{'optionId': 'allow-once', 'name': 'Allow once', 'kind': 'allow_once'}]}})
                write(run / 'results' / file.name, {'permissionSent': True, 'generation': generation})
            elif command.get('action') == 'disconnect':
                write(run / 'results' / file.name, {'disconnected': True, 'generation': generation})
                os._exit(0)
            else:
                assert active is not None and server is not None
                workers = [worker for worker in workers if worker.is_alive()]
                assert len(workers) < 4, 'fixture allows at most four concurrent calls'
                headers = {entry['name']: entry['value'] for entry in server['headers']}
                worker = threading.Thread(target=rpc, args=(command, run / 'results' / file.name, server['url'], headers), daemon=True)
                worker.start()
                workers.append(worker)


if __name__ == '__main__':
    main()
