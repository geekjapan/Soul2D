#!/usr/bin/env python3
"""Observe a real CLI agent against a scratch-only production Rust MCP server."""
import hashlib
import importlib.util
import json
import os
import shutil
import signal
from pathlib import Path
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
CORE = ROOT / 'out/rust/debug/soul2d-core'
CODEX = shutil.which('codex')
ALLOWED = ['open', 'snapshot', 'node_rename', 'node_transform', 'undo', 'redo', 'save', 'bounds', 'render']


def permitted(params, run):
    name, args = params.get('name'), params.get('arguments', {})
    if name not in ALLOWED:
        return False
    if name in {'open', 'save'} or (name == 'render' and 'path' in args):
        filenames = {'open': ['source.inx', 'saved.inx'], 'save': ['saved.inx'], 'render': ['preview.png']}
        path = args.get('path')
        return isinstance(path, str) and Path(path).resolve() in [run / file for file in filenames[name]]
    return True


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def proxy(run):
    child = subprocess.Popen([str(CORE), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=sys.stderr, text=True, bufsize=1)
    (run / 'core-pid.json').write_text(json.dumps({'pid': child.pid, 'proxy_pid': os.getpid()}))
    lock = threading.Lock()
    output_lock = threading.Lock()
    with (run / 'wire.jsonl').open('a', buffering=1) as log:
        def record(direction, message):
            with lock:
                log.write(json.dumps({'direction': direction, 'message': message}) + '\n')

        def receive():
            for line in child.stdout:
                record('server', json.loads(line))
                with output_lock:
                    sys.stdout.write(line)
                    sys.stdout.flush()

        reader = threading.Thread(target=receive)
        reader.start()
        try:
            for line in sys.stdin:
                message = json.loads(line)
                record('client', message)
                if message.get('method') == 'tools/call':
                    params = message['params']
                    if not permitted(params, run):
                        response = {'jsonrpc': '2.0', 'id': message['id'], 'result': {
                            'isError': True, 'content': [{'type': 'text', 'text': 'Outside scratch verification scope'}]}}
                        record('server', response)
                        with output_lock:
                            print(json.dumps(response), flush=True)
                        continue
                child.stdin.write(line)
                child.stdin.flush()
        finally:
            child.stdin.close()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            reader.join(timeout=5)
            assert not reader.is_alive()


def verify(run, original, tail, digest):
    helper = module('core_fixture', ROOT / 'tools/verify-rust-core.py')
    png_helper = module('acp_fixture', ROOT / 'tools/acp-protocol-fixture.py')
    assert hashlib.sha256((run / 'source.inx').read_bytes()).hexdigest() == digest
    saved, saved_tail = helper.split_container((run / 'saved.inx').read_bytes())
    expected = json.loads(json.dumps(original))
    expected['nodes']['children'][0]['name'] = 'AI verified'
    expected['nodes']['children'][0]['transform']['trans'] = [3, -2, 0]
    assert saved == expected, 'Unexpected persisted document change'
    assert saved_tail == tail, 'Opaque texture/extension bytes changed'
    wire = [json.loads(line) for line in (run / 'wire.jsonl').read_text().splitlines()]
    requests = {json.dumps(e['message']['id']): e['message'] for e in wire
                if e['direction'] == 'client' and 'id' in e['message']}
    methods = {request['method'] for request in requests.values()}
    assert {'initialize', 'tools/list', 'resources/list'} <= methods
    render_requests = [r['params']['arguments'] for r in requests.values()
                       if r.get('method') == 'tools/call' and r['params']['name'] == 'render']
    assert any(r.get('width') == 32 and r.get('height') == 32 and
               r.get('bounds') == [0, 0, 12, 12] and r.get('path') == str(run / 'preview.png')
               for r in render_requests)
    calls, rejected, discoveries, metadata = [], [], [], []
    for entry in wire:
        response = entry['message']
        if entry['direction'] != 'server' or 'id' not in response:
            continue
        request = requests[json.dumps(response['id'])]
        if request.get('method') != 'tools/call':
            if 'error' in response:
                discoveries.append({'method': request['method'], 'error': response['error']})
            continue
        metadata.append(request['params'].get('_meta', {}).get('x-codex-turn-metadata', {}))
        result = response.get('result', {})
        assert 'error' not in response, response
        if result.get('isError'):
            assert calls and calls[-1][0] == 'snapshot', 'Rejected call lacks preceding readback'
            rejected.append({'tool': request['params']['name'], 'result': result, 'before': calls[-1][1]})
            continue
        if rejected and 'after' not in rejected[-1]:
            assert request['params']['name'] == 'snapshot', 'Rejected call lacks immediate readback'
            assert result['structuredContent'] == rejected[-1]['before'], 'Rejected call mutated state'
            rejected[-1]['after'] = result['structuredContent']
        calls.append((request['params']['name'], result.get('structuredContent')))
    assert metadata and all(m.get('model') == 'gpt-6-luna' and m.get('reasoning_effort') == 'max' for m in metadata)
    assert all('after' in rejected_call for rejected_call in rejected)
    names = [name for name, _ in calls]
    for name in ['open', 'snapshot', 'node_rename', 'node_transform', 'undo', 'redo', 'save', 'bounds', 'render']:
        assert name in names, name
    assert names.count('open') >= 2
    def snapshot_after(action, translation):
        index = names.index(action)
        assert names[index + 1] == 'snapshot', 'Missing immediate readback after ' + action
        model = calls[index + 1][1]['model']
        assert model['nodes']['children'][0]['name'] == 'AI verified'
        assert model['nodes']['children'][0]['transform']['trans'] == translation
    snapshot_after('node_transform', [3, -2, 0])
    snapshot_after('undo', [0, 0, 0])
    snapshot_after('redo', [3, -2, 0])
    reopened = calls[names.index('open', names.index('open') + 1) + 1]
    assert reopened[0] == 'snapshot' and reopened[1]['model'] == expected
    bounds = next(result['bounds'] for name, result in calls if name == 'bounds')
    assert all(abs(a - b) < 1e-5 for a, b in zip(bounds, [4.6, -0.4, 9.4, 4.4])) and len(bounds) == 4
    data = (run / 'preview.png').read_bytes()
    png_helper.validate_png(data, 32, 32)
    width, height, pixels = helper.decode_rgba(data)
    assert pixels[(5 * width + 18) * 4:(5 * width + 18) * 4 + 4] == bytes([255, 0, 0, 255])
    assert pixels[(10 * width + 10) * 4:(10 * width + 10) * 4 + 4] == bytes([0, 0, 0, 0])
    events = [json.loads(line) for line in (run / 'codex.jsonl').read_text().splitlines()]
    assert any(event.get('type') == 'item.completed' and
               event.get('item', {}).get('type') == 'command_execution' and
               event['item'].get('exit_code') == 0 and
               'verify-vsoul/SKILL.md' in event['item'].get('command', '') and
               'name: verify-vsoul' in event['item'].get('aggregated_output', '') for event in events)
    result = {'status': 'PASS', 'surface': 'real Codex CLI / guarded Rust stdio MCP / Skill',
              'bounds': bounds, 'calls': names, 'source_sha256': digest,
              'runtime_model': 'gpt-6-luna', 'runtime_reasoning_effort': 'max',
              'rejected_calls': rejected, 'discovery_errors': discoveries,
              'saved_sha256': hashlib.sha256((run / 'saved.inx').read_bytes()).hexdigest(),
              'png_sha256': hashlib.sha256(data).hexdigest(),
              'appearance_check': '32x32 valid RGBA PNG; translated interior red and old position transparent',
              'limitations': ['No full-image equivalence claim', 'No ACP agent', 'No native GUI', 'No Live2D']}
    (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: result[k] for k in ['status', 'surface', 'bounds', 'runtime_model', 'runtime_reasoning_effort', 'discovery_errors']}, indent=2))


def finish_children(run):
    marker = run / 'core-pid.json'
    if not marker.exists():
        return
    pids = json.loads(marker.read_text())
    for key, expected in [('proxy_pid', str(Path(__file__).resolve()) + ' --proxy ' + str(run)),
                          ('pid', str(CORE) + ' --mcp')]:
        pid = pids[key]
        for attempt in range(12):
            process = subprocess.run(['ps', '-p', str(pid), '-o', 'stat=,command='],
                                     capture_output=True, text=True)
            if process.returncode or process.stdout.strip().startswith('Z'):
                break
            assert expected in process.stdout, f'Recorded PID {pid} no longer matches this run'
            if attempt in {5, 10}:
                os.kill(pid, signal.SIGTERM if attempt == 5 else signal.SIGKILL)
            time.sleep(0.5)
        else:
            raise AssertionError(f'Owned child {pid} did not stop')
    (run / 'cleanup.json').write_text(json.dumps({'owned_children_stopped': True, **pids}) + '\n')


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--proxy':
        proxy(Path(sys.argv[2]).resolve())
        return
    if sys.argv[1:] == ['--self-check']:
        run = ROOT / 'build/verification'
        assert permitted({'name': 'open', 'arguments': {'path': str(run / 'source.inx')}}, run)
        assert permitted({'name': 'save', 'arguments': {'path': str(run / 'saved.inx')}}, run)
        assert not permitted({'name': 'open', 'arguments': {'path': str(ROOT / 'source.inx')}}, run)
        assert not permitted({'name': 'save', 'arguments': {'path': str(run / 'source.inx')}}, run)
        assert not permitted({'name': 'save', 'arguments': {}}, run)
        assert not permitted({'name': 'node_create', 'arguments': {}}, run)
        print('scratch guard self-check PASS')
        return
    assert CODEX, 'Install/configure Codex CLI separately; no automatic installation or login'
    run = ROOT / 'build/verification' / ('real-ai-mcp-' + time.strftime('%Y%m%d-%H%M%S'))
    run.mkdir()
    helper = module('core_fixture', ROOT / 'tools/verify-rust-core.py')
    original, tail = helper.fixture(run / 'source.inx')
    digest = hashlib.sha256((run / 'source.inx').read_bytes()).hexdigest()
    prompt = f'''You are the worker for a scratch-only Soul2D verification. Do not edit code, build, run Git, install, or access other projects. Read {ROOT}/.cursor/skills/verify-vsoul/SKILL.md using a read-only shell command. Then use ONLY the vsoul MCP tools for this workflow. Explore only vsoul tools/resources (pass server=vsoul). Open {run}/source.inx and snapshot. Rename UUID 2 to "AI verified" and snapshot. Translate UUID 2 to [3,-2,0] and immediately snapshot. Undo and immediately snapshot. Redo and immediately snapshot. Save to {run}/saved.inx, reopen that file and immediately snapshot. Query bounds. Render width32 height32 bounds[0,0,12,12] to {run}/preview.png. Do each step in order. Do not substitute shell commands for MCP. Report observed results and limitations.'''
    approvals = ','.join(name + '={approval_mode="approve"}' for name in ALLOWED)
    server = ('{command=' + json.dumps(sys.executable) + ',args=' +
              json.dumps([str(Path(__file__).resolve()), '--proxy', str(run)]) +
              ',startup_timeout_sec=30,tool_timeout_sec=30,enabled_tools=' + json.dumps(ALLOWED) +
              ',tools={' + approvals + '}}')
    command = [str(CODEX), '--no-daemon', '-a', 'never', 'exec', '--ignore-user-config', '--ephemeral',
               '--skip-git-repo-check', '--json', '-s', 'read-only', '-C', str(run),
               '-m', 'gpt-6-luna', '-c', 'model_provider="openai"', '-c', 'model_reasoning_effort="max"',
               '-c', 'apps._default.enabled=false', '-c', 'mcp_servers.vsoul=' + server, '-o', str(run / 'final.txt'), prompt]
    metadata = {'command': command, 'core_sha256': hashlib.sha256(CORE.read_bytes()).hexdigest(),
                'codex_version': subprocess.check_output([str(CODEX), '--version'], text=True).strip()}
    (run / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    print(str(run), flush=True)
    with (run / 'codex.jsonl').open('w') as out, (run / 'codex.stderr').open('w') as err:
        child = subprocess.Popen(command, stdout=out, stderr=err)
        try:
            metadata['codex_pid'] = child.pid
            (run / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
            code = child.wait(timeout=600)
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            finish_children(run)
    assert code == 0, f'CLI exited {code}; inspect {run}/codex.stderr'
    verify(run, original, tail, digest)


if __name__ == '__main__':
    main()

