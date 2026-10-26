#!/usr/bin/env python3
import argparse
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import traceback

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / 'tools' / 'verify-animation-key-gui.py'
spec = importlib.util.spec_from_file_location('animation_key_gui_harness', HELPER)
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)

CORE_HELPER = ROOT / 'tools' / 'verify-rust-core.py'
core_spec = importlib.util.spec_from_file_location('rust_core_harness', CORE_HELPER)
core_harness = importlib.util.module_from_spec(core_spec)
core_spec.loader.exec_module(core_harness)

Inconclusive = harness.Inconclusive
Cua = harness.Cua
Mcp = harness.Mcp
ensure_daemon = harness.ensure_daemon
inx = harness.inx
png_info = harness.png_info
stop_owned = harness.stop_owned
decode_rgba = core_harness.decode_rgba
png = core_harness.png


def source_hashes():
    paths = sorted(ROOT.glob('rust/src/**/*.rs'))
    paths += [ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
              Path(__file__).resolve(), HELPER, CORE_HELPER]
    return {str(p.relative_to(ROOT)):
            hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}


def transform():
    return {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0], 'scale': [1.0, 1.0]}


def fixture_bytes():
    texture = png(2, 2, bytes([255, 0, 0, 255]) * 4)
    card = {
        'uuid': 5, 'name': 'Card', 'type': 'Part', 'enabled': True, 'zsort': 0.0,
        'transform': transform(), 'children': [], 'lockToRoot': False, 'pinToMesh': False,
        'mesh': {'verts': [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0],
                 'uvs': [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 'indices': [0, 1, 2, 0, 2, 3], 'origin': [0.0, 0.0]},
        'vertices': [[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]],
        'uvs': [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        'indices': [0, 1, 2, 0, 2, 3],
        'textures': [0], 'blend_mode': 'Normal', 'tint': [1, 1, 1], 'screenTint': [0, 0, 0],
        'emissionStrength': 1, 'opacity': 1,
    }
    model = {
        'meta': {'name': 'fixture', 'version': '1.0-alpha'},
        'nodes': {'uuid': 1, 'name': 'Fixture', 'type': 'Node', 'enabled': True, 'zsort': 0.0,
                  'transform': transform(), 'lockToRoot': False, 'pinToMesh': False,
                  'children': [
                      {'uuid': 4, 'name': 'Wrap', 'type': 'Node', 'enabled': True, 'zsort': 0.0,
                       'transform': transform(), 'lockToRoot': False, 'pinToMesh': False,
                       'children': [card]},
                  ]},
        'param': [], 'animations': {},
    }
    raw = json.dumps(model, separators=(',', ':')).encode()
    return (b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw
            + b'TEX_SECT' + struct.pack('>I', 1) + struct.pack('>IB', len(texture), 0) + texture
            + b'EXT_SECT' + struct.pack('>I', 0))


def read_inx(path):
    data = path.read_bytes()
    assert data[:8] == b'TRNSRTS\0', 'saved file missing TRNSRTS header'
    (length,) = struct.unpack('>I', data[8:12])
    payload = data[12:12 + length]
    assert data[12 + length:12 + length + 8] == b'TEX_SECT', 'missing TEX_SECT'
    return json.loads(payload)


def find_node(node, uuid):
    if node.get('uuid') == uuid:
        return node
    for child in node.get('children', []):
        found = find_node(child, uuid)
        if found is not None:
            return found
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path,
                        default=ROOT / 'out/rust/debug/soul2d-editor')
    parser.add_argument('--evidence-root', type=Path, default=ROOT / 'out/verification')
    args = parser.parse_args()
    binary = args.binary.resolve()
    result = {'status': 'FAIL', 'checks': {}, 'binary': str(binary),
              'binarySha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'sourceSha256': source_hashes()}

    args.evidence_root.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='node-visibility-gui-',
                                dir=args.evidence_root))
    result['run'] = str(run)
    print(run, flush=True)
    fixture = run / 'fixture.inx'
    fixture.write_bytes(fixture_bytes())
    original = fixture.read_bytes()
    saved = run / 'gui-saved.inx'
    result['inputSha256'] = hashlib.sha256(original).hexdigest()
    (run / 'receipt.json').write_text(json.dumps(
        {'inputSha256': result['inputSha256'],
         'binarySha256': result['binarySha256'],
         'sourceSha256': result['sourceSha256'],
         'binary': str(binary)}, indent=2) + '\n')

    cua = Cua(run)
    wire = (run / 'mcp-wire.jsonl').open('w')
    owned_daemon = None
    proc = None
    session = None
    screenshots = {}
    try:
        try:
            owned_daemon = ensure_daemon(result)
        except Inconclusive as error:
            result['status'] = 'INCONCLUSIVE'
            result['error'] = str(error)
            raise
        proc = subprocess.Popen([str(binary), '--mcp', '--open', str(fixture)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                env=harness.editor_env(run),
                                stderr=(run / 'editor-stderr.log').open('w'),
                                text=True)
        result['pid'] = proc.pid
        mcp = Mcp(proc, wire)
        session = f'node-visibility-gui-{proc.pid}'
        cua('start_session', {'session': session})

        deadline = time.monotonic() + 30
        window = None
        while time.monotonic() < deadline:
            body = cua('list_windows', {'pid': proc.pid})
            for w in body.get('windows', []):
                if w.get('pid') == proc.pid and (w.get('title') or '').startswith('Soul2D'):
                    window = w['window_id']
                    break
            if window:
                break
            time.sleep(0.5)
        assert window, 'no editor window'
        result['windowId'] = window
        target = {'kind': 'window', 'pid': proc.pid, 'window_id': window}
        mcp.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                               'clientInfo': {'name': 'gui-node-visibility', 'version': '1'}})

        def snapshot_state(name):
            body = {}
            tries = 25 if name == 'initial' else 6
            for _ in range(tries):
                shot = run / f'{name}.png'
                body = cua('get_window_state', {
                    'pid': proc.pid, 'window_id': window, 'session': session,
                    'timeout_ms': 8000, 'screenshot_out_file': str(shot)})
                if body.get('elements_complete') and any(
                        e.get('role') == 'AXStaticText'
                        for e in body.get('elements', [])):
                    break
                time.sleep(0.7)
            assert body['pid'] == proc.pid and body['window_id'] == window
            assert body.get('screenshot_frame_valid') is True
            (run / f'{name}-ax.json').write_text(json.dumps(body, indent=2) + '\n')
            screenshots[name] = png_info(run / f'{name}.png')
            return body

        def labelled(state, text):
            return [e for e in state['elements']
                    if (e.get('label') or e.get('value')) == text]

        def click_label(state, text):
            found = labelled(state, text)
            assert len(found) == 1, (text, [e.get('label')
                                            for e in state['elements']])
            cua('click', {'target': target,
                          'element_token': found[0]['element_token'],
                          'session': session})
            time.sleep(0.4)

        def click_after(state, anchor, text):
            elements = state['elements']
            anchor_index = next(i for i, e in enumerate(elements)
                                if (e.get('label') or e.get('value')) == anchor)
            found = [e for e in elements[anchor_index + 1:]
                     if (e.get('label') or e.get('value')) == text]
            assert found, (anchor, text)
            cua('click', {'target': target,
                          'element_token': found[0]['element_token'],
                          'session': session})
            time.sleep(0.4)

        def enabled_buttons(state, label):
            return [e for e in state['elements'] if e.get('role') == 'AXButton'
                    and (e.get('label') or e.get('value')) == label
                    and e.get('enabled', True)]

        def visibility_button(state):
            return [e for e in state['elements'] if e.get('role') == 'AXButton'
                    and (e.get('label') or e.get('value'))
                    in ('Hide node', 'Show node', 'Toggle visibility')]

        render_index = [0]

        def render_center():
            render_index[0] += 1
            out = mcp.rpc('tools/call', {'name': 'render', 'arguments': {
                'width': 8, 'height': 8, 'bounds': [0, 0, 8, 8]}})
            assert not out.get('isError', False), out
            data = base64.b64decode(out['content'][0]['data'])
            assert data[:8] == b'\x89PNG\r\n\x1a\n'
            width, height, pixels = decode_rgba(data)
            assert (width, height) == (8, 8), (width, height)
            (run / f'render-{render_index[0]:02d}.png').write_bytes(data)
            return pixels[(4 * 8 + 4) * 4:(4 * 8 + 4) * 4 + 4]

        state = snapshot_state('initial')
        assert labelled(state, 'Wrap #4') and labelled(state, 'Card #5')
        assert enabled_buttons(state, 'Copy') == []
        assert enabled_buttons(state, 'Delete') == []
        assert visibility_button(state) == [], (
            'disabled widgets are omitted from the AX tree; an enabled '
            'visibility button must not exist without a selection')
        result['checks']['unselectedDisablesToggleVisibility'] = True

        click_label(state, 'Card #5')
        state = snapshot_state('card-selected')
        button = visibility_button(state)
        assert len(button) == 1 and button[0].get('enabled', True)
        assert (button[0].get('label') or button[0].get('value')) == 'Hide node'
        assert render_center() == bytes([255, 0, 0, 255])
        result['checks']['selectedShowsHideNodeAndRedPixel'] = True

        click_label(state, 'Hide node')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 5)['enabled'] is False
        assert render_center() == bytes([0, 0, 0, 0])
        state = snapshot_state('card-hidden')
        button = visibility_button(state)
        assert (button[0].get('label') or button[0].get('value')) == 'Show node'
        result['checks']['nativeHideChildSuppressesPixels'] = True

        click_label(state, 'Show node')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 5)['enabled'] is True
        assert render_center() == bytes([255, 0, 0, 255])
        state = snapshot_state('card-shown')
        result['checks']['nativeShowChildRestoresPixels'] = True

        click_label(state, 'Wrap #4')
        state = snapshot_state('wrap-selected')
        click_label(state, 'Hide node')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 4)['enabled'] is False
        assert find_node(snap['model']['nodes'], 5)['enabled'] is True
        assert render_center() == bytes([0, 0, 0, 0])
        state = snapshot_state('wrap-hidden')
        result['checks']['hiddenParentSuppressesChildWithoutFlagChange'] = True

        click_label(state, 'Undo')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 4)['enabled'] is True
        state = snapshot_state('undo-hide')
        button = visibility_button(state)
        assert (button[0].get('label') or button[0].get('value')) == 'Hide node'
        click_label(state, 'Redo')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 4)['enabled'] is False
        state = snapshot_state('redo-hide')
        button = visibility_button(state)
        assert (button[0].get('label') or button[0].get('value')) == 'Show node'
        result['checks']['undoRedoRestoresFlagAndSelection'] = True

        click_label(state, 'Show node')
        assert render_center() == bytes([255, 0, 0, 255])
        state = snapshot_state('wrap-shown')

        click_label(state, 'Fixture #1')
        state = snapshot_state('root-selected')
        button = visibility_button(state)
        assert len(button) == 1 and button[0].get('enabled', True)
        assert (button[0].get('label') or button[0].get('value')) == 'Hide node'
        click_label(state, 'Hide node')
        snap = mcp.snapshot()
        assert snap['model']['nodes']['enabled'] is False
        assert render_center() == bytes([0, 0, 0, 0])
        state = snapshot_state('root-hidden')
        button = visibility_button(state)
        assert len(button) == 1 and button[0].get('enabled', True), button
        assert (button[0].get('label') or button[0].get('value')) == 'Show node'
        click_label(state, 'Show node')
        snap = mcp.snapshot()
        assert snap['model']['nodes']['enabled'] is True
        assert render_center() == bytes([255, 0, 0, 255])
        result['checks']['explicitRootHideShowKeepsControls'] = True

        state = snapshot_state('root-shown')
        click_label(state, 'Card #5')
        state = snapshot_state('card-for-draft')
        click_label(state, 'Edit Card')
        state = snapshot_state('draft-open')
        fields = [e for e in state['elements'] if e.get('role') == 'AXTextField'
                  and e.get('value') == 'Card']
        assert len(fields) == 1, [(e.get('label'), e.get('value'))
                                  for e in state['elements'] if e.get('role') == 'AXTextField']
        cua('click', {'target': target,
                      'element_token': fields[0]['element_token'],
                      'session': session})
        time.sleep(0.3)
        state = snapshot_state('draft-focused')
        fields = [e for e in state['elements'] if e.get('role') == 'AXTextField'
                  and e.get('value') == 'Card']
        assert len(fields) == 1
        cua('type_text', {'pid': proc.pid,
                          'element_token': fields[0]['element_token'],
                          'text': 'Draft', 'session': session})
        time.sleep(0.4)
        state = snapshot_state('draft-dirty')
        assert visibility_button(state) == [], (
            'dirty draft must leave the visibility control disabled and '
            'therefore absent from the AX tree')
        assert enabled_buttons(state, 'Copy'), 'Copy must stay enabled under a dirty draft'
        result['checks']['dirtyDraftDisablesToggleKeepsCopy'] = True

        click_label(state, 'Discard')
        state = snapshot_state('draft-discarded')
        button = visibility_button(state)
        assert len(button) == 1 and button[0].get('enabled', True), button

        click_label(state, 'Hide node')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 5)['enabled'] is False
        mcp.op('save', path=str(saved))
        saved_model = read_inx(saved)
        assert find_node(saved_model['nodes'], 5)['enabled'] is False
        assert find_node(saved_model['nodes'], 4)['enabled'] is True
        reopened = mcp.op('open', path=str(saved))
        assert find_node(reopened['model']['nodes'], 5)['enabled'] is False
        assert render_center() == bytes([0, 0, 0, 0])
        state = snapshot_state('reopened')
        assert visibility_button(state) == [], 'reopen clears selection'
        click_label(state, 'Card #5')
        state = snapshot_state('reopened-card-selected')
        button = visibility_button(state)
        assert len(button) == 1
        assert (button[0].get('label') or button[0].get('value')) == 'Show node'
        click_label(state, 'Show node')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], 5)['enabled'] is True
        assert render_center() == bytes([255, 0, 0, 255])
        result['checks']['savedAndReopenedFlagsPersist'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        assert source_hashes() == result['sourceSha256']
        result['screenshots'] = screenshots
        result['status'] = 'PASS_INX_NATIVE_NODE_VISIBILITY_ONLY'
    except Inconclusive as error:
        result['status'] = 'INCONCLUSIVE'
        result['error'] = str(error)
    except Exception:
        result['status'] = 'FAIL'
        result['error'] = traceback.format_exc()
    finally:
        if session:
            try:
                cua('end_session', {'session': session}, ok=False)
            except Exception:
                pass
        if proc is not None:
            stop_owned(proc)
            result['editorExitCode'] = proc.returncode
        if owned_daemon is not None:
            stop_owned(owned_daemon)
        cua.close()
        wire.close()
        result['screenshots'] = result.get('screenshots') or screenshots
        (run / 'gui-result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)
    return 0 if result['status'].startswith('PASS_') else 1


if __name__ == '__main__':
    sys.exit(main())
