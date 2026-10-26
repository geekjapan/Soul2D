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
        'children': [
            {'uuid': 9, 'name': 'A', 'type': 'Node', 'enabled': True,
             'zsort': 0.0,
             'transform': {'trans': [10.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                           'scale': [1.0, 1.0]},
             'lockToRoot': False, 'pinToMesh': False},
            {'uuid': 10, 'name': 'Parent', 'type': 'Node', 'enabled': True,
             'zsort': 0.0,
             'transform': {'trans': [100.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0],
                           'scale': [1.0, 1.0]},
             'lockToRoot': False, 'pinToMesh': False},
        ],
    },
    'param': [],
    'animations': {},
}


def find_node(node, uuid):
    if node.get('uuid') == uuid:
        return node
    for child in node.get('children', []):
        found = find_node(child, uuid)
        if found is not None:
            return found
    return None


def parent_of(root, uuid):
    for child in root.get('children', []):
        if child.get('uuid') == uuid:
            return root
        found = parent_of(child, uuid)
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
              'binarySha256': hashlib.sha256(binary.read_bytes()).hexdigest()}

    args.evidence_root.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='node-authoring-gui-', dir=args.evidence_root))
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
        session = f'node-auth-gui-{proc.pid}'
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
                               'clientInfo': {'name': 'gui-node-auth', 'version': '1'}})

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

        def enabled_buttons(state, label):
            return [e for e in state['elements'] if e.get('role') == 'AXButton'
                    and (e.get('label') or e.get('value')) == label
                    and e.get('enabled', True)]

        def combo(state, text):
            return [e for e in state['elements'] if e.get('role') == 'AXPopUpButton'
                    and (e.get('label') or e.get('value')) == text]

        def combo_pick(state, label, value):
            found = combo(state, label)
            assert len(found) == 1, (label, [e.get('label') for e in state['elements']])
            prior = {(round(e['frame']['x'], 1), round(e['frame']['y'], 1))
                     for e in labelled(state, value) if e.get('frame')}
            cua('click', {'target': target,
                          'element_token': found[0]['element_token'],
                          'session': session})
            time.sleep(0.5)
            state = snapshot_state('combo-open')
            options = [e for e in state['elements']
                       if (e.get('label') or e.get('value')) == value
                       and e.get('role') != 'AXPopUpButton'
                       and (round(e['frame']['x'], 1), round(e['frame']['y'], 1))
                       not in prior]
            assert len(options) == 1, (value,
                [(e.get('label'), e.get('role'), e.get('frame'))
                 for e in state['elements']])
            cua('click', {'target': target,
                          'element_token': options[0]['element_token'],
                          'session': session})
            time.sleep(0.4)
            return snapshot_state('combo-picked')

        def name_field(state):
            fields = [e for e in state['elements'] if e.get('role') == 'AXTextField']
            named = [e for e in fields
                     if (e.get('label') or e.get('value')) == 'New node name']
            assert len(named) == 1, [(e.get('label'), e.get('value'))
                                     for e in fields]
            return named[0]

        state = snapshot_state('initial')
        assert len(labelled(state, 'A #9')) == 1
        assert len(labelled(state, 'Parent #10')) == 1
        assert enabled_buttons(state, 'Create')
        assert not enabled_buttons(state, 'Paste')
        result['checks']['rowsPresent'] = True

        click_label(state, 'A #9')
        state = snapshot_state('a-selected')
        combo_picked = combo_pick(state, 'Node class', 'Composite')
        field = name_field(combo_picked)
        cua('click', {'target': target,
                      'element_token': field['element_token'],
                      'session': session})
        time.sleep(0.3)
        cua('type_text', {'pid': proc.pid,
                          'element_token': field['element_token'],
                          'text': 'Created', 'session': session})
        time.sleep(0.4)
        state = snapshot_state('named')
        click_label(state, 'Create')
        snap = mcp.snapshot()
        node_a = find_node(snap['model']['nodes'], 9)
        assert len(node_a.get('children', [])) == 1
        created = node_a['children'][0]
        created_id = created['uuid']
        assert created['name'] == 'Created' and created['type'] == 'Composite'
        assert created['transform']['trans'] == [0.0, 0.0, 0.0]
        result['checks']['nativeCreateUnderA'] = True

        click_label(snapshot_state('created'), 'Copy')
        snap = mcp.snapshot()
        assert len(snap['clipboard']) == 1
        state = snapshot_state('copied')
        click_label(state, 'Parent #10')
        state = snapshot_state('parent-selected')
        click_label(state, 'Paste')
        snap = mcp.snapshot()
        parent = find_node(snap['model']['nodes'], 10)
        assert len(parent.get('children', [])) == 1
        pasted = parent['children'][0]
        pasted_id = pasted['uuid']
        assert pasted_id != created_id
        assert pasted['transform']['trans'][0] == -90.0
        assert snap['clipboard'] == []
        result['checks']['nativeCopyPasteWorldPreserved'] = True

        state = snapshot_state('pasted')
        click_label(state, 'Undo')
        snap = mcp.snapshot()
        parent = find_node(snap['model']['nodes'], 10)
        assert not parent.get('children')
        result['checks']['nativeUndoRemovesPaste'] = True

        click_label(snapshot_state('undo-paste'), 'Redo')
        snap = mcp.snapshot()
        parent = find_node(snap['model']['nodes'], 10)
        assert len(parent.get('children', [])) == 1
        result['checks']['nativeRedoReappliesPaste'] = True

        state = snapshot_state('redo')
        click_label(state, f"{pasted['name']} #{pasted_id}")
        state = snapshot_state('copy-selected')
        combo_picked = combo_pick(state, 'Move destination', 'Fixture #1')
        click_label(combo_picked, 'Move')
        snap = mcp.snapshot()
        roots = snap['model']['nodes'].get('children', [])
        moved = find_node(snap['model']['nodes'], pasted_id)
        assert moved is not None
        assert parent_of(snap['model']['nodes'], pasted_id)['uuid'] == 1
        assert moved['transform']['trans'][0] == 10.0
        result['checks']['nativeReparentToRoot'] = True

        state = snapshot_state('moved')
        click_label(state, 'Undo')
        snap = mcp.snapshot()
        moved = find_node(snap['model']['nodes'], pasted_id)
        assert parent_of(snap['model']['nodes'], pasted_id)['uuid'] == 10
        assert moved['transform']['trans'][0] == -90.0
        result['checks']['nativeUndoRestoresParentAndLocal'] = True

        click_label(snapshot_state('undo-move'), 'Cut')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], pasted_id) is None
        assert len(snap['clipboard']) == 1
        cut_clip = snap['clipboard'][0]
        prior_root_ids = {c['uuid']
                          for c in snap['model']['nodes'].get('children') or []}
        state = snapshot_state('cut')
        click_label(state, 'Paste')
        snap = mcp.snapshot()
        root_children = snap['model']['nodes'].get('children') or []
        assert len(root_children) == 3
        added = [c for c in root_children if c['uuid'] not in prior_root_ids]
        assert len(added) == 1
        restored = added[0]
        assert restored['name'] == cut_clip['name']
        assert restored['transform']['trans'][0] == 10.0
        assert len(snap['clipboard']) == 0
        result['checks']['nativeCutPaste'] = True

        click_label(snapshot_state('repasted'), 'Delete')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], restored['uuid']) is None
        state = snapshot_state('deleted')
        click_label(state, 'Undo')
        snap = mcp.snapshot()
        assert find_node(snap['model']['nodes'], restored['uuid']) is not None
        state = snapshot_state('undo-delete')
        for label in ('Copy', 'Cut', 'Delete', 'Move'):
            assert not enabled_buttons(state, label), label
        result['checks']['nativeDeleteUndoRestores'] = True

        mcp.op('save', path=str(saved))
        raw = saved.read_bytes()
        assert raw[:8] == b'TRNSRTS\0'
        length = int.from_bytes(raw[8:12], 'big')
        model = json.loads(raw[12:12 + length])
        saved_parent = find_node(model['nodes'], 10)
        assert not saved_parent.get('children')
        root_children = model['nodes']['children']
        assert {child['uuid'] for child in root_children} == {9, 10, restored['uuid']}
        saved_restored = next(child for child in root_children
                              if child['uuid'] == restored['uuid'])
        assert saved_restored['transform']['trans'][0] == 10.0
        assert saved_restored['type'] == 'Composite'
        saved_a = find_node(model['nodes'], 9)
        assert saved_a['children'][0]['name'] == 'Created'
        assert saved_a['children'][0]['type'] == 'Composite'
        result['checks']['savedSerializedTree'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        result['screenshots'] = screenshots
        result['savedSha256'] = hashlib.sha256(raw).hexdigest()
        result['status'] = 'PASS_INX_NATIVE_NODE_AUTHORING_ONLY'
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
