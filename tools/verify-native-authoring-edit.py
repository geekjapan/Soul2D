#!/usr/bin/env python3
"""Exercise native CMO3 edits through the shared session on real supplied models."""
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import traceback
from xml.parsers import expat

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("cmo_edit_oracle", ROOT / "tools/verify-cmo-source.py")
CMO = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CMO)
CORE, CAFF = CMO.CORE, CMO.CAFF
CASES = {
    "hiyori_free_t08.cmo3": ("ce5719c0-0ce8-4aab-8a7e-dd460a2d3ccf", "fc796143-6e45-45e7-9fcd-f125efb0f75c", "Canvas"),
    "hiyori_pro_t11.cmo3": ("8e72ea6b-44c6-4e33-95bf-884ddcdcff24", "5d228936-bac1-407d-a434-da2c8be9a2c0", "Canvas"),
    "ren_t01.cmo3": ("05a50c09-4e80-4e90-9af1-e1144bd12f86", "74f4deac-1d56-4367-aa9a-799ebe2840c5", "DeformerLocal"),
    "zundamon_t01.cmo3": ("da8b2159-6b3c-4348-b5eb-23ec3ad5f593", "4c766346-827f-4e5f-9e04-eb89f087c0b3", "DeformerLocal"),
}
# Advanced blend materials have no GPU packet; render.packet refuses these models.
CPU_ONLY = {"ren_t01.cmo3"}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def target_form(scene, identity):
    owner, guid, coordinate = identity
    source = next(row for row in scene["sources"] if row["guid"] == owner)
    return next(row for row in source["forms"]
                if row["guid"] == guid and row["coordinateType"] == coordinate)


def position_tokens(xml, identity):
    graph = CMO.XMLGraph(xml)
    model = next(node for node in graph.root.iter("CModelSource")
                 if node.get("xs.ref") is None and len(node))
    owners = graph.source_items(model, "drawableSourceSet") + graph.source_items(model, "deformerSourceSet")
    owner = next(node for node in owners
                 if graph.guid(node, "guid") == identity[0])
    form = next(node for node in graph.items(graph.required(owner, "keyforms"))
                if graph.guid(node, "guid") == identity[1]
                and graph.text(graph.required(node, "coordType"), "coordName") == identity[2])
    positions = graph.required(form, "positions")
    target = next(index for index, node in enumerate(graph.root.iter()) if node is positions)
    parser = expat.ParserCreate()
    stack, bounds = [], []
    counter = 0

    def start(name, attributes):
        nonlocal counter
        selected = counter == target
        counter += 1
        stack.append(selected)
        if selected:
            assert name == "float-array"
            bounds.append(xml.find(b">", parser.CurrentByteIndex) + 1)

    def end(name):
        if stack.pop():
            bounds.append(parser.CurrentByteIndex)

    parser.StartElementHandler, parser.EndElementHandler = start, end
    parser.Parse(xml, True)
    assert len(bounds) == 2
    return [(bounds[0] + match.start(), bounds[0] + match.end())
            for match in re.finditer(rb"\S+", xml[bounds[0]:bounds[1]])]


def check_saved(original, saved, identity, expected):
    assert original.read_bytes()[:58] == saved.read_bytes()[:58], "CAFF header changed"
    assert original.read_bytes()[-2:] == saved.read_bytes()[-2:], "CAFF trailer changed"
    old_entries, old_xml = CAFF.decode(original.read_bytes())
    new_entries, new_xml = CAFF.decode(saved.read_bytes())
    assert len(old_entries) == len(new_entries)
    for old, new in zip(old_entries, new_entries):
        assert old[:4] == new[:4], "entry metadata changed"
        if old[0][0] != "main.xml":
            assert old == new, "opaque entry payload changed"
    actual = CMO.read_scene(new_xml, {entry[0][0] for entry in new_entries})
    assert actual == expected, "saved source fields differ from the independent XML oracle"
    old_ranges, new_ranges = position_tokens(old_xml, identity), position_tokens(new_xml, identity)
    assert len(old_ranges) == len(new_ranges)
    restored = new_xml
    for old_range, new_range in reversed(list(zip(old_ranges[:2], new_ranges[:2]))):
        restored = restored[:new_range[0]] + old_xml[old_range[0]:old_range[1]] + restored[new_range[1]:]
    assert restored == old_xml, "XML bytes outside the edited two scalar tokens changed"


def sequence(source, saved, identity, position):
    edit = {"op": "live2d.form.set_vertex", "sourceGuid": identity[0], "formGuid": identity[1],
            "coordinateType": identity[2], "vertexIndex": 0, "position": position, "expectedRevision": 1}
    return [
        {"op": "open", "path": str(source)}, {"op": "live2d.authoring.read"}, edit,
        {**edit, "expectedRevision": 2},  # no-op must not add history
        {**edit, "position": [1, 2]},  # stale revision
        {"op": "snapshot"}, {"op": "undo"}, {"op": "redo"},
        {"op": "save", "path": str(saved)}, {"op": "snapshot"},
        {"op": "open", "path": str(saved)}, {"op": "live2d.authoring.read"},
        {"op": "render.packet"}, {"op": "snapshot"},
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
                "clientInfo": {"name": "native-authoring-edit", "version": "1"}}},
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
        assert "live2d_form_set_vertex" in advertised
        assert advertised["live2d_authoring_read"]["inputSchema"]["required"] == []
        responses = [unwrap_mcp(rpc[index + 3]["result"]) for index in range(len(requests))]
    assert len(responses) == len(requests)
    return responses, process.stdout + process.stderr


def check_sequence(responses, original, edited, renders):
    assert [row["succeeded"] for row in responses] == [True] * 4 + [False] + [True] * 7 + [renders, True]
    values = [row.get("result") for row in responses]
    assert values[0]["model"] == original and values[0]["revision"] == 1 and not values[0]["modified"]
    assert values[1] == original
    assert values[2]["model"] == edited and values[2]["revision"] == 2 and values[2]["modified"]
    assert values[3] == values[2] == values[5]
    assert values[6]["model"] == original and values[6]["revision"] == 3 and not values[6]["modified"]
    assert values[7]["model"] == edited and values[7]["revision"] == 4
    assert values[9]["model"] == edited and values[9]["revision"] == 4 and not values[9]["modified"]
    assert values[10]["model"] == edited and values[10]["revision"] == 5 and not values[10]["canUndo"]
    assert values[11] == edited
    if renders:
        assert values[12]["draws"] and values[12]["textures"], "native render packet is empty"
    else:
        assert "CPU renderer" in responses[12]["message"]
    assert values[13] == values[10], "render altered native state"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="?", type=Path, default=CMO.DEFAULT_INPUTS)
    args = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="native-authoring-edit-", dir=ROOT / "build/verification"))
    binary = ROOT / "out/rust/debug/soul2d-core"
    library = ROOT / "out/rust/debug" / ("libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    result = {"status": "FAIL", "evidence": str(directory), "workflows": [], "negativeControls": []}
    try:
        paths = sorted(args.inputs.rglob("*.cmo3"))
        assert {path.name for path in paths} == set(CASES)
        cases = [(path, CASES[path.name], "ArtMesh") for path in paths]
        free = next(path for path in paths if path.name == "hiyori_free_t08.cmo3")
        cases.append((free, ("c8b456d2-0567-42a2-8cf3-2cffab0ea275",
                             "856ee049-cd03-4812-b489-2e1c213ee0a4", "DeformerLocal"), "Warp"))
        for path, identity, kind in cases:
            original_hash = sha(path)
            entries, xml = CAFF.decode(path.read_bytes())
            original = CMO.read_scene(xml, {entry[0][0] for entry in entries})
            position = target_form(original, identity)["data"]["positions"][:2]
            position = [position[0] + 0.125, position[1] - 0.25]
            edited = copy.deepcopy(original)
            target_form(edited, identity)["data"]["positions"][:2] = position
            for transport in ("ffi", "jsonl", "mcp"):
                saved = directory / f"{path.stem}-{kind}-{transport}.cmo3"
                core = CORE.Core(library) if transport == "ffi" else None
                try:
                    responses, wire = run_transport(binary, core, sequence(path, saved, identity, position), transport)
                    check_sequence(responses, original, edited, path.name not in CPU_ONLY)
                    check_saved(path, saved, identity, edited)
                    for index, label in ((4, "stale-revision"), (12, "cpu-only-render-packet")):
                        if not responses[index]["succeeded"]:
                            result["negativeControls"].append({"source": path.name, "transport": transport,
                                "kind": label, "error": responses[index].get("message")})
                    if core is not None:
                        snapshot = core.request("snapshot")
                        base = {"sourceGuid": identity[0], "formGuid": identity[1], "coordinateType": identity[2],
                                "vertexIndex": 0, "position": position, "expectedRevision": snapshot["revision"]}
                        for label, fields in (
                            ("unknown-owner", {"sourceGuid": "missing"}),
                            ("unknown-form", {"formGuid": "missing"}),
                            ("wrong-coordinates", {"coordinateType": "missing"}),
                            ("invalid-index", {"vertexIndex": 1_000_000}),
                            ("invalid-pair", {"position": [1]}),
                            ("float-overflow", {"position": [1e300, 0]}),
                        ):
                            core.request("live2d.form.set_vertex", success=False, **{**base, **fields})
                            assert core.request("snapshot") == snapshot
                            result["negativeControls"].append({"source": path.name, "kind": label})
                        saved_bytes = saved.read_bytes()
                        saved.write_bytes(saved_bytes + b"external-change")
                        core.request("save", success=False, path=str(saved))
                        assert core.request("snapshot") == snapshot
                        saved.write_bytes(saved_bytes)
                        result["negativeControls"].append({"source": path.name, "kind": "external-file-conflict"})
                        bad_suffix = directory / f"{path.stem}-{kind}-invalid.inx"
                        core.request("save", success=False, path=str(bad_suffix))
                        assert not bad_suffix.exists() and core.request("snapshot") == snapshot
                        result["negativeControls"].append({"source": path.name, "kind": "native-save-extension"})
                        for label, predicate, expected_error in (
                            ("extended-cache", lambda owner: bool(owner["extendedInterpolations"]), "extended interpolation"),
                            ("morph-metadata", lambda owner: not owner["extendedInterpolations"] and owner["morphTargets"] is not None
                             and bool(owner["morphTargets"]["targets"] or owner["morphTargets"]["blendWeightConstraints"]),
                             "primary morph metadata"),
                        ):
                            owner = next((owner for owner in edited["sources"]
                                          if owner["data"]["type"] == "ArtMesh" and owner["forms"] and predicate(owner)), None)
                            if owner is None:
                                continue
                            # A blend shape form is editable; the control needs a base form.
                            shapes = {target["formGuid"] for target in (owner["morphTargets"] or {}).get("targets", [])}
                            form = next((form for form in owner["forms"] if form["guid"] not in shapes), None)
                            if form is None:
                                continue
                            bad = {"op": "live2d.form.set_vertex", **base, "sourceGuid": owner["guid"],
                                   "formGuid": form["guid"], "coordinateType": form["coordinateType"]}
                            rejected, _ = run_transport(binary, core, [bad], "ffi")
                            assert not rejected[0]["succeeded"] and expected_error in rejected[0].get("message", ""), (
                                label, rejected[0].get("succeeded"), rejected[0].get("message"))
                            assert core.request("snapshot") == snapshot
                            result["negativeControls"].append({"source": path.name, "kind": label,
                                "error": rejected[0]["message"]})
                finally:
                    if core is not None:
                        core.close()
                log = directory / f"{path.stem}-{kind}-{transport}.jsonl"
                log.write_text(wire)
                result["workflows"].append({"source": str(path), "kind": kind, "transport": transport,
                    "saved": str(saved), "savedSha256": sha(saved), "wire": str(log), "requests": len(responses)})
            assert sha(path) == original_hash, "original model changed"
        owned = ["tools/verify-native-authoring-edit.py", "tools/verify-cmo-source.py", "tools/verify-caff-authoring.py",
                 "tools/verify-rust-core.py", *[str(path.relative_to(ROOT)) for path in (ROOT / "rust/src").glob("authoring*.rs")],
                 "rust/src/document.rs", "rust/src/caff.rs", "rust/src/lib.rs", "rust/src/main.rs"]
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
