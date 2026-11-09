#!/usr/bin/env python3
import argparse
import contextlib
import copy
import ctypes
import ctypes.util
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import traceback
from typing import NamedTuple
from xml.parsers import expat

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


NATIVE = load('forms_native_capture', 'tools/verify-editor-native-preview.py')
PEER = NATIVE.PEER
CMO = load('forms_container_oracle', 'tools/verify-cmo-source.py')
DONOR_SHA = 'c439042a960aa05fb009d92e90c469e668ac604fed4e260792f4a58fec72a4c3'
STATE = 'resource://soul2d/editor/state'
ANCHORS = (
    (44, 200, 16, 0, '85673234f5663f08aafc56d2ff69f8a7f5d1cf6ff612b77545271518b01ef624'),
    (44, 100, 16, 42, '447b0f622cba787cd88cd567c86eb439dcf90399b446ccea87a7aa90a134215e'),
    (44, 100, 16, 84, '56aaef1447e82dde20602aa8bdcc9c0320f56b12aed7593b5a56e49b96824534'),
)


class Fixture(NamedTuple):
    original: Path
    donor: Path
    saved: Path


class Anchor(NamedTuple):
    top: int
    screenshot: dict


class KeyStroke(NamedTuple):
    keycode: int
    shift: bool


def allowed_request(method, params):
    return ((method == 'initialize' and params == {})
            or (method == 'resources/read' and params == {'uri': STATE})
            or (method == 'tools/call' and params in [
                {'name': 'snapshot', 'arguments': {}}, {'name': 'editor_quit', 'arguments': {}}]))


class ObservationPeer(PEER.Mcp):
    def rpc(self, method, params):
        assert allowed_request(method, params), 'forbidden MCP action in GUI acceptance'
        return super().rpc(method, params)

    def initialize(self):
        return self.rpc('initialize', {})

    def document(self):
        return PEER.document(self)

    def state(self):
        return PEER.state(self)

    def quit(self):
        return self.op('editor_quit')


def crop(rgb, width, rect):
    x0, y0, x1, y1 = rect
    assert 0 <= x0 < x1 <= width and 0 <= y0 < y1 <= len(rgb) // (width * 3)
    return b''.join(rgb[(y * width + x0) * 3:(y * width + x1) * 3] for y in range(y0, y1))


def source_top(rgb, width=1600, height=1000, anchors=ANCHORS):
    assert len(rgb) == width * height * 3
    tops = []
    for x0, x1, rows, offset, digest in anchors:
        matches = [y for y in range(height - rows + 1)
                   if hashlib.sha256(crop(rgb, width, (x0, y, x1, y + rows))).hexdigest() == digest]
        assert len(matches) <= 1, 'ambiguous production text anchor'
        if not matches:
            return None
        tops.append(matches[0] - offset)
    assert len(set(tops)) == 1, 'production source controls changed relative positions'
    return tops[0]


def settled_source_anchor(capture, observe, deadline):
    previous, repeats = None, 0
    while time.monotonic() < deadline:
        observe()
        rgb, image = capture()
        top = source_top(rgb)
        if top is None:
            return None
        if not (48 <= top and top + 125 <= 876):
            return None
        repeats = repeats + 1 if top == previous else 1
        if repeats == 3:
            return Anchor(top, image)
        previous = top
        time.sleep(.2)
    raise TimeoutError('production source controls did not settle')


def ascii_key_plan(text, lookup):
    assert text and text.isascii() and all(32 <= ord(c) < 127 for c in text), 'ASCII printable path required'
    plan = []
    for char in text:
        code, levels = lookup(ord(char))
        assert 0 < code < 256 and ord(char) in levels, f'unmapped ASCII character {char!r}'
        plan.append(KeyStroke(code, levels[0] != ord(char)))
    return tuple(plan)


def key_chord(send, codes):
    try:
        for code in codes:
            assert send(code, True), 'XTest key press failed'
    finally:
        errors = []
        for code in reversed(codes):
            try:
                assert send(code, False), 'XTest key release failed'
            except BaseException:
                errors.append(traceback.format_exc())
        if errors:
            raise AssertionError('\n'.join(errors))


def prepare_fixture(example, run):
    fixture = Fixture(run / 'four-color.cmo3', run / 'clipping-donor.cmo3', run / 'gui-saved.cmo3')
    producer = subprocess.run([str(example), str(fixture.original)], capture_output=True, text=True, timeout=30)
    (run / 'producer.json').write_text(json.dumps(dict(exit=producer.returncode, stdout=producer.stdout,
                                                    stderr=producer.stderr), indent=2) + '\n')
    assert producer.returncode == 0 and PEER.digest(fixture.original) == NATIVE.FIXTURE_SHA
    original = fixture.original.read_bytes()
    entries, xml = CMO.CAFF.decode(original)
    start = xml.index(b"<CArtMeshSource xs.id='mesh2'>")
    end = xml.index(b'</CArtMeshSource>', start) + len(b'</CArtMeshSource>')
    segment = xml[start:end]
    fields = (
        (b"<float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array>",
         b"<b xs.n='invertClippingMask'>false</b>"),
        (b"<int-array xs.n='indices' count='3'>0 1 2</int-array>",
         b"<carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid uuid='mesh-guid-3'/></carray_list>"),
    )
    for before, field in fields:
        assert segment.count(before) == 1
        segment = segment.replace(before, before + field, 1)
    altered = xml[:start] + segment + xml[end:]
    fixture.donor.write_bytes(CMO.repack_main_xml(original, entries, altered))
    assert PEER.digest(fixture.donor) == DONOR_SHA
    actual, decoded = CMO.CAFF.decode(fixture.donor.read_bytes())
    assert decoded == altered and [e for e in actual if e[0][0] != 'main.xml'] == [
        e for e in entries if e[0][0] != 'main.xml']
    assert fixture.saved.parent == run and fixture.saved != fixture.donor and not fixture.saved.exists()
    return fixture


def without_target_insertions(xml):
    graph = CMO.XMLGraph(xml)
    target = graph.ids['mesh0']
    fields = {n.get('xs.n'): n for n in target if n.get('xs.n') in ('invertClippingMask', 'clipGuidList')}
    assert len(fields) == 2
    inverse, masks = fields['invertClippingMask'], fields['clipGuidList']
    assert inverse.tag == 'b' and inverse.attrib == {'xs.n': 'invertClippingMask'} and inverse.text == 'true'
    assert masks.tag == 'carray_list' and masks.attrib == {'xs.n': 'clipGuidList', 'count': '1'}
    assert len(masks) == 1 and masks[0].tag == 'CArtMeshGuid' and masks[0].attrib == {'uuid': 'mesh-guid-1'}
    parser, stack, ranges = expat.ParserCreate(), [], []
    owner_depth = None
    starts = {}

    def start(name, attrs):
        nonlocal owner_depth
        stack.append(name)
        if name == 'CArtMeshSource' and attrs.get('xs.id') == 'mesh0':
            assert owner_depth is None
            owner_depth = len(stack)
        if owner_depth is not None and len(stack) == owner_depth + 1 and attrs.get('xs.n') in fields:
            starts[len(stack)] = parser.CurrentByteIndex

    def end(name):
        nonlocal owner_depth
        if len(stack) in starts:
            ranges.append((starts.pop(len(stack)), xml.index(b'>', parser.CurrentByteIndex) + 1))
        if owner_depth == len(stack):
            owner_depth = None
        stack.pop()

    parser.StartElementHandler, parser.EndElementHandler = start, end
    parser.Parse(xml, True)
    assert len(ranges) == 2
    for start, end in sorted(ranges, reverse=True):
        xml = xml[:start] + xml[end:]
    return xml


def preserved_container(old_entries, old_xml, new_entries, new_xml):
    assert len(old_entries) == len(new_entries)
    for old, new in zip(old_entries, new_entries):
        assert old[:4] == new[:4], 'CAFF entry metadata changed'
        if old[0][0] != 'main.xml':
            assert old == new, 'opaque CAFF payload changed'
    assert without_target_insertions(new_xml) == old_xml, 'unrelated XML changed'


def expected_document(baseline, edited, revision, count, position, modified):
    expected = copy.deepcopy(baseline)
    if edited:
        source = next(s for s in expected['model']['sources'] if s['guid'] == 'mesh-guid-0')
        source['data'].update(clipGuids=['mesh-guid-1'], invertClippingMask=True)
    states = [dict(applied=True, current=position == 0, index=0, label='')]
    if count:
        assert count == 1
        states.append(dict(applied=position == 1, current=position == 1, index=1, label='live2d.source.set'))
    assert position in range(count + 1)
    expected.update(revision=revision, modified=modified,
                    history=dict(count=count, limit=baseline['history']['limit'], position=position,
                                 revision=revision, states=states))
    return expected


def assert_document(actual, expected):
    assert actual == expected, 'unexpected full document, values or history'


class OwnedXTest:
    def __init__(self, run, result):
        self.result, self.connection = result, None
        self.held_keys, self.held_buttons = set(), set()
        self.journal = (run / 'input-events.jsonl').open('w', encoding='utf-8')
        try:
            assert sys.platform.startswith('linux') and os.environ.get('GITHUB_ACTIONS') == 'true', 'hosted Linux CI only'
            assert os.environ.get('RUNNER_OS') == 'Linux' and os.environ.get('RUNNER_ARCH') == 'X64', 'Linux x64 only'
            display = os.environ.get('DISPLAY', '')
            assert re.fullmatch(r':\d+', display), 'owned local Xvfb required'
            pid = int(Path(f'/tmp/.X{display[1:]}-lock').read_text().strip())
            server = Path(f'/proc/{pid}')
            assert server.stat().st_uid == os.getuid()
            command = [v.decode() for v in (server / 'cmdline').read_bytes().split(b'\0') if v]
            assert Path(command[0]).name == 'Xvfb' and display in command
            authority = Path(command[command.index('-auth') + 1])
            assert authority.samefile(os.environ['XAUTHORITY'])
            result['display'] = dict(name=display, serverPid=pid, executable=command[0], authority=str(authority))
            x11_name, xtest_name = ctypes.util.find_library('X11'), ctypes.util.find_library('Xtst')
            assert x11_name and xtest_name
            self.x11, self.xtest = ctypes.CDLL(x11_name), ctypes.CDLL(xtest_name)
            self.x11.XOpenDisplay.argtypes, self.x11.XOpenDisplay.restype = [ctypes.c_char_p], ctypes.c_void_p
            self.x11.XCloseDisplay.argtypes, self.x11.XCloseDisplay.restype = [ctypes.c_void_p], ctypes.c_int
            self.x11.XSync.argtypes, self.x11.XSync.restype = [ctypes.c_void_p, ctypes.c_int], ctypes.c_int
            self.x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
            self.x11.XKeysymToKeycode.restype = ctypes.c_ubyte
            self.x11.XkbKeycodeToKeysym.argtypes = [ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_int, ctypes.c_int]
            self.x11.XkbKeycodeToKeysym.restype = ctypes.c_ulong
            self.xtest.XTestQueryExtension.argtypes = [ctypes.c_void_p] + [ctypes.POINTER(ctypes.c_int)] * 4
            self.xtest.XTestQueryExtension.restype = ctypes.c_int
            self.xtest.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
            self.xtest.XTestFakeButtonEvent.argtypes = self.xtest.XTestFakeKeyEvent.argtypes = [
                ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
            for name in ['XTestFakeMotionEvent', 'XTestFakeButtonEvent', 'XTestFakeKeyEvent']:
                getattr(self.xtest, name).restype = ctypes.c_int
            self.connection = self.x11.XOpenDisplay(display.encode())
            assert self.connection
            event, error, major, minor = (ctypes.c_int() for _ in range(4))
            assert self.xtest.XTestQueryExtension(self.connection, ctypes.byref(event), ctypes.byref(error),
                                                  ctypes.byref(major), ctypes.byref(minor))
            result['xtest'] = dict(library=xtest_name, major=major.value, minor=minor.value)
        except BaseException:
            self.close()
            raise

    def record(self, value):
        if value.get('state') == 'attempt':
            self.result['inputInjection'] = 'ATTEMPTED'
        self.journal.write(json.dumps(dict(at=time.monotonic(), **value)) + '\n')
        self.journal.flush()

    def move(self, x, y):
        assert 0 <= x < 1600 and 0 <= y < 1000
        self.record(dict(kind='motion', x=x, y=y, state='attempt'))
        returned = self.xtest.XTestFakeMotionEvent(self.connection, -1, x, y, 0)
        self.x11.XSync(self.connection, 0)
        self.record(dict(kind='motion', x=x, y=y, returned=returned))
        assert returned

    def button(self, button, down):
        assert button in (1, 5)
        self.record(dict(kind='button', button=button, down=down, state='attempt'))
        if down:
            self.held_buttons.add(button)
        returned = self.xtest.XTestFakeButtonEvent(self.connection, button, int(down), 0)
        self.x11.XSync(self.connection, 0)
        if not down and returned:
            self.held_buttons.discard(button)
        self.record(dict(kind='button', button=button, down=down, returned=returned))
        return returned

    def click(self, x, y, button=1):
        self.move(x, y)
        try:
            assert self.button(button, True)
        finally:
            assert self.button(button, False)
        time.sleep(.15)

    def key(self, code, down):
        assert 0 < code < 256
        self.record(dict(kind='key', code=code, down=down, state='attempt'))
        if down:
            self.held_keys.add(code)
        returned = self.xtest.XTestFakeKeyEvent(self.connection, code, int(down), 0)
        self.x11.XSync(self.connection, 0)
        if not down and returned:
            self.held_keys.discard(code)
        self.record(dict(kind='key', code=code, down=down, returned=returned))
        return returned

    def lookup(self, symbol):
        code = self.x11.XKeysymToKeycode(self.connection, symbol)
        return code, tuple(self.x11.XkbKeycodeToKeysym(self.connection, code, 0, level) for level in (0, 1))

    def path_plan(self, path):
        plan = ascii_key_plan(str(path), self.lookup)
        control, control_levels = self.lookup(0xffe3)
        shift, shift_levels = self.lookup(0xffe1)
        assert control and 0xffe3 in control_levels and shift and 0xffe1 in shift_levels
        letter_a, levels = self.lookup(ord('a'))
        assert letter_a and levels[0] == ord('a')
        return control, shift, letter_a, plan

    def replace_path(self, plan):
        control, shift, letter_a, characters = plan
        key_chord(self.key, (control, letter_a))
        for char in characters:
            key_chord(self.key, (shift, char.keycode) if char.shift else (char.keycode,))
            time.sleep(.01)

    def close(self):
        errors = []
        try:
            if self.connection:
                for codes, release in [(self.held_keys.copy(), self.key), (self.held_buttons.copy(), self.button)]:
                    for code in codes:
                        try:
                            assert release(code, False)
                        except BaseException:
                            errors.append(traceback.format_exc())
                self.x11.XCloseDisplay(self.connection)
                self.connection = None
        finally:
            self.result['inputCleanup'] = dict(heldKeys=sorted(self.held_keys), heldButtons=sorted(self.held_buttons),
                                               errors=errors, connectionClosed=self.connection is None)
            self.journal.close()
        assert not errors and not self.held_keys and not self.held_buttons, 'XTest input cleanup failed'


@contextlib.contextmanager
def editor_process(editor, project, run, label, result):
    directory = run / label
    directory.mkdir()
    env = PEER.HARNESS.editor_env(directory)
    env.pop('WAYLAND_DISPLAY', None)
    env.update(LIBGL_ALWAYS_SOFTWARE='1', WINIT_X11_SCALE_FACTOR='1')
    settings = directory / 'config/settings.json'
    settings.write_text(json.dumps(dict(json.loads(settings.read_text()), WinW=1440, WinH=900, WinMax=False)))
    record = dict(label=label, command=[str(editor), '--mcp', '--open', str(project)])
    result.setdefault('processes', []).append(record)
    proc = None
    try:
        with (directory / 'wire.jsonl').open('w', encoding='utf-8') as wire, (directory / 'editor.log').open('wb') as stderr:
            proc = subprocess.Popen(record['command'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=stderr, env=env, bufsize=0)
            record['pid'] = proc.pid
            os.set_blocking(proc.stdin.fileno(), False)
            os.set_blocking(proc.stdout.fileno(), False)
            peer = ObservationPeer(proc, wire)
            peer.initialize()
            yield peer
            reply = peer.quit()
            record['quit'] = reply
            assert reply['quit']['state'] == 'accepted'
            assert reply['quit']['policy'] == 'close_after_reply_flush_and_final_safety_check'
            assert proc.wait(timeout=10) == 0
    except BaseException:
        record['error'] = traceback.format_exc()
        raise
    finally:
        if proc is not None:
            forced = proc.poll() is None
            try:
                if forced:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait(timeout=5)
            except BaseException:
                record['cleanupError'] = traceback.format_exc()
                raise
            finally:
                record['cleanup'] = dict(forced=forced, exit=proc.returncode, running=proc.poll() is None)
                for stream in (proc.stdin, proc.stdout):
                    stream.close()


def verify(editor, example, out):
    out.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=out))
    result = dict(status='FAIL', scope='synthetic ArtMesh-with-form Linux X11', formsEditing='NOT_RUN',
                  inputInjection='NOT_RUN', verifiedStages=[], screenshots=[],
                  notRun=['Windows/macOS native editing', 'Part/no-form cases', 'vendor compatibility',
                          'rendered-mask parity', 'formal proof', 'IME input'])
    driver = None
    try:
        driver = OwnedXTest(run, result)
        sources = set((ROOT / 'rust/src').rglob('*.rs')) | {
            Path(__file__).resolve(), ROOT / '.github/workflows/rust-core.yml', ROOT / 'rust/Cargo.toml',
            ROOT / 'rust/Cargo.lock', ROOT / 'rust/examples/four-color-cmo3.rs',
            ROOT / 'rust/tests/common/authoring_fixture.rs', ROOT / 'tools/verify-editor-native-preview.py',
            ROOT / 'tools/verify-editor-texture-cache.py', ROOT / 'tools/verify-animation-key-gui.py',
            ROOT / 'tools/verify-rust-core.py', ROOT / 'tools/verify-cmo-source.py', ROOT / 'tools/verify-caff-authoring.py'}
        result['sourceHashes'] = {str(p.relative_to(ROOT)): PEER.digest(p) for p in sorted(sources)}
        result['gitHead'] = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, timeout=5).decode().strip()
        result['binarySha256'], result['exampleSha256'] = PEER.digest(editor), PEER.digest(example)
        fixture = prepare_fixture(example, run)
        path_plan = driver.path_plan(fixture.saved)
        result['fixtureHashes'] = dict(original=NATIVE.FIXTURE_SHA, donor=DONOR_SHA)
        result['anchorProvenance'] = dict(expander='4e695978632ccf063daba1eded309754a67b7cb2/artifact13655707389',
                                         sourceText='c54ed6a73c229d37bb51bfecca2d905896445be4/artifact13656189261')

        def screenshot(name):
            driver.move(800, 500)
            rgb, meta = NATIVE.capture(run, f'{len(result["screenshots"]):03}-{name}', time.monotonic() + 10)
            result['screenshots'].append(meta)
            return rgb, meta

        def readback(peer, expected, name):
            deadline = time.monotonic() + 20
            while True:
                actual = peer.document()
                (run / f'{name}-document.json').write_text(json.dumps(actual, indent=2) + '\n')
                if actual == expected:
                    break
                assert actual['revision'] <= expected['revision'], f'{name}: unexpected document revision'
                assert time.monotonic() < deadline, f'{name}: document/history readback mismatch'
                time.sleep(.1)
            state = peer.state()
            (run / f'{name}-state.json').write_text(json.dumps(state, indent=2) + '\n')
            result['verifiedStages'].append(name)
            screenshot(name)
            return state

        def source_anchor(peer, expected, dirty):
            def observe():
                assert_document(peer.document(), expected)
                deadline = time.monotonic() + 15
                while True:
                    state = peer.state()
                    (run / 'latest-input-state.json').write_text(json.dumps(state, indent=2) + '\n')
                    owners = state['live2dFormInputs']['owners']
                    assert not owners['main']['dirty'] and not owners['formSettings']['dirty'], 'sibling draft changed'
                    if all(v['retained'] and v['observation'] == 'current' and v['valid'] for v in owners.values()) \
                            and owners['sourceSettings']['dirty'] == dirty:
                        return
                    assert time.monotonic() < deadline, 'Forms owners did not reach expected clean/current draft state'
                    time.sleep(.1)

            for _ in range(12):
                anchor = settled_source_anchor(lambda: screenshot('source-anchor'), observe, time.monotonic() + 15)
                if anchor:
                    result.setdefault('sourceAnchors', []).append(dict(top=anchor.top, screenshot=anchor.screenshot))
                    return anchor
                for _ in range(3):
                    driver.click(350, 200, button=5)
            raise AssertionError('production source controls were not found')

        def toolbar(peer, expected, name, x):
            assert_document(peer.document(), expected)
            rgb, _ = screenshot(name + '-before')
            assert hashlib.sha256(crop(rgb, 1600, (543, 27, 591, 43))).hexdigest() == \
                'e078a374991bdde6de49ca605854949d75c89a069cd4db93eaa62e62d98ab04d', 'production toolbar changed'
            driver.click(x, 35)

        empty, _ = screenshot('empty-root')
        with editor_process(editor, fixture.donor, run, 'first', result) as peer:
            baseline = peer.document()
            assert baseline['documentFormat'] == 'live2d.cmo3'
            assert baseline == expected_document(baseline, False, 1, 0, 0, False)
            sources_by_guid = {s['guid']: s for s in baseline['model']['sources']}
            assert len(sources_by_guid) == 5 and sources_by_guid['mesh-guid-0']['data']['clipGuids'] == []
            assert not sources_by_guid['mesh-guid-0']['data']['invertClippingMask']
            assert sources_by_guid['mesh-guid-2']['data']['clipGuids'] == ['mesh-guid-3']
            for _ in range(20):
                rgb, _ = screenshot('production')
                if rgb != empty:
                    break
                time.sleep(.1)
            else:
                raise AssertionError('owned editor window did not appear')
            assert hashlib.sha256(crop(rgb, 1600, (8, 448, 230, 468))).hexdigest() == \
                'd83d13a25f8fd266f2e8d7b08a6b9b3ebc1851f65f11bfe82a6700ba4926be63', 'production expander changed'
            driver.click(100, 458)
            anchor = source_anchor(peer, baseline, False)
            result['formsEditing'] = 'ATTEMPTED'
            driver.click(33, anchor.top + 50)
            anchor = source_anchor(peer, baseline, True)
            driver.click(33, anchor.top + 8)
            readback(peer, baseline, 'pending-source-draft')
            anchor = source_anchor(peer, baseline, True)
            driver.click(95, anchor.top + 113)
            applied = expected_document(baseline, True, 2, 1, 1, True)
            readback(peer, applied, 'applied')
            toolbar(peer, applied, 'undo', 680)
            undone = expected_document(baseline, False, 3, 1, 0, False)
            readback(peer, undone, 'undone')
            toolbar(peer, undone, 'redo', 728)
            redone = expected_document(baseline, True, 4, 1, 1, True)
            readback(peer, redone, 'redone')
            assert not fixture.saved.exists()
            toolbar(peer, redone, 'path-field', 180)
            driver.replace_path(path_plan)
            readback(peer, redone, 'typed-save-path')
            toolbar(peer, redone, 'save-as', 568)
            saved = expected_document(baseline, True, 4, 1, 1, False)
            saved_state = readback(peer, saved, 'saved')
            assert saved_state['document']['path'] == str(fixture.saved)
            old, new = fixture.donor.read_bytes(), fixture.saved.read_bytes()
            assert old[:58] == new[:58] and old[-2:] == new[-2:], 'CAFF header/trailer changed'
            old_entries, old_xml = CMO.CAFF.decode(old)
            new_entries, new_xml = CMO.CAFF.decode(new)
            preserved_container(old_entries, old_xml, new_entries, new_xml)
            result['saved'] = dict(file=fixture.saved.name, sha256=PEER.digest(fixture.saved), bytes=len(new))
        assert screenshot('after-first-quit')[0] == empty, 'first editor window survived'
        with editor_process(editor, fixture.saved, run, 'reopened', result) as peer:
            fresh = expected_document(baseline, True, 1, 0, 0, False)
            reopened_state = readback(peer, fresh, 'fresh-process-reopened')
            assert reopened_state['document']['path'] == str(fixture.saved)
            for _ in range(20):
                if screenshot('fresh-process-window')[0] != empty:
                    break
                assert_document(peer.document(), fresh)
                time.sleep(.1)
            else:
                raise AssertionError('fresh editor window missing')
        assert screenshot('after-second-quit')[0] == empty, 'fresh editor window survived'
        assert all(p['cleanup'] == dict(forced=False, exit=0, running=False) for p in result['processes'])
        closing, driver = driver, None
        closing.close()
        assert PEER.digest(fixture.original) == NATIVE.FIXTURE_SHA and PEER.digest(fixture.donor) == DONOR_SHA
        assert PEER.digest(editor) == result['binarySha256'] and PEER.digest(example) == result['exampleSha256']
        assert all(PEER.digest(ROOT / f) == sha for f, sha in result['sourceHashes'].items())
        result.update(status='PASS', formsEditing='PASS', inputInjection='OS_EVENTS_SENT', immutableInputsFinal=True)
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if driver is not None:
                closing, driver = driver, None
                closing.close()
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(f"{result['status']}: {run}; scope={result['scope']}")


def self_test():
    from unittest.mock import patch

    def refuses(call):
        try:
            call()
        except (AssertionError, KeyError, ValueError):
            return
        raise AssertionError('negative control was accepted')

    allowed = [('initialize', {}), ('resources/read', {'uri': STATE}),
               ('tools/call', {'name': 'snapshot', 'arguments': {}}),
               ('tools/call', {'name': 'editor_quit', 'arguments': {}})]
    assert all(allowed_request(*value) for value in allowed)
    for method, params in [('tools/call', {'name': 'save', 'arguments': {'path': '/tmp/no'}}),
                           ('tools/call', {'name': 'snapshot', 'arguments': {'extra': True}}),
                           ('resources/read', {'uri': 'resource://other'}), ('initialize', {'extra': True})]:
        peer = ObservationPeer(None, None)
        refuses(lambda: peer.rpc(method, params))

    image = bytearray(3 * 6 * 3)
    image[(4 * 3 + 1) * 3:(4 * 3 + 2) * 3] = b'\x12\x34\x56'
    specs = ((1, 2, 1, 0, hashlib.sha256(b'\x12\x34\x56').hexdigest()),)
    assert source_top(bytes(image), 3, 6, specs) == 4
    assert source_top(bytes(54), 3, 6, specs) is None
    image[(2 * 3 + 1) * 3:(2 * 3 + 2) * 3] = b'\x12\x34\x56'
    refuses(lambda: source_top(bytes(image), 3, 6, specs))
    with patch.dict(settled_source_anchor.__globals__, source_top=lambda _: 100):
        anchor = settled_source_anchor(lambda: (b'', {'file': 'stable'}), lambda: None, time.monotonic() + 2)
        assert anchor.top == 100
    from itertools import cycle
    moving = cycle((100, 101))
    with patch.dict(settled_source_anchor.__globals__, source_top=lambda _: next(moving)):
        try:
            settled_source_anchor(lambda: (b'', {}), lambda: None, time.monotonic() + .7)
        except TimeoutError as error:
            assert 'did not settle' in str(error)
        else:
            raise AssertionError('moving source controls were accepted')

    def lookup(symbol):
        return {ord('a'): (38, (ord('a'), ord('A'))), ord('A'): (38, (ord('a'), ord('A'))),
                ord('_'): (20, (ord('-'), ord('_')))}.get(symbol, (0, (0, 0)))

    assert ascii_key_plan('aA_', lookup) == (KeyStroke(38, False), KeyStroke(38, True), KeyStroke(20, True))
    refuses(lambda: ascii_key_plan('?', lookup))
    refuses(lambda: ascii_key_plan('日本語', lookup))
    sent = []

    def failed_send(code, down):
        sent.append((code, down))
        return not (code == 38 and down)

    refuses(lambda: key_chord(failed_send, (37, 38)))
    assert sent == [(37, True), (38, True), (38, False), (37, False)]
    sent.clear()

    def failed_release(code, down):
        sent.append((code, down))
        return not (code == 38 and not down)

    refuses(lambda: key_chord(failed_release, (37, 38)))
    assert sent[-2:] == [(38, False), (37, False)]

    baseline = dict(model={'sources': [{'guid': 'mesh-guid-0', 'data': {'clipGuids': [], 'invertClippingMask': False}},
                                       {'guid': 'donor', 'data': {'clipGuids': ['untouched']}}]},
                    values={'parameter': 0}, documentFormat='live2d.cmo3', revision=1, modified=False,
                    history=dict(count=0, limit=100, position=0, revision=1,
                                 states=[dict(applied=True, current=True, index=0, label='')]))
    assert_document(baseline, expected_document(baseline, False, 1, 0, 0, False))
    applied = expected_document(baseline, True, 2, 1, 1, True)
    assert applied['model']['sources'][1] == baseline['model']['sources'][1]
    assert applied['model']['sources'][0]['data'] == dict(clipGuids=['mesh-guid-1'], invertClippingMask=True)
    assert applied['history'] == dict(count=1, limit=100, position=1, revision=2, states=[
        dict(applied=True, current=False, index=0, label=''),
        dict(applied=True, current=True, index=1, label='live2d.source.set')])
    for change in ['extra-model', 'two-edits', 'undo-position', 'fresh-history']:
        expected = expected_document(baseline, False, 3, 1, 0, False) if change == 'undo-position' else applied
        if change == 'fresh-history':
            expected = expected_document(baseline, True, 1, 0, 0, False)
        actual = copy.deepcopy(expected)
        if change == 'extra-model':
            actual['model']['sources'][1]['data']['clipGuids'] = []
        elif change == 'two-edits':
            actual['history']['count'] = 2
        elif change == 'undo-position':
            actual['history']['position'] = 1
        else:
            actual['history']['states'].append(dict(index=1, label='unexpected'))
        refuses(lambda: assert_document(actual, expected))

    xml = b"<root><CArtMeshSource xs.id='mesh0'><int-array xs.n='indices'>0 1 2</int-array></CArtMeshSource><Opaque>keep</Opaque></root>"
    inserted = b'''<b xs.n='invertClippingMask'>true</b><carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid uuid="mesh-guid-1"/></carray_list>'''
    edited_xml = xml.replace(b'</CArtMeshSource>', inserted + b'</CArtMeshSource>')
    entries = [(['main.xml', 'main_xml'], 1, b'flags', b'reserved', b'compressed'),
               (['opaque.bin', ''], 2, b'flags', b'reserved', b'payload')]
    preserved_container(entries, xml, entries, edited_xml)
    refuses(lambda: preserved_container(entries, xml, entries, edited_xml.replace(b'mesh-guid-1', b'mesh-guid-3')))
    refuses(lambda: preserved_container(entries, xml, entries, edited_xml.replace(b'keep', b'changed')))
    damaged = copy.deepcopy(entries)
    damaged[1] = (*damaged[1][:-1], b'changed')
    refuses(lambda: preserved_container(entries, xml, damaged, edited_xml))
    donor_xml = xml.replace(b'<Opaque>', b"<CArtMeshSource xs.id='mesh2'><s>donor</s></CArtMeshSource><Opaque>")
    donor_edited = donor_xml.replace(b'</CArtMeshSource>', inserted + b'</CArtMeshSource>', 1).replace(b'donor', b'changed')
    refuses(lambda: preserved_container(entries, donor_xml, entries, donor_edited))

    with tempfile.TemporaryDirectory(prefix='forms-self-test-') as temporary:
        out = Path(temporary)
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'false'}), contextlib.redirect_stdout(io.StringIO()):
            refuses(lambda: verify(Path('not-launched'), Path('not-launched'), out))
        results = list(out.rglob('result.json'))
        assert len(results) == 1
        result = json.loads(results[0].read_text())
        assert result['status'] == 'FAIL' and result['formsEditing'] == result['inputInjection'] == 'NOT_RUN'
        assert 'hosted Linux CI only' in result['error'] and result['inputCleanup']['connectionClosed']
    print('PASS: Forms oracle, anchors, key releases, read-only policy and setup failure self-checks')


def main():
    parser = argparse.ArgumentParser(description='Verify bounded production Forms editing through owned Linux XTest input.')
    parser.add_argument('--editor', type=Path)
    parser.add_argument('--example', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
    else:
        if not all((args.editor, args.example, args.out)):
            parser.error('--editor, --example and --out are required')
        verify(args.editor.resolve(), args.example.resolve(), args.out.resolve())


if __name__ == '__main__':
    main()
