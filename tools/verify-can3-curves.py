#!/usr/bin/env python3
import argparse
import copy
import hashlib
import importlib.util
import json
import re
import sys
import tempfile
import traceback
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "rust" / "tests" / "fixtures" / "can3-curve-editor.xml"

EXPECTED_SCHEMAS = {
    "live2d_animation_curve_add": {"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"modelPath":{"type":"string"},"parameterId":{"type":"string"},"partId":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","modelPath","expectedRevision"],"oneOf":[{"required":["parameterId"]},{"required":["partId"]}],"additionalProperties":False},
    "live2d_animation_curve_delete": {"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"curveId":{"type":"string"},"partOpacityMapKey":{"type":"string","minLength":1},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","curveId","expectedRevision"],"additionalProperties":False},
}


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DRIVER = load("physics_bake_proof", "verify-animation-physics-bake.py")
KEYPROOF = load("key_proof", "verify-can3-key-structure.py")
caff_pack = KEYPROOF.caff_pack
decode_entries = KEYPROOF.decode_entries
state = KEYPROOF.state
edit = KEYPROOF.edit


def refuse(host, label, tool, arguments, message):
    files = {p.name: p.read_bytes() for p in host.run.glob("*.cmo3")}
    response = KEYPROOF.refuse(host, label, tool, arguments, message)
    assert {p.name: p.read_bytes() for p in host.run.glob("*.cmo3")} == files, \
        f"{label}: source CMO3 bytes changed"
    return response


def cmo3_bytes():
    return caff_pack(cmo3_xml().encode())


def cmo3_xml():
    return """<root fileFormatVersion='401000004'><CModelSource>
      <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
      <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='3'>
        <CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/><CParameterId xs.n='id' idstr='ParamTest'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource>
        <CParameterSource><CParameterGuid xs.n='guid' uuid='new-parameter-guid'/><CParameterId xs.n='id' idstr='ParamNew'/><f xs.n='minValue'>-2</f><f xs.n='maxValue'>2</f><f xs.n='defaultValue'>0.25</f></CParameterSource>
        <CParameterSource><CParameterGuid xs.n='guid' uuid='keep-parameter-guid'/><CParameterId xs.n='id' idstr='ParamKeep'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource>
      </carray_list></CParameterSourceSet>
      <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='0'/></CDrawableSourceSet>
      <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/></CDeformerSourceSet>
      <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet>
      <CPartSource xs.n='rootPart' xs.ref='part'/>
    </CModelSource>
    <CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource></root>"""


def curve_ids(model, track):
    return [c["id"] for c in model["scenes"][0]["tracks"][track]["parameterCurves"]]


def curves(model, track):
    return model["scenes"][0]["tracks"][track]["parameterCurves"]


def element_span(text, needle):
    start = text.index(needle)
    open_end = text.index(">", start)
    if text[open_end - 1] == "/":
        return start, open_end + 1
    name = needle[1:].split(" ")[0]
    close = text.index(f"</{name}>", open_end)
    return start, close + len(f"</{name}>")


def require_add_oracle(original_xml, saved_xml, storage_id):
    text = saved_xml.decode()
    original = original_xml.decode()
    ref_start, ref_end = element_span(
        text, f'<CMvAttrF xs.ref="{storage_id}"/>')
    text = text[:ref_start] + text[ref_end:]
    def_start, def_end = element_span(
        text, f'<CMvAttrF xs.id="{storage_id}">')
    text = text[:def_start] + text[def_end:]
    list_start = text.index('xs.n="attrList"')
    list_start = text.rindex("<", 0, list_start)
    list_end = text.index(">", list_start)
    original_list_start = original.index('xs.n="attrList"')
    original_list_start = original.rindex("<", 0, original_list_start)
    original_list_end = original.index(">", original_list_start)
    count = re.search(r'\bcount\s*=\s*["\']([^"\']*)["\']', text[list_start:list_end])
    original_count = re.search(r'\bcount\s*=\s*["\']([^"\']*)["\']',
                               original[original_list_start:original_list_end])
    assert int(count.group(1)) == int(original_count.group(1)) + 1, \
        "expected saved count to be original + 1"
    if count.group(1) != original_count.group(1):
        value_start = list_start + count.start(1)
        value_end = list_start + count.end(1)
        text = text[:value_start] + original_count.group(1) + text[value_end:]
    assert text == original, "saved XML differs after removing only the new curve spans"


def require_delete_oracle(original_xml, saved_xml, owner_ref, definition_id):
    original = original_xml.decode()
    ref_start, ref_end = element_span(original, owner_ref)
    list_start = original.rindex("<carray_list", 0, ref_start)
    list_end = original.index(">", list_start)
    original_count = re.search(r'\bcount\s*=\s*["\']([^"\']*)["\']',
                               original[list_start:list_end])
    def_start, def_end = element_span(original, f'<CMvAttrF xs.id="{definition_id}">')
    count_start = list_start + original_count.start(1)
    count_end = list_start + original_count.end(1)
    patches = [
        (ref_start, ref_end, ""),
        (def_start, def_end, ""),
        (count_start, count_end, str(int(original_count.group(1)) - 1)),
    ]
    expected = original
    for start, end, replacement in sorted(patches, reverse=True):
        expected = expected[:start] + replacement + expected[end:]
    assert saved_xml.decode() == expected, \
        "saved XML differs beyond the owned definition, reference and count value"


def caff_bytes_equal(a, b):
    assert a[:58] == b[:58], "CAFF header changed"
    assert a[-2:] == b[-2:] == b"bc", "CAFF trailer changed"
    original_entries, _ = decode_entries(a)
    saved_entries, _ = decode_entries(b)
    assert [entry[:4] for entry in original_entries] == \
        [entry[:4] for entry in saved_entries], "CAFF entry metadata changed"
    for (names, _, _, _, payload) in original_entries:
        if names[0] != "main.xml":
            saved_match = next(entry for entry in saved_entries if entry[0] == names)
            assert saved_match[4] == payload, f"non-main entry {names[0]} changed"


def save_and_reopen(host, label, path, expected_model, template):
    host.call(f"{label}-save", "save", {"path": str(path)})
    data = path.read_bytes()
    caff_bytes_equal(template, data)
    host.call(f"{label}-reopen", "open", {"path": str(path)})
    _, model = state(host, f"{label}-persisted")
    assert model == expected_model, f"{label}: complete model changed after save/reopen"
    return data


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--njc", type=Path, required=True)
    args = parser.parse_args()
    evidence = ROOT / "build" / "verification" / "ani145-curves"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    result = {"passed": False,
              "scope": "Synthetic CAN3 parameter-curve HTTP MCP; native GUI SKIP; vendor acceptance NOT_RUN; local only",
              "run": str(run), "checks": []}
    host = None
    try:
        manifest = json.loads((ROOT / "doc" / "commands.manifest.json").read_text())
        paths = [args.core, args.njc, FIXTURE, Path(__file__),
                 ROOT / "tools" / "verify-can3-key-structure.py",
                 ROOT / "tools" / "verify-animation-physics-bake.py",
                 ROOT / "tools" / "verify-caff-authoring.py",
                 ROOT / "rust" / "src" / "animation_source.rs", ROOT / "rust" / "src" / "lib.rs",
                 ROOT / "rust" / "src" / "mcp.rs", ROOT / "doc" / "commands.manifest.json"]
        result["inputHashes"] = {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                                 for path in paths}
        host = DRIVER.Host(run, args.core.resolve(), args.njc.resolve(), DRIVER.free_port())
        host.wait_ready()
        result["hostPid"] = host.child.pid

        tools = host.tools()
        (run / "tools-list.json").write_text(json.dumps(tools, indent=2) + "\n")
        manifest_tools = {tool["id"]: tool for tool in manifest["commands"]}
        for name, expected in EXPECTED_SCHEMAS.items():
            live = next(tool for tool in tools if tool["name"] == name)
            entry = manifest_tools[name]
            assert live["inputSchema"] == expected == entry["inputSchema"], (name, live)
            assert live["description"] == entry["description"], name
            assert expected["additionalProperties"] is False
            assert expected["properties"]["expectedRevision"] == \
                {"type": "integer", "minimum": 0}, expected
            assert "expectedRevision" in expected["required"]

        can3 = run / "curves.can3"
        original_can3 = caff_pack(FIXTURE.read_bytes())
        can3.write_bytes(original_can3)
        cmo3 = run / "source.cmo3"
        cmo3.write_bytes(cmo3_bytes())
        host.call("open", "open", {"path": str(can3)})
        snap, model = state(host, "opened")
        ids = {"sceneGuid": model["scenes"][0]["guid"],
               "trackGuid": model["scenes"][0]["tracks"][0]["guid"],
               "track2Guid": model["scenes"][0]["tracks"][1]["guid"],
               "curveId": model["scenes"][0]["tracks"][0]["parameterCurves"][0]["id"]}
        assert ids == {"sceneGuid": "scene-guid", "trackGuid": "track-guid",
                       "track2Guid": "track2-guid", "curveId": "live2dParam_ParamTest"}, ids
        assert curve_ids(model, 0) == ["live2dParam_ParamTest"]
        assert curve_ids(model, 1) == ["live2dParam_ParamTest"]

        base = {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "modelPath": str(cmo3)}

        _, model = edit(host, "add", "live2d_animation_curve_add",
                        **base, parameterId="ParamNew")
        assert curve_ids(model, 0) == ["live2dParam_ParamTest", "live2dParam_ParamNew"]
        curve = curves(model, 0)[1]
        assert curve["parameterGuid"] == "new-parameter-guid"
        assert curve["sourceParameter"] == {"type": "CParameterId", "value": "ParamNew"}
        assert curve["range"] == [-2.0, 2.0]
        assert curve["points"] == [] and curve["curveTypes"] == []
        storage = "soul2d-curve-track-guid-live2dParam_ParamNew"

        _, model = edit(host, "key0", "live2d_animation_key_add",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=0, value=-1.5)
        _, model = edit(host, "key1", "live2d_animation_key_add",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=5, value=0.5)
        _, model = edit(host, "key2", "live2d_animation_key_add",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=12, value=1.25, type="SMOOTH")

        snap, _ = state(host, "pre-undo")
        position = snap["history"]["position"]
        host.call("undo", "history_goto",
                  {"index": position - 1, "expectedRevision": snap["revision"]})
        snap, model = state(host, "undo")
        assert len(curves(model, 0)[1]["points"]) == 2
        host.call("redo", "history_goto",
                  {"index": position, "expectedRevision": snap["revision"]})
        snap, model = state(host, "redo")
        assert len(curves(model, 0)[1]["points"]) == 3

        _, model = edit(host, "factory-key-set", "live2d_animation_key_set",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=5, value=0.75)
        _, model = edit(host, "factory-handle-set", "live2d_animation_handle_set",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=5,
                        side="next", frame=6.5, value=1.0)
        _, model = edit(host, "factory-type-set", "live2d_animation_curve_type_set",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew", anchorFrame=5, type="BEZIER")
        point = curves(model, 0)[1]["points"][1]
        assert point["anchor"] == {"frame": 5, "value": 0.75}
        assert point["corner"] is True
        assert point["next"] == {"frame": 6.5, "integerFrame": 5, "value": 1.0,
                                 "positionOptimized": False}
        assert curves(model, 0)[1]["curveTypes"] == ["LINEAR", "BEZIER", "SMOOTH"]

        _, model = edit(host, "del-test", "live2d_animation_curve_delete",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamTest")
        assert curve_ids(model, 0) == ["live2dParam_ParamNew"]

        _, model = edit(host, "readd-test", "live2d_animation_curve_add",
                        **base, parameterId="ParamTest")
        assert curve_ids(model, 0) == ["live2dParam_ParamNew", "live2dParam_ParamTest"]
        readded = curves(model, 0)[1]
        assert readded["parameterGuid"] == "parameter-guid"
        assert readded["range"] == [-1.0, 1.0]
        assert readded["points"] == [] and readded["curveTypes"] == []

        saved = run / "saved.can3"
        expected_model = copy.deepcopy(model)
        host.call("save", "save", {"path": str(saved)})
        saved_xml = decode_entries(saved.read_bytes())[1].decode()
        new_ref = '<CMvAttrF xs.ref="soul2d-curve-track-guid-live2dParam_ParamNew"/>'
        test_ref = '<CMvAttrF xs.ref="attr"/>'
        opaque_ref = '<CMvAttrF xs.ref="attr-opaque"/>'
        assert saved_xml.find(new_ref) < saved_xml.find(test_ref) < saved_xml.find(opaque_ref), \
            saved_xml
        assert saved_xml.find(new_ref) < saved_xml.index(
            'xs.ref="attr-opaque"', saved_xml.index("attrList"))
        assert "discount=\"999\"" in saved_xml
        assert "futureCurveData" in saved_xml
        assert f'<CMvAttrF xs.id="{storage}">' in saved_xml
        assert '<d xs.n="curMin">-1.5</d>' in saved_xml
        assert '<d xs.n="curMax">1.25</d>' in saved_xml
        assert '<CCurveType v="SMOOTH"/>' in saved_xml
        caff_bytes_equal(original_can3, saved.read_bytes())

        host.call("reopen", "open", {"path": str(saved)})
        snap, model = state(host, "reopened")
        assert model == expected_model, "factory: complete model changed after save/reopen"
        assert curve_ids(model, 0) == ["live2dParam_ParamNew", "live2dParam_ParamTest"]
        new = curves(model, 0)[0]
        assert [p["anchor"]["frame"] for p in new["points"]] == [0, 5, 12]
        assert new["points"][1]["anchor"] == {"frame": 5, "value": 0.75}
        assert new["points"][1]["next"] == {
            "frame": 6.5, "integerFrame": 5, "value": 1.0, "positionOptimized": False}
        assert new["points"][1]["corner"] is True
        assert new["curveTypes"] == ["LINEAR", "BEZIER", "SMOOTH"]
        result["checks"].append("new-curve-accumulated-key-handle-type-save-reopen")

        host.call("reopen-orig", "open", {"path": str(can3)})
        _, model = edit(host, "add-t1", "live2d_animation_curve_add",
                        **base, parameterId="ParamNew")
        _, model = edit(host, "add-t2", "live2d_animation_curve_add",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["track2Guid"],
                        modelPath=str(cmo3), parameterId="ParamNew")
        assert curve_ids(model, 0) == ["live2dParam_ParamTest", "live2dParam_ParamNew"]
        assert curve_ids(model, 1) == ["live2dParam_ParamTest", "live2dParam_ParamNew"]
        assert curves(model, 0)[1]["parameterGuid"] == curves(model, 1)[1]["parameterGuid"] \
            == "new-parameter-guid"
        expected_model = copy.deepcopy(model)
        saved2 = run / "saved2.can3"
        host.call("save2", "save", {"path": str(saved2)})
        xml2 = decode_entries(saved2.read_bytes())[1].decode()
        assert 'xs.id="soul2d-curve-track-guid-live2dParam_ParamNew"' in xml2
        assert 'xs.id="soul2d-curve-track2-guid-live2dParam_ParamNew"' in xml2
        host.call("reopen-2", "open", {"path": str(saved2)})
        snap2, model2 = state(host, "saved2")
        assert model2 == expected_model, "two-track add: complete model changed after save/reopen"
        assert curve_ids(model2, 0) == ["live2dParam_ParamTest", "live2dParam_ParamNew"]
        assert curve_ids(model2, 1) == ["live2dParam_ParamTest", "live2dParam_ParamNew"]
        caff_bytes_equal(original_can3, saved2.read_bytes())
        result["checks"].append("simultaneous-new-curves-on-two-tracks")

        host.call("reopen-orig2", "open", {"path": str(can3)})
        _, model = edit(host, "del-t2", "live2d_animation_curve_delete",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["track2Guid"],
                        curveId="live2dParam_ParamTest")
        assert curve_ids(model, 1) == [] and curve_ids(model, 0) == ["live2dParam_ParamTest"]
        expected_model = copy.deepcopy(model)
        saved3 = run / "saved3.can3"
        host.call("save3", "save", {"path": str(saved3)})
        xml3 = decode_entries(saved3.read_bytes())[1].decode()
        assert 'xs.id="attr-t2"' not in xml3 and 'xs.ref="attr-t2"' not in xml3
        assert 'xs.id="attr"' in xml3
        _, orig_xml = decode_entries(original_can3)
        require_delete_oracle(orig_xml, decode_entries(saved3.read_bytes())[1],
                              '<CMvAttrF xs.ref="attr-t2"/>', "attr-t2")
        host.call("reopen-3", "open", {"path": str(saved3)})
        snap3, model3 = state(host, "saved3")
        assert model3 == expected_model, "delete track2: complete model changed after save/reopen"
        assert curve_ids(model3, 0) == ["live2dParam_ParamTest"] \
            and curve_ids(model3, 1) == []
        caff_bytes_equal(original_can3, saved3.read_bytes())
        result["checks"].append("exact-owned-curve-delete-track2")

        host.call("open-delete-t1", "open", {"path": str(can3)})
        _, model = edit(host, "delete-t1", "live2d_animation_curve_delete",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamTest")
        assert curve_ids(model, 0) == [] and curve_ids(model, 1) == ["live2dParam_ParamTest"]
        deleted1 = run / "deleted-track1.can3"
        deleted1_bytes = save_and_reopen(
            host, "delete-t1", deleted1, copy.deepcopy(model), original_can3)
        require_delete_oracle(orig_xml, decode_entries(deleted1_bytes)[1],
                              '<CMvAttrF xs.ref="attr"/>', "attr")
        result["checks"].append("exact-owned-curve-delete-track1")

        host.call("reopen-orig3", "open", {"path": str(can3)})
        _, model = edit(host, "cancel-add", "live2d_animation_curve_add",
                        **base, parameterId="ParamNew")
        _, model = edit(host, "cancel-del", "live2d_animation_curve_delete",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamNew")
        cancel = run / "cancel.can3"
        host.call("save-cancel", "save", {"path": str(cancel)})
        assert cancel.read_bytes() == original_can3, "cancelled file differs"
        result["checks"].append("add-delete-cancellation-identical-bytes")

        host.call("reopen-orig4", "open", {"path": str(can3)})
        _, model = edit(host, "oracle-add", "live2d_animation_curve_add",
                        **base, parameterId="ParamNew")
        oracle = run / "oracle.can3"
        host.call("save-oracle", "save", {"path": str(oracle)})
        _, original_xml = decode_entries(original_can3)
        _, oracle_xml = decode_entries(oracle.read_bytes())
        require_add_oracle(original_xml, oracle_xml, storage)
        probed = oracle_xml.decode().replace(
            '<CAttrId xs.n="id" idstr="other_effect"/>',
            '<CAttrId xs.n="id" idstr="other_effect" probe="1"/>', 1)
        try:
            require_add_oracle(original_xml, probed.encode(), storage)
        except (AssertionError, ValueError):
            pass
        else:
            raise AssertionError("probe-mutated retained opaque entry was not detected")
        probed_ref = oracle_xml.decode().replace(
            '<CMvAttrF xs.ref="attr"/>', '<CMvAttrF xs.ref="attr" probe="1"/>', 1)
        try:
            require_add_oracle(original_xml, probed_ref.encode(), storage)
        except (AssertionError, ValueError):
            pass
        else:
            raise AssertionError("probe-mutated retained reference was not detected")
        probed_count = oracle_xml.decode().replace("count='3'", "count='7'", 1)
        try:
            require_add_oracle(original_xml, probed_count.encode(), storage)
        except (AssertionError, ValueError):
            pass
        else:
            raise AssertionError("probe-mutated saved count was not detected")
        result["checks"].append("exact-add-oracle-and-negative-probes")

        relocation_cases = [
            ("relocate-end", ["ParamNew"], ["ParamTest"], ["ParamTest"],
             ["live2dParam_ParamNew", "live2dParam_ParamTest"],
             ["attr-opaque", "soul2d-curve-track-guid-live2dParam_ParamNew", "attr"], "0003"),
            ("relocate-before-kept", ["ParamNew", "ParamKeep"],
             ["ParamTest", "ParamKeep"], ["ParamTest", "ParamKeep"],
             ["live2dParam_ParamNew", "live2dParam_ParamTest", "live2dParam_ParamKeep"],
             ["attr-opaque", "soul2d-curve-track-guid-live2dParam_ParamNew", "attr",
              "soul2d-curve-track-guid-live2dParam_ParamKeep"], "0004"),
        ]
        for label, additions, deletions, readditions, expected_ids, references, count_text in relocation_cases:
            host.call(f"{label}-original", "open", {"path": str(can3)})
            for parameter in additions:
                _, model = edit(host, f"{label}-seed-{parameter}", "live2d_animation_curve_add",
                                **base, parameterId=parameter)
            staged = run / f"{label}-original.can3"
            staged_bytes = save_and_reopen(
                host, f"{label}-seed", staged, copy.deepcopy(model), original_can3)
            staged_xml = decode_entries(staged_bytes)[1].decode()
            count_token = f"count='{int(count_text)}'"
            assert count_token in staged_xml
            staged.write_bytes(caff_pack(
                staged_xml.replace(count_token, f"count='{count_text}'", 1).encode()))
            host.call(f"{label}-lexical-original", "open", {"path": str(staged)})
            _, original_model = state(host, f"{label}-original")
            originals = {curve["id"]: copy.deepcopy(curve) for curve in curves(original_model, 0)}
            expected = copy.deepcopy(original_model)
            expected_curves = [originals[curve_id] for curve_id in expected_ids]
            for curve in expected_curves:
                if curve["parameterId"] in readditions:
                    curve["points"] = []
                    curve["curveTypes"] = []
            expected["scenes"][0]["tracks"][0]["parameterCurves"] = expected_curves
            for parameter in deletions:
                _, model = edit(host, f"{label}-delete-{parameter}", "live2d_animation_curve_delete",
                                sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                                curveId=f"live2dParam_{parameter}")
            for parameter in readditions:
                _, model = edit(host, f"{label}-readd-{parameter}", "live2d_animation_curve_add",
                                **base, parameterId=parameter)
            assert model == expected, f"{label}: curve identity or unrelated model changed"
            assert curve_ids(model, 0) == expected_ids
            relocated = run / f"{label}-saved.can3"
            relocated_bytes = save_and_reopen(
                host, label, relocated, expected, staged.read_bytes())
            relocated_xml = decode_entries(relocated_bytes)[1].decode()
            positions = [relocated_xml.index(f'<CMvAttrF xs.ref="{ref}"/>') for ref in references]
            assert all(a < b for a, b in zip(positions, positions[1:])), label
            attr_at = relocated_xml.index('xs.n="attrList"')
            attr_start = relocated_xml.rindex("<", 0, attr_at)
            attr_end = relocated_xml.index(">", attr_at)
            assert f"count='{count_text}'" in relocated_xml[attr_start:attr_end], \
                f"{label}: unchanged count spelling was rewritten"
            assert '<s xs.n="futureCurveData">keep-curve &amp; 7.777</s>' in relocated_xml
            assert curves(expected, 0)[1]["points"] == []
            result["checks"].append(label)

        alias_can3 = run / "alias.can3"
        alias_xml = original_xml.decode().replace(
            '<s xs.n="futureRootData">keep-root &amp; 0.1234500</s>',
            '<s xs.n="futureRootData">keep-root &amp; 0.1234500</s><CMvAttrF xs.n="friend" xs.ref="attr"/>')
        alias_can3.write_bytes(caff_pack(alias_xml.encode()))
        host.call("reopen-alias", "open", {"path": str(alias_can3)})
        refuse(host, "outside-alias", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": "live2dParam_ParamTest"}, "referenced")
        result["checks"].append("outside-definition-alias-atomic-refusal")

        template = original_xml.decode()
        effect_start = template.index('<CMvKeyParamEffect xs.n="keyParamEffect">')
        effect_end = template.index("</CMvKeyParamEffect>", effect_start) + len("</CMvKeyParamEffect>")
        effect = template[effect_start:effect_end]
        effect_cases = [
            ("shared-effect", '<CMvKeyParamEffect xs.n="keyParamEffect" xs.ref="shared-effect"/>',
             effect.replace('xs.n="keyParamEffect"', 'xs.id="shared-effect"', 1),
             '<CMvKeyParamEffect xs.n="friend" xs.ref="shared-effect"/>'),
            ("inherited-effect", '<FutureDefaults xs.n="super" xs.ref="future-track-defaults"/>',
             f'<FutureDefaults xs.id="future-track-defaults">{effect}</FutureDefaults>',
             '<FutureDefaults xs.n="friend" xs.ref="future-track-defaults"/>'),
        ]
        for label, field, definition, outside_ref in effect_cases:
            xml = template[:effect_start] + field + template[effect_end:]
            xml = xml.replace("</shared>", f"{definition}</shared>", 1)
            xml = xml.replace(
                '<s xs.n="futureRootData">keep-root &amp; 0.1234500</s>',
                '<s xs.n="futureRootData">keep-root &amp; 0.1234500</s>' + outside_ref, 1)
            variant = run / f"{label}.can3"
            variant_bytes = caff_pack(xml.encode())
            variant.write_bytes(variant_bytes)
            host.call(f"{label}-open", "open", {"path": str(variant)})
            _, model = state(host, f"{label}-loaded")
            assert curve_ids(model, 0) == ["live2dParam_ParamTest"]
            refuse(host, f"{label}-topology", "live2d_animation_curve_add",
                   dict(base, parameterId="ParamNew"), "")
            _, model = edit(host, f"{label}-legacy-key", "live2d_animation_key_set",
                            sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                            curveId="live2dParam_ParamTest", anchorFrame=0, value=0.5)
            assert curves(model, 0)[0]["points"][0]["anchor"] == {"frame": 0, "value": 0.5}
            legacy = run / f"{label}-legacy.can3"
            save_and_reopen(host, f"{label}-legacy", legacy, copy.deepcopy(model), variant_bytes)
            result["checks"].append(f"{label}-refusal-and-legacy-key-persistence")

        host.call("reopen-orig5", "open", {"path": str(can3)})

        bad_cmo3 = run / "bad.cmo3"
        malformed = run / "malformed.cmo3"
        malformed.write_bytes(b"not-a-caff")
        refuse(host, "malformed-cmo3", "live2d_animation_curve_add",
               dict(base, modelPath=str(malformed), parameterId="ParamNew"), "")
        refuse(host, "wrong-track", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["track2Guid"],
                "curveId": "live2dParam_ParamNew"}, "was not found")
        refuse(host, "empty-selector", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": ""}, "curveId")
        refuse(host, "extra-arg", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": "live2dParam_ParamNew", "bogus": 1}, "unknown argument")
        refuse(host, "duplicate", "live2d_animation_curve_add",
               dict(base, parameterId="ParamTest"), "already exists")
        refuse(host, "missing", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": "live2dParam_ParamMissing"}, "was not found")
        refuse(host, "unknownkey", "live2d_animation_curve_add",
               dict(base, parameterId="ParamNew", bogus=1), "unknown argument")
        refuse(host, "nullparam", "live2d_animation_curve_add",
               dict(base, parameterId=None), "parameterId")
        refuse(host, "nullcurve", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": None}, "curveId")
        refuse(host, "stale", "live2d_animation_curve_delete",
               {"sceneGuid": ids["sceneGuid"], "trackGuid": ids["trackGuid"],
                "curveId": "live2dParam_ParamNew", "expectedRevision": 0}, "revision")
        refuse(host, "unknown-source", "live2d_animation_curve_add",
               dict(base, parameterId="ParamMissing"), "source parameter was not found")
        refuse(host, "wrongid", "live2d_animation_curve_delete",
               {"sceneGuid": "none", "trackGuid": ids["trackGuid"],
                "curveId": ids["curveId"]}, "was not found")
        host.call("reopen-orig6", "open", {"path": str(can3)})
        _, model = edit(host, "del-for-badreadd", "live2d_animation_curve_delete",
                        sceneGuid=ids["sceneGuid"], trackGuid=ids["trackGuid"],
                        curveId="live2dParam_ParamTest")
        bad_cmo3.write_bytes(caff_pack(
            cmo3_xml().replace("parameter-guid", "different-guid").encode()))
        refuse(host, "incompatible-readd", "live2d_animation_curve_add",
               dict(base, modelPath=str(bad_cmo3), parameterId="ParamTest"), "")
        result["checks"].append("source-and-selector-refusals-atomic")
        result["passed"] = True
    except Exception:
        (run / "failure.txt").write_text(traceback.format_exc())
        traceback.print_exc()
    finally:
        if host is not None:
            host.close()
            result["hostExited"] = host.child.poll() is not None
            result["passed"] = result["passed"] and result["hostExited"]
        (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"passed": result["passed"], "run": str(run)}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
