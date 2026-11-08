import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import tempfile
import time


def run_owned(command, stdout, stderr, timeout):
    child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
    timed_out = False
    try:
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
    finally:
        absent = False
        for termination in [signal.SIGTERM, signal.SIGKILL]:
            try:
                os.killpg(child.pid, termination)
            except ProcessLookupError:
                absent = True
                break
            deadline = time.monotonic() + 3
            while True:
                child.poll()
                try:
                    os.killpg(child.pid, 0)
                except ProcessLookupError:
                    absent = True
                    break
                except PermissionError:
                    pass
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.01)
            if absent:
                break
        child.wait(timeout=3)
    return {'returncode': child.returncode, 'timedOut': timed_out, 'ownedGroupAbsent': absent}


def interrupted(signum, frame):
    raise KeyboardInterrupt(f'signal {signum}')


def main():
    if (os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_OS') != 'macOS'
            or platform.system() != 'Darwin' or platform.machine() != 'arm64'):
        print('CI-only macOS ARM64 native fixture; local GUI execution is disabled', file=sys.stderr)
        return 77
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    root = Path(__file__).resolve().parent.parent
    source = root / 'tools/macos-ime-context-probe.m'
    output = root / 'out/macos-ime-context'
    output.mkdir(parents=True, exist_ok=False)
    receipt = {'scope': 'NATIVE_CLIENT_QUEUE_PROTOTYPE_NOT_PRODUCT_ACCEPTANCE',
               'head': os.environ.get('GITHUB_SHA'), 'run': os.environ.get('GITHUB_RUN_ID'),
               'attempt': os.environ.get('GITHUB_RUN_ATTEMPT'), 'machine': platform.machine(),
               'sourceSha256': hashlib.sha256(source.read_bytes()).hexdigest(), 'variants': {},
               'status': 'INCOMPLETE', 'delayedOsReplyIsolation': 'NOT_PROVED',
               'physicalIme': 'NOT_RUN', 'pinnedWinitIntegration': 'NOT_RUN'}
    status = 1
    temporary = None
    try:
        with tempfile.TemporaryDirectory(prefix='soul2d-macos-ime-', dir=os.environ.get('RUNNER_TEMP')) as temp:
            temporary = Path(temp)
            executable = temporary / 'native-context-probe'
            with (output / 'compile.log').open('wb') as log:
                receipt['compile'] = run_owned(['xcrun', '--sdk', 'macosx', 'clang', '-fobjc-arc',
                    '-Wall', '-Wextra', '-Werror', '-mmacosx-version-min=15.0', str(source),
                    '-framework', 'Cocoa', '-o', str(executable)], log, subprocess.STDOUT, 90)
            if receipt['compile'] != {'returncode': 0, 'timedOut': False, 'ownedGroupAbsent': True}:
                raise RuntimeError('native fixture compilation did not complete cleanly')
            receipt['executableSha256'] = hashlib.sha256(executable.read_bytes()).hexdigest()
            for mode in ['shared', 'distinct']:
                with (output / (mode + '.jsonl')).open('wb') as log, (output / (mode + '.stderr')).open('wb') as errors:
                    receipt['variants'][mode] = run_owned([str(executable), mode], log, errors, 30)
            complete = {'returncode': 0, 'timedOut': False, 'ownedGroupAbsent': True}
            if all(value == complete for value in receipt['variants'].values()):
                receipt['status'] = 'BOTH_VARIANTS_COMPLETED_NOT_PRODUCT_ACCEPTANCE'
                status = 0
    except (KeyboardInterrupt, Exception) as error:
        receipt['error'] = f'{type(error).__name__}: {error}'
    finally:
        receipt['temporaryExecutableDirectoryAbsent'] = temporary is not None and not temporary.exists()
        if not receipt['temporaryExecutableDirectoryAbsent']:
            status = 1
        (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))
    return status


if __name__ == '__main__':
    sys.exit(main())
