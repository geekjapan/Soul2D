#!/usr/bin/env python3
"""Click through the editor panels that no other GUI script reaches, in a live window.

Areas (each launches its own editor with an isolated config directory, one at a time):
  a  parameters and bindings   b  animation   c  Motion3   d  depth tools
  e  agent panel               f  recovery notice   g  export (still image and frames)
  h  menu bar entries added after the first panels (View > Panels / Reset Layout / Stats for Nerds, File > Import
     and Export, Edit > Settings > OpenDyslexic)
  i  video export from the Tools panel (run to the end, progress, cancel), with the ffmpeg on PATH
  j  the File and Tools menu entries that open or change a document (Recent, Merge, Import PSD / puppet / image
     folder, the Tools entries, Texture Viewer, Flip Pairing) and the panel size after View > Reset Layout

Every check drives real controls through the accessibility tree of the editor window that this script
started (matched by pid), then asserts the Session through the editor's own stdio MCP and, for visual
effects, a screenshot property. A control that is not in the accessibility tree is reported INCONCLUSIVE,
never PASS. Documents are built here (INX bytes, `soul2d-core` MCP), never from third-party files.
"""
import argparse
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time
import traceback
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]


def load(name, rel):
    spec = importlib.util.spec_from_file_location(name, ROOT / rel)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


harness = load('animation_key_gui_harness', 'tools/verify-animation-key-gui.py')
core_harness = load('rust_core_harness', 'tools/verify-rust-core.py')
Inconclusive = harness.Inconclusive
Cua = harness.Cua
Mcp = harness.Mcp
stop_owned = harness.stop_owned

PASS, FAIL, INCONCLUSIVE, SKIP = 'PASS', 'FAIL', 'INCONCLUSIVE', 'SKIP'


# --------------------------------------------------------------------------------------------- fixtures

def transform():
    return {'trans': [0.0, 0.0, 0.0], 'rot': [0.0, 0.0, 0.0], 'scale': [1.0, 1.0]}


def node(uuid, name, children=(), kind='Node', **extra):
    body = {'uuid': uuid, 'name': name, 'type': kind, 'enabled': True, 'zsort': 0.0,
            'transform': transform(), 'children': list(children), 'lockToRoot': False, 'pinToMesh': False}
    body.update(extra)
    return body


def part(uuid, name, texture=0):
    return node(uuid, name, kind='Part',
                mesh={'verts': [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0],
                      'uvs': [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                      'indices': [0, 1, 2, 0, 2, 3], 'origin': [0.0, 0.0]},
                vertices=[[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]],
                uvs=[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], indices=[0, 1, 2, 0, 2, 3],
                textures=[texture], blend_mode='Normal', tint=[1, 1, 1], screenTint=[0, 0, 0],
                emissionStrength=1, opacity=1)


def inx_with_texture(model, pixels=bytes([255, 0, 0, 255]) * 4, extensions=()):
    """An INX file whose single texture is a 2x2 PNG (opaque red unless `pixels` says otherwise), with the
    `extensions` (name, payload) entries."""
    texture = core_harness.png(2, 2, pixels)
    raw = json.dumps(model, separators=(',', ':')).encode()
    entries = b''.join(struct.pack('>I', len(part)) + part for entry in extensions for part in entry)
    return (b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw
            + b'TEX_SECT' + struct.pack('>I', 1) + struct.pack('>IB', len(texture), 0) + texture
            + b'EXT_SECT' + struct.pack('>I', len(extensions)) + entries)


def param(uuid, name, vec2=False):
    return {'uuid': uuid, 'name': name, 'is_vec2': vec2, 'min': [0.0, 0.0], 'max': [1.0, 1.0],
            'defaults': [0.0, 0.0], 'axis_points': [[0.0, 1.0], [0.0, 1.0] if vec2 else [0.0]],
            'merge_mode': 'Additive'}


def card_model():
    """A root with one textured quad Part and no parameters or animations."""
    return {'meta': {'name': 'fixture', 'version': '1.0-alpha'},
            'nodes': node(1, 'Fixture', [part(5, 'Card')]), 'param': [], 'animations': {}}


# ------------------------------------------------------------------------------------------- screenshots

def load_pixels(path):
    """(width, height, RGBA bytes) of a PNG; Pillow when installed, else the stdlib decoder of the core harness."""
    try:
        from PIL import Image
    except ImportError:
        return core_harness.decode_rgba(Path(path).read_bytes())
    image = Image.open(path).convert('RGBA')
    return image.width, image.height, image.tobytes()


def changed_pixels(before, after, region=None):
    """Number of pixels that differ between two screenshots, optionally inside region (x0, y0, x1, y1)."""
    (w1, h1, a), (w2, h2, b) = load_pixels(before), load_pixels(after)
    assert (w1, h1) == (w2, h2), ((w1, h1), (w2, h2))
    x0, y0, x1, y1 = region or (0, 0, w1, h1)
    count = 0
    for y in range(max(0, int(y0)), min(h1, int(y1))):
        row = y * w1 * 4
        ra, rb = a[row + int(x0) * 4:row + int(x1) * 4], b[row + int(x0) * 4:row + int(x1) * 4]
        if ra != rb:
            count += sum(1 for i in range(0, len(ra), 4) if ra[i:i + 4] != rb[i:i + 4])
    return count


def find_pixels(path, region, predicate):
    """Pixel coordinates inside region whose RGBA value satisfies predicate."""
    width, height, data = load_pixels(path)
    x0, y0, x1, y1 = (int(v) for v in region)
    found = []
    for y in range(max(0, y0), min(height, y1)):
        for x in range(max(0, x0), min(width, x1)):
            i = (y * width + x) * 4
            if predicate(data[i], data[i + 1], data[i + 2], data[i + 3]):
                found.append((x, y))
    return found


def quad_corner(path, region, inset=6):
    """Top-left corner (screenshot pixels) of the filled red quad in region. The red X axis of the orientation
    gizmo is as red as the quad but 1-3 px thick, so only pixels with red `inset` px away on every side count."""
    red = set(find_pixels(path, region, lambda r, g, b, a: r > 200 and g < 40 and b < 40))
    inner = [(x, y) for x, y in red
             if all((x + dx, y + dy) in red for dx, dy in ((inset, 0), (-inset, 0), (0, inset), (0, -inset)))]
    if len(inner) < 50:
        raise Check(f'the red quad was not found in the viewport screenshot ({len(red)} red, {len(inner)} interior pixels)')
    return min(x for x, _ in inner) - inset, min(y for _, y in inner) - inset


def white_dots(path, region):
    """Centres (screenshot pixels) of the white vertex handles drawn in region: connected white blobs of 15-150 px."""
    white = set(find_pixels(path, region, lambda r, g, b, a: r > 245 and g > 245 and b > 245))
    dots, seen = [], set()
    for start in sorted(white):
        if start in seen:
            continue
        blob, todo = [], [start]
        seen.add(start)
        while todo:
            x, y = todo.pop()
            blob.append((x, y))
            for n in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
                if n in white and n not in seen:
                    seen.add(n)
                    todo.append(n)
        if 15 <= len(blob) <= 150:
            dots.append((round(sum(x for x, _ in blob) / len(blob)), round(sum(y for _, y in blob) / len(blob))))
    return dots


def lattice(dots, columns, rows, tolerance=6):
    """Group dots into a rows x columns lattice (row-major); raise Check unless every handle was found."""
    def groups(values):
        out = []
        for v in sorted(values):
            if out and v - out[-1][-1] <= tolerance:
                out[-1].append(v)
            else:
                out.append([v])
        return [round(sum(g) / len(g)) for g in out]
    xs, ys = groups([x for x, _ in dots]), groups([y for _, y in dots])
    if len(xs) != columns or len(ys) != rows:
        raise Check(f'expected a {columns}x{rows} lattice of handles, found {len(xs)}x{len(ys)} ({len(dots)} dots)')
    return [[(x, y) for x in xs] for y in ys]


# ------------------------------------------------------------------------------------------ the editor

def screen_locked():
    """True when macOS reports the console session as locked (nothing can be observed in a window then)."""
    try:
        out = subprocess.run(['ioreg', '-n', 'Root', '-d1', '-a'], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return False
    return re.search(r'CGSSessionScreenIsLocked</key>\s*<true/>', out) is not None


class Check(Exception):
    """Raised inside a step to report INCONCLUSIVE with a reason (a control that cannot be driven)."""


class Editor:
    """One launched editor window plus the accessibility and MCP handles to drive and read it."""

    def __init__(self, suite, area, fixture=None, settings=None):
        self.suite, self.area = suite, area
        # An editor started without a document draws no side panels, only the status bar.
        self.settle_text = 'Parameters' if fixture else ' rev '
        self.dir = suite.run / area
        self.dir.mkdir()
        self.cua = Cua(self.dir)
        self.wire = (self.dir / 'mcp-wire.jsonl').open('w')
        self.proc = self.session = None
        self.shots = 0
        try:
            env = harness.editor_env(self.dir)
            if settings:
                path = self.dir / 'config' / 'settings.json'
                merged = json.loads(path.read_text())
                merged.update(settings)
                path.write_text(json.dumps(merged))
            self.config = self.dir / 'config'
            args = [str(suite.binary), '--mcp'] + (['--open', str(fixture)] if fixture else [])
            self.proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=(self.dir / 'editor-stderr.log').open('w'), text=True, env=env)
            self.pid = self.proc.pid
            self.mcp = Mcp(self.proc, self.wire)
            self.session = f'panels-{area}-{self.pid}'
            self.cua('start_session', {'session': self.session})
            self.window = self._find_window()
            self.target = {'kind': 'window', 'pid': self.pid, 'window_id': self.window}
            self.mcp.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                                        'clientInfo': {'name': 'gui-panels', 'version': '1'}})
            self._settle()
        except BaseException:
            # The caller never receives this instance, so release what was started: only the pid launched above.
            self.stop()
            raise

    def _settle(self):
        """Wait for the first frames: the side panels are drawn only once the document has loaded."""
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.has_text(self.state('settle'), self.settle_text):
                return
            time.sleep(0.5)
        raise AssertionError('the editor panels did not appear')

    def _find_window(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            for w in self.cua('list_windows', {'pid': self.pid}).get('windows', []):
                if w.get('pid') == self.pid and (w.get('title') or '').startswith('Soul2D'):
                    return w['window_id']
            time.sleep(0.5)
        raise AssertionError('no editor window')

    def stop(self):
        if self.session:
            try:
                self.cua('end_session', {'session': self.session}, ok=False)
            except Exception:
                pass
        # A video export leaves an encoder running under the editor; it is stopped with it, by pid.
        children = self.children() if self.proc else []
        stop_owned(self.proc)
        for pid in children:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        self.cua.close()
        self.wire.close()

    def children(self):
        """Pids of the processes this editor started."""
        out = subprocess.run(['pgrep', '-P', str(self.pid)], capture_output=True, text=True).stdout
        return [int(pid) for pid in out.split()]

    # ---- reading
    def state(self, name=None):
        self.shots += 1
        name = f'{self.shots:03d}-{name or "state"}'.replace('/', '-').replace(' ', '_')
        body = {}
        for _ in range(6):
            body = self.cua('get_window_state', {
                'pid': self.pid, 'window_id': self.window, 'session': self.session,
                'timeout_ms': 8000, 'screenshot_out_file': str(self.dir / f'{name}.png')})
            if body.get('elements_complete') and any(e.get('role') == 'AXStaticText' for e in body.get('elements', [])):
                break
            time.sleep(0.7)
        if not body.get('elements') and body.get('degraded_reason'):
            # The driver refuses to ground actions when the window has no accessibility surface (locked screen,
            # window on another Space, screen capture denied). Nothing can be asserted, so this is not a FAIL.
            raise harness.Inconclusive(f'the editor window cannot be observed: {body["degraded_reason"][:160]}')
        assert body['pid'] == self.pid and body['window_id'] == self.window
        assert body.get('screenshot_frame_valid') is True
        (self.dir / f'{name}-ax.json').write_text(json.dumps(body, indent=1))
        body['_png'] = self.dir / f'{name}.png'
        return body

    @staticmethod
    def text(e):
        return e.get('label') or e.get('value')

    def find(self, state, label, role=None, enabled=None, after=None):
        """Elements labelled label (optionally of a role, enabled state, and following the element labelled `after`)."""
        elements = state['elements']
        start = 0
        if after is not None:
            at = [i for i, e in enumerate(elements) if self.text(e) == after]
            if not at:
                return []
            start = at[0]
        return [e for e in elements[start:] if self.text(e) == label and (role is None or e.get('role') == role)
                and (enabled is None or e.get('enabled', True) == enabled)]

    def one(self, state, label, role=None, after=None, enabled=None):
        found = self.find(state, label, role, enabled, after)
        if not found:
            raise Check(f'control {label!r} ({role or "any role"}) is not in the accessibility tree')
        return found[0]

    def after(self, state, caption, role):
        """The first element of `role` that follows the element labelled `caption` in tree order (a field next to its label)."""
        elements = state['elements']
        at = [i for i, e in enumerate(elements) if self.text(e) == caption]
        if not at:
            raise Check(f'caption {caption!r} is not in the accessibility tree')
        found = [e for e in elements[at[0] + 1:] if e.get('role') == role]
        if not found:
            raise Check(f'no {role} follows the caption {caption!r}')
        return found[0]

    def present(self, state, label, role=None):
        return bool(self.find(state, label, role))

    def has_text(self, state, fragment):
        return any(fragment in str(self.text(e) or '') for e in state['elements'] if e.get('role') == 'AXStaticText')

    def view(self, state):
        """Screenshot-pixel rectangle of the model viewport canvas."""
        canvas = [e for e in state['elements'] if e.get('label') == 'Model viewport']
        if not canvas:
            raise Check('the model viewport is not in the accessibility tree')
        f = canvas[0]['screenshot_frame']
        return f['x'], f['y'], f['x'] + f['w'], f['y'] + f['h']

    # ---- acting
    def click(self, element):
        self.cua('click', {'target': self.target, 'element_token': element['element_token'], 'session': self.session})
        time.sleep(0.5)

    def press(self, label, role='AXButton', after=None, name=None):
        state = self.state(name or f'before-{label}')
        self.click(self.one(state, label, role, after))
        return state

    def set_value(self, element, value):
        self.cua('set_value', {'pid': self.pid, 'element_token': element['element_token'],
                               'value': str(value), 'session': self.session})
        time.sleep(0.5)

    def type_into(self, element, text):
        self.click(element)
        self.cua('type_text', {'pid': self.pid, 'element_token': element['element_token'],
                               'text': text, 'session': self.session})
        time.sleep(0.5)

    def drag(self, start, end, steps=25):
        """Press-drag-release on the canvas. The driver cannot do this in the background (`background_unavailable`),
        so it briefly fronts this editor window (and only it) when --foreground-drags is given."""
        payload = {'pid': self.pid, 'window_id': self.window, 'from_x': start[0], 'from_y': start[1],
                   'to_x': end[0], 'to_y': end[1], 'duration_ms': 700, 'steps': steps, 'session': self.session}
        body = self.cua('drag', payload, ok=False)
        if body.get('code') == 'background_unavailable':
            if not self.suite.foreground_drags:
                raise Check('canvas drags need the driver to front the editor window; rerun with --foreground-drags')
            body = self.cua('drag', dict(payload, delivery_mode='foreground'), ok=False)
        assert body.get('_exitCode', 0) == 0 and not body.get('isError'), body
        time.sleep(0.7)

    def click_at(self, point):
        """Pixel click on the canvas (window-local screenshot pixels). The driver's background route turns a pixel
        click into an AXPress on the canvas element (route "accessibility"), which egui never sees as a pointer
        event at that position, so a real mouse click is made in the foreground: it briefly fronts this editor
        window (and only it) and needs --foreground-drags."""
        if not self.suite.foreground_drags:
            raise Check('canvas clicks need the driver to front the editor window; rerun with --foreground-drags')
        payload = {'pid': self.pid, 'window_id': self.window, 'x': point[0], 'y': point[1], 'session': self.session,
                   'delivery_mode': 'foreground'}
        body = self.cua('click', payload, ok=False)
        assert body.get('_exitCode', 0) == 0 and not body.get('isError'), body
        time.sleep(0.6)

    def wait_text(self, fragment, name, seconds=15, present=True):
        """Poll until some static text contains `fragment` (or, with present=False, until none does).
        While an agent runs the window repaints continuously and the accessibility walk can stay incomplete;
        that is reported INCONCLUSIVE rather than as a missing text."""
        deadline = time.monotonic() + seconds
        while True:
            state = self.state(name)
            if self.has_text(state, fragment) == present:
                return state
            if time.monotonic() > deadline:
                if not state.get('elements_complete'):
                    raise Check(f'the accessibility tree stayed incomplete while waiting for {fragment!r}')
                raise AssertionError(f'static text {fragment!r} {"never appeared" if present else "did not go away"}')
            time.sleep(1.0)

    def pop_menu(self, button, item, name):
        """Open the menu button and choose an item (menus are popups of buttons in the egui tree)."""
        state = self.state(f'{name}-closed')
        self.click(self.one(state, button, 'AXButton'))
        state = self.state(f'{name}-open')
        self.click(self.one(state, item, 'AXButton'))

    def snap(self):
        return self.mcp.snapshot()

    def tool(self, tool, arguments=None):
        """Call an MCP tool whose arguments may include a `name` key (Mcp.op reserves that keyword)."""
        out = self.mcp.rpc('tools/call', {'name': tool, 'arguments': arguments or {}})
        assert not out.get('isError', False), out
        return out.get('structuredContent') or json.loads(out['content'][0]['text'])


class Suite:
    def __init__(self, binary, run, foreground_drags=False):
        self.binary, self.run, self.foreground_drags = binary, run, foreground_drags
        self.results = []

    def record(self, area, name, status, detail=''):
        self.results.append({'area': area, 'check': name, 'status': status, 'detail': detail})
        print(f'{status:13s} {area}/{name} {detail}'.rstrip(), flush=True)

    def area(self, area, fixture=None, settings=None):
        return Area(self, area, fixture, settings)


class Area:
    """Runs the checks of one area against one editor; a failure blocks the rest of that area only."""

    def __init__(self, suite, area, fixture, settings):
        self.suite, self.name = suite, area
        self.blocked = None
        self.ed = None
        try:
            self.ed = Editor(suite, area, fixture, settings)
        except harness.Inconclusive as error:
            self.blocked = (INCONCLUSIVE, f'could not start the editor: {error}')
        except Exception as error:
            # Only a cause this script positively identifies makes a start-up failure INCONCLUSIVE.
            if screen_locked():
                self.blocked = (INCONCLUSIVE, f'could not start the editor: the screen is locked ({error})')
            else:
                reason = f'{type(error).__name__}: {error}'
                suite.record(area, 'startEditor', FAIL, reason)
                self.blocked = (FAIL, f'could not start the editor: {reason}')

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        if self.ed is not None:
            self.ed.stop()
        return False

    def step(self, name, fn):
        if self.blocked:
            status, reason = self.blocked
            self.suite.record(self.name, name, INCONCLUSIVE if status == FAIL else status, f'blocked: {reason}')
            return None
        try:
            detail = fn(self.ed)
            self.suite.record(self.name, name, PASS, detail or '')
            return True
        except Check as error:
            self.suite.record(self.name, name, INCONCLUSIVE, str(error))
            self.blocked = (INCONCLUSIVE, f'{name}: {error}')
        except harness.Inconclusive as error:
            self.suite.record(self.name, name, INCONCLUSIVE, str(error))
            self.blocked = (INCONCLUSIVE, f'{name}: {error}')
        except Exception:
            self.suite.record(self.name, name, FAIL, traceback.format_exc().strip().splitlines()[-1])
            (self.suite.run / self.name / f'{name}-traceback.txt').write_text(traceback.format_exc())
            self.blocked = (FAIL, name)
        return None


# ------------------------------------------------------------------------------------------ the areas

def area_parameters(suite):
    fixture = suite.run / 'parameters.inx'
    fixture.write_bytes(inx_with_texture(card_model()))
    with suite.area('parameters', fixture) as area:
        keep = {}

        def create(ed):
            ed.pop_menu('+ Add parameter', 'Add 1D Parameter (-1..1)', 'add')
            snap = ed.snap()
            params = snap['model']['param']
            assert len(params) == 1, params
            keep['id'] = params[0]['uuid']
            keep['revision'] = snap['revision']
            (suite.run / 'parameters').joinpath('snapshot-after-create.json').write_text(json.dumps(snap, indent=1))
            return f"parameter {params[0]['name']!r} uuid {keep['id']} min {params[0]['min']} max {params[0]['max']}"

        area.step('createFromPanel', create)

        def slide(ed):
            state = ed.state('before-slide')
            slider = ed.one(state, 'X', 'AXSlider', after='Parameters')
            ed.set_value(slider, 0.5)
            values = ed.snap()['values'][str(keep['id'])]
            assert abs(values[0] - 0.5) < 1e-6, values
            return f'slider X set to 0.5, session value {values}'

        area.step('moveSlider', slide)

        def arm(ed):
            state = ed.state('before-arm')
            assert ed.has_text(state, 'No parameter is armed'), 'expected nothing armed before the click'
            ed.click(ed.one(state, 'Arm', 'AXButton'))
            state = ed.state('armed')
            assert ed.present(state, 'Armed', 'AXButton'), 'the Arm button did not become Armed'
            assert ed.has_text(state, 'Bindings at key'), 'the armed parameter shows no binding list'
            assert ed.has_text(state, 'No bindings'), 'a fresh parameter should have no bindings'
            return 'armed; panel lists the bindings of the closest keypoint'

        area.step('armParameter', arm)

        def select_card(ed):
            state = ed.state('before-select')
            ed.click(ed.one(state, 'Card #5', 'AXButton'))
            state = ed.state('card-selected')
            ed.view(state)
            return 'Card #5 selected'

        area.step('selectNode', select_card)

        def bind(ed):
            # The transform handle is not offered while a parameter is armed (editor_ui: `handles: !armed`), so the
            # binding is made the way the armed viewport offers: the deform tool drags a vertex of the part.
            state = ed.state('before-drag')
            region = ed.view(state)
            top_left = quad_corner(state['_png'], region)
            keep['before'] = state['_png']
            keep['revision'] = ed.snap()['revision']
            ed.drag(top_left, (top_left[0] - 40, top_left[1] - 30))
            keep['after'] = ed.state('after-drag')['_png']
            snap = ed.snap()
            (suite.run / 'parameters' / 'snapshot-after-drag.json').write_text(json.dumps(snap, indent=1))
            assert snap['revision'] > keep['revision'], 'the drag did not change the document'
            bindings = snap['model']['param'][0]['bindings']
            assert bindings, 'the drag created no binding'
            keep['bindings'] = bindings
            keys = [(i, j) for i, row in enumerate(bindings[0]['isSet']) for j, v in enumerate(row) if v]
            assert len(keys) == 1, f'expected exactly one set keypoint, got {keys}'
            return f"binding(s) on the parameter: {json.dumps(bindings)[:300]}"

        area.step('setBindingByViewportDrag', bind)

        def keypoint_visible(ed):
            state = ed.state('bindings-listed')
            assert not ed.has_text(state, 'No bindings'), 'the panel still says No bindings'
            rows = [e for e in state['elements'] if 'Card' in str(ed.text(e) or '') and ':' in str(ed.text(e))]
            assert rows, 'the armed panel lists no "Card: ..." binding row'
            return f'panel row {ed.text(rows[0])!r}'

        area.step('bindingListedInPanel', keypoint_visible)

        def pixels_moved(ed):
            region = ed.view(ed.state('region'))
            moved = changed_pixels(keep['before'], keep['after'], region)
            assert moved > 200, f'only {moved} viewport pixels changed after the drag'
            return f'{moved} viewport pixels differ before/after the drag'

        area.step('viewportPixelsChanged', pixels_moved)

        def undo_redo(ed):
            before = ed.snap()
            ed.press('Undo')
            undone = ed.snap()
            assert undone['model']['param'][0]['bindings'] != before['model']['param'][0]['bindings'], 'Undo kept the binding'
            assert undone['canRedo'] is True
            ed.press('Redo')
            redone = ed.snap()
            assert redone['model']['param'][0]['bindings'] == before['model']['param'][0]['bindings'], 'Redo did not restore the binding'
            return f"undo -> {len(undone['model']['param'][0]['bindings'])} bindings, redo -> {len(redone['model']['param'][0]['bindings'])}"

        area.step('undoRedoOnce', undo_redo)


def area_animation(suite):
    model = card_model()
    model['param'] = [param(23, 'Blink')]
    fixture = suite.run / 'animation.inx'
    fixture.write_bytes(inx_with_texture(model))
    with suite.area('animation', fixture) as area:
        keep = {'name': 'Wave'}

        def create(ed):
            state = ed.state('before-create')
            field = ed.after(state, 'Animation name', 'AXTextField')
            ed.type_into(field, keep['name'])
            ed.click(ed.one(ed.state('typed'), 'Create animation', 'AXButton'))
            snap = ed.snap()
            animation = snap['model']['animations'].get(keep['name'])
            assert animation is not None, list(snap['model']['animations'])
            (suite.run / 'animation' / 'snapshot-after-create.json').write_text(json.dumps(snap, indent=1))
            return f"animation {keep['name']!r}: length {animation['length']}, timestep {animation['timestep']}"

        area.step('createAnimation', create)

        def slider(ed, value):
            ed.set_value(ed.one(ed.state('slider'), 'X', 'AXSlider', after='Parameters'), value)
            got = ed.snap()['values']['23'][0]
            assert abs(got - value) < 1e-6, got

        def seek(ed, frame):
            state = ed.state(f'seek-{frame}')
            ed.set_value(ed.after(state, 'Seek frame', 'AXIncrementor'), frame)
            ed.click(ed.one(ed.state('seek-set'), 'Seek', 'AXButton'))

        def lanes(ed):
            return ed.snap()['model']['animations'][keep['name']].get('lanes', [])

        def first_key(ed):
            slider(ed, 0.2)
            seek(ed, 0)
            ed.press('Add key')
            found = [l for l in lanes(ed) if l['uuid'] == 23]
            assert len(found) == 1 and found[0]['keyframes'][0]['frame'] == 0, found
            assert abs(found[0]['keyframes'][0]['value'] - 0.2) < 1e-6, found
            return f"lane for parameter 23 created by Add key at frame 0: {found[0]['keyframes']}"

        area.step('addLaneAndFirstKey', first_key)

        def second_key(ed):
            for _ in range(5):
                ed.press('Next frame')
            frame = ed.snap()['animation']['frame']
            assert abs(frame - 5) < 1e-6, f'the timeline Next frame x5 left the preview at frame {frame}'
            state = ed.state('after-next-frames')
            assert ed.has_text(state, 'Timeline: ' + keep['name'])
            # Seeking replays the lane, so the new value is set after the move: Add key keys the effective value.
            slider(ed, 0.9)
            ed.press('Add key')
            found = [l for l in lanes(ed) if l['uuid'] == 23][0]['keyframes']
            assert [k['frame'] for k in found] == [0, 5], found
            assert abs(found[1]['value'] - 0.9) < 1e-6, found
            return f'two keys: {found}'

        area.step('secondKeyFromTimelineSteps', second_key)

        def play_stop(ed):
            # The clip is 60 frames at 60 fps, so without Loop it ends within a second of pressing Play.
            ed.click(ed.one(ed.state('before-loop'), 'Loop', 'AXCheckBox'))
            seek(ed, 0)
            start = ed.snap()
            assert abs(start['values']['23'][0] - 0.2) < 1e-6, start['values']
            # Playing makes the window repaint every frame, and the driver's accessibility walk then times out, so
            # the controls are located first and the playback is read through MCP. A disabled button (Pause while
            # paused) is absent from the tree, so the stop used here is "Reset preview" (animation.reset pauses).
            before = ed.state('before-play')
            play, reset = ed.one(before, 'Play', 'AXButton'), ed.one(before, 'Reset preview', 'AXButton')
            ed.click(play)
            trace = []
            for _ in range(10):
                snap = ed.snap()
                trace.append((snap['animation']['frame'], snap['values']['23'][0]))
                time.sleep(0.1)
            (suite.run / 'animation' / 'playback-trace.json').write_text(json.dumps(trace))
            frames = [f for f, _ in trace]
            assert len(set(frames)) > 3 and max(frames) > 5, f'time did not advance during playback: {frames}'
            values = [v for _, v in trace]
            assert max(abs(v - 0.2) for v in values) > 1e-3, f'Blink never left 0.2 during playback: {values}'
            ed.click(reset)
            after = [ed.snap() for _ in range(3) if not time.sleep(0.25)]
            assert all(snap['animation'] is None for snap in after), f'the preview kept running: {[s["animation"] for s in after]}'
            assert ed.has_text(ed.state('stopped'), 'Paused')
            return (f"frames {min(frames):.1f}..{max(frames):.1f} over 1 s of playback, Blink "
                    f"{min(values):.2f}..{max(values):.2f}; Reset preview cleared the animation preview and the panel says Paused")

        area.step('playAndStop', play_stop)

        def keyed_on(ed, frame):
            return sorted({l['uuid'] for l in lanes(ed) if any(k['frame'] == frame for k in l['keyframes'])})

        def key_all(ed):
            # A second parameter, so "all" is more than the one lane Add key made. Frame 7 has no key yet.
            ed.tool('parameter_create', {'name': 'Turn', 'dimensions': 1})
            everyone = sorted(p['uuid'] for p in ed.snap()['model']['param'])
            assert len(everyone) == 2, everyone
            seek(ed, 7)
            # A disabled button is absent from the tree.
            assert not ed.find(ed.state('before-key-all'), 'Unkey all', 'AXButton'), 'Unkey all is offered on a frame without a key'
            ed.press('Key all')
            assert keyed_on(ed, 7) == everyone, lanes(ed)
            ed.press('Unkey all')
            assert keyed_on(ed, 7) == [], lanes(ed)
            assert keyed_on(ed, 5) == [23], f'the keys of another frame moved: {lanes(ed)}'
            return f'Key all keyed parameters {everyone} on frame 7 and Unkey all removed those keys; the key on frame 5 stayed'

        area.step('keyAllAndUnkeyAll', key_all)

        def key_all_chords(ed):
            if not ed.suite.foreground_drags:
                raise Check('a typed shortcut needs the driver to front the editor window; rerun with --foreground-drags')
            everyone = sorted(p['uuid'] for p in ed.snap()['model']['param'])
            for keys, want in ((['cmd', 'k'], everyone), (['cmd', 'shift', 'k'], [])):
                ed.cua('hotkey', {'pid': ed.pid, 'window_id': ed.window, 'keys': keys, 'delivery_mode': 'foreground',
                                  'session': ed.session})
                time.sleep(0.5)
                assert keyed_on(ed, 7) == want, f'{"+".join(keys)}: {lanes(ed)}'
            return f'Cmd+K keyed parameters {everyone} on frame 7 and Cmd+Shift+K removed those keys'

        area.step('keyAllShortcuts', key_all_chords)


def motion3_json():
    """A two-curve Motion3 clip, 10 s long: Blink rises 0 -> 1 and Look falls 1 -> 0 (one linear segment each)."""
    return {'Version': 3,
            'Meta': {'Duration': 10.0, 'Fps': 30.0, 'Loop': False, 'AreBeziersRestricted': True, 'CurveCount': 2,
                     'TotalSegmentCount': 2, 'TotalPointCount': 4, 'UserDataCount': 0, 'TotalUserDataSize': 0},
            'Curves': [{'Target': 'Parameter', 'Id': 'Blink', 'Segments': [0, 0, 0, 10, 1]},
                       {'Target': 'Parameter', 'Id': 'Look', 'Segments': [0, 1, 0, 10, 0]}]}


def area_motion3(suite):
    model = card_model()
    model['param'] = [param(23, 'Blink'), param(24, 'Look')]
    fixture = suite.run / 'motion3.inx'
    fixture.write_bytes(inx_with_texture(model))
    clip = suite.run / 'wave.motion3.json'
    clip.write_text(json.dumps(motion3_json()))
    with suite.area('motion3', fixture) as area:
        def motion_state(ed):
            return ed.snap()['motion3']

        def load(ed):
            state = ed.state('before-import')
            ed.type_into(ed.after(state, 'Motion3 name', 'AXTextField'), 'Wave')
            ed.type_into(ed.after(ed.state('named'), 'Motion3 JSON path', 'AXTextField'), str(clip))
            ed.click(ed.one(ed.state('pathed'), 'Import Motion3', 'AXButton', enabled=True))
            listed = ed.tool('motion3_list')
            (suite.run / 'motion3' / 'motion3-list.json').write_text(json.dumps(listed, indent=1))
            assert 'Wave' in json.dumps(listed), listed
            read = ed.tool('motion3_read', {'name': 'Wave'})
            curves = [c['Id'] for c in read['Curves']]
            assert 'Blink' in json.dumps(read) and 'Look' in json.dumps(read), read
            state = ed.state('imported')
            assert ed.has_text(state, 'Selected: Wave'), 'the panel does not show the imported motion as selected'
            return f'imported Wave with curves {curves}'

        area.step('importFromPanel', load)

        def map_curve(ed):
            state = ed.state('before-map')
            combo = ed.one(state, 'INX parameter', 'AXPopUpButton')
            ed.click(combo)
            state = ed.state('combo-open')
            option = [e for e in state['elements'] if ed.text(e) == 'Blink (23)' and e.get('role') != 'AXPopUpButton']
            if not option:
                raise Check('the INX parameter choices are not in the accessibility tree')
            ed.click(option[0])
            ed.click(ed.one(ed.state('chosen'), 'Add binding draft', 'AXButton', enabled=True))
            ed.click(ed.one(ed.state('drafted'), 'Apply bindings', 'AXButton', enabled=True))
            rows = ed.tool('motion3_bindings', {'name': 'Wave'})
            (suite.run / 'motion3' / 'motion3-bindings.json').write_text(json.dumps(rows, indent=1))
            assert '"Blink"' in json.dumps(rows) and '23' in json.dumps(rows), rows
            return f'mapping applied: {json.dumps(rows)[:200]}'

        area.step('mapCurveToParameter', map_curve)

        def seek(ed):
            state = ed.state('before-seek')
            ed.set_value(ed.after(state, 'Preview time (seconds)', 'AXIncrementor'), 5.0)
            ed.click(ed.one(ed.state('time-set'), 'Seek preview', 'AXButton', enabled=True))
            snap = ed.snap()
            active = snap['motion3']
            assert active and active['name'] == 'Wave' and abs(active['time'] - 5.0) < 1e-6, active
            blink = snap['values']['23'][0]
            assert abs(blink - 0.5) < 1e-3, f'Blink should be 0.5 at 5 s of a 10 s 0 -> 1 ramp, got {blink}'
            assert ed.has_text(ed.state('seeked'), 'Preview Wave at 5.000000')
            return f"motion3 state {json.dumps({k: active[k] for k in ('name', 'time', 'clipEnd') if k in active})}, Blink {blink:.3f}"

        area.step('seekPreview', seek)

        def play_pause(ed):
            ed.press('Stop / reset')
            # The tick caps dt at 0.1 s per frame, so a 10 s clip sought to 5 s ends within a second of Play and the
            # button then reads "Play" again; Loop keeps it playing until Pause is pressed.
            ed.click(ed.one(ed.state('before-loop'), 'Loop', 'AXCheckBox'))
            state = ed.state('before-play')
            play = ed.one(state, 'Play', 'AXButton')
            ed.click(play)
            trace = []
            for _ in range(8):
                active = ed.snap()['motion3']
                trace.append(None if active is None else active['time'])
                time.sleep(0.1)
            (suite.run / 'motion3' / 'playback-trace.json').write_text(json.dumps(trace))
            assert None not in trace and len(set(trace)) > 3, f'Motion3 time did not advance during playback: {trace}'
            # The same button reads "Pause" while playing (editor_motion3.rs), but the window repaints every frame
            # then and the driver's accessibility walk times out, so the label cannot be read. The Play button
            # located before playing is pressed again (same widget, same element token); that it acted as Pause is
            # asserted below through the Session (time held) and the button reading "Play" again once idle.
            ed.click(play)
            held = [ed.snap()['motion3']['time'] for _ in range(3) if not time.sleep(0.25)]
            assert len(set(held)) == 1, f'the preview kept moving after Pause: {held}'
            state = ed.state('paused')
            assert ed.present(state, 'Play', 'AXButton'), 'the button did not return to Play after Pause'
            return f'time {min(trace):.2f}..{max(trace):.2f} s while playing; held at {held[0]:.3f} s after Pause'

        area.step('playThenPause', play_pause)

        def stop_reset(ed):
            ed.press('Stop / reset')
            snap = ed.snap()
            assert snap['motion3'] is None or snap['motion3']['time'] == 0, snap['motion3']
            return f"after Stop / reset motion3 = {json.dumps(snap['motion3'])}"

        area.step('stopReset', stop_reset)


def build_with_core(suite, name, model, steps=()):
    """Install `model` in a headless `soul2d-core --mcp` session, run the (tool, arguments) steps and save an INX."""
    core = suite.binary.with_name('soul2d-core')
    out = suite.run / f'{name}.inx'
    proc = subprocess.Popen([str(core), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=(suite.run / f'{name}-core-stderr.log').open('w'), text=True)
    try:
        with (suite.run / f'{name}-core-wire.jsonl').open('w') as wire:
            mcp = Mcp(proc, wire)
            mcp.op('new', model=model)
            for tool, arguments in steps:
                mcp.rpc('tools/call', {'name': tool, 'arguments': arguments})
            mcp.op('save', path=str(out))
    finally:
        stop_owned(proc)
    return out


def grid_model(axis, depths=None):
    """A root with one GridDeformer whose lattice spans axis x axis (local units)."""
    count = len(axis) * len(axis)
    grid = node(5, 'Grid', kind='GridDeformer', dynamic=False, grid_axis_x=axis, grid_axis_y=axis,
                depths=depths or [0.0] * count)
    return {'meta': {'name': 'fixture', 'version': '1.0-alpha'}, 'nodes': node(1, 'Fixture', [grid]),
            'param': [], 'animations': {}}


def depth_png(path, size=32):
    """A grey ramp, left (far) to right (near), as an opaque RGBA PNG."""
    pixels = bytearray()
    for _ in range(size):
        for x in range(size):
            level = round(255 * x / (size - 1))
            pixels += bytes([level, level, level, 255])
    path.write_bytes(core_harness.png(size, size, bytes(pixels)))


def area_depth(suite):
    fixture = build_with_core(suite, 'depth', grid_model([-8.0, -4.0, 0.0, 4.0, 8.0]))
    layer = suite.run / 'ramp.png'
    depth_png(layer)
    with suite.area('depth', fixture) as area:
        keep = {}

        def grid_node(ed):
            return find_node(ed.snap()['model']['nodes'], 5)

        def enter(ed):
            state = ed.state('before-select')
            ed.click(ed.one(state, 'Grid #5', 'AXButton'))
            state = ed.state('grid-selected')
            ed.click(ed.one(state, 'Edit Depth', 'AXButton'))
            state = ed.state('depth-strip')
            for label in ('Direct Depth', 'Ring', 'Attached Point', 'Plane', 'Depth Draw', 'Apply', 'Cancel'):
                assert ed.present(state, label, 'AXButton') or ed.present(state, label), f'{label!r} missing from the depth strip'
            keep['revision'] = ed.snap()['revision']
            return 'Edit Depth opened the tool strip: Direct Depth, Ring, Attached Point, Plane, Depth Draw, Apply, Cancel'

        area.step('openDepthTools', enter)

        def draw_panel(ed):
            ed.click(ed.one(ed.state('before-draw'), 'Depth Draw', 'AXButton'))
            state = ed.state('draw-open')
            ed.type_into(ed.after(state, 'PNG path', 'AXTextField'), str(layer))
            ed.type_into(ed.after(ed.state('png-path'), 'Layer id', 'AXTextField'), 'ramp')
            ed.click(ed.one(ed.state('layer-id'), 'Load PNG', 'AXButton'))
            snap = ed.snap()
            assert snap['depthDraw']['layers'] == 1, snap['depthDraw']
            # Auto Bind Layers matches layer names to Parts; the ramp is named after no node, so bind it by hand.
            ed.click(ed.one(ed.state('loaded'), 'Bound to this grid', 'AXCheckBox'))
            assert ed.snap()['depthDraw']['bindings'] == 1, ed.snap()['depthDraw']
            ed.press('Compose Preview')
            ed.press('Apply DepthDraw')
            depths = grid_node(ed).get('depths')
            assert depths and max(depths) - min(depths) > 0.01, f'Apply DepthDraw left the depths flat: {depths}'
            return f'layer loaded, bound to the grid, composed and applied: depths {min(depths):.2f}..{max(depths):.2f}'

        area.step('depthDrawLoadBindComposeApply', draw_panel)

        def handles(ed, name):
            """Vertex handle positions (screenshot pixels). The first call reads the full 5x5 lattice off the screenshot;
            later calls place it again from its offsets to the viewport centre (the view neither zooms nor pans, but the
            viewport shrinks as the Depth Deformers list grows), and an operation's own overlay hides some white
            dots, so only 12 of the 25 predicted positions must still show a white handle."""
            state = ed.state(name)
            view = ed.view(state)
            cx, cy = (view[0] + view[2]) / 2, (view[1] + view[3]) / 2
            dots = white_dots(state['_png'], view)
            if 'offsets' not in keep:
                keep['offsets'] = [[(x - cx, y - cy) for x, y in row] for row in lattice(dots, 5, 5)]
            grid = [[(round(cx + dx), round(cy + dy)) for dx, dy in row] for row in keep['offsets']]
            shown = sum(1 for row in grid for x, y in row if any(abs(x - a) <= 3 and abs(y - b) <= 3 for a, b in dots))
            if shown < 12:
                raise Check(f'only {shown} of the 25 predicted vertex handles are drawn where the lattice was first seen')
            return state, grid

        def op_rows(ed, state):
            # editor_depth.rs `operations_ui` shows `<op_label>  <value>` (depth_draw.rs `op_label`: lower case, e.g.
            # "ring 3,4  0.50", "attached point 2  0.50", "plane 0,0  0.00"); the capitalised tool buttons have no value.
            rows = [e for e in state['elements'] if e.get('role') == 'AXButton' and ed.text(e)
                    and any(ed.text(e).startswith(k) for k in ('ring ', 'attached point ', 'plane ')) and '  ' in ed.text(e)]
            return [ed.text(e) for e in rows]

        def pick_tool(ed, tool):
            ed.click(ed.one(ed.state(f'before-{tool}'), tool, 'AXButton'))

        def ring(ed):
            # The open Depth Draw section fills the window height and leaves the viewport a 70 px strip that shows one
            # row of the lattice, so its header (the second "Depth Draw" button, after the tool strip's) collapses it.
            headers = ed.find(ed.state('before-collapse'), 'Depth Draw', 'AXButton')
            if len(headers) < 2:
                raise Check('the Depth Draw section header is not in the accessibility tree')
            ed.click(headers[-1])
            pick_tool(ed, 'Ring')
            state, grid = handles(ed, 'ring-handles')
            keep['ring_before'] = state['_png']
            ed.click_at(grid[1][1])
            state = ed.state('ring-made')
            rows = op_rows(ed, state)
            assert any(r.startswith('ring ') for r in rows), f'no ring entry in the Depth Deformers list: {rows}'
            return f'Ring operation created by a click: {rows}'

        area.step('ringToolClick', ring)

        def attached(ed):
            pick_tool(ed, 'Attached Point')
            state, grid = handles(ed, 'attached-handles')
            x, y = grid[3][3]
            ed.drag((x, y), (x, y - 40))
            state = ed.state('attached-made')
            rows = op_rows(ed, state)
            assert any(r.startswith('attached point ') for r in rows), f'no attached point entry: {rows}'
            return f'Attached Point operation created by a vertical drag: {rows}'

        area.step('attachedPointDrag', attached)

        def plane(ed):
            pick_tool(ed, 'Plane')
            state, grid = handles(ed, 'plane-handles')
            x, y = grid[2][2]
            ed.drag((x, y), (x + 70, y + 30))
            state = ed.state('plane-made')
            rows = op_rows(ed, state)
            assert any(r.startswith('plane ') for r in rows), f'no plane entry: {rows}'
            return f'Plane operation created by a drag: {rows}'

        area.step('planeToolDrag', plane)

        def edit_selected(ed):
            state = ed.state('before-edit-op')
            amount = ed.after(state, 'Selected Deformer', 'AXIncrementor')
            ed.set_value(amount, 0.5)
            state = ed.state('after-edit-op')
            before = len(op_rows(ed, state))
            ed.click(ed.one(state, 'Duplicate', 'AXButton'))
            state = ed.state('duplicated')
            assert len(op_rows(ed, state)) == before + 1, (before, op_rows(ed, state))
            ed.click(ed.one(state, 'Delete', 'AXButton', after='Duplicate'))
            state = ed.state('deleted')
            assert len(op_rows(ed, state)) == before, op_rows(ed, state)
            return f'Selected Deformer edited, duplicated and deleted; {before} operations remain: {op_rows(ed, state)}'

        area.step('editSelectedOperation', edit_selected)

        def apply_ops(ed):
            before = grid_node(ed)
            ed.click(ed.one(ed.state('before-apply'), 'Apply', 'AXButton'))
            node_after = grid_node(ed)
            ops = node_after.get('depth-ops') or []
            assert len(ops) >= 3, f'Apply stored {len(ops)} depth operations'
            assert node_after.get('depths') != before.get('depths'), 'Apply left the depth array unchanged'
            kinds = sorted({op['type'] for op in ops})
            return f'Apply stored {len(ops)} operations ({", ".join(kinds)}) and rewrote the depth array'

        area.step('applyOperations', apply_ops)


FAKE_AGENT = r"""
import json, sys
log = open(sys.argv[1], 'a')
def reply(request, result):
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
for line in sys.stdin:
    line = line.strip()
    if not line.startswith('{'):
        continue
    request = json.loads(line)
    method = request.get('method')
    log.write(method + '\n')
    log.flush()
    if method == 'initialize':
        reply(request, {'protocolVersion': 1, 'agentCapabilities': {}})
    elif method == 'session/new':
        reply(request, {'sessionId': 'fake-session'})
    elif method == 'session/prompt':
        text = request['params']['prompt'][0]['text']
        log.write('prompt: ' + text + '\n')
        log.flush()
        update = {'sessionId': 'fake-session',
                  'update': {'sessionUpdate': 'agent_message_chunk',
                             'content': {'type': 'text', 'text': 'FAKE-AGENT-REPLY: ' + text}}}
        print(json.dumps({'jsonrpc': '2.0', 'method': 'session/update', 'params': update}), flush=True)
        reply(request, {'stopReason': 'end_turn'})
"""


def echo_agent(suite):
    """The repo's `acp-echo` example, built on demand (offline)."""
    path = suite.binary.parent / 'examples' / 'acp-echo'
    if not path.exists():
        subprocess.run(['cargo', 'build', '--offline', '--example', 'acp-echo', '--manifest-path',
                        str(ROOT / 'rust/Cargo.toml'), '--target-dir', str(ROOT / 'out/rust')], check=True)
    if not path.exists():
        raise Check(f'the acp-echo example is not at {path}')
    return path


def area_agent(suite):
    echo = echo_agent(suite)
    fake = suite.run / 'fake-agent.py'
    fake.write_text(FAKE_AGENT)
    agent_log = suite.run / 'fake-agent.log'
    fixture = suite.run / 'agent.inx'
    fixture.write_bytes(inx_with_texture(card_model()))
    with suite.area('agent', fixture) as area:
        def command_field(ed, state=None):
            return ed.after(state or ed.state('command-field'), 'Command', 'AXTextField')

        def open_panel(ed):
            state = ed.state('before-open')
            header = [e for e in state['elements'] if ed.text(e) == 'Coding agent']
            if not header:
                raise Check('the "Coding agent" section is not in the accessibility tree')
            ed.click(header[0])
            ed.click(ed.one(ed.state('opened'), 'Enable coding agent (ACP)', 'AXCheckBox'))
            state = ed.state('enabled')
            field = command_field(ed, state)
            assert not (field.get('value') or ''), f'ACP.Command is unexpectedly set: {field.get("value")!r}'
            ed.click(ed.one(state, 'Start', 'AXButton'))
            state = ed.wait_text('ACP.Command is not set', 'unset-start')
            assert ed.has_text(state, 'Start failed'), 'the status line does not say that Start failed'
            return 'with ACP.Command unset, Start shows "ACP.Command is not set; enter the command that starts an ACP agent"'

        area.step('unsetCommandPrompt', open_panel)

        def type_command(ed, command):
            # egui's TextEdit answers no accessibility SetValue (only DragValue and Slider do), so the text is typed
            # like every other text field here, after Cmd+A so that an earlier command is replaced.
            field = command_field(ed)
            frame = field['screenshot_frame']
            if ed.suite.foreground_drags:
                # The field sits far below the window until the side panel is scrolled (mouse wheel over the panel).
                height = png_size(ed.state('command-height')['_png'])[1]
                for _ in range(8):
                    if frame['y'] + frame['h'] < height - 40:
                        break
                    ed.cua('scroll', {'pid': ed.pid, 'window_id': ed.window, 'x': 300, 'y': 500, 'direction': 'down',
                                      'amount': 20, 'session': ed.session})
                    time.sleep(0.5)
                    field = command_field(ed)
                    frame = field['screenshot_frame']
                if frame['y'] + frame['h'] >= height - 40:
                    raise Check('the Command field could not be scrolled into the window')
                # An accessibility press does not give the text field keyboard focus; a real click does.
                ed.click_at((frame['x'] + frame['w'] / 2, frame['y'] + frame['h'] / 2))
            else:
                ed.click(field)
            # A background Cmd+A is not seen by egui, so the editor window is fronted briefly (and only it).
            if ed.suite.foreground_drags:
                ed.cua('hotkey', {'pid': ed.pid, 'window_id': ed.window, 'keys': ['cmd', 'a'],
                                  'delivery_mode': 'foreground', 'session': ed.session})
            field = command_field(ed)
            ed.cua('type_text', {'pid': ed.pid, 'element_token': field['element_token'], 'text': command,
                                 'session': ed.session})
            time.sleep(0.5)
            shown = command_field(ed, ed.state('command-typed')).get('value') or ''
            if shown != command:
                raise Check(f'the Command field cannot be set through the accessibility tree (shows {shown!r})')

        def send(ed, text, name):
            box = ed.after(ed.state(f'{name}-box'), 'User message:', 'AXTextField')
            ed.type_into(box, text)
            ed.click(ed.one(ed.state(f'{name}-typed'), 'Send', 'AXButton'))

        def echo_agent_step(ed):
            type_command(ed, str(echo))
            ed.click(ed.one(ed.state('before-echo-start'), 'Start', 'AXButton'))
            ed.wait_text('Status: Initialized', 'echo-init')
            send(ed, 'hello echo', 'echo')
            state = ed.wait_text('Session failed', 'echo-session')
            assert ed.has_text(state, 'sessionId'), 'the error does not name the missing sessionId'
            return 'acp-echo initializes, but its session/new reply has no sessionId, so no prompt reply is possible'

        area.step('echoAgentHandshakeOnly', echo_agent_step)

        def fake_agent_step(ed):
            ed.click(ed.one(ed.state('before-stop'), 'Stop', 'AXButton'))
            ed.wait_text('Status: Stopped', 'stopped')
            type_command(ed, f'{sys.executable} {fake} {agent_log}')
            ed.click(ed.one(ed.state('before-fake-start'), 'Start', 'AXButton'))
            ed.wait_text('Status: Initialized', 'fake-init')
            send(ed, 'ping', 'fake')
            ed.wait_text('FAKE-AGENT-REPLY: ping', 'fake-reply', seconds=20)
            ed.wait_text('Status: Done', 'fake-done')
            seen = agent_log.read_text().splitlines()
            assert 'prompt: ping' in seen and 'session/new' in seen, seen
            return 'prompt "ping" reached the in-script ACP agent and its reply is shown in the conversation'

        area.step('promptAndReply', fake_agent_step)


def area_recovery(suite):
    with suite.area('recovery') as area:
        keep = {}

        def recovery_files(ed):
            # autosave.rs: RECOVERY_DIR "RustRecovery", file `rust-recovery-<unix time>-<pid>.<ext>` (untitled INX: .inx).
            folder = ed.config / 'RustRecovery'
            return sorted(folder.glob('rust-recovery-*.inx')) if folder.is_dir() else []

        def edit(ed):
            ed.mcp.op('new', model=card_model())
            ed.pop_menu('+ Add parameter', 'Add 1D Parameter (-1..1)', 'add')
            snap = ed.snap()
            assert snap['modified'] and len(snap['model']['param']) == 1, snap['modified']
            assert not recovery_files(ed), 'a recovery snapshot exists before the 30 s interval'
            keep['edited'] = time.monotonic()
            return 'untitled document modified by adding a parameter'

        area.step('modifyUntitledDocument', edit)

        def notice(ed):
            # RECOVERY_INTERVAL_SECS is 30; the editor wakes itself every 15 s while idle.
            deadline = time.monotonic() + 75
            while not recovery_files(ed):
                if time.monotonic() > deadline:
                    raise AssertionError('no recovery snapshot appeared within 75 s of the edit')
                time.sleep(2.0)
            files = recovery_files(ed)
            assert len(files) == 1, files
            assert files[0].read_bytes()[:7] == b'TRNSRTS', 'the snapshot is not an INX file'
            keep['snapshot'] = files[0]
            state = ed.wait_text('Saved recovery snapshot', 'notice')
            assert ed.has_text(state, files[0].name), 'the notice does not name the snapshot file'
            return f'snapshot {files[0].name} written {time.monotonic() - keep["edited"]:.0f} s after the edit, announced in the status bar'

        area.step('snapshotAndNotice', notice)

        def save(ed):
            target = suite.run / 'recovery' / 'saved.inx'
            ed.type_into(ed.after(ed.state('before-save-path'), 'Path', 'AXTextField'), str(target))
            ed.click(ed.one(ed.state('path-typed'), 'Save As', 'AXButton'))
            ed.wait_text('Saved recovery snapshot', 'notice-cleared', present=False)
            assert target.is_file(), 'Save As wrote no file'
            assert not keep['snapshot'].exists(), 'the recovery snapshot survived the save'
            assert not ed.snap()['modified'], 'the document is still modified after saving'
            return 'Save As cleared the notice and removed the snapshot file'

        area.step('saveClearsNoticeAndSnapshot', save)


def export_model():
    """Card_model plus a camera over the quad (8x8 at (4,4)) and a 4-frame animation "Idle" on parameter 17."""
    model = card_model()
    model['param'] = [param(17, 'Shift')]
    camera = node(6, 'Main', kind='Camera', viewport=[8.0, 8.0])
    camera['transform']['trans'] = [4.0, 4.0, 0.0]
    model['nodes']['children'].append(camera)
    model['animations'] = {'Idle': {
        'length': 4, 'timestep': 0.25, 'additive': False, 'animationWeight': 1.0, 'leadIn': -1, 'leadOut': -1,
        'lanes': [{'uuid': 17, 'target': 0, 'interpolation': 'Linear', 'merge_mode': 'Forced',
                   'keyframes': [{'frame': 0, 'value': 0.0, 'tension': 0.5},
                                 {'frame': 3, 'value': 3.0, 'tension': 0.5}]}]}}
    return model


def png_size(path):
    """(width, height) from the IHDR chunk of a PNG file."""
    data = Path(path).read_bytes()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', f'{path} is not a PNG file'
    return struct.unpack('>II', data[16:24])


def area_export(suite):
    """The Tools panel's "Export image" and "Export frames" sections (editor_ui.rs `draw_tools`). The video
    controls are not used here: area i starts the encodes."""
    fixture = suite.run / 'export.inx'
    fixture.write_bytes(inx_with_texture(export_model()))
    with suite.area('export', fixture) as area:
        def section(ed, title):
            """Expand a collapsed Tools section (its header is a button, like the "Coding agent" header)."""
            headers = ed.find(ed.state(f'before-{title}'), title)
            if not headers:
                raise Check(f'the {title!r} section is not in the accessibility tree')
            ed.click(headers[0])

        def still(ed):
            out = ed.dir / 'out'
            out.mkdir(exist_ok=True)
            target = out / 'still.png'
            section(ed, 'Export image')
            ed.type_into(ed.after(ed.state('image-open'), 'Image path', 'AXTextField'), str(target))
            # The section header and its action button share the label; the button comes last in tree order.
            buttons = ed.find(ed.state('image-path'), 'Export image', 'AXButton')
            if len(buttons) < 2:
                raise Check('the "Export image" action button is not in the accessibility tree')
            ed.click(buttons[-1])
            deadline = time.monotonic() + 15
            while not target.is_file() and time.monotonic() < deadline:
                time.sleep(0.5)
            assert target.is_file(), f'Export image wrote no file at {target}'
            assert png_size(target) == (8, 8), f'expected an 8x8 still from the 8x8 camera, got {png_size(target)}'
            return f'{target.name}: {target.stat().st_size} bytes, 8x8 PNG'

        area.step('exportStillImage', still)

        def frames(ed):
            out = ed.dir / 'frames'
            out.mkdir(exist_ok=True)
            target = out / 'clip.png'
            section(ed, 'Export frames')
            ed.type_into(ed.after(ed.state('frames-open'), 'Frames path (.png or .tga)', 'AXTextField'), str(target))
            buttons = ed.find(ed.state('frames-path'), 'Export frames', 'AXButton')
            if len(buttons) < 2:
                raise Check('the "Export frames" action button is not in the accessibility tree')
            ed.click(buttons[-1])
            expected = [out / f'clip-{n}.png' for n in range(1, 5)]
            deadline = time.monotonic() + 15
            while not all(f.is_file() for f in expected) and time.monotonic() < deadline:
                time.sleep(0.5)
            names = sorted(f.name for f in out.iterdir())
            assert names == [f.name for f in expected], f'expected clip-1..4.png (4 frames of Idle), found {names}'
            sizes = {png_size(f) for f in expected}
            assert sizes == {(8, 8)}, f'frame sizes {sizes}'
            return f'{len(expected)} numbered 8x8 frames: {names}'

        area.step('exportFrames', frames)


def video_model():
    """export_model with a 256x256 camera and a 60-frame clip: a looped encode lasts long enough to be cancelled."""
    model = export_model()
    camera = next(n for n in model['nodes']['children'] if n['type'] == 'Camera')
    camera['viewport'] = [256.0, 256.0]
    idle = model['animations']['Idle']
    idle.update(length=60, timestep=1 / 30)
    idle['lanes'][0]['keyframes'][1]['frame'] = 59
    return model


def video_frames(path):
    """Frames ffprobe counts in the first video stream, or None without ffprobe."""
    if not shutil.which('ffprobe'):
        return None
    out = subprocess.run(['ffprobe', '-v', 'error', '-count_frames', '-select_streams', 'v:0', '-show_entries',
                          'stream=nb_read_frames', '-of', 'csv=p=0', str(path)], capture_output=True, text=True)
    assert out.returncode == 0, out.stderr.strip()
    return int(out.stdout.strip())


def area_video(suite):
    """The video controls of the Tools panel's "Export frames" section: Run ffmpeg, the progress line, Cancel export."""
    fixture = suite.run / 'video.inx'
    fixture.write_bytes(inx_with_texture(video_model()))
    with suite.area('video', fixture) as area:
        def status(ed, job):
            out = ed.tool('export_video_status', {'job': job})
            return out.get('result', out)

        def wait_state(ed, job, want, seconds=120):
            deadline = time.monotonic() + seconds
            while True:
                got = status(ed, job)
                if got['state'] == want or got['state'] not in ('running', want) or time.monotonic() > deadline:
                    return got
                time.sleep(0.25)

        def run(ed, loops):
            ed.set_value(ed.after(ed.state('before-loops'), 'Loops', 'AXIncrementor'), loops)
            ed.click(ed.one(ed.state('before-run'), 'Run ffmpeg', 'AXButton'))

        def encoder(ed):
            if not shutil.which('ffmpeg'):
                raise Check('no ffmpeg on PATH: the video controls cannot start an encode')
            version = subprocess.run(['ffmpeg', '-hide_banner', '-version'], capture_output=True, text=True).stdout
            (ed.dir / 'ffmpeg-version.txt').write_text(version)
            headers = ed.find(ed.state('before-section'), 'Export frames')
            if not headers:
                raise Check('the "Export frames" section is not in the accessibility tree')
            ed.click(headers[0])
            (ed.dir / 'out').mkdir(exist_ok=True)
            # Typed once: typing into a field appends, and both encodes below write this file.
            ed.type_into(ed.after(ed.state('section-open'), 'Video file', 'AXTextField'), str(target(ed)))
            assert ed.find(ed.state('video-path'), 'Run ffmpeg', 'AXButton'), 'the section shows no Run ffmpeg button'
            return version.splitlines()[0]

        def target(ed):
            return ed.dir / 'out' / 'clip.mp4'

        area.step('encoderAndSection', encoder)

        def progress_and_cancel(ed):
            # 200 loops of the 60-frame clip: long enough to read the panel and press Cancel while it runs.
            run(ed, 200)
            running = ed.state('video-running')
            seen = status(ed, 1)
            if seen['state'] != 'running':
                raise Check(f'the encode ended before the panel was read: {seen}')
            line = [ed.text(e) for e in running['elements'] if str(ed.text(e)).startswith('Exporting video:')]
            cancel = ed.find(running, 'Cancel export', 'AXButton')
            if not line or not cancel:
                raise Check('the progress line or the Cancel export button is not in the accessibility tree while encoding')
            assert re.fullmatch(r'Exporting video: \d+ / 12000 frames', line[0]), line
            assert not ed.find(running, 'Run ffmpeg', 'AXButton', enabled=True), 'Run ffmpeg is enabled during an encode'
            ed.click(cancel[0])
            ended = wait_state(ed, 1, 'cancelled', seconds=30)
            assert ended['state'] == 'cancelled' and ended['partialFileKept'] is True, ended
            assert ended['framesDone'] < 12000, ended
            assert target(ed).is_file(), 'the partial file was not kept'
            ed.wait_text('Video export cancelled', 'video-cancelled')
            state = ed.state('after-cancel')
            assert ed.find(state, 'Run ffmpeg', 'AXButton'), 'Run ffmpeg did not come back after the cancel'
            assert not ed.find(state, 'Cancel export', 'AXButton'), 'Cancel export is still shown'
            assert ed.children() == [], f'an encoder is still running after the cancel: {ed.children()}'
            return (f'the panel showed {line[0]!r}; Cancel export stopped the job at {ended["framesDone"]} of 12000 frames, '
                    f'kept the partial {target(ed).name} ({target(ed).stat().st_size} bytes) and left no encoder process')

        area.step('progressAndCancel', progress_and_cancel)

        def run_to_end(ed):
            # The partial file is in the way: without Overwrite the encode is refused and the file stays as it was.
            partial = target(ed).read_bytes()
            run(ed, 1)
            refused = wait_state(ed, 2, 'failed', seconds=30)
            assert refused['state'] == 'failed' and refused['error'].endswith('already exists'), refused
            assert target(ed).read_bytes() == partial, 'the refused encode changed the existing file'
            ed.click(ed.one(ed.state('before-overwrite'), 'Overwrite existing files (frames)', 'AXCheckBox'))
            run(ed, 1)
            done = wait_state(ed, 3, 'finished')
            assert done['state'] == 'finished', done
            assert done['frames'] == 60 and done['framesDone'] == done['framesTotal'] == 60, done
            assert target(ed).stat().st_size > 0, 'Run ffmpeg wrote an empty file'
            counted = video_frames(target(ed))
            assert counted in (None, 60), f'ffprobe counts {counted} frames in {target(ed).name}'
            ed.wait_text('Exported video: 60 frames', 'video-done')
            assert ed.children() == [], f'an encoder is still running: {ed.children()}'
            probe = 'ffprobe is not installed, so the file was not opened' if counted is None else f'ffprobe counts {counted} frames'
            return (f'without Overwrite the encode was refused ({target(ed).name} already exists) and the partial file stayed; '
                    f'with it Run ffmpeg wrote {target(ed).stat().st_size} bytes and the job finished with 60 of 60 frames; {probe}')

        area.step('runToEnd', run_to_end)


def settings_value(ed, key, want, seconds=10):
    """Poll the editor's persisted settings.json until `key` equals `want`; returns the value last read."""
    path = ed.config / 'settings.json'
    deadline = time.monotonic() + seconds
    while True:
        got = json.loads(path.read_text()).get(key) if path.is_file() else None
        if got == want or time.monotonic() > deadline:
            return got
        time.sleep(0.5)


def menu(ed, *path):
    """Open the menus of `path` from the bar and press the last entry (a button or a check box). A menu that
    an earlier step left open (egui keeps a menu open after a check box) is not clicked again, which
    would close it."""
    def entries(state, label):
        # A submenu button is labelled "<name> <arrow glyph>" and an entry may end in "...".
        norm = lambda text: re.sub(r'\W+$', '', str(text or ''))
        return [e for e in state['elements'] if e.get('role') in ('AXButton', 'AXCheckBox')
                and norm(ed.text(e)) == norm(label)]

    state = ed.state('menu-open')
    start = next((i for i in range(len(path) - 1, 0, -1) if entries(state, path[i])), 0)
    for n, label in enumerate(path[start:]):
        if n:
            state = ed.state(f'menu-{label}')
        found = entries(state, label)
        if not found:
            raise Check(f'menu entry {" > ".join(path)!r} is not in the accessibility tree (at {label!r})')
        ed.click(found[0])


def answer_prompt(ed, text, verb):
    """Type `text` into the path prompt and press its `verb` button (Import, Merge, ...)."""
    ed.type_into(ed.after(ed.state(f'prompt-{verb}'), 'Path', 'AXTextField'), str(text))
    buttons = ed.find(ed.state('prompt-typed'), verb, 'AXButton', enabled=True)
    if not buttons:
        raise Check(f"the prompt's {verb} button is not enabled in the accessibility tree")
    ed.click(buttons[-1])


def tree_names(tree):
    """Names of the nodes below the root, in tree order."""
    names = []
    for child in tree.get('children', []):
        names.append(child['name'])
        names += tree_names(child)
    return names


SESSION_DATA_KEY = b'com.inochi2d.inochi-session.bindings'


def area_documents(suite):
    """The File and Tools menu entries that open or change a document, each from the menu bar of a real window."""
    files = suite.run / 'documents-files'
    files.mkdir()
    # Pixels (row by row): opaque red, fully transparent, fully transparent, half transparent red (not premultiplied).
    pixels = bytes([255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 128])
    recent_model = card_model()
    recent_model['meta']['name'] = 'recent'
    recent = files / 'recent.inx'
    recent.write_bytes(inx_with_texture(recent_model, pixels))
    donor_model = card_model()
    donor_model['nodes'] = node(1, 'Donor', [part(5, 'Sticker')])
    donor_model['param'] = [param(31, 'DonorParam')]
    donor = files / 'donor.inx'
    donor.write_bytes(inx_with_texture(donor_model, extensions=[(SESSION_DATA_KEY, b'[{"name":"gui"}]')]))
    stamp = files / 'stamp.png'
    stamp.write_bytes(core_harness.png(2, 2, bytes([0, 255, 0, 255]) * 4))
    folder = files / 'folder'
    folder.mkdir()
    for name in ('a.png', 'b.png'):
        (folder / name).write_bytes(core_harness.png(2, 2, bytes([0, 0, 255, 255]) * 4))
    psd = files / 'layers.psd'
    psd.write_bytes(core_harness.encode_minimal_psd(2, 2, [('Back', bytes([255, 0, 0, 255]) * 4),
                                                           ('Front', bytes([0, 255, 0, 255]) * 4)]))
    fixture = suite.run / 'documents.inx'
    fixture.write_bytes(inx_with_texture(card_model()))
    with suite.area('documents', fixture, settings={'prev_projects': [str(recent)]}) as area:
        def texture(ed):
            out = ed.tool('texture_get', {'slot': 0})
            return out.get('result', out)

        def texture_pixels(ed, name):
            got = texture(ed)
            data = next(v for k, v in got.items() if isinstance(v, str) and len(v) > 40)
            path = ed.dir / f'{name}.png'
            path.write_bytes(base64.b64decode(data))
            return load_pixels(path)

        def nodes(ed):
            return ed.snap()['model']['nodes']

        def discard(ed, name, modified):
            """A replacing import asks about unsaved changes exactly when the document has some: Don't Save."""
            found = ed.find(ed.state(f'unsaved-{name}'), "Don't Save", 'AXButton')
            assert bool(found) == modified, f'modified is {modified} but the unsaved-changes dialog is {"shown" if found else "not shown"}'
            if found:
                ed.click(found[0])
            return 'asked about the unsaved changes, then ' if found else ''

        def open_recent(ed):
            assert ed.snap()['model']['meta']['name'] == 'fixture'
            menu(ed, 'File', 'Recent', recent.name)
            deadline = time.monotonic() + 10
            while ed.snap()['model']['meta']['name'] != 'recent' and time.monotonic() < deadline:
                time.sleep(0.5)
            snap = ed.snap()
            assert snap['model']['meta']['name'] == 'recent', snap['model']['meta']
            assert not snap['modified'], 'the opened file counts as modified'
            return f'File > Recent > {recent.name} opened the file the settings listed (meta name "recent", unmodified)'

        area.step('fileRecent', open_recent)

        def premultiply(ed):
            before, revision = texture_pixels(ed, 'texture-before'), ed.snap()['revision']
            menu(ed, 'Tools', 'Premultiply textures')
            after = texture_pixels(ed, 'texture-premultiplied')
            assert ed.snap()['revision'] == revision + 1, 'Premultiply textures made no undo step'
            (suite.run / 'documents' / 'texture-pixels.json').write_text(json.dumps({'before': repr(before), 'after': repr(after)}))
            assert before != after, 'the texture did not change'
            return 'Tools > Premultiply textures rewrote the texture in one undo step'

        area.step('toolsPremultiply', premultiply)

        def bleed(ed):
            before, revision = texture(ed), ed.snap()['revision']
            menu(ed, 'Tools', 'Bleed textures...')
            assert texture(ed) != before, 'the texture did not change'
            assert ed.snap()['revision'] == revision + 1, 'Bleed textures made no undo step'
            return 'Tools > Bleed textures... spread colour into the transparent pixels in one undo step'

        area.step('toolsBleed', bleed)

        def fake_names(ed):
            assert 'psdLayerPath' not in nodes(ed)['children'][0]
            menu(ed, 'Tools', 'Generate fake layer name info...')
            path = nodes(ed)['children'][0].get('psdLayerPath')
            assert path == '/Card', path
            return f'Tools > Generate fake layer name info... gave the Part the layer path {path!r}'

        area.step('toolsFakeLayerNames', fake_names)

        def new_ids(ed, entry):
            before = nodes(ed)['children'][0]['uuid']
            menu(ed, 'Tools', entry)
            after = nodes(ed)['children'][0]
            assert after['uuid'] != before and after['name'] == 'Card', (before, after['uuid'], after['name'])
            return f'Tools > {entry} gave the Part a new id ({before} -> {after["uuid"]})'

        area.step('toolsRegenerateNodeIds', lambda ed: new_ids(ed, 'Regenerate Node IDs'))
        area.step('toolsAttemptFullRepair', lambda ed: new_ids(ed, 'Attempt full repair...'))

        def session_data(ed):
            menu(ed, 'Tools', 'Import Inochi Session Data...')
            answer_prompt(ed, donor, 'Import')
            got = ed.tool('extdata_get', {'key': SESSION_DATA_KEY.decode()})
            got = got.get('result', got)
            assert got['present'] and base64.b64decode(got['base64']) == b'[{"name":"gui"}]', got
            return 'Tools > Import Inochi Session Data... copied the tracking bindings entry from the other file'

        area.step('toolsImportSessionData', session_data)

        def viewers(ed):
            menu(ed, 'Tools', 'Texture Viewer')
            assert ed.has_text(ed.state('texture-viewer'), 'Select a Part in the node tree'), 'no Texture Viewer window'
            menu(ed, 'Tools', 'Flip Pairing')
            assert ed.has_text(ed.state('flip-pairing'), 'Nodes without a pair'), 'no Flip Pairing window'
            cancel = ed.find(ed.state('flip-cancel'), 'Cancel', 'AXButton')
            assert cancel, 'the Flip Pairing window has no Cancel button'
            ed.click(cancel[-1])
            assert not ed.has_text(ed.state('flip-closed'), 'Nodes without a pair'), 'Flip Pairing did not close'
            return 'Tools > Texture Viewer and Tools > Flip Pairing each opened their window; Cancel closed Flip Pairing'

        area.step('toolsViewerWindows', viewers)

        def merge_images(ed):
            before = tree_names(nodes(ed))
            menu(ed, 'File', 'Merge', 'Merge Image Files')
            answer_prompt(ed, stamp, 'Merge')
            after = tree_names(nodes(ed))
            assert sorted(after) == sorted(before + [stamp.name]), (before, after)
            return f'File > Merge > Merge Image Files added a Part for {stamp.name}: {after}'

        area.step('fileMergeImages', merge_images)

        def merge_project(ed):
            before = tree_names(nodes(ed))
            menu(ed, 'File', 'Merge', 'Merge soul2d project')
            answer_prompt(ed, donor, 'Merge')
            after = tree_names(nodes(ed))
            assert 'Sticker' in after and all(name in after for name in before), (before, after)
            assert any(p.get('name') == 'DonorParam' for p in ed.snap()['model']['param']), 'the parameter was not merged'
            return f'File > Merge > Merge soul2d project added the other file\'s nodes and parameter: {after}'

        area.step('fileMergeProject', merge_project)

        def replaced(ed, entry, source, want, name):
            modified = ed.snap()['modified']
            menu(ed, 'File', 'Import', entry)
            answer_prompt(ed, source, 'Import')
            asked = discard(ed, name, modified)
            deadline = time.monotonic() + 10
            while sorted(tree_names(nodes(ed))) != sorted(want) and time.monotonic() < deadline:
                time.sleep(0.5)
            got = tree_names(nodes(ed))
            assert sorted(got) == sorted(want), (got, want)
            return f'File > Import > {entry} {asked}opened {Path(source).name}: nodes {got}'

        area.step('fileImportPuppet', lambda ed: replaced(ed, 'Import soul2d_live puppet', donor, ['Sticker'], 'puppet'))
        # A freshly imported document has no changes of its own, so the next import replaces it without asking.
        area.step('fileImportImageFolder', lambda ed: replaced(ed, 'Import Image Folder', folder, ['a', 'b'], 'folder'))
        area.step('fileImportPsd', lambda ed: replaced(ed, 'Import PSD', psd, ['Back', 'Front'], 'psd'))

        def reset_sizes(ed):
            def edge(name):
                """(x, y) on the side panel's separator: the darkest column just left of the viewport."""
                state = ed.state(name)
                left, top, _, bottom = (int(v) for v in ed.view(state))
                width, _, pixels = load_pixels(state['_png'])
                y = (top + bottom) // 2
                shade = lambda x: sum(pixels[(y * width + x) * 4:(y * width + x) * 4 + 3])
                return min(range(left - 20, left - 1), key=shade), y

            start, y = edge('layout-before')
            ed.drag((start, y), (start + 80, y))
            wide, _ = edge('layout-dragged')
            if wide - start < 40:
                raise Check(f'dragging the edge of the side panel at x={start} did not resize it (now x={wide})')
            menu(ed, 'View', 'Reset Layout')
            back, _ = edge('layout-reset')
            assert abs(back - start) <= 2, f'Reset Layout left the panel edge at x={back}; it started at x={start} and was dragged to x={wide}'
            return f'the side panel edge was dragged from x={start} to x={wide}; View > Reset Layout put it back at x={back}'

        area.step('viewResetLayoutSize', reset_sizes)


def area_menus(suite):
    """The menu bar entries added after the first panels, each opened through the accessibility tree."""
    fixture = suite.run / 'menus.inx'
    fixture.write_bytes(inx_with_texture(export_model()))
    with suite.area('menus', fixture) as area:
        def outline_shown(ed):
            return ed.has_text(ed.state('outline'), 'Node filter')

        def panels(ed):
            assert outline_shown(ed), 'the Outline panel is not shown at the start'
            menu(ed, 'View', 'Panels', 'Outline')
            assert settings_value(ed, 'Outline.visible', False) is False, 'View > Panels > Outline did not store the setting'
            assert not outline_shown(ed), 'the Outline panel is still drawn after it was hidden'
            menu(ed, 'View', 'Panels', 'Outline')
            assert settings_value(ed, 'Outline.visible', True) is True, 'the second press did not show the panel setting again'
            assert outline_shown(ed), 'the Outline panel did not come back'
            return 'Outline hidden (setting false, panel gone) and shown again (setting true, panel back)'

        area.step('viewPanelsToggleOutline', panels)

        def reset_layout(ed):
            menu(ed, 'View', 'Panels', 'Outline')
            assert settings_value(ed, 'Outline.visible', False) is False and not outline_shown(ed), 'Outline did not hide'
            menu(ed, 'View', 'Reset Layout')
            assert settings_value(ed, 'Outline.visible', True) is True, 'Reset Layout left the Outline setting false'
            assert outline_shown(ed), 'Reset Layout did not bring the hidden Outline panel back'
            return 'Reset Layout restored a hidden Outline panel and its setting'

        area.step('viewResetLayout', reset_layout)

        def stats(ed):
            assert not ed.has_text(ed.state('before-stats'), 'Document revision'), 'the stats window is open at the start'
            menu(ed, 'View', 'Stats for Nerds')
            state = ed.state('stats-open')
            for row in ('Frame time', 'Document revision', 'Nodes', 'Parts', 'Animations'):
                assert ed.has_text(state, row), f'the stats window shows no {row!r} row'
            assert settings_value(ed, 'NerdStats', True) is True, 'NerdStats was not stored'
            menu(ed, 'View', 'Stats for Nerds')
            assert not ed.has_text(ed.state('stats-closed'), 'Document revision'), 'the stats window did not close'
            assert settings_value(ed, 'NerdStats', False) is False
            return 'Stats for Nerds opened a window with the frame, revision and node rows and closed again'

        area.step('viewStatsForNerds', stats)

        def prompt_export(ed, entry, target):
            out = ed.dir / 'out'
            out.mkdir(exist_ok=True)
            menu(ed, 'File', 'Export', entry)
            state = ed.state(f'prompt-{entry}')
            ed.type_into(ed.after(state, 'Path', 'AXTextField'), str(out / target))
            buttons = ed.find(ed.state('prompt-typed'), 'Export', 'AXButton', enabled=True)
            if not buttons:
                raise Check("the prompt's Export button is not enabled in the accessibility tree")
            ed.click(buttons[-1])
            return out / target

        def wait_file(path):
            deadline = time.monotonic() + 15
            while not path.is_file() and time.monotonic() < deadline:
                time.sleep(0.5)
            assert path.is_file(), f'{path.name} was not written'

        def export_image(ed):
            target = prompt_export(ed, 'Export Image...', 'menu-still.png')
            wait_file(target)
            assert png_size(target) == (8, 8), png_size(target)
            return f'File > Export > Export Image... wrote {target.name}: 8x8 PNG'

        area.step('fileExportImage', export_image)

        def export_frames(ed):
            target = prompt_export(ed, 'Export Frames...', 'menu-clip.png')
            expected = [target.with_name(f'menu-clip-{n}.png') for n in range(1, 5)]
            for f in expected:
                wait_file(f)
            assert {png_size(f) for f in expected} == {(8, 8)}
            return f'File > Export > Export Frames... wrote {[f.name for f in expected]}'

        area.step('fileExportFrames', export_frames)

        def export_inp(ed):
            target = prompt_export(ed, 'Export soul2d_live puppet...', 'menu-puppet.inp')
            wait_file(target)
            assert target.stat().st_size > 0, 'the exported puppet is empty'
            return f'File > Export > Export soul2d_live puppet... wrote {target.name}: {target.stat().st_size} bytes'

        area.step('fileExportPuppet', export_inp)

        def import_kra_cancel(ed):
            before = ed.snap()['revision']
            menu(ed, 'File', 'Import', 'Import KRA...')
            state = ed.state('kra-prompt')
            # The prompt window is the only place with a Cancel button here; its title ("Import KRA") and its
            # Import button (disabled until a path is typed) are not in the accessibility tree, only the window
            # screenshot shows the title.
            cancel = ed.find(state, 'Cancel', 'AXButton')
            assert cancel, 'the path prompt did not open (no Cancel button)'
            assert ed.find(state, 'Path', 'AXTextField'), 'the prompt has no Path field'
            ed.click(cancel[0])
            state = ed.state('kra-cancelled')
            assert not ed.find(state, 'Cancel', 'AXButton'), 'the KRA prompt did not close on Cancel'
            assert not ed.find(state, 'Path', 'AXTextField'), 'the prompt Path field is still there'
            assert ed.snap()['revision'] == before, 'cancelling the KRA import changed the document'
            return 'File > Import > Import KRA... opened the path prompt; Cancel closed it and left the document alone'

        area.step('fileImportKraPromptCancel', import_kra_cancel)

        def import_depth(ed):
            menu(ed, 'File', 'Import', 'Import Depth Map...')
            state = ed.state('depth-window')
            # The window title is not in the accessibility tree; the window's own "PSD path" field is.
            assert ed.find(state, 'PSD path', 'AXTextField'), 'no PSD Depth Map window (no "PSD path" field)'
            ed.click(ed.one(state, 'Close', 'AXButton'))
            state = ed.state('depth-closed')
            assert not ed.find(state, 'PSD path', 'AXTextField'), 'the depth window did not close'
            return 'File > Import > Import Depth Map... opened the PSD Depth Map window; Close closed it'

        area.step('fileImportDepthMapWindow', import_depth)

        def dyslexic(ed):
            menu(ed, 'Edit', 'Settings')
            state = ed.state('settings')
            box = ed.one(state, 'Use OpenDyslexic Font', 'AXCheckBox')
            before = state['_png']
            ed.click(box)
            assert settings_value(ed, 'useOpenDyslexic', True) is True, 'useOpenDyslexic was not stored'
            after = ed.state('dyslexic-on')['_png']
            moved = changed_pixels(before, after, (0, 0, png_size(before)[0], 100))
            assert moved > 100, f'the menu bar text did not change with the font ({moved} pixels differ)'
            ed.click(ed.one(ed.state('dyslexic-box'), 'Use OpenDyslexic Font', 'AXCheckBox'))
            assert settings_value(ed, 'useOpenDyslexic', False) is False
            return f'Edit > Settings > Use OpenDyslexic Font stored the setting and redrew the interface ({moved} pixels in the top 100 rows)'

        area.step('settingsOpenDyslexic', dyslexic)


def kra_bytes():
    """A 4x4 Krita document with one paint layer, "Background", whose only tile holds two pixels."""
    maindoc = ('<DOC xmlns="http://www.calligra.org/DTD/krita" kritaVersion="5.2.2" syntaxVersion="2.0">'
               '<IMAGE name="doc" width="4" height="4" colorspacename="RGBA" mime="application/x-kra"><layers>'
               '<layer name="Background" nodetype="paintlayer" uuid="{layer1}" filename="layer1" colorspacename="RGBA"'
               ' visible="1" opacity="255" compositeop="normal" x="0" y="0"/></layers></IMAGE></DOC>')
    tile = b'VERSION 2\nTILEWIDTH 2\nTILEHEIGHT 1\nPIXELSIZE 4\nDATA 1\n0,0,LZF,9\n\x00' + bytes([3, 3, 2, 2, 1, 1, 255, 255])
    out = io.BytesIO()
    with zipfile.ZipFile(out, 'w', zipfile.ZIP_STORED) as archive:
        archive.writestr('mimetype', 'application/x-krita')
        archive.writestr('maindoc.xml', maindoc)
        archive.writestr('doc/layers/layer1', tile)
    return out.getvalue()


def area_imports(suite):
    """The two imports that read a layered file through a window of their own: a depth map and a Krita document."""
    fixture = build_with_core(suite, 'imports', grid_model([-8.0, -4.0, 0.0, 4.0, 8.0]))
    # One opaque light-grey layer named after the grid, which "Direct Grid Name Match" (on by default) pairs with it.
    depth = suite.run / 'depth.psd'
    depth.write_bytes(core_harness.encode_minimal_psd(32, 32, [('Grid', bytes([204, 204, 204, 255]) * (32 * 32))]))
    kra = suite.run / 'doc.kra'
    kra.write_bytes(kra_bytes())
    with suite.area('imports', fixture) as area:
        def depth_map(ed):
            before = find_node(ed.snap()['model']['nodes'], 5)['depths']
            revision = ed.snap()['revision']
            menu(ed, 'File', 'Import', 'Import Depth Map...')
            ed.type_into(ed.after(ed.state('depth-window'), 'PSD path', 'AXTextField'), str(depth))
            ed.click(ed.one(ed.state('depth-path'), 'Plan', 'AXButton', enabled=True))
            deadline = time.monotonic() + 10
            while not ed.find(ed.state('depth-planned'), 'Only show problem layers', 'AXCheckBox') and time.monotonic() < deadline:
                time.sleep(0.5)
            assert ed.snap()['revision'] == revision, 'Plan changed the document'
            ed.click(ed.one(ed.state('depth-plan'), 'Apply', 'AXButton', enabled=True))
            deadline = time.monotonic() + 10
            while ed.snap()['revision'] == revision and time.monotonic() < deadline:
                time.sleep(0.5)
            after = find_node(ed.snap()['model']['nodes'], 5)['depths']
            assert after != before and min(after) > 0.01, f'Apply left the grid depths at {after}'
            ed.click(ed.one(ed.state('depth-applied'), 'Close', 'AXButton'))
            return (f'File > Import > Import Depth Map...: Plan left the document alone, Apply set the 25 grid depths to '
                    f'{min(after):.2f}..{max(after):.2f} from a light-grey PSD layer named after the grid')

        area.step('fileImportDepthMapPlanApply', depth_map)

        def krita(ed):
            modified = ed.snap()['modified']
            menu(ed, 'File', 'Import', 'Import KRA...')
            answer_prompt(ed, kra, 'Import')
            found = ed.find(ed.state('unsaved-kra'), "Don't Save", 'AXButton')
            assert bool(found) == modified, f'modified is {modified} but the unsaved-changes dialog is {"shown" if found else "not shown"}'
            if found:
                ed.click(found[0])
            deadline = time.monotonic() + 10
            while tree_names(ed.snap()['model']['nodes']) != ['Background'] and time.monotonic() < deadline:
                time.sleep(0.5)
            names = tree_names(ed.snap()['model']['nodes'])
            assert names == ['Background'], names
            return f'File > Import > Import KRA... replaced the document with the Krita file\'s layer: nodes {names}'

        area.step('fileImportKra', krita)


AREAS = {'a': area_parameters, 'b': area_animation, 'c': area_motion3, 'd': area_depth, 'e': area_agent,
         'f': area_recovery, 'g': area_export, 'h': area_menus, 'i': area_video,
         'j': area_documents, 'k': area_imports}


def find_node(tree, uuid):
    if tree.get('uuid') == uuid:
        return tree
    for child in tree.get('children', []):
        found = find_node(child, uuid)
        if found is not None:
            return found
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-editor')
    parser.add_argument('--evidence-root', type=Path, default=ROOT / 'out/verification')
    parser.add_argument('--foreground-drags', action='store_true',
                        help='let the driver briefly front the editor window for canvas drags (background drags are unavailable)')
    parser.add_argument('--areas', default=''.join(AREAS), help='letters of the areas to run, default all')
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.evidence_root.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='panels-gui-', dir=args.evidence_root))
    print(run, flush=True)
    suite = Suite(binary, run, args.foreground_drags)
    owned_daemon = None
    status = 'DONE'
    try:
        owned_daemon = harness.ensure_daemon({})
        for letter in args.areas:
            try:
                AREAS[letter](suite)
            except Check as error:
                suite.record(letter, 'area', INCONCLUSIVE, str(error))
            except Exception:
                suite.record(letter, 'area', FAIL, traceback.format_exc().strip().splitlines()[-1])
    except harness.Inconclusive as error:
        status = f'INCONCLUSIVE: {error}'
    finally:
        if owned_daemon is not None:
            stop_owned(owned_daemon)
    result = {'status': status, 'binary': str(binary), 'binarySha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'results': suite.results}
    (run / 'panels-result.json').write_text(json.dumps(result, indent=2) + '\n')
    counts = {s: sum(1 for r in suite.results if r['status'] == s) for s in (PASS, FAIL, INCONCLUSIVE, SKIP)}
    print(json.dumps(counts))
    # 1: a check failed; 2: nothing failed but a check was INCONCLUSIVE or the run could not finish; 3: only SKIPs.
    # Only a run in which every check PASSed exits 0.
    if counts[FAIL]:
        return 1
    if counts[INCONCLUSIVE] or status != 'DONE':
        return 2
    return 3 if counts[SKIP] else 0

if __name__ == '__main__':
    sys.exit(main())
