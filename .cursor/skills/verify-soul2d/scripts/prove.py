#!/usr/bin/env python3
"""Exercise the shipped njc client against an isolated headless soul2d-core MCP host.

Usage: prove.py [PORT]   (default 18088; pass another unused port to run beside a second instance)
SOUL2D_BIN_DIR selects the directory holding soul2d-core and njc (default out/rust/debug).
"""
import base64
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

root = Path(__file__).resolve().parents[4]
bin_dir = Path(os.environ.get("SOUL2D_BIN_DIR") or root / "out/rust/debug").resolve()
evidence = root / "build/verification" / time.strftime("%Y%m%d-%H%M%S")
evidence.mkdir(parents=True, exist_ok=False)
port = int(sys.argv[1]) if len(sys.argv) > 1 else 18088
with socket.socket() as probe:
    # SO_REUSEADDR ignores TIME_WAIT leftovers from a previous run but still refuses a live listener.
    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    probe.bind(("127.0.0.1", port))


def njc(label, *args, expected_error=False):
    command = [str(bin_dir / "njc"), "--port", str(port), *args]
    result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=10)
    (evidence / f"{label}.json").write_text(result.stdout)
    (evidence / f"{label}.stderr").write_text(result.stderr)
    with (evidence / "actions.jsonl").open("a") as log:
        log.write(json.dumps({"command": command, "exit": result.returncode}) + "\n")
    if not expected_error:
        result.check_returncode()
    response = json.loads(result.stdout)
    assert "error" not in response, response
    assert bool(response["result"].get("isError", False)) == expected_error, response
    return response["result"]


def tool(label, name, arguments, expected_error=False):
    result = njc(label, "tools", "call", name, "--json", json.dumps(arguments), expected_error=expected_error)
    if result["content"][0]["type"] == "text":
        value = json.loads(result["content"][0]["text"])
        # The Rust host returns the bare result; wrap it in the {succeeded, result} shape used below.
        if not isinstance(value, dict) or "succeeded" not in value:
            value = {"succeeded": not result.get("isError", False), "result": value}
        assert value["succeeded"] != expected_error, value
        return value
    return result


def png_chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def disc_png(size=64):
    """An opaque red disc on a transparent square: a texture with a real alpha outline."""
    rows = b"".join(
        b"\x00" + b"".join(
            b"\xdc\x50\x3c\xff" if (x - size // 2) ** 2 + (y - size // 2) ** 2 < (size * 7 // 16) ** 2 else bytes(4)
            for x in range(size)
        )
        for y in range(size)
    )
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", header) + png_chunk(b"IDAT", zlib.compress(rows)) + png_chunk(b"IEND", b"")


def capture(label):
    """Live viewport PNG bytes, its size, and whether any pixel is non-zero."""
    image = tool(label, "ViewCommand_CaptureLiveScreenshot", {})
    content = next(c for c in image["content"] if c["type"] == "image")
    assert content["mimeType"] == "image/png"
    png = base64.b64decode(content["data"], validate=True)
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    width, height = struct.unpack(">II", png[16:24])
    assert width > 0 and height > 0
    data, position = b"", 8
    while position < len(png):
        length, kind = struct.unpack(">I4s", png[position:position + 8])
        if kind == b"IDAT":
            data += png[position + 8:position + 8 + length]
        position += 12 + length
    raw = zlib.decompress(data)
    stride = len(raw) // height
    # Byte 0 of a scanline is its filter type; an all-zero image filters to all-zero payloads.
    painted = any(any(raw[y * stride + 1:(y + 1) * stride]) for y in range(height))
    return png, width, height, painted


print(f"Evidence: {evidence}", flush=True)
with tempfile.TemporaryDirectory(prefix="verify-soul2d-") as scratch:
    # The host writes its generated bearer token to the config directory and njc reads it from there; keep both
    # inside scratch so the run never touches the user's settings.
    config = Path(scratch) / "config"
    config.mkdir()
    for name in ("HOME", "XDG_CONFIG_HOME", "APPDATA"):
        os.environ[name] = str(config)
    os.environ.pop("SOUL2D_MCP_BEARER", None)
    os.environ.pop("INOCHI_CONFIG_PATH", None)
    with (evidence / "app.log").open("w") as log:
        app = subprocess.Popen(
            [str(bin_dir / "soul2d-core"), "--mcp-port", str(port)],
            cwd=root, stdout=log, stderr=subprocess.STDOUT,
        )
        (evidence / "instance.json").write_text(json.dumps({
            "pid": app.pid, "port": port, "binary": app.args[0],
        }, indent=2))
        try:
            deadline = time.monotonic() + 30
            while True:
                assert app.poll() is None, "Application exited; inspect app.log"
                listeners = subprocess.run(
                    ["lsof", "-t", f"-iTCP:{port}", "-sTCP:LISTEN"],
                    capture_output=True, text=True, timeout=5,
                ).stdout.split()
                if str(app.pid) in listeners:
                    assert listeners == [str(app.pid)], listeners
                    break
                assert time.monotonic() < deadline, "MCP listener did not become ready"
                time.sleep(0.2)
            tokens = list(config.rglob(f"mcp-{port}.token"))
            assert len(tokens) == 1 and tokens[0].stat().st_mode & 0o077 == 0, tokens
            with socket.create_connection(("127.0.0.1", port), timeout=5) as anonymous:
                body = b'{"jsonrpc":"2.0","id":1,"method":"ping"}'
                anonymous.sendall(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n"
                                  b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body)
                status = anonymous.recv(64).split(b"\r\n", 1)[0].decode()
            assert status == "HTTP/1.1 401 Unauthorized", status
            definitions = njc("doctor-tools", "tools", "list")
            assert any(t["name"] == "NodeCommand_AddNode" for t in definitions["tools"])
            njc("before", "resources", "list")
            created = tool("create", "NodeCommand_AddNode", {"className": "Node", "_suffix": ""})
            uuid = created["result"]["created"][0]["uuid"]
            tool("rename", "NodeCommand_SetNodeName", {
                "newNames": ["Verification Node"], "context": {"nodes": [uuid]},
            })
            # A scene with no texture renders nothing; this is the control for the painted check below.
            assert not capture("capture-empty")[3], "an untextured scene rendered pixels"
            art = Path(scratch) / "disc.png"
            art.write_bytes(disc_png())
            part = tool("merge-image", "merge_images", {"paths": [str(art)]})["result"]["created"][0]
            mesh = tool("automesh", "automesh_generate", {"id": part, "algorithm": "grid"})["result"]
            assert mesh["changed"] and mesh["vertices"] > 4 and mesh["triangles"] > 2, mesh
            project = Path(scratch) / "roundtrip.inx"
            tool("save", "FileCommand_SaveFile", {"file": str(project)})
            assert project.is_file() and project.stat().st_size > 0
            # Retain the written project as evidence before scratch cleanup.
            (evidence / "roundtrip.inx").write_bytes(project.read_bytes())
            tool("open", "FileCommand_OpenFile", {"file": str(project)})
            read = njc("reopened-node", "read", str(uuid))
            item = json.loads(read["contents"][0]["text"])["item"]
            assert item["name"] == "Verification Node", item
            geometry = tool("reopened-geometry", "scene_geometry", {})["result"]["parts"]
            assert [len(p["vertices"]) for p in geometry if p["id"] == part] == [mesh["vertices"]], geometry
            png, width, height, painted = capture("capture")
            assert painted, "the reopened textured Part rendered no pixels"
            (evidence / "viewport.png").write_bytes(png)
            (evidence / "result.json").write_text(json.dumps({
                "status": "PASS", "surface": "production njc/MCP", "node": uuid, "part": part,
                "mesh": mesh, "width": width, "height": height, "painted": painted,
                "auth": "generated token in the scratch config directory; an anonymous request was refused",
                "gui_input": "not exercised",
            }, indent=2))
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait(timeout=5)
            (evidence / "cleanup.json").write_text(json.dumps({
                "pid": app.pid, "exited": app.poll() is not None,
            }))
assert not Path(scratch).exists()
assert (evidence / "viewport.png").is_file()
assert (evidence / "roundtrip.inx").is_file()
print("PASS: node and textured Part save/reopen, automesh, painted viewport capture, and cleanup", flush=True)
