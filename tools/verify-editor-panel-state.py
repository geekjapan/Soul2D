#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
STATE = 'resource://soul2d/editor/state'


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HARNESS = load('editor_mcp_harness', 'tools/verify-animation-key-gui.py')
CORE = load('panel_fixture', 'tools/verify-rust-core.py')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def state(peer):
    response = peer.rpc('resources/read', {'uri': STATE})
    return json.loads(response['contents'][0]['text'])


def document(snapshot):
    return {k: snapshot.get(k) for k in ['model', 'revision', 'modified', 'history', 'values', 'documentFormat']}


def stop(proc):
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    return {'pid': proc.pid, 'exit': proc.returncode, 'running': proc.poll() is None}


def main():
    parser = argparse.ArgumentParser(description='Verify individual editor panel state through production stdio MCP and a settings restart; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    editor, core = args.editor.resolve(), args.core.resolve()
    evidence = ROOT / 'build/verification/issue99-panels'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HARNESS.__file__), Path(CORE.__file__), ROOT / 'rust/Cargo.toml',
        ROOT / 'rust/Cargo.lock', ROOT / 'rust/tests/app_shell.rs', ROOT / 'rust/tests/last_gaps.rs',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/SKILL.md', ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}
    binaries = {str(p): digest(p) for p in [editor, core]}
    result = {'status': 'FAIL', 'scope': 'Outline/Timeline editor state, independent settings persistence and headless refusal',
              'sourceHashes': hashes, 'binaries': binaries, 'nativeGui': 'NOT_RUN', 'realAcpAgent': 'NOT_RUN',
              'vendorAcceptance': 'NOT_RUN', 'cleanup': []}
    proc = None
    try:
        source = run / 'source.inx'
        CORE.fixture(source)
        original = source.read_bytes()
        env = HARNESS.editor_env(run)
        settings = run / 'config/settings.json'
        with (run / 'headless-wire.jsonl').open('w') as wire, (run / 'headless.stderr').open('w') as stderr:
            proc = subprocess.Popen([str(core), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=stderr, text=True)
            peer = HARNESS.Mcp(proc, wire)
            peer.rpc('initialize', {})
            before = peer.snapshot()
            listed = peer.rpc('tools/list', {})['tools']
            schema = next(t for t in listed if t['name'] == 'editor_panel_set')['inputSchema']
            assert schema['properties']['panel']['enum'] == ['outline', 'timeline']
            assert set(schema['required']) == {'panel', 'visible'} and schema['additionalProperties'] is False
            response = peer.rpc('tools/call', {'name': 'editor_panel_set', 'arguments': {'panel': 'outline', 'visible': False}})
            assert response['isError'] is True
            assert 'editor_panel_set needs the editor: this host has no window' in response['content'][0]['text']
            assert peer.snapshot() == before and state(peer)['editor'] is False
            result['cleanup'].append(stop(proc))
            proc = None
        stages = []
        for phase in ['first', 'restarted']:
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen([str(editor), '--mcp', '--open', str(source)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=stderr, text=True, env=env)
                peer = HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = document(peer.snapshot())
                initial = state(peer)
                assert initial['editor'] is True
                assert initial['panels'] == ({'outline': True, 'timeline': True} if phase == 'first'
                                             else {'outline': False, 'timeline': True})
                if phase == 'first':
                    for panel in ['outline', 'timeline']:
                        for visible in [False, True, True]:
                            previous = state(peer)['panels']
                            response = peer.op('editor_panel_set', panel=panel, visible=visible)
                            expected = dict(previous, **{panel: visible})
                            assert response['panels'] == expected and state(peer)['panels'] == expected
                            assert document(peer.snapshot()) == baseline
                            stored = json.loads(settings.read_text())
                            assert stored[panel.title() + '.visible'] is visible
                    for arguments in [{}, {'panel': 'outline'}, {'visible': True},
                                      {'panel': 'Outline', 'visible': False}, {'panel': 'unknown', 'visible': True},
                                      {'panel': 1, 'visible': True}, {'panel': 'outline', 'visible': None},
                                      {'panel': 'outline', 'visible': 1}, {'panel': 'outline', 'visible': False, 'extra': True}]:
                        previous, data = state(peer)['panels'], settings.read_bytes()
                        response = peer.rpc('tools/call', {'name': 'editor_panel_set', 'arguments': arguments})
                        assert response['isError'] is True
                        assert state(peer)['panels'] == previous and settings.read_bytes() == data
                        assert document(peer.snapshot()) == baseline
                    expected = {'outline': False, 'timeline': True}
                    peer.op('editor_panel_set', panel='outline', visible=False)
                else:
                    expected = {'outline': False, 'timeline': False}
                    peer.op('editor_panel_set', panel='timeline', visible=False)
                assert state(peer)['panels'] == expected and document(peer.snapshot()) == baseline
                stages.append({'phase': phase, 'panels': expected, 'revision': baseline['revision'],
                               'history': baseline['history'], 'documentUnchanged': True})
                result['cleanup'].append(stop(proc))
                proc = None
        assert source.read_bytes() == original
        stored = json.loads(settings.read_text())
        assert stored['Outline.visible'] is False and stored['Timeline.visible'] is False
        assert stored['firstrun_complete'] is True and stored['hasDoneQuickSetup'] is True
        result.update(status='PASS', stages=stages, sourceSha256=digest(source), settingsSha256=digest(settings))
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if proc is not None:
                result['cleanup'].append(stop(proc))
            assert all(not item['running'] for item in result['cleanup'])
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)} == hashes
            assert {str(p): digest(p) for p in [editor, core]} == binaries
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
