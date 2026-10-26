#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile
import traceback
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('parameter_curves_proof', ROOT / 'tools/verify-can3-curves.py')
CURVES = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CURVES)
KEY = CURVES.KEYPROOF
FIXTURE = ROOT / 'rust/tests/fixtures/can3-part-opacity.xml'
IDS = {'sceneGuid': 'scene-guid', 'trackGuid': 'track-guid'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cmo3_xml():
    refs, definitions = '', ''
    for part, guid in [('PartNewA', 'new-a'), ('PartNewB', 'new-b'),
                       ('PartArmA', 'part-guid-a'), ('PartArmB', 'part-guid-b')]:
        refs += f"<CPartSource xs.ref='{part}'/>"
        definitions += (f"<CPartSource xs.id='{part}'><CPartGuid xs.n='guid' uuid='{guid}'/>"
                        f"<CPartId xs.n='id' idstr='{part}'/><carray_list xs.n='keyforms' count='0'/></CPartSource>")
    return CURVES.cmo3_xml().replace("count='1'><CPartSource xs.ref='part'/>",
                                    f"count='5'><CPartSource xs.ref='part'/>{refs}") \
        .replace('</root>', definitions + '</root>')


def require_addition_only(original, saved):
    CURVES.caff_bytes_equal(original, saved)
    old = KEY.decode_entries(original)[1].decode()
    new = KEY.decode_entries(saved)[1].decode()
    tree = ET.fromstring(new)
    patches = []
    for part, guid in [('PartNewA', 'new-a'), ('PartNewB', 'new-b')]:
        id_node = next(n for n in tree.iter('CAttrId') if n.get('idstr') == 'live2DPartsOpacity_' + part)
        attribute = next(n for n in tree.iter('CMvAttrF') if id_node in list(n.iter()))
        storage = attribute.get('xs.id')
        assert storage and sum(n.get('xs.ref') == storage for n in tree.iter()) == 3
        assert next(n for n in attribute.iter('CPartGuid')).get('uuid') == guid
        assert next(n for n in attribute.iter('CPartId')).attrib == {
            'xs.n': 'KEY_PARTS_VISIBLE_ID', 'idstr': part}
        for field in ['curMin', 'curMax']:
            assert next(n for n in attribute.iter() if n.get('xs.n') == field).text == '0.6'
        span = CURVES.element_span(new, f'<CMvAttrF xs.id="{storage}">')
        patches.append((*span, ''))
        entry = re.search(r'<entry><CAttrId xs.n="key" idstr="live2DPartsOpacity:' + part
                          + r'"/><CMvAttrF xs.n="value" xs.ref="' + re.escape(storage) + r'"/></entry>', new)
        assert entry
        patches.append((entry.start(), entry.end(), ''))
        member = re.search(r'<CMvAttrF xs.ref="' + re.escape(storage) + r'"/>', new)
        assert member
        patches.append((member.start(), member.end(), ''))
    for name, before, after in [('attrMap', '3', '5'), ('attrList', '2', '4')]:
        match = re.search(r'<(?:hash_map|array) xs.n="' + name + r'" count="(' + after + r')"', new)
        assert match
        patches.append((match.start(1), match.end(1), before))
    for start, end, replacement in sorted(patches, reverse=True):
        new = new[:start] + replacement + new[end:]
    assert new == old, 'XML outside added Part definitions, map/list members and counts changed'


def main():
    parser = argparse.ArgumentParser(description='Verify modern CAN3 PartOpacity curve CRUD through owned HTTP MCP.')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    args = parser.parse_args()
    assert __debug__, 'verification requires assertions'
    core, njc = args.core.resolve(), args.njc.resolve()
    evidence = ROOT / 'build/verification/ani145-part-curves'
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='mcp-', dir=evidence))
    sources = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(CURVES.__file__), Path(KEY.__file__), Path(KEY.DRIVER.__file__),
        Path(KEY.CAFF.__file__), Path(KEY.CAFF.CORE.__file__), FIXTURE,
        ROOT / 'rust/tests/core.rs', ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/commands.manifest.json', ROOT / 'doc/live2d-authoring.md',
        ROOT / '.cursor/skills/operate-soul2d/families/live2d-animation.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)}
    binaries = {str(p): digest(p) for p in (core, njc)}
    result = {'status': 'FAIL', 'scope': 'Generated modern PartOpacity curve CRUD, source preservation and MCP history',
              'sourceHashes': hashes, 'binaries': binaries, 'nativeGui': 'SKIP', 'vendorAcceptance': 'NOT_RUN'}
    host = None
    try:
        source, cmo3, saved = run / 'source.can3', run / 'source.cmo3', run / 'saved.can3'
        original = KEY.caff_pack(FIXTURE.read_bytes())
        source.write_bytes(original)
        model_bytes = KEY.caff_pack(cmo3_xml().encode())
        cmo3.write_bytes(model_bytes)
        host = CURVES.DRIVER.Host(run, core, njc, CURVES.DRIVER.free_port())
        host.wait_ready()
        result['hostPid'] = host.child.pid
        tools = {tool['name']: tool for tool in host.tools()}
        manifest = {tool['id']: tool for tool in json.loads((ROOT / 'doc/commands.manifest.json').read_text())['commands']}
        for name, schema in CURVES.EXPECTED_SCHEMAS.items():
            assert tools[name]['inputSchema'] == manifest[name]['inputSchema'] == schema
        host.call('open', 'open', {'path': str(source)})
        initial, model = KEY.state(host, 'initial')
        history = host.call('before-explicit-read-history', 'history_summary', {})
        authoring = host.call('explicit-cmo3-read', 'live2d_authoring_read', {'path': str(cmo3)})
        assert authoring['format'] == 'live2d.authoring.source.v1'
        assert any(item['data']['type'] == 'Part' for item in authoring['sources'])
        assert KEY.state(host, 'after-explicit-read') == (initial, model)
        for label, arguments in [('no-path', {}), ('missing-path', {'path': str(run / 'missing.cmo3')}),
                                 ('can3-path', {'path': str(source)})]:
            host.call('refuse-authoring-read-' + label, 'live2d_authoring_read', arguments, expected_error=True)
            assert KEY.state(host, 'after-authoring-read-' + label) == (initial, model)
        assert host.call('after-explicit-read-history', 'history_summary', {}) == history
        assert source.read_bytes() == original and cmo3.read_bytes() == model_bytes
        result['explicitCmo3ReadWhileCan3Active'] = 'PASS'
        for part in ['PartNewA', 'PartNewB']:
            before = model
            _, model = KEY.edit(host, 'add-' + part, 'live2d_animation_curve_add', **IDS,
                                modelPath=str(cmo3), partId=part)
            host.call('undo-' + part, 'undo', {})
            assert KEY.state(host, 'undone-' + part)[1] == before
            host.call('redo-' + part, 'redo', {})
            assert KEY.state(host, 'redone-' + part)[1] == model
            _, model = KEY.edit(host, 'key-' + part, 'live2d_animation_key_add', **IDS,
                                curveId='live2DPartsOpacity_' + part, anchorFrame=5, value=0.6)
        host.call('save-additions', 'save', {'path': str(saved)})
        require_addition_only(original, saved.read_bytes())
        for arguments, message in [
            ({'partId': 'PartNewA'}, 'already exists'),
            ({'partId': 'missing'}, 'not found'),
            ({'partId': 'root', 'parameterId': 'ParamNew'}, 'exactly one'),
            ({}, 'exactly one'),
            ({'partId': 'root', 'expectedRevision': initial['revision']}, 'revision conflict'),
        ]:
            KEY.refuse(host, 'refuse-' + str(len(arguments)) + '-' + message, 'live2d_animation_curve_add',
                       {**IDS, 'modelPath': str(cmo3), **arguments}, message)
        for part in ['PartNewA', 'PartNewB']:
            _, model = KEY.edit(host, 'delete-' + part, 'live2d_animation_curve_delete', **IDS,
                                curveId='live2DPartsOpacity_' + part)
        host.call('save-cancelled', 'save', {'path': str(saved)})
        assert saved.read_bytes() == original, 'cumulative additions/deletions did not cancel byte-exactly'
        for part in ['PartArmA', 'PartArmB']:
            _, model = KEY.edit(host, 'delete-' + part, 'live2d_animation_curve_delete', **IDS,
                                curveId='live2DPartsOpacity_' + part)
        for part in ['PartArmB', 'PartArmA']:
            _, model = KEY.edit(host, 'readd-' + part, 'live2d_animation_curve_add', **IDS,
                                modelPath=str(cmo3), partId=part)
        CURVES.save_and_reopen(host, 'readded', saved, model, original)
        xml = KEY.decode_entries(saved.read_bytes())[1].decode()
        assert '<s xs.n="KEY_PARTS_VISIBLE_ID">PartArmA</s>' in xml
        assert '<CPartId xs.n="KEY_PARTS_VISIBLE_ID" idstr="PartArmB"/>' in xml
        assert 'keep-part-a' in xml and 'keep-sequence-part-b' in xml
        assert source.read_bytes() == original and cmo3.read_bytes() == model_bytes
        result['status'] = 'PASS'
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode,
                                     'running': host.child.poll() is None}
                assert not result['cleanup']['running']
            assert {str(p): digest(p) for p in (core, njc)} == binaries
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)} == hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


if __name__ == '__main__':
    main()
