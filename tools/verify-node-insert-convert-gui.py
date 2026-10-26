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


def node(uuid, name, node_type='Node', trans=None, children=None, extra=None):
    entry = {
        'uuid': uuid, 'name': name, 'type': node_type, 'enabled': True,
        'zsort': 0.0,
        'transform': {'trans': trans or [0.0, 0.0, 0.0],
                      'rot': [0.0, 0.0, 0.0], 'scale': [1.0, 1.0]},
        'lockToRoot': False, 'pinToMesh': False,
    }
    if children is not None:
        entry['children'] = children
    if extra:
        entry.update(extra)
    return entry


MODEL = {
    'meta': {'name': 'fixture', 'version': '1.0-alpha'},
    'nodes': node(1, 'Fixture', children=[
        node(9, 'A', trans=[10.0, 0.0, 0.0]),
        node(10, 'Cam', 'Camera', extra={'viewport': [1920.0, 1080.0]}),
    ]),
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


def read_inx(path):
    data = path.read_bytes()
    assert data[:8] == b'TRNSRTS\0', 'saved file missing TRNSRTS header'
    (length,) = struct.unpack('>I', data[8:12])
    payload = data[12:12 + length]
    assert data[12 + length:12 + length + 8] == b'TEX_SECT', 'missing TEX_SECT'
    return json.loads(payload)


def source_hashes():
    paths = sorted(ROOT.glob('rust/src/**/*.rs'))
    paths += [ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
              Path(__file__).resolve(), HELPER]
    return {str(p.relative_to(ROOT)):
            hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}


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
    run = Path(tempfile.mkdtemp(prefix='node-insert-convert-gui-',
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
        session = f'node-insert-gui-{proc.pid}'
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
                               'clientInfo': {'name': 'gui-node-insert', 'version': '1'}})

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

        def combo_pick(state, label, value, open_shot, picked_shot):
            found = combo(state, label)
            assert len(found) == 1, (label,
                                     [e.get('label') for e in state['elements']])
            prior = {(round(e['frame']['x'], 1), round(e['frame']['y'], 1))
                     for e in labelled(state, value) if e.get('frame')}
            cua('click', {'target': target,
                          'element_token': found[0]['element_token'],
                          'session': session})
            time.sleep(0.5)
            open_state = snapshot_state(open_shot)
            options = [e for e in open_state['elements']
                       if (e.get('label') or e.get('value')) == value
                       and e.get('role') != 'AXPopUpButton'
                       and (round(e['frame']['x'], 1), round(e['frame']['y'], 1))
                       not in prior]
            assert len(options) == 1, (value,
                [(e.get('label'), e.get('role'), e.get('frame'))
                 for e in open_state['elements']])
            cua('click', {'target': target,
                          'element_token': options[0]['element_token'],
                          'session': session})
            time.sleep(0.4)
            return snapshot_state(picked_shot)

        def text_field(state, label):
            found = [e for e in state['elements']
                     if e.get('role') == 'AXTextField'
                     and (e.get('label') or e.get('value')) == label]
            assert len(found) == 1, (label,
                [(e.get('label'), e.get('value'))
                 for e in state['elements'] if e.get('role') == 'AXTextField'])
            return found[0]

        def type_into(state, label, text, focus_shot):
            field = text_field(state, label)
            cua('click', {'target': target,
                          'element_token': field['element_token'],
                          'session': session})
            time.sleep(0.3)
            state = snapshot_state(focus_shot)
            field = text_field(state, label)
            cua('type_text', {'pid': proc.pid,
                              'element_token': field['element_token'],
                              'text': text, 'session': session})
            time.sleep(0.4)
            return snapshot_state(f'{focus_shot}-typed')

        def clear_field(state, label, focus_shot):
            field = text_field(state, label)
            cua('click', {'target': target,
                          'element_token': field['element_token'],
                          'session': session})
            time.sleep(0.3)
            state = snapshot_state(focus_shot)
            field = text_field(state, label)
            cua('hotkey', {'keys': ['cmd', 'a'], 'pid': proc.pid,
                           'window_id': window, 'session': session})
            time.sleep(0.3)
            cua('press_key', {'key': 'delete', 'pid': proc.pid,
                              'window_id': window, 'session': session})
            time.sleep(0.4)
            return snapshot_state(f'{focus_shot}-cleared')

        def clipboard_sources(mcp):
            snap = mcp.snapshot()
            return [entry.get('source') for entry in snap.get('clipboard', [])]

        state = snapshot_state('initial')
        assert len(labelled(state, 'A #9')) == 1
        assert len(labelled(state, 'Cam #10')) == 1
        assert len(combo(state, 'Insert class')) == 1
        assert len(combo(state, 'Convert to')) == 1
        text_field(state, 'Insert suffix')
        assert not enabled_buttons(state, 'Insert')
        assert not enabled_buttons(state, 'Convert')
        result['checks']['controlsPresentAndGated'] = True

        click_label(state, 'Fixture #1')
        state = snapshot_state('root-selected')
        assert not enabled_buttons(state, 'Insert')
        assert not enabled_buttons(state, 'Convert')
        assert not enabled_buttons(state, 'Copy')
        result['checks']['rootSelectionDisablesInsertConvert'] = True

        click_label(state, 'A #9')
        state = snapshot_state('a-selected')
        assert enabled_buttons(state, 'Insert')
        assert not enabled_buttons(state, 'Convert')
        state = type_into(state, 'Insert suffix', ' Group', 'suffix-focused')
        state = combo_pick(state, 'Insert class', 'MeshGroup',
                           'insert-class-open', 'insert-class-picked')
        click_label(state, 'Insert')
        snap = mcp.snapshot()
        root_children = snap['model']['nodes']['children']
        assert len(root_children) == 2
        wrapper = root_children[0]
        assert wrapper['type'] == 'MeshGroup'
        assert wrapper['name'] == 'A Group'
        assert wrapper['transform']['trans'] == [10.0, 0.0, 0.0]
        child = find_node(wrapper, 9)
        assert child is not None and child['transform']['trans'] == [0.0, 0.0, 0.0]
        wrapper_id = wrapper['uuid']
        result['wrapperId'] = wrapper_id
        result['checks']['nativeInsertWraps'] = True

        state = snapshot_state('inserted')
        suffix_field = text_field(state, 'Insert suffix')
        assert not (suffix_field.get('value') or ''), suffix_field
        result['checks']['suffixClearedOnSuccess'] = True

        click_label(state, 'Copy')
        assert wrapper_id in clipboard_sources(mcp)
        result['checks']['wrapperSelectionByCopy'] = True

        state = snapshot_state('wrapper-copied')
        click_label(state, 'A #9')
        state = snapshot_state('child-a-selected')
        state = type_into(state, 'Insert suffix', ' Group', 'suffix-refocused')
        failed_before = mcp.snapshot()
        click_label(state, 'Insert')
        assert mcp.snapshot() == failed_before
        state = snapshot_state('insert-dup-failed')
        suffix_field = text_field(state, 'Insert suffix')
        assert (suffix_field.get('value') or '').endswith('Group'), suffix_field
        assert labelled(state, 'Name already taken'), [
            e.get('label') for e in state['elements']]
        result['checks']['duplicateInsertFailsPreservesInput'] = True
        state = clear_field(state, 'Insert suffix', 'suffix-clear-focused')
        suffix_field = text_field(state, 'Insert suffix')
        assert not (suffix_field.get('value') or ''), suffix_field
        result['checks']['suffixClearedByNativeTextAction'] = True

        click_label(state, f'A Group #{wrapper_id}')
        state = snapshot_state('select-wrapper-undo')
        click_label(state, 'Undo')
        snap = mcp.snapshot()
        root_children = snap['model']['nodes']['children']
        assert [c['uuid'] for c in root_children] == [9, 10]
        assert root_children[0]['transform']['trans'] == [10.0, 0.0, 0.0]
        state = snapshot_state('undo-insert')
        assert not enabled_buttons(state, 'Copy')
        result['checks']['nativeUndoRemovesWrapper'] = True

        click_label(state, 'Redo')
        snap = mcp.snapshot()
        wrapper = find_node(snap['model']['nodes'], wrapper_id)
        assert wrapper is not None and wrapper['type'] == 'MeshGroup'
        state = snapshot_state('redo-insert')
        assert not enabled_buttons(state, 'Copy')
        result['checks']['nativeRedoRecreatesWrapperWithoutSelection'] = True

        click_label(state, 'A #9')
        state = snapshot_state('a-reselected')
        state = combo_pick(state, 'Convert to', 'MeshGroup',
                           'convert-open', 'convert-picked')
        assert combo(state, 'Convert to')[0].get('value') == 'MeshGroup'
        click_label(state, 'Convert')
        snap = mcp.snapshot()
        node_a = find_node(snap['model']['nodes'], 9)
        assert node_a['type'] == 'MeshGroup'
        assert node_a['name'] == 'A'
        assert node_a['transform']['trans'] == [0.0, 0.0, 0.0]
        result['checks']['nativeConvertRetainsUuidNameTransform'] = True

        state = snapshot_state('converted')
        assert combo(state, 'Convert to')[0].get('value') == 'Convert to', (
            'destination must reset after the type changes', [
                (e.get('label'), e.get('value'))
                for e in state['elements'] if e.get('role') == 'AXPopUpButton'])
        click_label(state, 'Copy')
        assert 9 in clipboard_sources(mcp)
        result['checks']['convertRetainsSelectionAndResetsDestination'] = True

        state = snapshot_state('convert-copied')
        click_label(state, 'Undo')
        snap = mcp.snapshot()
        node_a = find_node(snap['model']['nodes'], 9)
        assert node_a['type'] == 'Node' and node_a['uuid'] == 9
        state = snapshot_state('undo-convert')
        click_label(state, 'Copy')
        assert 9 in clipboard_sources(mcp)
        state = snapshot_state('undo-convert-copied')
        click_label(state, 'Redo')
        snap = mcp.snapshot()
        node_a = find_node(snap['model']['nodes'], 9)
        assert node_a['type'] == 'MeshGroup' and node_a['uuid'] == 9
        state = snapshot_state('redo-convert')
        click_label(state, 'Copy')
        assert 9 in clipboard_sources(mcp)
        result['checks']['convertUndoRedoRestoresTypeAndSelection'] = True

        state = snapshot_state('redo-convert-copied')
        state = combo_pick(state, 'Convert to', 'DynamicComposite',
                           'convert-dynamic-open', 'convert-dynamic-picked')
        assert combo(state, 'Convert to')[0].get('value') == 'DynamicComposite'
        click_label(state, 'Cam #10')
        state = snapshot_state('cam-selected')
        assert combo(state, 'Convert to')[0].get('value') == 'Convert to'
        result['checks']['invalidDestinationResets'] = True

        click_label(state, 'Insert')
        snap = mcp.snapshot()
        root_children = snap['model']['nodes']['children']
        cam_wrap = root_children[0]
        assert cam_wrap['type'] == 'MeshGroup'
        assert cam_wrap['name'] == 'Unnamed MeshGroup'
        assert find_node(cam_wrap, 10) is not None
        assert root_children[1]['uuid'] == wrapper_id
        state = snapshot_state('cam-wrapped')
        result['checks']['defaultNamedWrapperAboveCamera'] = True

        click_label(state, 'Undo')
        snap = mcp.snapshot()
        root_children = snap['model']['nodes']['children']
        assert [c['uuid'] for c in root_children] == [wrapper_id, 10]
        assert root_children[1]['type'] == 'Camera'
        result['checks']['cameraWrapUndone'] = True

        mcp.op('save', path=str(saved))
        reopened = read_inx(saved)
        saved_children = reopened['nodes']['children']
        assert saved_children[0]['type'] == 'MeshGroup'
        assert saved_children[0]['name'] == 'A Group'
        assert find_node(saved_children[0], 9)['type'] == 'MeshGroup'
        reopened = mcp.op('open', path=str(saved))
        saved_children = reopened['model']['nodes']['children']
        assert saved_children[0]['type'] == 'MeshGroup'
        assert find_node(saved_children[0], 9)['type'] == 'MeshGroup'
        result['checks']['savedAndReopenedModel'] = True

        snapshot_state('final')
        assert fixture.read_bytes() == original
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == result['binarySha256']
        assert source_hashes() == result['sourceSha256']
        result['screenshots'] = screenshots
        result['status'] = 'PASS_INX_NATIVE_NODE_INSERT_CONVERT_ONLY'
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
