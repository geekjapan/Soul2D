#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
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

FILLERS = [{'uuid': 100 + n,
            'name': f'Filler{n:02d}', 'type': 'Node', 'enabled': True,
            'zsort': 0.0,
            'transform': {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                          'scale': [1.0, 1.0]},
            'lockToRoot': False, 'pinToMesh': False}
           for n in range(1, 41)]

MODEL = {
    'meta': {'name': 'fixture', 'version': '1.0-alpha'},
    'nodes': {
        'uuid': 1, 'name': 'Fixture', 'type': 'Node', 'enabled': True,
        'zsort': 0.0,
        'transform': {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                      'scale': [1.0, 1.0]},
        'lockToRoot': False, 'pinToMesh': False,
        'children': [
            {'uuid': 9, 'name': 'Alpha', 'type': 'Node', 'enabled': True,
             'zsort': 0.0,
             'transform': {'trans': [10.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                           'scale': [1.0, 1.0]},
             'lockToRoot': False, 'pinToMesh': False,
             'children': [
                 {'uuid': 11, 'name': 'Needle', 'type': 'Node', 'enabled': True,
                  'zsort': 0.0,
                  'transform': {'trans': [0.0, 0.0, 0.0],
                                'rot': [0.0, 0.0, 0.0], 'scale': [1.0, 1.0]},
                  'lockToRoot': False, 'pinToMesh': False},
             ]},
            {'uuid': 10, 'name': 'Beta', 'type': 'Node', 'enabled': True,
             'zsort': 0.0,
             'transform': {'trans': [100.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                           'scale': [1.0, 1.0]},
             'lockToRoot': False, 'pinToMesh': False},
            *FILLERS,
        ],
    },
    'param': [],
    'animations': {},
}

ROW = re.compile(r'#\d+$')


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
    run = Path(tempfile.mkdtemp(prefix='node-tree-view-gui-', dir=args.evidence_root))
    result['run'] = str(run)
    print(run, flush=True)
    fixture = run / 'fixture.inx'
    fixture.write_bytes(inx(MODEL))
    original = fixture.read_bytes()
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
        session = f'node-tree-gui-{proc.pid}'
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
                               'clientInfo': {'name': 'gui-node-tree', 'version': '1'}})

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

        def rows(state):
            return [e for e in state['elements']
                    if e.get('role') == 'AXButton'
                    and ROW.search(str(e.get('label') or e.get('value') or ''))]

        def window_bounds(state):
            bounds = state.get('window_bounds') or {}
            return (bounds.get('y', 0.0),
                    bounds.get('y', 0.0) + bounds.get('height', 0.0))

        def on_screen(state, text):
            top, bottom = window_bounds(state)
            for e in labelled(state, text):
                frame = e.get('frame') or {}
                y, h = frame.get('y'), frame.get('h', 0.0)
                if y is not None and y < bottom and y + h > top:
                    return True
            return False

        def first_row(state):
            entries = [e for e in rows(state)
                       if (e.get('frame') or {}).get('y') is not None
                       and (e.get('label') or '') != 'Fixture #1']
            return min(entries, key=lambda e: e['frame']['y']) if entries else None

        def filter_field(state):
            found = [e for e in state['elements'] if e.get('role') == 'AXTextField'
                     and (e.get('label') or e.get('value')) == 'Node filter']
            assert len(found) == 1, [(e.get('label'), e.get('value'))
                                     for e in state['elements']
                                     if e.get('role') == 'AXTextField']
            return found[0]

        def type_filter(state, text, replace):
            field = filter_field(state)
            cua('click', {'target': target,
                          'element_token': field['element_token'],
                          'session': session})
            time.sleep(0.3)
            if replace:
                cua('hotkey', {'keys': ['cmd', 'a'], 'pid': proc.pid,
                               'window_id': window, 'session': session})
                time.sleep(0.3)
                if not text:
                    cua('press_key', {'key': 'delete', 'pid': proc.pid,
                                      'window_id': window, 'session': session})
                    time.sleep(0.4)
                    return
            if text:
                cua('type_text', {'pid': proc.pid,
                                  'element_token': field['element_token'],
                                  'text': text, 'session': session})
            time.sleep(0.5)

        state = snapshot_state('initial')
        filter_field(state)
        # filter_field() already asserts that exactly one text field is labelled 'Node filter'; the other
        # panels' text fields (names, suffixes, metadata) are not counted because their number changes.
        for label in ('Reverse node order', 'Follow selection'):
            assert len(labelled(state, label)) == 1, label
        for label in ('Alpha #9', 'Beta #10', 'Needle #11'):
            assert len(labelled(state, label)) == 1, label
        assert first_row(state)['label'] == 'Alpha #9'
        result['checks']['controlsPresent'] = True

        snapshot_before = mcp.snapshot()

        type_filter(state, 'NEEDLE', replace=False)
        state = snapshot_state('filter-needle')
        for label in ('Fixture #1', 'Alpha #9', 'Needle #11'):
            assert len(labelled(state, label)) == 1, label
        for label in ('Beta #10', 'Filler01 #101', 'Filler40 #140'):
            assert not labelled(state, label), label
        result['checks']['filterNeedle'] = True

        type_filter(state, 'ALPHA', replace=True)
        state = snapshot_state('filter-alpha')
        for label in ('Fixture #1', 'Alpha #9'):
            assert len(labelled(state, label)) == 1, label
        for label in ('Needle #11', 'Beta #10'):
            assert not labelled(state, label), label
        result['checks']['filterParentOnly'] = True

        type_filter(state, 'ZZZNOMATCH', replace=True)
        state = snapshot_state('filter-none')
        assert len(labelled(state, 'No nodes match the filter')) == 1
        assert not rows(state)
        result['checks']['filterNoMatch'] = True

        type_filter(state, '', replace=True)
        state = snapshot_state('filter-cleared')
        for label in ('Alpha #9', 'Beta #10', 'Needle #11'):
            assert len(labelled(state, label)) == 1, label
        result['checks']['filterCleared'] = True

        click_label(state, 'Reverse node order')
        state = snapshot_state('reversed')
        first = first_row(state)
        assert first is not None and first['label'] == 'Filler40 #140', (
            first, [e.get('label') for e in rows(state)])
        canonical = [child['uuid']
                     for child in mcp.snapshot()['model']['nodes']['children']]
        assert canonical[:2] == [9, 10] and canonical[2:] == list(range(101, 141))
        result['checks']['nativeReverse'] = True

        click_label(state, 'Reverse node order')
        state = snapshot_state('normal-order')
        assert first_row(state)['label'] == 'Alpha #9'
        result['checks']['nativeReverseRestored'] = True

        shot_w = state.get('screenshot_width', 0)
        shot_h = state.get('screenshot_height', 0)
        assert shot_w > 0 and shot_h > 0, (shot_w, shot_h)
        sx = shot_w * 0.15
        sy = shot_h * 0.5

        found = False
        for _ in range(12):
            cua('scroll', {'target': target, 'direction': 'down',
                           'by': 'page', 'amount': 1,
                           'x': sx, 'y': sy, 'session': session})
            time.sleep(0.7)
            state = snapshot_state('scroll-down')
            if on_screen(state, 'Filler40 #140'):
                found = True
                break
        assert found, 'sidebar did not scroll Filler40 into view'
        result['checks']['nativeScrollRevealsLastRow'] = True

        click_label(state, 'Filler40 #140')
        state = snapshot_state('filler40-selected')
        assert on_screen(state, 'Filler40 #140')
        result['checks']['nativeSelectBottomRow'] = True

        hidden = False
        for _ in range(12):
            cua('scroll', {'target': target, 'direction': 'up',
                           'by': 'page', 'amount': 1,
                           'x': sx, 'y': sy, 'session': session})
            time.sleep(0.7)
            state = snapshot_state('scroll-up')
            if not on_screen(state, 'Filler40 #140'):
                hidden = True
                break
        assert hidden, 'sidebar did not scroll Filler40 out of view'
        result['checks']['nativeScrollHidesSelectedRow'] = True

        click_label(state, 'Follow selection')
        state = snapshot_state('followed')
        assert on_screen(state, 'Filler40 #140'), (
            'follow did not scroll the selected row on screen',
            [e.get('label') for e in rows(state)])
        result['checks']['nativeFollowScrollsToSelection'] = True

        hidden = False
        for _ in range(8):
            cua('scroll', {'target': target, 'direction': 'up',
                           'by': 'page', 'amount': 1,
                           'x': sx, 'y': sy, 'session': session})
            time.sleep(0.7)
            state = snapshot_state('manual-away')
            if not on_screen(state, 'Filler40 #140'):
                hidden = True
                break
        assert hidden, 'manual scroll did not move away from the selection'
        state = snapshot_state('manual-away-check')
        assert not on_screen(state, 'Filler40 #140'), (
            'follow recentered after the one-time scroll')
        result['checks']['nativeFollowOnceOnly'] = True

        assert mcp.snapshot() == snapshot_before
        result['checks']['snapshotUnchanged'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        result['screenshots'] = screenshots
        result['status'] = 'PASS_INX_NATIVE_NODE_TREE_VIEW_ONLY'
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
