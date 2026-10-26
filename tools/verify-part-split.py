#!/usr/bin/env python3
"""Prove the part split loop through MCP tools only (standard library only).

Starts `out/rust/debug/soul2d-core --mcp` (stdio), generates a layered "character" PNG, splits it with prompts and
checks: per-part IoU against the visible region of each original layer is at least 0.9, the applied Parts stack back
to the source, `split_apply` is one undo step and one redo step, draft tools leave the document alone, and a saved
plan replays in a fresh host to the same Parts. Evidence goes to build/verification/<timestamp>-part-split/.
"""
import argparse
import base64
import hashlib
import json
import select
import struct
import subprocess
import sys
import traceback
import zlib
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
W, H = 96, 128
HAIR = (90, 50, 20, 255)
OUTLINE = (20, 20, 20, 255)
MIN_IOU = 0.9


# ----------------------------------------------------------------------------------------------- fixture

def in_rect(x, y, left, top, width, height):
    return left <= x < left + width and top <= y < top + height


def head(x, y):
    return (x + 0.5 - 48) ** 2 + (y + 0.5 - 40) ** 2 <= 22 * 22


def fringe(x, y):
    if not in_rect(x, y, 30, 16, 36, 14):
        return None
    return HAIR if in_rect(x, y, 32, 18, 32, 10) else OUTLINE


# Back to front: (part name, colour function). The hair crosses the symmetry axis x=48 in one colour.
LAYERS = [
    ('HairBack.L', lambda x, y: HAIR if in_rect(x, y, 24, 10, 24, 60) else None),
    ('HairBack.R', lambda x, y: HAIR if in_rect(x, y, 48, 10, 24, 60) else None),
    ('Chest', lambda x, y: (40, 80, 200, 255) if in_rect(x, y, 34, 66, 28, 54) else None),
    ('Head', lambda x, y: (240, 200, 170, 255) if head(x, y) else None),
    ('Eye.L', lambda x, y: (60, 30, 30, 255) if in_rect(x, y, 38, 36, 6, 6) else None),
    ('Eye.R', lambda x, y: (60, 30, 30, 255) if in_rect(x, y, 52, 36, 6, 6) else None),
    ('Fringe', fringe),
]

PROMPTS = [
    ('HairBack.L', [{'op': 'flood', 'point': [26, 50], 'tolerance': 8}, {'op': 'side', 'axisX': 48, 'keep': 'left'}]),
    ('HairBack.R', [{'op': 'flood', 'point': [70, 50], 'tolerance': 8}, {'op': 'side', 'axisX': 48, 'keep': 'right'}]),
    ('Chest', [{'op': 'flood', 'point': [48, 100], 'tolerance': 0}]),
    ('Head', [{'op': 'flood', 'point': [48, 60], 'tolerance': 8}]),
    ('Eye.L', [{'op': 'flood', 'point': [40, 38], 'tolerance': 0}]),
    ('Eye.R', [{'op': 'flood', 'point': [54, 38], 'tolerance': 0}]),
    ('Fringe', [{'op': 'boundedFill', 'point': [48, 22], 'lineThreshold': 40}, {'op': 'grow', 'radius': 3},
                {'op': 'box', 'rect': [30, 16, 36, 14], 'mode': 'intersect'}]),
]


def flatten():
    """Straight-alpha RGBA bytes and, per pixel, the index of the topmost layer that paints it."""
    rgba, owner = bytearray(W * H * 4), [None] * (W * H)
    for y in range(H):
        for x in range(W):
            for index, (_, colour) in enumerate(LAYERS):
                value = colour(x, y)
                if value:
                    at = y * W + x
                    rgba[at * 4:at * 4 + 4] = bytes(value)
                    owner[at] = index
    return bytes(rgba), owner


def premultiply(rgba):
    out = bytearray(rgba)
    for at in range(0, len(out), 4):
        alpha = out[at + 3]
        for c in range(3):
            out[at + c] = out[at + c] * alpha // 255
    return bytes(out)


# ----------------------------------------------------------------------------------------------- PNG

def encode_png(width, height, rgba):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\0' + rgba[y * width * 4:(y + 1) * width * 4] for y in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def decode_png(data):
    """RGBA 8-bit PNG with any scanline filter, as the Rust encoder writes them."""
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    offset, compressed = 8, b''
    while offset < len(data):
        size = struct.unpack('>I', data[offset:offset + 4])[0]
        kind, payload = data[offset + 4:offset + 8], data[offset + 8:offset + 8 + size]
        if kind == b'IHDR':
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', payload)
            assert (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0), payload
        elif kind == b'IDAT':
            compressed += payload
        offset += size + 12
    raw, previous, pixels, stride = zlib.decompress(compressed), bytes(width * 4), bytearray(), width * 4
    for y in range(height):
        start = y * (stride + 1)
        mode, row = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        for x in range(stride):
            left = row[x - 4] if x >= 4 else 0
            up = previous[x]
            corner = previous[x - 4] if x >= 4 else 0
            if mode == 0:
                predictor = 0
            elif mode == 1:
                predictor = left
            elif mode == 2:
                predictor = up
            elif mode == 3:
                predictor = (left + up) // 2
            else:
                assert mode == 4, mode
                p = left + up - corner
                predictor = min((left, up, corner), key=lambda n: abs(p - n))
            row[x] = (row[x] + predictor) & 255
        pixels.extend(row)
        previous = row
    return width, height, bytes(pixels)


# ----------------------------------------------------------------------------------------------- MCP host

class Host:
    """One `soul2d-core --mcp` child; every request and response is kept in <name>.jsonl."""

    def __init__(self, run, name, binary):
        self.run, self.name = run, name
        self.stderr = (run / f'{name}.stderr').open('w')
        self.wire = (run / f'{name}.jsonl').open('w')
        self.child = subprocess.Popen([str(binary), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=self.stderr, text=True)
        self.sequence = 0
        self.rpc('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                                'clientInfo': {'name': 'part-split-verifier', 'version': '1'}})

    def rpc(self, method, params):
        self.sequence += 1
        message = {'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}
        self.wire.write(json.dumps({'request': message}) + '\n')
        self.child.stdin.write(json.dumps(message) + '\n')
        self.child.stdin.flush()
        ready, _, _ = select.select([self.child.stdout], [], [], 60)
        assert ready, f'{method} did not answer within 60s'
        line = self.child.stdout.readline()
        assert line, 'host closed stdout'
        response = json.loads(line)
        shown = json.loads(line)
        for item in shown.get('result', {}).get('content', []):
            if item.get('type') == 'image':
                image = base64.b64decode(item.pop('data'), validate=True)
                item['imageSha256'] = hashlib.sha256(image).hexdigest()
        if method == 'tools/call' and params['name'] == 'texture_get' and not shown['result'].get('isError'):
            metadata = shown['result']['structuredContent']
            image = base64.b64decode(metadata.pop('base64'), validate=True)
            image_path = self.run / f'{self.name}-texture-{self.sequence}.png'
            image_path.write_bytes(image)
            metadata.update(imageSha256=hashlib.sha256(image).hexdigest(), imageFile=image_path.name)
            shown['result']['content'] = [{'type': 'text', 'text': json.dumps(metadata)}]
        self.wire.write(json.dumps({'response': shown}) + '\n')
        self.wire.flush()
        assert response['id'] == self.sequence and 'error' not in response, response
        return response['result']

    def call(self, tool_name, /, **arguments):
        result = self.rpc('tools/call', {'name': tool_name, 'arguments': arguments})
        assert not result.get('isError', False), (tool_name, result)
        return result

    def tool(self, tool_name, /, **arguments):
        result = self.call(tool_name, **arguments)
        return result.get('structuredContent') or json.loads(result['content'][0]['text'])

    def refused(self, tool_name, needle, /, **arguments):
        result = self.rpc('tools/call', {'name': tool_name, 'arguments': arguments})
        assert result.get('isError') is True, (tool_name, result)
        text = result['content'][0]['text']
        assert needle in text, (tool_name, text)

    def close(self):
        self.child.stdin.close()
        try:
            self.child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.child.terminate()
            self.child.wait(timeout=5)
        self.wire.close()
        self.stderr.close()
        assert self.child.returncode == 0, self.child.returncode


# ----------------------------------------------------------------------------------------------- checks

def parts_of(snapshot):
    children = snapshot['model']['nodes'].get('children') or []
    return [{'name': n['name'], 'id': n['uuid'], 'slot': n['textures'][0], 'trans': n['transform']['trans'][:2]}
            for n in children if n['type'] == 'Part']


def placed(host, snapshot):
    """Each Part's texture on source coordinates: (name, width, height, left, top, rgba, png bytes)."""
    out = []
    for part in parts_of(snapshot):
        texture = host.tool('texture_get', slot=part['slot'])
        png = base64.b64decode(texture['base64'], validate=True)
        width, height, rgba = decode_png(png)
        left = int(part['trans'][0]) + W // 2 - width // 2
        top = int(part['trans'][1]) + H // 2 - height // 2
        out.append((part['name'], width, height, left, top, rgba, png))
    return out


def alpha_set(item):
    _, width, height, left, top, rgba, _ = item
    covered = set()
    for y in range(height):
        for x in range(width):
            if rgba[(y * width + x) * 4 + 3] and 0 <= left + x < W and 0 <= top + y < H:
                covered.add((top + y) * W + left + x)
    return covered


def stack(items):
    canvas = [[0, 0, 0, 0] for _ in range(W * H)]
    for _, width, height, left, top, rgba, _ in items:
        for y in range(height):
            for x in range(width):
                sx, sy = left + x, top + y
                if not (0 <= sx < W and 0 <= sy < H):
                    continue
                pixel = rgba[(y * width + x) * 4:(y * width + x) * 4 + 4]
                below = canvas[sy * W + sx]
                keep = 255 - pixel[3]
                for c in range(4):
                    below[c] = pixel[c] + below[c] * keep // 255
    return b''.join(bytes(p) for p in canvas)


def verify(run, binary):
    evidence = {}
    source = run / 'character.png'
    rgba, owner = flatten()
    source.write_bytes(encode_png(W, H, rgba))

    host = Host(run, 'host-a', binary)
    try:
        names = {tool['name'] for tool in host.rpc('tools/list', {})['tools']}
        wanted = {'split_open', 'split_part_set', 'split_part_remove', 'split_part_order', 'split_plan',
                  'split_overlay', 'split_apply', 'split_close'}
        assert wanted <= names, wanted - names
        host.tool('new')
        before = host.tool('snapshot')

        opened = host.tool('split_open', path=str(source))
        assert (opened['width'], opened['height']) == (W, H), opened
        overlay = host.call('split_overlay')
        image = next(c for c in overlay['content'] if c['type'] == 'image')
        assert decode_png(base64.b64decode(image['data']))[:2] == (W, H)
        (run / 'overlay-empty.png').write_bytes(base64.b64decode(image['data']))
        for name, steps in PROMPTS:
            stats = host.tool('split_part_set', name=name, steps=steps)
            assert stats['part']['pixels'] > 0, stats
        overlay = host.call('split_overlay')
        (run / 'overlay-parts.png').write_bytes(base64.b64decode(next(c for c in overlay['content'] if c['type'] == 'image')['data']))
        plan = host.tool('split_plan')
        (run / 'plan.json').write_text(json.dumps(plan['plan'], indent=2))
        assert plan['unclaimedPixels'] == 0 and plan['multiplyClaimedPixels'] == 0, plan
        evidence['planStatistics'] = {p['name']: {'pixels': p['pixels'], 'bbox': p['bbox']} for p in plan['parts']}

        draft = host.tool('snapshot')
        for key in ('model', 'revision', 'history', 'modified', 'canUndo'):
            assert draft[key] == before[key], key
        assert draft['partSplit']['parts'] == len(PROMPTS)

        applied = host.tool('split_apply')
        assert [c['name'] for c in applied['created']] == [name for name, _ in PROMPTS]
        after = host.tool('snapshot')
        assert after['history']['count'] == before['history']['count'] + 1, after['history']
        assert after['modified'] is True

        items = placed(host, after)
        assert [item[0] for item in items] == [layer[0] for layer in LAYERS], 'draw order'
        iou = {}
        for index, item in enumerate(items):
            truth = {at for at, o in enumerate(owner) if o == index}
            mine = alpha_set(item)
            iou[item[0]] = len(truth & mine) / len(truth | mine)
            assert iou[item[0]] >= MIN_IOU, (item[0], iou[item[0]])
        evidence['iou'] = iou
        stacked = stack(items)
        expected = premultiply(rgba)
        mismatched = sum(1 for at in range(W * H) if stacked[at * 4:at * 4 + 4] != expected[at * 4:at * 4 + 4])
        evidence['stackedMismatchedPixels'] = mismatched
        assert mismatched == 0, mismatched

        render = host.call('render', width=W, height=H, bounds=[-(W // 2), -(H // 2), W - W // 2, H - H // 2])
        render_png = base64.b64decode(next(c for c in render['content'] if c['type'] == 'image')['data'])
        rendered = decode_png(render_png)
        (run / 'render.png').write_bytes(render_png)
        assert rendered[:2] == (W, H)
        close = sum(1 for at in range(W * H) if rgba[at * 4 + 3] == 255
                    and all(abs(rendered[2][at * 4 + c] - rgba[at * 4 + c]) <= 2 for c in range(3)))
        opaque = sum(1 for at in range(W * H) if rgba[at * 4 + 3] == 255)
        evidence['renderWithinTwoLevels'] = f'{close}/{opaque}'
        assert close >= 0.99 * opaque, evidence['renderWithinTwoLevels']

        undone = host.tool('undo')
        assert undone['model'] == before['model'] and not parts_of(undone), 'one undo removes every Part'
        redone = host.tool('redo')
        assert redone['model'] == after['model'], 'one redo restores every Part'
        assert host.tool('snapshot')['history']['count'] == before['history']['count'] + 1
        evidence['undoRedo'] = 'one step each'

        host.refused('split_part_set', 'outside', name='Bad', steps=[{'op': 'flood', 'point': [W, 0]}])
        host.refused('split_part_set', 'cycle', name='Chest', steps=[{'op': 'part', 'name': 'Chest'}])
        assert host.tool('snapshot')['partSplit']['parts'] == len(PROMPTS)
        evidence['refusals'] = 'out of range point and self reference refused, draft unchanged'
        first_parts = [(item[0], item[1], item[2], item[3], item[4], item[6]) for item in items]
        first_model = redone['model']

        root = host.tool('node_create', className='DepthRigRoot', name='SplitRig')['created'][0]['uuid']
        host.tool('rig_add_skeleton', root=root)
        bones = {bone['boneId']: bone for bone in host.tool('rig_list_bones', root=root)['items']}
        assert len(bones) == 19
        host.tool('rig_set_constraint', bone=bones['Head']['uuid'], constraint={'allowParentToTargets': True})
        for part in applied['created']:
            bone = bones['Chest' if part['name'] == 'Chest' else 'Head']['uuid']
            host.tool('rig_add_source', root=root, target=part['id'], bone=bone)
        host.tool('rig_fit_human', root=root)
        fitted = {bone['boneId']: bone for bone in host.tool('rig_list_bones', root=root)['items']}
        for name, head, tail in [('Head', [0, -2, 0], [0, -36, 0]), ('Chest', [0, 56, 0], [0, 2, 0])]:
            assert all(abs(a - b) < 1e-4 for a, b in zip(fitted[name]['restHead'], head)), fitted[name]
            assert all(abs(a - b) < 1e-4 for a, b in zip(fitted[name]['restTail'], tail)), fitted[name]
        before_parameters = host.tool('snapshot')
        parameters = host.tool('rig_add_standard_parameters', root=root)
        assert [p['name'] for p in parameters] == ['Face::Yaw-Pitch', 'Face::Roll', 'Body::Yaw-Pitch', 'Body::Roll']
        rigged = host.tool('snapshot')
        assert rigged['partSplit']['applied'] is True
        assert rigged['history']['count'] == before_parameters['history']['count'] + 1
        rig_node = next(n for n in rigged['model']['nodes']['children'] if n['uuid'] == root)
        assert {b['target'] for b in rig_node['bindings']} == {p['id'] for p in applied['created']}
        for binding in rig_node['bindings']:
            part = next(p for p in applied['created'] if p['id'] == binding['target'])
            bone = bones['Chest' if part['name'] == 'Chest' else 'Head']['uuid']
            assert binding['sourceBoneUuids'] == [bone]
            assert binding['vertexWeights'] == [[1, 1, 1, 1]], 'one source has full weight on all four vertices'
        roll = next(p['uuid'] for p in parameters if p['name'] == 'Face::Roll')

        def rig_render():
            response = host.call('render', width=W, height=H,
                                 bounds=[-(W // 2), -(H // 2), W - W // 2, H - H // 2])
            return base64.b64decode(next(c['data'] for c in response['content'] if c['type'] == 'image'))

        neutral = rig_render()
        host.tool('value_set', id=roll, value=[1, 0])
        moved = rig_render()
        assert decode_png(moved)[2] != decode_png(neutral)[2], 'Face::Roll must move the split Parts'
        host.tool('value_set', id=roll, value=[0, 0])
        assert decode_png(rig_render())[2] == decode_png(neutral)[2], 'reset restores the neutral render'
        runtime = host.tool('snapshot')
        assert runtime['model'] == rigged['model'] and runtime['history'] == rigged['history']
        assert host.tool('undo')['model'] == before_parameters['model']
        assert host.tool('redo')['model'] == rigged['model']
        assert host.tool('snapshot')['partSplit']['applied'] is True
        (run / 'rig-neutral.png').write_bytes(neutral)
        (run / 'rig-face-roll.png').write_bytes(moved)
        evidence['rigFollowOn'] = {'bones': 19, 'weightedParts': len(applied['created']),
            'parameters': [p['name'] for p in parameters], 'faceRollMovesParts': True,
            'standardParametersUndoRedo': 'one step each', 'splitAppliedPreserved': True}
    finally:
        host.close()

    fresh = Host(run, 'host-b', binary)
    try:
        fresh.tool('new')
        assert fresh.tool('snapshot')['partSplit']['open'] is False
        saved = json.loads((run / 'plan.json').read_text())
        replayed = fresh.tool('split_apply', plan=saved)
        assert len(replayed['created']) == len(PROMPTS)
        snapshot = fresh.tool('snapshot')
        assert snapshot['partSplit']['open'] is False, 'a plan replays without a draft'
        again = [(item[0], item[1], item[2], item[3], item[4], item[6]) for item in placed(fresh, snapshot)]
        assert again == first_parts, 'names, sizes, offsets and texture bytes'
        assert snapshot['model'] == first_model, 'the whole model'
        evidence['planReplay'] = 'fresh host produced identical Parts, offsets and texture bytes'
    finally:
        fresh.close()
    return evidence


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    run = ROOT / 'build/verification' / (datetime.now().strftime('%Y%m%d-%H%M%S') + '-part-split')
    run.mkdir(parents=True)
    print(run, flush=True)
    binary_hash = hashlib.sha256(args.binary.read_bytes()).hexdigest()
    result = {'binary': str(args.binary.resolve()), 'binarySha256': binary_hash,
              'verifierSha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'scope': 'Generated fixture and existing rig follow-on only; no real-character, AI/provider, native GUI or vendor acceptance'}
    try:
        result['evidence'] = verify(run, args.binary.resolve())
        assert hashlib.sha256(args.binary.read_bytes()).hexdigest() == binary_hash, 'binary changed during proof'
        assert all('iVBORw0K' not in path.read_text() for path in run.glob('*.jsonl')), \
            'PNG payload leaked into a text transcript'
        result['passed'] = True
    except Exception:
        result['passed'] = False
        result['error'] = traceback.format_exc()
    (run / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    sys.exit(0 if result['passed'] else 1)


if __name__ == '__main__':
    main()
