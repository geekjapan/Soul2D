#!/usr/bin/env python3
"""Prove PHY01 over an owned HTTP MCP host using the synthetic GUI physics fixture.

Uses the committed synthetic CMO3 shared with editor_test_support. An explicit
--fixture must use the same schema and literal values, not a vendor model.
This checks transport, current-state resources, history and file bytes; it does
not establish native GUI behavior or Cubism vendor acceptance.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback


ROOT = Path(__file__).resolve().parents[1]
URI = "resource://soul2d/document/physics-source"
sys.dont_write_bytecode = True


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DRIVER = load("physics_bake_proof", "verify-animation-physics-bake.py")
CAFF = load("caff_proof", "verify-caff-authoring.py")


def rpc(host, label, *arguments):
    command = [str(host.njc), "--port", str(host.port), *arguments]
    response = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
    (host.run / f"{label}.json").write_text(response.stdout)
    (host.run / f"{label}.stderr").write_text(response.stderr)
    with (host.run / "commands.jsonl").open("a") as log:
        log.write(json.dumps({"label": label, "command": command, "exit": response.returncode}) + "\n")
    response.check_returncode()
    envelope = json.loads(response.stdout)
    assert "error" not in envelope, envelope
    return envelope["result"]


def resource(host, label):
    result = rpc(host, label, "resources", "read", URI)
    return json.loads(result["contents"][0]["text"])


def view(host, label):
    result = host.call(label, "live2d_physics3_read", {})
    assert resource(host, f"{label}-resource") == result, label
    return result


def edit(host, label, **arguments):
    snapshot = host.call(f"{label}-before", "snapshot", {})
    return host.call(label, "live2d_physics3_edit",
                     {"expectedRevision": snapshot["revision"], **arguments})


def preserved_bytes(source, saved):
    old, new = source.read_bytes(), saved.read_bytes()
    assert old[:58] == new[:58] and old[-2:] == new[-2:], "CAFF envelope changed"
    original_entries, original_xml = CAFF.decode(old)
    saved_entries, saved_xml = CAFF.decode(new)
    assert len(original_entries) == len(saved_entries)
    for original, current in zip(original_entries, saved_entries):
        assert original[:4] == current[:4], "CAFF entry metadata changed"
        if original[0][0] != "main.xml":
            assert original == current, "opaque entry payload changed"
    expected_changes = [
        (b"<s xs.n='name'>Sway</s>", b"<s xs.n='name'>MCP &amp; Sway</s>"),
        (b"<f xs.n='delay'>0.6</f>", b"<f xs.n='delay'>0.800001</f>"),
    ]
    restored = saved_xml
    for original, edited in expected_changes:
        assert original_xml.count(original) == 1 and restored.count(edited) == 1
        restored = restored.replace(edited, original)
    assert restored == original_xml, "XML outside the two edited fields changed"
    assert b"keep-this-xml" in saved_xml and b"0.800001" in saved_xml


def verify(host, source):
    tools = {tool["name"]: tool for tool in host.tools()}
    assert tools["live2d_physics3_edit"]["inputSchema"]["required"] == ["expectedRevision"]
    assert tools["live2d_physics3_read"]["inputSchema"]["required"] == []
    host.call("open-fixture", "open", {"path": str(source)})
    resources = rpc(host, "resources-list", "resources", "list")["resources"]
    assert any(row["uri"] == URI for row in resources)
    initial = view(host, "initial")
    assert initial["fps"] == 60 and len(initial["groups"]) == 1, initial
    group = initial["groups"][0]
    assert group["name"] == "Sway" and group["inputs"][0]["source"] == "ParamTest", group
    assert len(group["vertices"]) == 2 and group["vertices"][1]["delay"] == 0.6, group
    assert group["vertices"][1]["radius"] == 3.0 and group["outputs"][0]["vertexIndex"] == 1

    edit(host, "rename", group=0, field="name", value="MCP & Sway")
    assert view(host, "renamed")["groups"][0]["name"] == "MCP & Sway"
    edit(host, "precision", group=0, list="vertices", index=1, field="delay", value=0.800001)
    precise = view(host, "precise")
    assert precise["groups"][0]["vertices"][1]["delay"] == 0.800001
    edit(host, "radius", group=0, list="vertices", index=1, field="radius", value=4.5)
    assert view(host, "radius-edited")["groups"][0]["vertices"][1]["radius"] == 4.5
    host.call("undo-radius", "undo", {})
    assert view(host, "radius-undone") == precise
    host.call("undo-precision", "undo", {})
    assert view(host, "precision-undone")["groups"][0]["vertices"][1]["delay"] == 0.6
    host.call("redo-precision", "redo", {})
    assert view(host, "precision-redone") == precise

    before = host.call("before-refusals", "snapshot", {})
    invalid = [
        {"expectedRevision": before["revision"] - 1, "group": 0, "field": "name", "value": "stale"},
        {"group": 0, "field": "name", "value": "invalid", "remove": "yes"},
        {"group": 0, "list": "inputs", "index": 0, "field": "weight", "value": 101},
        {"group": 0, "list": "vertices", "index": 1, "field": "delay", "value": -0.5},
        {"group": 0, "list": "inputs", "insert": 1,
         "value": {"source": "ParamTest", "type": "X", "bogus": True}},
        {"group": 0, "list": "vertices", "insert": 0,
         "value": {"x": 0, "y": 0, "mobility": 1, "delay": 0.5, "acceleration": 1, "radius": 0}},
    ]
    invalid.extend({"group": 0, "list": name, "index": 999, "remove": True}
                   for name in ["inputs", "outputs", "vertices"])
    for index, arguments in enumerate(invalid):
        host.call(f"refused-{index}", "live2d_physics3_edit",
                  {"expectedRevision": before["revision"], **arguments}, expected_error=True)
        assert host.call(f"after-refusal-{index}", "snapshot", {}) == before, arguments
    assert view(host, "after-refusals") == precise

    edit(host, "insert-input", group=0, list="inputs", insert=1,
         value={"source": "ParamTest", "type": "Angle", "weight": 25})
    inputs = view(host, "input-inserted")["groups"][0]["inputs"]
    assert len(inputs) == 2 and inputs[1]["type"] == "Angle" and inputs[1]["weight"] == 25
    edit(host, "remove-input", group=0, list="inputs", index=1, remove=True)
    assert view(host, "input-removed") == precise
    edit(host, "insert-vertex", group=0, list="vertices", insert=1,
         value={"x": 1.5, "y": 2.5, "mobility": 0.9, "delay": 0.7, "acceleration": 1.2, "radius": 1})
    inserted = view(host, "vertex-inserted")["groups"][0]
    assert len(inserted["vertices"]) == 3 and inserted["vertices"][1]["x"] == 1.5
    assert inserted["outputs"][0]["vertexIndex"] == 2
    host.call("undo-insertion", "undo", {})
    assert view(host, "insertion-undone") == precise
    host.call("redo-insertion", "redo", {})
    assert view(host, "insertion-redone")["groups"][0]["outputs"][0]["vertexIndex"] == 2
    edit(host, "remove-vertex", group=0, list="vertices", index=1, remove=True)
    assert view(host, "vertex-removed") == precise

    exported = host.run / "authored.physics3.json"
    document = host.call("export", "live2d_physics3_export", {"path": str(exported)})
    assert json.loads(exported.read_text()) == document
    assert document["PhysicsSettings"][0]["Vertices"][1]["Delay"] == 0.8
    exported_bytes = exported.read_bytes()
    host.call("refuse-export-overwrite", "live2d_physics3_export",
              {"path": str(exported)}, expected_error=True)
    assert exported.read_bytes() == exported_bytes
    saved = host.run / "authored.cmo3"
    host.call("save", "save", {"path": str(saved), "mark_saved": True})
    preserved_bytes(source, saved)
    host.call("reopen", "open", {"path": str(saved)})
    assert view(host, "reopened") == precise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", required=True, type=Path)
    parser.add_argument("--njc", required=True, type=Path)
    parser.add_argument("--fixture", type=Path, default=ROOT / "rust/tests/fixtures/physics3-editor.cmo3",
                        help="synthetic GUI physics fixture (defaults to the committed test asset)")
    args = parser.parse_args()
    evidence = ROOT / "build/verification/ani236"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    result = {"passed": False, "scope": "Synthetic PHY01 HTTP MCP; no native GUI or vendor acceptance",
              "run": str(run)}
    host = None
    try:
        paths = [args.core.resolve(), args.njc.resolve(), args.fixture.resolve(), Path(__file__).resolve()]
        paths.extend(ROOT / name for name in [
            "rust/src/authoring_physics.rs", "rust/src/authoring_scene.rs",
            "rust/src/authoring_edit.rs", "rust/src/lib.rs", "rust/src/mcp.rs", "rust/Cargo.lock",
        ])
        result["inputHashes"] = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
        source = run / "source.cmo3"
        source.write_bytes(args.fixture.read_bytes())
        host = DRIVER.Host(run, args.core.resolve(), args.njc.resolve(), DRIVER.free_port())
        result["hostPid"] = host.child.pid
        host.wait_ready()
        verify(host, source)
        result["passed"] = True
    except Exception:
        result["error"] = traceback.format_exc()
    finally:
        if host is not None:
            try:
                host.close()
            except Exception:
                result["cleanupError"] = traceback.format_exc()
                result["passed"] = False
            result["hostExited"] = host.child.poll() is not None
            result["passed"] = result["passed"] and result["hostExited"]
        (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"passed": result["passed"], "run": str(run)}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
