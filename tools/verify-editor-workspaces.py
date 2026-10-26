#!/usr/bin/env python3
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]

spec = importlib.util.spec_from_file_location('workspace_helper', ROOT / 'tools/verify-editor-panel-state.py')
HELPER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(HELPER)
TOOLS = ['editor_workspace_save', 'editor_workspace_apply', 'editor_workspace_delete']


def workspace(peer, action, name):
    result = peer.rpc('tools/call', {'name': 'editor_workspace_' + action, 'arguments': {'name': name}})
    assert result['isError'] is False, result
    return result['structuredContent']


def drawn_layout(peer, settings, name, expected_sizes=None):
    deadline = time.monotonic() + 5
    while True:
        workspace(peer, 'save', name)
        layout = json.loads(json.loads(settings.read_text())['workspaces'][name])
        sizes = layout['sizes']
        if sizes['outline'] is not None and (expected_sizes is None or sizes == expected_sizes):
            return layout
        assert time.monotonic() < deadline, (name, sizes, expected_sizes)
        time.sleep(0.05)


def main():
    parser = argparse.ArgumentParser(description='Verify production workspace and Reset Layout MCP, persistence and write errors; no UI automation.')
    parser.add_argument('--editor', type=Path, required=True)
    parser.add_argument('--core', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    binaries = [args.editor.resolve(), args.core.resolve()]
    evidence = ROOT / 'build/verification/issue99-layout-reset'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HELPER.__file__), Path(HELPER.HARNESS.__file__), Path(HELPER.CORE.__file__),
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / 'doc/commands.manifest.json',
        ROOT / 'doc/mcp_api.md', ROOT / 'doc/gui-operation-inventory.md',
        ROOT / '.cursor/skills/operate-soul2d/families/editor-state.md'}
    hashes = {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)}
    binary_hashes = {str(p): HELPER.digest(p) for p in binaries}
    result = {'status': 'FAIL', 'scope': 'Workspace and Reset Layout, sizes, restart, headless refusal and session-only write errors',
              'sourceHashes': hashes, 'binaries': binary_hashes, 'nativeGui': 'NOT_RUN',
              'hostAcpApproval': 'Rust editor and authenticated TCP tests only; not exercised by this stdio proof',
              'realAcpAgent': 'NOT_RUN', 'cleanup': [], 'stages': []}
    proc = None
    try:
        source = run / 'source.inx'
        HELPER.CORE.fixture(source)
        original = source.read_bytes()
        env = HELPER.HARNESS.editor_env(run)
        sized_env = None
        for phase in ['headless', 'fresh-reset', 'sized-reset', 'sized-restarted', 'first', 'restarted',
                      'failure-save', 'failure-apply', 'failure-delete', 'failure-reset']:
            phase_env = env
            config = run / 'config'
            if phase == 'sized-restarted':
                phase_env = sized_env
                config = run / 'sized-reset/config'
            elif phase not in ['headless', 'first', 'restarted']:
                isolated = run / phase
                isolated.mkdir()
                phase_env = HELPER.HARNESS.editor_env(isolated)
                config = isolated / 'config'
            settings = config / 'settings.json'
            if phase == 'sized-reset':
                sized_env = phase_env
                data = json.loads(settings.read_text())
                data.update({'Outline.visible': False, 'Timeline.visible': False, 'NerdStats': True,
                             'workspaces': {'Sized': json.dumps({'panels': {'outline': False, 'timeline': False},
                                                                'sizes': {'outline': 777, 'timeline': 222}})}})
                settings.write_text(json.dumps(data))
            command = [str(binaries[1]), '--mcp'] if phase == 'headless' else [str(binaries[0]), '--mcp', '--open', str(source)]
            with (run / (phase + '-wire.jsonl')).open('w') as wire, (run / (phase + '.stderr')).open('w') as stderr:
                proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=stderr, text=True, env=phase_env)
                peer = HELPER.HARNESS.Mcp(proc, wire)
                peer.rpc('initialize', {})
                baseline = HELPER.document(peer.snapshot())
                if phase == 'headless':
                    for tool in TOOLS + ['editor_layout_reset']:
                        arguments = {} if tool == 'editor_layout_reset' else {'name': 'Modeling'}
                        response = peer.rpc('tools/call', {'name': tool, 'arguments': arguments})
                        assert response['isError'] is True
                        assert f'{tool} needs the editor: this host has no window' in response['content'][0]['text']
                elif phase == 'fresh-reset':
                    data = settings.read_bytes()
                    assert peer.op('editor_layout_reset')['panels'] == {'outline': True, 'timeline': True}
                    assert settings.read_bytes() == data
                    assert not {'Outline.visible', 'Timeline.visible'} & json.loads(data).keys()
                elif phase in ['sized-reset', 'sized-restarted']:
                    assert HELPER.state(peer)['windows']['nerdStats'] is True
                    if phase == 'sized-reset':
                        peer.op('editor_layout_reset')
                        default = drawn_layout(peer, settings, 'Baseline')
                        workspace(peer, 'apply', 'Sized')
                        workspace(peer, 'save', 'Before')
                        stored = json.loads(settings.read_text())['workspaces']
                        before = json.loads(stored['Before'])
                        assert before['sizes'] == {'outline': 777, 'timeline': 222}
                        assert peer.op('editor_layout_reset')['panels'] == {'outline': True, 'timeline': True}
                        assert json.loads(settings.read_text())['workspaces'] == stored
                        after = drawn_layout(peer, settings, 'Defaults', default['sizes'])
                        assert after['sizes']['outline'] != 777 and after['sizes']['timeline'] != 222
                        (run / 'sized-layout-baseline.json').write_text(json.dumps(default, indent=2) + '\n')
                        (run / 'sized-layout-before.json').write_text(json.dumps(before, indent=2) + '\n')
                        (run / 'sized-layout-after.json').write_text(json.dumps(after, indent=2) + '\n')
                        data, state = settings.read_bytes(), HELPER.state(peer)
                        for arguments in [{'extra': True}, [], None]:
                            response = peer.rpc('tools/call', {'name': 'editor_layout_reset', 'arguments': arguments})
                            assert response['isError'] is True
                            assert settings.read_bytes() == data and HELPER.state(peer) == state
                    assert HELPER.state(peer)['panels'] == {'outline': True, 'timeline': True}
                    assert HELPER.state(peer)['workspaces'] == ['Baseline', 'Before', 'Defaults', 'Sized']
                    assert peer.op('editor_layout_reset')['windows']['nerdStats'] is True
                elif phase == 'restarted':
                    assert HELPER.state(peer)['workspaces'] == ['Modeling']
                    assert HELPER.state(peer)['panels']['outline'] is False
                    assert workspace(peer, 'delete', 'Modeling')['workspaces'] == []
                    assert json.loads(settings.read_text())['workspaces'] == {}
                else:
                    assert HELPER.state(peer)['workspaces'] == []
                    peer.op('editor_panel_set', panel='outline', visible=False)
                    assert workspace(peer, 'save', ' Modeling ')['workspaces'] == ['Modeling']
                    peer.op('editor_panel_set', panel='outline', visible=True)
                    if phase.startswith('failure-'):
                        reset = phase == 'failure-reset'
                        if reset:
                            peer.op('editor_panel_set', panel='timeline', visible=False)
                        data = settings.read_bytes()
                        Path(str(settings) + '.swp').mkdir()
                        tool = 'editor_layout_reset' if reset else 'editor_workspace_' + phase.removeprefix('failure-')
                        name = 'New' if tool == TOOLS[0] else 'Modeling'
                        response = peer.rpc('tools/call', {'name': tool, 'arguments': {} if reset else {'name': name}})
                        assert response['isError'] is True
                        prefix = 'layout reset' if reset else 'workspace changed'
                        assert response['content'][0]['text'] == prefix + ' for this session, but the settings file could not be written'
                        current = HELPER.state(peer)
                        if reset:
                            assert current['panels'] == {'outline': True, 'timeline': True}
                        elif tool == TOOLS[0]:
                            assert current['workspaces'] == ['Modeling', 'New']
                        elif tool == TOOLS[1]:
                            assert current['panels']['outline'] is False
                        else:
                            assert current['workspaces'] == []
                        assert settings.read_bytes() == data
                    else:
                        assert workspace(peer, 'apply', 'Modeling')['panels']['outline'] is False
                        assert json.loads(settings.read_text())['Outline.visible'] is False
                        workspace(peer, 'save', 'Discard')
                        assert workspace(peer, 'delete', 'Discard')['workspaces'] == ['Modeling']
                        before, data = HELPER.state(peer), settings.read_bytes()
                        for tool, arguments in [(TOOLS[0], {'name': ''}), (TOOLS[0], {'name': 'a\nb'}),
                                                (TOOLS[0], {'name': 'x', 'extra': True}),
                                                (TOOLS[1], {'name': 'Unknown'}), (TOOLS[2], {'name': 'Unknown'})]:
                            response = peer.rpc('tools/call', {'name': tool, 'arguments': arguments})
                            assert response['isError'] is True
                            assert HELPER.state(peer) == before and settings.read_bytes() == data
                assert HELPER.document(peer.snapshot()) == baseline
                result['stages'].append({'phase': phase, 'documentUnchanged': True})
                result['cleanup'].append(HELPER.stop(proc))
                proc = None
        assert source.read_bytes() == original
        result.update(status='PASS', sourceSha256=HELPER.digest(source), settingsRestart='PASS', writeFailures=4,
                      layoutReset='PASS', nativeFloatingGeometry='NOT_RUN; Rust context test covers area reset')
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if proc is not None:
                result['cleanup'].append(HELPER.stop(proc))
            assert all(not row['running'] for row in result['cleanup'])
            assert {str(p.relative_to(ROOT)): HELPER.digest(p) for p in sorted(paths)} == hashes
            assert {str(p): HELPER.digest(p) for p in binaries} == binary_hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
