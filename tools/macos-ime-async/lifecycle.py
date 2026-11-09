from contextlib import contextmanager
from dataclasses import asdict, dataclass
import ctypes
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import pwd
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid

from build_bundle import build_client_bundle, build_server_bundle, run_logged

NATIVE_STEPS = frozenset({'snapshot_original', 'inspect_owned_before_install', 'register', 'inspect_registered',
                          'enable', 'verify_enabled', 'select', 'verify_selected', 'cleanup_current', 'restore',
                          'verify_restored', 'cleanup_owned_sources', 'disable', 'verify_disabled'})


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def atomic_json(path, value):
    temporary = path.with_name(path.name + '.new-' + uuid.uuid4().hex)
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(value, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def identity(path):
    require(not path.is_symlink(), 'owned directory became a symlink')
    info = path.stat()
    require(path.is_dir() and info.st_uid == os.getuid(), 'owned directory has wrong type or user')
    return {'device': info.st_dev, 'inode': info.st_ino, 'uid': info.st_uid}


def load_state(journal):
    require(not journal.is_symlink() and journal.is_file() and journal.stat().st_uid == os.getuid()
            and journal.stat().st_size <= 2097152, 'invalid lifecycle journal file')
    state = json.loads(journal.read_bytes())
    require(isinstance(state, dict), 'cleanup journal must contain an object')
    return state


@dataclass(frozen=True)
class Paths:
    run: str
    attempt: str
    head: str
    output: str
    temporary: str
    home: str
    installed: str
    bundle: str
    client_bundle: str

    @classmethod
    def environment(cls):
        run, attempt = os.environ.get('GITHUB_RUN_ID', ''), os.environ.get('GITHUB_RUN_ATTEMPT', '')
        require(re.fullmatch(r'[0-9]{1,20}', run) and re.fullmatch(r'[0-9]{1,20}', attempt), 'numeric run/attempt required')
        head = os.environ.get('GITHUB_SHA', '')
        require(re.fullmatch(r'[0-9a-f]{40}', head), 'exact GitHub head required')
        home = Path(pwd.getpwuid(os.getuid()).pw_dir)
        require(home == home.resolve() and Path.home().resolve() == home, 'unexpected runner home')
        workspace = Path(os.environ['GITHUB_WORKSPACE']).resolve(strict=True)
        temporary_root = Path(os.environ['RUNNER_TEMP']).resolve(strict=True)
        require(temporary_root.stat().st_uid == os.getuid(), 'runner temporary directory has wrong owner')
        suffix = f'r{run}-a{attempt}'
        installed = home / 'Library/Input Methods' / f'Soul2D-CI-IME-{run}-{attempt}.app'
        return cls(run, attempt, head, str(workspace / 'out' / f'macos-ime-async-{suffix}'),
                   str(temporary_root / f'soul2d-ime-async-{suffix}'), str(home), str(installed),
                   f'com.geekjapan.soul2d.ci.ime.r{run}.a{attempt}',
                   f'com.geekjapan.soul2d.ci.client.r{run}.a{attempt}')


@contextmanager
def locked(output):
    path = output / 'lifecycle.lock'
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        require(os.fstat(descriptor).st_uid == os.getuid(), 'lifecycle lock has wrong owner')
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(descriptor)


class Native:
    def __init__(self, paths):
        self.paths = paths
        self.output = Path(paths.output)
        self.temporary = Path(paths.temporary)
        self.helper = self.output / 'tis-helper'
        self.server = Path(paths.installed) / 'Contents/MacOS/S2DCIAsyncIME'
        self.client = self.temporary / f'Soul2D-CI-Client-{paths.run}-{paths.attempt}.app/Contents/MacOS/S2DCIAsyncClient'
        self.children = []
        self.cancelled = False

    def build(self):
        server = build_server_bundle(self.temporary / 'server-build', self.paths.run, self.paths.attempt)
        client = build_client_bundle(self.temporary, self.paths.run, self.paths.attempt)
        source = Path(__file__).with_name('tis.m')
        run_logged(['clang', '-fobjc-arc', '-Wall', '-Wextra', '-Werror', '-mmacosx-version-min=15.0',
                    str(source), '-framework', 'Foundation', '-framework', 'Carbon', '-o', str(self.helper)],
                   self.output / 'compile-tis.log', 90)
        run_logged(['/usr/bin/codesign', '--force', '--sign', '-', str(self.helper)], self.output / 'sign-tis.log', 30)
        run_logged(['/usr/bin/codesign', '--verify', '--strict', str(self.helper)], self.output / 'verify-tis.log', 30)
        return {'server': server, 'client': client, 'helperSha256': hashlib.sha256(self.helper.read_bytes()).hexdigest(),
                'helperSourceSha256': hashlib.sha256(source.read_bytes()).hexdigest()}

    def native(self, operation, *arguments):
        log = self.output / f'tis-{operation}-{uuid.uuid4().hex}.log'
        result = run_logged([str(self.helper), operation, *arguments], log, 10)
        require(len(result.stdout) <= 65536, 'oversized TIS response')
        value = json.loads(result.stdout)
        require(isinstance(value, dict) and value.get('operation') == operation, 'unexpected TIS response')
        if operation not in ['current', 'list']:
            require(type(value.get('os_status')) is int and value['os_status'] == 0, 'TIS mutation lacks successful OSStatus')
        value['log'] = log.name
        return value

    def archive_build_logs(self, ownership):
        count = 0
        if ownership is None:
            return {'copied': 0, 'scope': 'UNOWNED_TEMPORARY_DIRECTORY_NOT_READ'}
        require(not self.temporary.is_symlink(), 'build log directory became a symlink')
        if self.temporary.exists():
            require(identity(self.temporary) == ownership, 'build log directory ownership changed')
            for source in self.temporary.rglob('*.log*'):
                require(not source.is_symlink(), 'build log became a symlink')
                if not source.is_file():
                    continue
                target = self.output / 'build-logs' / source.relative_to(self.temporary)
                require(target == target.resolve() and not target.is_symlink(), 'symlinked build log archive target')
                target.parent.mkdir(parents=True, exist_ok=True)
                require(target.parent == target.parent.resolve(), 'symlinked build log archive directory')
                if target.exists():
                    require(target.is_file() and source.read_bytes() == target.read_bytes(), 'existing archived build log changed')
                else:
                    with target.open('xb') as stream:
                        stream.write(source.read_bytes())
                require(source.read_bytes() == target.read_bytes(), 'build log archive differs')
                count += 1
        return {'copied': count, 'scope': 'RAW_BUILD_LOGS_ONLY_NO_EXECUTABLES'}

    def create_install(self):
        destination = Path(self.paths.installed)
        home = Path(self.paths.home)
        for path in [home, home / 'Library', destination.parent]:
            require(not path.is_symlink() and path == path.resolve(), 'symlinked input-method parent')
            if path.exists():
                require(path.is_dir() and path.stat().st_uid == os.getuid(), 'unexpected input-method parent owner')
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.mkdir(mode=0o700, exist_ok=False)
        return identity(destination)

    def copy_install(self, artifacts, ownership):
        destination = Path(self.paths.installed)
        require(identity(destination) == ownership, 'installed directory ownership changed')
        shutil.copytree(artifacts['server']['bundle'], destination, dirs_exist_ok=True)
        require(hashlib.sha256(self.server.read_bytes()).hexdigest() == artifacts['server']['executableSha256'], 'installed executable changed')
        info = plistlib.loads((destination / 'Contents/Info.plist').read_bytes())
        require(info['CFBundleIdentifier'] == self.paths.bundle, 'installed bundle identity changed')
        return {'executableSha256': artifacts['server']['executableSha256']}

    def launch(self, executable, arguments, name):
        require(not self.cancelled, 'cancelled before launch')
        stdout = (self.output / f'{name}.jsonl').open('xb')
        stderr = (self.output / f'{name}.stderr').open('xb')
        try:
            child = subprocess.Popen([str(executable), *arguments], stdout=stdout, stderr=stderr, start_new_session=True)
        finally:
            stdout.close()
            stderr.close()
        self.children.append(child)
        return {'pid': child.pid, 'group': child.pid, 'executable': str(executable)}

    def server_ready(self):
        deadline = time.monotonic() + 10
        path = self.output / 'server.jsonl'
        while time.monotonic() < deadline:
            require(not self.cancelled, 'cancelled while awaiting server')
            if path.exists():
                data = path.read_bytes()
                require(len(data) <= 1048576, 'server readiness trace oversized')
                if b'\n' in data:
                    row = json.loads(data.split(b'\n', 1)[0])
                    require(row.get('event') == 'server_ready' and row.get('bundle') == self.paths.bundle, 'unexpected server readiness')
                    matches = self.processes([self.server])
                    require(len(matches) == 1 and row.get('pid') == matches[0]['pid'], 'missing or duplicate owned server')
                    return row
            time.sleep(0.01)
        raise TimeoutError('server readiness not observed')

    def client_result(self, process):
        child = next(child for child in self.children if child.pid == process['pid'])
        deadline = time.monotonic() + 120
        while child.poll() is None:
            require(not self.cancelled, 'cancelled while client runs')
            require(time.monotonic() < deadline, 'client deadline exceeded')
            time.sleep(0.02)
        atomic_json(self.output / 'client-process.json', {'pid': child.pid, 'returncode': child.returncode})
        data = (self.output / 'client.jsonl').read_bytes()
        require(len(data) <= 1048576 and data.endswith(b'\n'), 'client trace missing, incomplete or oversized')
        rows = [json.loads(line) for line in data.splitlines()]
        require(all(isinstance(row, dict) and row.get('seq') == i + 1 for i, row in enumerate(rows)), 'client trace sequence invalid')
        terminal = rows[-1] if rows else {}
        cleanup = [row for row in rows if row.get('event') == 'client_cleanup']
        require(child.returncode in [0, 3] and terminal.get('event') == 'experiment_complete' and len(cleanup) == 1,
                'client did not complete native experiment')
        require(terminal.get('refuted') is (child.returncode == 3) and cleanup[0].get('status') == child.returncode,
                'client status and terminal evidence disagree')
        require(cleanup[0].get('window_closed') is True and cleanup[0].get('keyboard_source_released') is True, 'client cleanup evidence missing')
        qualified = [row for row in rows if row.get('event') == 'qualified']
        cases = [row for row in rows if row.get('event') == 'case_complete']
        require(len(qualified) == 1 and qualified[0].get('case') == 'qualify', 'native qualification evidence missing')
        require(len(cases) == 2 and {row.get('case') for row in cases} == {'after_B_focus', 'during_B_handler'}, 'native ownership cases missing or duplicated')
        require(not any(row.get('event') == 'incomplete' for row in rows), 'native experiment contains incomplete evidence')
        if child.returncode == 0:
            require(all(row.get('refuted') is False and row.get('old_owner_observed') is True for row in cases), 'native success contradicts case evidence')
        return {'exit': child.returncode, 'terminal': terminal, 'traceSha256': hashlib.sha256(data).hexdigest(),
                'scope': 'NATIVE_FIXTURE_ONLY_NOT_PRODUCT_ACCEPTANCE'}

    @staticmethod
    def process_identity(pid):
        library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        library.proc_pidpath.restype = ctypes.c_int
        buffer = ctypes.create_string_buffer(4096)
        ctypes.set_errno(0)
        count = library.proc_pidpath(pid, buffer, len(buffer))
        if count == 0:
            if ctypes.get_errno() in [errno.ESRCH, errno.ENOENT]:
                return None
            raise RuntimeError(f'cannot inspect process {pid}')
        query = subprocess.run(['/bin/ps', '-p', str(pid), '-o', 'uid=', '-o', 'lstart='], capture_output=True, text=True, timeout=5)
        if query.returncode == 1 and not query.stdout.strip():
            return None
        require(query.returncode == 0 and len(query.stdout) <= 256, 'process identity query failed')
        uid, start = query.stdout.strip().split(maxsplit=1)
        return {'pid': pid, 'uid': int(uid), 'start': start, 'executable': os.fsdecode(buffer.value)}

    def processes(self, executables):
        paths = {str(path) for path in executables}
        result = subprocess.run(['/bin/ps', '-axo', 'uid=,pid='], capture_output=True, text=True, timeout=5)
        require(result.returncode == 0 and len(result.stdout) <= 2097152, 'process inventory failed')
        matches = []
        for line in result.stdout.splitlines():
            uid, pid = map(int, line.split())
            if uid != os.getuid():
                continue
            item = self.process_identity(pid)
            if item and item['uid'] == os.getuid() and item['executable'] in paths:
                matches.append(item)
        return matches

    def stop(self, executables):
        if not executables:
            return {'absent': True, 'observed': [], 'reaped': []}
        observed = []
        for termination in [signal.SIGTERM, signal.SIGKILL]:
            current = self.processes(executables)
            observed.extend(current)
            for item in current:
                if self.process_identity(item['pid']) == item:
                    try:
                        os.kill(item['pid'], termination)
                    except ProcessLookupError:
                        pass
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                for child in self.children:
                    child.poll()
                if not self.processes(executables):
                    reaped = []
                    for child in self.children:
                        if child.args[0] in {str(path) for path in executables}:
                            reaped.append({'pid': child.pid, 'exit': child.wait(timeout=1)})
                    return {'absent': True, 'observed': observed, 'reaped': reaped}
                time.sleep(0.02)
        raise RuntimeError('owned executable processes remain after termination')

    def verify_helper(self, artifacts):
        expected = artifacts.get('helperSha256')
        require(isinstance(expected, str) and re.fullmatch(r'[0-9a-f]{64}', expected), 'helper ownership hash missing')
        require(not self.helper.is_symlink(), 'helper executable became a symlink')
        if self.helper.exists():
            require(self.helper.is_file() and self.helper.stat().st_uid == os.getuid()
                    and hashlib.sha256(self.helper.read_bytes()).hexdigest() == expected, 'helper ownership changed')
        return [self.helper]

    def remove(self, path, ownership):
        if not path.exists() and not path.is_symlink():
            return {'absent': True}
        require(identity(path) == ownership, 'refusing deletion of changed owned directory')
        shutil.rmtree(path)
        require(not path.exists() and not path.is_symlink(), 'owned directory removal incomplete')
        return {'absent': True}


class Lifecycle:
    def __init__(self, paths, backend, state=None):
        self.paths, self.backend = paths, backend
        self.file = Path(paths.output) / 'journal.json'
        require(state is None or isinstance(state, dict), 'lifecycle journal is not an object')
        self.state = state if state is not None else {'version': 1, 'identity': asdict(paths), 'original': None, 'artifacts': None,
                               'temporaryOwnership': None, 'installOwnership': None, 'source': None,
                               'operations': [], 'nativeResult': None, 'cleanup': None}
        require(type(self.state.get('version')) is int and self.state['version'] == 1
                and self.state.get('identity') == asdict(paths), 'foreign lifecycle journal')
        required = {'version', 'identity', 'original', 'artifacts', 'temporaryOwnership', 'installOwnership',
                    'source', 'operations', 'nativeResult', 'cleanup'}
        require(required <= self.state.keys() <= required | {'runError'}, 'malformed lifecycle journal fields')
        operations = self.state['operations']
        require(isinstance(operations, list) and len(operations) <= 4096, 'invalid lifecycle operation ledger')
        for index, operation in enumerate(operations):
            require(isinstance(operation, dict) and type(operation.get('seq')) is int and operation['seq'] == index + 1
                    and isinstance(operation.get('operation'), str) and operation.get('status') in ['STARTED', 'OBSERVED', 'UNKNOWN_EFFECT'],
                    'invalid lifecycle operation record')
        fields = {'create_temporary': 'temporaryOwnership', 'build': 'artifacts',
                  'snapshot_original': 'original', 'create_install': 'installOwnership'}
        for operation, field in fields.items():
            records = [item for item in operations if item['operation'] == operation and item['status'] == 'OBSERVED']
            require(len(records) <= 1, 'initial lifecycle operation was replayed')
            if records:
                result = records[0]['result']['source'] if field == 'original' else records[0]['result']
                require(self.state[field] is None or self.state[field] == result, 'immutable lifecycle snapshot changed')
                self.state[field] = result
            else:
                require(self.state[field] is None, 'lifecycle field has no observed receipt')
        if self.state['original']:
            original = self.state['original']
            require(isinstance(original, dict) and isinstance(original.get('id'), str) and 0 < len(original['id']) <= 512
                    and original.get('enabled') is True and original.get('selected') is True, 'invalid original source snapshot')
        for field in ['installOwnership', 'temporaryOwnership']:
            ownership = self.state[field]
            require(ownership is None or (isinstance(ownership, dict) and set(ownership) == {'device', 'inode', 'uid'}
                    and all(type(value) is int and value >= 0 for value in ownership.values()) and ownership['uid'] == os.getuid()),
                    'invalid directory ownership receipt')
        if any(item['operation'] in ['copy_install', 'register', 'enable', 'select', 'launch_server', 'launch_client'] for item in operations):
            require(self.state['original'] is not None and self.state['installOwnership'] is not None and self.state['artifacts'] is not None,
                    'configuration intent lacks initial ownership/snapshot evidence')
        self.cleaning = False

    def save(self):
        atomic_json(self.file, self.state)

    def act(self, operation, callback):
        require(self.cleaning or not self.backend.cancelled, 'cancelled before lifecycle operation')
        require(len(self.state['operations']) < 4096, 'lifecycle operation limit')
        entry = {'seq': len(self.state['operations']) + 1, 'operation': operation, 'status': 'STARTED'}
        self.state['operations'].append(entry)
        self.save()
        try:
            value = callback()
        except BaseException as error:
            entry.update(status='UNKNOWN_EFFECT', error=f'{type(error).__name__}: {error}')
            self.save()
            raise
        entry.update(status='OBSERVED', result=json.loads(json.dumps(value, allow_nan=False)))
        self.save()
        return value

    def run(self):
        require(not self.state['operations'], 'run is not replayable; use cleanup')
        temporary = Path(self.paths.temporary)
        self.state['temporaryOwnership'] = self.act('create_temporary', lambda: self.create_temporary(temporary))
        self.save()
        self.state['artifacts'] = self.act('build', self.backend.build)
        self.save()
        original = self.act('snapshot_original', lambda: self.backend.native('current'))['source']
        require(original.get('id') and original.get('enabled') is True and original.get('selected') is True, 'original source is not restorable')
        self.state['original'] = original
        self.save()
        require(not self.act('inspect_owned_before_install', lambda: self.backend.native('list', self.paths.bundle))['sources'], 'owned source already exists')
        self.state['installOwnership'] = self.act('create_install', self.backend.create_install)
        self.save()
        self.act('copy_install', lambda: self.backend.copy_install(self.state['artifacts'], self.state['installOwnership']))
        self.act('launch_server', lambda: self.backend.launch(self.backend.server, [self.paths.output, self.paths.client_bundle], 'server'))
        self.act('server_ready', self.backend.server_ready)
        self.act('register', lambda: self.backend.native('register', self.paths.installed))
        sources = self.act('inspect_registered', lambda: self.backend.native('list', self.paths.bundle))['sources']
        require(len(sources) == 1 and sources[0].get('selectable') is True and sources[0].get('id'), 'owned source missing or ambiguous')
        self.state['source'] = sources[0]['id']
        self.save()
        self.act('enable', lambda: self.backend.native('enable', self.paths.bundle, self.state['source']))
        enabled = self.act('verify_enabled', lambda: self.backend.native('list', self.paths.bundle))['sources']
        require(len(enabled) == 1 and enabled[0].get('id') == self.state['source'] and enabled[0].get('enabled') is True, 'owned source not enabled')
        self.act('select', lambda: self.backend.native('select', self.paths.bundle, self.state['source']))
        selected = self.act('verify_selected', lambda: self.backend.native('current'))['source']
        require(selected.get('id') == self.state['source'] and selected.get('selected') is True, 'owned source not selected')
        process = self.act('launch_client', lambda: self.backend.launch(self.backend.client,
            [self.paths.output, str(Path(self.paths.output) / 'server.jsonl'), self.state['source']], 'client'))
        self.state['nativeResult'] = self.act('client_terminal', lambda: self.backend.client_result(process))
        self.save()

    @staticmethod
    def create_temporary(path):
        path.mkdir(mode=0o700, exist_ok=False)
        return identity(path)

    def cleanup(self):
        self.cleaning = True
        facts = {'restoration': 'NOT_ATTEMPTED', 'disable': 'NOT_ATTEMPTED', 'processesAbsent': False,
                 'bundleAbsent': False, 'temporaryAbsent': False, 'buildLogsArchived': False,
                 'errors': [], 'registrationCacheErased': 'NOT_PROVED'}
        try:
            helper_paths = []
            if self.state['artifacts'] and self.state['temporaryOwnership'] and any(
                    item['operation'] in NATIVE_STEPS for item in self.state['operations']):
                helper_paths = self.backend.verify_helper(self.state['artifacts'])
            self.act('stop_helpers', lambda: self.backend.stop(helper_paths))
            configured = bool(self.state['installOwnership']) and any(
                item['operation'] in ['copy_install', 'register', 'enable', 'select'] for item in self.state['operations'])
            if configured:
                require(self.state['original'] is not None, 'configuration has no original source snapshot')
                current = self.act('cleanup_current', lambda: self.backend.native('current'))['source']
                if current.get('id') != self.state['original']['id']:
                    self.act('restore', lambda: self.backend.native('restore', self.state['original']['id']))
                restored = self.act('verify_restored', lambda: self.backend.native('current'))['source']
                require(restored.get('id') == self.state['original']['id'] and restored.get('selected') is True, 'original source was not restored')
                facts['restoration'] = 'VERIFIED'
                sources = self.act('cleanup_owned_sources', lambda: self.backend.native('list', self.paths.bundle))['sources']
                for source in sources:
                    if source.get('enabled') is True:
                        self.act('disable', lambda source=source: self.backend.native('disable', self.paths.bundle, source['id']))
                remaining = self.act('verify_disabled', lambda: self.backend.native('list', self.paths.bundle))['sources']
                require(all(source.get('enabled') is False for source in remaining), 'owned source not disabled')
                facts['disable'] = 'VERIFIED'
            else:
                facts['restoration'] = facts['disable'] = 'NOT_NEEDED_NO_OWNED_CONFIGURATION'
        except BaseException as error:
            facts['errors'].append(f'{type(error).__name__}: {error}')
        try:
            applications = []
            intents = {item['operation'] for item in self.state['operations']}
            for field, path, executable, eligible in [
                ('installOwnership', Path(self.paths.installed), self.backend.server,
                 bool(intents & {'copy_install', 'launch_server', 'register', 'enable', 'select'})),
                ('temporaryOwnership', Path(self.paths.temporary), self.backend.client, 'launch_client' in intents),
            ]:
                if self.state[field] and eligible:
                    require(not path.is_symlink(), 'owned executable directory became a symlink')
                    if path.exists():
                        require(identity(path) == self.state[field], 'owned executable directory identity changed')
                    applications.append(executable)
            self.act('stop_apps', lambda: self.backend.stop(applications))
            facts['processesAbsent'] = True
        except BaseException as error:
            facts['errors'].append(f'{type(error).__name__}: {error}')
        try:
            self.act('archive_build_logs', lambda: self.backend.archive_build_logs(self.state['temporaryOwnership']))
            facts['buildLogsArchived'] = True
        except BaseException as error:
            facts['errors'].append(f'{type(error).__name__}: {error}')
        safe = facts['restoration'] in ['VERIFIED', 'NOT_NEEDED_NO_OWNED_CONFIGURATION'] and facts['disable'] in [
            'VERIFIED', 'NOT_NEEDED_NO_OWNED_CONFIGURATION'] and facts['processesAbsent'] and facts['buildLogsArchived']
        if safe:
            try:
                installed = Path(self.paths.installed)
                if self.state['installOwnership']:
                    self.act('remove_bundle', lambda: self.backend.remove(installed, self.state['installOwnership']))
                require(not installed.exists() and not installed.is_symlink(), 'unproven bundle ownership; preserving directory')
                facts['bundleAbsent'] = True
                temporary = Path(self.paths.temporary)
                if self.state['temporaryOwnership']:
                    self.act('remove_temporary', lambda: self.backend.remove(temporary, self.state['temporaryOwnership']))
                require(not temporary.exists() and not temporary.is_symlink(), 'unproven temporary ownership; preserving directory')
                facts['temporaryAbsent'] = True
            except BaseException as error:
                facts['errors'].append(f'{type(error).__name__}: {error}')
        facts['complete'] = safe and facts['bundleAbsent'] and facts['temporaryAbsent'] and not facts['errors']
        self.state['cleanup'] = facts
        self.save()
        self.cleaning = False
        return facts


def supported():
    return (os.environ.get('GITHUB_ACTIONS') == 'true' and os.environ.get('RUNNER_OS') == 'macOS'
            and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted' and platform.system() == 'Darwin'
            and platform.machine() == 'arm64' and os.environ.get('GITHUB_REPOSITORY') == 'geekjapan/Soul2D'
            and os.environ.get('GITHUB_REF') == 'refs/heads/main')


def main():
    if not supported():
        print('Hosted-CI-only lifecycle; local configuration access disabled', file=sys.stderr)
        return 77
    require(len(sys.argv) == 2 and sys.argv[1] in ['run', 'cleanup'], 'usage: lifecycle.py run|cleanup')
    paths = Paths.environment()
    output = Path(paths.output)
    require(not output.is_symlink() and output == output.resolve(), 'unexpected output path')
    if sys.argv[1] == 'run':
        output.mkdir(parents=True, exist_ok=False)
    require(output.is_dir() and output.stat().st_uid == os.getuid(), 'lifecycle output missing or foreign')
    with locked(output):
        journal = output / 'journal.json'
        state = None
        if sys.argv[1] == 'cleanup':
            state = load_state(journal)
        backend = Native(paths)
        lifecycle = Lifecycle(paths, backend, state)
        for signum in [signal.SIGINT, signal.SIGTERM]:
            signal.signal(signum, lambda received, frame: setattr(backend, 'cancelled', True))
        try:
            if sys.argv[1] == 'run':
                lifecycle.save()
                lifecycle.run()
        except BaseException as error:
            lifecycle.state['runError'] = f'{type(error).__name__}: {error}'
            lifecycle.save()
        finally:
            cleanup = lifecycle.cleanup()
        native = lifecycle.state['nativeResult']
        accepted = cleanup['complete'] if sys.argv[1] == 'cleanup' else (
            cleanup['complete'] and native and native['exit'] == 0 and not lifecycle.state.get('runError'))
        status = 0 if accepted else 1
        print(json.dumps({'status': status, 'native': native, 'cleanup': cleanup,
                          'scope': 'OWNED_NATIVE_IME_EXPERIMENT_NOT_PRODUCT_ACCEPTANCE'}, indent=2))
        return status


if __name__ == '__main__':
    sys.exit(main())
