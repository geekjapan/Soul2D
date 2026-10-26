#!/usr/bin/env python3
import argparse
import base64
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('underscore_keys', ROOT / 'tools/verify-can3-underscore-keys.py')
UNDERSCORE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(UNDERSCORE)
MODERN, KEY = UNDERSCORE.MODERN, UNDERSCORE.KEY
PNG = KEY.load('part_pixels', 'verify-part-split.py')
MODEL = ROOT / 'rust/tests/fixtures/cmo3-underscore-playback.xml'
MAP_KEYS = ['live2DPartsOpacity_PartArmA', 'live2DPartsOpacity_PartArmB']
IDS = {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid', 'curveId': 'live2DPartsOpacity_PartArmA'}


def document(snapshot):
    return {key: snapshot[key] for key in ['model', 'revision', 'modified', 'canUndo', 'canRedo']}


def edit(host, label, tool, arguments):
    revision = KEY.snapshot(host, label + '-before')['revision']
    return host.call(label, tool, {**arguments, 'expectedRevision': revision})


def attach(host, model, label):
    host.call(label, 'live2d_model_attach', {'path': str(model), 'resourceGuid': 'res-guid'})


def refuse_seek(host, label):
    before = KEY.snapshot(host, label + '-before')
    history = host.call(label + '-history', 'history_summary', {})
    response = host.call(label, 'live2d_animation_seek', {'sceneGuid': 'scene-guid', 'frame': 0}, expected_error=True)
    assert 'multiple active animation curves write the same Part' in json.dumps(response)
    assert KEY.snapshot(host, label + '-after') == before
    assert host.call(label + '-history-after', 'history_summary', {}) == history


def render_alpha(host, label):
    response = KEY.rpc(host, label, 'tools', 'call', 'render', '--json',
                       json.dumps({'width': 32, 'height': 32, 'bounds': [0, 0, 10, 10]}))
    assert not response.get('isError', False), response
    data = base64.b64decode(next(row['data'] for row in response['content'] if row['type'] == 'image'))
    (host.run / (label + '.png')).write_bytes(data)
    width, height, pixels = PNG.decode_png(data)
    assert (width, height) == (32, 32)
    return max(pixels[3::4])


def exercise(host, label, populated=True):
    before = document(KEY.snapshot(host, label + '-before'))
    history = host.call(label + '-history', 'history_summary', {})
    for frame, opacity in [(0, 0.0), (10, 1.0), (1, 0.1)]:
        host.call(f'{label}-seek-{frame}', 'live2d_animation_seek', {'sceneGuid': 'scene-guid', 'frame': frame})
        snapshot = KEY.snapshot(host, f'{label}-state-{frame}')
        active = snapshot['nativePlayback']['active']
        assert active['partOpacities'] == ({'part-guid-a': opacity} if populated else {})
        assert active['poseSignals'] == ({'root': opacity} if populated else {})
        assert not {'root', 'PartArmA', 'PartArmB'} & snapshot['values'].keys()
        assert document(snapshot) == before
        assert not any('underscore' in warning for warning in snapshot['nativePlayback']['warnings'])
    host.call(label + '-seek-zero', 'live2d_animation_seek', {'sceneGuid': 'scene-guid', 'frame': 0})
    assert render_alpha(host, label + '-zero') == (0 if populated else 255)
    host.call(label + '-advance', 'live2d_animation_advance', {'dt': 1 / 60, 'loop': False})
    advanced = KEY.snapshot(host, label + '-advanced')['nativePlayback']['active']
    host.call(label + '-seek-one', 'live2d_animation_seek', {'sceneGuid': 'scene-guid', 'frame': 1})
    assert KEY.snapshot(host, label + '-one')['nativePlayback']['active'] == advanced
    host.call(label + '-seek-full', 'live2d_animation_seek', {'sceneGuid': 'scene-guid', 'frame': 10})
    assert render_alpha(host, label + '-full') == 255
    assert document(KEY.snapshot(host, label + '-final')) == before
    assert host.call(label + '-history-final', 'history_summary', {}) == history


def main():
    parser = argparse.ArgumentParser(description='Verify exact underscore PartOpacity binding/playback over production HTTP MCP; generated CAFF only, no UI automation.')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    evidence = ROOT / 'build/verification/issue145-underscore-playback'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    modules = [Path(__file__), Path(UNDERSCORE.__file__), Path(UNDERSCORE.LEGACY.__file__), Path(MODERN.__file__),
               Path(KEY.__file__), Path(KEY.DRIVER.__file__), Path(KEY.CAFF.__file__), Path(PNG.__file__)]
    paths = set((ROOT / 'rust/src').rglob('*.rs')) | set(modules) | {
        MODEL, UNDERSCORE.FIXTURE, ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(path.relative_to(ROOT)): UNDERSCORE.digest(path) for path in sorted(paths)}
    inputs = {str(path): UNDERSCORE.digest(path) for path in [core, njc]}
    result = {'status': 'FAIL', 'scope': 'Generated underscore CAFF exact-GUID/map-key playback, empty channels, fail-closed collision, render and native save/reopen',
              'sourceHashes': hashes, 'inputHashes': inputs, 'nativeGui': 'NOT_RUN', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        model = run / 'model.cmo3'
        animation = run / 'original.can3'
        model.write_bytes(KEY.caff_pack(MODEL.read_bytes()))
        original = KEY.caff_pack(UNDERSCORE.FIXTURE.read_bytes())
        animation.write_bytes(original)
        owned = {str(path): UNDERSCORE.digest(path) for path in [model, animation]}
        host = KEY.DRIVER.Host(run, core, njc, KEY.DRIVER.free_port())
        host.wait_ready()
        host.call('open-original', 'open', {'path': str(animation)})
        report = host.call('bindings', 'live2d_animation_bindings', {'modelPath': str(model), 'resourceGuid': 'res-guid'})
        rows = report['underscorePartBindings']
        assert [row['mapKey'] for row in rows] == MAP_KEYS
        assert [row['partId'] for row in rows] == ['root', 'root']
        assert [row['sourcePart']['value'] for row in rows] == ['PartArmA', 'PartArmB']
        assert rows[0]['curveId'] == rows[1]['curveId'] == IDS['curveId']
        attach(host, model, 'attach-both')
        refuse_seek(host, 'both-populated')
        for index, key in enumerate(MAP_KEYS):
            label = f'empty-{index}'
            host.call(label + '-open', 'open', {'path': str(animation)})
            for frame in [0, 10]:
                edit(host, f'{label}-delete-{frame}', 'live2d_animation_key_delete',
                     {**IDS, 'partOpacityMapKey': key, 'anchorFrame': frame})
            attach(host, model, label + '-attach')
            exercise(host, label)
            saved = run / (label + '.can3')
            host.call(label + '-save', 'save', {'path': str(saved)})
            MODERN.require_preservation(original, saved.read_bytes(), storage=['part-a', 'part-b'][index], retained_points=())
            host.call(label + '-reopen', 'open', {'path': str(saved)})
            attach(host, model, label + '-reattach')
            exercise(host, label + '-reopened')
        for frame in [0, 10]:
            edit(host, f'empty-both-delete-{frame}', 'live2d_animation_key_delete',
                 {**IDS, 'partOpacityMapKey': MAP_KEYS[0], 'anchorFrame': frame})
        attach(host, model, 'empty-both-attach')
        exercise(host, 'empty-both', populated=False)
        assert {str(path): UNDERSCORE.digest(path) for path in [model, animation]} == owned
        result.update(status='PASS', generatedInputHashes=owned, bindings=rows,
                      scenarios=['both-populated-refused', 'first-empty', 'first-empty-reopened',
                                 'second-empty', 'second-empty-reopened', 'both-empty'])
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode, 'running': host.child.poll() is None}
                assert result['cleanup']['running'] is False
            assert {str(path.relative_to(ROOT)): UNDERSCORE.digest(path) for path in sorted(paths)} == hashes
            assert {str(path): UNDERSCORE.digest(path) for path in [core, njc]} == inputs
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
