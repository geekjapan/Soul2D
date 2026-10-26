#!/usr/bin/env python3
"""Verify primary linear keyform weights against raw CMO3 XML over all transports."""
import argparse
import bisect
import ctypes
import hashlib
import importlib.util
import itertools
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUTS = ROOT / "build/verification/live2d-inputs-vjq5llby"
OP = "live2d.authoring.linear_weights"
MCP_TOOL = "live2d_authoring_linear_weights"
RAW_NUMBER = "__PRIMARY_WEIGHTS_RAW_1E309__"
sys.dont_write_bytecode = True


def import_tool(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CMO = import_tool("verify_cmo_source", ROOT / "tools/verify-cmo-source.py")
CAFF = import_tool("verify_caff_authoring", ROOT / "tools/verify-caff-authoring.py")
CORE = import_tool("verify_rust_core", ROOT / "tools/verify-rust-core.py")


FIVE_AXIS_OVERRIDES = (
    ("symmetric", {
        "ParamMouthOpenY": 0.7, "ParamMouthForm": 0.5,
        "ParamPatternMouth": 0.5, "ParamcheekPuff": 0.4, "ParamAngleX": 15.0,
    }),
    ("asymmetric", {
        "ParamMouthOpenY": 0.7, "ParamMouthForm": 0.25,
        "ParamPatternMouth": -0.4, "ParamcheekPuff": 0.3, "ParamAngleX": 12.0,
    }),
)
EXPECTED_VERTEX_ZERO = {
    ("ArtMesh149", "symmetric"): (0.8531014521875, 0.5350316075),
    ("ArtMesh149", "asymmetric"): (0.87505805696875, 0.60902047071875),
    ("ArtMesh151", "symmetric"): (0.83334404625, 0.45066513903125),
    ("ArtMesh151", "asymmetric"): (0.85809579846875, 0.4219526321125),
    ("ArtMesh154", "symmetric"): (0.37852786, 0.4889786121875),
    ("ArtMesh154", "asymmetric"): (0.37973238034375, 0.49486404003125),
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def response_summary(file_name, case, transport, response):
    encoded = json.dumps(response, sort_keys=True, separators=(",", ":"),
                         ensure_ascii=False, allow_nan=False).encode()
    return {
        "file": file_name, "case": case, "transport": transport,
        "sourceCount": len(response["sources"]),
        "positiveWeightCount": sum(len(source["weights"] or []) for source in response["sources"]),
        "sha256": hashlib.sha256(encoded).hexdigest(),
    }


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n")


def json_text(value):
    text = json.dumps(value, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    marker = json.dumps(RAW_NUMBER)
    if marker in text:
        text = text.replace(marker, "1e309")
    return text


def axis_bracket(keys, value):
    upper = bisect.bisect_left(keys, value)
    if upper < len(keys) and keys[upper] == value:
        return upper, upper, 1.0, 0.0
    if upper == 0 or upper == len(keys):
        raise ValueError("sample is outside its grid key range")
    lower = upper - 1
    lower_key, upper_key = keys[lower], keys[upper]
    span = upper_key - lower_key
    if math.isfinite(span):
        lower_weight = (upper_key - value) / span
        upper_weight = (value - lower_key) / span
    else:
        lower_half, upper_half, value_half = lower_key / 2.0, upper_key / 2.0, value / 2.0
        span = upper_half - lower_half
        lower_weight = (upper_half - value_half) / span
        upper_weight = (value_half - lower_half) / span
    if not (0.0 < lower_weight <= 1.0 and 0.0 < upper_weight <= 1.0):
        raise ValueError("sample has unrepresentable grid weights")
    return lower, upper, lower_weight, upper_weight


def expected_source(source, parameters, overrides):
    grid = source["grid"]
    if grid is None:
        return {"guid": source["guid"], "id": source["id"], "axisValues": [], "weights": None}

    axes = grid["axes"]
    brackets = []
    axis_values = []
    for axis in axes:
        if (axis["interpolation"], axis["extendedInterpolation"]) != ("LINEAR", "LINEAR"):
            raise AssertionError(f"unsupported primary interpolation in {source['id']}")
        parameter_id = axis["parameterId"]
        value = overrides[parameter_id] if parameter_id in overrides else parameters[parameter_id]["default"]
        keys = axis["keys"]
        try:
            brackets.append(axis_bracket(keys, value))
        except ValueError as error:
            raise ValueError(f"sample for {parameter_id} {error}") from error
        axis_values.append(value)

    required_coordinates = set(itertools.product(*[
        (lower,) if lower == upper else (lower, upper)
        for lower, upper, _, _ in brackets
    ]))
    cell_coordinates = {tuple(cell["indices"]) for cell in grid["cells"]}
    missing = required_coordinates - cell_coordinates
    if missing:
        raise AssertionError(f"{source['id']} lacks positive-weight corners: {sorted(missing)}")

    weights = []
    for cell_index, cell in enumerate(grid["cells"]):
        weight = 1.0
        for coordinate, (lower, upper, lower_weight, upper_weight) in zip(
            cell["indices"], brackets):
            if lower == upper:
                factor = lower_weight if coordinate == lower else 0.0
            elif coordinate == lower:
                factor = lower_weight
            elif coordinate == upper:
                factor = upper_weight
            else:
                factor = 0.0
            if factor == 0.0:
                weight = 0.0
                break
            weight *= factor
        if weight > 0.0:
            weights.append({"cellIndex": cell_index, "formGuid": cell["formGuid"], "weight": weight})
    if len(weights) != len(required_coordinates):
        raise AssertionError(f"{source['id']} did not produce every positive-weight corner")
    return {"guid": source["guid"], "id": source["id"], "axisValues": axis_values, "weights": weights}


def expected_response(scene, overrides=None, source_guid=None):
    parameters = {parameter["id"]: parameter for parameter in scene["parameters"]}
    return {
        "format": "live2d.authoring.primary-weights.v1",
        "sources": [expected_source(source, parameters, overrides or {})
                    for source in scene["sources"]
                    if source_guid is None or source["guid"] == source_guid],
    }


def compare_response(actual, expected):
    assert set(actual) == {"format", "sources"}, actual
    assert actual["format"] == expected["format"], actual["format"]
    assert len(actual["sources"]) == len(expected["sources"]), (
        len(actual["sources"]), len(expected["sources"]))
    for actual_source, expected_source_row in zip(actual["sources"], expected["sources"]):
        assert set(actual_source) == {"guid", "id", "axisValues", "weights"}, actual_source
        for field in ("guid", "id"):
            assert actual_source[field] == expected_source_row[field], (field, actual_source, expected_source_row)
        assert actual_source["axisValues"] == expected_source_row["axisValues"], (
            actual_source["axisValues"], expected_source_row["axisValues"])
        actual_weights = actual_source["weights"]
        expected_weights = expected_source_row["weights"]
        if expected_weights is None:
            assert actual_weights is None, actual_weights
            continue
        assert isinstance(actual_weights, list), actual_weights
        assert len(actual_weights) == len(expected_weights), (actual_weights, expected_weights)
        for actual_weight, expected_weight in zip(actual_weights, expected_weights):
            assert set(actual_weight) == {"cellIndex", "formGuid", "weight"}, actual_weight
            assert actual_weight["cellIndex"] == expected_weight["cellIndex"], (
                actual_weight, expected_weight)
            assert actual_weight["formGuid"] == expected_weight["formGuid"], (
                actual_weight, expected_weight)
            assert math.isclose(actual_weight["weight"], expected_weight["weight"],
                                rel_tol=0.0, abs_tol=1e-12), (actual_weight, expected_weight)


def weighted_vertex_zero(source, source_weights):
    forms = {form["guid"]: form for form in source["forms"]}
    position = [0.0, 0.0]
    for item in source_weights["weights"]:
        form = forms[item["formGuid"]]
        coordinates = form["data"]["positions"]
        position[0] += coordinates[0] * item["weight"]
        position[1] += coordinates[1] * item["weight"]
    return position


def ffi_request(core, request):
    payload = json_text(request).encode()
    raw = core.lib.vsoul_session_request(core.handle, payload)
    if not raw:
        raise RuntimeError("Rust FFI returned a null response")
    try:
        return json.loads(ctypes.string_at(raw))
    finally:
        core.lib.vsoul_string_free(raw)


def jsonl_request(binary, request, success=True):
    process = subprocess.run([str(binary)], input=json_text(request) + "\n", text=True,
                             capture_output=True, timeout=120)
    lines = [line for line in process.stdout.splitlines() if line.strip()]
    if process.returncode or len(lines) != 1:
        raise RuntimeError(f"JSONL request failed: {process.stderr}")
    response = json.loads(lines[0])
    assert response.get("succeeded") is success, response
    return response.get("result"), process.stdout + process.stderr


def mcp_requests(binary, requests, tool_name=MCP_TOOL):
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "primary-weights-verifier", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    for index, request in enumerate(requests):
        messages.append({"jsonrpc": "2.0", "id": index + 3, "method": "tools/call",
                         "params": {"name": tool_name, "arguments": request}})
    process = subprocess.run([str(binary), "--mcp"],
                             input="".join(json_text(message) + "\n" for message in messages),
                             text=True, capture_output=True, timeout=120)
    if process.returncode:
        raise RuntimeError(f"stdio MCP process failed: {process.stderr}")
    responses = {row["id"]: row for row in
                 (json.loads(line) for line in process.stdout.splitlines() if line.strip())
                 if "id" in row}
    if not {1, 2, *range(3, len(requests) + 3)} <= responses.keys():
        raise RuntimeError("stdio MCP responses are incomplete")
    tools = {tool["name"]: tool for tool in responses[2]["result"]["tools"]}
    tool = tools.get(tool_name)
    if tool is None:
        raise RuntimeError(f"stdio MCP does not advertise {tool_name}")
    schema = tool["inputSchema"]
    properties = schema.get("properties", {})
    assert schema.get("required") == [], schema
    assert set(properties) == {"path", "sourceGuid", "parameterValues"}, schema
    assert properties["path"].get("type") == "string", schema
    assert properties["sourceGuid"].get("type") == "string", schema
    assert properties["parameterValues"] == {
        "type": "object", "additionalProperties": {"type": "number"}}, schema

    results = []
    for index in range(len(requests)):
        response = responses[index + 3]["result"]
        if response.get("isError"):
            results.append({"succeeded": False,
                            "message": response.get("content", [{}])[0].get("text", "")})
            continue
        value = response.get("structuredContent")
        if value is None:
            value = json.loads(next(item["text"] for item in response["content"]
                                    if item.get("type") == "text"))
        if isinstance(value, dict) and set(value) == {"result"}:
            value = value["result"]
        results.append({"succeeded": True, "result": value})
    return results, schema, process.stdout + process.stderr


def request_args(path, overrides=None, source_guid=None):
    result = {"path": str(path)}
    if overrides is not None:
        result["parameterValues"] = overrides
    if source_guid is not None:
        result["sourceGuid"] = source_guid
    return result


def source_manifest(library, binary):
    paths = [
        Path(__file__).resolve(), Path(CMO.__file__).resolve(), Path(CAFF.__file__).resolve(),
        Path(CORE.__file__).resolve(), ROOT / "rust/src/authoring_keyforms.rs",
        ROOT / "rust/src/authoring_scene.rs", ROOT / "rust/src/authoring_graph.rs",
        ROOT / "rust/src/authoring_hierarchy.rs",
        ROOT / "rust/src/authoring_edit.rs", ROOT / "rust/src/document.rs",
        ROOT / "rust/src/authoring_values.rs", ROOT / "rust/src/caff.rs",
        ROOT / "rust/src/authoring.rs", ROOT / "rust/src/lib.rs", ROOT / "rust/src/main.rs",
        library, binary,
    ]
    return {str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path):
            sha(path) if path.is_file() else None for path in paths}


def main():
    bracket = axis_bracket([-1.0, 1.0], math.nextafter(1.0, 0.0))
    assert bracket[0:2] == (0, 1) and bracket[2] > 0.0 and bracket[3] == 1.0
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="?", type=Path, default=DEFAULT_INPUTS)
    args = parser.parse_args()
    evidence_root = ROOT / "build/verification"
    evidence_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="primary-weights-", dir=evidence_root))
    library = ROOT / "out/rust/debug" / (
        "libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    binary = ROOT / "out/rust/debug/soul2d-core"
    result = {"status": "FAIL", "evidence": str(directory), "models": [],
              "verifiedResponses": [], "negativeControls": [], "fiveAxisQueries": []}
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
        source_counts = [len(item["scene"]["sources"]) for item in parsed]
        grid_counts = [sum(source["grid"] is not None
                           for source in item["scene"]["sources"]) for item in parsed]
        zero_axis_count = sum(source["grid"] is not None and not source["grid"]["axes"]
                              for item in parsed for source in item["scene"]["sources"])
        if source_counts != [151, 271, 400, 439] or grid_counts != source_counts:
            raise AssertionError(f"unexpected source/grid counts: {source_counts}, {grid_counts}")
        if zero_axis_count != 367:
            raise AssertionError(f"expected 367 zero-axis grids, found {zero_axis_count}")

        five_axis_models = []
        for item in parsed:
            for source in item["scene"]["sources"]:
                if source["grid"] is not None and len(source["grid"]["axes"]) == 5:
                    five_axis_models.append((item, source))
        if len(five_axis_models) != 3 or {source["id"] for _, source in five_axis_models} != {
            "ArtMesh149", "ArtMesh151", "ArtMesh154"
        }:
            raise AssertionError("expected the three Zundamon 5-axis ArtMesh149/151/154 grids")

        active_path = directory / "active-document.inx"
        CORE.fixture(active_path)
        core = CORE.Core(library)
        core.request("open", path=str(active_path))
        core.request("node.rename", id=2, name="Preserve active authored model")
        active_snapshot = core.request("snapshot")
        assert CORE.find_child(active_snapshot["model"]["nodes"], 2)["name"] == \
            "Preserve active authored model"

        mcp_plan = []
        for item in parsed:
            path, scene = item["path"], item["scene"]
            defaults = expected_response(scene)
            if any(len(source["grid"]["axes"]) != 0 and
                   (len(row["weights"]) != 1 or row["weights"][0]["weight"] != 1.0)
                   for source, row in zip(scene["sources"], defaults["sources"])):
                raise AssertionError(f"default values are not exact stored corners in {path.name}")
            cases = [{"label": "defaults", "args": request_args(path), "expected": defaults}]
            five_axis_rows = [source for source in scene["sources"]
                              if source["grid"] is not None and len(source["grid"]["axes"]) == 5]
            for source in five_axis_rows:
                for label, overrides in FIVE_AXIS_OVERRIDES:
                    expected = expected_response(scene, overrides, source["guid"])
                    if len(expected["sources"]) != 1 or len(expected["sources"][0]["weights"]) != 32:
                        raise AssertionError(f"{source['id']} {label} query is not 32 corners")
                    position = weighted_vertex_zero(source, expected["sources"][0])
                    known = EXPECTED_VERTEX_ZERO[(source["id"], label)]
                    assert all(math.isclose(value, target, rel_tol=0.0, abs_tol=1e-12)
                               for value, target in zip(position, known)), (
                                   source["id"], label, position, known)
                    cases.append({"label": f"{source['id']}-{label}",
                                  "args": request_args(path, overrides, source["guid"]),
                                  "expected": expected})
                    result["fiveAxisQueries"].append({
                        "sourceId": source["id"], "sourceGuid": source["guid"],
                        "sample": label, "parameterValues": overrides,
                        "positiveCorners": len(expected["sources"][0]["weights"]),
                        "weightedVertex0": position,
                    })

            for case in cases:
                request = {"op": OP, **case["args"]}
                ffi_response = ffi_request(core, request)
                assert ffi_response.get("succeeded") is True, ffi_response
                ffi_result = ffi_response["result"]
                compare_response(ffi_result, case["expected"])
                assert core.request("snapshot") == active_snapshot, (
                    f"FFI {case['label']} changed the active INX snapshot")

                jsonl_result, jsonl_wire = jsonl_request(binary, request)
                compare_response(jsonl_result, case["expected"])
                result["verifiedResponses"].append(
                    response_summary(path.name, case["label"], "ffi", ffi_result))
                result["verifiedResponses"].append(
                    response_summary(path.name, case["label"], "jsonl", jsonl_result))
                case["jsonlWireBytes"] = len(jsonl_wire.encode())
            mcp_plan.append((path.name, cases))
            result["models"].append({
                "source": str(path), "sha256": original_hashes[str(path)],
                "containerEntries": item["entries"], "xmlBytes": item["xmlBytes"],
                "sourceCount": len(scene["sources"]), "gridCount": len(defaults["sources"]),
                "zeroAxisGridCount": sum(source["grid"] is not None and
                                          not source["grid"]["axes"]
                                          for source in scene["sources"]),
            })

        for path_name, cases in mcp_plan:
            mcp_values, schema, mcp_wire = mcp_requests(
                binary, [case["args"] for case in cases])
            result["mcpSchema"] = schema
            for case, response in zip(cases, mcp_values):
                assert response["succeeded"], response
                compare_response(response["result"], case["expected"])
                result["verifiedResponses"].append(response_summary(
                    path_name, case["label"], "stdio-mcp", response["result"]))
            result.setdefault("mcpWireBytes", 0)
            result["mcpWireBytes"] += len(mcp_wire.encode())

        zundamon = next(item for item in parsed if any(
            source["id"] == "ArtMesh149" and source["grid"] is not None and
            len(source["grid"]["axes"]) == 5 for source in item["scene"]["sources"]))
        negative_requests = [
            ("unknown-parameter", request_args(zundamon["path"], {"NoSuchParameter": 0.0})),
            ("wrong-parameterValues-type", request_args(zundamon["path"], [0.0])),
            ("nonnumeric-value", request_args(zundamon["path"], {"ParamAngleX": "15"})),
            ("nonfinite-json-number", request_args(zundamon["path"], {"ParamAngleX": RAW_NUMBER})),
            ("outside-key-range", request_args(zundamon["path"], {"ParamAngleX": 31.0},
                                                next(source["guid"] for source in zundamon["scene"]["sources"]
                                                     if source["id"] == "ArtMesh149"))),
            ("unknown-source", request_args(zundamon["path"], source_guid="missing-source-guid")),
        ]
        for label, arguments in negative_requests:
            response = ffi_request(core, {"op": OP, **arguments})
            assert response.get("succeeded") is False, (label, response)
            assert core.request("snapshot") == active_snapshot, (
                f"FFI negative control {label} changed the active INX snapshot")
            result["negativeControls"].append({"case": label, "transport": "ffi",
                                               "message": response.get("message", "")})

        mcp_negative, _, _ = mcp_requests(binary, [
            request_args(zundamon["path"], {"NoSuchParameter": 0.0})])
        assert len(mcp_negative) == 1 and not mcp_negative[0]["succeeded"], mcp_negative
        result["negativeControls"].append({"case": "unknown-parameter", "transport": "stdio-mcp",
                                           "message": mcp_negative[0]["message"]})

        final_hashes = {str(path): sha(path) for path in paths}
        assert final_hashes == original_hashes, "a source CMO3 changed during verification"
        assert core.request("snapshot") == active_snapshot, "active INX snapshot changed"
        result["originalSourceHashes"] = original_hashes
        result["activeInx"] = {"path": str(active_path), "sha256": sha(active_path),
                               "renamedNode": "Preserve active authored model",
                               "snapshotPreserved": True}
        result["totals"] = {
            "sourceCount": sum(source_counts), "gridCount": sum(grid_counts),
            "zeroAxisGridCount": zero_axis_count,
            "defaultExactCorners": sum(len(row["weights"]) == 1 and
                                        row["weights"][0]["weight"] == 1.0
                                        for item in parsed
                                        for row in expected_response(item["scene"])["sources"]),
        }
        if result["totals"] != {"sourceCount": 1261, "gridCount": 1261,
                                 "zeroAxisGridCount": 367, "defaultExactCorners": 1261}:
            raise AssertionError(f"unexpected primary weight totals: {result['totals']}")
        result["sourceBinaryHashes"] = source_manifest(library, binary)
        result["scope"] = "Mathematical primary LINEAR/LINEAR keyform weights only."
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = str(error)
    finally:
        if core is not None:
            core.close()
        write_json(directory / "result.json", result)
    print(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
