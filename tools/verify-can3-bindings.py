#!/usr/bin/env python3
"""Verify strict CAN3/CMO3 identity bindings independently from the Rust matcher."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
PAIRS = {
    "hiyori_free_t03.can3": "hiyori_free_t08.cmo3",
    "hiyori_pro_t04.can3": "hiyori_pro_t11.cmo3",
    "ren_t01.can3": "ren_t01.cmo3",
    "zundamon_t01.can3": "zundamon_t01.cmo3",
}
HASHES = {
    "hiyori_free_t03.can3": "3d2cae7fe76c05a3ca6fcc7af25b0a845827be0c18909c3144523d28cca9de07",
    "hiyori_free_t08.cmo3": "cc66c29024a2c3f008ccd7993583e56ea5c4bd00ffa33eac9a755fbb47b1e80a",
    "hiyori_pro_t04.can3": "416f33ab0c5d31cd44ac85d725e7a7c9a6f7c702eb561024203f69228323e0e4",
    "hiyori_pro_t11.cmo3": "00f90521daecd8c75afebe2d91efa1857485801ee01595f7ec1b9a10151b039b",
    "ren_t01.can3": "33907bd63068c987bcdd6c422a7d0a91409ddbec745a2940bf2b9edb6246fd98",
    "ren_t01.cmo3": "efb40b557abec3c46f097a681c98fe0e93b8e1b7615b90604ae0270f0bdfb6f0",
    "zundamon_t01.can3": "1287fe9fbb4fdc1323331a3f88a5ced6b1c6921cf5d298cbe624e18fa24ffe3e",
    "zundamon_t01.cmo3": "64b5ed6f4951f5565b3d7e200c3089a417629d8a901aa25d2863b348448ca442",
}


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


CMO = module("binding_cmo_xml", ROOT / "tools/verify-cmo-source.py")
EDIT = module("binding_transport", ROOT / "tools/verify-native-authoring-edit.py")


def binding_oracle(animation_bytes, model_bytes, resource_guid):
    _, animation_xml = CMO.CAFF.decode(animation_bytes)
    _, model_xml = CMO.CAFF.decode(model_bytes)
    animation_graph, model_graph = CMO.XMLGraph(animation_xml), CMO.XMLGraph(model_xml)
    animation = animation_graph.resolve(list(animation_graph.root.find("main"))[0])
    assert animation.tag == "CAnimation"
    resources = [
        animation_graph.required(entry, "resourceRef")
        for entry in animation_graph.named_items(
            animation_graph.required(animation, "resourceManager"), "_resourceRefList")
    ]
    assert sum(r.tag == "CResource_Linked_Model"
               and animation_graph.guid(r, "guid", True) == resource_guid for r in resources) == 1
    models = [node for node in model_graph.root.iter("CModelSource")
              if node.get("xs.ref") is None and len(node)]
    assert len(models) == 1
    model = models[0]
    parameters, ids = {}, set()
    for parameter in model_graph.source_items(model, "parameterSourceSet"):
        guid = model_graph.guid(parameter, "guid", True)
        identifier = model_graph.required(parameter, "id").get("idstr")
        assert guid and identifier and guid not in parameters and identifier not in ids
        parameters[guid] = identifier
        ids.add(identifier)
    part_ids_by_guid, part_guids_by_id = {}, {}
    for part in model_graph.source_items(model, "partSourceSet"):
        guid = model_graph.guid(part, "guid", True)
        identifier = model_graph.required(part, "id").get("idstr")
        assert guid and identifier and guid not in part_ids_by_guid
        assert identifier not in part_guids_by_id
        part_ids_by_guid[guid] = identifier
        part_guids_by_id[identifier] = guid
    bindings = []
    part_bindings = []
    legacy_bindings = []
    unbound_legacy = []
    for scene in animation_graph.named_items(animation, "_scenes"):
        for track in animation_graph.source_items(scene, "trackSourceSet"):
            if track.tag != "CMvTrack_Live2DModel_Source":
                continue
            if animation_graph.required(track, "_resourceGuid").get("uuid") != resource_guid:
                continue
            effect = animation_graph.required(track, "keyParamEffect")
            for attribute in animation_graph.named_items(effect, "attrList"):
                curve_id = animation_graph.required(attribute, "id").get("idstr", "")
                if not curve_id.startswith("live2dParam_") or curve_id == "live2dParam_":
                    continue
                guid = animation_graph.guid(attribute, "guid", True)
                identifier = parameters[guid]
                assert curve_id[len("live2dParam_"):] == identifier
                options = animation_graph.required(attribute, "optionParam")
                assert options.tag in ("hash_map", "COptionParam")
                entries = animation_graph.items(options)
                if options.tag == "hash_map" and not entries and options.get("count") == "0":
                    source_metadata, status = None, "missing"
                else:
                    source = animation_graph.required(options, "KEY_PARAM_ID")
                    assert source.tag in ("s", "CParameterId")
                    if source.tag == "CParameterId":
                        value = source.get("idstr")
                        status = "matchesId" if value == identifier else "differentId"
                    else:
                        value, status = source.text, "legacyText"
                    source_metadata = {"type": source.tag, "value": value}
                bindings.append({
                    "sceneGuid": animation_graph.guid(scene, "guid", True),
                    "trackGuid": animation_graph.guid(track, "guid", True),
                    "curveId": curve_id, "parameterId": identifier, "parameterGuid": guid,
                    "sourceParameter": source_metadata,
                    "sourceParameterStatus": status,
                })
            effect = animation_graph.field(track, "partsVisibleEffect")
            if effect is None or effect.tag == "null":
                continue
            assert effect.tag == "CMvEffect_Live2DPartsVisible"
            assert animation_graph.required(effect, "track") is animation_graph.resolve(track)
            attr_map = animation_graph.required(effect, "attrMap")
            assert attr_map.tag == "hash_map"
            map_keys, curve_ids, part_ids, part_guids = set(), set(), set(), set()
            for entry in animation_graph.items(attr_map):
                assert entry.tag == "entry"
                key = animation_graph.required(entry, "key")
                assert key.tag == "CAttrId"
                key_id = key.get("idstr")
                assert key_id and key_id not in map_keys
                map_keys.add(key_id)
                prefix = "live2DPartsOpacity:"
                if not key_id.startswith(prefix):
                    continue
                part_id_from_key = key_id[len(prefix):]
                attribute = animation_graph.required(entry, "value")
                assert attribute.tag == "CMvAttrF"
                curve_id = animation_graph.required(attribute, "id").get("idstr")
                if not curve_id.startswith("live2DPartsOpacity_"):
                    assert curve_id.startswith("live2DPartsOpacity:")
                    guid_node = animation_graph.required(attribute, "guid")
                    assert guid_node.tag == "CPartGuid" and guid_node.get("uuid")
                    guid = guid_node.get("uuid")
                    item = {"sceneGuid": animation_graph.guid(scene, "guid", True),
                        "trackGuid": animation_graph.guid(track, "guid", True),
                        "curveId": curve_id, "partGuid": guid}
                    if guid not in part_ids_by_guid:
                        unbound_legacy.append(dict(item, reason="partGuidNotFound"))
                    else:
                        source = animation_graph.required(animation_graph.required(attribute, "optionParam"), "KEY_PARTS_VISIBLE_ID")
                        item.update(partId=part_ids_by_guid[guid], sourcePart={"type": source.tag,
                            "value": source.get("idstr") if source.tag == "CPartId" else source.text})
                        legacy_bindings.append(item)
                    continue
                assert animation_graph.required(attribute, "track") is animation_graph.resolve(track)
                assert part_id_from_key and ":" not in part_id_from_key
                assert curve_id == f"live2DPartsOpacity_{part_id_from_key}"
                assert curve_id not in curve_ids
                curve_ids.add(curve_id)
                guid_node = animation_graph.required(attribute, "guid")
                assert guid_node.tag == "CPartGuid"
                part_guid = guid_node.get("uuid")
                assert part_guid and part_guid not in part_guids
                part_guids.add(part_guid)
                option_param = animation_graph.required(attribute, "optionParam")
                assert option_param.tag == "hash_map"
                option_nodes = [child for child in option_param
                                if isinstance(child.tag, str)]
                assert option_param.get("count") in (None, str(len(option_nodes)))
                assert len(option_nodes) == 1
                option = option_nodes[0]
                assert option.get("xs.n") == "KEY_PARTS_VISIBLE_ID"
                source = animation_graph.resolve(option)
                if source.tag == "CPartId":
                    source_value = source.get("idstr")
                else:
                    assert source.tag == "s"
                    source_value = source.text
                assert source_value and source_value.strip()
                part_id = source_value.strip()
                assert part_id == part_id_from_key and part_id not in part_ids
                part_ids.add(part_id)
                assert part_ids_by_guid[part_guid] == part_id
                assert part_guids_by_id[part_id] == part_guid
                part_bindings.append({
                    "sceneGuid": animation_graph.guid(scene, "guid", True),
                    "trackGuid": animation_graph.guid(track, "guid", True),
                    "curveId": curve_id, "partId": part_id, "partGuid": part_guid,
                    "sourcePart": {"type": source.tag, "value": source_value},
                })
    return {"format": "live2d.animation.bindings.v1", "resourceGuid": resource_guid,
            "modelName": model_graph.text(model, "name", ""),
            "bindingBasis": "parameterGuid+attributeId", "bindings": bindings,
            "partCurveCount": len(part_bindings), "partBindings": part_bindings,
            "legacyPartBindings": legacy_bindings, "unboundLegacyPartCurves": unbound_legacy}


def part_opacity_counts(animation_bytes, resource_guid):
    _, animation_xml = CMO.CAFF.decode(animation_bytes)
    graph = CMO.XMLGraph(animation_xml)
    animation = graph.resolve(list(graph.root.find("main"))[0])
    modern, skipped_legacy = 0, 0
    for scene in graph.named_items(animation, "_scenes"):
        for track in graph.source_items(scene, "trackSourceSet"):
            if track.tag != "CMvTrack_Live2DModel_Source":
                continue
            if graph.required(track, "_resourceGuid").get("uuid") != resource_guid:
                continue
            effect = graph.field(track, "partsVisibleEffect")
            if effect is None or effect.tag == "null":
                continue
            assert effect.tag == "CMvEffect_Live2DPartsVisible"
            assert graph.required(effect, "track") is graph.resolve(track)
            attr_map = graph.required(effect, "attrMap")
            assert attr_map.tag == "hash_map"
            for entry in graph.items(attr_map):
                assert entry.tag == "entry"
                key = graph.required(entry, "key").get("idstr", "")
                if not key.startswith("live2DPartsOpacity:"):
                    continue
                attribute = graph.required(entry, "value")
                assert attribute.tag == "CMvAttrF"
                id_node = graph.required(attribute, "id")
                assert id_node.tag == "CAttrId"
                curve_id = graph.required(attribute, "id").get("idstr", "")
                assert curve_id
                if curve_id.startswith("live2DPartsOpacity_"):
                    modern += 1
                elif curve_id.startswith("live2DPartsOpacity:"):
                    skipped_legacy += 1
                else:
                    raise AssertionError("unsupported Part opacity channel ID family")
    return modern, skipped_legacy, modern + skipped_legacy


def resource_guid(data):
    _, xml = CMO.CAFF.decode(data)
    graph = CMO.XMLGraph(xml)
    animation = graph.resolve(list(graph.root.find("main"))[0])
    resources = [graph.required(item, "resourceRef") for item in graph.named_items(
        graph.required(animation, "resourceManager"), "_resourceRefList")]
    resources = [item for item in resources if item.tag == "CResource_Linked_Model"]
    assert len(resources) == 1
    return graph.guid(resources[0], "guid", True)


def workflow(animation_path, model_path, wrong_model_path, guid):
    args = {"modelPath": str(model_path), "resourceGuid": guid}
    explicit = dict(args, path=str(animation_path))
    op = "live2d.animation.bindings"
    return [
        {"op": "node.create", "className": "Node", "name": "Binding history guard"},
        {"op": "undo"}, {"op": "snapshot"},
        dict(explicit, op=op), {"op": "snapshot"},
        dict(explicit, op=op, resourceGuid="missing"), {"op": "snapshot"},
        {"op": "open", "path": str(animation_path)},
        dict(args, op=op), {"op": "snapshot"},
        dict(args, op=op, modelPath=str(wrong_model_path)), {"op": "snapshot"},
        dict(args, op=op, modelPath=str(animation_path)), {"op": "snapshot"},
        {"op": op, "resourceGuid": guid}, {"op": "snapshot"},
        dict(args, op=op, resourceGuid=1), {"op": "snapshot"},
        dict(args, op=op, path=None), {"op": "snapshot"},
        dict(args, op=op, path=str(animation_path.parent)), {"op": "snapshot"},
        {"op": op, "modelPath": str(model_path)}, {"op": "snapshot"},
    ]


def check_workflow(responses, expected):
    negatives = {5, 10, 12, 14, 16, 18, 20, 22}
    assert len(responses) == 24
    for index, response in enumerate(responses):
        assert response["succeeded"] is (index not in negatives), (index, response)
        if index in negatives:
            assert response.get("message"), response
    assert responses[3]["result"] == expected
    assert responses[8]["result"] == expected
    guard = responses[2]["result"]
    assert guard["canRedo"] is True and guard["canUndo"] is False
    for index in (4, 6):
        assert responses[index]["result"] == guard
    active = responses[7]["result"]
    assert active["documentFormat"] == "live2d.can3"
    for index in (9, 11, 13, 15, 17, 19, 21, 23):
        assert responses[index]["result"] == active


def check_dirty_history(core, expected, model_path, guid):
    before = core.request("snapshot")
    choices = [(s, t, c, p) for s in before["model"]["scenes"]
               for t in s["tracks"] for c in t["parameterCurves"] for p in c["points"]]
    scene, track, curve, point = next((row for row in choices if row[3]["anchor"]["frame"] < 0),
                                     choices[0])
    edited = core.request(
        "live2d.animation.key.set", sceneGuid=scene["guid"], trackGuid=track["guid"],
        curveId=curve["id"], anchorFrame=point["anchor"]["frame"],
        value=max(p["anchor"]["value"] for p in curve["points"]) + 1.125,
        expectedRevision=before["revision"])
    assert edited["modified"] is True and edited["canUndo"] is True
    args = {"modelPath": str(model_path), "resourceGuid": guid}
    assert core.request("live2d.animation.bindings", **args) == expected
    assert core.request("snapshot") == edited
    undone = core.request("undo")
    assert undone["model"] == before["model"] and undone["canRedo"] is True
    assert core.request("live2d.animation.bindings", **args) == expected
    assert core.request("snapshot") == undone
    redone = core.request("redo")
    assert redone["model"] == edited["model"]
    assert core.request("live2d.animation.bindings", **args) == expected
    assert core.request("snapshot") == redone


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, default=CMO.DEFAULT_INPUTS)
    args = parser.parse_args()
    evidence = Path(tempfile.mkdtemp(prefix="can3-bindings-", dir=ROOT / "build/verification"))
    result = {"status": "FAIL", "evidence": str(evidence), "workflows": [],
              "limits": ["Identity matching only; no native playback or vendor equivalence"]}
    binary = ROOT / "out/rust/debug/soul2d-core"
    library = ROOT / "out/rust/debug" / (
        "libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    try:
        inputs = {}
        for name, digest in HASHES.items():
            paths = list(args.inputs.rglob(name))
            assert len(paths) == 1, (name, paths)
            path = paths[0].resolve()
            assert CMO.sha(path) == digest, ("Original fixture hash differs", path)
            inputs[name] = path
        result["binarySha256"], result["librarySha256"] = CMO.sha(binary), CMO.sha(library)
        result["sourceHashes"] = {str(inputs[name]): digest for name, digest in HASHES.items()}
        model_names = list(PAIRS.values())
        for index, (animation_name, model_name) in enumerate(PAIRS.items()):
            animation_path, model_path = inputs[animation_name], inputs[model_name]
            wrong_model_path = inputs[model_names[(index + 1) % len(model_names)]]
            guid = resource_guid(animation_path.read_bytes())
            expected = binding_oracle(animation_path.read_bytes(), model_path.read_bytes(), guid)
            modern_parts, skipped_legacy_parts, total_part_channels = part_opacity_counts(
                animation_path.read_bytes(), guid)
            assert expected["bindings"], animation_name
            assert expected["partCurveCount"] == modern_parts
            assert total_part_channels == modern_parts + skipped_legacy_parts
            try:
                binding_oracle(animation_path.read_bytes(), wrong_model_path.read_bytes(), guid)
            except (AssertionError, KeyError):
                pass
            else:
                raise AssertionError("Wrong-model negative control is not discriminating")
            (evidence / f"{animation_path.stem}-expected.json").write_text(
                json.dumps(expected, indent=2, ensure_ascii=False) + "\n")
            requests = workflow(animation_path, model_path, wrong_model_path, guid)
            for transport in ("ffi", "jsonl", "mcp"):
                core = CMO.CORE.Core(library) if transport == "ffi" else None
                try:
                    responses, wire = EDIT.run_transport(binary, core, requests, transport)
                    (evidence / f"{animation_path.stem}-{transport}.jsonl").write_text(wire)
                    check_workflow(responses, expected)
                    if core is not None:
                        check_dirty_history(core, expected, model_path, guid)
                    result["workflows"].append({
                        "source": str(animation_path), "modelPath": str(model_path),
                        "resourceGuid": guid, "transport": transport, "status": "PASS",
                        "bindingCount": len(expected["bindings"]),
                        "partCurveCount": modern_parts,
                        "skippedLegacyPartOpacityCount": skipped_legacy_parts,
                        "partOpacityMapChannelCount": total_part_channels,
                        "negativeControls": 8, "activeSessionUnchanged": True,
                    })
                finally:
                    if core is not None:
                        core.close()
        for name, digest in HASHES.items():
            assert CMO.sha(inputs[name]) == digest
        assert CMO.sha(binary) == result["binarySha256"]
        assert CMO.sha(library) == result["librarySha256"]
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        (evidence / "failure.txt").write_text(traceback.format_exc())
    (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
