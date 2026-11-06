#!/usr/bin/env python3
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import time
import traceback
from typing import NamedTuple

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
STATE = 'resource://soul2d/editor/state'
GEOMETRY = {'WinW', 'WinH', 'WinMax'}


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HARNESS = load('texture_editor_harness', 'tools/verify-animation-key-gui.py')
CORE = load('texture_fixture', 'tools/verify-rust-core.py')


class Texture(NamedTuple):
    width: int
    height: int
    rgb: tuple


class Fixture(NamedTuple):
    name: str
    textures: tuple
    slots: tuple

    def descriptors(self):
        return [None if slot is None else dict(slot=slot, width=self.textures[slot].width,
                height=self.textures[slot].height, displayable=True) for slot in self.slots]


A = Fixture('texture-cache-A', (Texture(256, 256, (96, 96, 96)), Texture(96, 64, (255, 0, 255))), (0, None, 1))
B = Fixture('texture-cache-B', (Texture(128, 80, (32, 224, 64)), Texture(320, 256, (0, 255, 255))), (1, None, 0))
MARKERS = {'magenta': A.textures[1].rgb, 'green': B.textures[0].rgb, 'cyan': B.textures[1].rgb}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def extension_tail(tail):
    assert tail[:8] == b'TEX_SECT' and len(tail) >= 12
    count, = struct.unpack_from('>I', tail, 8)
    offset = 12
    for _ in range(count):
        assert offset + 5 <= len(tail)
        size, = struct.unpack_from('>I', tail, offset)
        offset += 5 + size
        assert offset <= len(tail)
    assert tail[offset:offset + 8] == b'EXT_SECT'
    return tail[offset:]


def fixture(path, spec):
    model, tail = CORE.fixture(path)
    model['meta']['name'] = spec.name
    part = model['nodes']['children'][0]
    part['mesh']['verts'] = [-512, -512, 512, -512, 512, 512, -512, 512]
    part['vertices'] = [[-512, -512], [512, -512], [512, 512], [-512, 512]]
    part['textures'] = [0xffffffff if slot is None else slot for slot in spec.slots]
    textures = [CORE.png(t.width, t.height, bytes((*t.rgb, 255)) * (t.width * t.height)) for t in spec.textures]
    tail = (b'TEX_SECT' + struct.pack('>I', len(textures))
            + b''.join(struct.pack('>IB', len(png), 0) + png for png in textures) + extension_tail(tail))
    raw = json.dumps(model, separators=(',', ':')).encode()
    path.write_bytes(b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw + tail)
    return model


def read_line(fd, deadline, partial):
    while b'\n' not in partial:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], max(0, remaining))[0]:
            raise TimeoutError('editor MCP incomplete JSONL response')
        chunk = os.read(fd, 4096)
        if not chunk:
            raise EOFError('editor MCP closed before complete response')
        partial.extend(chunk)
        assert len(partial) <= 1024 * 1024, 'editor MCP response exceeds 1 MiB'
    assert partial.endswith(b'\n') and partial.count(b'\n') == 1, 'unexpected extra editor MCP output'
    return json.loads(partial)


class Mcp(HARNESS.Mcp):
    def rpc(self, method, params):
        self.seq += 1
        request = {'jsonrpc': '2.0', 'id': self.seq, 'method': method, 'params': params}
        self.wire.write(json.dumps({'request': request}) + '\n')
        self.wire.flush()
        pending = memoryview((json.dumps(request) + '\n').encode())
        deadline, partial = time.monotonic() + 30, bytearray()
        try:
            while pending:
                remaining = deadline - time.monotonic()
                fd = self.proc.stdin.fileno()
                if remaining <= 0 or not select.select([], [fd], [], max(0, remaining))[1]:
                    raise TimeoutError('editor MCP request write timed out')
                pending = pending[os.write(fd, pending):]
            response = read_line(self.proc.stdout.fileno(), deadline, partial)
            self.wire.write(json.dumps({'response': response}) + '\n')
            self.wire.flush()
            assert response.get('jsonrpc') == '2.0' and type(response.get('id')) is int
            assert response['id'] == self.seq and 'error' not in response, response
            return response['result']
        except BaseException:
            self.wire.write(json.dumps({'partialResponseHex': partial.hex(), 'error': traceback.format_exc()}) + '\n')
            self.wire.flush()
            raise


def state(peer):
    return json.loads(peer.rpc('resources/read', {'uri': STATE})['contents'][0]['text'])


def document(peer):
    snapshot = peer.snapshot()
    return {k: snapshot.get(k) for k in ['model', 'revision', 'modified', 'history', 'values', 'documentFormat']}


def assert_view(current, spec, revision, loaded):
    assert current['editor'] is True
    windows = current['windows']
    assert windows['textureViewer'] is True
    assert windows['textureViewerState'] == {'slot': 2, 'zoom': 2.0}
    expected = dict(node=2, revision=revision, slots=spec.descriptors()) if loaded else None
    assert windows['textureViewerLoadState'] == dict(loading=False, pending=None, loaded=expected, hasError=False)


def pixel_stats(width, height, rgb):
    assert width > 0 and height > 0 and len(rgb) == width * height * 3, 'invalid RGB image dimensions'
    histogram = Counter(struct.iter_unpack('3B', rgb))
    result = {}
    for name, marker in MARKERS.items():
        matching = {color for color in histogram if all(abs(a - b) <= 12 for a, b in zip(color, marker))}
        count = sum(histogram[color] for color in matching)
        solid = False
        if count >= 8192:
            heights = [0] * width
            pixels = iter(struct.iter_unpack('3B', rgb))
            for _ in range(height):
                run = 0
                for x in range(width):
                    heights[x] = heights[x] + 1 if next(pixels) in matching else 0
                    run = run + 1 if heights[x] >= 48 else 0
                    if run >= 48:
                        solid = True
                        break
                if solid:
                    break
        result[name] = {'pixels': count, 'solid48': solid}
    return result


def matches(stats, positive, absent):
    return (all(stats[name]['pixels'] >= 8192 and stats[name]['solid48'] for name in positive)
            and all(stats[name]['pixels'] == 0 for name in absent))


def capture_until(run, stage, positive, absent, attempts, proc):
    deadline, consecutive = time.monotonic() + 20, 0
    while time.monotonic() < deadline:
        assert proc.poll() is None, 'editor exited during screenshot witness'
        path = run / f'{stage}-{len(attempts):03}.png'
        attempt = {'stage': stage, 'file': path.name}
        attempts.append(attempt)
        with (run / 'capture.log').open('ab') as log:
            for command in [['import', '-window', 'root', str(path)],
                            ['convert', str(path), '-alpha', 'off', '-depth', '8', 'RGB:-']]:
                timeout = min(5, deadline - time.monotonic())
                if timeout <= 0:
                    raise TimeoutError(f'{stage} capture deadline exceeded')
                output = subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=log, timeout=timeout).stdout
        header = path.read_bytes()[:24]
        assert header[:8] == b'\x89PNG\r\n\x1a\n' and header[12:16] == b'IHDR'
        width, height = struct.unpack('>II', header[16:24])
        assert (width, height) == (1600, 1000), 'unexpected Xvfb root size'
        stats = pixel_stats(width, height, output)
        attempt.update(width=width, height=height, sha256=digest(path), colors=stats)
        good = matches(stats, positive, absent)
        attempt['matches'] = good
        consecutive = consecutive + 1 if good else 0
        if consecutive == 2:
            return
        time.sleep(min(.2, max(0, deadline - time.monotonic())))
    raise AssertionError(f'{stage} did not show the required window pixels twice')


def self_test():
    with tempfile.TemporaryDirectory() as directory:
        for spec in [A, B]:
            path = Path(directory) / 'fixture.inx'
            model = fixture(path, spec)
            actual, tail = CORE.split_container(path.read_bytes())
            assert actual == model and actual['future_root']['large_integer'] == 18446744073709551617
            assert actual['nodes']['children'][0]['textures'] == [0xffffffff if n is None else n for n in spec.slots]
            offset = 12
            for texture in spec.textures:
                size, tag = struct.unpack_from('>IB', tail, offset)
                assert tag == 0
                width, height, rgba = CORE.decode_rgba(tail[offset + 5:offset + 5 + size])
                assert (width, height) == (texture.width, texture.height)
                assert rgba == bytes((*texture.rgb, 255)) * (width * height)
                offset += 5 + size
            assert extension_tail(tail) == tail[offset:] and tail.endswith(b'FUTURE_TAIL')
            assert [d and d['slot'] for d in spec.descriptors()] == list(spec.slots)
        for malformed in [b'', b'TEX_SECT', b'TEX_SECT' + struct.pack('>I', 1),
                          b'TEX_SECT' + struct.pack('>IIB', 1, 100, 0) + b'EXT_SECT']:
            try:
                extension_tail(malformed)
            except AssertionError:
                pass
            else:
                raise AssertionError('malformed texture section accepted')
        embedded_marker = b'not-an-image:EXT_SECT'
        counted = b'TEX_SECT' + struct.pack('>IIB', 1, len(embedded_marker), 0) + embedded_marker + b'EXT_SECTsuffix'
        assert extension_tail(counted) == b'EXT_SECTsuffix'
    for name, marker in MARKERS.items():
        stats = pixel_stats(96, 96, bytes(marker) * (96 * 96))
        assert matches(stats, [name], [n for n in MARKERS if n != name])
        assert not matches(pixel_stats(96, 96, bytes(96 * 96 * 3)), [name], [])
        assert not matches(pixel_stats(96, 96, bytes(marker) * 95 + bytes((96 * 96 - 95) * 3)), [name], [])
        stripes = (bytes(marker) * 47 + bytes(3)) * 200
        stats = pixel_stats(48, 200, stripes)
        assert stats[name]['pixels'] > 8192 and not stats[name]['solid48']
        assert not matches(stats, [name], [])
        assert not matches(pixel_stats(96, 96, bytes(marker) * (96 * 96)), [], [name])
    assert pixel_stats(96, 96, bytes((0, 255, 0)) * (96 * 96))['green']['pixels'] == 0
    assert pixel_stats(96, 96, bytes((44, 212, 76)) * (96 * 96))['green']['pixels'] == 96 * 96
    assert pixel_stats(96, 96, bytes((45, 224, 64)) * (96 * 96))['green']['pixels'] == 0
    try:
        pixel_stats(2, 2, bytes(11))
    except AssertionError:
        pass
    else:
        raise AssertionError('truncated RGB accepted')
    if os.name == 'posix':
        reader, writer = os.pipe()
        try:
            os.write(writer, b'{"id":1')
            partial = bytearray()
            try:
                read_line(reader, time.monotonic() + .02, partial)
            except TimeoutError:
                assert partial == b'{"id":1'
            else:
                raise AssertionError('partial JSONL escaped deadline')
            os.write(writer, b'}\n')
            assert read_line(reader, time.monotonic() + 1, partial) == {'id': 1}
        finally:
            os.close(reader)
            os.close(writer)
    print('PASS texture-cache self-test: fixtures, slot maps, pixel controls, framing (POSIX deadline where supported)')


def verify(editor, out):
    assert sys.platform.startswith('linux') and os.environ.get('DISPLAY'), 'run inside a fresh Linux Xvfb display'
    out.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=out))
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(HARNESS.__file__), Path(CORE.__file__),
        ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock', ROOT / '.github/workflows/rust-core.yml'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}
    result = {'status': 'FAIL', 'scope': 'production Linux X11 Texture Viewer document replacement, stdio MCP',
              'sourceHashes': hashes, 'binarySha256': digest(editor), 'screenshots': [], 'stages': [],
              'platform': sys.platform, 'display': os.environ['DISPLAY'], 'displayPixels': [1600, 1000], 'requestedScale': 1,
              'otherPlatformWindows': 'NOT_RUN', 'realAcpAgent': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN',
              'formalProof': 'NOT_RUN', 'cleanup': None}
    proc = None
    try:
        sources = [run / f'{spec.name}.inx' for spec in [A, B]]
        models = [fixture(path, spec) for path, spec in zip(sources, [A, B])]
        result['fixtures'] = {path.name: {'sha256': digest(path), 'spec': spec._asdict()}
                              for path, spec in zip(sources, [A, B])}

        def assert_inputs():
            assert all(digest(path) == result['fixtures'][path.name]['sha256'] for path in sources)
            assert all(digest(ROOT / path) == expected for path, expected in hashes.items())
            assert digest(editor) == result['binarySha256']

        env = HARNESS.editor_env(run)
        env.pop('WAYLAND_DISPLAY', None)
        env.update(LIBGL_ALWAYS_SOFTWARE='1', WINIT_X11_SCALE_FACTOR='1')
        settings = run / 'config/settings.json'
        seed = dict(json.loads(settings.read_text()), WinW=1440, WinH=900, WinMax=False)
        settings.write_text(json.dumps(seed))
        result['settingsSeed'] = seed
        command = [str(editor), '--mcp', '--open', str(sources[0])]
        result['command'] = command
        result['gitHead'] = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, check=True,
                                           capture_output=True, text=True, timeout=5).stdout.strip()
        with (run / 'wire.jsonl').open('w') as wire, (run / 'editor.log').open('wb') as stderr:
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                                    env=env, bufsize=0)
            result['pid'] = proc.pid
            os.set_blocking(proc.stdin.fileno(), False)
            os.set_blocking(proc.stdout.fileno(), False)
            peer = Mcp(proc, wire)
            peer.rpc('initialize', {})
            baseline = document(peer)
            assert baseline['model'] == models[0] and baseline['modified'] is False
            peer.op('editor_selection_set', nodes=[2])
            peer.op('editor_camera_set', centre=[0, 0], zoom=1)
            peer.op('editor_texture_viewer_set', open=True, slot=2, zoom=2)
            peer.op('editor_texture_viewer_load')

            def witness(name, spec, baseline, loaded, positive, absent):
                before = state(peer)
                assert_view(before, spec, baseline['revision'], loaded)
                assert document(peer) == baseline
                capture_until(run, name, positive, absent, result['screenshots'], proc)
                after = state(peer)
                assert_view(after, spec, baseline['revision'], loaded)
                assert document(peer) == baseline
                result['stages'].append({'name': name, 'document': baseline, 'stateBefore': before, 'stateAfter': after})

            witness('A-loaded', A, baseline, True, ['magenta'], ['green', 'cyan'])
            stored = settings.read_bytes()
            (run / 'settings-postlaunch.json').write_bytes(stored)
            peer.op('open', path=str(sources[1]))
            baseline = document(peer)
            assert baseline['model'] == models[1] and baseline['modified'] is False
            peer.op('editor_selection_set', nodes=[2])
            peer.op('editor_camera_set', centre=[0, 0], zoom=1)
            witness('B-cleared', B, baseline, False, ['cyan'], ['magenta', 'green'])
            peer.op('editor_texture_viewer_load')
            witness('B-loaded', B, baseline, True, ['cyan', 'green'], ['magenta'])
            refused = peer.rpc('tools/call', {'name': 'open', 'arguments': {'path': str(run / 'absent.inx')}})
            assert refused.get('isError') is True, refused
            witness('B-refused-open', B, baseline, True, ['cyan', 'green'], ['magenta'])
            assert settings.read_bytes() == stored, 'view/file operations changed settings before Quit'
            result['settingsPreQuitSha256'] = digest(settings)
            assert_inputs()
            result['immutableInputsPreQuit'] = True
            quit_reply = peer.op('editor_quit')
            result['quit'] = quit_reply
            assert quit_reply['quit']['state'] == 'accepted', quit_reply
            assert proc.wait(timeout=10) == 0, 'editor did not exit naturally with status0'
        final_settings = json.loads(settings.read_text())
        before_settings = json.loads(stored)
        assert {k: v for k, v in final_settings.items() if k not in GEOMETRY} == {
            k: v for k, v in before_settings.items() if k not in GEOMETRY}
        assert all(type(final_settings[k]) is int and 320 <= final_settings[k] <= 16384 for k in ['WinW', 'WinH'])
        assert type(final_settings['WinMax']) is bool
        result.update(settingsFinal=final_settings, settingsFinalSha256=digest(settings))
        assert_inputs()
        result['immutableInputsFinal'] = True
        result['status'] = 'PASS'
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
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(f"{result['status']}: {run}")


def main():
    if not __debug__:
        raise RuntimeError('verification requires Python assertions; do not use -O')
    parser = argparse.ArgumentParser(description='Verify real Linux X11 Texture Viewer cache replacement; no local GUI automation.')
    parser.add_argument('--editor', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
    elif args.editor and args.out:
        verify(args.editor.resolve(), args.out.resolve())
    else:
        parser.error('--editor and --out are required unless --self-test is given')


if __name__ == '__main__':
    main()
