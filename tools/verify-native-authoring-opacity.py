#!/usr/bin/env python3
"""Exercise native ArtMesh opacity edits through all Rust session transports."""
import argparse
import copy
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import traceback
from xml.parsers import expat

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUTS = ROOT / "build/verification/live2d-inputs-vjq5llby"
sys.dont_write_bytecode = True


def import_tool(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


EDIT = import_tool("verify_native_authoring_edit", ROOT / "tools/verify-native-authoring-edit.py")
CMO, CAFF, CORE = EDIT.CMO, EDIT.CAFF, EDIT.CORE
PRIMARY = import_tool("verify_primary_forms", ROOT / "tools/verify-primary-forms.py")
CASES = EDIT.CASES


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def opacity_token(xml, identity):
    graph = CMO.XMLGraph(xml)
    model = next(node for node in graph.root.iter("CModelSource")
                 if node.get("xs.ref") is None and len(node))
    owners = graph.source_items(model, "drawableSourceSet") + graph.source_items(model, "deformerSourceSet")
    owner = next(node for node in owners if graph.guid(node, "guid") == identity[0])
    form = next(node for node in graph.items(graph.required(owner, "keyforms"))
                if graph.guid(node, "guid") == identity[1]
                and graph.text(graph.required(node, "coordType"), "coordName") == identity[2])
    field = graph.field(form, "opacity")
    assert field is not None and field.tag == "f" and field.get("xs.n") == "opacity"
    assert not list(field), "opacity field contains child elements"
    target = next(index for index, node in enumerate(graph.root.iter()) if node is field)
    parser = expat.ParserCreate()
    stack, bounds = [], []
    counter = 0

    def start(name, attributes):
        nonlocal counter
        selected = counter == target
        counter += 1
        stack.append(selected)
        if selected:
            assert name == "f"
            bounds.append(xml.find(b">", parser.CurrentByteIndex) + 1)

    def end(name):
        if stack.pop():
            bounds.append(parser.CurrentByteIndex)

    parser.StartElementHandler, parser.EndElementHandler = start, end
    parser.Parse(xml, True)
    assert len(bounds) == 2
    body = xml[bounds[0]:bounds[1]]
    tokens = list(re.finditer(rb"\S+", body))
    assert len(tokens) == 1, "opacity field is not one plain text scalar"
    value = float(tokens[0].group().decode("ascii"))
    assert math.isfinite(value)
    return bounds[0] + tokens[0].start(), bounds[0] + tokens[0].end()


def check_saved(original, saved, identity, expected):
    """Check saved scene and allow only independently changed scalar tokens."""
    original_bytes, saved_bytes = original.read_bytes(), saved.read_bytes()
    assert original_bytes[:58] == saved_bytes[:58], "CAFF header changed"
    assert original_bytes[-2:] == saved_bytes[-2:], "CAFF trailer changed"
    old_entries, old_xml = CAFF.decode(original_bytes)
    new_entries, new_xml = CAFF.decode(saved_bytes)
    assert len(old_entries) == len(new_entries)
    for old, new in zip(old_entries, new_entries):
        assert old[:4] == new[:4], "entry metadata changed"
        if old[0][0] != "main.xml":
            assert old == new, "opaque entry payload changed"
    actual = CMO.read_scene(new_xml, {entry[0][0] for entry in new_entries})
    assert actual == expected, "saved source fields differ from the independent XML oracle"

    old_scene = CMO.read_scene(old_xml, {entry[0][0] for entry in old_entries})
    old_form = EDIT.target_form(old_scene, identity)
    new_form = EDIT.target_form(expected, identity)
    old_positions = old_form["data"]["positions"]
    new_positions = new_form["data"]["positions"]
    old_position_tokens = EDIT.position_tokens(old_xml, identity)
    new_position_tokens = EDIT.position_tokens(new_xml, identity)
    assert len(old_positions) == len(new_positions) == len(old_position_tokens) == len(new_position_tokens)
    changed_positions = [index for index, (before, after) in enumerate(zip(old_positions, new_positions))
                         if float(before).hex() != float(after).hex()]
    old_ranges = [old_position_tokens[index] for index in changed_positions]
    new_ranges = [new_position_tokens[index] for index in changed_positions]
    if float(old_form["opacity"]).hex() != float(new_form["opacity"]).hex():
        old_ranges.append(opacity_token(old_xml, identity))
        new_ranges.append(opacity_token(new_xml, identity))
    assert len(old_ranges) == len(new_ranges)
    restored = new_xml
    for old_range, new_range in sorted(zip(old_ranges, new_ranges), key=lambda pair: pair[1][0], reverse=True):
        restored = restored[:new_range[0]] + old_xml[old_range[0]:old_range[1]] + restored[new_range[1]:]
    assert restored == old_xml, "XML bytes outside target position and opacity scalar tokens changed"


def choose_opacity(current):
    value = current + 0.125
    if value == current:
        value = current - 0.125
    assert value != current and value == value and abs(value) != float("inf")
    return value


def sequence(source, saved, identity, initial, edited, position):
    revision = 1
    after_vertex = revision + 1
    after_opacity = after_vertex + 1
    return [
        {"op": "open", "path": str(source)},
        {"op": "live2d.authoring.read"},
        {"op": "live2d.authoring.primary_forms", "sourceGuid": identity[0]},
        {"op": "live2d.form.set_opacity", "sourceGuid": identity[0], "formGuid": identity[1],
         "coordinateType": identity[2], "opacity": initial, "expectedRevision": revision},
        {"op": "snapshot"},
        {"op": "live2d.form.set_vertex", "sourceGuid": identity[0], "formGuid": identity[1],
         "coordinateType": identity[2], "vertexIndex": 0, "position": position,
         "expectedRevision": revision},
        {"op": "live2d.form.set_opacity", "sourceGuid": identity[0], "formGuid": identity[1],
         "coordinateType": identity[2], "opacity": edited, "expectedRevision": after_vertex},
        {"op": "live2d.authoring.read"},
        {"op": "live2d.authoring.primary_forms", "sourceGuid": identity[0]},
        {"op": "live2d.form.set_opacity", "sourceGuid": identity[0], "formGuid": identity[1],
         "coordinateType": identity[2], "opacity": edited, "expectedRevision": after_opacity},
        {"op": "snapshot"},
        {"op": "live2d.form.set_opacity", "sourceGuid": identity[0], "formGuid": identity[1],
         "coordinateType": identity[2], "opacity": initial, "expectedRevision": after_vertex},
        {"op": "snapshot"},
        {"op": "undo"},
        {"op": "snapshot"},
        {"op": "redo"},
        {"op": "snapshot"},
        {"op": "save", "path": str(saved)},
        {"op": "open", "path": str(saved)},
        {"op": "live2d.authoring.read"},
        {"op": "live2d.authoring.primary_forms", "sourceGuid": identity[0]},
        {"op": "snapshot"},
    ]


def unwrap_mcp(result):
    if result.get("isError"):
        return {"succeeded": False, "message": json.dumps(result)}
    value = result.get("structuredContent")
    if value is None:
        value = json.loads(next(item["text"] for item in result["content"] if item["type"] == "text"))
    if isinstance(value, dict) and set(value) == {"result"}:
        value = value["result"]
    return {"succeeded": True, "result": value}


def run_transport(binary, core, requests, transport):
    if transport == "ffi":
        responses = []
        for request in requests:
            raw = core.lib.vsoul_session_request(core.handle, json.dumps(request).encode())
            assert raw
            try:
                responses.append(json.loads(CMO.ctypes.string_at(raw)))
            finally:
                core.lib.vsoul_string_free(raw)
        return responses, json.dumps(responses, indent=2)
    if transport == "jsonl":
        process = subprocess.run([str(binary)], input="".join(json.dumps(row) + "\n" for row in requests),
                                 text=True, capture_output=True, timeout=120)
        assert process.returncode == 0, process.stderr
        responses = [json.loads(line) for line in process.stdout.splitlines() if line.strip()]
    else:
        messages = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "native-authoring-opacity", "version": "1"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ]
        for index, request in enumerate(requests):
            messages.append({"jsonrpc": "2.0", "id": index + 3, "method": "tools/call", "params": {
                "name": request["op"].replace(".", "_"),
                "arguments": {key: value for key, value in request.items() if key != "op"}}})
        process = subprocess.run([str(binary), "--mcp"],
                                 input="".join(json.dumps(row) + "\n" for row in messages),
                                 text=True, capture_output=True, timeout=120)
        assert process.returncode == 0, process.stderr
        rpc = {row["id"]: row for row in map(json.loads, process.stdout.splitlines()) if "id" in row}
        advertised = {tool["name"]: tool for tool in rpc[2]["result"]["tools"]}
        assert "live2d_form_set_opacity" in advertised
        assert advertised["live2d_form_set_opacity"]["inputSchema"]["required"] == [
            "sourceGuid", "formGuid", "coordinateType", "opacity", "expectedRevision"]
        responses = [unwrap_mcp(rpc[index + 3]["result"]) for index in range(len(requests))]
    assert len(responses) == len(requests)
    return responses, process.stdout + process.stderr


def check_primary(actual, scene, source_guid):
    expected = PRIMARY.expected_response(scene, source_guid=source_guid)
    assert expected["sources"][0]["evaluatedForms"] is not None, "sample source lacks evaluated primary forms"
    PRIMARY.compare_value(actual, expected)


def check_sequence(responses, original, after_vertex, edited, original_primary, edited_primary):
    assert len(responses) == 22
    assert [response["succeeded"] for response in responses] == [True] * 11 + [False] + [True] * 10
    values = [response.get("result") for response in responses]
    initial_revision = values[0]["revision"]
    assert initial_revision == 1 and values[0]["model"] == original and not values[0]["modified"]
    assert values[1] == original
    assert values[2]["format"] == original_primary["format"]
    assert values[3] == values[0] == values[4], "same-opacity edit changed revision or history"
    assert values[5]["revision"] == initial_revision + 1 and values[5]["model"] == after_vertex
    assert values[6]["revision"] == initial_revision + 2 and values[6]["model"] == edited
    assert values[7] == edited
    assert values[8]["format"] == edited_primary["format"]
    assert values[9] == values[6] == values[10], "opacity no-op changed revision or history"
    assert values[12] == values[10], "stale opacity edit changed session state"
    assert values[13]["model"] == after_vertex and values[13]["revision"] == initial_revision + 3
    assert values[14] == values[13]
    assert values[15]["model"] == edited and values[15]["revision"] == initial_revision + 4
    assert values[16] == values[15]
    assert values[17]["path"]
    assert values[18]["model"] == edited and not values[18]["modified"] and not values[18]["canUndo"]
    assert values[19] == edited
    assert values[20]["format"] == edited_primary["format"]
    assert values[21] == values[18]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="?", type=Path, default=DEFAULT_INPUTS)
    args = parser.parse_args()
    evidence_root = ROOT / "build/verification"
    evidence_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="native-authoring-opacity-", dir=evidence_root))
    binary = ROOT / "out/rust/debug/soul2d-core"
    library = ROOT / "out/rust/debug" / ("libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    result = {"status": "FAIL", "scope": "native ArtMesh opacity edit", "evidence": str(directory),
              "workflows": [], "negativeControls": ["same-value no-op", "stale revision"]}
    try:
        paths = sorted(args.inputs.rglob("*.cmo3"))
        assert {path.name for path in paths} == set(CASES)
        for path in paths:
            identity = CASES[path.name]
            original_hash = sha(path)
            entries, xml = CAFF.decode(path.read_bytes())
            original = CMO.read_scene(xml, {entry[0][0] for entry in entries})
            current = EDIT.target_form(original, identity)["opacity"]
            opacity = choose_opacity(current)
            position = EDIT.target_form(original, identity)["data"]["positions"][:2]
            position = [position[0] + 0.125, position[1] - 0.25]
            after_vertex = copy.deepcopy(original)
            EDIT.target_form(after_vertex, identity)["data"]["positions"][:2] = position
            edited = copy.deepcopy(after_vertex)
            EDIT.target_form(edited, identity)["opacity"] = opacity
            original_primary = PRIMARY.expected_response(original, source_guid=identity[0])
            edited_primary = PRIMARY.expected_response(edited, source_guid=identity[0])
            for transport in ("ffi", "jsonl", "mcp"):
                saved = directory / f"{path.stem}-{transport}.cmo3"
                core = CORE.Core(library) if transport == "ffi" else None
                try:
                    requests = sequence(path, saved, identity, current, opacity, position)
                    responses, wire = run_transport(binary, core, requests, transport)
                    check_sequence(responses, original, after_vertex, edited,
                                   original_primary, edited_primary)
                    check_primary(responses[2]["result"], original, identity[0])
                    check_primary(responses[8]["result"], edited, identity[0])
                    check_primary(responses[20]["result"], edited, identity[0])
                    check_saved(path, saved, identity, edited)
                    if core is not None:
                        snapshot = core.request("snapshot")
                        expected_revision = snapshot["revision"]
                        invalid = [
                            ("unknown-owner", {"sourceGuid": "missing"}),
                            ("unknown-form", {"formGuid": "missing"}),
                            ("wrong-coordinate", {"coordinateType": "missing"}),
                            ("wrong-type", {"opacity": "0.5"}),
                            ("missing-opacity", {"opacity": None}),
                            ("float-overflow", {"opacity": 1e300}),
                        ]
                        base = {"sourceGuid": identity[0], "formGuid": identity[1],
                                "coordinateType": identity[2], "opacity": 0.5,
                                "expectedRevision": expected_revision}
                        for label, fields in invalid:
                            request = {**base, **fields}
                            if request.get("opacity") is None:
                                request.pop("opacity")
                            rejected = core.request("live2d.form.set_opacity", success=False, **request)
                            assert core.request("snapshot") == snapshot, label
                            result["negativeControls"].append({"source": path.name, "kind": label,
                                                               "error": rejected})
                        warp = next((source for source in original["sources"]
                                     if source["data"]["type"] == "Warp" and source["forms"]), None)
                        if warp is not None:
                            form = warp["forms"][0]
                            rejected = core.request("live2d.form.set_opacity", success=False,
                                sourceGuid=warp["guid"], formGuid=form["guid"],
                                coordinateType=form["coordinateType"], opacity=0.5,
                                expectedRevision=expected_revision)
                            assert core.request("snapshot") == snapshot
                            result["negativeControls"].append({"source": path.name, "kind": "warp-owner",
                                                               "error": rejected})
                finally:
                    if core is not None:
                        core.close()
                log = directory / f"{path.stem}-{transport}.jsonl"
                log.write_text(wire)
                result["workflows"].append({"source": str(path), "transport": transport,
                    "saved": str(saved), "savedSha256": sha(saved), "wire": str(log),
                    "requests": len(responses)})
            assert sha(path) == original_hash, "original model changed"

        owned = ["tools/verify-native-authoring-opacity.py", "tools/verify-native-authoring-edit.py",
                 "tools/verify-primary-forms.py", "tools/verify-cmo-source.py", "tools/verify-caff-authoring.py",
                 "tools/verify-rust-core.py", "rust/src/authoring_edit.rs", "rust/src/lib.rs",
                 "rust/src/main.rs", "rust/tests/core.rs"]
        result["sourceBinaryHashes"] = {path: sha(ROOT / path) for path in owned}
        result["sourceBinaryHashes"].update({str(path.relative_to(ROOT)): sha(path) for path in (library, binary)})
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        (directory / "failure.txt").write_text(traceback.format_exc())
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "evidence": str(directory),
                      "workflows": len(result["workflows"]), "negativeControls": len(result["negativeControls"]),
                      "error": result.get("error")}, indent=2))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
