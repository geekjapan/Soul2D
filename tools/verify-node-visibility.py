#!/usr/bin/env python3
import argparse
import base64
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import traceback

ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / 'tools' / 'verify-rust-core.py'
spec = importlib.util.spec_from_file_location('rust_core_harness', HARNESS)
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)

Core = harness.Core
decode_rgba = harness.decode_rgba
fixture = harness.fixture
split_container = harness.split_container


def source_hashes():
    paths = sorted(ROOT.glob('rust/src/**/*.rs'))
    paths += [ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
              Path(__file__).resolve(), HARNESS]
    return {str(p.relative_to(ROOT)):
            hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}


def find_child(node, uuid):
    if node.get('uuid') == uuid:
        return node
    for child in node.get('children', []):
        found = find_child(child, uuid)
        if found is not None:
            return found
    return None


class LoggedCore(Core):
    def __init__(self, library, wire):
        super().__init__(library)
        self.wire = wire

    def request(self, op, success=True, **args):
        payload = {'op': op, **args}
        raw = self.lib.vsoul_session_request(self.handle, json.dumps(payload).encode())
        assert raw, op
        try:
            result = json.loads(ctypes.string_at(raw))
        finally:
            self.lib.vsoul_string_free(raw)
        self.wire.write(json.dumps({'request': payload, 'response': result}) + '\n')
        self.wire.flush()
        assert result['succeeded'] == success, (op, result)
        return result.get('result')


def save_render(pixels_path, image):
    data = base64.b64decode(image['png'])
    assert data[:8] == b'\x89PNG\r\n\x1a\n', 'render must be a PNG'
    width, height, pixels = decode_rgba(data)
    assert (width, height) == (8, 8), (width, height)
    pixels_path.write_bytes(data)
    return pixels


def noop_key_cases(session):
    model = {'meta': {'name': 'noop', 'version': '1'},
             'nodes': {'uuid': 1, 'name': 'Root', 'type': 'Node', 'enabled': True,
                       'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                       'children': [
                           {'uuid': 2, 'name': 'Absent', 'type': 'Node', 'children': []},
                           {'uuid': 3, 'name': 'Malformed', 'type': 'Node', 'enabled': 'yes',
                            'children': []}]},
             'param': [], 'animations': {}}
    session.request('new', model=model)
    before = session.request('snapshot')
    assert session.request('node.enabled.set', id=2, enabled=True) == {'uuid': 2, 'enabled': True}
    assert session.request('snapshot') == before, 'absent-key same-value Set must be a no-op'
    assert 'enabled' not in find_child(session.request('snapshot')['model']['nodes'], 2)
    assert session.request('node.enabled.set', id=3, enabled=True) == {'uuid': 3, 'enabled': True}
    assert session.request('snapshot') == before, 'malformed-key same-value Set must be a no-op'
    assert find_child(session.request('snapshot')['model']['nodes'], 3)['enabled'] == 'yes'


def verify_ffi(core, other, directory, original, tail):
    noop_key_cases(other)

    core.request('open', path=str(original))
    pixels = save_render(directory / 'render-before.png', core.request(
        'render', width=8, height=8, bounds=[0, 0, 8, 8]))
    assert pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([255, 0, 0, 255])
    assert pixels[:4] == bytes([0, 0, 0, 0])

    wrapper = core.request('node.insert', nodes=[2], className='MeshGroup')
    gid = wrapper['created'][0]['uuid']

    baseline = core.request('snapshot')
    history_count = baseline['history']['count']
    revision = baseline['revision']
    reply = core.request('node.enabled.set', id=gid, enabled=False)
    assert reply == {'uuid': gid, 'enabled': False}, reply
    after = core.request('snapshot')
    assert after['revision'] == revision + 1 and after['history']['count'] == history_count + 1
    assert find_child(after['model']['nodes'], 2)['enabled'] is True
    pixels = save_render(directory / 'render-hidden.png', core.request(
        'render', width=8, height=8, bounds=[0, 0, 8, 8]))
    assert pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([0, 0, 0, 0]), \
        'hidden parent must suppress child pixels'

    unchanged = core.request('snapshot')
    assert core.request('node.enabled.set', id=gid, enabled=False) == {'uuid': gid, 'enabled': False}
    assert core.request('snapshot') == unchanged, 'same-value Set must be a complete no-op'

    assert core.request('node.enabled.toggle', id=gid) == {'uuid': gid, 'enabled': True}
    pixels = save_render(directory / 'render-restored.png', core.request(
        'render', width=8, height=8, bounds=[0, 0, 8, 8]))
    assert pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([255, 0, 0, 255])

    core.request('node.enabled.set', id=gid, enabled=False)
    core.request('node.enabled.set', id=2, enabled=False)
    core.request('node.enabled.set', id=gid, enabled=True)
    pixels = save_render(directory / 'render-child-hidden.png', core.request(
        'render', width=8, height=8, bounds=[0, 0, 8, 8]))
    assert pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([0, 0, 0, 0]), \
        'locally disabled child stays hidden under shown parent'
    core.request('node.enabled.toggle', id=2)
    pixels = save_render(directory / 'render-child-restored.png', core.request(
        'render', width=8, height=8, bounds=[0, 0, 8, 8]))
    assert pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([255, 0, 0, 255])

    for bad in [{'id': gid}, {'id': 'x', 'enabled': False}, {'id': 0, 'enabled': False},
                {'id': gid, 'enabled': 'yes'}, {'id': gid, 'enabled': 1}]:
        before = core.request('snapshot')
        core.request('node.enabled.set', False, **bad)
        assert core.request('snapshot') == before
    before = core.request('snapshot')
    core.request('node.enabled.toggle', False, id=gid, enabled=False)
    assert core.request('snapshot') == before

    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], 2)['enabled'] is False
    core.request('redo')
    assert find_child(core.request('snapshot')['model']['nodes'], 2)['enabled'] is True

    saved = directory / 'visibility.inx'
    core.request('save', path=str(saved))
    model, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail, 'opaque tail must round-trip'
    other.request('open', path=str(saved))
    assert other.request('snapshot')['model'] == model
    assert original.read_bytes().startswith(b'TRNSRTS\0')
    print('PASS FFI visibility: no-op keys, literal pixels, inheritance, errors, history, save+tail')


def verify_cli(binary, original, directory):
    requests = [{'op': 'open', 'path': str(original)},
                {'op': 'node.enabled.set', 'id': 2, 'enabled': False},
                {'op': 'render', 'width': 8, 'height': 8, 'bounds': [0, 0, 8, 8]},
                {'op': 'undo'},
                {'op': 'render', 'width': 8, 'height': 8, 'bounds': [0, 0, 8, 8]},
                {'op': 'snapshot'}]
    stdin_payload = ''.join(json.dumps(v) + '\n' for v in requests)
    (directory / 'cli-wire-in.jsonl').write_text(stdin_payload)
    run = subprocess.run([str(binary)], input=stdin_payload,
                         text=True, capture_output=True, timeout=30)
    (directory / 'cli-wire-out.jsonl').write_text(run.stdout)
    (directory / 'cli-stderr.log').write_text(run.stderr)
    (directory / 'cli-process.json').write_text(json.dumps(
        {'command': [str(binary)], 'returncode': run.returncode}, indent=2) + '\n')
    responses = [json.loads(line) for line in run.stdout.splitlines()]
    assert run.returncode == 0 and len(responses) == len(requests), run.stderr
    assert all(v['succeeded'] for v in responses), responses
    assert responses[1]['result'] == {'uuid': 2, 'enabled': False}
    hidden = save_render(directory / 'cli-hidden.png', responses[2]['result'])
    assert hidden[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([0, 0, 0, 0])
    shown = save_render(directory / 'cli-shown.png', responses[4]['result'])
    assert shown[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4] == bytes([255, 0, 0, 255])
    assert responses[5]['result']['model']['nodes']['children'][0]['enabled'] is True
    print('PASS CLI visibility: JSONL set/render/undo/pixels')


def verify_mcp(binary, original, directory):
    process = subprocess.Popen([str(binary), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
    timeout = threading.Timer(30, process.kill)
    timeout.start()
    wire = (directory / 'mcp-wire.jsonl').open('w')
    uid = 0
    stderr_bytes = ''

    def call(method, params=None):
        nonlocal uid
        uid += 1
        request = {'jsonrpc': '2.0', 'id': uid, 'method': method, 'params': params or {}}
        process.stdin.write(json.dumps(request) + '\n')
        process.stdin.flush()
        response = json.loads(process.stdout.readline())
        wire.write(json.dumps({'request': request, 'response': response}) + '\n')
        wire.flush()
        assert response['id'] == uid and 'result' in response, response
        return response['result']

    def tool(name, arguments=None):
        response = call('tools/call', {'name': name, 'arguments': arguments or {}})
        if response.get('isError'):
            return response, None
        return response, (response.get('structuredContent')
                          or json.loads(response['content'][0]['text']))

    try:
        call('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                            'clientInfo': {'name': 'verify-visibility', 'version': '1'}})
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
        process.stdin.flush()
        tools = {t['name']: t for t in call('tools/list')['tools']}
        assert {'node_enabled_set', 'node_enabled_toggle', 'NodeCommand_ToggleVisibility'} <= set(tools)
        set_schema = tools['node_enabled_set']['inputSchema']
        assert set_schema['required'] == ['id', 'enabled']
        assert set_schema['properties']['id']['type'] == 'integer'
        assert set_schema['properties']['enabled']['type'] == 'boolean'
        toggle_schema = tools['node_enabled_toggle']['inputSchema']
        assert toggle_schema['required'] == ['id']
        assert toggle_schema['properties']['id']['type'] == 'integer'
        assert 'enabled' not in toggle_schema['properties']
        legacy_schema = tools['NodeCommand_ToggleVisibility']['inputSchema']
        assert legacy_schema['required'] == ['context']
        context_schema = legacy_schema['properties']['context']
        assert context_schema['required'] == ['nodes']
        assert context_schema['properties']['nodes']['type'] == 'array'
        assert context_schema['properties']['nodes']['minItems'] == 1
        assert context_schema['properties']['nodes']['items']['type'] == 'integer'

        response, _ = tool('open', {'path': str(original)})
        assert not response.get('isError'), response
        response, wrap = tool('Node_Insert_MeshGroup', {'_suffix': '', 'context': {'nodes': [2]}})
        assert not response.get('isError'), response
        gid = wrap['created'][0]['uuid']

        response, body = tool('node_enabled_set', {'id': gid, 'enabled': False})
        assert not response.get('isError'), response
        assert body == {'uuid': gid, 'enabled': False}
        response, body = tool('node_enabled_toggle', {'id': gid})
        assert not response.get('isError'), response
        assert body == {'uuid': gid, 'enabled': True}

        response, _ = tool('NodeCommand_ToggleVisibility', {'context': {'nodes': [gid, 2]}})
        assert not response.get('isError'), response
        response, snap = tool('nodes_list')
        nodes = {entry['uuid']: entry for entry in snap}
        assert nodes[gid]['enabled'] is False and nodes[2]['enabled'] is True, nodes

        for name, arguments in [
            ('NodeCommand_ToggleVisibility', {'context': {'nodes': ['bad', 2]}}),
            ('NodeCommand_ToggleVisibility', {'context': {'nodes': []}}),
            ('NodeCommand_ToggleVisibility', {'context': {'nodes': 'x'}}),
            ('NodeCommand_ToggleVisibility', {'context': {}}),
            ('NodeCommand_ToggleVisibility', {}),
            ('NodeCommand_ToggleVisibility', {'context': {'nodes': [gid]}, 'id': 2}),
            ('node_enabled_toggle', {'id': gid, 'enabled': True}),
            ('node_enabled_set', {'id': gid}),
            ('node_enabled_set', {'id': gid, 'enabled': 'yes'}),
            ('node_enabled_toggle', {'id': 'bad'}),
        ]:
            response, _ = tool(name, arguments)
            assert response.get('isError'), (name, arguments, response)
        response, snap = tool('nodes_list')
        nodes = {entry['uuid']: entry for entry in snap}
        assert nodes[gid]['enabled'] is False and nodes[2]['enabled'] is True
        print('PASS MCP visibility: stdio tools, first-context alias, strict rejects')
    finally:
        timeout.cancel()
        process.kill()
        process.wait()
        stderr_bytes = process.stderr.read()
        wire.close()
        (directory / 'mcp-stderr.log').write_text(stderr_bytes)
        (directory / 'mcp-process.json').write_text(json.dumps(
            {'command': [str(binary), '--mcp'], 'returncode': process.returncode},
            indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--target', type=Path, default=ROOT / 'out/rust/debug')
    parser.add_argument('--evidence-root', type=Path, default=ROOT / 'out/rust-verification')
    args = parser.parse_args()
    result = {'status': 'FAIL', 'checks': {},
              'sourceSha256': source_hashes()}
    suffix = '.dylib' if sys.platform == 'darwin' else '.dll' if os.name == 'nt' else '.so'
    library = (args.target / (('' if os.name == 'nt' else 'lib') + 'soul2d_core' + suffix)).resolve()
    binary = (args.target / ('soul2d-core.exe' if os.name == 'nt' else 'soul2d-core')).resolve()
    result['binarySha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
    result['librarySha256'] = hashlib.sha256(library.read_bytes()).hexdigest()

    args.evidence_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='node-visibility-', dir=args.evidence_root))
    result['run'] = str(directory)
    print(directory, flush=True)
    original = directory / 'fixture.inx'
    _, tail = fixture(original)
    result['inputSha256'] = hashlib.sha256(original.read_bytes()).hexdigest()
    (directory / 'receipt.json').write_text(json.dumps(
        {'binarySha256': result['binarySha256'],
         'librarySha256': result['librarySha256'],
         'sourceSha256': result['sourceSha256'],
         'inputSha256': result['inputSha256'],
         'binary': str(binary), 'library': str(library)}, indent=2) + '\n')
    before_bytes = original.read_bytes()
    try:
        wire = (directory / 'ffi-wire.jsonl').open('w')
        core = LoggedCore(library, wire)
        other = LoggedCore(library, wire)
        try:
            verify_ffi(core, other, directory, original, tail)
            result['checks']['ffi'] = True
        finally:
            core.close()
            other.close()
            wire.close()
        verify_cli(binary, original, directory)
        result['checks']['cli'] = True
        verify_mcp(binary, original, directory)
        result['checks']['mcp'] = True
        assert original.read_bytes() == before_bytes, 'fixture bytes changed'
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        assert hashlib.sha256(library.read_bytes()).hexdigest() == result['librarySha256']
        assert source_hashes() == result['sourceSha256']
        result['status'] = 'PASS_INX_NODE_VISIBILITY_ONLY'
    except Exception:
        result['status'] = 'FAIL'
        result['error'] = traceback.format_exc()
    finally:
        (directory / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)
    return 0 if result['status'].startswith('PASS_') else 1


if __name__ == '__main__':
    sys.exit(main())
