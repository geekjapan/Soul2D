#!/usr/bin/env python3
import argparse
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

Inconclusive = harness.Inconclusive
Cua = harness.Cua
Mcp = harness.Mcp
ensure_daemon = harness.ensure_daemon
inx = harness.inx
png_info = harness.png_info
stop_owned = harness.stop_owned

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
    run = Path(tempfile.mkdtemp(prefix='animation-list-gui-',
                                dir=args.evidence_root))
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
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                env=harness.editor_env(run),
                                stderr=(run / 'editor-stderr.log').open('w'),
                                text=True)
        result['pid'] = proc.pid
        mcp = Mcp(proc, wire)
        session = f'animation-list-gui-{proc.pid}'
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
                               'clientInfo': {'name': 'gui-animation-list', 'version': '1'}})

        screenshots = {}

        def snapshot_state(name):
            body = {}
            for _ in range(6):
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

        def type_after(state, label, text):
            elements = state['elements']
            at = next(i for i, e in enumerate(elements) if e.get('value') == label)
            field = next(e for e in elements[at:] if e.get('role') == 'AXTextField')
            cua('type_text', {'target': target, 'element_token': field['element_token'],
                              'text': text, 'delivery_mode': 'foreground', 'session': session})
            time.sleep(0.4)

        def animations():
            return sorted(mcp.snapshot()['model']['animations'])

        # Same bootstrap as the history verifier: a session edit makes the editor draw.
        for name in ('First', 'Second'):
            out = mcp.rpc('tools/call', {'name': 'node_rename', 'arguments': {'id': 9, 'name': name}})
            assert not out.get('isError', False), out
        base = mcp.snapshot()
        assert animations() == []
        for _ in range(8):
            state = snapshot_state('bootstrapped')
            if state['elements']:
                break
            time.sleep(1)
        assert len(labelled(state, 'No authored INX animations.')) == 1
        type_after(state, 'Animation name', 'Wave')
        click_label(snapshot_state('name-typed'), 'Create animation')
        assert animations() == ['Wave']
        created = mcp.snapshot()
        assert created['revision'] == base['revision'] + 1
        assert created['history']['states'][-1]['label'] == 'animation.create'
        result['checks']['nativeCreate'] = True

        state = snapshot_state('created')
        assert len(labelled(state, 'Apply animation settings')) == 1
        type_after(state, 'Animation name', '2')
        state = snapshot_state('rename-typed')
        at = next(i for i, e in enumerate(state['elements']) if e.get('value') == 'Animation name')
        renamed = next(e for e in state['elements'][at:] if e.get('role') == 'AXTextField')['value']
        assert renamed in ('2', 'Wave2'), renamed
        click_label(state, 'Rename animation')
        assert animations() == [renamed]
        result['checks']['nativeRename'] = renamed

        click_label(snapshot_state('renamed'), 'Delete animation')
        assert animations() == []
        assert mcp.op('undo')['model']['animations'].keys() == {renamed}
        state = snapshot_state('delete-undone')
        assert len(labelled(state, 'Delete animation')) == 1
        result['checks']['nativeDeleteSharesUndo'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        result['screenshots'] = screenshots
        result['status'] = 'PASS_INX_NATIVE_ANIMATION_LIST_ONLY'
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
