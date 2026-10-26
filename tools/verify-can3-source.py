#!/usr/bin/env python3
"""Check CAN3 source reads against the supplied XML through Rust FFI, JSONL and MCP."""
import argparse
import copy
import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
from xml.parsers import expat

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import importlib.util

spec = importlib.util.spec_from_file_location("cmo_source", ROOT / "tools/verify-cmo-source.py")
CMO = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CMO)
spec = importlib.util.spec_from_file_location("native_edit_transport", ROOT / "tools/verify-native-authoring-edit.py")
EDIT = importlib.util.module_from_spec(spec)
spec.loader.exec_module(EDIT)

EXPECTED = {
    "hiyori_free_t03.can3": (8, 990),
    "hiyori_pro_t04.can3": (10, 1582),
    "ren_t01.can3": (8, 1785),
    "zundamon_t01.can3": (23, 4157),
}


def key_fields(xml, identity):
    graph = CMO.XMLGraph(xml)
    animation = graph.resolve(list(graph.root.find("main"))[0])
    scene = next(s for s in graph.named_items(animation, "_scenes")
        if graph.guid(s, "guid", True) == identity["sceneGuid"])
    track = next(t for t in graph.source_items(scene, "trackSourceSet")
        if graph.guid(t, "guid", True) == identity["trackGuid"])
    attr = next(a for a in graph.named_items(graph.required(track, "keyParamEffect"), "attrList")
        if graph.required(a, "id").get("idstr") == identity["curveId"])
    sequence = graph.required(attr, "valueData")
    anchors = [graph.required(p, "anchor") for p in graph.named_items(sequence, "points")]
    anchor = next(a for a in anchors if graph.integer(a, "pos") == identity["anchorFrame"])
    return graph, [graph.required(anchor, "doubleValue"), graph.required(sequence, "curMin"),
        graph.required(sequence, "curMax")], [graph.number(a, "doubleValue") for a in anchors]


def scalar_tokens(xml, graph, fields):
    targets = {i: field for i, field in enumerate(graph.root.iter()) if field in fields}
    parser = expat.ParserCreate()
    counter, stack, bounds = 0, [], {}
    def start(name, attributes):
        nonlocal counter
        field = targets.get(counter)
        counter += 1
        stack.append(field)
        if field is not None:
            assert name == "d"
            bounds[id(field)] = [xml.find(b">", parser.CurrentByteIndex) + 1]
    def end(name):
        field = stack.pop()
        if field is not None:
            bounds[id(field)].append(parser.CurrentByteIndex)
    parser.StartElementHandler, parser.EndElementHandler = start, end
    parser.Parse(xml, True)
    result = []
    for field in fields:
        begin, end = bounds[id(field)]
        tokens = list(re.finditer(rb"\S+", xml[begin:end]))
        assert len(tokens) == 1
        result.append((begin + tokens[0].start(), begin + tokens[0].end()))
    return result


def check_key_saved(original, saved, identity, value):
    old_entries, old_xml = CMO.CAFF.decode(original)
    new_entries, new_xml = CMO.CAFF.decode(saved)
    assert len(old_entries) == len(new_entries)
    for old, new in zip(old_entries, new_entries):
        assert old[:4] == new[:4]
        if "main.xml" not in old[0]:
            assert old == new
    old_graph, old_fields, old_values = key_fields(old_xml, identity)
    new_graph, new_fields, new_values = key_fields(new_xml, identity)
    assert float(old_fields[1].text) == min(old_values) and float(old_fields[2].text) == max(old_values)
    assert float(new_fields[0].text) == value
    assert float(new_fields[1].text) == min(new_values) and float(new_fields[2].text) == max(new_values)
    old_ranges = scalar_tokens(old_xml, old_graph, old_fields)
    new_ranges = scalar_tokens(new_xml, new_graph, new_fields)
    expected = old_xml
    changes = []
    for field, old_range, new_range in zip(old_fields, old_ranges, new_ranges):
        if float(field.text) != float(new_xml[slice(*new_range)]):
            changes.append((old_range, new_xml[slice(*new_range)]))
    for (begin, end), token in sorted(changes, reverse=True):
        expected = expected[:begin] + token + expected[end:]
    assert new_xml == expected, "XML changed beyond anchor value and extrema tokens"
    assert original[:58] == saved[:58] and original[-2:] == saved[-2:]
    return len(changes)


def check_xml(actual, xml):
    graph = CMO.XMLGraph(xml)
    animation = graph.resolve(list(graph.root.find("main"))[0])
    assert actual["format"] == "live2d.animation.source.v1"
    assert actual["fileFormatVersion"] == int(graph.root.get("fileFormatVersion"))
    assert actual["name"] == graph.text(animation, "name", "")
    resources = graph.named_items(graph.required(animation, "resourceManager"), "_resourceRefList")
    linked = [graph.required(entry, "resourceRef") for entry in resources]
    images = [entry for entry in linked if entry.tag == "CResource_Linked_Image"]
    assert actual["imageResources"] == [dict({"guid": graph.guid(entry, "guid", True)}, **{
        key: graph.text(entry, field, "") for field, key in [("name", "name"), ("srcFile", "sourceFile")]
        if graph.field(entry, field) is not None}) for entry in images]
    linked = [entry for entry in linked if entry.tag == "CResource_Linked_Model"]
    assert actual["resources"] == [{"guid": graph.guid(entry, "guid", True),
        "name": graph.text(entry, "name", ""), "sourceFile": graph.text(entry, "srcFile", "")}
        for entry in linked]
    scenes = graph.named_items(animation, "_scenes")
    assert len(actual["scenes"]) == len(scenes)
    point_count = 0
    for row, scene in zip(actual["scenes"], scenes):
        assert row["guid"] == graph.guid(scene, "guid", True)
        assert row["name"] == graph.text(scene, "sceneName", "")
        movie = graph.required(scene, "movieInfo")
        assert row["fps"] == graph.number(movie, "fps")
        assert row["durationFrames"] == graph.integer(movie, "duration")
        assert row.get("tag") == (None if graph.field(scene, "tag") is None else graph.text(scene, "tag", ""))
        images = [track for track in graph.source_items(scene, "trackSourceSet") if track.tag == "CMvTrack_Image_Source"]
        assert row["imageTracks"] == [{"guid": graph.guid(track, "guid", True),
            "name": graph.text(track, "name", ""), "resourceGuid": graph.required(track, "_resourceGuid").get("uuid"),
            **{key: graph.integer(track, key) for key in ["start", "internalOffset", "duration"]}} for track in images]
        tracks = [track for track in graph.source_items(scene, "trackSourceSet")
            if track.tag == "CMvTrack_Live2DModel_Source"]
        assert len(row["tracks"]) == len(tracks)
        for item, track in zip(row["tracks"], tracks):
            assert item["guid"] == graph.guid(track, "guid", True)
            assert item["name"] == graph.text(track, "name", "")
            assert item["resourceGuid"] == graph.required(track, "_resourceGuid").get("uuid")
            for key in ["start", "internalOffset", "duration"]:
                assert item[key] == graph.integer(track, key)
            attrs = graph.named_items(graph.required(track, "keyParamEffect"), "attrList")
            attrs = [a for a in attrs if graph.required(a, "id").get("idstr", "").startswith("live2dParam_")]
            assert len(item["parameterCurves"]) == len(attrs)
            channels = [(curve, attr, False) for curve, attr in zip(item["parameterCurves"], attrs)]
            effect = graph.field(track, "partsVisibleEffect")
            part_entries = [] if effect is None or effect.tag == "null" else graph.named_items(effect, "attrMap")
            all_part_attrs = [graph.required(entry, "value") for entry in part_entries
                if graph.required(entry, "key").get("idstr", "").startswith("live2DPartsOpacity:")]
            part_attrs = [attr for attr in all_part_attrs
                if graph.required(attr, "id").get("idstr", "").startswith("live2DPartsOpacity_")]
            assert len(item.get("partOpacityCurves", [])) == len(part_attrs)
            # Entries keyed `live2DPartsOpacity_<part>` are read as underscore curves and count as skipped too.
            underscore = [entry for entry in part_entries
                if graph.required(entry, "key").get("idstr", "").startswith("live2DPartsOpacity_")]
            assert len(item.get("underscorePartOpacityCurves", [])) == len(underscore)
            assert item.get("skippedLegacyPartOpacityCount", 0) == len(all_part_attrs)-len(part_attrs)+len(underscore)
            channels.extend((curve, attr, True) for curve, attr in zip(item.get("partOpacityCurves", []), part_attrs))
            legacy_entries = [entry for entry in part_entries
                if graph.required(entry, "key").get("idstr", "").startswith("live2DPartsOpacity:")
                and graph.required(graph.required(entry, "value"), "id").get("idstr", "").startswith("live2DPartsOpacity:")]
            legacy_curves = item.get("legacyPartOpacityCurves", [])
            assert len(legacy_curves) == len(legacy_entries)
            assert len({graph.required(graph.required(e, "value"), "id").get("idstr") for e in legacy_entries}) == len(legacy_entries)
            assert len({graph.guid(graph.required(e, "value"), "guid", True) for e in legacy_entries}) == len(legacy_entries)
            for curve, entry in zip(legacy_curves, legacy_entries):
                attr = graph.required(entry, "value")
                attr_id = graph.required(attr, "id").get("idstr")
                map_id = graph.required(entry, "key").get("idstr")
                assert curve["legacyMapPartId"] == map_id[len("live2DPartsOpacity:"):]
                assert curve["legacyAttributeId"] == attr_id[len("live2DPartsOpacity:"):]
                assert "partId" not in curve
                channels.append((curve, attr, "legacy"))
            for curve, attr, is_part in channels:
                attr_id = graph.required(attr, "id").get("idstr")
                prefix = "live2DPartsOpacity_" if is_part else "live2dParam_"
                assert curve["id"] == attr_id
                assert graph.required(attr, "track") is graph.resolve(track)
                if is_part != "legacy":
                    assert curve["partId" if is_part else "parameterId"] == attr_id[len(prefix):]
                assert curve["partGuid" if is_part else "parameterGuid"] == graph.guid(attr, "guid", True)
                options = graph.required(attr, "optionParam")
                assert options.tag in ("hash_map", "COptionParam")
                entries = graph.items(options)
                if not is_part and options.tag == "hash_map" and not entries and options.get("count") == "0":
                    assert "sourceParameter" in curve and curve["sourceParameter"] is None
                else:
                    source_id = graph.required(options,
                        "KEY_PARTS_VISIBLE_ID" if is_part else "KEY_PARAM_ID")
                    assert curve["sourcePart" if is_part else "sourceParameter"] == {"type": source_id.tag,
                        "value": source_id.get("idstr") if source_id.tag in ["CParameterId", "CPartId"] else source_id.text}
                assert curve["range"] == [graph.number(attr, "rangeMin"), graph.number(attr, "rangeMax")]
                sequence = graph.required(attr, "valueData")
                assert graph.required(sequence, "attr") is graph.resolve(attr)
                base = graph.field(sequence, "baseValue")
                if is_part or base is None:
                    assert "baseValue" not in curve
                else:
                    assert curve["baseValue"] == graph.number(sequence, "baseValue")
                points = graph.named_items(sequence, "points")
                assert len(curve["points"]) == len(points)
                assert curve["curveTypes"] == [t.get("v") for t in graph.named_items(sequence, "curveTypes")]
                for key, point in zip(curve["points"], points):
                    anchor = graph.required(point, "anchor")
                    assert key["anchor"] == {"frame": graph.integer(anchor, "pos"),
                        "value": graph.number(anchor, "doubleValue")}
                    assert key["corner"] == graph.boolean(anchor, "isCorner", None)
                    for target, source in [("previous", "prev"), ("next", "next")]:
                        control = graph.required(point, source)
                        assert key[target] == {"frame": graph.number(control, "posF"),
                            "integerFrame": graph.integer(control, "pos"),
                            "value": graph.number(control, "doubleValue"),
                            "positionOptimized": graph.boolean(control, "isPosOptimized", None)}
                if not is_part:
                    point_count += len(points)
    return len(scenes), point_count


def mcp_read(binary, path, success=True):
    messages = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "can3-source-verifier", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "live2d_animation_read", "arguments": {"path": str(path)}}}]
    process = subprocess.run([str(binary), "--mcp"], input="".join(json.dumps(m) + "\n" for m in messages),
        capture_output=True, text=True, timeout=120, check=True)
    responses = [json.loads(line) for line in process.stdout.splitlines()]
    tools = responses[1]["result"]["tools"]
    tool = next(t for t in tools if t["name"] == "live2d_animation_read")
    assert tool["inputSchema"]["required"] == []  # an active CAN3 document is read when no path is given
    result = responses[2]["result"]
    assert bool(result.get("isError")) == (not success), result
    if not success:
        return result, process.stdout
    body = result.get("structuredContent")
    if body is None:
        body = json.loads(next(c["text"] for c in result["content"] if c["type"] == "text"))
    if set(body) == {"result"}:
        body = body["result"]
    return body, process.stdout


def malformed_source(xml, kind):
    graph = CMO.XMLGraph(xml)
    animation = graph.resolve(list(graph.root.find("main"))[0])
    scenes = graph.named_items(animation, "_scenes")
    scene = scenes[0]
    track = next(t for t in graph.source_items(scene, "trackSourceSet")
        if t.tag == "CMvTrack_Live2DModel_Source")
    if kind.startswith("legacy-"):
        effect = graph.required(track, "partsVisibleEffect")
        attr = next(graph.required(entry, "value") for entry in graph.named_items(effect, "attrMap")
            if graph.required(graph.required(entry, "value"), "id").get("idstr", "").startswith("live2DPartsOpacity:")
            and graph.named_items(graph.required(graph.required(entry, "value"), "valueData"), "points"))
        sequence = graph.required(attr, "valueData")
        if kind == "legacy-guid-type":
            graph.required(attr, "guid").tag = "CParameterGuid"
            message = "legacy part opacity GUID is not CPartGuid"
        elif kind == "legacy-sequence-owner":
            ref = next(n for n in sequence.iter() if n.get("xs.n") == "attr")
            ref.set("xs.ref", track.get("xs.id"))
            message = "sequence attribute backlink"
        elif kind == "legacy-empty-id":
            graph.required(attr, "id").set("idstr", "live2DPartsOpacity:")
            message = "legacy part opacity map or attribute ID is empty"
        else:
            assert kind == "legacy-nonfinite"
            anchor = graph.required(graph.named_items(sequence, "points")[0], "anchor")
            graph.required(anchor, "doubleValue").text = "NaN"
            message = "invalid finite authoring number"
        return CMO.ET.tostring(graph.root, encoding="utf-8"), message
    effect = graph.required(track, "keyParamEffect")
    attrs = [a for a in graph.named_items(effect, "attrList")
        if graph.required(a, "id").get("idstr", "").startswith("live2dParam_")]
    attr = next(a for a in attrs if len(graph.named_items(graph.required(a, "valueData"), "points")) >= 2)
    sequence = graph.required(attr, "valueData")
    messages = {
        "scene-owner": "scene project backlink",
        "effect-owner": "effect track backlink",
        "sequence-owner": "sequence attribute backlink",
        "duplicate-frame": "duplicate animation anchor frame",
        "duplicate-field": "duplicate authoring field",
        "count": "array count does not match content",
        "nonfinite": "invalid finite authoring number",
        "curve-type": "curve type is not CCurveType",
    }
    if kind in ["scene-owner", "effect-owner", "sequence-owner"]:
        field = {"scene-owner": (scene, "_animation"), "effect-owner": (effect, "track"),
            "sequence-owner": (sequence, "attr")}[kind]
        # Mutate the reference token, not its resolved target definition.
        ref = next(n for n in field[0].iter() if n.get("xs.n") == field[1])
        ref.set("xs.ref", scene.get("xs.id") if kind != "scene-owner" else track.get("xs.id"))
    elif kind == "duplicate-field":
        owner = graph.required(attr, "super")
        owner.append(copy.deepcopy(next(n for n in owner if n.get("xs.n") == "id")))
    elif kind == "count":
        graph.required(sequence, "points").set("count", "999999")
    elif kind == "curve-type":
        graph.named_items(sequence, "curveTypes")[0].tag = "UnknownCurveType"
    else:
        anchors = [graph.required(p, "anchor") for p in graph.named_items(sequence, "points")]
        if kind == "duplicate-frame":
            graph.required(anchors[1], "pos").text = graph.required(anchors[0], "pos").text
        else:
            graph.required(anchors[0], "doubleValue").text = "NaN"
    return CMO.ET.tostring(graph.root, encoding="utf-8"), messages[kind]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, default=CMO.DEFAULT_INPUTS)
    args = parser.parse_args()
    paths = sorted(args.inputs.rglob("*.can3"))
    assert {p.name for p in paths} == set(EXPECTED) and len(paths) == 4
    evidence = Path(tempfile.mkdtemp(prefix="can3-source-", dir=ROOT / "build/verification"))
    binary = ROOT / "out/rust/debug/soul2d-core"
    library = ROOT / "out/rust/debug/libsoul2d_core.dylib"
    core = CMO.CORE.Core(library)
    result = {"status": "FAIL", "files": [], "binarySha256": CMO.sha(binary),
        "librarySha256": CMO.sha(library)}
    try:
        core.request("node.create", className="Node", _suffix="", name="CAN3 read history guard")
        core.request("undo")
        before = core.request("snapshot")
        for path in paths:
            original = path.read_bytes()
            _, xml = CMO.CAFF.decode(original)
            ffi = core.request("live2d.animation.read", path=str(path.resolve()))
            assert check_xml(ffi, xml) == EXPECTED[path.name]
            jsonl, wire = CMO.jsonl_request(binary, "live2d.animation.read", path.resolve())
            assert jsonl["succeeded"] and jsonl["result"] == ffi
            (evidence / (path.stem + "-jsonl.log")).write_text(wire)
            mcp, wire = mcp_read(binary, path.resolve())
            assert mcp == ffi
            (evidence / (path.stem + "-mcp.log")).write_text(wire)
            assert core.request("snapshot") == before and path.read_bytes() == original
            result["files"].append({"path": str(path), "sha256": CMO.sha(path),
                "scenes": len(ffi["scenes"]), "parameterKeys": EXPECTED[path.name][1], "routes": ["FFI", "JSONL", "MCP"]})
        original = paths[0].read_bytes()
        entries, xml = CMO.CAFF.decode(original)
        result["negativeControls"] = []
        for kind in ["scene-owner", "effect-owner", "sequence-owner", "duplicate-frame",
                "duplicate-field", "count", "nonfinite", "curve-type", "legacy-guid-type",
                "legacy-sequence-owner", "legacy-empty-id", "legacy-nonfinite"]:
            malformed_xml, message = malformed_source(xml, kind)
            path = evidence / (kind + ".can3")
            path.write_bytes(CMO.repack_main_xml(original, entries, malformed_xml))
            ffi = CMO.ffi_request(core, "live2d.animation.read", path)
            assert not ffi["succeeded"] and message in ffi["message"], ffi
            jsonl, wire = CMO.jsonl_request(binary, "live2d.animation.read", path)
            assert not jsonl["succeeded"] and message in jsonl["message"], jsonl
            (evidence / (kind + "-jsonl.log")).write_text(wire)
            mcp, wire = mcp_read(binary, path, success=False)
            assert message in str(mcp), mcp
            (evidence / (kind + "-mcp.log")).write_text(wire)
            assert core.request("snapshot") == before
            result["negativeControls"].append({"kind": kind, "sha256": CMO.sha(path),
                "message": message, "routes": ["FFI", "JSONL", "MCP"]})
        result["sourceReadsPreserveSessionHistory"] = True
        result["documentRoundtrips"] = []
        for path in paths:
            original = path.read_bytes()
            opened = core.request("open", path=str(path.resolve()))
            assert opened["documentFormat"] == "live2d.can3"
            assert opened["values"] == {} and opened["motion3"] is None
            assert not any(opened[key] for key in ["modified", "canUndo", "canRedo"])
            assert core.request("live2d.animation.read") == opened["model"]
            for operation in ["node.create", "render.packet", "motion3.seek", "live2d.parameter.set"]:
                core.request(operation, success=False)
                assert core.request("snapshot") == opened
            recovery = evidence / (path.stem + "-recovery.can3")
            core.request("save", path=str(recovery.resolve()), mark_saved=False)
            assert recovery.read_bytes() == original and core.request("snapshot") == opened
            saved = evidence / (path.stem + "-saved.can3")
            core.request("save", path=str(saved.resolve()))
            assert saved.read_bytes() == original and path.read_bytes() == original
            reopened = core.request("open", path=str(saved.resolve()))
            assert reopened["model"] == opened["model"]
            assert reopened["documentFormat"] == "live2d.can3"
            result["documentRoundtrips"].append({"source": str(path), "sourceSha256": CMO.sha(path),
                "saved": str(saved), "savedSha256": CMO.sha(saved), "byteExact": True,
                "format": reopened["documentFormat"]})
        result["keyEditRoundtrips"] = []
        result["keyEditNegativeControls"] = []
        result["keyEditTransportWorkflows"] = []
        for path in paths:
            original = path.read_bytes()
            opened = core.request("open", path=str(path.resolve()))
            choices = [(scene, track, curve, point) for scene in opened["model"]["scenes"]
                for track in scene["tracks"] for curve in track["parameterCurves"] for point in curve["points"]]
            scene, track, curve, point = next((c for c in choices if c[3]["anchor"]["frame"] < 0), choices[0])
            identity = {"sceneGuid": scene["guid"], "trackGuid": track["guid"], "curveId": curve["id"],
                "anchorFrame": point["anchor"]["frame"]}
            old_value = point["anchor"]["value"]
            value = max(p["anchor"]["value"] for p in curve["points"]) + 1.125
            request = dict(identity, value=value, expectedRevision=opened["revision"])
            assert core.request("live2d.animation.key.set", **dict(request, value=old_value)) == opened
            for invalid in [dict(request, expectedRevision=opened["revision"] + 1),
                    dict(request, curveId="missing"), dict(request, anchorFrame=2**63 - 1)]:
                core.request("live2d.animation.key.set", success=False, **invalid)
                assert core.request("snapshot") == opened
            edited = core.request("live2d.animation.key.set", **request)
            expected = copy.deepcopy(opened["model"])
            selected = next(p for s in expected["scenes"] if s["guid"] == identity["sceneGuid"]
                for t in s["tracks"] if t["guid"] == identity["trackGuid"]
                for c in t["parameterCurves"] if c["id"] == identity["curveId"]
                for p in c["points"] if p["anchor"]["frame"] == identity["anchorFrame"])
            selected["anchor"]["value"] = value
            assert edited["model"] == expected and edited["modified"] and edited["canUndo"]
            core.request("live2d.animation.key.set", success=False, **request)
            assert core.request("snapshot") == edited
            undone = core.request("undo")
            assert undone["model"] == opened["model"] and not undone["modified"] and undone["canRedo"]
            redone = core.request("redo")
            assert redone["model"] == expected and redone["modified"]
            saved = evidence / (path.stem + "-key-edited.can3")
            core.request("save", path=str(saved.resolve()))
            token_count = check_key_saved(original, saved.read_bytes(), identity, value)
            reopened = core.request("open", path=str(saved.resolve()))
            assert reopened["model"] == expected and not reopened["modified"] and not reopened["canUndo"]
            assert path.read_bytes() == original
            result["keyEditRoundtrips"].append({"source": str(path), "sourceSha256": CMO.sha(path),
                "saved": str(saved), "savedSha256": CMO.sha(saved), "identity": identity,
                "value": value, "changedTokens": token_count, "unknownDataPreserved": True})
            for transport in ["jsonl", "mcp"]:
                destination = evidence / (path.stem + "-key-" + transport + ".can3")
                key_request = dict(identity, op="live2d.animation.key.set", value=value, expectedRevision=1)
                requests = [{"op":"open", "path":str(path.resolve())},
                    dict(key_request, value=old_value), key_request, key_request,
                    {"op":"snapshot"}, {"op":"undo"}, {"op":"redo"},
                    {"op":"save", "path":str(destination.resolve())},
                    {"op":"open", "path":str(destination.resolve())}]
                responses, wire = EDIT.run_transport(binary, None, requests, transport)
                assert responses[0]["result"]["revision"] == 1
                assert all(r["succeeded"] for i,r in enumerate(responses) if i != 3)
                assert not responses[3]["succeeded"]
                assert responses[1]["result"] == responses[0]["result"]
                assert responses[2]["result"]["model"] == expected and responses[2]["result"]["canUndo"]
                assert responses[4] == responses[2]
                assert responses[5]["result"]["model"] == opened["model"] and responses[5]["result"]["canRedo"]
                assert responses[6]["result"]["model"] == expected
                assert responses[8]["result"]["model"] == expected and not responses[8]["result"]["canUndo"]
                check_key_saved(original, destination.read_bytes(), identity, value)
                log = evidence / (path.stem + "-key-" + transport + ".jsonl")
                log.write_text(wire)
                result["keyEditTransportWorkflows"].append({"source":path.name, "transport":transport,
                    "saved":str(destination), "wire":str(log), "requests":len(requests),
                    "unknownDataPreserved":True})
            entries, xml = CMO.CAFF.decode(original)
            for kind in ["cache-mismatch", "shared-scalar"]:
                graph, fields, values = key_fields(xml, identity)
                if kind == "cache-mismatch":
                    fields[2].text = str(max(values) + 0.5)
                else:
                    fields[0].set("xs.id", "key-edit-shared-scalar")
                    graph.root.append(CMO.ET.Element("d", {"xs.ref": "key-edit-shared-scalar"}))
                malformed = evidence / (path.stem + "-" + kind + ".can3")
                malformed.write_bytes(CMO.repack_main_xml(original, entries, CMO.ET.tostring(graph.root, encoding="utf-8")))
                before = core.request("open", path=str(malformed.resolve()))
                core.request("live2d.animation.key.set", success=False,
                    **dict(identity, value=value, expectedRevision=before["revision"]))
                assert core.request("snapshot") == before
                result["keyEditNegativeControls"].append({"source": path.name, "kind": kind,
                    "sessionUnchanged": True, "sha256": CMO.sha(malformed)})
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = repr(error)
        raise
    finally:
        core.close()
        (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({"evidence": str(evidence), "status": result["status"],
            "files": result["files"], "error": result.get("error")}, indent=2))


if __name__ == "__main__":
    main()
