import argparse
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import traceback
from urllib.parse import quote
import zlib

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
URI = "resource://soul2d/part-split/plan"


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


DRIVER = load("split_context_host", "verify-animation-physics-bake.py")
PNG = load("split_context_png", "verify-rust-core.py")
REAL = load("split_context_decode", "verify-real-files.py")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def gray_png(width, height, coverage):
    assert len(coverage) == width * height
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\0" + coverage[y*width:(y+1)*width] for y in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser(description="Verify split-plan context through an owned MCP host and production njc.")
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--njc", type=Path, required=True)
    args = parser.parse_args()
    if not __debug__:
        raise RuntimeError("verification requires Python assertions")
    core, njc = args.core.resolve(), args.njc.resolve()
    evidence = ROOT / "build" / "verification" / "part-split-context"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=evidence))
    binaries = {"core": core, "njc": njc}
    before_hashes = {name: digest(path) for name, path in binaries.items()}
    sources = set((ROOT / "rust" / "src").rglob("*.rs"))
    sources.update([
        Path(__file__).resolve(), Path(DRIVER.__file__).resolve(), Path(PNG.__file__).resolve(),
        Path(REAL.__file__).resolve(),
        ROOT / "rust" / "Cargo.toml", ROOT / "rust" / "Cargo.lock",
        ROOT / "doc" / "part-split.md", ROOT / "doc" / "mcp_api.md",
        ROOT / ".cursor" / "skills" / "split-illustration" / "SKILL.md",
    ])
    source_hashes = {str(path.relative_to(ROOT)): digest(path) for path in sorted(sources)}
    result = {
        "status": "FAIL",
        "scope": "Split-plan and binary image resources, component discovery, raster input/replay and causal applied state; GUI and character split quality excluded",
        "nativeGui": "SKIP", "vendorAcceptance": "NOT_RUN", "ci": "DISABLED",
        "python": sys.version,
        "binaries": {name: {"path": str(path), "sha256": before_hashes[name]} for name, path in binaries.items()},
        "sources": source_hashes,
    }
    host = None
    try:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        host = DRIVER.Host(run, core, njc, port)
        write_json(run / "instance.json", {"pid": host.child.pid, "port": port, "core": str(core), "njc": str(njc)})
        host.wait_ready()

        def rpc(label, tail, expect_rpc_error=False, binary=False):
            command = [str(njc), "--port", str(port), *tail]
            completed = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
            response = None
            if binary and completed.returncode == 0:
                response = json.loads(completed.stdout)
                retained = copy.deepcopy(response)
                [item] = retained["result"]["contents"]
                png = base64.b64decode(item.pop("blob"), validate=True)
                (run / f"{label}.png").write_bytes(png)
                item["blobSha256"] = hashlib.sha256(png).hexdigest()
                write_json(run / f"{label}.stdout", retained)
            else:
                (run / f"{label}.stdout").write_text(completed.stdout)
            (run / f"{label}.stderr").write_text(completed.stderr)
            with (run / "commands.jsonl").open("a") as log:
                log.write(json.dumps({"label": label, "command": command, "exit": completed.returncode}) + "\n")
            if expect_rpc_error:
                prefix = "njc: MCP error: "
                assert completed.returncode == 1 and not completed.stdout.strip(), (label, completed)
                assert completed.stderr.startswith(prefix), (label, completed.stderr)
                return {"error": json.loads(completed.stderr[len(prefix):])}
            assert completed.returncode == 0, (label, completed.returncode, completed.stderr)
            return response if response is not None else json.loads(completed.stdout)

        def tool(label, name, arguments=None, refused=False):
            response = rpc(label, ["tools", "call", name, "--json", json.dumps(arguments or {})])
            assert "error" not in response, (label, response)
            value = response["result"]
            assert bool(value.get("isError", False)) == refused, (label, value)
            if refused:
                return value
            return json.loads(next(item["text"] for item in value["content"] if item["type"] == "text"))

        def context(label, message=None):
            response = rpc(label, ["read", URI], expect_rpc_error=message is not None)
            if message is not None:
                assert response["error"]["code"] == -32002, response
                assert message in response["error"]["message"], response
                return None
            assert "error" not in response, response
            [item] = response["result"]["contents"]
            assert item["uri"] == URI and item["mimeType"] == "application/json", item
            value = json.loads(item["text"])
            write_json(run / f"{label}.json", value)
            return value

        tools = rpc("tools", ["tools", "list"])["result"]["tools"]
        assert {"snapshot", "new", "open", "undo", "redo", "split_open", "split_part_set", "split_part_remove",
                "split_part_order", "split_close", "split_plan", "split_apply", "node_delete",
                "NodeCommand_SetNodeName", "NodeCommand_AddNode", "history_clear", "history_limit_set",
                "FileCommand_SaveFile", "split_components", "split_mask_inspect"} <= {item["name"] for item in tools}
        resources = rpc("resources", ["resources", "list"])["result"]["resources"]
        assert any(item["uri"] == URI and item["mimeType"] == "application/json" for item in resources)
        initial = tool("initial", "snapshot")
        document_revision = initial["revision"]
        assert initial["partSplit"]["open"] is False
        assert initial["partSplit"]["applied"] is False
        context("closed", "no source is open")
        assert tool("after-closed", "snapshot") == initial

        rgba = (bytes([255, 0, 0, 255]) * 4 + bytes([0, 0, 255, 255]) * 4) * 8
        source = run / "red-blue.png"
        source.write_bytes(PNG.png(8, 8, rgba))
        tool("source", "split_open", {"path": str(source)})
        tool("left", "split_part_set", {"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 4, 8]}]})
        tool("right", "split_part_set", {"name": "Right", "steps": [{"op": "box", "rect": [4, 0, 4, 8]}]})
        before_read = tool("before-read", "snapshot")
        first = context("first")
        assert first["documentRevision"] == document_revision and first["draftRevision"] == 3
        assert first["applied"] is False
        assert first["plan"]["version"] == 1
        assert first["plan"]["source"] == {
            "path": str(source), "width": 8, "height": 8, "crc32": zlib.crc32(rgba) & 0xFFFFFFFF,
        }
        assert [part["name"] for part in first["plan"]["parts"]] == ["Left", "Right"]
        assert [(part["name"], part["pixels"], part["bbox"]) for part in first["parts"]] == [
            ("Left", 32, [0, 0, 4, 8]), ("Right", 32, [4, 0, 4, 8]),
        ]
        assert (first["unclaimedPixels"], first["multiplyClaimedPixels"]) == (0, 0)
        assert context("repeat") == first
        assert tool("after-read", "snapshot") == before_read
        write_json(run / "plan.json", first["plan"])

        tool("narrow-left", "split_part_set", {"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 2, 8]}]})
        changed = context("changed")
        assert changed["documentRevision"] == document_revision and changed["draftRevision"] == 4
        assert [(part["pixels"], part["bbox"]) for part in changed["parts"]] == [
            (16, [0, 0, 2, 8]), (32, [4, 0, 4, 8]),
        ]
        assert (changed["unclaimedPixels"], changed["multiplyClaimedPixels"]) == (16, 0)
        tool("reorder", "split_part_order", {"names": ["Right", "Left"]})
        ordered = context("ordered")
        assert ordered["draftRevision"] == 5 and ordered["documentRevision"] == document_revision
        assert [part["name"] for part in ordered["plan"]["parts"]] == ["Right", "Left"]
        assert [part["name"] for part in ordered["parts"]] == ["Right", "Left"]
        tool("close", "split_close")
        after_close = tool("after-close", "snapshot")
        context("closed-again", "no source is open")
        assert tool("after-close-read", "snapshot") == after_close

        tool("new-context", "new")
        other = run / "green-yellow.png"
        other.write_bytes(PNG.png(8, 8, bytes([0, 255, 0, 255]) * 32 + bytes([255, 255, 0, 255]) * 32))
        tool("other-source", "split_open", {"path": str(other)})
        tool("top", "split_part_set", {"name": "Top", "steps": [{"op": "box", "rect": [0, 0, 8, 4]}]})
        tool("bottom", "split_part_set", {"name": "Bottom", "steps": [{"op": "box", "rect": [0, 4, 8, 4]}]})
        fresh = context("fresh")
        assert fresh["documentRevision"] == document_revision + 1 and fresh["draftRevision"] == 3
        assert fresh["plan"]["source"]["path"] == str(other)
        assert [(part["name"], part["pixels"], part["bbox"]) for part in fresh["parts"]] == [
            ("Top", 32, [0, 0, 8, 4]), ("Bottom", 32, [0, 4, 8, 4]),
        ]

        tool("new-replay", "new")
        before_replay = tool("before-replay", "snapshot")
        tool("whole-context-refused", "split_apply", {"plan": first}, refused=True)
        assert tool("after-refusal", "snapshot") == before_replay
        created = tool("replay", "split_apply", {"plan": first["plan"]})
        assert [part["name"] for part in created["created"]] == ["Left", "Right"]
        applied = tool("applied", "snapshot")
        nodes = applied["model"]["nodes"]["children"]
        assert [(node["name"], node["type"], node["transform"]["trans"][:2]) for node in nodes] == [
            ("Left", "Part", [-2.0, 0.0]), ("Right", "Part", [2.0, 0.0]),
        ]
        assert applied["revision"] == before_replay["revision"] + 1
        assert applied["history"]["count"] == before_replay["history"]["count"] + 1
        assert applied["partSplit"]["open"] is False and applied["partSplit"]["applied"] is False
        tool("undo", "undo")
        assert tool("undone", "snapshot")["model"] == before_replay["model"]
        tool("redo", "redo")
        assert tool("redone", "snapshot")["model"] == applied["model"]

        def draft(label, image=source):
            tool(label + "-open", "split_open", {"path": str(image)})
            for index, part in enumerate(first["plan"]["parts"]):
                tool(label + "-part-" + str(index), "split_part_set", part)

        def applied_state(label, expected):
            before = tool(label + "-before", "snapshot")
            assert before["partSplit"]["applied"] is expected, (label, before["partSplit"])
            view = context(label)
            assert view["applied"] is expected, (label, view)
            plan_view = tool(label + "-tool", "split_plan")
            assert plan_view["applied"] is expected and plan_view["plan"] == view["plan"], (label, plan_view)
            assert view["documentRevision"] == before["revision"]
            assert view["draftRevision"] == before["partSplit"]["revision"]
            assert tool(label + "-after", "snapshot") == before, label
            return before

        draft("review-replay")
        applied_state("replay-origin", True)
        for index, part in enumerate(created["created"]):
            tool("rename-origin-" + str(index), "NodeCommand_SetNodeName",
                 {"id": part["id"], "name": "Renamed " + part["name"]})
        applied_state("renamed-origin", True)
        left = first["plan"]["parts"][0]
        tool("identical-draft", "split_part_set", left)
        applied_state("identical-origin", True)
        tool("padding-draft", "split_part_set", dict(left, padding=1))
        applied_state("changed-padding", False)
        tool("restore-padding", "split_part_set", left)
        applied_state("restored-padding", True)
        tool("steps-draft", "split_part_set",
             {"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 2, 8]}]})
        applied_state("changed-steps", False)
        tool("restore-steps", "split_part_set", left)
        tool("reverse-origin", "split_part_order", {"names": ["Right", "Left"]})
        applied_state("changed-order", False)
        tool("restore-order", "split_part_order", {"names": ["Left", "Right"]})
        applied_state("restored-order", True)
        tool("name-draft", "split_part_set", dict(left, name="ChangedLeft"))
        tool("remove-old-name", "split_part_remove", {"name": "Left"})
        tool("order-changed-name", "split_part_order", {"names": ["ChangedLeft", "Right"]})
        applied_state("changed-name", False)
        tool("remove-changed-name", "split_part_remove", {"name": "ChangedLeft"})
        tool("restore-name", "split_part_set", left)
        tool("restore-name-order", "split_part_order", {"names": ["Left", "Right"]})
        applied_state("restored-name", True)
        draft("different-source", other)
        applied_state("changed-source", False)
        draft("original-source")
        before_repeat = applied_state("reopened-source-origin", True)
        repeated = tool("explicit-repeat", "split_apply")
        assert [part["name"] for part in repeated["created"]] == ["Left", "Right"]
        after_repeat = applied_state("repeated-origin", True)
        assert len(after_repeat["model"]["nodes"]["children"]) == 4
        assert after_repeat["revision"] == before_repeat["revision"] + 1
        assert after_repeat["history"]["count"] == before_repeat["history"]["count"] + 1
        tool("repeat-undo", "undo")
        assert applied_state("older-origin-still-present", True)["model"] == before_repeat["model"]
        tool("repeat-redo", "redo")
        assert applied_state("repeat-redone", True)["model"] == after_repeat["model"]
        for index in range(3):
            tool("unrelated-" + str(index), "NodeCommand_AddNode", {"className": "Node", "_suffix": str(index)})
        before_limit = applied_state("unrelated-origin", True)
        tool("limit-origin-history", "history_limit_set", {"limit": 2})
        after_limit = applied_state("pruned-origin", True)
        assert after_limit["model"] == before_limit["model"] and after_limit["history"]["limit"] == 2
        tool("clear-origin-history", "history_clear")
        after_clear = applied_state("cleared-origin", True)
        assert after_clear["model"] == after_limit["model"] and after_clear["history"]["count"] == 0
        saved = run / "origin-roundtrip.inx"
        tool("save-origin", "FileCommand_SaveFile", {"file": str(saved)})
        assert applied_state("saved-origin", True)["model"] == after_clear["model"]
        assert saved.is_file() and str(source).encode() not in saved.read_bytes()
        tool("reopen-origin", "open", {"path": str(saved)})
        reopened = tool("reopened-origin", "snapshot")
        expected_reopened = copy.deepcopy(after_clear["model"])
        for node in expected_reopened["nodes"]["children"]:
            if node["type"] == "Part":
                assert node["mesh"]["verts"] == [-2, -4, -2, 4, 2, -4, 2, 4]
                assert node["mesh"]["uvs"] == [0, 0, 0, 1, 1, 0, 1, 1]
                assert node["mesh"]["indices"] == [0, 1, 2, 2, 1, 3]
                node.update(vertices=[[-2, -4], [-2, 4], [2, -4], [2, 4]],
                            uvs=[[0, 0], [0, 1], [1, 0], [1, 1]], indices=[0, 1, 2, 2, 1, 3])
        assert reopened["model"] == expected_reopened, "open adds canonical mesh aliases without changing geometry"
        assert reopened["partSplit"]["open"] is False and reopened["partSplit"]["applied"] is False
        draft("reopened-draft")
        applied_state("origin-not-persisted", False)

        tool("new-branch", "new")
        tool("branch-history-limit", "history_limit_set", {"limit": 100})
        draft("branch-draft")
        applied_state("branch-before-apply", False)
        original = tool("branch-apply", "split_apply")
        branch_applied = applied_state("branch-applied", True)
        tool("branch-undo", "undo")
        assert applied_state("branch-undone", False)["model"]["nodes"].get("children", []) == []
        tool("branch-redo", "redo")
        assert applied_state("branch-redone", True)["model"] == branch_applied["model"]
        tool("branch-undo-again", "undo")
        branch_before = applied_state("branch-ready", False)
        invalid = copy.deepcopy(first["plan"])
        invalid["version"] = 2
        tool("invalid-apply", "split_apply", {"plan": invalid}, refused=True)
        assert tool("after-invalid-apply", "snapshot") == branch_before
        foreign = copy.deepcopy(first["plan"])
        foreign["parts"] = [dict(foreign["parts"][0], name="Unrelated")]
        foreign_created = tool("foreign-branch-apply", "split_apply", {"plan": foreign})
        assert foreign_created["created"][0]["id"] == original["created"][0]["id"]
        foreign_state = applied_state("foreign-not-original", False)
        assert foreign_state["history"]["position"] == branch_applied["history"]["position"]
        assert foreign_state["partSplit"]["revision"] == branch_applied["partSplit"]["revision"]
        assert [(node["name"], node["type"]) for node in foreign_state["model"]["nodes"]["children"]] == [
            ("Unrelated", "Part"),
        ]
        restored = tool("apply-after-branch", "split_apply")
        applied_state("original-after-branch", True)
        restored_ids = [part["id"] for part in restored["created"]]
        tool("delete-one-origin", "node_delete", {"nodes": restored_ids[:1]})
        applied_state("partial-origin", True)
        tool("delete-last-origin", "node_delete", {"nodes": restored_ids[1:]})
        applied_state("deleted-origin", False)
        reused = tool("foreign-after-delete", "split_apply", {"plan": foreign})
        assert reused["created"][0]["id"] == restored_ids[0]
        applied_state("reused-id-not-origin", False)

        source_uri = "resource://soul2d/part-split/source"
        mask_prefix = "resource://soul2d/part-split/masks/"
        tool("images-new", "new")
        pixels = bytes([9, 8, 7, 0]) + bytes([9, 8, 7, 255]) * 63
        image_path = run / "loaded-source.png"
        image_path.write_bytes(PNG.png(8, 8, pixels))
        tool("images-open", "split_open", {"path": str(image_path)})
        name = "髪 L/&?+#%"
        mask_uri = mask_prefix + quote(name, safe="")
        tool("images-mask", "split_part_set", {"name": name, "padding": 2, "steps": [
            {"op": "box", "rect": [0, 0, 4, 8]}, {"op": "feather", "radius": 1},
        ]})
        tool("images-ref", "split_part_set", {"name": "A%2F", "steps": [{"op": "part", "name": name}]})

        def image_resource(label, uri, channels, expected):
            before = tool(label + "-before", "snapshot")
            response = rpc(label, ["read", uri], binary=True)
            [item] = response["result"]["contents"]
            assert item["uri"] == uri and item["mimeType"] == "image/png" and "text" not in item
            png = base64.b64decode(item["blob"], validate=True)
            assert REAL.decode_png(png) == (8, 8, channels, expected), label
            assert tool(label + "-after", "snapshot") == before, label

        image_resource("images-source", source_uri, 4, pixels)
        coverage = bytes([255, 255, 255, 170, 85, 0, 0, 0]) * 8
        image_resource("images-soft-mask", mask_uri, 1, coverage)
        image_resource("images-reference", mask_prefix + "A%252F", 1, coverage)
        listed_images = rpc("images-list", ["resources", "list"])["result"]["resources"]
        for uri in (source_uri, mask_uri, mask_prefix + "A%252F"):
            [item] = [item for item in listed_images if item["uri"] == uri]
            assert item["mimeType"] == "image/png" and "blob" not in item and "text" not in item
        templates = rpc("images-templates", ["resources", "templates"])["result"]["resourceTemplates"]
        assert any(item["uriTemplate"] == mask_prefix + "{name}" and item["mimeType"] == "image/png"
                   for item in templates)
        image_path.write_bytes(PNG.png(8, 8, bytes([0, 255, 0, 255]) * 64))
        image_resource("images-still-loaded", source_uri, 4, pixels)
        tool("images-apply", "split_apply")
        assert tool("images-applied", "snapshot")["partSplit"]["applied"] is True
        image_resource("images-applied-mask", mask_uri, 1, coverage)

        def image_error(label, uri, code):
            before = tool(label + "-before", "snapshot")
            response = rpc(label, ["read", uri], expect_rpc_error=True)
            assert response["error"]["code"] == code, response
            assert tool(label + "-after", "snapshot") == before

        for index, suffix in enumerate(("", "%", "%GG", "%FF", "bad/raw", "bad?query", "bad#fragment")):
            image_error("images-malformed-" + str(index), mask_prefix + suffix, -32602)
        image_error("images-missing", mask_prefix + "Missing", -32002)
        tool("images-reopen", "split_open", {"path": str(image_path)})
        image_resource("images-reloaded", source_uri, 4, bytes([0, 255, 0, 255]) * 64)
        image_error("images-mask-reset", mask_uri, -32002)
        tool("images-close", "split_close")
        image_error("images-closed-source", source_uri, -32002)
        image_error("images-closed-mask", mask_uri, -32002)
        closed_images = rpc("images-closed-list", ["resources", "list"])["result"]["resources"]
        assert any(item["uri"] == source_uri for item in closed_images)
        assert not any(item["uri"].startswith(mask_prefix) for item in closed_images)

        components_closed = tool("components-closed-state", "snapshot")
        tool("components-closed", "split_components", refused=True)
        assert tool("components-still-closed", "snapshot") == components_closed
        tool("components-new", "new")
        islands = {(0, 0): 1, (1, 0): 128, (0, 1): 255, (1, 1): 255,
                   (7, 0): 255, (3, 3): 128, (4, 4): 129, (6, 7): 255, (7, 7): 255}
        island_path = run / "islands.png"
        island_path.write_bytes(PNG.png(8, 8, bytes(
            channel for y in range(8) for x in range(8) for channel in (32, 64, 96, islands.get((x, y), 0)))))
        tool("components-open", "split_open", {"path": str(island_path)})
        before_components = tool("components-before", "snapshot")
        assert tool("components-default", "split_components") == {
            "components": [{"seed": [0, 0], "pixels": 4, "bbox": [0, 0, 2, 2]},
                           {"seed": [7, 0], "pixels": 1, "bbox": [7, 0, 1, 1]},
                           {"seed": [3, 3], "pixels": 1, "bbox": [3, 3, 1, 1]},
                           {"seed": [4, 4], "pixels": 1, "bbox": [4, 4, 1, 1]},
                           {"seed": [6, 7], "pixels": 2, "bbox": [6, 7, 2, 1]}],
            "totalComponents": 5, "nextOffset": None,
        }
        assert tool("components-strict", "split_components", {"threshold": 128}) == {
            "components": [{"seed": [7, 0], "pixels": 1, "bbox": [7, 0, 1, 1]},
                           {"seed": [0, 1], "pixels": 2, "bbox": [0, 1, 2, 1]},
                           {"seed": [4, 4], "pixels": 1, "bbox": [4, 4, 1, 1]},
                           {"seed": [6, 7], "pixels": 2, "bbox": [6, 7, 2, 1]}],
            "totalComponents": 4, "nextOffset": None,
        }
        assert tool("components-empty", "split_components", {"threshold": 255}) == {
            "components": [], "totalComponents": 0, "nextOffset": None,
        }
        for index, arguments in enumerate(({"threshold": -1}, {"threshold": 256}, {"threshold": 0.5},
                                            {"offset": -1}, {"offset": 0.5}, {"offset": 4294967296})):
            tool("components-invalid-" + str(index), "split_components", arguments, refused=True)
        assert tool("components-after", "snapshot") == before_components
        tool("components-select", "split_part_set", {"name": "Island", "steps": [{"op": "component", "point": [0, 0]}]})
        mask = bytes(255 if i in (0, 1, 8, 9) else 0 for i in range(64))
        image_resource("components-seed-mask", mask_prefix + "Island", 1, mask)
        tool("components-apply", "split_apply")
        before_applied_components = tool("components-applied-before", "snapshot")
        assert before_applied_components["partSplit"]["applied"] is True
        tool("components-applied-read", "split_components")
        assert tool("components-applied-after", "snapshot") == before_applied_components
        checker_path = run / "checkerboard.png"
        checker_path.write_bytes(PNG.png(16, 16, bytes(channel for y in range(16) for x in range(16)
            for channel in (1, 2, 3, 255 if (x+y) % 2 == 0 and y*8+x//2 < 65 else 0))))
        tool("components-checker", "split_open", {"path": str(checker_path)})
        checker_state = tool("components-checker-before", "snapshot")
        for offset in (0, 32, 64, 65, 4294967295):
            expected = [{"seed": [2*(i % 8)+(i//8) % 2, i//8], "pixels": 1,
                         "bbox": [2*(i % 8)+(i//8) % 2, i//8, 1, 1]}
                        for i in range(offset, min(offset+32, 65))]
            assert tool("components-page-" + str(offset), "split_components", {"offset": offset}) == {
                "components": expected, "totalComponents": 65,
                "nextOffset": offset+len(expected) if offset+len(expected) < 65 else None,
            }
            assert tool("components-page-state-" + str(offset), "snapshot") == checker_state

        tool("raster-new", "new")
        raster_source = run / "raster-source.png"
        raster_source.write_bytes(PNG.png(8, 8, bytes([200, 100, 50, 255]) * 64))
        tool("raster-open", "split_open", {"path": str(raster_source)})
        if os.name == "posix":
            fifo = run / "raster.fifo"
            os.mkfifo(fifo)
            alias = run / "raster-fifo-link"
            alias.symlink_to(fifo)
            before_fifo = tool("raster-fifo-before", "snapshot")
            for index, path in enumerate((fifo, alias)):
                refused = tool(f"raster-fifo-inspect-{index}", "split_mask_inspect",
                               {"path": str(path)}, refused=True)
                assert "regular file" in json.dumps(refused)
                tool(f"raster-fifo-set-{index}", "split_part_set", {"name": "FIFO", "steps": [
                    {"op": "raster", "path": str(path), "crc32": 0}]}, refused=True)
                assert tool(f"raster-fifo-after-{index}", "snapshot") == before_fifo
        raster_path = run / "raster-mask.png"
        coverage = bytes([0, 32, 64, 128, 192, 255, 16, 255]) * 8
        encoded_mask = gray_png(8, 8, coverage)
        assert REAL.decode_png(encoded_mask) == (8, 8, 1, coverage)
        raster_path.write_bytes(encoded_mask)
        raster_before = tool("raster-before-inspect", "snapshot")
        inspected = tool("raster-inspect", "split_mask_inspect", {"path": str(raster_path)})
        assert inspected == {"width": 8, "height": 8, "step": {
            "op": "raster", "path": str(raster_path.resolve()), "crc32": zlib.crc32(coverage), "mode": "add"}}
        assert tool("raster-after-inspect", "snapshot") == raster_before
        tool("raster-set", "split_part_set", {"name": "Raster", "steps": [inspected["step"]]})
        image_resource("raster-coverage", mask_prefix + "Raster", 1, coverage)
        raster_plan = tool("raster-plan", "split_plan")["plan"]
        assert raster_plan["parts"][0]["steps"] == [inspected["step"]]
        write_json(run / "raster-plan.json", raster_plan)
        raster_ready = tool("raster-before-apply", "snapshot")
        tool("raster-apply", "split_apply")
        raster_applied = tool("raster-applied", "snapshot")
        assert raster_applied["revision"] == raster_ready["revision"] + 1
        assert raster_applied["history"]["count"] == raster_ready["history"]["count"] + 1
        tool("raster-undo", "undo")
        assert tool("raster-undone", "snapshot")["model"] == raster_ready["model"]
        tool("raster-redo", "redo")
        assert tool("raster-redone", "snapshot")["model"] == raster_applied["model"]
        replay_run = run / "raster-fresh-host"
        replay_run.mkdir()
        previous_port = port
        replay_host = None
        try:
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            replay_host = DRIVER.Host(replay_run, core, njc, port)
            write_json(replay_run / "instance.json", {"pid": replay_host.child.pid, "port": port})
            replay_host.wait_ready()
            tool("raster-fresh-new", "new")
            tool("raster-fresh-replay", "split_apply", {"plan": raster_plan})
            replayed = tool("raster-fresh-replayed", "snapshot")
            assert replayed["model"] == raster_applied["model"]
            assert replayed["partSplit"]["open"] is False
        finally:
            port = previous_port
            if replay_host is not None:
                replay_host.close()
                write_json(replay_run / "cleanup.json", {"pid": replay_host.child.pid,
                    "exit": replay_host.child.returncode, "running": replay_host.child.poll() is None})
        raster_path.write_bytes(gray_png(8, 8, bytes([128]) * 64))
        stale_before = tool("raster-stale-before", "snapshot")
        for name in ("split_plan", "split_overlay", "split_apply"):
            tool("raster-stale-" + name, name, refused=True)
            assert tool("raster-stale-state-" + name, "snapshot") == stale_before
        image_error("raster-stale-resource", mask_prefix + "Raster", -32002)
        raster_path.write_bytes(encoded_mask)
        assert tool("raster-restored", "split_plan")["plan"] == raster_plan
        raster_path.unlink()
        tool("raster-missing", "split_apply", refused=True)
        assert tool("raster-missing-state", "snapshot") == stale_before
        raster_path.write_bytes(encoded_mask)
        assert tool("raster-final-plan", "split_plan")["plan"] == raster_plan

        native = ROOT / "rust" / "tests" / "fixtures" / "physics3-editor.cmo3"
        native_hash = digest(native)
        tool("native", "open", {"path": str(native)})
        native_state = tool("native-before", "snapshot")
        assert native_state["documentFormat"] == "live2d.cmo3"
        assert native_state["partSplit"]["applied"] is False
        native_resources = rpc("native-resources", ["resources", "list"])["result"]["resources"]
        assert not any(item["uri"] == URI for item in native_resources)
        assert not any(item["uri"] == source_uri or item["uri"].startswith(mask_prefix) for item in native_resources)
        image_error("images-native-source", source_uri, -32002)
        image_error("images-native-mask", mask_uri, -32002)
        context("native-refused", "split plans require an INP/INX document")
        tool("components-native-refused", "split_components", refused=True)
        tool("raster-native-refused", "split_mask_inspect", {"path": str(raster_path)}, refused=True)
        assert tool("native-after", "snapshot") == native_state
        assert digest(native) == native_hash
        result.update(status="PASS", initialDocumentRevision=document_revision, savedPlan=str(run / "plan.json"))
    except BaseException:
        result["error"] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                write_json(run / "cleanup.json", {"pid": host.child.pid, "exit": host.child.returncode,
                                                  "running": host.child.poll() is None})
            after_hashes = {name: digest(path) for name, path in binaries.items()}
            assert after_hashes == before_hashes, "binary changed during proof"
            assert {str(path.relative_to(ROOT)): digest(path) for path in sorted(sources)} == source_hashes, \
                "source changed during proof"
        except BaseException:
            result.update(status="FAIL", cleanupError=traceback.format_exc())
            raise
        finally:
            write_json(run / "result.json", result)
            print(run)


if __name__ == "__main__":
    main()
