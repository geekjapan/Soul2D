#!/usr/bin/env python3
"""Verify omitted CAN3 source metadata, strict bindings and real key persistence."""
import argparse
import copy
import json
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import importlib.util


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


CAN = module('empty_can_source', ROOT / 'tools/verify-can3-source.py')
BIND = module('empty_can_binding', ROOT / 'tools/verify-can3-bindings.py')
CMO = CAN.CMO


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('animation', type=Path)
    parser.add_argument('model', type=Path)
    parser.add_argument('--target', type=Path, default=ROOT / 'out/rust/debug')
    args = parser.parse_args()
    animation, model = args.animation.resolve(), args.model.resolve()
    binary = args.target.resolve() / 'soul2d-core'
    library = args.target.resolve() / ('libsoul2d_core.dylib' if sys.platform == 'darwin' else 'libsoul2d_core.so')
    original = animation.read_bytes()
    original_hash, model_hash = CMO.sha(animation), CMO.sha(model)
    entries, xml = CMO.CAFF.decode(original)
    guid = BIND.resource_guid(original)
    expected_bindings = BIND.binding_oracle(original, model.read_bytes(), guid)
    missing = [b for b in expected_bindings['bindings'] if b['sourceParameterStatus'] == 'missing']
    assert missing and all(b['sourceParameter'] is None for b in missing)
    run = Path(tempfile.mkdtemp(prefix='empty-can3-', dir=ROOT / 'build/verification'))
    expected = None
    for transport in ('ffi', 'jsonl', 'mcp'):
        core = CMO.CORE.Core(library) if transport == 'ffi' else None
        saved = run / (transport + '-saved.can3')
        requests = [
            {'op': 'live2d.animation.read', 'path': str(animation)},
            {'op': 'live2d.animation.bindings', 'path': str(animation), 'modelPath': str(model), 'resourceGuid': guid},
            {'op': 'open', 'path': str(animation)}, {'op': 'save', 'path': str(saved)},
            {'op': 'open', 'path': str(saved)}, {'op': 'live2d.animation.read'}]
        try:
            replies, wire = CAN.EDIT.run_transport(binary, core, requests, transport)
        finally:
            if core:
                core.close()
        (run / (transport + '.log')).write_text(wire)
        assert len(replies) == len(requests) and all(r['succeeded'] for r in replies), replies
        actual = replies[0]['result']
        CAN.check_xml(actual, xml)
        if expected is None:
            expected = actual
        assert actual == expected and replies[-1]['result'] == expected
        assert replies[1]['result'] == expected_bindings and CMO.sha(saved) == original_hash
    core = CMO.CORE.Core(library)
    try:
        opened = core.request('open', path=str(animation))
        scene, track, curve = next((s, t, c) for s in opened['model']['scenes']
            for t in s['tracks'] for c in t['parameterCurves']
            if c['sourceParameter'] is None and c['points'])
        point = curve['points'][0]
        value = max(p['anchor']['value'] for p in curve['points']) + 1.125
        request = {'sceneGuid': scene['guid'], 'trackGuid': track['guid'], 'curveId': curve['id'],
            'anchorFrame': point['anchor']['frame'], 'value': value, 'expectedRevision': opened['revision']}
        edited = core.request('live2d.animation.key.set', **request)
        wanted = copy.deepcopy(opened['model'])
        selected = next(p for s in wanted['scenes'] if s['guid'] == scene['guid']
            for t in s['tracks'] if t['guid'] == track['guid']
            for c in t['parameterCurves'] if c['id'] == curve['id']
            for p in c['points'] if p['anchor']['frame'] == point['anchor']['frame'])
        selected['anchor']['value'] = value
        assert edited['model'] == wanted and edited['modified'] and edited['canUndo']
        core.request('live2d.animation.key.set', success=False, **request)
        assert core.request('snapshot') == edited
        assert core.request('undo')['model'] == opened['model']
        assert core.request('redo')['model'] == wanted
        saved = run / 'edited.can3'
        core.request('save', path=str(saved))
        reopened = core.request('open', path=str(saved))
        assert reopened['model'] == wanted
        changed_tokens = CAN.check_key_saved(original, saved.read_bytes(), request, value)
        assert changed_tokens > 0
        _, changed_xml = CMO.CAFF.decode(saved.read_bytes())
        CAN.check_xml(wanted, changed_xml)
        assert BIND.binding_oracle(saved.read_bytes(), model.read_bytes(), guid) == expected_bindings
        assert core.request('live2d.animation.bindings', modelPath=str(model), resourceGuid=guid) == expected_bindings
    finally:
        core.close()
    for name in ('wrong-count', 'nonempty-unknown-key', 'wrong-map-type'):
        graph = CMO.XMLGraph(xml)
        options = next(graph.required(attr, 'optionParam')
            for attr in graph.root.iter('CMvAttrF')
            if graph.required(attr, 'id').get('idstr', '').startswith('live2dParam_')
            and graph.required(attr, 'optionParam').get('count') == '0'
            and not len(graph.required(attr, 'optionParam')))
        if name == 'wrong-count':
            options.set('count', '1')
        elif name == 'nonempty-unknown-key':
            options.set('count', '1')
            CMO.ET.SubElement(options, 's', {'xs.n': 'UNKNOWN_KEY'}).text = 'no inferred identity'
        else:
            options.tag = 'null'
        altered = CMO.ET.tostring(graph.root, encoding='utf-8')
        path = run / (name + '.can3')
        path.write_bytes(CMO.repack_main_xml(original, entries, altered))
        actual, wire = CMO.jsonl_request(binary, 'live2d.animation.read', path)
        (run / (name + '.log')).write_text(wire)
        assert not actual['succeeded'], name
    assert CMO.sha(animation) == original_hash and CMO.sha(model) == model_hash
    CMO.write_json(run / 'result.json', {'status': 'PASS', 'source': str(animation), 'model': str(model),
        'sourceSha256': original_hash, 'modelSha256': model_hash, 'binarySha256': CMO.sha(binary),
        'librarySha256': CMO.sha(library), 'missingMetadataBindings': len(missing),
        'transports': ['FFI', 'JSONL', 'MCP'], 'byteExactSaveReopen': True,
        'keyEditUndoRedoSaveReopen': True, 'malformedControls': 3,
        'editedScalarTokens': changed_tokens,
        'limits': ['Source and identity checks; GUI and Cubism playback parity unverified']})
    print(run)
    print('missing metadata, strict bindings and key persistence PASS')


if __name__ == '__main__':
    main()
