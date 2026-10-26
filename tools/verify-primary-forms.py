#!/usr/bin/env python3
"""Verify primary-form interpolation against raw CMO3 XML over all transports."""
import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUTS = ROOT / "build/verification/live2d-inputs-vjq5llby"
OP = "live2d.authoring.primary_forms"
MCP_TOOL = "live2d_authoring_primary_forms"
sys.dont_write_bytecode = True


def import_tool(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


WEIGHTS = import_tool("verify_primary_weights", ROOT / "tools/verify-primary-weights.py")
CMO, CAFF, CORE = WEIGHTS.CMO, WEIGHTS.CAFF, WEIGHTS.CORE
REAL_PROBES = (
    {"label": "ArtMesh3-draw-order", "file": "hiyori_pro_t11.cmo3",
     "source": "ArtMesh3", "parameter": "ParamCheek", "lower": 0.0,
     "upper": 1.0, "fraction": 0.25, "varying": ("drawOrder",),
     "rawDrawOrder": (650, 600), "expectedDrawOrder": 637.5},
    {"label": "Warp46-positions", "file": "hiyori_pro_t11.cmo3",
     "source": "Warp46", "parameter": "ParamBodyAngleX", "lower": -10.0,
     "upper": 0.0, "fraction": 0.25, "varying": ("data.positions",)},
    {"label": "Rotation23-transform", "file": "hiyori_pro_t11.cmo3",
     "source": "Rotation23", "parameter": "ParamLeg", "lower": 0.0,
     "upper": 1.0, "fraction": 0.25,
     "varying": ("data.angle", "data.origin", "data.scale")},
    {"label": "Ren-PartAll-opacity", "file": "ren_t01.cmo3",
     "source": "PartAll", "parameter": "ParamHologram", "lower": 0.0,
     "upper": 1.0, "fraction": 0.25, "varying": ("opacity",)},
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def weighted_number(forms, weights, field):
    if len(forms) != len(weights):
        raise ValueError(f"primary form field {field} has inconsistent corner counts")
    value = 0.0
    for form, item in zip(forms, weights):
        value += form[field] * item["weight"]
    if not math.isfinite(value):
        raise ValueError(f"non-finite weighted form field {field}")
    return value


def weighted_array(forms, weights, field):
    if len(forms) != len(weights):
        raise ValueError(f"primary form field {field} has inconsistent corner counts")
    values = [form[field] for form in forms]
    if not values or any(len(value) != len(values[0]) for value in values):
        raise ValueError(f"primary form field {field} has inconsistent lengths")
    result = [0.0] * len(values[0])
    for row, item in zip(values, weights):
        for index, value in enumerate(row):
            result[index] += value * item["weight"]
    if any(not math.isfinite(value) for value in result):
        raise ValueError(f"non-finite weighted form field {field}")
    return result


def primary_corner_forms(source, weights):
    if not weights:
        raise ValueError(f"primary grid for {source['id']} produced no positive weights")

    active_guids = {item["formGuid"] for item in weights}
    forms_by_guid = {}
    for form in source["forms"]:
        if form["guid"] in active_guids:
            forms_by_guid.setdefault(form["guid"], {})[form["coordinateType"]] = form

    first_guid = weights[0]["formGuid"]
    first_corner = forms_by_guid.get(first_guid, {})
    coordinate_types = list(first_corner)
    if not coordinate_types:
        raise ValueError(f"first primary corner for {source['id']} has no owner-local forms")
    expected_types = set(coordinate_types)
    for item in weights:
        corner = forms_by_guid.get(item["formGuid"], {})
        if set(corner) != expected_types:
            raise ValueError(
                f"primary corners for {source['id']} have different coordinate type sets")

    return [(coordinate_type,
             [forms_by_guid[item["formGuid"]][coordinate_type] for item in weights])
            for coordinate_type in coordinate_types]


def evaluated_forms(source, weights):
    if source["grid"] is None:
        return None
    result = []
    for coordinate_type, corner_forms in primary_corner_forms(source, weights):
        first = corner_forms[0]
        draw_orders = [form["drawOrder"] for form in corner_forms]
        if any((value is None) != (draw_orders[0] is None) for value in draw_orders):
            raise ValueError(f"primary drawOrder presence differs for {source['id']}")
        data_type = first["data"]["type"]
        if data_type in ("ArtMesh", "Warp"):
            data = {"type": data_type, "positions": weighted_array(
                [form["data"] for form in corner_forms], weights, "positions")}
        elif data_type == "Rotation":
            for field in ("reflectX", "reflectY"):
                flags = [form["data"][field] for form in corner_forms]
                if any(flag != flags[0] for flag in flags):
                    raise ValueError(f"primary {field} differs for {source['id']}")
            data = {
                "type": "Rotation",
                "angle": weighted_number(
                    [{"angle": form["data"]["angle"]} for form in corner_forms], weights, "angle"),
                "origin": weighted_array(
                    [{"origin": form["data"]["origin"]} for form in corner_forms], weights, "origin"),
                "scale": weighted_number(
                    [{"scale": form["data"]["scale"]} for form in corner_forms], weights, "scale"),
                "reflectX": first["data"]["reflectX"],
                "reflectY": first["data"]["reflectY"],
            }
        elif data_type == "Part":
            data = {"type": "Part"}
        elif data_type == "Glue":
            data = {"type": "Glue", "intensity": weighted_number(
                [form["data"] for form in corner_forms], weights, "intensity")}
        else:
            raise ValueError(f"unsupported primary form data type {data_type}")

        result.append({
            "coordinateType": coordinate_type,
            "opacity": weighted_number(corner_forms, weights, "opacity"),
            "drawOrder": None if draw_orders[0] is None else weighted_number(
                [{"drawOrder": value} for value in draw_orders], weights, "drawOrder"),
            "multiplyColor": weighted_array(corner_forms, weights, "multiplyColor"),
            "screenColor": weighted_array(corner_forms, weights, "screenColor"),
            "data": data,
        })
    return result


def expected_response(scene, overrides=None, source_guid=None):
    weighted = WEIGHTS.expected_response(scene, overrides, source_guid)
    sources = [source for source in scene["sources"]
               if source_guid is None or source["guid"] == source_guid]
    if len(sources) != len(weighted["sources"]):
        raise AssertionError("weights oracle returned a different source set")
    result = []
    for source, weight_source in zip(sources, weighted["sources"]):
        result.append({
            "guid": source["guid"],
            "id": source["id"],
            "parentPartGuid": source["parentPartGuid"],
            "targetDeformerGuid": source["targetDeformerGuid"],
            "axisValues": weight_source["axisValues"],
            "weights": weight_source["weights"],
            "evaluatedForms": evaluated_forms(source, weight_source["weights"]),
        })
    return {"format": "live2d.authoring.primary-forms.v1", "sources": result}


def make_real_probe(item, probe):
    scene = item["scene"]
    source = next((source for source in scene["sources"]
                   if source["id"] == probe["source"]), None)
    if source is None or source["grid"] is None:
        raise AssertionError(f"missing primary form probe source {probe['source']}")
    axis = next((axis for axis in source["grid"]["axes"]
                 if axis["parameterId"] == probe["parameter"]), None)
    if axis is None or not any(left == probe["lower"] and right == probe["upper"]
                               for left, right in zip(axis["keys"], axis["keys"][1:])):
        raise AssertionError(f"{probe['source']} lacks the recorded key interval")
    sample = probe["lower"] + (probe["upper"] - probe["lower"]) * probe["fraction"]
    overrides = {probe["parameter"]: sample}
    weight_source = WEIGHTS.expected_source(
        source, {parameter["id"]: parameter for parameter in scene["parameters"]}, overrides)
    if len(weight_source["weights"]) != 2:
        raise AssertionError(f"{probe['label']} did not select exactly two corners")
    groups = primary_corner_forms(source, weight_source["weights"])

    def field_value(form, field):
        if field.startswith("data."):
            return form["data"][field.removeprefix("data.")]
        return form[field]

    for field in probe["varying"]:
        if not any(any(field_value(form, field) != field_value(corners[0], field)
                       for form in corners[1:]) for _, corners in groups):
            raise AssertionError(f"{probe['label']} does not vary {field} in the raw forms")
    if "rawDrawOrder" in probe:
        if any(tuple(form["drawOrder"] for form in corners) != probe["rawDrawOrder"]
               for _, corners in groups):
            raise AssertionError(f"{probe['label']} raw drawOrder corners changed")

    expected = expected_response(scene, overrides, source["guid"])
    if len(expected["sources"]) != 1:
        raise AssertionError(f"{probe['label']} was not source-filtered")
    if "expectedDrawOrder" in probe and any(
        form["drawOrder"] != probe["expectedDrawOrder"]
        for form in expected["sources"][0]["evaluatedForms"]
    ):
        raise AssertionError(f"{probe['label']} did not produce literal drawOrder 637.5")
    return {"label": probe["label"], "args": WEIGHTS.request_args(
                item["path"], overrides, source["guid"]), "expected": expected,
            "sourceId": source["id"], "parameterValues": overrides,
            "varyingFields": list(probe["varying"])}


def compare_value(actual, expected, path="$"):
    if isinstance(expected, bool):
        assert type(actual) is bool and actual == expected, (path, actual, expected)
    elif isinstance(expected, (int, float)):
        assert isinstance(actual, (int, float)) and not isinstance(actual, bool), (
            path, actual, expected)
        assert math.isclose(actual, expected, rel_tol=0.0, abs_tol=1e-12), (
            path, actual, expected)
    elif isinstance(expected, dict):
        assert isinstance(actual, dict) and set(actual) == set(expected), (path, actual, expected)
        for key in expected:
            compare_value(actual[key], expected[key], f"{path}.{key}")
    elif isinstance(expected, list):
        assert isinstance(actual, list) and len(actual) == len(expected), (path, actual, expected)
        for index, (actual_item, expected_item) in enumerate(zip(actual, expected)):
            compare_value(actual_item, expected_item, f"{path}[{index}]")
    else:
        assert actual == expected, (path, actual, expected)


def compare_response(actual, expected):
    compare_value(actual, expected)


def self_check():
    def form(guid, coordinate_type, start, end):
        return {
            "guid": guid, "coordinateType": coordinate_type, "opacity": start,
            "drawOrder": 100 + start, "multiplyColor": [start] * 4,
            "screenColor": [end] * 4,
            "data": {"type": "ArtMesh", "positions": [start, end]},
        }

    source = {"id": "fixture", "grid": {}, "forms": [
        form("a", "y", 0.0, 1.0), form("a", "x", 10.0, 11.0),
        form("b", "x", 14.0, 15.0), form("b", "y", 4.0, 5.0),
    ]}
    result = evaluated_forms(source, [
        {"formGuid": "a", "weight": 0.25}, {"formGuid": "b", "weight": 0.75}])
    assert [item["coordinateType"] for item in result] == ["y", "x"]
    assert result[0]["data"]["positions"] == [3.0, 4.0]
    assert result[1]["data"]["positions"] == [13.0, 14.0]
    assert "animated" not in result[0] and "localAnimated" not in result[0]

    missing = {**source, "forms": source["forms"][:-1]}
    try:
        evaluated_forms(missing, [
            {"formGuid": "a", "weight": 0.25}, {"formGuid": "b", "weight": 0.75}])
    except ValueError:
        pass
    else:
        raise AssertionError("self-check accepted different corner coordinate type sets")


def source_manifest(library, binary):
    paths = [
        Path(__file__).resolve(), Path(WEIGHTS.__file__).resolve(),
        Path(CMO.__file__).resolve(), Path(CAFF.__file__).resolve(),
        Path(CORE.__file__).resolve(),
        ROOT / "rust/src/authoring_forms.rs", ROOT / "rust/src/authoring_scene.rs",
        ROOT / "rust/src/authoring_keyforms.rs", ROOT / "rust/src/authoring.rs",
        ROOT / "rust/src/authoring_graph.rs", ROOT / "rust/src/authoring_values.rs",
        ROOT / "rust/src/authoring_hierarchy.rs",
        ROOT / "rust/src/authoring_edit.rs", ROOT / "rust/src/document.rs",
        ROOT / "rust/src/caff.rs", ROOT / "rust/src/lib.rs", ROOT / "rust/src/main.rs",
        library, binary,
    ]
    return {str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path):
            sha(path) if path.is_file() else None for path in paths}


def main():
    self_check()
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="?", type=Path, default=DEFAULT_INPUTS)
    args = parser.parse_args()
    evidence_root = ROOT / "build/verification"
    evidence_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="primary-forms-", dir=evidence_root))
    library = ROOT / "out/rust/debug" / (
        "libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    binary = ROOT / "out/rust/debug/soul2d-core"
    result = {"status": "FAIL", "evidence": str(directory), "models": [],
              "verifiedResponses": [], "negativeControls": [], "fiveAxisQueries": [],
              "realDataProbes": []}
    core = None
    try:
        paths = sorted(path.resolve() for path in args.inputs.rglob("*.cmo3"))
        if len(paths) != 4:
            raise AssertionError(f"expected 4 CMO3 inputs, found {len(paths)}")
        original_hashes = {str(path): sha(path) for path in paths}
        parsed = []
        for path in paths:
            entries, xml = CAFF.decode(path.read_bytes())
            scene = CMO.read_scene(xml, {entry[0][0] for entry in entries})
            parsed.append({"path": path, "scene": scene, "entries": len(entries),
                           "xmlBytes": len(xml)})

        zundamon_models = [item for item in parsed if any(
            source["grid"] is not None and len(source["grid"]["axes"]) == 5
            for source in item["scene"]["sources"])]
        if len(zundamon_models) != 1:
            raise AssertionError("expected exactly one five-axis Zundamon model")
        zundamon = zundamon_models[0]
        five_axis = [source for source in zundamon["scene"]["sources"]
                     if source["grid"] is not None and len(source["grid"]["axes"]) == 5]
        if len(five_axis) != 3 or {source["id"] for source in five_axis} != {
            "ArtMesh149", "ArtMesh151", "ArtMesh154"
        }:
            raise AssertionError("expected Zundamon ArtMesh149/151/154 five-axis grids")

        active_path = directory / "active-document.inx"
        CORE.fixture(active_path)
        core = CORE.Core(library)
        core.request("open", path=str(active_path))
        core.request("node.rename", id=2, name="Preserve active authored model")
        active_snapshot = core.request("snapshot")
        active_hash = sha(active_path)
        assert CORE.find_child(active_snapshot["model"]["nodes"], 2)["name"] == \
            "Preserve active authored model"

        mcp_plan = []
        for item in parsed:
            path, scene = item["path"], item["scene"]
            defaults = expected_response(scene)
            weight_defaults = WEIGHTS.expected_response(scene)
            if any(source["grid"] is not None and
                   (len(row["weights"]) != 1 or row["weights"][0]["weight"] != 1.0)
                   for source, row in zip(scene["sources"], weight_defaults["sources"])):
                raise AssertionError(f"default values are not exact stored corners in {path.name}")
            cases = [{"label": "defaults", "args": WEIGHTS.request_args(path),
                      "expected": defaults}]
            if path == zundamon["path"]:
                for source in five_axis:
                    for label, overrides in WEIGHTS.FIVE_AXIS_OVERRIDES:
                        expected = expected_response(scene, overrides, source["guid"])
                        if len(expected["sources"]) != 1 or \
                                len(expected["sources"][0]["weights"]) != 32:
                            raise AssertionError(f"{source['id']} {label} query is not 32 corners")
                        position = WEIGHTS.weighted_vertex_zero(
                            source, expected["sources"][0])
                        known = WEIGHTS.EXPECTED_VERTEX_ZERO[(source["id"], label)]
                        assert all(math.isclose(value, target, rel_tol=0.0, abs_tol=1e-12)
                                   for value, target in zip(position, known)), (
                                       source["id"], label, position, known)
                        cases.append({"label": f"{source['id']}-{label}",
                                      "args": WEIGHTS.request_args(path, overrides, source["guid"]),
                                      "expected": expected})
                        result["fiveAxisQueries"].append({
                            "sourceId": source["id"], "sourceGuid": source["guid"],
                            "sample": label, "parameterValues": overrides,
                            "positiveCorners": len(expected["sources"][0]["weights"]),
                            "weightedVertex0": position,
                        })
            for probe in REAL_PROBES:
                if path.name == probe["file"]:
                    probe_case = make_real_probe(item, probe)
                    cases.append(probe_case)
                    result["realDataProbes"].append({
                        "label": probe_case["label"], "sourceId": probe_case["sourceId"],
                        "parameterValues": probe_case["parameterValues"],
                        "varyingFields": probe_case["varyingFields"],
                        "positiveCorners": len(probe_case["expected"]["sources"][0]["weights"]),
                        "expectedDrawOrder": probe.get("expectedDrawOrder"),
                    })

            for case in cases:
                request = {"op": OP, **case["args"]}
                ffi_response = WEIGHTS.ffi_request(core, request)
                assert ffi_response.get("succeeded") is True, ffi_response
                compare_response(ffi_response["result"], case["expected"])
                assert core.request("snapshot") == active_snapshot, (
                    f"FFI {case['label']} changed the active INX snapshot")

                jsonl_result, jsonl_wire = WEIGHTS.jsonl_request(binary, request)
                compare_response(jsonl_result, case["expected"])
                for transport, response in (("ffi", ffi_response["result"]),
                                            ("jsonl", jsonl_result)):
                    summary = WEIGHTS.response_summary(path.name, case["label"], transport,
                                                       response)
                    result["verifiedResponses"].append(summary)
                case["jsonlWireBytes"] = len(jsonl_wire.encode())
            mcp_plan.append((path.name, cases))
            result["models"].append({
                "source": str(path), "sha256": original_hashes[str(path)],
                "containerEntries": item["entries"], "xmlBytes": item["xmlBytes"],
                "sourceCount": len(scene["sources"]),
                "primaryGridCount": sum(source["grid"] is not None
                                         for source in scene["sources"]),
            })

        for path_name, cases in mcp_plan:
            mcp_values, schema, mcp_wire = WEIGHTS.mcp_requests(
                binary, [case["args"] for case in cases], MCP_TOOL)
            result["mcpSchema"] = schema
            for case, response in zip(cases, mcp_values):
                assert response["succeeded"], response
                compare_response(response["result"], case["expected"])
                result["verifiedResponses"].append(WEIGHTS.response_summary(
                    path_name, case["label"], "stdio-mcp", response["result"]))
            result.setdefault("mcpWireBytes", 0)
            result["mcpWireBytes"] += len(mcp_wire.encode())

        target = next(source for source in five_axis if source["id"] == "ArtMesh149")
        negative_requests = [
            ("unknown-parameter", WEIGHTS.request_args(
                zundamon["path"], {"NoSuchParameter": 0.0})),
            ("wrong-parameterValues-type", WEIGHTS.request_args(
                zundamon["path"], [0.0])),
            ("nonnumeric-value", WEIGHTS.request_args(
                zundamon["path"], {"ParamAngleX": "15"})),
            ("nonfinite-json-number", WEIGHTS.request_args(
                zundamon["path"], {"ParamAngleX": WEIGHTS.RAW_NUMBER})),
            ("outside-key-range", WEIGHTS.request_args(
                zundamon["path"], {"ParamAngleX": 31.0}, target["guid"])),
            ("unknown-source", WEIGHTS.request_args(
                zundamon["path"], source_guid="missing-source-guid")),
        ]
        for label, arguments in negative_requests:
            response = WEIGHTS.ffi_request(core, {"op": OP, **arguments})
            assert response.get("succeeded") is False, (label, response)
            assert core.request("snapshot") == active_snapshot, (
                f"FFI negative control {label} changed the active INX snapshot")
            result["negativeControls"].append({"case": label, "transport": "ffi",
                                               "message": response.get("message", "")})

        for label, arguments in negative_requests:
            WEIGHTS.jsonl_request(binary, {"op": OP, **arguments}, success=False)
            result["negativeControls"].append({"case": label, "transport": "jsonl"})
        mcp_negative, _, _ = WEIGHTS.mcp_requests(
            binary, [arguments for _, arguments in negative_requests], MCP_TOOL)
        if len(mcp_negative) != len(negative_requests):
            raise AssertionError("stdio MCP negative responses are incomplete")
        for (label, _), response in zip(negative_requests, mcp_negative):
            assert not response["succeeded"], (label, response)
            result["negativeControls"].append({"case": label, "transport": "stdio-mcp",
                                               "message": response.get("message", "")})

        final_hashes = {str(path): sha(path) for path in paths}
        assert final_hashes == original_hashes, "a source CMO3 changed during verification"
        assert sha(active_path) == active_hash, "active INX fixture file changed during verification"
        assert core.request("snapshot") == active_snapshot, "active INX snapshot changed"
        result["originalSourceHashes"] = original_hashes
        result["activeInx"] = {"path": str(active_path), "sha256": active_hash,
                               "renamedNode": "Preserve active authored model",
                               "snapshotPreserved": True}
        result["sourceBinaryHashes"] = source_manifest(library, binary)
        result["scope"] = "Owner-local primary keyform values and geometry only."
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = str(error)
    finally:
        if core is not None:
            core.close()
        (directory / "result.json").write_text(
            json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False) + "\n")
    print(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
