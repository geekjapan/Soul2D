#!/usr/bin/env python3
"""Verify real CMO3/CAN3 containers independently of the Rust implementation."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
import zlib

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("core", ROOT / "tools/verify-rust-core.py")
CORE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CORE)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def decode(data):
    assert data[:4] == b"CAFF" and int.from_bytes(data[4:8], "big") == 45
    seed = int.from_bytes(data[14:18], "big")
    key = seed & 255
    count = int.from_bytes(data[54:58], "big") ^ seed
    position, entries = 58, []
    for _ in range(count):
        names = []
        for _ in range(2):
            length = data[position] ^ key
            position += 1
            names.append(bytes(x ^ key for x in data[position:position + length]).decode())
            position += length
        fields = [int.from_bytes(data[position + i:position + i + 4], "big") ^ seed
                  for i in (0, 4, 8)]
        flags = bytes(x ^ key for x in data[position + 12:position + 14])
        reserved = data[position + 14:position + 22]
        position += 22
        unknown, offset, size = fields
        payload = bytes(x ^ key for x in data[offset:offset + size])
        assert len(payload) == size
        entries.append((names, unknown, flags, reserved, payload))
    payload = next(entry[-1] for entry in entries if entry[0][0] == "main.xml")
    assert payload[:4] == b"PK\x03\x04"
    start = 30 + int.from_bytes(payload[26:28], "little") + int.from_bytes(payload[28:30], "little")
    inflater = zlib.decompressobj(-15)
    xml = inflater.decompress(payload[start:]) + inflater.flush()
    assert inflater.eof
    trailer = inflater.unused_data
    assert len(trailer) == 16 and trailer[:4] == b"PK\x07\x08"
    assert int.from_bytes(trailer[4:8], "little") == zlib.crc32(xml)
    assert int.from_bytes(trailer[8:12], "little") == len(payload) - start - 16
    assert int.from_bytes(trailer[12:16], "little") == len(xml)
    ET.fromstring(xml)
    return entries, xml


def request(binary, operation, path, mcp):
    if mcp:
        messages = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "caff-verifier", "version": "1"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "live2d_authoring_inspect", "arguments": {"path": str(path)}}}]
    else:
        messages = [{"op": operation, "path": str(path)}]
    process = subprocess.run([str(binary)] + (["--mcp"] if mcp else []),
        input="".join(json.dumps(row) + "\n" for row in messages), text=True,
        capture_output=True, check=True, timeout=60)
    responses = [json.loads(row) for row in process.stdout.splitlines()]
    result = responses[-1]["result"]
    if mcp:
        assert not result.get("isError"), result
        result = result.get("structuredContent") or json.loads(result["content"][0]["text"])
    else:
        assert responses[-1]["succeeded"], responses[-1]
    return result, process.stdout + process.stderr


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", type=Path)
    arguments = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="caff-authoring-", dir=ROOT / "build/verification"))
    library = ROOT / "out/rust/debug" / ("libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    binary = ROOT / "out/rust/debug/soul2d-core"
    example = ROOT / "out/rust/debug/examples/caff-roundtrip"
    core = CORE.Core(library)
    result = {"status": "FAIL", "evidence": str(directory), "files": []}
    try:
        seed = directory / "active-document.inx"
        CORE.fixture(seed)
        core.request("open", path=str(seed))
        core.request("node.rename", id=2, name="Preserve active authored model")
        paths = sorted(p.resolve() for p in arguments.inputs.rglob("*") if p.suffix in (".cmo3", ".can3"))
        assert sum(p.suffix == ".cmo3" for p in paths) == 4
        assert sum(p.suffix == ".can3" for p in paths) == 4
        for index, path in enumerate(paths):
            original_hash = sha(path)
            entries, original_xml = decode(path.read_bytes())
            before = core.request("snapshot")
            inspected = core.request("live2d.authoring.inspect", path=str(path))
            assert core.request("snapshot") == before
            tree = ET.fromstring(original_xml)
            assert inspected["fileFormatVersion"] == tree.get("fileFormatVersion")
            assert inspected["container"]["xmlBytes"] == len(original_xml)
            assert [e["name"] for e in inspected["container"]["entries"]] == [e[0][0] for e in entries]
            models = [e for e in tree.iter("CModelSource") if len(e)]
            if models:
                assert len(models) == 1
                def field(element, name):
                    return next(c for c in element if c.get("xs.n") == name)
                expected_parameters = []
                for parameter in models[0].iter("CParameterSource"):
                    if not len(parameter):
                        continue
                    expected_parameters.append({"id": field(parameter, "id").get("idstr"),
                        "name": field(parameter, "name").text or "",
                        "minimum": float(field(parameter, "minValue").text),
                        "maximum": float(field(parameter, "maxValue").text),
                        "default": float(field(parameter, "defaultValue").text)})
                assert inspected["model"]["parameters"] == expected_parameters
                assert inspected["model"]["name"] == field(models[0], "name").text
                canvas = field(models[0], "canvas")
                assert inspected["model"]["canvas"] == {"width": field(canvas, "pixelWidth").text,
                                                          "height": field(canvas, "pixelHeight").text}
            else:
                assert inspected["model"] is None
            for label, tag in (("artMeshes", "CArtMeshSource"), ("warpDeformers", "CWarpDeformerSource"),
                               ("rotationDeformers", "CRotationDeformerSource"), ("glues", "CGlueSource"),
                               ("artPaths", "CArtPathSource")):
                assert inspected["definitions"][label] == sum(bool(len(e)) for e in tree.iter(tag))
            for mcp in (False, True):
                actual, wire = request(binary, "live2d.authoring.inspect", path, mcp)
                assert actual == inspected
                (directory / f"{index}-{'mcp' if mcp else 'jsonl'}.log").write_text(wire)
            process = subprocess.run([str(example), str(path), str(directory / str(index))],
                text=True, capture_output=True, check=True, timeout=60)
            summary = json.loads(process.stdout)
            output = directory / str(index)
            rewritten, xml = decode((output / "rewritten.caff").read_bytes())
            assert (output / "original.xml").read_bytes() == original_xml
            assert (output / "edited.xml").read_bytes() == xml
            assert len(entries) == len(rewritten)
            for old, new in zip(entries, rewritten):
                assert old[:4] == new[:4]
                if old[0][0] != "main.xml":
                    assert old[-1] == new[-1]
            if "parameter" in summary["edit"]:
                assert path.suffix == ".cmo3"
                old = str(summary["edit"]["oldDefault"])
                new = str(summary["edit"]["newDefault"])
                tree = ET.fromstring(xml)
                parameter = next(p for p in tree.iter("CParameterSource")
                    if any(c.get("idstr") == "ParamAngleX" for c in p))
                value = next(c.text for c in parameter if c.get("xs.n") == "defaultValue")
                assert float(value) == float(new) and float(value) != float(old)
            assert sha(path) == original_hash
            (output / "inspection.json").write_text(json.dumps(inspected, indent=2))
            result["files"].append({"source": str(path), "sha256": original_hash,
                "entries": len(entries), "xmlBytes": len(xml), "roundtrip": summary,
                "rewrittenHash": sha(output / "rewritten.caff")})
        result["sources"] = {str(p.relative_to(ROOT)): sha(p) for p in (
            ROOT / "rust/src/caff.rs", ROOT / "rust/src/authoring.rs", ROOT / "rust/src/lib.rs",
            ROOT / "rust/src/authoring_graph.rs",
            ROOT / "rust/src/authoring_values.rs",
            ROOT / "rust/src/main.rs", ROOT / "rust/examples/caff-roundtrip.rs",
            Path(__file__).resolve(), library, binary, example)}
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = str(error)
    finally:
        core.close()
        (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
