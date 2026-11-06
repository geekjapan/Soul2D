#!/usr/bin/env python3
import argparse
from collections import deque
import contextlib
import copy
import importlib.util
import io
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('native_preview_peer', ROOT / 'tools/verify-editor-texture-cache.py')
PEER = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PEER)
BG = (7, 11, 13)
COLORS = ((220, 40, 20), (20, 190, 40), (30, 50, 210), (190, 40, 180))
FADED = (113, 25, 16)
OFFSETS = ((0, 0), (2, 0), (0, 2), (2, 2))
FIXTURE_SHA = 'd4457e2b009309dee12a3596ab18ae7ad74725d1c34f7a15f63e82a0bc787550'


def close_color(pixel, color, tolerance):
    return all(abs(a-b) <= tolerance for a, b in zip(pixel, color))


def half_coverage(pixel, color):
    channel = max(range(3), key=lambda k: abs(color[k]-BG[k]))
    delta = color[channel]-BG[channel]
    alpha = (pixel[channel]-BG[channel])/delta
    return .5 <= alpha <= 1+2/abs(delta) and all(
        abs(p-(b+alpha*(c-b))) <= 2 for p, b, c in zip(pixel, BG, color))


def shape(mask, qx, qy):
    assert mask, 'missing triangle footprint'
    edge = math.ceil(qx+qy)+2
    assert edge <= 6
    x0, y0 = min(x for x, y in mask), min(y for x, y in mask)
    x1, y1 = max(x for x, y in mask), max(y for x, y in mask)
    assert abs(x1-x0+1-24) <= edge and abs(y1-y0+1-24) <= edge, 'wrong footprint extent'
    rows = []
    for y in range(y0, y1+1):
        xs = sorted(x for x, yy in mask if yy == y)
        assert xs, 'missing footprint row'
        span, gaps = xs[-1]-xs[0]+1, xs[-1]-xs[0]+1-len(xs)
        # Shader dithering can alternate pixels on the first half-coverage boundary row.
        assert y == y0 or gaps <= 2, 'sparse triangle interior'
        assert abs(span-(24-(y-y0))) <= edge and abs(xs[0]-x0) <= edge, 'wrong triangle profile'
        rows.append(dict(y=y, span=span, gaps=gaps))
    return dict(bounds=[x0, y0, x1, y1], pixels=len(mask), edge=edge, rows=rows)


def viewport(width, height, rgb, points):
    assert width > 0 and height > 0 and len(rgb) == width*height*3, 'invalid RGB image'
    assert len(points) == 2 and all(type(p) in (int, float) and math.isfinite(p) for p in points)
    assert 200 <= points[0] <= 1550 and 200 <= points[1] <= 950, 'unexpected viewport points'
    background = bytearray(close_color(pixel, BG, 1) for pixel in struct.iter_unpack('3B', rgb))
    pending, candidates = background[:], []
    while (start := pending.find(1)) != -1:
        pending[start] = 0
        queue = deque([start])
        x0 = x1 = start % width
        y0 = y1 = start // width
        while queue:
            index = queue.popleft()
            x, y = index % width, index // width
            x0, x1, y0, y1 = min(x0, x), max(x1, x), min(y0, y), max(y1, y)
            for neighbor in (index-1 if x else -1, index+1 if x+1 < width else -1,
                             index-width if y else -1, index+width if y+1 < height else -1):
                if neighbor >= 0 and pending[neighbor]:
                    pending[neighbor] = 0
                    queue.append(neighbor)
        if abs(x1-x0+1-points[0]) <= 2 and abs(y1-y0+1-points[1]) <= 2:
            candidates.append([x0, y0, x1+1, y1+1])
    assert len(candidates) == 1, f'expected one scale1 viewport, got {candidates}'
    return candidates[0], background


def inside(x, y, origin, margin):
    dx, dy = x+.5-origin[0], y+.5-origin[1]
    return dx >= -margin and dy >= -margin and dx+dy <= 24+math.sqrt(2)*margin


def pixels(width, height, rgb, points, centre, edited):
    rect, background = viewport(width, height, rgb, points)
    scale = min(768/max(points), 1)
    raster = [max(1, math.floor(p*scale+.5)) for p in points]
    qx, qy = [p/r for p, r in zip(points, raster)]
    assert 0 < qx <= 2 and 0 < qy <= 2, 'unsupported preview texel footprint'
    margin = math.ceil(math.hypot(qx, qy)+math.sqrt(2))
    assert margin <= 5
    origins = [(rect[0]+(x-centre[0])*12+points[0]/2,
                rect[1]+(y-centre[1])*12+points[1]/2) for x, y in OFFSETS]
    colors = (FADED, *COLORS[1:]) if edited else COLORS
    for y in range(rect[1]+2, rect[3]-2):
        for x in range(rect[0]+2, rect[2]-2):
            if background[y*width+x]:
                continue
            assert any(inside(x, y, origin, margin) for origin in origins), 'unexpected viewport drawing'
            pixel = rgb[(y*width+x)*3:(y*width+x+1)*3]
            for color, origin in zip(colors, origins):
                if close_color(pixel, color, 2):
                    assert inside(x, y, origin, margin), 'color in wrong triangle'
    stats = []
    for color, origin in zip(colors, origins):
        mask, core = set(), 0
        for y in range(math.floor(origin[1]-margin), math.ceil(origin[1]+24+margin)):
            for x in range(math.floor(origin[0]-margin), math.ceil(origin[0]+24+margin)):
                assert rect[0]+2 <= x < rect[2]-2 and rect[1]+2 <= y < rect[3]-2, 'clipped witness'
                pixel = rgb[(y*width+x)*3:(y*width+x+1)*3]
                if inside(x, y, origin, -margin):
                    assert close_color(pixel, color, 2), f'wrong core color at {(x, y)}: {list(pixel)} expected {color}'
                    core += 1
                if inside(x, y, origin, margin) and half_coverage(pixel, color):
                    mask.add((x, y))
        assert core >= 16, 'insufficient triangle core'
        stats.append(dict(color=color, origin=origin, corePixels=core, **shape(mask, qx, qy)))
    return dict(viewport=rect, physicalScale=[(rect[2]-rect[0])/points[0], (rect[3]-rect[1])/points[1]],
                raster=raster, texelFootprint=[qx, qy], margin=margin, colors=stats)


def capture(run, name, deadline):
    path = run / f'{name}.png'
    with (run / 'capture.log').open('ab') as log:
        for command in [['import', '-window', 'root', str(path)],
                        ['convert', str(path), '-alpha', 'off', '-depth', '8', 'RGB:-']]:
            remaining = min(5, deadline-time.monotonic())
            if remaining <= 0:
                raise TimeoutError('compositor capture deadline exceeded')
            output = subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=log, timeout=remaining).stdout
    header = path.read_bytes()[:24]
    assert header[:8] == b'\x89PNG\r\n\x1a\n' and header[12:16] == b'IHDR'
    width, height = struct.unpack('>II', header[16:24])
    assert (width, height) == (1600, 1000) and len(output) == width*height*3, 'unexpected root image'
    return output, dict(file=path.name, sha256=PEER.digest(path), width=width, height=height)


def view_state(state, centre):
    assert state['editor'] is True
    camera = state['camera']
    assert camera['centre'] == list(centre) and camera['zoom'] == 12 and camera['mirror'] is False
    assert state['viewport'] == dict(vertices=False, bounds=False, orientation=False, background=list(BG), partSort=None)
    assert state['grid']['show'] is False and state['guides']['show'] is False
    assert state['solo']['show'] is False and state['physicsPreview']['enabled'] is False
    assert state['animationOverlays'] == dict(onion=False, trajectories=False)
    assert state['selection'] == dict(nodes=[], bindings=[], vertices=None, timeline=None)


def verify(editor, example, out):
    out.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=out))
    result = dict(status='FAIL', scope='synthetic CMO3 production Linux X11 preview correctness, not capacity performance',
                  platform=sys.platform, display=os.environ.get('DISPLAY'), requestedScale=1,
                  stages=[], screenshots=[], otherPlatformWindows='NOT_RUN', vendorAcceptance='NOT_RUN',
                  formalProof='NOT_RUN', practicalCapacity='NOT_RUN', cleanup=None)
    proc = None
    try:
        sources = set((ROOT / 'rust/src').rglob('*.rs')) | {Path(__file__).resolve(), Path(PEER.__file__),
            Path(PEER.HARNESS.__file__), Path(PEER.CORE.__file__), ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
            ROOT / 'rust/examples/four-color-cmo3.rs', ROOT / 'rust/tests/common/authoring_fixture.rs',
            ROOT / '.github/workflows/rust-core.yml'}
        hashes = {str(p.relative_to(ROOT)): PEER.digest(p) for p in sorted(sources)}
        result['sourceHashes'] = hashes
        result['binarySha256'] = PEER.digest(editor)
        result['exampleSha256'] = PEER.digest(example)
        assert sys.platform.startswith('linux') and os.environ.get('DISPLAY'), 'run inside a fresh Linux Xvfb display'
        fixture = run / 'four-color-日本語.cmo3'
        generated = subprocess.run([str(example), str(fixture)], capture_output=True, text=True, encoding='utf-8', timeout=30)
        result['producer'] = dict(exit=generated.returncode, stdout=generated.stdout, stderr=generated.stderr)
        assert generated.returncode == 0 and fixture.stat().st_size == 345963 and PEER.digest(fixture) == FIXTURE_SHA
        result['fixtureSha256'] = FIXTURE_SHA

        def immutable():
            assert PEER.digest(fixture) == FIXTURE_SHA
            assert PEER.digest(editor) == result['binarySha256'] and PEER.digest(example) == result['exampleSha256']
            assert all(PEER.digest(ROOT / p) == sha for p, sha in hashes.items())

        env = PEER.HARNESS.editor_env(run)
        env.pop('WAYLAND_DISPLAY', None)
        env.update(LIBGL_ALWAYS_SOFTWARE='1', WINIT_X11_SCALE_FACTOR='1')
        settings = run / 'config/settings.json'
        seed = dict(json.loads(settings.read_text()), WinW=1440, WinH=900, WinMax=False)
        settings.write_text(json.dumps(seed))
        result['settingsSeed'] = seed
        empty, result['emptyRoot'] = capture(run, 'empty-root', time.monotonic()+10)
        command = [str(editor), '--mcp', '--open', str(fixture)]
        result['command'] = command
        result['gitHead'] = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, check=True,
                                          capture_output=True, text=True, encoding='utf-8', timeout=5).stdout.strip()
        with (run / 'wire.jsonl').open('w', encoding='utf-8') as wire, (run / 'editor.log').open('wb') as stderr:
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, env=env, bufsize=0)
            result['pid'] = proc.pid
            os.set_blocking(proc.stdin.fileno(), False)
            os.set_blocking(proc.stdout.fileno(), False)
            peer = PEER.Mcp(proc, wire)
            peer.rpc('initialize', {})
            original = PEER.document(peer)
            assert original['documentFormat'] == 'live2d.cmo3' and original['modified'] is False
            assert [source['guid'] for source in original['model']['sources']] == [
                'mesh-guid-0', 'mesh-guid-1', 'mesh-guid-2', 'mesh-guid-3', 'part-guid']
            assert len(original['model']['textures']) == 4
            peer.op('editor_camera_set', centre=[2, 2], zoom=12, mirror=False)
            peer.op('editor_viewport_set', vertices=False, bounds=False, orientation=False, background=list(BG))
            peer.op('editor_grid_set', show=False)
            peer.op('editor_guides_set', show=False)

            def witness(name, expected, centre, edited):
                deadline, consecutive, latest = time.monotonic()+20, 0, None
                while time.monotonic() < deadline:
                    assert proc.poll() is None, 'editor exited during witness'
                    before = PEER.state(peer)
                    view_state(before, centre)
                    assert PEER.document(peer) == expected
                    rgb, attempt = capture(run, f'{name}-{len(result["screenshots"]):03}', deadline)
                    result['screenshots'].append(attempt)
                    after = PEER.state(peer)
                    view_state(after, centre)
                    assert before['camera'] == after['camera'] and PEER.document(peer) == expected
                    attempt.update(stage=name, stateBefore=before, stateAfter=after)
                    try:
                        latest = pixels(1600, 1000, rgb, before['camera']['viewSize'], centre, edited)
                        attempt.update(matches=True, pixels=latest)
                    except AssertionError as error:
                        attempt.update(matches=False, rejection=str(error))
                    consecutive = consecutive+1 if attempt['matches'] else 0
                    if consecutive == 2:
                        result['stages'].append(dict(name=name, document=expected, pixels=latest))
                        return latest
                    time.sleep(min(.2, max(0, deadline-time.monotonic())))
                raise AssertionError(f'{name} required two presented frames')

            initial = witness('baseline', original, (2, 2), False)
            stored = settings.read_bytes()
            (run / 'settings-postlaunch.json').write_bytes(stored)
            peer.op('editor_camera_set', centre=[3, 2])
            moved = witness('pan-left', original, (3, 2), False)
            peer.op('editor_camera_set', centre=[2, 2])
            returned = witness('returned', original, (2, 2), False)
            for first, pan, back in zip(initial['colors'], moved['colors'], returned['colors']):
                assert all(abs(b-a-delta) <= 2 for a, b, delta in zip(first['bounds'], pan['bounds'], [-12, 0, -12, 0]))
                assert all(abs(a-b) <= 2 for a, b in zip(first['bounds'], back['bounds']))
            args = dict(sourceGuid='mesh-guid-0', formGuid='form-guid-0', coordinateType='Canvas')
            peer.op('live2d_form_set_opacity', **args, opacity=.5, expectedRevision=original['revision'])
            edited = PEER.document(peer)
            model = copy.deepcopy(original['model'])
            assert model['sources'][0]['forms'][0]['opacity'] == 1
            model['sources'][0]['forms'][0]['opacity'] = .5
            assert edited['model'] == model and edited['values'] == original['values']
            assert edited['documentFormat'] == original['documentFormat'] and edited['modified'] is True
            assert edited['revision'] > original['revision']
            witness('edited', edited, (2, 2), True)
            refused = peer.rpc('tools/call', dict(name='live2d_form_set_opacity', arguments=dict(
                **args, opacity=.25, expectedRevision=original['revision'])))
            assert refused.get('isError') is True, refused
            result['staleRefusal'] = refused
            witness('stale-refused', edited, (2, 2), True)
            peer.op('undo')
            restored = PEER.document(peer)
            assert all(restored[k] == original[k] for k in ['model', 'values', 'documentFormat', 'modified'])
            assert restored['revision'] > edited['revision']
            witness('restored', restored, (2, 2), False)
            assert settings.read_bytes() == stored, 'settings changed before Quit'
            result['settingsPreQuitSha256'] = PEER.digest(settings)
            immutable()
            result['immutableInputsPreQuit'] = True
            reply = peer.op('editor_quit')
            result['quit'] = reply
            assert reply['quit']['state'] == 'accepted' and reply['quit']['attempt'] > 0
            assert reply['quit']['policy'] == 'close_after_reply_flush_and_final_safety_check'
            assert proc.wait(timeout=10) == 0, 'editor did not exit naturally with status0'
        final = json.loads(settings.read_text())
        assert {k: v for k, v in final.items() if k not in PEER.GEOMETRY} == {
            k: v for k, v in json.loads(stored).items() if k not in PEER.GEOMETRY}
        assert all(type(final[k]) is int and 320 <= final[k] <= 16384 for k in ['WinW', 'WinH'])
        assert type(final['WinMax']) is bool
        result.update(settingsFinal=final, settingsFinalSha256=PEER.digest(settings))
        after, result['afterQuitRoot'] = capture(run, 'after-quit-root', time.monotonic()+10)
        assert after == empty, 'owned window did not disappear from root'
        immutable()
        result.update(immutableInputsFinal=True, status='PASS')
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
            (run / 'result.json').write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
            print(f"{result['status']}: {run}")


def sampled_mask(inside_shape, qx, qy, px, py, color, dither):
    texels, mask = {}, set()
    def texel(i, j):
        if (i, j) not in texels:
            texels[i, j] = int(inside_shape((i+.5)*qx+px, (j+.5)*qy+py))
        return texels[i, j]
    for y in range(-6, 35):
        v = (y+.5-py)/qy-.5
        j, b = math.floor(v), v-math.floor(v)
        for x in range(-6, 35):
            u = (x+.5-px)/qx-.5
            i, a = math.floor(u), u-math.floor(u)
            alpha = sum(texel(i+di, j+dj)*wx*wy for di, wx in [(0, 1-a), (1, a)] for dj, wy in [(0, 1-b), (1, b)])
            rgb = [round(bg+alpha*(fg-bg)+(.49 if (x+y+k) % 2 else -.49)*dither)
                   for k, (bg, fg) in enumerate(zip(BG, color))]
            if half_coverage(rgb, color):
                mask.add((x, y))
    return mask


def self_test():
    def rejects(call):
        try:
            call()
        except AssertionError:
            return
        raise AssertionError('bad pixel control accepted')

    triangle = lambda x, y: x >= 0 and y >= 0 and x+y <= 24
    pairs = [(1, 1), (1.25, 1.25), (1.75, 1.75), (1.875, 1.873), (1.873, 1.875), (2, 2)]
    positive = negative = 0
    for qx, qy in pairs:
        for px in [0, .2, .4, .6, .8, 1]:
            for py in [0, .2, .4, .6, .8, 1]:
                shape(sampled_mask(triangle, qx, qy, px*qx, py*qy, COLORS[0], False), qx, qy)
                positive += 1
        for px, py in [(0, 0), (.4, .4), (.8, .2), (1, 1)]:
            for color in (*COLORS, FADED):
                for dither in [False, True]:
                    shape(sampled_mask(triangle, qx, qy, px*qx, py*qy, color, dither), qx, qy)
                    positive += 1
        for px, py in [(0, 0), (.4, .4), (.8, .2)]:
            for w, h in [(7, 7), (12, 12), (18, 18), (24, 24), (20, 8), (8, 20), (24, 10), (10, 24)]:
                rectangle = lambda x, y: 3 <= x <= 3+w and 3 <= y <= 3+h
                mask = sampled_mask(rectangle, qx, qy, px*qx, py*qy, COLORS[0], False)
                rejects(lambda: shape(mask, qx, qy))
                negative += 1
    for w in range(1, 33):
        for h in range(1, 33):
            rejects(lambda: shape({(x, y) for y in range(h) for x in range(w)}, 2, 2))
            negative += 1
    for mask in [set(), {(0, 0), (23, 0), (0, 23)},
                 {(x, y) for y in range(24) for x in range(24-y) if x < 14 and y < 14},
                 {(x, y) for y in range(24) for x in range(24-y) if x == 0 or y == 0 or x+y == 23},
                 {(x, y) for y in range(24) for x in range(24-y) if x % 2 == 0},
                 {(x, y) for y in range(24) for x in range(24-y) if (5 <= x < 13 and 5 <= y < 13) or x == 0 or y == 0}]:
        rejects(lambda: shape(mask, 2, 2))
        negative += 1

    def image(colors=COLORS, centre=(2, 2), scale=1, shift=0, square=False):
        width, height = 640, 480
        rgb = bytearray(bytes((50, 50, 50))*(width*height))
        for y in range(80, 380):
            rgb[(y*width+100)*3:(y*width+500)*3] = bytes(BG)*400
        for color, (ox, oy) in zip(colors, OFFSETS):
            origin = (300+(ox-centre[0])*12+shift, 230+(oy-centre[1])*12)
            for y in range(190, 270):
                for x in range(250, 340):
                    dx, dy = x+.5-origin[0], y+.5-origin[1]
                    hit = dx >= 0 and dy >= 0 and (max(dx, dy) < 24*scale if square else dx+dy <= 24*scale)
                    if hit:
                        rgb[(y*width+x)*3:(y*width+x+1)*3] = bytes(color)
        return bytes(rgb)

    baseline = image()
    pixels(640, 480, baseline, [400, 300], (2, 2), False)
    pixels(640, 480, image(colors=(FADED, *COLORS[1:])), [400, 300], (2, 2), True)
    pixels(640, 480, image(centre=(3, 2)), [400, 300], (3, 2), False)
    for bad in [image(colors=COLORS[::-1]), image(colors=(BG, *COLORS[1:])), image(shift=10),
                image(scale=.5), image(scale=2), image(square=True)]:
        rejects(lambda: pixels(640, 480, bad, [400, 300], (2, 2), False))
    rejects(lambda: pixels(640, 480, baseline, [400, 300], (3, 2), False))
    rejects(lambda: pixels(640, 480, baseline, [400, 300], (2, 2), True))
    rejects(lambda: pixels(640, 480, baseline, [800, 600], (2, 2), False))
    rejects(lambda: pixels(640, 480, baseline[:-1], [400, 300], (2, 2), False))
    rejects(lambda: pixels(640, 480, bytes(640*480*3), [400, 300], (2, 2), False))
    ambiguous = bytearray(bytes((50, 50, 50))*(1000*480))
    for y in range(480):
        ambiguous[y*1000*3:(y*1000+640)*3] = baseline[y*640*3:(y+1)*640*3]
        if 80 <= y < 380:
            ambiguous[(y*1000+502)*3:(y*1000+902)*3] = bytes(BG)*400
    rejects(lambda: pixels(1000, 480, bytes(ambiguous), [400, 300], (2, 2), False))
    with tempfile.TemporaryDirectory(prefix='native-preview-failure-') as directory:
        out = Path(directory)
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                verify(out / 'missing-editor', out / 'missing-example', out / 'evidence')
            except FileNotFoundError:
                pass
            else:
                raise AssertionError('missing executable accepted')
        receipts = list((out / 'evidence').glob('run-*/result.json'))
        assert len(receipts) == 1
        failed = json.loads(receipts[0].read_text())
        assert failed['status'] == 'FAIL' and 'missing-editor' in failed['error'] and failed['cleanup'] is None
        assert 'pid' not in failed and not list((out / 'evidence').rglob('editor.log'))
    print(f'PASS native-preview self-test: {positive} sampled positives, {negative} shape negatives, '
          'full-frame controls, setup failure receipt')


def main():
    if not __debug__:
        raise RuntimeError('verification requires Python assertions; do not use -O')
    parser = argparse.ArgumentParser(description='Verify a synthetic CMO3 in the production Linux X11 editor window.')
    parser.add_argument('--editor', type=Path)
    parser.add_argument('--example', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
    elif args.editor and args.example and args.out:
        verify(args.editor.resolve(), args.example.resolve(), args.out.resolve())
    else:
        parser.error('--editor, --example and --out are required unless --self-test is given')


if __name__ == '__main__':
    main()
