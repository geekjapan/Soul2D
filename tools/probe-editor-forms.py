#!/usr/bin/env python3
import argparse
import ctypes
import ctypes.util
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import traceback

ROOT = Path(__file__).resolve().parents[1]


def probe(editor, example, out):
    out.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='probe-', dir=out))
    result = dict(status='FAILED', formsEditing='NOT_RUN', inputInjection='NOT_RUN',
                  imageGrounding='REQUIRES_INSPECTION', run=str(run))
    proc = None
    try:
        assert sys.platform.startswith('linux') and os.environ.get('GITHUB_ACTIONS') == 'true', 'hosted Linux CI only'
        assert os.environ.get('RUNNER_OS') == 'Linux' and os.environ.get('RUNNER_ARCH') == 'X64', 'Linux x64 only'
        display = os.environ.get('DISPLAY', '')
        assert re.fullmatch(r':\d+', display), 'a local owned Xvfb display is required'
        server_pid = int(Path(f'/tmp/.X{display[1:]}-lock').read_text().strip())
        server = Path(f'/proc/{server_pid}')
        assert server.stat().st_uid == os.getuid(), 'Xvfb must belong to this runner user'
        command = [part.decode() for part in (server / 'cmdline').read_bytes().split(b'\0') if part]
        assert Path(command[0]).name == 'Xvfb' and display in command, 'refuse a physical or unrelated X server'
        authority = Path(command[command.index('-auth') + 1])
        assert authority.samefile(os.environ['XAUTHORITY']), 'Xauthority must match this Xvfb'
        result['display'] = dict(name=display, serverPid=server_pid, executable=command[0], authority=str(authority))
        x11_name, xtest_name = ctypes.util.find_library('X11'), ctypes.util.find_library('Xtst')
        assert x11_name and xtest_name, 'X11 and XTest libraries are required'
        x11, xtest = ctypes.CDLL(x11_name), ctypes.CDLL(xtest_name)
        x11.XOpenDisplay.argtypes, x11.XOpenDisplay.restype = [ctypes.c_char_p], ctypes.c_void_p
        x11.XCloseDisplay.argtypes, x11.XCloseDisplay.restype = [ctypes.c_void_p], ctypes.c_int
        xtest.XTestQueryExtension.argtypes = [ctypes.c_void_p] + [ctypes.POINTER(ctypes.c_int)] * 4
        xtest.XTestQueryExtension.restype = ctypes.c_int
        connection = x11.XOpenDisplay(display.encode())
        assert connection, 'could not connect to owned Xvfb'
        try:
            event, error, major, minor = (ctypes.c_int() for _ in range(4))
            assert xtest.XTestQueryExtension(connection, ctypes.byref(event), ctypes.byref(error),
                                            ctypes.byref(major), ctypes.byref(minor)), 'XTEST extension unavailable'
            result['xtest'] = dict(library=xtest_name, major=major.value, minor=minor.value,
                                   eventBase=event.value, errorBase=error.value)
        finally:
            x11.XCloseDisplay(connection)

        sys.dont_write_bytecode = True
        spec = importlib.util.spec_from_file_location('forms_preview_probe', ROOT / 'tools/verify-editor-native-preview.py')
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        peer_module = native.PEER
        sources = ['tools/probe-editor-forms.py', '.github/workflows/rust-core.yml',
                   'tools/verify-editor-native-preview.py', 'tools/verify-editor-texture-cache.py',
                   'tools/verify-animation-key-gui.py', 'rust/src/editor_ui.rs', 'rust/src/editor_windows.rs',
                   'rust/src/editor_live2d_forms.rs', 'rust/examples/four-color-cmo3.rs']
        result['sourceHashes'] = {path: peer_module.digest(ROOT / path) for path in sources}
        result['gitHead'] = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, timeout=5).decode().strip()
        result['binarySha256'], result['exampleSha256'] = peer_module.digest(editor), peer_module.digest(example)
        fixture = run / 'four-color-日本語.cmo3'
        generated = subprocess.run([str(example), str(fixture)], capture_output=True, text=True, encoding='utf-8', timeout=30)
        result['producer'] = dict(exit=generated.returncode, stdout=generated.stdout, stderr=generated.stderr)
        assert generated.returncode == 0 and peer_module.digest(fixture) == native.FIXTURE_SHA
        result['fixtureSha256'] = native.FIXTURE_SHA
        env = peer_module.HARNESS.editor_env(run)
        env.pop('WAYLAND_DISPLAY', None)
        env.update(LIBGL_ALWAYS_SOFTWARE='1', WINIT_X11_SCALE_FACTOR='1')
        settings = run / 'config/settings.json'
        seed = dict(json.loads(settings.read_text()), WinW=1440, WinH=900, WinMax=False)
        settings.write_text(json.dumps(seed))
        result['settingsSeed'] = seed
        empty, result['emptyRoot'] = native.capture(run, 'empty-root', time.monotonic() + 10)
        with (run / 'wire.jsonl').open('w', encoding='utf-8') as wire, (run / 'editor.log').open('wb') as stderr:
            command = [str(editor), '--mcp', '--open', str(fixture)]
            result['command'] = command
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, env=env, bufsize=0)
            result['pid'] = proc.pid
            os.set_blocking(proc.stdin.fileno(), False)
            os.set_blocking(proc.stdout.fileno(), False)
            peer = peer_module.Mcp(proc, wire)
            peer.rpc('initialize', {})
            original = peer_module.document(peer)
            assert original['documentFormat'] == 'live2d.cmo3' and not original['modified']
            (run / 'document-before.json').write_text(json.dumps(original, indent=2) + '\n')
            deadline = time.monotonic() + 20
            for attempt in range(20):
                assert proc.poll() is None, 'editor exited before capture'
                current = peer_module.state(peer)
                pixels, screenshot = native.capture(run, f'production-{attempt:02}', deadline)
                result.setdefault('captures', []).append(screenshot)
                assert peer_module.document(peer) == original, 'read-only probe changed the document'
                if pixels != empty:
                    (run / 'editor-state.json').write_text(json.dumps(current, indent=2) + '\n')
                    result['productionScreenshot'] = screenshot
                    break
                time.sleep(.1)
            else:
                raise AssertionError('production editor never appeared in the owned display')
            reply = peer.op('editor_quit')
            result['quit'] = reply
            assert reply['quit']['state'] == 'accepted' and reply['quit']['policy'] == 'close_after_reply_flush_and_final_safety_check'
            assert proc.wait(timeout=10) == 0
        after, result['afterQuitRoot'] = native.capture(run, 'after-quit-root', time.monotonic() + 10)
        assert after == empty, 'owned editor window remains visible'
        assert peer_module.digest(fixture) == native.FIXTURE_SHA
        assert peer_module.digest(editor) == result['binarySha256'] and peer_module.digest(example) == result['exampleSha256']
        assert all(peer_module.digest(ROOT / path) == sha for path, sha in result['sourceHashes'].items())
        result.update(status='PROBE_COMPLETE', immutableInputsFinal=True)
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if proc is not None:
                forced = proc.poll() is None
                if forced:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait(timeout=5)
                result['cleanup'] = dict(pid=proc.pid, forced=forced, exit=proc.returncode, running=proc.poll() is None)
                for stream in [proc.stdin, proc.stdout]:
                    stream.close()
        except BaseException:
            result.update(status='FAILED', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(f"{result['status']}: {run}; Forms editing and OS input remain NOT_RUN")


def main():
    parser = argparse.ArgumentParser(description='Throwaway hosted-Xvfb capture probe; not Forms editing acceptance.')
    parser.add_argument('--editor', required=True, type=Path)
    parser.add_argument('--example', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    probe(args.editor.resolve(), args.example.resolve(), args.out.resolve())


if __name__ == '__main__':
    main()
