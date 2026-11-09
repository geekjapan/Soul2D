from pathlib import Path
import hashlib
import json
import plistlib
import re
import subprocess


def build_output(output, run, attempt):
    if not re.fullmatch(r'[0-9]+', run) or not re.fullmatch(r'[0-9]+', attempt):
        raise ValueError('numeric run and attempt required')
    output = Path(output).resolve()
    installation_roots = [(Path.home() / 'Library/Input Methods').resolve(),
                          Path('/Library/Input Methods').resolve()]
    if any(output == root or root in output.parents for root in installation_roots):
        raise ValueError('build output cannot be the input-method installation directory')
    return output


def run_logged(command, log, timeout):
    def preserve(stdout, stderr, returncode, timed_out, error=None):
        stdout = stdout or b''
        stderr = stderr or b''
        log.write_bytes(stdout + stderr)
        log.with_suffix(log.suffix + '.stdout').write_bytes(stdout)
        log.with_suffix(log.suffix + '.stderr').write_bytes(stderr)
        log.with_suffix(log.suffix + '.json').write_text(json.dumps({
            'command': command, 'returncode': returncode, 'timedOut': timed_out, 'error': error,
            'stdoutSha256': hashlib.sha256(stdout).hexdigest(),
            'stderrSha256': hashlib.sha256(stderr).hexdigest(),
        }, indent=2) + '\n')

    try:
        result = subprocess.run(command, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        preserve(error.stdout, error.stderr, None, True)
        raise
    except OSError as error:
        preserve(b'', str(error).encode(), None, False, type(error).__name__)
        raise
    preserve(result.stdout, result.stderr, result.returncode, False)
    result.check_returncode()
    return result


def build_server_bundle(output, run, attempt):
    output = build_output(output, run, attempt)
    identity = f'com.geekjapan.soul2d.ci.ime.r{run}.a{attempt}'
    connection = f'Soul2DCIAsyncIME-r{run}-a{attempt}'
    bundle = output / f'Soul2D-CI-IME-{run}-{attempt}.app'
    bundle.mkdir(parents=True, exist_ok=False)
    contents = bundle / 'Contents'
    (contents / 'MacOS').mkdir(parents=True, exist_ok=False)
    info = {
        'CFBundleIdentifier': identity,
        'CFBundleName': 'Soul2D CI Async IME',
        'CFBundleExecutable': 'S2DCIAsyncIME',
        'CFBundlePackageType': 'APPL',
        'CFBundleVersion': '1',
        'CFBundleShortVersionString': '1.0',
        'LSBackgroundOnly': True,
        'InputMethodConnectionName': connection,
        'InputMethodServerControllerClass': 'S2DCIAsyncController',
        'tsInputMethodCharacterRepertoireKey': ['Latn'],
        'tsInputMethodLanguagesKey': ['en'],
        'tsInputMethodPrimaryInScriptKey': 'smRoman',
    }
    (contents / 'Info.plist').write_bytes(plistlib.dumps(info, sort_keys=True))
    source = Path(__file__).with_name('server.m')
    executable = contents / 'MacOS/S2DCIAsyncIME'
    run_logged([
        'clang', '-fobjc-arc', '-Wall', '-Wextra', '-Werror', '-mmacosx-version-min=15.0',
        str(source), '-framework', 'Cocoa', '-framework', 'InputMethodKit', '-framework', 'Carbon',
        '-o', str(executable),
    ], output / 'compile.log', 90)
    run_logged(['/usr/bin/codesign', '--force', '--sign', '-', str(bundle)], output / 'sign-server.log', 30)
    run_logged(['/usr/bin/codesign', '--verify', '--strict', str(bundle)], output / 'verify-server.log', 30)
    return {
        'scope': 'BUILD_AND_ADHOC_SIGN_ONLY_NO_INSTALL_REGISTRATION_SELECTION_OR_NATIVE_LAUNCH',
        'bundle': str(bundle), 'bundleIdentifier': identity, 'connectionName': connection,
        'executable': str(executable),
        'sourceSha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'executableSha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
        'infoSha256': hashlib.sha256((contents / 'Info.plist').read_bytes()).hexdigest(),
    }


def build_client_bundle(output, run, attempt):
    output = build_output(output, run, attempt)
    identity = f'com.geekjapan.soul2d.ci.client.r{run}.a{attempt}'
    bundle = output / f'Soul2D-CI-Client-{run}-{attempt}.app'
    bundle.mkdir(parents=True, exist_ok=False)
    contents = bundle / 'Contents'
    (contents / 'MacOS').mkdir(parents=True, exist_ok=False)
    info = {
        'CFBundleIdentifier': identity,
        'CFBundleName': 'Soul2D CI Async Client',
        'CFBundleExecutable': 'S2DCIAsyncClient',
        'CFBundlePackageType': 'APPL',
        'CFBundleVersion': '1',
        'CFBundleShortVersionString': '1.0',
        'NSPrincipalClass': 'NSApplication',
        'LSUIElement': True,
    }
    (contents / 'Info.plist').write_bytes(plistlib.dumps(info, sort_keys=True))
    source = Path(__file__).with_name('client.m')
    executable = contents / 'MacOS/S2DCIAsyncClient'
    run_logged([
        'clang', '-fobjc-arc', '-Wall', '-Wextra', '-Werror', '-mmacosx-version-min=15.0',
        str(source), '-framework', 'Cocoa', '-framework', 'CoreGraphics', '-o', str(executable),
    ], output / 'compile-client.log', 90)
    run_logged(['/usr/bin/codesign', '--force', '--sign', '-', str(bundle)], output / 'sign-client.log', 30)
    run_logged(['/usr/bin/codesign', '--verify', '--strict', str(bundle)], output / 'verify-client.log', 30)
    return {
        'scope': 'BUILD_AND_ADHOC_SIGN_ONLY_NO_NATIVE_CLIENT_LAUNCH_OR_CONFIGURATION',
        'bundle': str(bundle), 'bundleIdentifier': identity, 'executable': str(executable),
        'sourceSha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'executableSha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
        'infoSha256': hashlib.sha256((contents / 'Info.plist').read_bytes()).hexdigest(),
    }


if __name__ == '__main__':
    import sys
    if len(sys.argv) != 4:
        raise SystemExit('usage: build_bundle.py OUTPUT_DIRECTORY RUN ATTEMPT')
    print(json.dumps(build_server_bundle(sys.argv[1], sys.argv[2], sys.argv[3]), indent=2))
