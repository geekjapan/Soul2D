#!/usr/bin/env python3
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback
import zlib

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "rust" / "tests" / "fixtures" / "can3-key-editor.xml"
SNAPSHOT = "resource://vsoul/snapshot"
SOURCE = "resource://soul2d/document/animation-source"
sys.dont_write_bytecode = True


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DRIVER = load("physics_bake_proof", "verify-animation-physics-bake.py")
CAFF = load("caff_proof", "verify-caff-authoring.py")


def caff_pack(xml):
    seed = 0x562E099F
    mask = seed & 0xFF
    deflated = zlib.compressobj(6, zlib.DEFLATED, -15)
    compressed = deflated.compress(xml) + deflated.flush()
    zipp = (0x04034B50).to_bytes(4, "little") + (20).to_bytes(2, "little") \
        + (0x0808).to_bytes(2, "little") + (8).to_bytes(2, "little") \
        + b"\x00" * 4 + b"\x00" * 12 + (8).to_bytes(2, "little") \
        + (0).to_bytes(2, "little") + b"contents" + compressed + b"PK\x07\x08" \
        + zlib.crc32(xml).to_bytes(4, "little") + len(compressed).to_bytes(4, "little") \
        + len(xml).to_bytes(4, "little")
    entries = [
        ("main.xml", "main_xml", [0x01, 0x21], zipp),
        ("mesh.png", "", [0x01, 0x10], DRIVER.png()),
        ("opaque.bin", "", [0x01, 0x10], bytes([0, 1, 2, 0xFF])),
    ]
    data = bytearray(58)
    data[:4] = b"CAFF"
    data[4:8] = (45).to_bytes(4, "big")
    data[14:18] = seed.to_bytes(4, "big")
    data[54:58] = (len(entries) ^ seed).to_bytes(4, "big")
    fields = []
    for name, alias, flags, _ in entries:
        data.append(len(name) ^ mask)
        data.extend(byte ^ mask for byte in name.encode())
        data.append(len(alias) ^ mask)
        data.extend(byte ^ mask for byte in alias.encode())
        data.extend((0x12345678 ^ seed).to_bytes(4, "big"))
        fields.append(len(data))
        data.extend(seed.to_bytes(4, "big"))
        fields.append(len(data))
        data.extend(seed.to_bytes(4, "big"))
        data.extend(byte ^ mask for byte in flags)
        data.extend(byte ^ mask for byte in [0] * 8)
    for index, (_, _, _, payload) in enumerate(entries):
        data[fields[2 * index]:fields[2 * index] + 4] = (len(data) ^ seed).to_bytes(4, "big")
        data[fields[2 * index + 1]:fields[2 * index + 1] + 4] = (len(payload) ^ seed).to_bytes(4, "big")
        data.extend(byte ^ mask for byte in payload)
    data.extend(b"bc")
    return bytes(data)


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


def resource(host, label, uri):
    result = rpc(host, label, "resources", "read", uri)
    return json.loads(result["contents"][0]["text"])


def snapshot(host, label):
    return resource(host, f"{label}-snapshot", SNAPSHOT)


def source_model(host, label):
    return resource(host, f"{label}-source", SOURCE)


def curve(model):
    return model["scenes"][0]["tracks"][0]["parameterCurves"][0]


def frames(model):
    return [point["anchor"]["frame"] for point in curve(model)["points"]]


def state(host, label):
    snap, model = snapshot(host, label), source_model(host, label)
    assert snap["model"] == model, f"{label}: snapshot model differs from animation-source resource"
    return snap, model


def decode_entries(data):
    entries, xml = CAFF.decode(data)
    return entries, xml


def can3_files(run):
    return {path.name: path.read_bytes() for path in run.glob("*.can3")}


def edit(host, label, tool, **arguments):
    snap, _ = state(host, label)
    before = host.call(f"{label}-history", "history_summary", {})
    files = can3_files(host.run)
    arguments["expectedRevision"] = snap["revision"]
    result = host.call(label, tool, arguments)
    snap2, model = state(host, f"{label}-after")
    after = host.call(f"{label}-history2", "history_summary", {})
    assert result["model"] == model, f"{label}: tool result differs from resource"
    assert result["revision"] == snap["revision"] + 1, f"{label}: revision did not advance once"
    assert snap2["revision"] == snap["revision"] + 1, f"{label}: snapshot revision did not advance once"
    assert result["revision"] == snap2["revision"], f"{label}: result/snapshot revision mismatch"
    assert after["position"] == before["position"] + 1, (label, before, after)
    assert after["count"] == before["position"] + 1, (label, before, after)
    assert len(after["states"]) == before["position"] + 2, (label, before, after)
    assert can3_files(host.run) == files, f"{label}: file changed outside save"
    return result, model


def refuse(host, label, tool, arguments, message):
    snap, _ = state(host, label)
    arguments.setdefault("expectedRevision", snap["revision"])
    history = host.call(f"{label}-history", "history_summary", {})
    files = can3_files(host.run)
    response = host.call(label, tool, arguments, expected_error=True)
    assert message in json.dumps(response), (label, response)
    snap2, model2 = state(host, f"{label}-after")
    assert snap2 == snap, f"{label}: snapshot changed"
    assert host.call(f"{label}-history2", "history_summary", {}) == history, label
    assert can3_files(host.run) == files, f"{label}: file changed"
    return response


def element_spans(text, parent_marker, child_name):
    import xml.parsers.expat as expat
    assert text.isascii(), "oracle requires ASCII XML for byte offsets"
    spans = []
    stack = []
    parser = expat.ParserCreate()
    def start(name, attrs):
        if name == child_name and any(a.get("xs.n") == parent_marker for _, a in stack):
            spans.append((parser.CurrentByteIndex, None))
        stack.append((name, attrs))
    def end(name):
        closed = stack.pop()
        if closed[0] == child_name and spans and spans[-1][1] is None:
            at = parser.CurrentByteIndex
            if text.startswith(f"</{name}>", at):
                at += len(f"</{name}>")
            spans[-1] = (spans[-1][0], at)
    parser.StartElementHandler = start
    parser.EndElementHandler = end
    parser.Parse(text, True)
    return spans


def require_fixture_roundtrip(original_can3_bytes, saved_can3_bytes):
    assert original_can3_bytes[:58] == saved_can3_bytes[:58], "CAFF header changed"
    assert original_can3_bytes[-2:] == saved_can3_bytes[-2:] == b"bc", "CAFF trailer changed"
    original_entries, original_xml = decode_entries(original_can3_bytes)
    saved_entries, saved_xml = decode_entries(saved_can3_bytes)
    assert len(original_entries) == len(saved_entries)
    assert [entry[:4] for entry in original_entries] == \
        [entry[:4] for entry in saved_entries], "CAFF entry metadata changed"
    for (names, _, _, _, payload) in original_entries:
        if names[0] != "main.xml":
            saved_match = next(entry for entry in saved_entries if entry[0] == names)
            assert saved_match[4] == payload, f"non-main entry {names[0]} changed"
    text = saved_xml.decode()
    original_text = original_xml.decode()

    point_spans = element_spans(text, "points", "CBezierPt")
    point_frames = []
    for start, end in point_spans:
        body = text[start:end]
        pos = body.index('<i xs.n="pos">')
        pos_end = body.index("</i>", pos)
        point_frames.append(int(body[pos + 14:pos_end]))
    removed_indices = [i for i, frame in enumerate(point_frames) if frame in (5, 7)]
    assert removed_indices == [1, 2], f"unexpected inserted point frames {point_frames}"
    type_spans = element_spans(text, "curveTypes", "CCurveType")
    assert len(type_spans) == len(point_spans)
    removals = sorted([point_spans[i] for i in removed_indices] +
                      [type_spans[i] for i in removed_indices], reverse=True)
    for start, end in removals:
        text = text[:start] + text[end:]

    import re
    for marker in ("points", "curveTypes"):
        tag_start = text.index(f'xs.n="{marker}"')
        tag_start = text.rindex("<", 0, tag_start)
        tag_end = text.index(">", tag_start)
        original_tag_start = original_text.index(f'xs.n="{marker}"')
        original_tag_start = original_text.rindex("<", 0, original_tag_start)
        original_tag_end = original_text.index(">", original_tag_start)
        count = re.search(r'count\s*=\s*["\']([^"\']*)["\']',
                          text[tag_start:tag_end])
        original_count = re.search(r'count\s*=\s*["\']([^"\']*)["\']',
                                   original_text[original_tag_start:original_tag_end])
        assert count and original_count, f"missing count attribute for {marker}"
        if count.group(1) != original_count.group(1):
            tag = text[tag_start:tag_end]
            value_start = tag_start + count.start(1)
            value_end = tag_start + count.end(1)
            text = text[:value_start] + original_count.group(1) + text[value_end:]

    curmax = re.search(r'<d xs\.n="curMax">([^<]*)</d>', text)
    original_curmax = re.search(r'<d xs\.n="curMax">([^<]*)</d>', original_text)
    assert curmax and original_curmax
    if curmax.group(1) != original_curmax.group(1):
        text = text[:curmax.start(1)] + original_curmax.group(1) + text[curmax.end(1):]
    assert text == original_text, "saved XML differs from fixture after span removal"

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--njc", type=Path, required=True)
    args = parser.parse_args()
    evidence = ROOT / "build" / "verification" / "ani145"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    result = {"passed": False,
              "scope": "Synthetic CAN3 key-structure HTTP MCP; native GUI SKIP; vendor acceptance NOT_RUN; local only",
              "run": str(run)}
    host = None
    try:
        manifest = json.loads((ROOT / "doc" / "commands.manifest.json").read_text())
        paths = [args.core, args.njc, FIXTURE, Path(__file__),
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
        for name in ("live2d_animation_key_add", "live2d_animation_key_delete"):
            live = next(tool for tool in tools if tool["name"] == name)
            entry = manifest_tools[name]
            assert live["inputSchema"] == entry["inputSchema"], (name, live)
            assert live["description"] == entry["description"], name
        add_schema = manifest_tools["live2d_animation_key_add"]["inputSchema"]
        anchor = add_schema["properties"]["anchorFrame"]
        assert anchor["type"] == "integer" and anchor["minimum"] == -2147483648 \
            and anchor["maximum"] == 2147483647
        assert add_schema["properties"]["type"]["default"] == "LINEAR"
        assert add_schema["properties"]["type"]["enum"] == \
            ["SMOOTH", "BEZIER", "LINEAR", "STEP"]
        assert add_schema["additionalProperties"] is False
        delete_schema = manifest_tools["live2d_animation_key_delete"]["inputSchema"]
        assert delete_schema["additionalProperties"] is False
        assert delete_schema["properties"]["anchorFrame"]["type"] == "integer"

        can3 = run / "keys.can3"
        fixture_bytes = FIXTURE.read_bytes()
        can3.write_bytes(caff_pack(fixture_bytes))
        original_can3 = can3.read_bytes()
        host.call("open", "open", {"path": str(can3)})
        snap, model = state(host, "opened")
        ids = {"sceneGuid": model["scenes"][0]["guid"],
               "trackGuid": model["scenes"][0]["tracks"][0]["guid"],
               "curveId": model["scenes"][0]["tracks"][0]["parameterCurves"][0]["id"]}
        assert ids == {"sceneGuid": "scene-guid", "trackGuid": "track-guid",
                       "curveId": "live2dParam_ParamTest"}, ids

        _, model = edit(host, "add5", "live2d_animation_key_add",
                        **ids, anchorFrame=5, value=2)
        assert frames(model) == [0, 5, 10]
        assert curve(model)["curveTypes"] == ["LINEAR"] * 3
        point = curve(model)["points"][1]
        assert point["corner"] is True
        for side in ("previous", "next"):
            assert point[side] == {"frame": 5.0, "integerFrame": 5, "value": 2.0,
                                   "positionOptimized": False}, point[side]

        _, model = edit(host, "add7", "live2d_animation_key_add",
                        **ids, anchorFrame=7, value=0.75, type="STEP")
        assert frames(model) == [0, 5, 7, 10]
        assert curve(model)["curveTypes"] == ["LINEAR", "LINEAR", "STEP", "LINEAR"]

        _, model = edit(host, "set5", "live2d_animation_key_set",
                        **ids, anchorFrame=5, value=1.2345678901234567)
        _, model = edit(host, "handle5", "live2d_animation_handle_set",
                        **ids, anchorFrame=5, side="next", frame=6.5, value=1.75)

        def goto(label, index):
            snap, _ = state(host, label)
            return host.call(label, "history_goto",
                             {"index": index, "expectedRevision": snap["revision"]})

        snap, model = state(host, "pre-undo")
        position = snap["history"]["position"]
        goto("undo1", position - 1)
        snap, model = state(host, "undo1")
        assert frames(model) == [0, 5, 7, 10]
        assert curve(model)["points"][1]["anchor"]["value"] == 1.2345678901234567
        assert curve(model)["points"][1]["next"]["value"] == 2.0
        goto("undo2", position - 2)
        snap, model = state(host, "undo2")
        assert frames(model) == [0, 5, 7, 10]
        assert curve(model)["points"][1]["anchor"]["value"] == 2.0
        goto("redo2", position)
        snap, model = state(host, "redo2")
        assert curve(model)["points"][1]["anchor"]["value"] == 1.2345678901234567
        assert curve(model)["points"][1]["next"]["value"] == 1.75

        saved = run / "grown.can3"
        host.call("save-grown", "save", {"path": str(saved)})
        host.call("reopen-grown", "open", {"path": str(saved)})
        snap, model = state(host, "grown")
        assert frames(model) == [0, 5, 7, 10]
        assert curve(model)["curveTypes"] == ["LINEAR", "LINEAR", "STEP", "LINEAR"]
        point = curve(model)["points"][1]
        assert point["anchor"] == {"frame": 5, "value": 1.2345678901234567}
        assert point["next"] == {"frame": 6.5, "integerFrame": 5, "value": 1.75,
                                 "positionOptimized": False}
        assert point["corner"] is True

        require_fixture_roundtrip(original_can3, saved.read_bytes())

        _, grown_xml = decode_entries(saved.read_bytes())
        probed = grown_xml.decode().replace(
            '<CCurveType v="LINEAR"/>', '<CCurveType v="LINEAR" probe="1"/>', 1)
        try:
            require_fixture_roundtrip(original_can3, caff_pack(probed.encode()))
        except AssertionError:
            pass
        else:
            raise AssertionError("probe-mutated retained type was not detected")

        _, model = edit(host, "del0", "live2d_animation_key_delete", **ids, anchorFrame=0)
        assert frames(model) == [5, 7, 10]
        _, model = edit(host, "readd0", "live2d_animation_key_add", **ids, anchorFrame=0, value=0.25)
        point = curve(model)["points"][0]
        assert point["anchor"] == {"frame": 0, "value": 0.25}
        assert point["corner"] is True
        for side in ("previous", "next"):
            assert point[side] == {"frame": 0.0, "integerFrame": 0, "value": 0.25,
                                   "positionOptimized": False}
        readd = run / "readd.can3"
        host.call("save-readd", "save", {"path": str(readd)})
        _, xml = decode_entries(readd.read_bytes())
        assert "opaque-key-data &amp; 0.12345678901234567890" in xml.decode()
        assert "keep-root &amp; 0.1234500" in xml.decode()

        _, model = edit(host, "delall1", "live2d_animation_key_delete", **ids, anchorFrame=0)
        _, model = edit(host, "delall2", "live2d_animation_key_delete", **ids, anchorFrame=5)
        _, model = edit(host, "delall3", "live2d_animation_key_delete", **ids, anchorFrame=7)
        _, model = edit(host, "delall4", "live2d_animation_key_delete", **ids, anchorFrame=10)
        assert curve(model)["points"] == [] and curve(model)["curveTypes"] == []
        empty = run / "empty.can3"
        host.call("save-empty", "save", {"path": str(empty)})
        _, xml = decode_entries(empty.read_bytes())
        assert '<d xs.n="curMin">0.0000</d>' in xml.decode()
        assert '<d xs.n="curMax">1.2345678901234567</d>' in xml.decode()
        _, model = edit(host, "addneg", "live2d_animation_key_add", **ids, anchorFrame=-5, value=-0.25)
        assert frames(model) == [-5]
        refill = run / "refill.can3"
        host.call("save-refill", "save", {"path": str(refill)})
        _, xml = decode_entries(refill.read_bytes())
        assert '<d xs.n="curMin">-0.25</d>' in xml.decode()
        assert '<d xs.n="curMax">-0.25</d>' in xml.decode()

        host.call("reopen-source", "open", {"path": str(can3)})
        _, model = edit(host, "cancel-add", "live2d_animation_key_add",
                        **ids, anchorFrame=5, value=2)
        assert frames(model) == [0, 5, 10]
        _, model = edit(host, "cancel-del", "live2d_animation_key_delete",
                        **ids, anchorFrame=5)
        cancel = run / "cancel.can3"
        host.call("save-cancel", "save", {"path": str(cancel)})
        assert cancel.read_bytes() == original_can3, "cancelled file differs"

        refuse(host, "stale", "live2d_animation_key_add",
               dict(ids, anchorFrame=20, value=1, expectedRevision=0), "revision")
        refuse(host, "duplicate", "live2d_animation_key_add",
               dict(ids, anchorFrame=0, value=1), "already")
        refuse(host, "missing", "live2d_animation_key_delete",
               dict(ids, anchorFrame=99), "was not found")
        refuse(host, "badtype", "live2d_animation_key_add",
               dict(ids, anchorFrame=20, value=1, type="smooth"), "type")
        refuse(host, "nulltype", "live2d_animation_key_add",
               dict(ids, anchorFrame=20, value=1, type=None), "type")
        refuse(host, "badframe", "live2d_animation_key_add",
               dict(ids, anchorFrame=1.5, value=1), "integer")
        refuse(host, "wideframe", "live2d_animation_key_add",
               dict(ids, anchorFrame=2147483648, value=1), "32-bit")
        refuse(host, "badvalue", "live2d_animation_key_add",
               dict(ids, anchorFrame=20, value="x"), "finite")
        refuse(host, "unknownkey", "live2d_animation_key_add",
               dict(ids, anchorFrame=20, value=1, bogus=1), "unknown")
        refuse(host, "wrongid", "live2d_animation_key_delete",
               {"sceneGuid": "none", "trackGuid": ids["trackGuid"],
                "curveId": ids["curveId"], "anchorFrame": 0}, "was not found")
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
