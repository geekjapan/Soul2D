#!/usr/bin/env python3
import argparse
import base64
import ctypes
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import threading
import zlib


def png(width, height, pixels):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\0' + pixels[y * width * 4:(y + 1) * width * 4] for y in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def fixture(path):
    transform = {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]}
    part = {'uuid': 2, 'name': 'Card', 'type': 'Part', 'enabled': True, 'zsort': 0,
            'transform': transform, 'children': [], 'lockToRoot': False, 'pinToMesh': False,
            'mesh': {'verts': [2, 2, 6, 2, 6, 6, 2, 6], 'uvs': [0, 0, 1, 0, 1, 1, 0, 1],
                     'indices': [0, 1, 2, 0, 2, 3], 'origin': [0, 0]},
            'vertices': [[2, 2], [6, 2], [6, 6], [2, 6]],
            'uvs': [[0, 0], [1, 0], [1, 1], [0, 1]],
            'indices': [0, 1, 2, 0, 2, 3],
            'textures': [0], 'blend_mode': 'Normal', 'tint': [1, 1, 1], 'screenTint': [0, 0, 0],
            'emissionStrength': 1, 'opacity': 1, 'future_part': {'version': 17}}
    model = {'meta': {'name': 'Rust migration fixture', 'version': '0.0.1'},
             'physics': {'pixelsPerMeter': 1000, 'gravity': 9.8},
             'nodes': {'uuid': 1, 'name': 'Root', 'type': 'Node', 'enabled': True, 'zsort': 0,
                       'transform': transform, 'lockToRoot': False, 'pinToMesh': False, 'children': [part]},
             'param': [], 'automation': [], 'animations': {}, 'groups': [],
             'future_root': {'nested': [1, {'keep': 'unknown'}], 'large_integer': 18446744073709551617}}
    texture = png(2, 2, bytes([255, 0, 0, 255]) * 4)
    tail = (b'TEX_SECT' + struct.pack('>I', 2) + struct.pack('>IB', len(texture), 0) + texture
            + struct.pack('>IB', 0, 0) + b'EXT_SECT' + struct.pack('>I', 1)
            + struct.pack('>I', 6) + b'future' + struct.pack('>I', 5) + b'\0\xffABC' + b'FUTURE_TAIL')
    raw = json.dumps(model, separators=(',', ':')).encode()
    path.write_bytes(b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw + tail)
    return model, tail


def split_container(data):
    assert data[:8] == b'TRNSRTS\0'
    size = struct.unpack('>I', data[8:12])[0]
    return json.loads(data[12:12 + size]), data[12 + size:]


def decode_rgba(data):
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    offset, compressed = 8, b''
    while offset < len(data):
        size = struct.unpack('>I', data[offset:offset + 4])[0]
        kind, payload = data[offset + 4:offset + 8], data[offset + 8:offset + 8 + size]
        if kind == b'IHDR':
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', payload)
            assert (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0)
        elif kind == b'IDAT':
            compressed += payload
        offset += size + 12
    raw, previous, pixels = zlib.decompress(compressed), bytes(width * 4), bytearray()
    stride = width * 4
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
                assert mode == 4
                p = left + up - corner
                predictor = min((left, up, corner), key=lambda n: abs(p - n))
            row[x] = (row[x] + predictor) & 255
        pixels.extend(row)
        previous = row
    return width, height, bytes(pixels)


class Core:
    def __init__(self, library):
        self.lib = ctypes.CDLL(str(library))
        self.lib.vsoul_session_new.restype = ctypes.c_void_p
        self.lib.vsoul_session_free.argtypes = [ctypes.c_void_p]
        self.lib.vsoul_session_request.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        self.lib.vsoul_session_request.restype = ctypes.c_void_p
        self.lib.vsoul_string_free.argtypes = [ctypes.c_void_p]
        self.handle = self.lib.vsoul_session_new()
        assert self.handle

    def request(self, op, success=True, **args):
        raw = self.lib.vsoul_session_request(self.handle, json.dumps({'op': op, **args}).encode())
        assert raw, op
        try:
            result = json.loads(ctypes.string_at(raw))
        finally:
            self.lib.vsoul_string_free(raw)
        assert result['succeeded'] == success, (op, result)
        return result.get('result')

    def close(self):
        if self.handle:
            self.lib.vsoul_session_free(self.handle)
            self.handle = None


def encode_minimal_psd(width, height, layers):
    def be16(value):
        return int(value & 0xFFFF).to_bytes(2, 'big')

    def be32(value):
        return int(value & 0xFFFFFFFF).to_bytes(4, 'big')

    info = bytearray(be16(len(layers)))
    pixels = bytearray()
    for name, rgba in layers:
        info += be32(0) + be32(0) + be32(height) + be32(width)
        info += be16(4)
        channel_len = be32(2 + width * height)
        for channel_id in (0, 1, 2, 0xFFFF):
            info += be16(channel_id) + channel_len
        info += b'8BIMnorm' + bytes([255, 0, 0, 0])
        raw_name = name.encode('ascii')
        pascal = bytes([len(raw_name)]) + raw_name
        pascal += bytes((-len(pascal)) % 4)
        info += be32(8 + len(pascal)) + be32(0) + be32(0) + pascal
        count = width * height
        for channel in range(4):
            pixels += b'\x00\x00'
            pixels += bytes(rgba[index * 4 + channel] for index in range(count))
    info += pixels
    if len(info) % 2:
        info.append(0)
    section = be32(len(info)) + info + be32(0)
    return (b'8BPS' + be16(1) + bytes(6) + be16(3) + be32(height) + be32(width) +
            be16(8) + be16(3) + be32(0) + be32(0) + be32(len(section)) + section)


def close_transform(node):
    transform = node['transform']
    return {
        'name': node['name'],
        'type': node.get('type'),
        'trans': [float(v) for v in transform['trans']],
        'rot': [float(v) for v in transform['rot']],
        'scale': [float(v) for v in transform['scale']],
    }


def find_child(nodes, node_id):
    if nodes.get('uuid') == node_id:
        return nodes
    for child in nodes.get('children') or []:
        found = find_child(child, node_id)
        if found:
            return found
    return None


def verify_node_tree(core, other, directory, original, tail):
    core.request('open', path=str(original))
    parent = core.request('node.create', className='Node', _suffix='', name='Tree Parent')['created'][0]['uuid']
    child = core.request('node.create', className='Node', _suffix='', name='Tree Child')['created'][0]['uuid']
    composite = core.request('node.create', className='Composite', _suffix='', name='Tree Composite')['created'][0]
    assert composite['type'] == 'Composite', composite
    core.request('node.move', nodes=[child], newParent=parent, index=0)
    core.request('node.move', nodes=[parent], newParent=composite['uuid'], index=0)
    before_delete = core.request('snapshot')
    core.request('node.delete', nodes=[child])
    assert find_child(core.request('snapshot')['model']['nodes'], child) is None
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], child) is not None
    core.request('redo')
    assert find_child(core.request('snapshot')['model']['nodes'], child) is None
    core.request('undo')
    assert core.request('snapshot')['model'] == before_delete['model']
    reject_before = core.request('snapshot')
    core.request('node.create', False, className='Shapes', _suffix='')
    assert core.request('snapshot') == reject_before
    saved = directory / 'node-tree.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reopened = other.request('snapshot')['model']['nodes']
    assert find_child(reopened, composite['uuid'])['name'] == 'Tree Composite'
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    print('PASS Node tree: move/delete/undo/redo, Composite create, Shapes rejection, save/reload')


def verify_node_clipboard(core, other, directory, original, tail):
    core.request('open', path=str(original))
    part = core.request('node.copy', nodes=[2])
    assert part['count'] == 1 and part['clipboard'][0]['name'] == "Card'"
    assert part['clipboard'][0]['uuid'] != 2
    before_copy = core.request('snapshot')
    assert before_copy['model']['nodes']['children'][0]['uuid'] == 2
    assert before_copy['clipboard'][0]['name'] == "Card'"
    pasted = core.request('node.paste', parent=1, index=0)
    pasted_node = pasted['created'][0]
    assert pasted_node['name'] == "Card'" and pasted_node['uuid'] != 2
    assert pasted_node['textures'] == [0]
    assert pasted_node['mesh']['indices'] == [0, 1, 2, 0, 2, 3]
    assert core.request('snapshot')['clipboard'] == []
    core.request('undo')
    assert core.request('snapshot')['model']['nodes']['children'][0]['uuid'] == 2
    assert len(core.request('snapshot')['model']['nodes']['children']) == 1
    core.request('redo')
    parent = core.request('node.create', className='Node', _suffix='', name='Clipboard Parent')['created'][0]['uuid']
    child = core.request('node.create', className='Node', _suffix='', name='Clipboard Child', parents=[parent])['created'][0]['uuid']
    grandchild = core.request('node.create', className='Node', _suffix='', name='Clipboard Grandchild', parents=[child])['created'][0]['uuid']
    core.request('node.transform', id=child, translation=[3, 4, 1])
    before_model = core.request('snapshot')['model']
    copied = core.request('node.copy', nodes=[child])
    assert core.request('snapshot')['model'] == before_model
    assert copied['clipboard'][0]['name'] == "Clipboard Child'"
    pasted = core.request('node.paste', parent=parent, index=0)['created'][0]
    assert pasted['name'] == "Clipboard Child'" and pasted['uuid'] != child
    assert pasted['children'][0]['name'] == "Clipboard Grandchild'"
    assert pasted['children'][0]['uuid'] != grandchild
    assert abs(pasted['transform']['trans'][0] - 3) < 1e-6
    assert abs(pasted['transform']['trans'][2] - 1) < 1e-6
    before_paste = core.request('snapshot')['model']
    core.request('undo')
    assert len(find_child(core.request('snapshot')['model']['nodes'], parent)['children']) == 1
    core.request('redo')
    assert core.request('snapshot')['model'] == before_paste
    pasted_id = pasted['uuid']
    core.request('node.cut', nodes=[pasted_id])
    assert find_child(core.request('snapshot')['model']['nodes'], pasted_id) is None
    assert core.request('snapshot')['clipboard'][0]['source'] == pasted_id
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], pasted_id)['name'] == "Clipboard Child'"
    converted = core.request('node.convert', nodes=[parent], className='Composite')['converted'][0]
    assert converted['type'] == 'Composite' and converted['uuid'] == parent
    assert converted['blend_mode'] == 'Normal'
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['type'] == 'Node'
    core.request('redo')
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['type'] == 'Composite'
    core.request('node.convert', nodes=[parent], className='Camera')
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['type'] == 'Camera'
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['viewport'] == [1920, 1080]
    core.request('node.convert', nodes=[parent], className='GridDeformer')
    grid = find_child(core.request('snapshot')['model']['nodes'], parent)
    assert grid['type'] == 'GridDeformer'
    assert grid['grid_axis_x'] == [-0.5, 0.5]
    assert grid['grid_axis_y'] == [-0.5, 0.5]
    assert grid['formation'] == 'Bilinear'
    assert grid['dynamic'] is False
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['type'] == 'Camera'
    core.request('redo')
    assert find_child(core.request('snapshot')['model']['nodes'], parent)['type'] == 'GridDeformer'
    reject_before = core.request('snapshot')
    for class_name in ('Part', 'DepthBone', 'SimplePhysics'):
        core.request('node.convert', False, nodes=[parent], className=class_name)
        assert core.request('snapshot') == reject_before
    saved = directory / 'node-clipboard.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reopened = other.request('snapshot')['model']['nodes']
    assert find_child(reopened, parent)['type'] == 'GridDeformer'
    assert find_child(reopened, parent)['grid_axis_x'] == [-0.5, 0.5]
    assert find_child(reopened, pasted_id)['children'][0]['name'] == "Clipboard Grandchild'"
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    print('PASS Node clipboard: copy/cut/paste/undo, structural convert, GridDeformer convert, Part rejection, save/reload')


def verify_part_mesh(core):
    core.request('new')
    created = core.request('node.create', className='Node', name='Card')['created'][0]
    core.request('node.convert', nodes=[created['uuid']], className='Part')
    part = core.request('node.get', id=created['uuid'])
    assert part['vertices'] == [] and part['uvs'] == [] and part['indices'] == []
    core.request('undo')
    assert core.request('node.get', id=created['uuid'])['type'] == 'Node'
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node',
            'children': [{
                'uuid': 2, 'name': 'Tri', 'type': 'Part',
                'vertices': [[0, 0], [1, 0], [0, 1]],
                'uvs': [[0, 0], [1, 0], [0, 1]],
                'indices': [0, 1, 2],
            }],
        },
        'param': [],
    })
    tri = core.request('node.get', id=2)
    assert tri['vertices'] == [[0, 0], [1, 0], [0, 1]]
    assert tri['uvs'] == tri['vertices'] and tri['indices'] == [0, 1, 2]
    held = core.request('snapshot')
    core.request('node.convert', False, nodes=[2], className='Part')
    assert core.request('snapshot') == held
    core.request('node.convert', False, nodes=[2], className='Node')
    assert core.request('node.get', id=2)['type'] == 'Part'
    assert core.request('node.get', id=2)['vertices'] == [[0, 0], [1, 0], [0, 1]]
    assert core.request('snapshot') == held
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    held = core.request('snapshot')
    core.request('node.convert', False, nodes=[grid['uuid']], className='Part')
    assert core.request('snapshot') == held
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node',
            'children': [{
                'uuid': 4, 'name': 'Layer', 'type': 'DynamicComposite',
                'mesh': {
                    'verts': [0, 0, 2, 0, 0, 2],
                    'uvs': [0, 0, 1, 0, 0, 1],
                    'indices': [0, 1, 2],
                    'origin': [3, -1],
                },
            }],
        },
        'param': [],
    })
    core.request('node.convert', nodes=[4], className='Part')
    layer = core.request('node.get', id=4)
    assert layer['type'] == 'Part'
    assert layer['vertices'] == [[0, 0], [2, 0], [0, 2]]
    assert layer['indices'] == [0, 1, 2]
    assert layer['mesh']['origin'] == [3, -1]
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node',
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [{
                'uuid': 2, 'name': 'Partial', 'type': 'Part',
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'mesh': {'verts': [0, 0, 1, 0, 0, 1], 'origin': [1, 2], 'note': 'keep'},
            }],
        },
        'param': [],
    })
    partial = core.request('node.get', id=2)
    assert partial['uvs'] == [[0, 0], [1, 0], [0, 1]]
    assert partial['mesh']['uvs'] == [0, 0, 1, 0, 0, 1]
    assert partial['mesh']['indices'] == []
    assert partial['mesh']['note'] == 'keep'
    core.request('render', width=2, height=2, bounds=[-1, -1, 1, 1])
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node',
            'children': [{
                'uuid': 3, 'name': 'Cover', 'type': 'Mask', 'textures': [0],
                'mesh': {
                    'verts': [0, 0, 1, 0, 0, 1],
                    'uvs': [0, 0, 1, 0, 0, 1],
                    'indices': [0, 1, 2],
                    'origin': [0, 0],
                },
            }],
        },
        'param': [],
    })
    core.request('node.convert', nodes=[3], className='Part')
    cover = core.request('node.get', id=3)
    assert cover['type'] == 'Part' and cover['textures'] == [0]
    print('PASS Part mesh: empty Node convert, triangle round-trip, Grid stays closed, DynamicComposite mesh kept')


def verify_node_slice(core, other, directory, original, tail):
    core.request('open', path=str(original))
    listed = core.request('nodes.list')
    assert any(entry['uuid'] == 1 for entry in listed), listed
    created = core.request('node.create', className='Node', _suffix='', name='Slice Node')
    node_id = created['created'][0]['uuid']
    core.request('node.rename', id=node_id, name='Slice Renamed')
    core.request('node.transform', id=node_id, translation=[1, 2, 0], rotation=[0, 0, 0.5], scale=[1.5, 1.5])
    before = core.request('snapshot')
    core.request('node.transform', id=node_id, component='translationX', value=9)
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    core.request('redo')
    reject_before = core.request('snapshot')
    core.request('node.create', False, className='DepthBone', _suffix='')
    assert core.request('snapshot') == reject_before
    saved = directory / 'node-slice.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reopened = other.request('snapshot')['model']['nodes']['children']
    node = next(child for child in reopened if child['uuid'] == node_id)
    oracle = close_transform(node)
    (directory / 'node-slice-oracle.json').write_text(json.dumps(oracle, indent=2) + '\n')
    assert oracle['name'] == 'Slice Renamed'
    assert abs(oracle['trans'][0] - 9) < 1e-6
    assert abs(oracle['rot'][2] - 0.5) < 1e-6
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    print('PASS Node slice: list/create/rename/transform/undo/redo/save/reload and DepthBone rejection')


def verify_deformers(core, other, directory, original, tail):
    core.request('open', path=str(original))
    grid = core.request('node.create', className='GridDeformer', _suffix='', name='Verify Grid')['created'][0]
    grid_id = grid['uuid']
    assert grid['grid_axis_x'] == [-0.5, 0.5] and grid['formation'] == 'Bilinear'
    path = core.request('node.create', className='PathDeformer', _suffix='', name='Verify Path', parents=[grid_id])['created'][0]
    path_id = path['uuid']
    assert path['curve_type'] == 'Spline' and path['vertices'] == [] and 'physics' not in path
    mesh = core.request('node.create', className='MeshGroup', _suffix='', name='Verify Mesh')['created'][0]
    mesh_id = mesh['uuid']
    assert mesh['dynamic_deformation'] is False and mesh['translate_children'] is True
    assert mesh['mesh']['verts'] == [] and mesh['mesh']['indices'] == []
    plain = core.request('node.create', className='Node', _suffix='', name='Verify Plain')['created'][0]['uuid']
    before = core.request('snapshot')
    core.request('deformer.define_vertices', False, nodes=[grid_id], vertices=[0, 0, 1, 1])
    assert core.request('snapshot') == before
    mixed = core.request('deformer.define_grid', nodes=[grid_id, plain], axisX=[-8, 8], axisY=[-7, 7])
    assert mixed['nodes'] == [grid_id]
    assert find_child(core.request('snapshot')['model']['nodes'], plain).get('grid_axis_x') is None
    assert find_child(core.request('snapshot')['model']['nodes'], grid_id)['grid_axis_x'] == [-8, 8]
    only_plain = core.request('snapshot')
    core.request('deformer.define_grid', False, nodes=[plain], axisX=[0, 1], axisY=[0, 1])
    assert core.request('snapshot') == only_plain
    core.request('deformer.define_vertices', nodes=[path_id], vertices=[0, 0, 4, 1, 8, -2])
    core.request('deformer.define_grid', nodes=[grid_id, plain], axisX=[2, 0], axisY=[0, 1, 0])
    grid_node = find_child(core.request('snapshot')['model']['nodes'], grid_id)
    assert grid_node['grid_axis_x'] == [0, 2]
    assert grid_node['grid_axis_y'] == [0, 1]
    core.request('deformer.set', nodes=[grid_id], property='GridDeformDynamic', value=True)
    core.request('deformer.set', nodes=[path_id], property='PathDeformCurveType', value='Bezier')
    core.request('deformer.set', nodes=[path_id], property='PathDeformPhysicsEnabled', value=True)
    core.request('deformer.set', nodes=[path_id], property='PathDeformGravity', value=2.25)
    core.request('deformer.set', nodes=[mesh_id], property='MeshGroupTranslateChildren', value=False)
    core.request('deformer.define_mesh', nodes=[mesh_id], vertices=[-1, -1, 1, -1, -1, 1, 1, 1], indices=[0, 1, 2, 1, 3, 2])
    edited = core.request('snapshot')
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], mesh_id)['mesh']['verts'] == []
    core.request('redo')
    assert core.request('snapshot')['model'] == edited['model']
    core.request('node.convert', nodes=[grid_id], className='PathDeformer')
    converted = find_child(core.request('snapshot')['model']['nodes'], grid_id)
    assert converted['type'] == 'PathDeformer'
    assert converted['vertices'] == [0, 0, 2, 0, 0, 1, 2, 1]
    core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], grid_id)['type'] == 'GridDeformer'
    before_binding = core.request('snapshot')
    binding_model = json.loads(json.dumps(before_binding['model']))
    binding_model['param'] = [{
        'uuid': 44, 'name': 'Deform', 'is_vec2': False,
        'min': [0, 0], 'max': [1, 0], 'defaults': [0, 0],
        'axis_points': [[0, 1], [0]],
        'bindings': [{'node': grid_id, 'param_name': 'deform', 'values': [], 'isSet': []}],
    }]
    core.request('new', model=binding_model)
    blocked = core.request('snapshot')
    core.request('deformer.define_grid', False, nodes=[grid_id], axisX=[0, 1], axisY=[0, 1])
    assert core.request('snapshot') == blocked
    saved = directory / 'deformers.inx'
    core.request('open', path=str(original))
    core.request('node.create', className='GridDeformer', _suffix='', name='Saved Grid')
    saved_id = core.request('snapshot')['model']['nodes']['children'][-1]['uuid']
    core.request('deformer.define_grid', nodes=[saved_id], axisX=[-4, 4], axisY=[-3, 3])
    core.request('deformer.set', nodes=[saved_id], property='GridDeformDynamic', value=True)
    core.request('save', path=str(saved), mark_saved=True)
    other.request('open', path=str(saved))
    reopened = find_child(other.request('snapshot')['model']['nodes'], saved_id)
    assert reopened['type'] == 'GridDeformer'
    assert reopened['dynamic'] is True
    assert reopened['grid_axis_x'] == [-4, 4]
    assert reopened['grid_axis_y'] == [-3, 3]
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    print('PASS Mesh deformers: create, control points, properties, undo, fail-closed, save/reload')
    verify_mesh_mirror(core)
    verify_depths(core, other, directory, tail)


def verify_mesh_mirror(core):
    core.request('new')
    path = core.request('node.create', className='PathDeformer', name='MirrorPath')['created'][0]
    core.request('deformer.define_vertices', nodes=[path['uuid']], vertices=[1, 4, 6, 1])
    core.request('mesh.mirror', node=path['uuid'], axis='y', origin=[2, 100], vertices=[0])
    mirrored = core.request('node.get', id=path['uuid'])
    assert mirrored['vertices'] == [1, 0, 6, 1]
    grid = core.request('node.create', className='GridDeformer', name='MirrorGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 2])
    held = core.request('snapshot')
    core.request('mesh.mirror', False, node=grid['uuid'], axis='x', origin=[0, 0], vertices=[1])
    assert core.request('snapshot') == held
    core.request('mesh.mirror', node=grid['uuid'], axis='x', origin=[0, 0], vertices=[0, 1, 2, 3])
    full = core.request('node.get', id=grid['uuid'])
    assert full['grid_axis_x'] == [-1, 0]
    assert full['grid_axis_y'] == [0, 2]
    core.request('mesh.mirror', node=grid['uuid'], axis='x', origin=[0, 0], vertices=[0, 1, 2, 3])
    assert core.request('node.get', id=grid['uuid'])['grid_axis_x'] == [0, 1]
    core.request('undo')
    assert core.request('node.get', id=grid['uuid'])['grid_axis_x'] == [-1, 0]
    print('PASS Mesh mirror: mirrorV uses origin x, a grid corner fails, a full mirror reverses the axis')


def verify_depths(core, other, directory, tail):
    transform = {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]}
    model = {
        'meta': {'name': 'depth', 'version': '1.0-alpha'},
        'physics': {},
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node', 'enabled': True, 'zsort': 0,
            'transform': transform, 'lockToRoot': False, 'pinToMesh': False,
            'children': [
                {
                    'uuid': 4, 'name': 'Body', 'type': 'GridDeformer', 'enabled': True,
                    'grid_axis_x': [-1, 1], 'grid_axis_y': [-2, 2],
                    'depths': [0, 1, -0.5, 0.25],
                    'depth-ops': [{'type': 'attached-point', 'index': 1, 'amount': 0.4}],
                    'depth-op-base-depths': [0, 0, 0, 0],
                    'future_grid': {'keep': True},
                },
                {
                    'uuid': 5, 'name': 'Hair', 'type': 'PathDeformer', 'enabled': True,
                    'vertices': [0, 0, 1, 1], 'depths': [9, 9],
                },
                {
                    'uuid': 6, 'name': 'Rig', 'type': 'DepthRigRoot', 'enabled': True,
                    'bindings': [{
                        'target': 4, 'targetKind': 'grid', 'sourceBoneUuids': [7],
                        'future_binding': {'keep': True},
                    }],
                    'children': [{
                        'uuid': 7, 'name': 'Spine', 'type': 'DepthBone', 'enabled': True,
                        'boneId': 'Spine', 'lockToRoot': True,
                        'restHead': [0, 0, 0], 'restTail': [0, 10, 1],
                        'children': [{
                            'uuid': 8, 'name': 'Badge', 'type': 'Node', 'enabled': True, 'future_badge': 'keep',
                        }],
                    }],
                },
            ],
        },
        'param': [], 'automation': [], 'animations': {},
    }
    raw = json.dumps(model, separators=(',', ':')).encode()
    source = directory / 'depth-rig.inx'
    source.write_bytes(b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw + tail)
    core.request('open', path=str(source))
    opened = core.request('snapshot')['model']
    grid = opened['nodes']['children'][0]
    assert grid['depths'] == [0, 1, -0.5, 0.25]
    assert grid['depth-ops'] == model['nodes']['children'][0]['depth-ops']
    assert grid['depth-op-base-depths'] == [0, 0, 0, 0]
    assert opened['nodes']['children'][1]['depths'] == [9, 9]
    assert opened['nodes']['children'][2]['bindings'][0]['sourceBoneUuids'] == [7]
    assert opened['nodes']['children'][2]['children'][0]['boneId'] == 'Spine'
    before = core.request('snapshot')
    core.request('node.create', False, className='DepthBone', name='Nope')
    core.request('node.transform', id=7, translation=[1, 2, 3], rotation=[0, 0, 0], scale=[1, 1])
    assert core.request('node.get', id=7)['transform']['trans'] == [1, 2, 3]
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    core.request('depth.set', target=5, depths=[0, 1])
    assert core.request('depth.list', target=5)['depths'] == [0, 1]
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    listed = core.request('depth.list', target=4)
    assert listed['count'] == 4 and listed['depths'] == [0, 1, -0.5, 0.25]
    core.request('depth.set', target=4, depths=[1e39, 0.25, -0.5, 1])
    stored = core.request('depth.list', target=4)
    assert stored['depths'] == [0, 0.25, -0.5, 1]
    assert core.request('node.get', id=4)['depth-ops'][0]['type'] == 'attached-point'
    edited = core.request('snapshot')
    core.request('undo')
    assert core.request('node.get', id=4)['depths'] == [0, 1, -0.5, 0.25]
    core.request('redo')
    restored = core.request('snapshot')['model']['nodes']['children'][0]['depths']
    assert restored == edited['model']['nodes']['children'][0]['depths']
    core.request('depth.set', False, target=4, depths=[1, 2, 3])
    core.request('depth.clear', target=5)
    assert core.request('depth.list', target=5)['depths'] is None
    core.request('undo')
    core.request('deformer.define_vertices', nodes=[5], vertices=[0, 0, 2, 0])
    assert core.request('depth.list', target=5)['depths'] == [9, 9]
    core.request('undo')
    blocked = core.request('snapshot')
    core.request('node.convert', False, nodes=[4], className='PathDeformer')
    assert core.request('snapshot') == blocked
    core.request('deformer.define_grid', nodes=[4], axisX=[-1, 0, 1], axisY=[-2, 0, 2])
    resampled = core.request('node.get', id=4)
    assert resampled['grid_axis_x'] == [-1, 0, 1]
    assert resampled['grid_axis_y'] == [-2, 0, 2]
    assert len(resampled['depths']) == 9
    assert resampled['depths'][0] == 0
    assert resampled['depths'][4] == 0.1875
    assert resampled['depths'][8] == 1
    assert resampled['depth-ops'][0]['index'] == 2
    assert resampled['depth-ops'][0]['amount'] == 0.4
    assert len(resampled['depth-op-base-depths']) == 9
    core.request('undo')
    restored = core.request('snapshot')
    assert restored['model'] == blocked['model']
    assert restored['canRedo'] is True
    assert core.request('node.get', id=4)['depths'] == [0, 0.25, -0.5, 1]
    saved = directory / 'depth-rig-saved.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reopened = other.request('snapshot')['model']
    assert reopened['nodes']['children'][2]['type'] == 'DepthRigRoot'
    assert reopened['nodes']['children'][0]['depths'] == [0, 0.25, -0.5, 1]
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    exported_bytes = bytes(other.request('export')['bytes'])
    exported_model, exported_tail = split_container(exported_bytes)
    assert exported_tail == tail
    assert exported_model['nodes']['children'][0]['depths'] == [0, 0.25, -0.5, 1]
    assert exported_model['nodes']['children'][1]['depths'] == [9, 9]
    assert all(child.get('type') not in ('DepthBone', 'DepthRigRoot') for child in exported_model['nodes']['children'])
    assert exported_model['nodes']['children'][2]['uuid'] == 8
    assert other.request('snapshot')['model']['nodes']['children'][2]['type'] == 'DepthRigRoot'
    fresh = core.request('new')
    assert fresh['model']['nodes'].get('children') in (None, [])
    created = core.request('node.create', className='GridDeformer', name='Live Grid')['created'][0]
    live = created['uuid']
    assert core.request('depth.list', target=live)['count'] == -1
    core.request('depth.set', nodes=[live], depths=[0, 0.5, -1, 0.25])
    core.request('depth.clear', id=live)
    assert 'depths' not in core.request('node.get', id=live)
    core.request('undo')
    assert core.request('depth.list', target=live)['count'] == 4
    core.request('depth.ops.add', target=live, index=-1, operation={
        'type': 'attached-point', 'index': 1, 'amount': 0.25,
    })
    listed_ops = core.request('depth.ops.list', target=live)
    assert listed_ops['count'] == 1 and listed_ops['operations'][0]['index'] == 1
    assert core.request('node.get', id=live)['depth-op-base-depths'] == [0, 0.5, -1, 0.25]
    core.request('depth.ops.apply', target=live)
    assert core.request('depth.list', target=live)['depths'] == [0, 0.75, -1, 0.25]
    core.request('undo')
    assert core.request('depth.list', target=live)['depths'] == [0, 0.5, -1, 0.25]
    assert core.request('depth.ops.list', target=live)['count'] == 1
    path = core.request('node.create', className='PathDeformer', name='Ops Path')['created'][0]['uuid']
    core.request('depth.ops.add', False, target=path, index=-1, operation={
        'type': 'attached-point', 'index': 0, 'amount': 1,
    })
    core.request('depth.ops.add', False, target=live, index=-1, operation={'type': 'nope'})
    core.request('depth.ops.apply', False, target=path)
    core.request('depth.import_psd', False, target=live)
    card = core.request('node.create', className='GridDeformer', name='Card')['created'][0]['uuid']
    core.request('deformer.define_grid', nodes=[card], axisX=[-1, 0], axisY=[-1, 0])
    psd_path = directory / 'card-depth.psd'
    psd_path.write_bytes(encode_minimal_psd(2, 2, [
        ('Card', bytes([255, 255, 255, 255]) + bytes(12)),
        ('Card', bytes([128]) * 16),
        ('Nope', bytes([255]) * 16),
    ]))
    before_import = core.request('snapshot')
    imported = core.request('depth.import_psd', path=str(psd_path), convolution='Nearest')
    assert imported['unmatchedLayers'] == 1 and imported['changedGrids'] == 1
    sampled = core.request('node.get', id=card)
    gray = float(__import__('struct').unpack('f', __import__('struct').pack('f', -1.0 + 2.0 * (128.0 / 255.0)))[0])
    assert sampled['depths'][0] == 1.0
    assert abs(sampled['depths'][1] - gray) < 1e-6
    assert sampled['depths'][1] == sampled['depths'][2] == sampled['depths'][3]
    assert 'depth-ops' not in sampled
    core.request('undo')
    assert core.request('snapshot')['model'] == before_import['model']
    assert 'depths' not in core.request('node.get', id=card)
    print('PASS Depth D0/D4: opaque rig round-trip, depths, depth-ops, grid resample, PSD import/undo, fail-closed')
    verify_psd_import_refreshes_rig(core, directory)


def deform_binding(parameter, node):
    return next(
        item for item in parameter['bindings']
        if item['param_name'] == 'deform' and item['node'] == node
    )


def verify_psd_import_refreshes_rig(core, directory):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='PsdRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Bone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    card = core.request('node.create', className='GridDeformer', name='Card')['created'][0]
    core.request('deformer.define_grid', nodes=[card['uuid']], axisX=[-1, 0], axisY=[-1, 0])
    hair = core.request(
        'node.create', className='PathDeformer', name='Hair', parents=[card['uuid']],
    )['created'][0]
    core.request('deformer.define_vertices', nodes=[hair['uuid']], vertices=[0, 0, 1, 0])
    sibling = core.request('node.create', className='GridDeformer', name='Other')['created'][0]
    core.request('deformer.define_grid', nodes=[sibling['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('depth.set', target=sibling['uuid'], depths=[9, 9, 9, 9])
    for target in (card['uuid'], hair['uuid'], sibling['uuid']):
        core.request('rig.add_source', root=rig['uuid'], target=target, bone=bone['uuid'])
    core.request(
        'rig.set_source_settings', root=rig['uuid'], target=card['uuid'], bone=bone['uuid'],
        settings={'depthScale': 2, 'depthOffset': 0.25},
    )
    parameter = core.request('parameter.create', name='Yaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[2, 0], rotation_y=1.5707963267948966,
    )
    core.request('depth.ops.add', target=card['uuid'], index=-1, operation={
        'type': 'attached-point', 'index': 0, 'amount': 0.2,
    })
    before = core.request('snapshot')
    psd_path = directory / 'rig-card-depth.psd'
    psd_path.write_bytes(encode_minimal_psd(2, 2, [
        ('Card', bytes([255, 255, 255, 255]) + bytes(12)),
    ]))
    imported = core.request('depth.import_psd', path=str(psd_path), convolution='Nearest')
    assert imported['changedGrids'] == 1
    sampled = core.request('node.get', id=card['uuid'])
    assert sampled['depths'][0] == 1.0
    assert 'depth-ops' not in sampled and 'depth-op-base-depths' not in sampled
    card_sources = core.request('rig.list_sources', root=rig['uuid'], target=card['uuid'])
    assert card_sources['sources'][0]['depthScale'] == 1
    assert card_sources['sources'][0]['depthOffset'] == 0
    assert 'depths' not in core.request('node.get', id=hair['uuid'])
    refreshed = core.request('parameter.get', id=parameter['uuid'])
    for node in (card['uuid'], hair['uuid']):
        binding = deform_binding(refreshed, node)
        assert all(binding['isSet'][index][0] is True for index in range(3))
    sibling_binding = deform_binding(refreshed, sibling['uuid'])
    assert sibling_binding['isSet'][0][0] is False and sibling_binding['isSet'][2][0] is True
    assert core.request('node.get', id=sibling['uuid'])['depths'] == [9, 9, 9, 9]
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']

    core.request('new')
    plain_rig = core.request('node.create', className='DepthRigRoot', name='PlainRig')['created'][0]
    plain_bone = core.request(
        'node.create', className='DepthBone', name='Bone', parents=[plain_rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    plain_card = core.request('node.create', className='GridDeformer', name='Card')['created'][0]
    core.request('deformer.define_grid', nodes=[plain_card['uuid']], axisX=[-1, 0], axisY=[-1, 0])
    core.request('rig.add_source', root=plain_rig['uuid'], target=plain_card['uuid'], bone=plain_bone['uuid'])
    idle = core.request('parameter.create', name='Idle', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request('depth.import_psd', path=str(psd_path), convolution='Nearest')
    assert core.request('parameter.get', id=idle['uuid'])['bindings'] == []
    assert core.request('node.get', id=plain_card['uuid'])['depths'][0] == 1.0

    heavy = {
        'nodes': {
            'uuid': 1, 'type': 'Node',
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [{
                'uuid': 2, 'type': 'DepthRigRoot',
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'bindings': [{'target': 4, 'targetKind': 'grid', 'sourceBoneUuids': [3]}],
                'children': [{
                    'uuid': 3, 'type': 'DepthBone', 'children': [],
                    'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                }],
            }, {
                'uuid': 4, 'name': 'Card', 'type': 'GridDeformer',
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'grid_axis_x': [float(index) for index in range(1000)],
                'grid_axis_y': [float(index) for index in range(1001)],
            }],
        },
        'param': [{
            'uuid': 9, 'is_vec2': False, 'min': [0, 0], 'max': [1, 0],
            'axis_points': [[0, 1], [0]],
            'bindings': [{
                'node': 3, 'param_name': 'transform.r.y',
                'values': [[0], [0.2]], 'isSet': [[True], [True]],
            }],
        }],
        'automation': [],
    }
    core.request('new', model=heavy)
    held = core.request('snapshot')
    core.request('depth.import_psd', False, path=str(psd_path), convolution='Nearest')
    assert core.request('snapshot') == held
    print('PASS PSD rig import: deform refresh, depth-only without a driver, max-vertex rollback')


def verify_pitch_yaw_z(core, other, directory, original, tail):
    core.request('open', path=str(original))
    baseline = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
    assert base64.b64decode(baseline['png'])[:8] == b'\x89PNG\r\n\x1a\n'
    created = core.request('node.create', className='Node', _suffix='', name='Pitch Node')
    node_id = created['created'][0]['uuid']
    part_id = 2
    core.request('node.transform', id=part_id, translation=[0, 0, 3], rotation=[0, 0, 0], scale=[1, 1])
    core.request('node.transform', id=node_id, translation=[0, 0, 3], rotation=[0, 0, 0], scale=[1, 1])
    param = core.request('parameter.create', name='PitchYawZ', dimensions=1, min=[-1, 0], max=[1, 1])
    uid = param['uuid']
    for index, tz in ((0, -2.0), (1, 1.0), (2, 4.0)):
        core.request('binding.set_trs', parameter=uid, nodes=[part_id, node_id], key=[index, 0], translation_z=tz)
    low = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [-1, 0]})
    high = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [1, 0]})
    assert low['png'] == high['png']
    z_only = core.request('evaluate', values={str(uid): [0.5, 0]})
    assert abs(find_child(z_only['nodes'], part_id)['transform']['trans'][2] - 5.5) < 1e-6
    for index, rx, ry in ((0, -0.25, 0.5), (1, 0.125, 0.0), (2, 0.5, -0.5)):
        core.request('binding.set_trs', parameter=uid, nodes=[part_id, node_id], key=[index, 0],
                     rotation_x=rx, rotation_y=ry)
    before = core.request('snapshot')
    core.request('binding.set_trs', False, parameter=uid, nodes=[part_id], key=[9, 0], rotation_x=9)
    assert core.request('snapshot') == before
    evaluated = core.request('evaluate', values={str(uid): [0.5, 0]})
    for node in (find_child(evaluated['nodes'], part_id), find_child(evaluated['nodes'], node_id)):
        transform = node['transform']
        assert abs(transform['trans'][2] - 5.5) < 1e-6, transform
        assert abs(transform['rot'][0] - 0.3125) < 1e-6, transform
        assert abs(transform['rot'][1] - (-0.25)) < 1e-6, transform
        assert abs(transform['rot'][2]) < 1e-6
    assert core.request('snapshot')['model']['nodes']['children'][0]['transform']['trans'][2] == 3
    pitched = core.request('snapshot')
    pitched_png = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [0.5, 0]})
    assert pitched_png['png'] != high['png']
    assert core.request('snapshot') == pitched
    for _ in range(3):
        core.request('undo')
    restored = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [1, 0]})
    assert restored['png'] == high['png']
    for _ in range(3):
        core.request('redo')
    saved = directory / 'pitch-yaw-z.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reloaded = other.request('evaluate', values={str(uid): [0.5, 0]})
    assert abs(find_child(reloaded['nodes'], part_id)['transform']['rot'][0] - 0.3125) < 1e-6
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    bone = {
        'nodes': {
            'uuid': 1, 'type': 'Node', 'enabled': True,
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [{
                'uuid': 4, 'type': 'DepthBone', 'enabled': False, 'name': 'Bone',
                'transform': {'trans': [1, 2, 3], 'rot': [0.25, -0.5, 0.125], 'scale': [1, 1]},
                'depths': [0.5, -1], 'children': [],
            }],
        },
        'param': [{
            'uuid': 8, 'is_vec2': False, 'min': [0, 0], 'max': [1, 1], 'defaults': [0, 0],
            'axis_points': [[0, 1], [0]],
            'bindings': [{
                'node': 4, 'param_name': 'transform.t.z', 'interpolate_mode': 'Linear',
                'values': [[1], [5]], 'isSet': [[True], [True]],
            }, {
                'node': 4, 'param_name': 'deform', 'values': [], 'isSet': [[False]],
            }],
        }],
    }
    other.request('new', model=bone)
    other.request('binding.set_trs', parameter=8, nodes=[4], key=[1, 0], rotation_x=0.125)
    posed = other.request('evaluate', values={'8': [0.5, 0]})['nodes']['children'][0]
    assert abs(posed['transform']['trans'][2] - 6) < 1e-6, posed
    assert abs(posed['transform']['rot'][0] - 0.3125) < 1e-6, posed
    assert posed['depths'] == [0.5, -1]
    before_bone = other.request('snapshot')
    other.request('node.transform', id=4, component='rotationX', value=1)
    assert other.request('node.get', id=4)['transform']['rot'][0] == 1
    other.request('undo')
    assert other.request('snapshot')['model'] == before_bone['model']
    other.request('node.create', False, className='DepthBone', _suffix='')
    assert other.request('snapshot')['model'] == before_bone['model']
    nonlinear = json.loads(json.dumps(bone))
    nonlinear['param'][0]['bindings'][0]['interpolate_mode'] = 'Nearest'
    other.request('new', model=nonlinear)
    before_nonlinear = other.request('snapshot')
    other.request('evaluate', False)
    assert other.request('snapshot') == before_nonlinear
    deform = json.loads(json.dumps(bone))
    deform['param'][0]['bindings'] = [{
        'node': 4, 'param_name': 'deform', 'values': [[[1, 0]]], 'isSet': [[True]],
    }]
    other.request('new', model=deform)
    before_deform = other.request('snapshot')
    posed_deform = other.request('evaluate')['nodes']['children'][0]['transform']
    assert posed_deform['trans'] == [1, 2, 3]
    assert posed_deform['rot'] == [0.25, -0.5, 0.125]
    assert other.request('snapshot') == before_deform
    print('PASS Pitch/yaw/Z: linear evaluation, undo/redo, save/reload, Part X/Y projects, translation Z ignored')
    verify_part_lock_to_root(core)


def verify_part_lock_to_root(core):
    def model(root_shift, parent_shift, parent_lock, part_shift, part_lock):
        return {
            'nodes': {
                'uuid': 1, 'type': 'Node', 'enabled': True,
                'transform': {'trans': root_shift, 'rot': [0, 0, 0], 'scale': [1, 1]},
                'children': [{
                    'uuid': 4, 'type': 'Node', 'enabled': True, 'lockToRoot': parent_lock,
                    'transform': {'trans': parent_shift, 'rot': [0, 0, 0], 'scale': [1, 1]},
                    'children': [{
                        'uuid': 2, 'type': 'Part', 'enabled': True, 'lockToRoot': part_lock,
                        'transform': {'trans': part_shift, 'rot': [0, 0, 0], 'scale': [1, 1]},
                        'mesh': {
                            'verts': [0, 0, 0, 8, 8, 0, 8, 8],
                            'uvs': [0, 0, 0, 1, 1, 0, 1, 1],
                            'indices': [0, 1, 2, 0, 2, 3],
                            'origin': [0, 0],
                        },
                    }],
                }],
            },
            'param': [], 'automation': [],
        }
    core.request('new', model=model([100, 0, 0], [50, 0, 0], False, [0, 0, 0], True))
    held = core.request('snapshot')
    locked = core.request('bounds')['bounds']
    assert core.request('snapshot') == held
    core.request('new', model=model([100, 0, 0], [0, 0, 0], False, [0, 0, 0], True))
    assert core.request('bounds')['bounds'] == locked
    core.request('new', model=model([100, 0, 0], [50, 0, 0], False, [0, 0, 0], False))
    assert abs(core.request('bounds')['bounds'][0] - locked[0]) > 20
    core.request('new', model=model([0, 0, 0], [0, 0, 0], False, [0, 0, 0], True))
    assert abs(core.request('bounds')['bounds'][0] - locked[0]) > 20
    core.request('new', model=model([100, 0, 0], [50, 0, 0], True, [0, 0, 0], False))
    inherited = core.request('bounds')['bounds']
    core.request('new', model=model([100, 0, 0], [50, 0, 0], True, [0, 0, 0], True))
    assert core.request('bounds')['bounds'] == locked
    assert abs(inherited[0] - locked[0]) > 10
    pinned = model([0, 0, 0], [0, 0, 0], False, [0, 0, 0], False)
    pinned['nodes']['children'][0]['children'][0]['pinToMesh'] = True
    core.request('new', model=pinned)
    before = core.request('snapshot')
    core.request('render', False, width=8, height=8, bounds=[0, 0, 8, 8])
    assert core.request('snapshot') == before
    print('PASS Part lockToRoot: root composition, locked parent, pinToMesh stays closed')
    verify_pin_to_mesh(core)


def verify_pin_to_mesh(core):
    def host(child_translation, pin, parent_type='Part'):
        return {
            'nodes': {
                'uuid': 1, 'type': 'Node', 'enabled': True,
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'children': [{
                    'uuid': 4, 'type': parent_type, 'enabled': True,
                    'textures': [2 ** 32 - 1, 2 ** 32 - 1, 2 ** 32 - 1],
                    'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                    'mesh': {
                        'verts': [0, 0, 4, 0, 0, 4],
                        'uvs': [0, 0, 1, 0, 0, 1],
                        'indices': [0, 1, 2],
                        'origin': [0, 0],
                    },
                    'children': [{
                        'uuid': 5, 'type': 'Part', 'enabled': True, 'pinToMesh': pin,
                        'textures': [2 ** 32 - 1, 2 ** 32 - 1, 2 ** 32 - 1],
                        'transform': {'trans': child_translation, 'rot': [0, 0, 0], 'scale': [1, 1]},
                        'mesh': {
                            'verts': [0, 0, 10, 0, 10, 0.2, 0, 0.2],
                            'uvs': [0, 0, 1, 0, 1, 1, 0, 1],
                            'indices': [0, 1, 2, 0, 2, 3],
                            'origin': [0, 0],
                        },
                    }],
                }],
            },
            'param': [{
                'uuid': 9, 'is_vec2': False,
                'min': [0, 0], 'max': [1, 0], 'defaults': [1, 0],
                'axis_points': [[0, 1], [0]],
                'bindings': [{
                    'node': 4, 'param_name': 'deform',
                    'values': [[[[0, 0], [0, 0], [0, 0]]], [[[0, 0], [8, 0], [0, 0]]]],
                    'isSet': [[True], [True]],
                }],
            }],
            'automation': [],
        }
    core.request('new', model=host([1, 1, 0], True))
    before = core.request('snapshot')
    pinned = core.request('bounds')['bounds']
    assert abs(pinned[2] - 14.3) < 1e-9, pinned
    assert core.request('snapshot') == before
    core.request('render', width=4, height=4, bounds=[-2, -2, 16, 8])
    assert core.request('snapshot') == before
    core.request('new', model=host([1, 1, 0], False))
    assert abs(core.request('bounds')['bounds'][2] - 13.2) < 1e-9
    core.request('new', model=host([10, 10, 0], True))
    outside = core.request('bounds')['bounds']
    core.request('new', model=host([10, 10, 0], False))
    assert core.request('bounds')['bounds'] == outside
    core.request('new', model=host([1, 1, 0], True, 'Node'))
    held = core.request('snapshot')
    core.request('bounds', False)
    core.request('render', False, width=4, height=4, bounds=[-2, -2, 16, 8])
    assert core.request('snapshot') == held
    print('PASS Pin to mesh: deformed triangle moves the child, a miss and a non-Part stay closed')
    verify_part_weld(core)


def verify_part_weld(core):
    def part(uuid):
        return {
            'uuid': uuid, 'type': 'Part', 'enabled': True, 'zsort': 0,
            'textures': [2 ** 32 - 1, 2 ** 32 - 1, 2 ** 32 - 1],
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'mesh': {
                'verts': [0, 0, 1, 0, 0, 1],
                'uvs': [0, 0, 1, 0, 0, 1],
                'indices': [0, 1, 2],
                'origin': [0, 0],
            },
        }
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'type': 'Node', 'enabled': True,
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [part(4), part(5)],
        },
        'param': [{
            'uuid': 9, 'is_vec2': False,
            'min': [0, 0], 'max': [1, 0], 'defaults': [1, 0],
            'axis_points': [[0, 1], [0]],
            'bindings': [{
                'node': 5, 'param_name': 'deform',
                'values': [[
                    [[0, 0], [0, 0], [0, 0]],
                ], [
                    [[4, 0], [4, 0], [4, 0]],
                ]],
                'isSet': [[True], [True]],
            }],
        }],
        'automation': [],
    })
    before = core.request('snapshot')
    core.request('part.set_weld', part=4, target=5, indices=[0, 1, 2], weight=0.25)
    assert core.request('node.get', id=4)['weldedLinks'][0]['weight'] == 0.25
    assert core.request('node.get', id=4)['weldedLinks'][0]['indices'] == [0, 1, 2]
    assert core.request('node.get', id=5)['weldedLinks'][0]['weight'] == 0.75
    assert core.request('node.get', id=5)['weldedLinks'][0]['indices'] == [0, 1, 2]
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    core.request('part.set_weld', part=4, target=5, indices=[0, 1, 2], weight=1)
    held = core.request('snapshot')
    pulled = core.request('bounds')['bounds']
    assert abs(pulled[2] - 1.1) < 1e-9, pulled
    assert core.request('snapshot') == held
    core.request('part.clear_weld', part=4, target=5)
    assert 'weldedLinks' not in core.request('node.get', id=4)
    assert 'weldedLinks' not in core.request('node.get', id=5)
    core.request('undo')
    assert core.request('node.get', id=5)['weldedLinks'][0]['weight'] == 0
    one = core.request('snapshot')['model']
    one['nodes']['children'][1].pop('weldedLinks')
    core.request('new', model=one)
    blocked = core.request('snapshot')
    assert abs(core.request('bounds')['bounds'][2] - 5.5) < 1e-9
    assert core.request('snapshot') == blocked
    core.request('part.set_weld', False, part=4, target=4, indices=[0, 1, 2], weight=1)
    core.request('part.set_weld', False, part=4, target=6, indices=[0, 1, 2], weight=1)
    assert core.request('snapshot') == blocked
    mixed = json.loads(json.dumps(part(4)))
    mixed['vertices'] = []
    mixed['uvs'] = []
    mixed['indices'] = []
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'type': 'Node', 'enabled': True,
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [mixed, part(5)],
        },
        'param': [], 'automation': [],
    })
    held = core.request('snapshot')
    core.request('part.set_weld', False, part=4, target=5, indices=[], weight=1)
    assert core.request('snapshot') == held
    core.request('part.set_weld', part=4, target=5, indices=[0, -1, -1], weight=1)
    assert core.request('node.get', id=4)['weldedLinks'][0]['indices'] == [0, -1, -1]
    linked = core.request('snapshot')
    core.request('node.delete', nodes=[5])
    assert 'weldedLinks' not in core.request('node.get', id=4)
    core.request('undo')
    assert core.request('snapshot')['model'] == linked['model']
    core.request('node.copy', nodes=[4, 5])
    created = core.request('node.paste', parent=1)['created']
    created_ids = {node['uuid'] for node in created}
    assert len(created_ids) == 2
    for node in created:
        target = node['weldedLinks'][0]['targetUUID']
        assert target in created_ids and target not in (4, 5)
        assert node['weldedLinks'][0]['indices'] == [0, -1, -1]
    assert core.request('node.get', id=4)['weldedLinks'][0]['targetUUID'] == 5
    third = part(40)
    pair = core.request('snapshot')['model']
    pair['nodes']['children'].append(third)
    core.request('new', model=pair)
    core.request('part.set_weld', part=4, target=5, indices=[0, -1, -1], weight=1)
    held = core.request('snapshot')
    core.request('part.set_weld', False, part=4, target=40, indices=[0, -1, -1], weight=1)
    assert core.request('snapshot') == held
    core.request('part.set_weld', False, part=4, target=5, indices=[0, 1, 2], weight=2)
    assert core.request('snapshot') == held
    print('PASS Part weld: reciprocal links, weight 1 pulls the other part, one-sided stays put')


def verify_rig_nodes(core, other, directory, original, tail):
    core.request('open', path=str(original))
    baseline = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
    rig = core.request('node.create', className='DepthRigRoot', name='DepthRig')['created'][0]
    assert rig['type'] == 'DepthRigRoot' and rig['name'] == 'DepthRig'
    bone = core.request(
        'node.create', className='DepthBone', parents=[rig['uuid']], boneId='Spine',
        restHead=[4, 5, 1], restTail=[4, 15, 1], restRoll=0.25,
    )['created'][0]
    child = core.request(
        'node.create', className='DepthBone', parents=[bone['uuid']], boneId='Chest',
        restHead=[4, 9, 1], restTail=[4, 19, 1],
    )['created'][0]
    spine = find_child(core.request('snapshot')['model']['nodes'], bone['uuid'])
    assert spine['boneId'] == 'Spine'
    assert spine['restHead'] == [4, 5, 1]
    assert spine['hingeAxis'] == [0, 0, 1]
    assert spine['allowParentToTargets'] is True
    assert 'bindings' not in find_child(core.request('snapshot')['model']['nodes'], rig['uuid'])
    assert find_child(core.request('snapshot')['model']['nodes'], child['uuid'])['transform']['trans'] == [0, 4, 0]
    before = core.request('snapshot')
    core.request('node.create', False, className='DepthBone', boneId='Loose', restHead=[0, 0, 0], restTail=[0, 1, 0])
    core.request('node.create', False, className='DepthRigRoot', name='Bound', bindings=[])
    core.request('node.insert', False, className='DepthBone', nodes=[2])
    assert core.request('snapshot') == before
    with_rig = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
    assert with_rig['png'] == baseline['png']
    for _ in range(3):
        core.request('undo')
    assert find_child(core.request('snapshot')['model']['nodes'], rig['uuid']) is None
    for _ in range(3):
        core.request('redo')
    saved = directory / 'rig-nodes.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    assert find_child(other.request('snapshot')['model']['nodes'], bone['uuid'])['restTail'] == [4, 15, 1]
    exported = other.request('export')
    raw = bytes(exported['bytes'])
    model, saved_tail = split_container(raw)
    assert saved_tail == tail
    assert find_child(model['nodes'], rig['uuid']) is None
    assert find_child(model['nodes'], bone['uuid']) is None
    assert find_child(other.request('snapshot')['model']['nodes'], bone['uuid'])['boneId'] == 'Spine'
    print('PASS Rig nodes: create, undo/redo, save keeps, export drops, Part PNG unchanged, fail-closed')
    verify_delete_rig_root(core)
    verify_standard_skeleton(core, other, directory)


def verify_delete_rig_root(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='DeleteRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Bone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    core.request(
        'rig.set_source_settings', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'],
        settings={'depthOffset': 0.25},
    )
    core.request(
        'rig.set_influence_rule', root=rig['uuid'], target=grid['uuid'],
        rule={'multipliersByBoneUuid': {str(bone['uuid']): 0.5}},
    )
    parameter = core.request('parameter.create', name='Yaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[2, 0], rotation_y=0.4,
    )
    before = core.request('snapshot')
    deform = next(item for item in before['model']['param'][0]['bindings'] if item['param_name'] == 'deform')
    core.request('node.delete', nodes=[rig['uuid']])
    assert core.request('node.get', False, id=rig['uuid']) is None
    assert core.request('node.get', False, id=bone['uuid']) is None
    assert core.request('node.get', id=grid['uuid'])['type'] == 'GridDeformer'
    bindings = core.request('parameter.get', id=parameter['uuid'])['bindings']
    assert all(item['node'] != bone['uuid'] for item in bindings)
    assert next(item for item in bindings if item['param_name'] == 'deform') == deform
    core.request('node.delete', False, nodes=[1])
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    sources = core.request('rig.list_sources', root=rig['uuid'], target=grid['uuid'])
    assert sources['sources'][0]['depthOffset'] == 0.25
    rule = core.request('rig.get_influence_rule', root=rig['uuid'], target=grid['uuid'])
    assert rule['multipliersByBoneUuid'][str(bone['uuid'])] == 0.5
    core.request('redo')
    assert core.request('node.get', False, id=rig['uuid']) is None
    core.request('undo')
    core.request('node.cut', nodes=[rig['uuid']])
    assert core.request('node.get', False, id=bone['uuid']) is None
    kept = core.request('parameter.get', id=parameter['uuid'])['bindings']
    assert next(item for item in kept if item['param_name'] == 'deform') == deform
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    print('PASS Rig delete: external deform stays, undo restores offset and multipliers, cut, puppet root stays')


def f32(value):
    return struct.unpack('f', struct.pack('f', float(value)))[0]


def verify_standard_skeleton(core, other, directory):
    core.request('new')
    before = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
    rig = core.request('node.create', className='DepthRigRoot', name='DepthRig')['created'][0]
    keep = core.request(
        'node.create', className='DepthBone', parents=[rig['uuid']], boneId='Keep',
        restHead=[0, 0, 0], restTail=[0, 1, 0],
    )['created'][0]
    core.request('node.create', className='Node', name='Head', parents=[1])
    revision = core.request('snapshot')['revision']
    core.request('rig.add_skeleton', False, root=keep['uuid'])
    assert core.request('snapshot')['revision'] == revision
    added = core.request('rig.add_skeleton', root=rig['uuid'], scale=0)
    assert len(added['items']) == 20
    pelvis = next(item for item in added['items'] if item['boneId'] == 'Pelvis')
    expected_y = f32(f32(-300.0) + f32(f32(0.58) * f32(600.0)))
    assert abs(pelvis['restHead'][1] - expected_y) < 1e-4
    assert pelvis['parent'] == rig['uuid']
    foot = next(item for item in added['items'] if item['boneId'] == 'Foot.L')
    assert core.request('node.get', id=foot['uuid'])['lockToRoot'] is True
    after = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
    assert after['png'] == before['png']
    assert any(item['boneId'] == 'Head' for item in added['items'])
    core.request('node.create', False, className='Node', name='Pelvis', parents=[1])
    core.request('undo')
    assert core.request('node.get', id=keep['uuid'])['boneId'] == 'Keep'
    assert len(core.request('rig.list_bones', root=rig['uuid'])['items']) == 1
    core.request('redo')
    assert len(core.request('rig.list_bones', root=rig['uuid'])['items']) == 20
    saved = directory / 'skeleton.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    assert other.request('node.get', id=foot['uuid'])['lockToRoot'] is True
    exported = other.request('export')
    model, _tail = split_container(bytes(exported['bytes']))
    assert find_child(model['nodes'], rig['uuid']) is None
    assert find_child(other.request('snapshot')['model']['nodes'], foot['uuid'])['boneId'] == 'Foot.L'
    core.request('undo')
    held = core.request('snapshot')
    core.request('rig.add_skeleton', False, root=rig['uuid'], scale=1e39)
    assert core.request('snapshot') == held
    tiny = core.request('rig.add_skeleton', root=rig['uuid'], scale=1e-50)
    tiny_pelvis = next(item for item in tiny['items'] if item['boneId'] == 'Pelvis')
    assert abs(tiny_pelvis['restHead'][1] - expected_y) < 1e-4
    print('PASS Standard skeleton: D placement, append, undo, save keeps, export drops, fail-closed')
    verify_bone_bindings(core, other, directory)


def verify_bone_bindings(core, other, directory):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='DepthRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', parents=[rig['uuid']], boneId='Spine',
        restHead=[0, 0, 0], restTail=[0, 1, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Warp')['created'][0]
    added = core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    assert added['sourceBoneUuids'] == [bone['uuid']]
    assert 'sources' not in core.request('node.get', id=grid['uuid'])
    held = core.request('snapshot')
    core.request('rig.set_source_settings', False, root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'],
                 settings={'weights': [1, 0]})
    core.request('rig.set_source_settings', False, root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'],
                 settings='{"weights":[1,0]}')
    core.request('rig.set_influence_rule', False, root=rig['uuid'], target=grid['uuid'],
                 rule={'maxInfluences': 2 ** 32})
    assert core.request('snapshot') == held
    core.request('rig.set_influence_rule', root=rig['uuid'], target=grid['uuid'],
                 rule={'maxInfluences': 0, 'falloff': 'linear'})
    assert core.request('rig.get_influence_rule', root=rig['uuid'], target=grid['uuid'])['maxInfluences'] == 1
    preview_before = core.request('snapshot')
    preview = core.request('rig.preview_influence', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    assert preview['sourceBoneUuids'] == [bone['uuid']] and 'weights' not in preview
    assert core.request('snapshot') == preview_before
    core.request('node.delete', nodes=[grid['uuid']])
    assert 'bindings' not in core.request('node.get', id=rig['uuid'])
    core.request('undo')
    assert core.request('rig.list_sources', root=rig['uuid'], target=grid['uuid'])['sourceBoneUuids'] == [bone['uuid']]
    core.request('rig.remove_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    assert 'bindings' not in core.request('node.get', id=rig['uuid'])
    core.request('undo')
    saved = directory / 'bone-bindings.inx'
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    assert other.request('rig.list_sources', root=rig['uuid'], target=grid['uuid'])['sources'][0]['uuid'] == bone['uuid']
    exported, _tail = split_container(bytes(other.request('export')['bytes']))
    assert find_child(exported['nodes'], rig['uuid']) is None
    print('PASS Bone bindings: add, rule, delete rewrites the row, undo, save keeps, export drops')
    verify_bone_deform(core, other, directory)


def verify_bone_deform(core, other, directory):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='DeformRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', parents=[rig['uuid']], boneId='Spine',
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Warp')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='Yaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request('binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']], key=[1, 0], rotation_y=1.5707963267948966)
    posed = core.request('parameter.get', id=parameter['uuid'])
    posed_deform = next(item for item in posed['bindings'] if item['param_name'] == 'deform')
    assert posed_deform['isSet'][1][0] is True
    assert posed_deform['isSet'][0][0] is False and posed_deform['isSet'][2][0] is False
    held = core.request('snapshot')
    preview = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    offsets = preview['targets'][0]['offsets']
    assert len(offsets) == 4
    assert abs(offsets[1][0] + 1) < 1e-4 and abs(offsets[1][1]) < 1e-4
    after = core.request('snapshot')
    assert after['canUndo'] == held['canUndo']
    assert after['model'] == held['model']
    assert after['revision'] == held['revision']
    again = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    assert again['targets'][0]['offsets'] == offsets
    assert core.request('snapshot')['canUndo'] == held['canUndo']
    core.request('rig.apply_deform', False, root=rig['uuid'])
    assert core.request('snapshot')['model'] == held['model']
    applied = core.request('rig.apply_deform', root=rig['uuid'], parameter=parameter['uuid'])
    assert len(applied['targets'][0]['offsets']) == 4
    stored = core.request('parameter.get', id=parameter['uuid'])
    deform = next(item for item in stored['bindings'] if item['param_name'] == 'deform')
    assert deform['isSet'][1][0] is True
    assert len(deform['values'][1][0]) == 4
    core.request('undo')
    undone = core.request('parameter.get', id=parameter['uuid'])
    assert all(item['param_name'] != 'deform' for item in undone['bindings'])
    assert all(item['param_name'] != 'transform.r.y' for item in undone['bindings'])
    core.request('redo')
    core.request('node.transform', id=grid['uuid'], translation=[10, 0, 0])
    moved = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    assert abs(moved['targets'][0]['offsets'][0][0] + 10) < 1e-4
    parent = core.request('node.create', className='GridDeformer', name='Parent')['created'][0]
    core.request('deformer.define_grid', nodes=[parent['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('node.move', nodes=[grid['uuid']], parent=parent['uuid'])
    core.request('node.transform', id=grid['uuid'], translation=[0, 0, 0])
    core.request('rig.add_source', root=rig['uuid'], target=parent['uuid'], bone=bone['uuid'])
    nested = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    child = next(item for item in nested['targets'] if item['target'] == grid['uuid'])
    assert all(abs(pair[0]) < 1e-4 and abs(pair[1]) < 1e-4 for pair in child['offsets'])
    camera = core.request('node.create', className='Camera', name='Cam', parents=[parent['uuid']])['created'][0]
    core.request('node.move', nodes=[grid['uuid']], parent=camera['uuid'])
    core.request('node.transform', id=camera['uuid'], translation=[1, 0, 0])
    core.request('node.transform', id=grid['uuid'], translation=[0, 0, 0])
    shifted = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    shifted_child = next(item for item in shifted['targets'] if item['target'] == grid['uuid'])
    assert any(abs(pair[0]) > 1e-3 or abs(pair[1]) > 1e-3 for pair in shifted_child['offsets'])
    held_camera = core.request('snapshot')
    core.request('node.transform', id=camera['uuid'], translation=[2, 0, 0])
    assert core.request('snapshot')['revision'] == held_camera['revision'] + 1
    core.request('undo')
    assert core.request('snapshot')['model'] == held_camera['model']
    composite = core.request('node.create', className='Composite', name='Comp', parents=[parent['uuid']])['created'][0]
    core.request('node.move', nodes=[grid['uuid']], parent=composite['uuid'])
    core.request('node.transform', id=grid['uuid'], translation=[0, 0, 0])
    passed = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    passed_child = next(item for item in passed['targets'] if item['target'] == grid['uuid'])
    assert all(abs(pair[0]) < 1e-4 and abs(pair[1]) < 1e-4 for pair in passed_child['offsets'])
    after_nested = core.request('snapshot')
    core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    assert core.request('snapshot')['canUndo'] == after_nested['canUndo']
    assert core.request('snapshot')['model'] == after_nested['model']
    saved = directory / 'bone-deform.inx'
    core.request('rig.apply_deform', root=rig['uuid'], parameter=parameter['uuid'])
    core.request('save', path=str(saved))
    other.request('open', path=str(saved))
    reloaded = other.request('parameter.get', id=parameter['uuid'])
    assert any(item['param_name'] == 'deform' for item in reloaded['bindings'])
    print('PASS Bone deform: preview stays outside history, apply writes one key, undo and reload, Camera origin and Composite pass-through')
    verify_deform_raster(core)
    verify_standard_parameters(core)


def verify_deform_raster(core):
    part = {
        'uuid': 2, 'type': 'Part', 'enabled': True,
        'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
        'mesh': {
            'verts': [0, 0, 0, 8, 8, 0, 8, 8],
            'uvs': [0, 0, 0, 1, 1, 0, 1, 1],
            'indices': [],
            'origin': [0, 0],
        },
        'children': [],
    }
    root = {
        'uuid': 1, 'type': 'Node', 'enabled': True,
        'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
        'children': [part],
    }
    zeros = [[0, 0]] * 4
    shifted = [[4, 0]] * 4
    core.request('new', model={
        'nodes': root,
        'automation': [],
        'param': [{
            'uuid': 9, 'is_vec2': False,
            'min': [0, 0], 'max': [1, 0], 'defaults': [0, 0],
            'axis_points': [[0, 1], [0]],
            'bindings': [{
                'node': 2, 'param_name': 'deform', 'interpolate_mode': 'Linear',
                'values': [[zeros], [shifted]],
                'isSet': [[True], [True]],
            }],
        }],
    })
    held = core.request('snapshot')
    core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={'9': [0.5, 0]})
    after = core.request('snapshot')
    assert after['model'] == held['model']
    assert after['revision'] == held['revision']
    assert after['canUndo'] == held['canUndo']
    assert after['model']['nodes']['children'][0]['mesh']['verts'] == [0, 0, 0, 8, 8, 0, 8, 8]
    stepped = core.request('snapshot')['model']
    stepped['param'][0]['bindings'][0]['interpolate_mode'] = 'Stepped'
    core.request('new', model=stepped)
    blocked = core.request('snapshot')
    core.request('render', False, width=8, height=8, bounds=[0, 0, 8, 8], values={'9': [0.5, 0]})
    assert core.request('snapshot') == blocked
    print('PASS Deform raster: linear draw leaves history alone, stepped interpolation fails closed')


def verify_standard_parameters(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='ParamRig')['created'][0]
    held = core.request('snapshot')
    core.request('rig.add_standard_parameters', False, root=rig['uuid'])
    assert core.request('snapshot') == held
    core.request('rig.add_skeleton', root=rig['uuid'], scale=1)
    created = core.request('rig.add_standard_parameters', root=rig['uuid'])
    face = next(item for item in created if item['name'] == 'Face::Yaw-Pitch')
    assert face['axis_points'] == [[0, 0.25, 0.5, 0.75, 1], [0, 0.25, 0.5, 0.75, 1]]
    yaw = next(item for item in face['bindings'] if item['param_name'] == 'transform.r.y')
    assert abs(yaw['values'][0][2] + 0.5235988) < 1e-6
    roll = next(item for item in created if item['name'] == 'Face::Roll')
    neck = next(item for item in roll['bindings'] if item['param_name'] == 'transform.r.z')
    assert abs(neck['values'][0][0] + 0.41800633) < 1e-6
    assert abs(neck['values'][4][0] - 0.41887903) < 1e-6
    before = core.request('snapshot')
    core.request('rig.add_standard_parameters', False, root=rig['uuid'])
    assert core.request('snapshot') == before
    core.request('undo')
    assert core.request('parameters') == []
    print('PASS Standard parameters: D axes and binding rows, second call is unchanged, undo')
    verify_fit_z(core)


def verify_fit_z(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='FitRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='FitBone', parents=[rig['uuid']],
        restHead=[-10, -10, 0], restTail=[-10, 90, 0],
    )['created'][0]
    sibling = core.request(
        'node.create', className='DepthBone', name='Sibling', parents=[rig['uuid']],
        restHead=[10, -10, 0], restTail=[10, 90, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='FitGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[-10, 10], axisY=[-10, 10])
    held = core.request('snapshot')
    core.request('rig.fit_root_z', False, root=rig['uuid'])
    assert core.request('snapshot') == held
    core.request('depth.set', target=grid['uuid'], depths=[0.5, 0.5, 0.5, 0.5])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    before = core.request('snapshot')
    core.request('rig.fit_bone_z', False, bone=grid['uuid'])
    assert core.request('snapshot') == before
    fitted = core.request('rig.fit_root_z', root=rig['uuid'])
    assert len(fitted['changed']) == 2
    def f32(value):
        return struct.unpack('f', struct.pack('f', value))[0]
    scale = max(1.0, f32(f32(20.0) * f32(f32(0.42) / f32(2.9))))
    expected = 0.5 * scale
    bone_z = core.request('node.get', id=bone['uuid'])['transform']['trans'][2]
    sibling_z = core.request('node.get', id=sibling['uuid'])['transform']['trans'][2]
    assert abs(bone_z - expected) < 1e-4 and abs(sibling_z - expected) < 1e-4
    assert core.request('node.get', id=grid['uuid'])['depths'] == [0.5, 0.5, 0.5, 0.5]
    assert core.request('parameters') == []
    aligned = core.request('snapshot')
    core.request('rig.fit_root_z', root=rig['uuid'])
    assert core.request('snapshot') == aligned
    core.request('undo')
    assert abs(core.request('node.get', id=bone['uuid'])['transform']['trans'][2]) < 1e-9
    assert abs(core.request('node.get', id=sibling['uuid'])['transform']['trans'][2]) < 1e-9
    print('PASS Fit Z: D scaled depth, fallback sibling, second call unchanged, one undo')
    verify_dirty_queue(core)


def verify_dirty_queue(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='DirtyRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='DirtyBone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='DirtyGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('depth.set', target=grid['uuid'], depths=[0, 0, 0, 0])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='DirtyYaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[2, 0], rotation_y=1.5707963267948966,
    )
    posed = core.request('parameter.get', id=parameter['uuid'])
    deform = next(item for item in posed['bindings'] if item['param_name'] == 'deform')
    assert deform['isSet'][2][0] is True
    assert deform['isSet'][0][0] is False and deform['isSet'][1][0] is False
    posed_model = core.request('snapshot')['model']
    core.request('depth.set', target=grid['uuid'], depths=[1, 1, 1, 1])
    refreshed = core.request('parameter.get', id=parameter['uuid'])
    deform = next(item for item in refreshed['bindings'] if item['param_name'] == 'deform')
    assert all(deform['isSet'][index][0] is True for index in range(3))
    assert deform['values'][0][0] != deform['values'][2][0]
    core.request('undo')
    restored = core.request('snapshot')
    assert restored['model'] == posed_model
    assert core.request('node.get', id=grid['uuid'])['depths'] == [0, 0, 0, 0]
    core.request('value.set', id=parameter['uuid'], value=[1, 0])
    after = core.request('snapshot')
    assert after['model'] == restored['model']
    assert after['revision'] == restored['revision']
    assert after['canUndo'] == restored['canUndo']
    print('PASS Dirty queue: one key on pose, every key on depths, one undo, value.set stays outside history')
    verify_key_budget(core)


def verify_key_budget(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='BudgetRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='BudgetBone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='BudgetGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request(
        'parameter.create', name='BudgetPose', dimensions=2, min=[0, 0], max=[1, 1],
    )
    core.request('value.set', id=parameter['uuid'], value=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[1, 1], rotation_y=1.5707963267948966,
    )
    core.request('depth.set', target=grid['uuid'], depths=[1, 1, 1, 1])
    stored = core.request('parameter.get', id=parameter['uuid'])
    deform = next(item for item in stored['bindings'] if item['param_name'] == 'deform')
    assert all(deform['isSet'][x][y] is True for x in range(2) for y in range(2))
    assert deform['values'][0][0] != deform['values'][1][1]
    preview = core.request('snapshot')['previewDeformation'][str(grid['uuid'])]
    assert preview == deform['values'][1][1]
    held = core.request('snapshot')
    xs = [float(index) for index in range(1000)]
    ys = [float(index) for index in range(1001)]
    core.request('deformer.define_grid', False, nodes=[grid['uuid']], axisX=xs, axisY=ys)
    assert core.request('snapshot') == held
    print('PASS Key budget: 2x2 cells match the armed preview, a cell above 1000000 leaves revision unchanged')
    verify_flush_continue(core)
    verify_invalidation(core)


def verify_flush_continue(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='ContinueRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Bone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Wide')['created'][0]
    core.request(
        'deformer.define_grid', nodes=[grid['uuid']],
        axisX=[float(index) for index in range(400)],
        axisY=[float(index) for index in range(500)],
    )
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='Pose', dimensions=1, min=[0, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[1, 0], translation=[4, 0],
    )
    before = core.request('snapshot')
    core.request('node.transform', id=bone['uuid'], translation=[12, 0, 0])
    partial = core.request('snapshot')
    assert partial['deformPending'] is True
    assert partial['revision'] == before['revision'] + 1
    core.request('snapshot')
    assert core.request('snapshot')['revision'] == partial['revision']
    core.request('node.rename', False, id=grid['uuid'], name='No')
    assert core.request('snapshot')['revision'] == partial['revision']
    assert core.request('deform.continue')['pending'] is False
    done = core.request('snapshot')
    assert done['deformPending'] is False
    assert done['canUndo'] == partial['canUndo']
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    assert core.request('snapshot')['deformPending'] is False
    print('PASS Flush continue: first cells commit, deform.continue finishes the same undo')
    verify_bone_overlay(core)


def verify_bone_overlay(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(parent, name, x, y):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[parent],
            restHead=[x, y, 0], restTail=[x, y + 10, 0],
        )['created'][0]
        core.request('node.transform', id=created['uuid'], translation=[x, y, 0])
        return created['uuid']

    parent = bone(rig['uuid'], 'Parent', 0, 0)
    selected = bone(parent, 'Selected', 0, 10)
    bone(selected, 'Grand', 0, 10)
    bone(parent, 'Sibling', 10, 0)
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=selected)
    core.request(
        'rig.set_source_settings', root=rig['uuid'], target=grid['uuid'], bone=selected,
        settings={'rotation': 0.5, 'depthOffset': 1},
    )
    before = core.request('snapshot')
    overlay = core.request('rig.overlay', root=rig['uuid'], selected=selected)
    assert len(overlay['selectedPoints']) == 1
    assert abs(overlay['selectedPoints'][0][1] - 10) < 1e-4
    assert len(overlay['points']) == 3
    assert len(overlay['parentLines']) == 1
    assert len(overlay['childLines']) == 1
    assert len(overlay['lines']) == 1
    assert abs(overlay['effectivePivots'][0]['rotationPivotXShift']) > 1e-4
    assert core.request('snapshot') == before
    other = core.request('node.create', className='DepthRigRoot', name='Other')['created'][0]
    foreign = bone(other['uuid'], 'Foreign', 0, 0)
    held = core.request('snapshot')
    core.request('rig.overlay', False, root=rig['uuid'], selected=foreign)
    assert core.request('snapshot') == held
    print('PASS Bone overlay: classified links and yaw pivot, read does not write history')
    verify_bone_pick_drag(core)
    verify_vertex_weights(core)


def verify_bone_pick_drag(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(parent, name, x, y):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[parent],
            restHead=[x, y, 0], restTail=[x, y + 10, 0],
        )['created'][0]
        core.request('node.transform', id=created['uuid'], translation=[x, y, 0])
        return created['uuid']

    parent = bone(rig['uuid'], 'Parent', 0, 0)
    child = bone(parent, 'Child', 0, 10)
    before = core.request('snapshot')
    assert core.request('rig.pick_bone', root=rig['uuid'], point=[0, 10, 0], radius=1)['bone'] == child
    assert core.request('rig.pick_bone', root=rig['uuid'], point=[100, 100, 0], radius=1)['bone'] is None
    assert core.request('snapshot') == before
    core.request('rig.pick_bone', False, root=rig['uuid'], point=[0, 10, 0], radius=-1)
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 10], axisY=[0, 10])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=child)
    before = core.request('snapshot')
    core.request('rig.drag_bone', root=rig['uuid'], bone=child, translation=[0, 40, 0])
    assert core.request('node.get', id=child)['transform']['trans'] == [0, 40, 0]
    assert core.request('rig.pick_bone', root=rig['uuid'], point=[0, 40, 0], radius=0.5)['bone'] == child
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    core.request('rig.drag_bone', root=rig['uuid'], bone=child, rotation=[0, 0.4, 0])
    assert core.request('snapshot')['previewDeformation'] != before['previewDeformation']
    core.request('undo')
    parameter = core.request('parameter.create', name='Pose', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[child], key=[0, 0],
        translation=[5, 5], translation_z=1,
    )
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[child], key=[1, 0],
        translation=[0, 0], translation_z=0,
    )
    before = core.request('snapshot')
    core.request('rig.drag_bone', root=rig['uuid'], bone=child, translation=[12, 4, 1])
    assert core.request('node.get', id=child)['transform']['trans'] == [0, 10, 0]
    assert core.request('rig.pick_bone', root=rig['uuid'], point=[12, 4, 1], radius=0.5)['bone'] == child
    assert core.request('rig.pick_bone', root=rig['uuid'], point=[12, 14, 1], radius=0.5)['bone'] is None
    stored = core.request('parameter.get', id=parameter['uuid'])

    def channel(name, key):
        binding = next(item for item in stored['bindings'] if item['param_name'] == name and item['node'] == child)
        return binding['values'][key][0]

    assert abs(channel('transform.t.x', 1) - 12) < 1e-4
    assert abs(channel('transform.t.y', 1) - -6) < 1e-4
    assert abs(channel('transform.t.z', 1) - 1) < 1e-4
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    other = core.request('parameter.create', name='Split', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request('binding.set_trs', parameter=other['uuid'], nodes=[child], key=[1, 0], translation_z=0)
    held = core.request('snapshot')
    core.request('rig.drag_bone', False, root=rig['uuid'], bone=child, translation=[3, 0, 0])
    assert core.request('snapshot') == held
    print('PASS Bone pick and drag: closest point, keyed pose, split channels fail closed')
    verify_solve_ik(core)


def verify_solve_ik(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(parent, name, y0, y1):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[parent],
            restHead=[0, y0, 0], restTail=[0, y1, 0],
        )['created'][0]
        core.request('node.transform', id=created['uuid'], translation=[0, y0, 0])
        return created['uuid']

    parent = bone(rig['uuid'], 'Parent', 0, 100)
    child = bone(parent, 'Child', 100, 200)
    before = core.request('snapshot')
    solved = core.request(
        'rig.solve_ik', root=rig['uuid'], endEffector=child, target=[70, 80, 0],
    )
    assert solved['distanceAfter'] < solved['distanceBefore']
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    held = core.request('snapshot')
    core.request('rig.solve_ik', False, root=rig['uuid'], endEffector=child, target=[1, 'no', 0])
    assert core.request('snapshot') == held
    print('PASS IK: CCD shortens the effector distance and a bad target stays closed')
    verify_mirror(core)
    verify_fit_human(core)
    verify_gpu_deform(core)


def verify_gpu_deform(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(parent, name, head, tail):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[parent],
            restHead=head, restTail=tail,
        )['created'][0]
        core.request('node.transform', id=created['uuid'], translation=head)
        return created['uuid']

    parent = bone(rig['uuid'], 'Parent', [0, 0, 0], [0, 100, 0])
    child = bone(parent, 'Child', [0, 100, 0], [0, 200, 0])
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 40], axisY=[80, 140])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=child)
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=parent)
    core.request('rig.set_vertex_weights', root=rig['uuid'], target=grid['uuid'], weights=[
        {'bone': child, 'values': [1, 0.25, 0, 0]},
        {'bone': parent, 'values': [0, 0.75, 1, 1]},
    ])
    parameter = core.request('parameter.create', dimensions=1, name='Bend')
    for bone_id in (parent, child):
        core.request(
            'binding.set_trs', parameter=parameter['uuid'], nodes=[bone_id], key=[1, 0],
            rotation_x=0, rotation_y=0, rotation_degrees=0,
        )
    core.request('rig.solve_ik', root=rig['uuid'], endEffector=child, target=[70, 80, 0])
    before = core.request('snapshot')
    report = core.request('rig.gpu_offsets', root=rig['uuid'], parameter=parameter['uuid'])
    assert report['device'] is True
    assert report['backend'] == 'software'
    assert report['matched'] is True
    assert report['maxAbs'] <= 1e-4
    assert core.request('snapshot') == before
    print('PASS GPU deform: software kernel matches the weighted IK pose and does not write')


def verify_fit_human(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]
    core.request('rig.add_skeleton', root=rig['uuid'])
    bones = core.request('rig.list_bones', root=rig['uuid'])['items']
    head = next(bone['uuid'] for bone in bones if bone['boneId'] == 'Head')
    pelvis = next(bone for bone in bones if bone['boneId'] == 'Pelvis')
    core.request('node.rename', id=head, name='Head Bone')
    grid = core.request('node.create', className='GridDeformer', name='Head')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[10, 30], axisY=[20, 40])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=head)
    before = core.request('snapshot')
    core.request('rig.fit_human', root=rig['uuid'])
    fitted = next(bone for bone in core.request('rig.list_bones', root=rig['uuid'])['items'] if bone['boneId'] == 'Head')
    assert abs(fitted['restHead'][0] - 20) < 1e-4
    assert abs(fitted['restTail'][0] - 20) < 1e-4
    assert sorted([fitted['restHead'][1], fitted['restTail'][1]]) == sorted([20, 40]) or (
        abs(min(fitted['restHead'][1], fitted['restTail'][1]) - 20) < 1e-4
        and abs(max(fitted['restHead'][1], fitted['restTail'][1]) - 40) < 1e-4
    )
    still = next(bone for bone in core.request('rig.list_bones', root=rig['uuid'])['items'] if bone['boneId'] == 'Pelvis')
    assert still['restHead'] == pelvis['restHead']
    binding = next(item for item in core.request('node.get', id=rig['uuid'])['bindings'] if item['target'] == grid['uuid'])
    assert binding['vertexWeights'] == [[1, 1, 1, 1]]
    revision = core.request('snapshot')['revision']
    core.request('rig.fit_human', root=rig['uuid'])
    assert core.request('snapshot')['revision'] == revision
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    core.request('node.delete', nodes=[head])
    held = core.request('snapshot')
    core.request('rig.fit_human', False, root=rig['uuid'])
    assert core.request('snapshot') == held
    print('PASS fit human: named bounds move rest, gaussian weights are stored, and a missing Head stays closed')


def verify_mirror(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(parent, name, head, tail, roll=0):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[parent],
            restHead=head, restTail=tail, restRoll=roll,
        )['created'][0]
        return created['uuid']

    parent = bone(rig['uuid'], 'Parent', [10, 0, 0], [10, 100, 0], 0.15)
    core.request('node.transform', id=parent, rotation=[0.2, -0.15, 0.4])
    child = bone(parent, 'Child', [30, 30, 4], [30, 130, 4], 0.35)
    core.request('node.transform', id=child, rotation=[0.05, 0.1, -0.2])
    before = core.request('snapshot')
    parent_before = core.request('node.get', id=parent)
    child_before = core.request('node.get', id=child)

    def origins():
        return core.request('rig.overlay', root=rig['uuid'])['points']

    def child_point(points):
        return max(points, key=lambda point: abs(point[0] - 10) + abs(point[1]) + abs(point[2]))

    child_origin = child_point(origins())
    held = core.request('snapshot')
    core.request('rig.mirror', False, root=rig['uuid'], bones=[parent], axis='x')
    assert core.request('snapshot') == held
    core.request('rig.mirror', root=rig['uuid'], bones=[child], axis='x')
    mirrored = core.request('node.get', id=child)
    assert core.request('node.get', id=parent)['transform'] == parent_before['transform']
    assert abs(mirrored['restHead'][0] + child_before['restHead'][0]) < 1e-9
    assert abs(mirrored['restHead'][1] - child_before['restHead'][1]) < 1e-9
    assert abs(mirrored['restRoll'] + child_before['restRoll']) < 1e-9
    naive = [-child_before['transform']['trans'][0], child_before['transform']['trans'][1], child_before['transform']['trans'][2]]
    actual = mirrored['transform']['trans']
    assert sum(abs(a - b) for a, b in zip(actual, naive)) > 1e-2
    reflected = child_point(origins())
    assert abs(reflected[0] + child_origin[0]) < 1e-4
    assert abs(reflected[1] - child_origin[1]) < 1e-4
    assert abs(reflected[2] - child_origin[2]) < 1e-4
    core.request('rig.mirror', root=rig['uuid'], bones=[child], axis='x')
    restored = core.request('node.get', id=child)
    assert abs(restored['restHead'][0] - child_before['restHead'][0]) < 1e-6
    assert abs(restored['transform']['rot'][2] - child_before['transform']['rot'][2]) < 1e-5
    core.request('undo')
    core.request('undo')
    assert core.request('snapshot')['model'] == before['model']
    held = core.request('snapshot')
    core.request('rig.mirror', False, root=rig['uuid'], bones=[child], axis='y')
    assert core.request('snapshot') == held

    left = bone(rig['uuid'], 'Left', [-20, 0, 0], [-20, 40, 0])
    right = bone(rig['uuid'], 'Right', [20, 0, 0], [20, 40, 0])
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[-1, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=left)
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=right)
    core.request('rig.set_vertex_weights', root=rig['uuid'], target=grid['uuid'], weights=[
        {'bone': left, 'values': [1, 0, 0.2, 0.8]},
        {'bone': right, 'values': [0, 1, 0.8, 0.2]},
    ])
    weighted = core.request('snapshot')
    core.request('rig.mirror', False, root=rig['uuid'], bones=[left], axis='x')
    assert core.request('snapshot') == weighted
    core.request('rig.mirror', root=rig['uuid'], bones=[left, right], axis='x')
    rows = next(item['vertexWeights'] for item in core.request('node.get', id=rig['uuid'])['bindings'] if item['target'] == grid['uuid'])
    assert rows == [[0, 1, 0.8, 0.2], [1, 0, 0.2, 0.8]]
    core.request('undo')
    assert core.request('snapshot')['model'] == weighted['model']
    print('PASS Bone mirror: reflected child, closed subtree, weights, undo')

def verify_vertex_weights(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]

    def bone(name, x):
        created = core.request(
            'node.create', className='DepthBone', name=name, parents=[rig['uuid']],
            restHead=[x, 0, 0], restTail=[x, 100, 0],
        )['created'][0]
        core.request('node.transform', id=created['uuid'], translation=[x, 0, 0])
        return created['uuid']

    moved = bone('Moved', 0)
    still = bone('Still', 0)
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=moved)
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=still)
    parameter = core.request('parameter.create', dimensions=1, name='Slide')
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[moved],
        key=[1, 0], translation=[10, 0],
    )
    gaussian = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    core.request('rig.set_vertex_weights', root=rig['uuid'], target=grid['uuid'], weights=[
        {'bone': moved, 'values': [1, 1, 0, 0]},
        {'bone': still, 'values': [0, 0, 1, 1]},
    ])
    weighted = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    offsets = weighted['targets'][0]['offsets']
    assert offsets[0] == [10, 0] or all(abs(offsets[0][i] - [10, 0][i]) < 1e-4 for i in range(2))
    assert all(abs(offsets[2][i]) < 1e-4 for i in range(2))
    assert offsets != gaussian['targets'][0]['offsets']
    held = core.request('snapshot')
    core.request(
        'rig.set_vertex_weights', False, root=rig['uuid'], target=grid['uuid'],
        weights=[
            {'bone': moved, 'values': [1, 0]},
            {'bone': still, 'values': [0, 1, 0, 0]},
        ],
    )
    core.request(
        'rig.add_source', False, root=rig['uuid'], target=grid['uuid'], bone=moved,
    )
    assert core.request('snapshot') == held
    saved = Path(tempfile.mkdtemp()) / 'weights.inx'
    core.request('save', path=str(saved))
    core.request('open', path=str(saved))
    binding = next(
        item for item in core.request('node.get', id=rig['uuid'])['bindings']
        if item['target'] == grid['uuid']
    )
    assert binding['vertexWeights'] == [[1, 1, 0, 0], [0, 0, 1, 1]]
    core.request('rig.clear_vertex_weights', root=rig['uuid'], target=grid['uuid'])
    restored = core.request('rig.preview_deform', root=rig['uuid'], parameter=parameter['uuid'])
    assert restored['targets'][0]['offsets'] == gaussian['targets'][0]['offsets']
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=moved)
    print('PASS Vertex weights: locked offsets, save/reload, clear restores gaussian')

def verify_invalidation(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='CleanupRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Head', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    target = core.request('node.create', className='GridDeformer', name='Eye')['created'][0]
    sibling = core.request('node.create', className='GridDeformer', name='Sibling')['created'][0]
    for node in (target, sibling):
        core.request('deformer.define_grid', nodes=[node['uuid']], axisX=[-10, 10], axisY=[0, 100])
        core.request('rig.add_source', root=rig['uuid'], target=node['uuid'], bone=bone['uuid'])
    core.request(
        'rig.set_source_settings', root=rig['uuid'], target=target['uuid'], bone=bone['uuid'],
        settings={'depthOffset': 0.25},
    )
    core.request(
        'rig.set_influence_rule', root=rig['uuid'], target=target['uuid'],
        rule={'multipliersByBoneUuid': {str(bone['uuid']): 0.75}},
    )
    parameter = core.request('parameter.create', name='Shift', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[1, 0], translation=[5, 0],
    )
    before = core.request('snapshot')['model']
    core.request('node.convert', nodes=[target['uuid']], className='Node')
    assert core.request('node.get', id=target['uuid'])['type'] == 'Node'
    bindings = core.request('node.get', id=rig['uuid'])['bindings']
    assert len(bindings) == 1 and bindings[0]['target'] == sibling['uuid']
    stored = core.request('parameter.get', id=parameter['uuid'])
    sibling_deform = next(
        item for item in stored['bindings']
        if item['param_name'] == 'deform' and item['node'] == sibling['uuid']
    )
    assert sibling_deform['isSet'][1][0] is True
    core.request('undo')
    assert core.request('snapshot')['model'] == before
    restored = next(
        item for item in core.request('node.get', id=rig['uuid'])['bindings']
        if item['target'] == target['uuid']
    )
    assert restored['sourceSettings'][0]['depthOffset'] == 0.25
    assert restored['influenceRule']['multipliersByBoneUuid'][str(bone['uuid'])] == 0.75
    print('PASS Invalidation: convert drops the node binding, refreshes the sibling, one undo')
    verify_bone_pose(core)


def verify_bone_pose(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='PoseRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Head', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='PoseGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='Yaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[2, 0], rotation_y=1.5707963267948966,
    )
    before = core.request('snapshot')['model']
    core.request(
        'rig.set_constraint', bone=bone['uuid'], constraint={'lockRotation': True},
    )
    stored = core.request('parameter.get', id=parameter['uuid'])
    deform = next(item for item in stored['bindings'] if item['param_name'] == 'deform')
    assert deform['isSet'][2][0] is True
    assert all(abs(component) < 1e-6 for pair in deform['values'][2][0] for component in pair)
    assert core.request('node.get', id=bone['uuid'])['lockRotation'] is True
    core.request('undo')
    assert core.request('snapshot')['model'] == before
    core.request(
        'rig.set_rest', bone=bone['uuid'],
        restHead=[1, 2, 3], restTail=[1, 12, 3], restRoll=0.25,
    )
    core.request(
        'rig.set_constraint', bone=bone['uuid'],
        constraint={
            'constraintType': 'hinge',
            'hingeAxis': [1, 0, 0],
            'rotationLimits': [-0.5, 0.5],
            'maxStepRadians': 0.125,
        },
    )
    held = core.request('snapshot')
    core.request('rig.set_constraint', False, bone=bone['uuid'], constraint={'hingeAxis': [1, 0]})
    assert core.request('snapshot') == held
    node = core.request('node.get', id=bone['uuid'])
    assert node['restTail'] == [1, 12, 3]
    assert node['restRoll'] == 0.25
    assert node['constraintType'] == 'hinge'
    assert node['rotationLimits'] == [-0.5, 0.5]
    assert node['maxStepRadians'] == 0.125
    print('PASS Bone pose: lockRotation clears yaw, rest and hinge fields undo, bad hinge is unchanged')
    verify_rig_transform(core)


def verify_rig_transform(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='MoveRig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Head', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='MoveGrid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='Yaw', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[2, 0], rotation_y=1.5707963267948966,
    )
    before = core.request('snapshot')['model']
    core.request('node.transform', id=bone['uuid'], translation=[30, 0, 0])
    assert core.request('node.get', id=bone['uuid'])['transform']['trans'] == [30, 0, 0]
    stored = core.request('parameter.get', id=parameter['uuid'])
    assert not any(item['param_name'] == 'transform.t.z' for item in stored['bindings'])
    core.request('undo')
    assert core.request('snapshot')['model'] == before
    held = core.request('snapshot')
    core.request('node.transform', False, id=bone['uuid'], translation=['inf', 0, 0])
    assert core.request('snapshot') == held
    core.request('node.move', False, nodes=[1], parent=bone['uuid'])
    assert core.request('snapshot') == held
    print('PASS Rig transform: bone translation undoes with deform, non-finite and puppet root stay closed')
    verify_rig_move(core)
    verify_undriven_deform(core)


def verify_rig_move(core):
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node', 'enabled': True, 'zsort': 0,
            'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
            'children': [{
                'uuid': 2, 'name': 'Card', 'type': 'Part', 'enabled': True, 'zsort': 0,
                'transform': {'trans': [10, 0, 4], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'children': [],
            }],
        },
        'param': [],
    })
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Bone', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 10, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Grid')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[0, 1], axisY=[0, 1])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    core.request(
        'node.transform', id=bone['uuid'],
        translation=[1, 2, 8], rotation=[0.25, 0.5, 0], scale=[1, 1],
    )
    bindings = core.request('node.get', id=rig['uuid'])['bindings']
    deform = core.request('snapshot')['model']['param']
    before = core.request('snapshot')['model']
    core.request('node.move', nodes=[bone['uuid']], parent=2)
    moved = core.request('node.get', id=bone['uuid'])
    assert moved['transform']['trans'] == [-9, 2, 4]
    assert moved['transform']['rot'] == [0.25, 0.5, 0]
    assert core.request('node.get', id=rig['uuid'])['bindings'] == bindings
    assert all(child['uuid'] != bone['uuid'] for child in core.request('node.get', id=rig['uuid'])['children'])
    assert any(child['uuid'] == bone['uuid'] for child in core.request('node.get', id=2)['children'])
    assert core.request('snapshot')['model']['param'] == deform
    core.request('undo')
    assert core.request('snapshot')['model'] == before
    core.request('node.move', False, nodes=[1], parent=rig['uuid'])
    core.request('node.move', False, nodes=[rig['uuid']], parent=bone['uuid'])
    assert core.request('snapshot')['model'] == before
    print('PASS Rig move: reparent keeps bindings and deform, undo restores the parent')


def verify_undriven_deform(core):
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node', 'enabled': True,
            'children': [{
                'uuid': 4, 'name': 'Grid', 'type': 'GridDeformer', 'enabled': True,
                'grid_axis_x': [0, 2], 'grid_axis_y': [0, 2],
            }],
        },
        'param': [{
            'uuid': 9, 'name': 'Deform', 'is_vec2': False,
            'min': [0, 0], 'max': [1, 0], 'defaults': [0, 0],
            'axis_points': [[0, 1], [0]],
            'bindings': [{
                'node': 4, 'param_name': 'deform',
                'values': [[
                    [[1, 0], [0, 0], [0, 0], [1, 0]],
                ], [
                    [[0, 0], [0, 0], [0, 0], [0, 0]],
                ]],
                'isSet': [[True], [True]],
            }],
        }],
        'automation': [],
    })
    before = core.request('snapshot')['model']
    core.request('deformer.define_grid', nodes=[4], axisX=[0, 1, 2], axisY=[0, 2])
    binding = core.request('parameter.get', id=9)['bindings'][0]
    assert binding['values'][0][0] == [[1, 0], [0.5, 0], [0, 0], [0, 0], [0.5, 0], [1, 0]]
    assert len(binding['values'][1][0]) == 6
    core.request('undo')
    assert core.request('snapshot')['model'] == before
    core.request('deformer.define_grid', nodes=[4], axisX=[0, 1, 2], axisY=[0, 2])
    resized = core.request('parameter.get', id=9)['bindings'][0]['values']
    core.request('node.convert', nodes=[4], className='Node')
    assert core.request('node.get', id=4)['type'] == 'Node'
    assert core.request('parameter.get', id=9)['bindings'][0]['values'] == resized
    core.request('new', model={
        'nodes': before['nodes'],
        'param': [{
            'uuid': 9, 'name': 'Deform', 'is_vec2': False,
            'axis_points': [[0, 1], [0]],
            'bindings': [{'node': 4, 'param_name': 'deform', 'values': [], 'isSet': []}],
        }],
        'automation': [],
    })
    blocked = core.request('snapshot')
    core.request('deformer.define_grid', False, nodes=[4], axisX=[0, 1], axisY=[0, 1])
    assert core.request('snapshot') == blocked
    core.request('new', model={
        'nodes': {
            'uuid': 1, 'name': 'Root', 'type': 'Node',
            'children': [{
                'uuid': 6, 'name': 'Textured', 'type': 'GridDeformer',
                'grid_axis_x': [0, 1], 'grid_axis_y': [0, 1], 'textures': [1],
            }],
        },
        'param': [], 'automation': [],
    })
    textured = core.request('snapshot')
    core.request('deformer.define_grid', False, nodes=[6], axisX=[0, 1, 2], axisY=[0, 1])
    core.request('node.convert', False, nodes=[6], className='Part')
    assert core.request('snapshot') == textured
    print('PASS Undriven deform: grid cells resize with one undo, convert keeps them, malformed and Part stay closed')
    verify_leftover_deform(core)


def verify_leftover_deform(core):
    core.request('new')
    rig = core.request('node.create', className='DepthRigRoot', name='Rig')['created'][0]
    bone = core.request(
        'node.create', className='DepthBone', name='Head', parents=[rig['uuid']],
        restHead=[0, 0, 0], restTail=[0, 100, 0],
    )['created'][0]
    grid = core.request('node.create', className='GridDeformer', name='Eye')['created'][0]
    core.request('deformer.define_grid', nodes=[grid['uuid']], axisX=[-10, 10], axisY=[0, 100])
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    parameter = core.request('parameter.create', name='Shift', dimensions=1, min=[-1, 0], max=[1, 1])
    core.request(
        'binding.set_trs', parameter=parameter['uuid'], nodes=[bone['uuid']],
        key=[1, 0], translation=[5, 0],
    )
    posed = deform_row(core, parameter['uuid'], grid['uuid'])
    assert any(pair != [0, 0] for pair in posed['values'][1][0])
    core.request('node.convert', nodes=[grid['uuid']], className='Node')
    assert core.request('node.get', id=grid['uuid'])['type'] == 'Node'
    assert rig_targets(core, rig['uuid']) == []
    assert deform_row(core, parameter['uuid'], grid['uuid']) == posed
    held = core.request('snapshot')
    core.request('rig.add_source', False, root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    assert core.request('snapshot') == held
    core.request('node.convert', nodes=[grid['uuid']], className='GridDeformer')
    assert rig_targets(core, rig['uuid']) == []
    assert deform_row(core, parameter['uuid'], grid['uuid']) == posed
    core.request('rig.add_source', root=rig['uuid'], target=grid['uuid'], bone=bone['uuid'])
    refreshed = deform_row(core, parameter['uuid'], grid['uuid'])
    assert refreshed['isSet'] == [[True], [True], [True]]
    assert refreshed['isSet'] != posed['isSet']
    assert refreshed['values'][1][0] == [[5, 0]] * 4
    core.request('undo')
    assert deform_row(core, parameter['uuid'], grid['uuid']) == posed
    assert rig_targets(core, rig['uuid']) == []
    print('PASS Leftover deform: convert keeps offsets, add_source regenerates them, a Node stays unpacked')


def deform_row(core, parameter_id, node_id):
    bindings = core.request('parameter.get', id=parameter_id)['bindings']
    return next(item for item in bindings if item['param_name'] == 'deform' and item['node'] == node_id)


def rig_targets(core, rig_id):
    bindings = core.request('node.get', id=rig_id).get('bindings') or []
    return [item['target'] for item in bindings]


def verify_simple_physics(core, other, directory, original, tail):
    core.request('open', path=str(original))
    created = core.request('node.create', className='SimplePhysics', name='Hair')['created'][0]
    node_id = created['uuid']
    node = core.request('node.get', id=node_id)
    assert node['type'] == 'SimplePhysics'
    assert node['param'] == 4294967295
    assert node['model_type'] == 'Pendulum'
    assert node['map_mode'] == 'AngleLength'
    assert node['gravity'] == 1
    assert node['length'] == 100
    assert node['frequency'] == 1
    assert node['angle_damping'] == 0.5
    assert node['length_damping'] == 0.5
    assert node['output_scale'] == [1, 1]
    assert node['local_only'] is False
    parameter = core.request('parameter.create', name='Phys', dimensions=1)
    before = core.request('snapshot')
    core.request(
        'physics.set', node=node_id, param=parameter['uuid'], model_type='SpringPendulum',
        map_mode='XY', gravity=2.25, length=123, frequency=4.5, angle_damping=0.35,
        length_damping=0.45, output_scale=[1.25, 1.75], local_only=True,
    )
    edited = core.request('node.get', id=node_id)
    assert edited['model_type'] == 'SpringPendulum'
    assert edited['map_mode'] == 'XY'
    assert edited['param'] == parameter['uuid']
    assert edited['local_only'] is True
    assert abs(edited['gravity'] - 2.25) < 1e-9
    assert edited['output_scale'] == [1.25, 1.75]
    assert core.request('snapshot')['model']['param'] == before['model']['param']
    assert core.request('snapshot')['values'] == before['values']
    core.request('undo')
    restored = core.request('node.get', id=node_id)
    assert restored['model_type'] == 'Pendulum'
    assert restored['param'] == 4294967295
    core.request('redo')
    held = core.request('snapshot')
    for kwargs in (
        {'model_type': 'spring_pendulum'},
        {'map_mode': 'xy'},
        {'gravity': 'fast'},
        {'param': 1},
        {'output_scale': [1]},
    ):
        core.request('physics.set', False, node=node_id, **kwargs)
        assert core.request('snapshot') == held
    core.request('physics.step', False, node=node_id)
    assert core.request('snapshot') == held
    path = core.request('node.create', className='PathDeformer', name='Rope')['created'][0]['uuid']
    driver = {'type': 'SpringPendulum', 'damping': 1, 'future_path': {'keep': 7}}
    model = json.loads(json.dumps(core.request('snapshot')['model']))
    find_child(model['nodes'], path)['physics'] = driver
    find_child(model['nodes'], node_id)['future_driver'] = {'keep': True}
    raw = json.dumps(model, separators=(',', ':')).encode()
    source = directory / 'simple-physics-source.inx'
    source.write_bytes(b'TRNSRTS\0' + struct.pack('>I', len(raw)) + raw + tail)
    core.request('open', path=str(source))
    opaque = core.request('snapshot')
    core.request('physics.set', False, node=path, gravity=3)
    assert core.request('snapshot') == opaque
    core.request('physics.set', node=node_id, gravity=3)
    changed = core.request('snapshot')['model']
    assert find_child(changed['nodes'], path)['physics'] == driver
    assert find_child(changed['nodes'], node_id)['future_driver'] == {'keep': True}
    assert find_child(changed['nodes'], node_id)['gravity'] == 3
    saved = directory / 'simple-physics.inx'
    core.request('save', path=str(saved))
    _, saved_tail = split_container(saved.read_bytes())
    assert saved_tail == tail
    other.request('open', path=str(saved))
    reloaded = find_child(other.request('snapshot')['model']['nodes'], node_id)
    assert reloaded['model_type'] == 'SpringPendulum'
    assert reloaded['gravity'] == 3
    assert reloaded['future_driver'] == {'keep': True}
    assert find_child(other.request('snapshot')['model']['nodes'], path)['physics'] == driver
    print('PASS SimplePhysics: settings, undo, fail-closed, opaque path driver, save/reload')


def verify_ffi(library, directory, original, expected, tail):
    core, other = Core(library), Core(library)
    try:
        core.request('open', path=str(original))
        assert core.request('snapshot')['model'] == expected
        projection = core.request('snapshot', d_compatible=True)
        assert projection['model']['future_root']['large_integer'] == str(expected['future_root']['large_integer'])
        assert core.request('snapshot')['model'] == expected, 'D projection mutated canonical document'
        param = core.request('parameter.create', name='HeadYaw', dimensions=1, min=[-1, 0], max=[1, 1])
        uid = param['uuid']
        assert param['axis_points'] == [[0, .5, 1], [0]]
        assert other.request('parameters') == [], 'sessions share state'
        core.request('parameter.rename', id=uid, name='Yaw')
        assert core.request('parameter.get', id=uid)['name'] == 'Yaw'
        core.request('binding.set_trs', parameter=uid, nodes=[2], key=[0, 0], translation=[-2, 0])
        core.request('binding.set_trs', parameter=uid, nodes=[2], key=[2, 0], translation=[2, 0])
        evaluated = core.request('evaluate', values={str(uid): [.5, 0]})
        node = evaluated['nodes']['children'][0]
        assert abs(node['transform']['trans'][0] - 1) < 1e-6, evaluated
        before = core.request('snapshot')
        core.request('binding.set_trs', False, parameter=uid, nodes=[2, 999999], key=[2, 0], translation=[4, 0])
        assert core.request('snapshot') == before, 'failed edit changed state/history'
        core.request('parameter.rename', False, id=999999, name='wrong')
        assert core.request('snapshot') == before
        core.request('undo')
        assert core.request('snapshot')['canRedo']
        core.request('redo')
        image0 = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [0, 0]})
        image1 = core.request('render', width=8, height=8, bounds=[0, 0, 8, 8], values={str(uid): [1, 0]})
        normal = base64.b64decode(image0['png'])
        shifted = base64.b64decode(image1['png'])
        assert normal[:8] == b'\x89PNG\r\n\x1a\n' and normal != shifted
        width, height, pixels = decode_rgba(normal)
        assert (width, height) == (8, 8)
        assert pixels[(3 * 8 + 3) * 4:(3 * 8 + 3) * 4 + 4] == bytes([255, 0, 0, 255])
        assert pixels[:4] == bytes(4)
        _, _, shifted_pixels = decode_rgba(shifted)
        assert shifted_pixels[(3 * 8 + 3) * 4:(3 * 8 + 3) * 4 + 4] == bytes(4)
        assert shifted_pixels[(3 * 8 + 5) * 4:(3 * 8 + 5) * 4 + 4] == bytes([255, 0, 0, 255])
        (directory / 'preview.png').write_bytes(normal)
        (directory / 'preview-shifted.png').write_bytes(shifted)
        state = core.request('snapshot')
        core.request('value.set', id=uid, value=[.25, 0])
        assert core.request('snapshot')['model'] == state['model']
        run_directory = Path(tempfile.mkdtemp(prefix='roundtrip-', dir=directory))
        saved = run_directory / 'roundtrip.inx'
        core.request('save', path=str(saved))
        assert not core.request('snapshot')['modified']
        model, saved_tail = split_container(saved.read_bytes())
        assert saved_tail == tail, 'texture/extension/trailing bytes changed'
        assert model['future_root'] == expected['future_root']
        assert model['nodes']['children'][0]['future_part'] == expected['nodes']['children'][0]['future_part']
        other.request('open', path=str(saved))
        assert other.request('snapshot')['model'] == model
        core.request('undo')
        core.request('parameter.rename', id=uid, name='Branched')
        assert not core.request('snapshot')['canRedo']
        assert core.request('snapshot')['modified']
        core.request('redo', False)
        broken = directory / 'truncated.inx'
        broken.write_bytes(original.read_bytes()[:15])
        before = core.request('snapshot')
        core.request('open', False, path=str(broken))
        assert core.request('snapshot') == before, 'failed open replaced session'
        translucent_model = json.loads(json.dumps(expected))
        translucent_model['nodes']['children'][0]['opacity'] = .5
        encoded = json.dumps(translucent_model).encode()
        translucent = run_directory / 'translucent.inx'
        translucent.write_bytes(b'TRNSRTS\0' + struct.pack('>I', len(encoded)) + encoded + tail)
        other.request('open', path=str(translucent))
        image = other.request('render', width=8, height=8, bounds=[0, 0, 8, 8])
        _, _, pixels = decode_rgba(base64.b64decode(image['png']))
        assert pixels[(3 * 8 + 3) * 4:(3 * 8 + 3) * 4 + 4] == bytes([255, 0, 0, 128]), 'PNG uses premultiplied output colors'
        verify_node_slice(core, other, directory, original, tail)
        verify_part_mesh(core)
        verify_node_tree(core, other, directory, original, tail)
        verify_node_clipboard(core, other, directory, original, tail)
        verify_deformers(core, other, directory, original, tail)
        verify_pitch_yaw_z(core, other, directory, original, tail)
        verify_rig_nodes(core, other, directory, original, tail)
        verify_simple_physics(core, other, directory, original, tail)
        print('PASS FFI: identity, isolated sessions, TRS, history, atomic failures, opaque roundtrip, PNG changes')
    finally:
        core.close()
        other.close()


def verify_mcp(binary, original, directory):
    process = subprocess.Popen([str(binary), '--mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
    timeout = threading.Timer(30, process.kill)
    timeout.start()
    log = []

    def call(method, params=None):
        uid = len(log) + 1
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': uid, 'method': method, 'params': params or {}}) + '\n')
        process.stdin.flush()
        response = json.loads(process.stdout.readline())
        log.append(response)
        assert response['id'] == uid and 'result' in response, response
        return response['result']

    try:
        call('initialize', {'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'verify', 'version': '1'}})
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
        tools = {t['name'] for t in call('tools/list')['tools']}
        assert {'open', 'parameter_create', 'binding_set_trs', 'binding_set_deform',
                'animation_create', 'animation_set', 'animation_key_set', 'animation_key_delete',
                'animation_sample', 'animation_seek', 'animation_advance', 'animation_reset',
                'render', 'undo', 'redo', 'node_create',
                'node_copy', 'node_cut', 'node_paste', 'node_convert',
                'NodeCommand_AddNode', 'NodeCommand_SetNodeName', 'NodeCommand_MoveNode',
                'NodeCommand_DeleteNode', 'NodeCommand_CopyNode', 'NodeCommand_CutNode',
                'NodeCommand_PasteNode', 'NodeCommand_ConvertTo', 'Node_ConvertTo_Composite',
                'Node_ConvertTo_Camera', 'Node_ConvertTo_Mask', 'Node_ConvertTo_GridDeformer',
                'Node_Add_PathDeformer', 'VertexCommand_DefineGrid',
                'Inspector_Apply_PathDeformCurveType',
                'ModelCommand_SetRotationXBinding', 'ModelCommand_SetRotationYBinding',
                'DepthBoneCommand_CreateDepthRigRoot', 'DepthBoneCommand_AddDepthBone',
                'DepthBoneCommand_AddStandardDepthSkeleton', 'DepthBoneCommand_AddStandardDepthParameters',
                'DepthBoneCommand_ListDepthBones',
                'DepthBoneCommand_AddDepthBoneSource', 'DepthBoneCommand_ListDepthBoneSources',
                'DepthBoneCommand_GetDepthBoneInfluenceRule', 'DepthBoneCommand_PreviewDepthBoneInfluence',
                'DepthBoneCommand_PreviewDepthBoneDeform', 'DepthBoneCommand_ApplyDepthBoneDeform',
                'DepthBoneCommand_FitDepthRigRootZToDepth', 'DepthBoneCommand_FitDepthBoneZToDepth',
                'DepthMapCommand_ListDepths', 'DepthMapCommand_SetDepths',
                'DepthMapCommand_ClearDepths', 'DepthMapCommand_ListDepthOps',
                'DepthMapCommand_SetDepthOps', 'DepthMapCommand_ApplyDepthOps',
                'DepthMapCommand_ImportPSDDepths', 'part_set_weld', 'part_clear_weld'} <= tools
        assert 'Node_ConvertTo_Part' in tools
        for name, arguments in [
            ('animation_create', {'name': 'Protocol Clock'}),
            ('animation_set', {'name': 'Protocol Clock', 'length': 10, 'timestep': 0.125}),
            ('animation_seek', {'name': 'Protocol Clock', 'frame': 0}),
            ('animation_advance', {'dt': 0.25, 'loop': False}),
            ('animation_reset', {}),
        ]:
            response = call('tools/call', {'name': name, 'arguments': arguments})
            assert not response.get('isError', False), response
            if name == 'animation_advance':
                state = response.get('structuredContent') or json.loads(response['content'][0]['text'])
                assert state['animation']['frame'] == 2, state
        invalid_step = call('tools/call', {'name': 'animation_advance', 'arguments': {'dt': -1e-100}})
        assert invalid_step.get('isError', False), invalid_step
        assert not call('tools/call', {'name': 'open', 'arguments': {'path': str(original)}}).get('isError', False)
        rig_created = call('tools/call', {'name': 'DepthBoneCommand_CreateDepthRigRoot', 'arguments': {'name': 'MCP Rig'}})
        assert not rig_created.get('isError', False), rig_created
        rig_uuid = (rig_created.get('structuredContent') or json.loads(rig_created['content'][0]['text']))['created'][0]['uuid']
        bone_created = call('tools/call', {'name': 'DepthBoneCommand_AddDepthBone', 'arguments': {
            'parent': rig_uuid, 'boneId': 'Spine', 'restHead': [0, 1, 0], 'restTail': [0, 11, 0],
        }})
        assert not bone_created.get('isError', False), bone_created
        loose_bone = call('tools/call', {'name': 'DepthBoneCommand_AddDepthBone', 'arguments': {
            'parent': 1, 'boneId': 'Loose', 'restHead': [0, 0, 0], 'restTail': [0, 1, 0],
        }})
        assert loose_bone.get('isError', False), loose_bone
        occupied = call('tools/call', {'name': 'DepthBoneCommand_AddStandardDepthSkeleton', 'arguments': {'root': rig_uuid}})
        assert not occupied.get('isError', False), occupied
        assert not call('tools/call', {'name': 'undo', 'arguments': {}}).get('isError', False)
        template = call('tools/call', {'name': 'DepthBoneCommand_CreateDepthRigRoot', 'arguments': {'name': 'Template'}})
        assert not template.get('isError', False), template
        template_uuid = (template.get('structuredContent') or json.loads(template['content'][0]['text']))['created'][0]['uuid']
        skeleton = call('tools/call', {'name': 'DepthBoneCommand_AddStandardDepthSkeleton', 'arguments': {
            'root': template_uuid, 'scale': 0,
        }})
        assert not skeleton.get('isError', False), skeleton
        listed = call('tools/call', {'name': 'DepthBoneCommand_ListDepthBones', 'arguments': {'root': template_uuid}})
        assert not listed.get('isError', False), listed
        bones = (listed.get('structuredContent') or json.loads(listed['content'][0]['text']))['items']
        assert len(bones) == 19
        assert bones[0]['boneId'] == 'Pelvis' and bones[0]['parent'] == template_uuid
        head = next(item for item in bones if item['boneId'] == 'Head')
        assert head['allowParentToTargets'] is False
        node_created = call('tools/call', {'name': 'NodeCommand_AddNode',
                                           'arguments': {'className': 'Node', '_suffix': ''}})
        assert not node_created.get('isError', False), node_created
        node_payload = node_created.get('structuredContent') or json.loads(node_created['content'][0]['text'])
        node_uuid = node_payload['created'][0]['uuid']
        renamed = call('tools/call', {'name': 'NodeCommand_SetNodeName', 'arguments': {
            'newNames': ['MCP Node'], 'context': {'nodes': [node_uuid]},
        }})
        assert not renamed.get('isError', False), renamed
        move_parent = call('tools/call', {'name': 'NodeCommand_AddNode',
                                          'arguments': {'className': 'Node', '_suffix': ' Parent'}})
        parent_uuid = (move_parent.get('structuredContent') or json.loads(move_parent['content'][0]['text']))['created'][0]['uuid']
        moved = call('tools/call', {'name': 'NodeCommand_MoveNode', 'arguments': {
            'newParent': parent_uuid, 'index': 0, 'context': {'nodes': [node_uuid]},
        }})
        assert not moved.get('isError', False), moved
        deleted = call('tools/call', {'name': 'NodeCommand_DeleteNode', 'arguments': {
            'context': {'nodes': [node_uuid]},
        }})
        assert not deleted.get('isError', False), deleted
        copied = call('tools/call', {'name': 'NodeCommand_CopyNode', 'arguments': {
            'context': {'nodes': [parent_uuid]},
        }})
        assert not copied.get('isError', False), copied
        pasted = call('tools/call', {'name': 'NodeCommand_PasteNode', 'arguments': {
            'context': {'nodes': [1]},
        }})
        assert not pasted.get('isError', False), pasted
        converted = call('tools/call', {'name': 'Node_ConvertTo_Composite', 'arguments': {
            'context': {'nodes': [parent_uuid]},
        }})
        assert not converted.get('isError', False), converted
        blocked = call('tools/call', {'name': 'NodeCommand_ConvertTo', 'arguments': {
            'className': 'GridDeformer', 'context': {'nodes': [parent_uuid]},
        }})
        assert not blocked.get('isError', False), blocked
        grid_added = call('tools/call', {'name': 'Node_Add_GridDeformer', 'arguments': {'_suffix': ''}})
        assert not grid_added.get('isError', False), grid_added
        grid_uuid = (grid_added.get('structuredContent') or json.loads(grid_added['content'][0]['text']))['created'][0]['uuid']
        listed_depths = call('tools/call', {'name': 'DepthMapCommand_ListDepths', 'arguments': {'target': grid_uuid}})
        assert not listed_depths.get('isError', False), listed_depths
        listed_payload = listed_depths.get('structuredContent') or json.loads(listed_depths['content'][0]['text'])
        assert listed_payload['count'] == -1 and listed_payload['depths'] is None
        set_depths = call('tools/call', {'name': 'DepthMapCommand_SetDepths', 'arguments': {
            'depths': [0, 0.25, -0.5, 1], 'context': {'nodes': [grid_uuid]},
        }})
        assert not set_depths.get('isError', False), set_depths
        set_payload = set_depths.get('structuredContent') or json.loads(set_depths['content'][0]['text'])
        assert set_payload['depths'] == [0, 0.25, -0.5, 1]
        set_ops = call('tools/call', {'name': 'DepthMapCommand_SetDepthOps', 'arguments': {
            'target': grid_uuid,
            'operations': [{'type': 'attached-point', 'index': 0, 'amount': 0.25}],
        }})
        assert not set_ops.get('isError', False), set_ops
        applied_ops = call('tools/call', {'name': 'DepthMapCommand_ApplyDepthOps', 'arguments': {
            'target': grid_uuid,
        }})
        assert not applied_ops.get('isError', False), applied_ops
        applied_payload = applied_ops.get('structuredContent') or json.loads(applied_ops['content'][0]['text'])
        assert applied_payload['depths'][0] == 0.25
        blocked_psd = call('tools/call', {'name': 'DepthMapCommand_ImportPSDDepths', 'arguments': {
            'path': '/tmp/unused.psd',
        }})
        assert blocked_psd.get('isError', False), blocked_psd
        cleared_depths = call('tools/call', {'name': 'DepthMapCommand_ClearDepths', 'arguments': {'target': grid_uuid}})
        assert not cleared_depths.get('isError', False), cleared_depths
        path_added = call('tools/call', {'name': 'Node_Add_PathDeformer', 'arguments': {
            '_suffix': '', 'context': {'nodes': [1]},
        }})
        assert not path_added.get('isError', False), path_added
        path_uuid = (path_added.get('structuredContent') or json.loads(path_added['content'][0]['text']))['created'][0]['uuid']
        defined = call('tools/call', {'name': 'VertexCommand_DefineVertices', 'arguments': {
            'vertices': [0, 0, 3, 1], 'context': {'nodes': [path_uuid]},
        }})
        assert not defined.get('isError', False), defined
        curved = call('tools/call', {'name': 'Inspector_Apply_PathDeformCurveType', 'arguments': {
            'value': 'Bezier', 'context': {'nodes': [path_uuid]},
        }})
        assert not curved.get('isError', False), curved
        card = call('tools/call', {'name': 'NodeCommand_AddNode',
                                  'arguments': {'className': 'Node', '_suffix': ' Card'}})
        assert not card.get('isError', False), card
        card_uuid = (card.get('structuredContent') or json.loads(card['content'][0]['text']))['created'][0]['uuid']
        part = call('tools/call', {'name': 'Node_ConvertTo_Part', 'arguments': {
            'context': {'nodes': [card_uuid]},
        }})
        assert not part.get('isError', False), part
        part_blocked = call('tools/call', {'name': 'node_convert', 'arguments': {
            'className': 'Part', 'nodes': [parent_uuid],
        }})
        assert part_blocked.get('isError', False), part_blocked
        restored = call('tools/call', {'name': 'undo', 'arguments': {}})
        assert not restored.get('isError', False), restored
        assert not call('tools/call', {'name': 'open', 'arguments': {'path': str(original)}}).get('isError', False)
        created = call('tools/call', {'name': 'parameter_create', 'arguments': {'name': 'McpParameter', 'dimensions': 1, 'min': [-1, 0], 'max': [1, 1]}})
        assert not created.get('isError', False), created
        uid = created['structuredContent']['uuid']
        grid = call('tools/call', {'name': 'node_create', 'arguments': {
            'className': 'GridDeformer', 'name': 'MCP Deform Grid',
        }})
        assert not grid.get('isError', False), grid
        grid_uid = grid['structuredContent']['created'][0]['uuid']
        deformed = call('tools/call', {'name': 'binding_set_deform', 'arguments': {
            'parameter': uid, 'nodes': [grid_uid], 'key': [2, 0], 'values': [1, 2] * 4,
        }})
        assert not deformed.get('isError', False), deformed
        assert deformed['structuredContent']['updated'] == [grid_uid]
        deform_record = json.loads(call('resources/read', {
            'uri': f'resource://vsoul/parameters/{uid}',
        })['contents'][0]['text'])
        deform_binding = next(binding for binding in deform_record['bindings']
                              if binding['node'] == grid_uid and binding['param_name'] == 'deform')
        assert deform_binding['isSet'][2][0] is True
        assert deform_binding['values'][2][0] == [[1, 2]] * 4
        pitched = call('tools/call', {'name': 'ModelCommand_SetRotationXBinding', 'arguments': {
            'parameter': uid, 'nodes': [2], 'key': [2, 0], 'rotationRadians': 0.25,
        }})
        assert not pitched.get('isError', False), pitched
        yawed = call('tools/call', {'name': 'ModelCommand_SetRotationYBinding', 'arguments': {
            'parameter': uid, 'nodes': [2], 'key': [2, 0], 'rotationRadians': 0,
        }})
        assert not yawed.get('isError', False), yawed
        record = json.loads(call('resources/read', {'uri': f'resource://vsoul/parameters/{uid}'})['contents'][0]['text'])
        by_name = {binding['param_name']: binding for binding in record['bindings']}
        assert abs(by_name['transform.r.x']['values'][2][0] - 0.25) < 1e-6
        assert by_name['transform.r.y']['values'][2][0] == 0
        assert by_name['transform.r.y']['isSet'][2][0] is True
        missed = call('tools/call', {'name': 'ModelCommand_SetRotationXBinding', 'arguments': {
            'parameter': uid, 'nodes': [2], 'key': [9, 0], 'rotationRadians': 1,
        }})
        assert missed.get('isError', False), missed
        for invalid in [[], {}, {'jsonrpc': '1.0', 'id': 99, 'method': 'tools/call',
                                 'params': {'name': 'parameter_create', 'arguments': {'dimensions': 1}}}]:
            process.stdin.write(json.dumps(invalid) + '\n')
            process.stdin.flush()
            rejected = json.loads(process.stdout.readline())
            log.append(rejected)
            assert rejected['id'] is None and rejected['error']['code'] == -32600, rejected
        snapshot = call('resources/read', {'uri': 'resource://vsoul/snapshot'})
        assert 'McpParameter' in json.dumps(snapshot)
        assert len(json.loads(snapshot['contents'][0]['text'])['model']['param']) == 1
        listed = {r['uri'] for r in call('resources/list')['resources']}
        uri = f'resource://vsoul/parameters/{uid}'
        assert uri in listed
        record = call('resources/read', {'uri': uri})
        assert json.loads(record['contents'][0]['text'])['uuid'] == uid
        rendered = call('tools/call', {'name': 'render', 'arguments': {'width': 8, 'height': 8, 'bounds': [0, 0, 8, 8]}})
        assert any(c['type'] == 'image' and c['mimeType'] == 'image/png' for c in rendered['content'])
        assert call('tools/call', {'name': 'parameter_rename', 'arguments': {'id': 99999, 'name': 'No'}})['isError']
        def mcp_part(uuid):
            return {
                'uuid': uuid, 'type': 'Part', 'enabled': True, 'zsort': 0,
                'textures': [2 ** 32 - 1, 2 ** 32 - 1, 2 ** 32 - 1],
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'vertices': [[0, 0], [1, 0], [0, 1]],
                'uvs': [[0, 0], [1, 0], [0, 1]],
                'indices': [0, 1, 2],
            }
        installed = call('tools/call', {'name': 'new', 'arguments': {'model': {
            'nodes': {
                'uuid': 1, 'type': 'Node', 'enabled': True,
                'transform': {'trans': [0, 0, 0], 'rot': [0, 0, 0], 'scale': [1, 1]},
                'children': [mcp_part(4), mcp_part(5)],
            },
            'param': [], 'automation': [],
        }}})
        assert not installed.get('isError', False), installed
        welded = call('tools/call', {'name': 'part_set_weld', 'arguments': {
            'part': 4, 'target': 5, 'indices': [0, 1, 2], 'weight': 0.25,
        }})
        assert not welded.get('isError', False), welded
        weld_body = welded.get('structuredContent') or json.loads(welded['content'][0]['text'])
        assert weld_body['part'] == 4 and weld_body['target'] == 5
        cleared = call('tools/call', {'name': 'part_clear_weld', 'arguments': {'part': 4, 'target': 5}})
        assert not cleared.get('isError', False), cleared
        process.stdin.close()
        assert process.wait(timeout=30) == 0, process.stderr.read()
    finally:
        timeout.cancel()
        if process.poll() is None:
            process.kill()
            process.wait()
        (directory / 'mcp.log').write_text('\n'.join(json.dumps(v) for v in log) + '\n')
    print('PASS MCP: real stdio initialize, tools, mutations, resources, PNG image, error propagation')


def verify_cli(binary, original):
    requests = [{'op': 'open', 'path': str(original)},
                {'op': 'parameter.create', 'dimensions': 2, 'name': 'CLI'},
                {'op': 'undo'}, {'op': 'snapshot'},
                {'op': 'render', 'width': 8, 'height': 8, 'bounds': [0, 0, 8, 8]}]
    run = subprocess.run([str(binary)], input=''.join(json.dumps(v) + '\n' for v in requests),
                         text=True, capture_output=True, timeout=30)
    responses = [json.loads(line) for line in run.stdout.splitlines()]
    assert run.returncode == 0 and len(responses) == len(requests), run.stderr
    assert all(v['succeeded'] for v in responses), responses
    assert responses[3]['result']['model']['param'] == []
    assert responses[3]['result']['canRedo']
    assert decode_rgba(base64.b64decode(responses[4]['result']['png']))[:2] == (8, 8)
    print('PASS CLI: real JSONL persistent session, edits, undo, snapshot, headless PNG')


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument('--target', type=Path, default=root / 'out/rust/debug')
    parser.add_argument('--fixture-only', action='store_true')
    args = parser.parse_args()
    directory = root / 'out/rust-verification'
    directory.mkdir(parents=True, exist_ok=True)
    if not args.fixture_only:
        directory = Path(tempfile.mkdtemp(prefix='core-', dir=directory))
        print(f'Evidence: {directory}', flush=True)
    original = directory / 'fixture.inx'
    model, tail = fixture(original)
    if args.fixture_only:
        print(original)
        return
    suffix = '.dylib' if sys.platform == 'darwin' else '.dll' if os.name == 'nt' else '.so'
    library = args.target / (('' if os.name == 'nt' else 'lib') + 'soul2d_core' + suffix)
    verify_ffi(library, directory, original, model, tail)
    binary = args.target / ('soul2d-core.exe' if os.name == 'nt' else 'soul2d-core')
    verify_cli(binary, original)
    verify_mcp(binary, original, directory)


if __name__ == '__main__':
    main()
