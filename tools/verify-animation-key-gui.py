#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import time
import traceback

ROOT = Path(__file__).resolve().parents[1]
SOCKET = Path.home() / "Library/Caches/cua-driver/cua-driver.sock"
PERMISSION_MARKERS = ("permission", "not authorized", "denied", "accessibility",
                      "screen recording", "tcc")

MODEL = {
    'nodes': {'uuid': 1, 'name': 'Root', 'type': 'Node',
              'transform': {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                            'scale': [1.0, 1.0]}},
    'param': [
        {'uuid': 17, 'name': 'Move', 'is_vec2': True, 'min': [-1.0, -1.0], 'max': [1.0, 1.0],
         'defaults': [0.125, 0.375], 'axis_points': [[0.0, 1.0], [0.0, 1.0]],
         'merge_mode': 'Additive'},
        {'uuid': 23, 'name': 'Blink', 'is_vec2': False, 'min': [-1.0, 0.0], 'max': [1.0, 1.0],
         'defaults': [0.0, 0.0], 'axis_points': [[0.0, 1.0], [0.0]], 'merge_mode': 'Additive'},
    ],
    'animations': {
        'Idle': {'length': 12, 'timestep': 0.1, 'additive': False, 'animationWeight': 1.0,
                 'leadIn': -1, 'leadOut': -1, 'lanes': []},
    },
}


def inx(model):
    payload = json.dumps(model, separators=(',', ':')).encode()
    return b'TRNSRTS\0' + struct.pack('>I', len(payload)) + payload + b'TEX_SECT' + struct.pack('>I', 0)


def png_info(path):
    raw = path.read_bytes()
    assert raw[:8] == b'\x89PNG\r\n\x1a\n', path
    assert raw[12:16] == b'IHDR', path
    width, height = struct.unpack('>II', raw[16:24])
    assert width > 0 and height > 0, path
    return {'bytes': len(raw), 'width': width, 'height': height,
            'sha256': hashlib.sha256(raw).hexdigest()}


def lanes(snapshot, animation, parameter):
    return [l for l in snapshot['model']['animations'][animation].get('lanes', [])
            if l['uuid'] == parameter]


def editor_env(run):
    """Environment for a launched editor: settings, autosave and recovery data stay inside the run directory."""
    config = Path(run) / 'config'
    config.mkdir(exist_ok=True)
    # Skip the first-run welcome window so it does not cover the panels under test.
    (config / 'settings.json').write_text(json.dumps({'firstrun_complete': True, 'hasDoneQuickSetup': True}))
    return {**os.environ, 'INOCHI_CONFIG_PATH': str(config)}


class Inconclusive(Exception):
    pass


def permission_failure(text):
    lowered = text.lower()
    return any(marker in lowered for marker in PERMISSION_MARKERS)


class Cua:
    def __init__(self, run):
        self.log = (run / 'driver-events.jsonl').open('w')

    def __call__(self, tool, payload, ok=True):
        proc = subprocess.run(['cua-driver', 'call', tool, json.dumps(payload)],
                              capture_output=True, text=True, timeout=45)
        self.log.write(json.dumps({'tool': tool, 'input': payload,
                                   'exitCode': proc.returncode, 'stdout': proc.stdout,
                                   'stderr': proc.stderr}) + '\n')
        self.log.flush()
        if proc.returncode != 0 and permission_failure(proc.stdout + proc.stderr):
            raise Inconclusive(f'{tool}: {proc.stderr or proc.stdout}')
        try:
            body = json.loads(proc.stdout) if proc.stdout.strip() else {}
        except json.JSONDecodeError:
            body = {'_stdout': proc.stdout}
        if ok:
            assert proc.returncode == 0, (tool, proc.stderr, proc.stdout)
            assert not body.get('isError', False), body
        else:
            body['_exitCode'] = proc.returncode
        return body

    def close(self):
        self.log.close()


def daemon_running():
    try:
        proc = subprocess.run(['cua-driver', 'status'], capture_output=True, text=True,
                              timeout=10)
    except subprocess.TimeoutExpired as error:
        raise Inconclusive(f'cua-driver status timed out: {error}') from error
    out = f'{proc.stdout}\n{proc.stderr}'.lower()
    if proc.returncode == 0 and 'daemon is running' in out:
        return True
    if proc.returncode == 1 and 'daemon is not running' in out:
        return False
    raise Inconclusive(
        f'cua-driver status unclear: rc={proc.returncode} '
        f'out={proc.stdout!r} err={proc.stderr!r}')


def stop_owned(proc):
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def ensure_daemon(result):
    if daemon_running():
        result['cuaDaemon'] = 'reused-existing'
        return None
    proc = subprocess.Popen(['cua-driver', 'serve', '--socket', str(SOCKET)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    result['cuaDaemon'] = f'owned-serve-pid-{proc.pid}'
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if daemon_running():
                return proc
            if proc.poll() is not None:
                raise Inconclusive(f'cua-driver serve exited {proc.returncode}')
            time.sleep(0.5)
        raise Inconclusive('cua-driver daemon did not start')
    except Exception:
        stop_owned(proc)
        result['cuaDaemon'] += '-stopped'
        raise


class Mcp:
    def __init__(self, proc, wire):
        self.proc = proc
        self.wire = wire
        self.seq = 0

    def rpc(self, method, params):
        self.seq += 1
        msg = {'jsonrpc': '2.0', 'id': self.seq, 'method': method, 'params': params}
        self.proc.stdin.write(json.dumps(msg) + '\n')
        self.proc.stdin.flush()
        self.wire.write(json.dumps({'request': msg}) + '\n')
        self.wire.flush()
        deadline = time.monotonic() + 30
        line = ''
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.proc.stdout], [], [], 0.5)
            if ready:
                line = self.proc.stdout.readline()
                break
        if not line:
            raise TimeoutError('editor MCP no response')
        resp = json.loads(line)
        self.wire.write(json.dumps({'response': resp}) + '\n')
        self.wire.flush()
        assert resp['id'] == self.seq and 'error' not in resp, resp
        return resp['result']

    def op(self, name, /, **args):
        out = self.rpc('tools/call', {'name': name, 'arguments': args})
        assert not out.get('isError', False), out
        return out.get('structuredContent') or json.loads(out['content'][0]['text'])

    def snapshot(self):
        return self.op('snapshot')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path,
                        default=ROOT / 'out/rust/debug/soul2d-editor')
    parser.add_argument('--evidence-root', type=Path, default=ROOT / 'out/verification')
    args = parser.parse_args()
    binary = args.binary.resolve()
    result = {'status': 'FAIL', 'checks': {}, 'binary': str(binary),
              'binarySha256': hashlib.sha256(binary.read_bytes()).hexdigest()}

    args.evidence_root.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='animation-key-gui-', dir=args.evidence_root))
    result['run'] = str(run)
    print(run, flush=True)
    fixture = run / 'fixture.inx'
    fixture.write_bytes(inx(MODEL))
    original = fixture.read_bytes()
    saved = run / 'gui-saved.inx'
    result['inputSha256'] = hashlib.sha256(original).hexdigest()
    (run / 'receipt.json').write_text(json.dumps(
        {'inputSha256': result['inputSha256'],
         'binarySha256': result['binarySha256'],
         'binary': str(binary)}, indent=2) + '\n')

    cua = Cua(run)
    wire = (run / 'mcp-wire.jsonl').open('w')
    owned_daemon = None
    proc = None
    session = None
    try:
        try:
            owned_daemon = ensure_daemon(result)
        except Inconclusive as error:
            result['status'] = 'INCONCLUSIVE'
            result['error'] = str(error)
            raise
        proc = subprocess.Popen([str(binary), '--mcp', '--open', str(fixture)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=editor_env(run),
                                stderr=(run / 'editor-stderr.log').open('w'), text=True)
        result['pid'] = proc.pid
        mcp = Mcp(proc, wire)
        session = f'key-edit-gui-{proc.pid}'
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
                               'clientInfo': {'name': 'gui-key-edit', 'version': '1'}})

        screenshots = {}

        def snapshot_state(name):
            body = {}
            for _ in range(6):
                shot = run / f'{name}.png'
                body = cua('get_window_state', {
                    'pid': proc.pid, 'window_id': window, 'session': session,
                    'timeout_ms': 8000, 'screenshot_out_file': str(shot)})
                if body.get('elements_complete') and any(
                        e.get('role') == 'AXStaticText' for e in body.get('elements', [])):
                    break
                time.sleep(0.7)
            assert body['pid'] == proc.pid and body['window_id'] == window
            assert body.get('screenshot_frame_valid') is True
            (run / f'{name}-ax.json').write_text(json.dumps(body, indent=2) + '\n')
            screenshots[name] = png_info(run / f'{name}.png')
            return body

        def items(state, role, text):
            return [e for e in state['elements'] if e.get('role') == role
                    and (e.get('label') or e.get('value')) == text]

        def enabled_buttons(state, label):
            return [e for e in state['elements'] if e.get('role') == 'AXButton'
                    and (e.get('label') or e.get('value')) == label
                    and e.get('enabled', True)]

        def click(state, label):
            found = items(state, 'AXButton', label)
            assert len(found) == 1, (label, [e.get('label') for e in state['elements']])
            cua('click', {'target': target,
                          'element_token': found[0]['element_token'], 'session': session})
            time.sleep(0.4)

        def seek_element(state):
            # The seek field is the incrementor that follows the "Seek frame" caption.
            elements = state['elements']
            caption = [i for i, e in enumerate(elements) if e.get('label') == 'Seek frame']
            assert len(caption) == 1, [e.get('label') for e in elements]
            found = [e for e in elements[caption[0]:] if e.get('role') == 'AXIncrementor']
            assert found, 'no seek incrementor'
            return found[0]

        def seek_value(state):
            raw = seek_element(state).get('value') or seek_element(state).get('label')
            return float(str(raw))

        def set_frame(state, frame, name):
            el = seek_element(state)
            cua('set_value', {'pid': proc.pid, 'element_token': el['element_token'],
                              'value': str(frame), 'session': session})
            time.sleep(0.4)
            state = snapshot_state(name)
            assert abs(seek_value(state) - frame) < 0.01, (frame, seek_value(state))
            return state

        def param_combo(state):
            found = [e for e in state['elements'] if e.get('role') == 'AXPopUpButton'
                     and '#' in str(e.get('label') or '')]
            assert len(found) == 1, found
            return found[0]

        state = snapshot_state('initial')
        assert not items(state, 'AXStaticText', 'node 1.transform must be an object')
        assert param_combo(state)['label'] == 'Move #17'
        assert enabled_buttons(state, 'Add key') and enabled_buttons(state, 'Copy key')
        assert not enabled_buttons(state, 'Paste key')
        result['checks']['controlsPresentPasteGated'] = True

        base = mcp.snapshot()
        assert base['animationClipboard'] == []
        revision = base['revision']
        assert not lanes(base, 'Idle', 17) and not lanes(base, 'Idle', 23)

        for bad, name in ((2.5, 'frame-2-5'), (-1, 'frame-neg1'), (12, 'frame-12')):
            state = set_frame(state, bad, name)
            assert items(state, 'AXStaticText',
                         'Choose an integer frame within the animation.'), name
            for label in ('Add key', 'Copy key', 'Paste key'):
                assert not enabled_buttons(state, label), (label, name)
            assert mcp.snapshot() == base, name
        result['checks']['invalidFramesDisabledUnchanged'] = [2.5, -1, 12]

        state = set_frame(state, 2, 'frame-2')
        assert enabled_buttons(state, 'Add key')
        click(state, 'Add key')
        snap = mcp.snapshot()
        assert snap['revision'] == revision + 1, snap['revision']
        added = {lane['target']: lane for lane in lanes(snap, 'Idle', 17)}
        assert added[0]['keyframes'] == [{'frame': 2, 'value': 0.125, 'tension': 0.5}], added[0]
        assert added[1]['keyframes'] == [{'frame': 2, 'value': 0.375, 'tension': 0.5}], added[1]
        state = snapshot_state('added')
        result['checks']['nativeAddBothAxesEffectiveValues'] = True

        click(state, 'Copy key')
        snap = mcp.snapshot()
        assert snap['animationClipboard'] == [{'axis': 0, 'value': 0.125},
                                              {'axis': 1, 'value': 0.375}]
        state = snapshot_state('copied')
        assert enabled_buttons(state, 'Paste key')
        result['checks']['nativeCopyPasteEnabled'] = True

        combo = param_combo(state)
        body = cua('set_value', {'pid': proc.pid, 'element_token': combo['element_token'],
                                 'value': 'Blink #23', 'session': session}, ok=False)
        if body.get('_exitCode') or body.get('isError'):
            popup = items(state, 'AXPopUpButton', 'Move #17')
            assert len(popup) == 1
            cua('click', {'target': target, 'element_token': popup[0]['element_token'],
                          'session': session})
            time.sleep(0.5)
            state = snapshot_state('combo-open')
            blink = [e for e in state['elements']
                     if (e.get('label') or e.get('value')) == 'Blink #23'
                     and e.get('role') != 'AXPopUpButton']
            assert blink, 'Blink option not found'
            cua('click', {'target': target, 'element_token': blink[0]['element_token'],
                          'session': session})
            time.sleep(0.4)
        state = snapshot_state('blink-selected')
        assert param_combo(state)['label'] == 'Blink #23'
        open_options = [e for e in state['elements']
                        if e.get('role') != 'AXPopUpButton'
                        and (e.get('label') or e.get('value')) in ('Move #17', 'Blink #23')]
        assert not open_options, open_options
        assert enabled_buttons(state, 'Paste key')
        result['checks']['comboSelectScalar'] = True
        result['checks']['popupClosedBeforePaste'] = True

        state = set_frame(state, 4, 'frame-4')
        click(state, 'Paste key')
        snap = mcp.snapshot()
        scalar = lanes(snap, 'Idle', 23)
        assert len(scalar) == 1 and scalar[0]['target'] == 0, scalar
        assert scalar[0]['keyframes'] == [{'frame': 4, 'value': 0.125, 'tension': 0.5}], scalar
        state = snapshot_state('pasted')
        result['checks']['nativePasteScalarAxis0Only'] = True

        click(state, 'Undo')
        snap = mcp.snapshot()
        assert lanes(snap, 'Idle', 23) == []
        state = snapshot_state('undone')
        click(state, 'Redo')
        snap = mcp.snapshot()
        assert len(lanes(snap, 'Idle', 23)) == 1
        result['checks']['singleUndoRedo'] = True

        mcp.op('save', path=str(saved))
        raw = saved.read_bytes()
        assert raw[:8] == b'TRNSRTS\0'
        length = int.from_bytes(raw[8:12], 'big')
        model = json.loads(raw[12:12 + length])
        saved17 = {lane['target']: lane for lane in model['animations']['Idle']['lanes']
                   if lane['uuid'] == 17}
        assert len(saved17) == 2
        assert saved17[0]['keyframes'] == [{'frame': 2, 'value': 0.125, 'tension': 0.5}]
        assert saved17[1]['keyframes'] == [{'frame': 2, 'value': 0.375, 'tension': 0.5}]
        saved23 = [l for l in model['animations']['Idle']['lanes'] if l['uuid'] == 23]
        assert len(saved23) == 1 and saved23[0]['target'] == 0
        assert saved23[0]['keyframes'] == [{'frame': 4, 'value': 0.125, 'tension': 0.5}]
        result['checks']['savedSerializedKeys'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        result['screenshots'] = screenshots
        result['savedSha256'] = hashlib.sha256(raw).hexdigest()
        result['status'] = 'PASS_INX_NATIVE_KEY_EDIT_WORKFLOW_ONLY'
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
        (run / 'gui-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    return 0 if result['status'].startswith('PASS_') else 1


if __name__ == '__main__':
    sys.exit(main())
