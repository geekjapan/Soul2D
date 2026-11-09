import copy
from contextlib import redirect_stdout
import hashlib
import importlib.util
import itertools
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

import lifecycle as module


class InjectedFailure(RuntimeError):
    pass


class Fake(module.Native):
    def __init__(self, paths):
        super().__init__(paths)
        self.current = 'original'
        self.registered = self.enabled = False
        self.running = {}
        self.calls = []
        self.failure = None
        self.source_count = 1
        self.native_exit = 0

    def step(self, name, action):
        self.calls.append(name)
        failure = self.failure
        if failure and failure == (name, 'before'):
            self.failure = None
            raise InjectedFailure(name + ' before effect')
        value = action()
        if failure and failure == (name, 'after'):
            self.failure = None
            raise InjectedFailure(name + ' after effect')
        return value

    def build(self):
        def effect():
            self.helper.write_bytes(b'unit-fixture-not-executable')
            return {'helperSha256': hashlib.sha256(self.helper.read_bytes()).hexdigest(),
                    'server': {'bundle': str(self.temporary / 'server.app'), 'executableSha256': '0' * 64},
                    'client': {'bundle': str(self.client.parent.parent)}, 'scope': 'SIMULATED_ONLY'}
        return self.step('build', effect)

    def create_install(self):
        saved = json.loads((self.output / 'journal.json').read_text())
        assert saved['original']['id'] == 'original'
        return self.step('create_install', lambda: super(Fake, self).create_install())

    def copy_install(self, artifacts, ownership):
        def effect():
            assert module.identity(Path(self.paths.installed)) == ownership
            (Path(self.paths.installed) / 'owned-copy').write_bytes(b'fixture')
            return {'copied': True}
        return self.step('copy_install', effect)

    def native(self, operation, *arguments):
        def effect():
            if operation in ['register', 'enable', 'select']:
                saved = json.loads((self.output / 'journal.json').read_text())
                assert saved['original']['id'] == 'original' and saved['operations'][-1]['status'] == 'STARTED'
            if operation == 'register':
                assert arguments == (self.paths.installed,)
                self.registered = True
            elif operation == 'enable':
                self.enabled = True
            elif operation == 'select':
                assert self.enabled
                self.current = 'owned-0'
            elif operation == 'restore':
                assert arguments == ('original',)
                self.current = 'original'
            elif operation == 'disable':
                assert self.current == 'original'
                self.enabled = False
            if operation == 'current':
                return {'operation': operation, 'source': {'id': self.current, 'selected': True, 'enabled': True, 'selectable': True}}
            if operation == 'list':
                assert arguments == (self.paths.bundle,)
                return {'operation': operation, 'sources': [
                    {'id': f'owned-{i}', 'selected': self.current == f'owned-{i}', 'enabled': self.enabled, 'selectable': True}
                    for i in range(self.source_count if self.registered else 0)]}
            return {'operation': operation, 'os_status': 0}
        return self.step(operation, effect)

    def launch(self, executable, arguments, name):
        def effect():
            self.running[str(executable)] = 1000 + len(self.running)
            return {'pid': self.running[str(executable)], 'group': self.running[str(executable)], 'executable': str(executable)}
        return self.step('launch_' + name, effect)

    def server_ready(self):
        return self.step('server_ready', lambda: {'event': 'server_ready', 'bundle': self.paths.bundle})

    def client_result(self, process):
        return self.step('client_terminal', lambda: {'exit': self.native_exit, 'scope': 'SIMULATED_ONLY'})

    def stop(self, executables):
        def effect():
            for executable in executables:
                self.running.pop(str(executable), None)
            return {'absent': True, 'scope': 'SIMULATED_ONLY'}
        return self.step('stop_helpers' if executables == [self.helper] else 'stop_apps', effect)

    def archive_build_logs(self, ownership):
        return self.step('archive_build_logs', lambda: super(Fake, self).archive_build_logs(ownership))

    def remove(self, path, ownership):
        def effect():
            assert self.current == 'original' and not self.enabled
            return super(Fake, self).remove(path, ownership)
        return self.step('remove_bundle' if str(path) == self.paths.installed else 'remove_temporary', effect)


def setup(directory):
    base = Path(directory).resolve()
    output = base / 'out'
    output.mkdir()
    home = base / 'home'
    home.mkdir()
    paths = module.Paths('1', '1', '0' * 40, str(output), str(base / 'temporary'), str(home),
                         str(home / 'Library/Input Methods/owned.app'), 'fixture.ime', 'fixture.client')
    backend = Fake(paths)
    flow = module.Lifecycle(paths, backend)
    flow.save()
    return paths, backend, flow


def resumed(paths, backend):
    state = json.loads((Path(paths.output) / 'journal.json').read_text())
    return module.Lifecycle(paths, backend, state)


results = {}
with tempfile.TemporaryDirectory(prefix='soul2d-ime-flow-') as directory:
    paths, backend, flow = setup(directory)
    flow.run()
    assert flow.state['nativeResult']['exit'] == 0
    assert flow.cleanup()['complete'] and resumed(paths, backend).cleanup()['complete']
    assert backend.current == 'original' and not backend.enabled and not backend.running
    results['normalAndRepeatedRecovery'] = 'PASS'

forward = ['build', 'create_install', 'copy_install', 'launch_server', 'server_ready', 'register', 'enable', 'select', 'launch_client', 'client_terminal']
retained = 0
for operation in forward:
    for point in ['before', 'after']:
        with tempfile.TemporaryDirectory(prefix='soul2d-ime-injected-') as directory:
            paths, backend, flow = setup(directory)
            backend.failure = (operation, point)
            try:
                flow.run()
            except InjectedFailure:
                pass
            else:
                raise AssertionError((operation, point, 'injection did not fire'))
            first = resumed(paths, backend).cleanup()
            second = resumed(paths, backend).cleanup()
            assert backend.current == 'original' and not backend.enabled and not backend.running
            if operation == 'create_install' and point == 'after':
                assert not first['complete'] and not second['complete'] and Path(paths.installed).exists()
                retained += 1
            else:
                assert first['complete'] and second['complete'], (operation, point, first, second)
            assert backend.calls.count('register') <= 1
results['forwardFailurePoints'] = {'passed': len(forward) * 2, 'unprovenPartialBundlePreserved': retained}

for operation in ['restore', 'disable', 'stop_helpers', 'stop_apps', 'archive_build_logs', 'remove_bundle', 'remove_temporary']:
    for point in ['before', 'after']:
        with tempfile.TemporaryDirectory(prefix='soul2d-ime-cleanup-injected-') as directory:
            paths, backend, flow = setup(directory)
            flow.run()
            backend.failure = (operation, point)
            assert not flow.cleanup()['complete']
            recovered = resumed(paths, backend).cleanup()
            assert recovered['complete'], (operation, point, recovered)
            assert backend.current == 'original' and not backend.enabled and not backend.running
results['cleanupFailurePoints'] = 14

for count in [0, 2]:
    with tempfile.TemporaryDirectory(prefix='soul2d-ime-source-count-') as directory:
        paths, backend, flow = setup(directory)
        backend.source_count = count
        try:
            flow.run()
        except RuntimeError as error:
            assert 'missing or ambiguous' in str(error)
        else:
            raise AssertionError('invalid source count accepted')
        assert flow.cleanup()['complete'] and 'enable' not in backend.calls and 'select' not in backend.calls
results['zeroMultipleSources'] = 2

with tempfile.TemporaryDirectory(prefix='soul2d-ime-cancel-') as directory:
    paths, backend, flow = setup(directory)
    flow.run()
    backend.cancelled = True
    assert flow.cleanup()['complete'] and flow.state['nativeResult']['exit'] == 0
    results['cleanupIgnoresForwardCancellation'] = 'PASS'

with tempfile.TemporaryDirectory(prefix='soul2d-ime-foreign-source-') as directory:
    paths, backend, flow = setup(directory)
    backend.registered = backend.enabled = True
    preexisting = {str(backend.server): 44444}
    backend.running.update(preexisting)
    try:
        flow.run()
    except RuntimeError as error:
        assert 'already exists' in str(error)
    else:
        raise AssertionError('preexisting source accepted')
    flow.cleanup()
    assert backend.enabled and not any(name in backend.calls for name in ['restore', 'disable', 'select'])
    assert backend.running == preexisting, 'preexisting server process was terminated'
    results['preexistingSourceUntouched'] = 'PASS'

with tempfile.TemporaryDirectory(prefix='soul2d-ime-foreign-temp-process-') as directory:
    paths, backend, flow = setup(directory)
    backend.temporary.mkdir()
    (backend.temporary / 'foreign.log').write_bytes(b'foreign fixture log must not be archived')
    preexisting = {str(backend.client): 55555, str(backend.helper): 66666}
    backend.running.update(preexisting)
    try:
        flow.run()
    except FileExistsError:
        pass
    else:
        raise AssertionError('preexisting temporary directory accepted')
    flow.cleanup()
    assert backend.running == preexisting, 'unowned client/helper process was terminated'
    assert not (backend.output / 'build-logs/foreign.log').exists(), 'unowned temporary log was archived'
    results['preexistingClientHelperUntouched'] = 'PASS'

for field in ['installed', 'temporary']:
    with tempfile.TemporaryDirectory(prefix='soul2d-ime-replaced-owned-directory-') as directory:
        paths, backend, flow = setup(directory)
        flow.run()
        path = Path(getattr(paths, field))
        path.rename(path.with_name(path.name + '-retained-original'))
        path.mkdir()
        (path / 'foreign.log').write_bytes(b'foreign directory bytes')
        running = dict(backend.running)
        result = flow.cleanup()
        assert not result['complete'] and backend.running == running and path.exists()
        assert not (backend.output / 'build-logs/foreign.log').exists()
results['replacedOwnedDirectoriesPreservedBeforeSignalsOrArchive'] = 2

with tempfile.TemporaryDirectory(prefix='soul2d-ime-replaced-helper-') as directory:
    paths, backend, flow = setup(directory)
    flow.run()
    backend.helper.write_bytes(b'changed helper must not execute')
    backend.running[str(backend.helper)] = 77777
    before = backend.calls.count('restore')
    assert not flow.cleanup()['complete']
    assert backend.running == {str(backend.helper): 77777} and backend.calls.count('restore') == before
    results['changedHelperPreservedWithoutExecutionOrSignals'] = 'PASS'

with tempfile.TemporaryDirectory(prefix='soul2d-ime-journal-check-') as directory:
    paths, backend, flow = setup(directory)
    flow.run()
    saved = copy.deepcopy(flow.state)
    invalid = [None, [], {}]
    changed = copy.deepcopy(saved)
    changed['original']['id'] = 'changed'
    invalid.append(changed)
    changed = copy.deepcopy(saved)
    changed['identity']['home'] = '/foreign'
    invalid.append(changed)
    changed = copy.deepcopy(saved)
    changed['operations'][0]['seq'] = True
    invalid.append(changed)
    changed = copy.deepcopy(saved)
    changed['operations'][0]['status'] = 'PASS'
    invalid.append(changed)
    changed = copy.deepcopy(saved)
    changed['version'] = True
    invalid.append(changed)
    before_calls = len(backend.calls)
    for value in invalid[1:]:
        try:
            module.Lifecycle(paths, backend, value)
        except (RuntimeError, KeyError, TypeError):
            pass
        else:
            raise AssertionError('corrupt journal accepted')
    assert len(backend.calls) == before_calls
    assert flow.cleanup()['complete']
    results['malformedJournalRejectedBeforeNativeCalls'] = len(invalid) - 1

with tempfile.TemporaryDirectory(prefix='soul2d-ime-null-cleanup-entry-') as directory:
    paths, backend, flow = setup(directory)
    journal = Path(paths.output) / 'journal.json'
    payloads = [b'null', b'false', b'[]', b'"text"', b'3', b'{"truncated":']
    for payload in payloads:
        journal.write_bytes(payload)
        captured = io.StringIO()
        rejected = False
        with patch.object(module, 'supported', return_value=True), patch.object(module.Paths, 'environment', return_value=paths), patch.object(
                module, 'Native', return_value=backend) as factory, patch.object(module.sys, 'argv', ['lifecycle.py', 'cleanup']), patch.object(
                module.signal, 'signal'), redirect_stdout(captured):
            try:
                module.main()
            except (RuntimeError, ValueError):
                rejected = True
        if not rejected:
            (Path(__file__).resolve().parent / 'lifecycle-null-before-v1.json').write_text(json.dumps({
                'rejected': False, 'backendConstructed': factory.called, 'journalChanged': journal.read_bytes() != payload,
                'stdout': captured.getvalue(), 'scope': 'PURE_FAKE_BACKEND_NO_TIS'}, indent=2) + '\n')
            raise AssertionError('null cleanup journal accepted')
        assert not factory.called and journal.read_bytes() == payload
    results['nullCleanupEntrypointPreservesBytesBeforeBackend'] = 'PASS'
    results['nonObjectOrTruncatedCleanupJournalsRejected'] = len(payloads)

with tempfile.TemporaryDirectory(prefix='soul2d-ime-lock-check-') as directory:
    output = Path(directory)
    with module.locked(output):
        try:
            with module.locked(output):
                raise AssertionError('concurrent writer acquired lifecycle lock')
        except BlockingIOError:
            pass
    module.atomic_json(output / 'journal.json', {'one': 1})
    (output / 'journal.json.new-old-crash').write_bytes(b'partial')
    module.atomic_json(output / 'journal.json', {'two': 2})
    assert json.loads((output / 'journal.json').read_text()) == {'two': 2}
    results['singleWriterAndAbandonedTemporaryJournal'] = 'PASS'

with tempfile.TemporaryDirectory(prefix='soul2d-ime-process-model-') as directory:
    paths, fake, flow = setup(directory)
    backend = module.Native(paths)
    item = {'pid': 12345, 'uid': os.getuid(), 'executable': str(backend.server), 'start': 'first'}
    duplicate = {**item, 'pid': 23456, 'start': 'second'}
    with patch.object(backend, 'processes', side_effect=[[item, duplicate], []]), patch.object(
            backend, 'process_identity', side_effect=lambda pid: item if pid == item['pid'] else duplicate), patch.object(module.os, 'kill') as kill:
        assert backend.stop([backend.server])['absent']
        assert [call.args[0] for call in kill.call_args_list] == [12345, 23456]
    with patch.object(backend, 'processes', side_effect=[[item], []]), patch.object(
            backend, 'process_identity', return_value={**item, 'start': 'reused', 'executable': '/foreign'}), patch.object(module.os, 'kill') as kill:
        assert backend.stop([backend.server])['absent'] and not kill.called
    with patch.object(backend, 'processes', side_effect=RuntimeError('inspection unavailable')), patch.object(module.os, 'kill') as kill:
        try:
            backend.stop([backend.server])
        except RuntimeError:
            pass
        else:
            raise AssertionError('inspection failure treated as absence')
        assert not kill.called
    with patch.object(backend, 'processes', return_value=[item]), patch.object(backend, 'process_identity', return_value=item), patch.object(
            module.os, 'kill') as kill, patch.object(module.time, 'monotonic', side_effect=itertools.count(0, 10)):
        try:
            backend.stop([backend.server])
        except RuntimeError as error:
            assert 'remain' in str(error)
        else:
            raise AssertionError('termination timeout accepted')
        assert [call.args[1] for call in kill.call_args_list] == [signal.SIGTERM, signal.SIGKILL]
    results['mockedProcessOwnershipCases'] = 4

with tempfile.TemporaryDirectory(prefix='soul2d-ime-terminal-model-') as directory:
    paths, fake, flow = setup(directory)
    backend = module.Native(paths)
    child = SimpleNamespace(pid=77, returncode=0, poll=lambda: 0)
    backend.children = [child]
    valid = [
        {'event': 'qualified', 'case': 'qualify'},
        {'event': 'case_complete', 'case': 'after_B_focus', 'refuted': False, 'old_owner_observed': True},
        {'event': 'case_complete', 'case': 'during_B_handler', 'refuted': False, 'old_owner_observed': True},
        {'event': 'client_cleanup', 'status': 0, 'window_closed': True, 'keyboard_source_released': True},
        {'event': 'experiment_complete', 'refuted': False},
    ]

    def trace(rows):
        (backend.output / 'client.jsonl').write_text(''.join(json.dumps({**row, 'seq': index + 1}) + '\n' for index, row in enumerate(rows)))

    trace(valid)
    assert backend.client_result({'pid': 77})['exit'] == 0
    variants = [valid[1:], valid[:1] + valid[2:], valid[:3] + valid[4:]]
    for index, field, value in [(1, 'case', 'during_B_handler'), (1, 'old_owner_observed', False),
                                 (3, 'status', 3), (3, 'window_closed', False), (4, 'refuted', True)]:
        rows = copy.deepcopy(valid)
        rows[index][field] = value
        variants.append(rows)
    variants.append(valid[:-1] + [{'event': 'incomplete'}] + valid[-1:])
    for rows in variants:
        trace(rows)
        try:
            backend.client_result({'pid': 77})
        except RuntimeError:
            pass
        else:
            raise AssertionError('incomplete native terminal evidence accepted')
    trace(valid)
    child.returncode = 2
    try:
        backend.client_result({'pid': 77})
    except RuntimeError:
        pass
    else:
        raise AssertionError('client infrastructure failure accepted')
    assert json.loads((backend.output / 'client-process.json').read_text())['returncode'] == 2
    results['nativeTerminalParserNegativeCases'] = len(variants) + 1

with tempfile.TemporaryDirectory(prefix='soul2d-ime-archive-boundary-') as directory:
    paths, backend, flow = setup(directory)
    flow.run()
    source = backend.temporary / 'compile.log.stderr'
    source.write_bytes(b'fixture compiler failure bytes')
    assert flow.cleanup()['complete']
    assert (backend.output / 'build-logs/compile.log.stderr').read_bytes() == b'fixture compiler failure bytes'
    assert not backend.temporary.exists()
    results['buildFailureLogSurvivesTemporaryRemoval'] = 'PASS'

with tempfile.TemporaryDirectory(prefix='soul2d-ime-archive-symlink-') as directory:
    paths, backend, flow = setup(directory)
    backend.temporary.mkdir()
    (backend.temporary / 'compile.log').write_bytes(b'new build output')
    archive = backend.output / 'build-logs'
    archive.mkdir()
    unrelated = Path(directory) / 'unrelated-fixture'
    unrelated.write_bytes(b'preserve unrelated bytes')
    (archive / 'compile.log').symlink_to(unrelated)
    try:
        backend.archive_build_logs(module.identity(backend.temporary))
    except RuntimeError:
        pass
    else:
        raise AssertionError('archive followed destination symlink')
    assert unrelated.read_bytes() == b'preserve unrelated bytes'
    results['archiveSymlinkDestinationRefused'] = 'PASS'

root = Path(__file__).resolve().parent
print(json.dumps({'scope': 'PURE_SIMULATED_LIFECYCLE_AND_OWNED_TEMP_FILES_NO_TIS_GUI_OR_NATIVE_EXECUTION',
    'lifecycleSha256': hashlib.sha256((root / 'lifecycle.py').read_bytes()).hexdigest(), 'cases': results}, indent=2))
