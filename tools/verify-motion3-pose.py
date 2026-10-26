#!/usr/bin/env python3
"""Verify mapped real Motion3 curves drive INX transforms, deformation and PNG output."""
import argparse
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PRO_ORACLE = ('hiyori_ja/hiyori_pro/runtime/motion/hiyori_m01.motion3.json',
              1, 0.3535, 2.97688315714495432748)


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def verify(peer, label, fixtures, run, raw, helper):
    source, saved = run / f'{label}-source.inx', run / f'{label}-saved.inx'
    model, tail = helper.fixture(source)
    model['param'] = [{'uuid': 10, 'name': 'Mapped curve', 'is_vec2': False,
                      'min': [-40, 0], 'max': [40, 1], 'defaults': [0, 0],
                      'axis_points': [[0, 1], [0]], 'merge_mode': 'Additive', 'bindings': [
                          {'node': 2, 'param_name': 'transform.t.y', 'interpolate_mode': 'Linear',
                           'values': [[-4], [4]], 'isSet': [[True], [True]]},
                          {'node': 2, 'param_name': 'deform', 'interpolate_mode': 'Linear',
                           'values': [[[[-4, 0]] * 4], [[[4, 0]] * 4]], 'isSet': [[True], [True]]}]}]
    encoded = json.dumps(model, separators=(',', ':')).encode()
    source.write_bytes(b'TRNSRTS\0' + len(encoded).to_bytes(4, 'big') + encoded + tail)
    original = source.read_bytes()
    peer.request('open', path=str(source))
    names = set()
    for index, (name, curve, seconds, expected) in enumerate([*raw.ORACLES[:4], PRO_ORACLE]):
        motion = json.loads((fixtures / name).read_text())
        if name not in names:
            peer.request('motion3.import', name=name, path=str(fixtures / name))
            bindings = [{'id': motion['Curves'][curve]['Id'], 'parameter': 10, 'axis': 0, 'future': 'keep'}]
            peer.request('motion3.bind', name=name, bindings=bindings)
            assert peer.request('motion3.bindings', name=name) == bindings
            names.add(name)
        before = peer.request('snapshot')
        pose = peer.request('motion3.pose', name=name, time=seconds)
        assert abs(pose['values']['10'][0] - expected) < 1e-10
        assert len(pose['unmapped']) == len(motion['Curves']) - 1
        assert peer.request('snapshot') == before
        peer.request('motion3.seek', name=name, time=seconds)
        active = peer.request('snapshot')
        assert active['model'] == before['model'] and active['revision'] == before['revision']
        assert abs(active['values']['10'][0] - expected) < 1e-10
        scene = peer.request('scene.evaluate')
        offset = expected / 10
        for pair in scene['localOffsets']['2']:
            assert abs(pair[0] - offset) < 1e-10 and pair[1] == 0
        bounds = peer.request('bounds')['bounds']
        (run / f'{label}-{index}.json').write_text(json.dumps({'name': name, 'time': seconds,
            'expected': expected, 'pose': pose, 'snapshot': active, 'scene': scene, 'bounds': bounds}, indent=2))
        # Bounds include the renderer's ten-percent framing margin around the four-unit card.
        assert all(abs(a - b) < 1e-10 for a, b in zip(bounds, [1.6+offset, 1.6+offset, 6.4+offset, 6.4+offset])), (bounds, offset)
        png = base64.b64decode(peer.request('render', width=64, height=64, bounds=[-4, -4, 12, 12])['png'], validate=True)
        width, height, pixels = helper.decode_rgba(png)
        x = int((8 + offset) * 4)
        assert (width, height) == (64, 64) and pixels[(x * width + x) * 4:(x * width + x) * 4 + 4] == bytes([255,0,0,255])
        (run / f'{label}-{index}.png').write_bytes(png)
        peer.request('physics.step', dt=0)
        assert abs(peer.request('snapshot')['values']['10'][0] - expected) < 1e-10
        checkpoint = peer.request('snapshot')
        peer.request('motion3.seek', name=name, time=-1, success=False)
        peer.request('animation.reset', success=False)
        assert peer.request('snapshot') == checkpoint
        peer.request('motion3.reset')
        assert peer.request('snapshot') == before
    before = peer.request('snapshot')
    peer.request('save', path=str(saved))
    saved_model, saved_tail = helper.split_container(saved.read_bytes())
    assert saved_model == before['model'] and saved_tail == tail
    peer.request('open', path=str(saved))
    name, curve, seconds, expected = raw.ORACLES[0]
    peer.request('motion3.seek', name=name, time=seconds)
    assert abs(peer.request('snapshot')['values']['10'][0] - expected) < 1e-10
    peer.request('parameter.delete', id=10)
    assert all(value == [] for value in peer.request('snapshot')['model']['live2d']['motionBindings'].values())
    peer.request('undo')
    assert peer.request('snapshot')['model'] == saved_model
    peer.request('motion3.seek', name=name, time=seconds)
    assert abs(peer.request('snapshot')['values']['10'][0] - expected) < 1e-10
    assert source.read_bytes() == original
    return {'passed': True, 'samples': 5, 'sourceModels': 4,
            'limitations': ['Mapped INX pose only; not imported Cubism geometry, CMO authoring, fades or blending']}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('fixtures', type=Path)
    fixtures = parser.parse_args().fixtures.resolve()
    raw = module('motion_raw', ROOT / 'tools/verify-motion3.py')
    helper = module('core_fixture', ROOT / 'tools/verify-rust-core.py')
    files = sorted(fixtures.rglob('*.motion3.json'))
    assert len(files) == 44
    hashes = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}
    run = Path(tempfile.mkdtemp(prefix='motion3-pose-', dir=ROOT / 'build/verification'))
    print(run, flush=True)
    library = ROOT / 'out/rust/debug' / ('libsoul2d_core.dylib' if raw.sys.platform == 'darwin' else 'libsoul2d_core.so')
    artifacts = [library, ROOT / 'out/rust/debug/soul2d-core']
    provenance = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in artifacts}
    results = {}
    core = helper.Core(library)
    try:
        results['ffi'] = verify(core, 'ffi', fixtures, run, raw, helper)
    finally:
        core.close()
    peer = raw.Mcp(run)
    try:
        peer.initialize()
        tools = {item['name'] for item in peer.rpc('tools/list', {})['tools']}
        assert {'motion3_bind', 'motion3_bindings', 'motion3_pose', 'motion3_seek', 'motion3_advance', 'motion3_reset'} <= tools
        results['mcp'] = verify(peer, 'mcp', fixtures, run, raw, helper)
    finally:
        peer.close()
    assert all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest for path, digest in {**hashes, **provenance}.items())
    results.update(passed=True, sourceHashes=hashes, artifactHashes=provenance)
    (run / 'result.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'passed': True, 'ffi': results['ffi'], 'mcp': results['mcp']}), flush=True)


if __name__ == '__main__':
    main()
