#!/usr/bin/env python3
"""Live proof of `animation.physics.bake` (ANI25) over the shipped njc client.

Launches a disposable headless `soul2d-core` MCP host on a free port, drives it
with `njc --port PORT tools call`, and keeps the wire log, fixture, host log and
result.json under a unique build/verification/ani25-26/run-* directory.

Proves: the tool is listed with its schema, a synthetic clip with a moving
SimplePhysics pendulum bakes its output parameter to Linear/Forced keys matching
an independently stepped physics recipe, undo/redo and save/reopen round-trip the
bake, and a refusal leaves the document untouched.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import time
import traceback
import zlib

ROOT = Path(__file__).resolve().parents[1]
EVIDENCE_ROOT = ROOT / "build" / "verification" / "ani25-26"

TIMESTEP = 0.0625
LENGTH = 11


def png() -> bytes:
    """A 2x2 white PNG, stdlib only."""
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
    raw = b"".join(b"\x00" + b"\xff\xff\xff\xff" * 2 for _ in range(2))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b""))


def simple(uuid, name, param, map_mode, length):
    node = {
        "uuid": uuid, "name": name, "type": "SimplePhysics", "enabled": True, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "model_type": "Pendulum", "map_mode": map_mode, "gravity": 1.0, "length": length,
        "frequency": 1.0, "angle_damping": 0.5, "length_damping": 0.5,
        "output_scale": [1.0, 1.0], "local_only": False, "children": [],
    }
    if param is not None:
        node["param"] = param
    return node


def model():
    return {
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": True, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": False, "pinToMesh": False,
            "children": [{
                "uuid": 2, "name": "Body", "type": "Part", "enabled": True, "zsort": 0.0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "lockToRoot": False, "pinToMesh": False,
                "mesh": {"verts": [-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0],
                         "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                         "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
                "textures": [0], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
                "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5,
                "opacity": 1.0,
                "children": [
                    simple(3, "Hair", 11, "XY", 30.0),
                    simple(5, "Fringe", 13, "AngleLength", 60.0),
                ],
            }],
        },
        "param": [
            {"uuid": 10, "name": "Move", "is_vec2": False, "min": [0.0, 0.0], "max": [1.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": [{"node": 2, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                           "values": [[0.0], [30.0]], "isSet": [[True], [True]]}]},
            {"uuid": 11, "name": "Swing", "is_vec2": True, "min": [-2.0, -2.0], "max": [2.0, 2.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
             "bindings": []},
            {"uuid": 13, "name": "Tilt", "is_vec2": False, "min": [-2.0, 0.0], "max": [2.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": []},
        ],
        "automation": [],
        "animations": {
            "Run": {"length": LENGTH, "timestep": TIMESTEP, "additive": False,
                    "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                    "lanes": [
                        {"uuid": 10, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                         "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5},
                                       {"frame": 10, "value": 1.0, "tension": 0.5}]},
                    ]},
        },
    }


def inx(model_dict):
    payload = json.dumps(model_dict, separators=(",", ":")).encode()
    texture = png()
    return (b"TRNSRTS\0" + struct.pack(">I", len(payload)) + payload
            + b"TEX_SECT" + struct.pack(">I", 1)
            + struct.pack(">I", len(texture)) + b"\x00" + texture)


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Host:
    """The disposable soul2d-core MCP host and its njc client calls."""

    def __init__(self, run, core, njc_binary, port):
        """Spawns the host child; call wait_ready() before driving it."""
        self.run = run
        self.port = port
        self.njc = njc_binary
        self.log = (run / "host.log").open("w")
        try:
            self.child = subprocess.Popen(
                [str(core), "--mcp-port", str(port)],
                cwd=ROOT, stdout=self.log, stderr=subprocess.STDOUT)
        except Exception:
            self.log.close()
            raise

    def wait_ready(self):
        """Blocks until this child owns the loopback TCP listener on its port (30s cap)."""
        deadline = time.monotonic() + 30
        while True:
            assert self.child.poll() is None, f"host exited; see {self.run / 'host.log'}"
            listeners = subprocess.run(
                ["lsof", "-t", f"-iTCP:{self.port}", "-sTCP:LISTEN"],
                capture_output=True, text=True, timeout=5).stdout.split()
            if str(self.child.pid) in listeners:
                assert listeners == [str(self.child.pid)], listeners
                return
            assert time.monotonic() < deadline, "MCP listener did not become ready"
            time.sleep(0.2)

    def call(self, label, name, arguments, expected_error=False):
        command = [str(self.njc), "--port", str(self.port),
                   "tools", "call", name, "--json", json.dumps(arguments)]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
        with (self.run / "commands.jsonl").open("a") as wire:
            wire.write(json.dumps({"label": label, "command": command[2:],
                                   "exit": result.returncode}) + "\n")
            wire.write(result.stdout + "\n")
        if result.returncode != 0:
            raise RuntimeError(
                f"njc exited {result.returncode} for {label}: {result.stderr.strip()}")
        response = json.loads(result.stdout)
        assert "error" not in response, (label, response)
        mcp_result = response["result"]
        assert bool(mcp_result.get("isError", False)) == expected_error, (label, mcp_result)
        if expected_error:
            return mcp_result
        text = next(c["text"] for c in mcp_result["content"] if c["type"] == "text")
        return json.loads(text)

    def tools(self):
        command = [str(self.njc), "--port", str(self.port), "tools", "list"]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
        result.check_returncode()
        (self.run / "tools-list.json").write_text(result.stdout)
        return json.loads(result.stdout)["result"]["tools"]

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)
        self.log.close()
        assert self.child.returncode in (0, -15), self.child.returncode


def close(actual, expected):
    assert abs(actual - expected) <= 1e-5 * max(1.0, abs(expected)), (actual, expected)


def lanes(animation, uuid):
    return [lane for lane in animation.get("lanes", []) if lane["uuid"] == uuid]


def verify(host, run):
    fixture = run / "fixture.inx"
    fixture.write_bytes(inx(model()))
    host.call("open", "open", {"path": str(fixture)})

    tool = next(t for t in host.tools() if t["name"] == "animation_physics_bake")
    schema = tool["inputSchema"]
    assert schema["required"] == ["name"], schema
    for key in ["parameters", "from", "to", "warmup"]:
        assert key in schema["properties"], schema

    expected = []
    host.call("seek", "animation_seek", {"name": "Run", "frame": 0})
    for index, frame in enumerate(range(LENGTH)):
        if index > 0:
            host.call("warm-advance", "animation_advance", {"dt": TIMESTEP})
        host.call("warm-step", "physics_step", {"dt": TIMESTEP})
    for frame in range(LENGTH):
        host.call("capture-advance", "animation_advance", {"dt": TIMESTEP})
        snapshot = host.call("capture-step", "physics_step", {"dt": TIMESTEP})
        expected.append(snapshot["values"]["11"])
    before = host.call("snapshot", "snapshot", {})

    baked = host.call("bake", "animation_physics_bake", {"name": "Run"})
    assert baked == {"name": "Run", "from": 0, "to": 10, "frames": 11, "keys": 33,
                     "parameters": [11, 13], "warmup": True}, baked

    snapshot = host.call("after-bake", "snapshot", {})
    animation = snapshot["model"]["animations"]["Run"]
    swing = lanes(animation, 11)
    assert len(swing) == 2, swing
    for lane in swing:
        assert lane["interpolation"] == "Linear" and lane["merge_mode"] == "Forced"
        assert [key["frame"] for key in lane["keyframes"]] == list(range(11))
    assert len(lanes(animation, 13)) == 1
    read = subprocess.run(
        [str(host.njc), "--port", str(host.port), "resources", "read", "resource://vsoul/snapshot"],
        cwd=ROOT, capture_output=True, text=True, timeout=30)
    (run / "snapshot-resource.json").write_text(read.stdout)
    if read.returncode != 0:
        raise RuntimeError(f"njc resources read exited {read.returncode}: {read.stderr.strip()}")
    envelope = json.loads(read.stdout)
    assert "error" not in envelope and "result" in envelope, envelope
    contents = envelope["result"]["contents"]
    assert isinstance(contents, list) and contents and isinstance(contents[0].get("text"), str), envelope
    observed = json.loads(contents[0]["text"])
    assert isinstance(observed.get("model"), dict), observed
    assert observed["model"]["animations"]["Run"] == animation

    host.call("physics-reset", "physics_reset", {})
    for frame in range(LENGTH):
        sampled = host.call("sample", "animation_sample", {"name": "Run", "frame": frame})
        close(sampled["values"]["11"][0], expected[frame][0])
        close(sampled["values"]["11"][1], expected[frame][1])

    host.call("undo", "undo", {})
    assert host.call("snapshot-undo", "snapshot", {})["model"] == before["model"]
    host.call("redo", "redo", {})
    assert host.call("snapshot-redo", "snapshot", {})["model"] == snapshot["model"]

    saved = run / "baked.inx"
    host.call("save", "save", {"path": str(saved), "mark_saved": True})
    host.call("reopen", "open", {"path": str(saved)})
    reopened = host.call("snapshot-reopened", "snapshot", {})
    assert reopened["model"]["animations"]["Run"] == animation

    still = host.call("snapshot-before-refusal", "snapshot", {})
    refusal = host.call("bake-refusal", "animation_physics_bake",
                        {"name": "Run", "parameters": [10]}, expected_error=True)
    (run / "refusal.json").write_text(json.dumps(refusal, indent=2))
    assert host.call("snapshot-after-refusal", "snapshot", {}) == still
    return {"keys": baked["keys"], "frames": baked["frames"], "fixture": str(fixture)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", type=Path, default=ROOT / "out/rust/debug/soul2d-core")
    parser.add_argument("--njc", type=Path, default=ROOT / "out/rust/debug/njc")
    args = parser.parse_args()
    core = args.core.resolve()
    njc = args.njc.resolve()
    for binary in (core, njc):
        assert binary.is_file(), f"build first: ./tools/build-rust-core.sh ({binary} missing)"
    EVIDENCE_ROOT.mkdir(parents=True, exist_ok=True)
    run = EVIDENCE_ROOT / f"run-{time.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}"
    run.mkdir()
    print(run, flush=True)
    port = free_port()
    results = {"core": str(core), "njc": str(njc), "port": port, "run": str(run)}
    host = None
    try:
        host = Host(run, core, njc, port)
        results["hostPid"] = host.child.pid
        host.wait_ready()
        results.update(verify(host, run))
        results["passed"] = True
    except Exception:
        results["passed"] = False
        results["error"] = traceback.format_exc()
    finally:
        if host is not None:
            try:
                host.close()
            except Exception:
                results["cleanupError"] = traceback.format_exc()
                results["passed"] = False
            results["hostExited"] = host.child.poll() is not None
            if not results["hostExited"]:
                results["passed"] = False
        (run / "result.json").write_text(json.dumps(results, indent=2))
    if not results["passed"]:
        print(json.dumps({"passed": False, "run": str(run)}), flush=True)
        sys.exit(1)
    print(json.dumps({"passed": True, "run": str(run)}), flush=True)


if __name__ == "__main__":
    main()
