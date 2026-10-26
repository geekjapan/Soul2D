#!/usr/bin/env python3
"""Live proof of `animation.motionsync.*` (ANI26) over the shipped njc client.

Launches a disposable headless `soul2d-core` MCP host on a free port with
SOUL2D_RHUBARB pointing at a real Rhubarb Lip Sync executable, drives it with
`njc --port PORT tools call`, and keeps the wire log, fixture, WAV copies, host
log and result.json under a unique build/verification/ani25-26/motionsync-*
directory.

Proves: the four tools are listed with their schemas; a real analysis of the
retained TTS WAV finishes with non-X cues identical to a direct Rhubarb run; the
bake writes Linear/Forced keys whose every-frame samples match the direct cue
list through the literal shape mapping; undo/redo and save/reopen round-trip;
mutating the source WAV afterwards refuses the bake without touching the
document; and a real silence analysis reports only the rest shape X.
"""

import argparse
import json
import math
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import time
import traceback
import zlib

ROOT = Path(__file__).resolve().parents[1]
EVIDENCE_ROOT = ROOT / "build" / "verification" / "ani25-26"
TIMESTEP = 0.125
AUDIO_FRAME = 2
LENGTH = 64

MOUTH_SHAPES = {"A": 0.1, "B": 0.2, "C": 0.4, "D": 1.0, "E": 0.6, "F": 0.25, "G": 0.15, "H": 0.7, "X": 0.0}
JAW_SHAPES = {"A": 0.0, "B": 0.1, "C": 0.2, "D": 0.5, "E": 0.3, "F": -0.75, "G": 0.1, "H": 0.4, "X": 0.0}


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


def silence_wav(path: Path, seconds: float = 2.0, rate: int = 16000):
    """Writes a PCM16 mono silence WAV with the stdlib, so no retained artifact is needed."""
    import wave
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(b"\x00\x00" * int(seconds * rate))


def model(wav):
    """A Part the Mouth parameter (10) lifts by up to 30 units, plus a 2D Jaw (11)."""
    return {
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": True, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": False, "pinToMesh": False,
            "children": [{
                "uuid": 9, "name": "Cam", "type": "Camera", "enabled": True, "zsort": 0.0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "lockToRoot": False, "pinToMesh": False, "viewport": [128.0, 128.0],
                "children": [],
            }, {
                "uuid": 2, "name": "Body", "type": "Part", "enabled": True, "zsort": 0.0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "lockToRoot": False, "pinToMesh": False,
                "mesh": {"verts": [-12.0, -12.0, -12.0, 12.0, 12.0, -12.0, 12.0, 12.0],
                         "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                         "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
                "textures": [0], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
                "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5,
                "opacity": 1.0,
                "children": [],
            }],
        },
        "param": [
            {"uuid": 10, "name": "Mouth", "is_vec2": False, "min": [0.0, 0.0], "max": [1.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": [{"node": 2, "param_name": "transform.t.y", "interpolate_mode": "Linear",
                           "values": [[0.0], [30.0]], "isSet": [[True], [True]]}]},
            {"uuid": 11, "name": "Jaw", "is_vec2": True, "min": [-1.0, -1.0], "max": [1.0, 1.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
             "bindings": []},
        ],
        "automation": [],
        "animations": {
            "Talk": {"length": LENGTH, "timestep": TIMESTEP, "additive": False,
                     "animationWeight": 1.0, "leadIn": -1, "leadOut": -1, "lanes": [],
                     "audio": [{"path": wav, "frame": AUDIO_FRAME}]},
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


def analyze_directly(rhubarb, wav):
    """Runs Rhubarb on `wav` outside the session: the independent cue source."""
    command = [str(rhubarb), "--recognizer", "phonetic", "--exportFormat", "json",
               "--extendedShapes", "GHX", "--threads", "2", "--quiet", str(wav)]
    result = subprocess.run(command, capture_output=True, timeout=60)
    assert result.returncode == 0, (command, result.stderr[-2000:])
    return json.loads(result.stdout)


def shape_at(cues, at):
    """The value letter of the half-open cue covering audio time `at`, X outside."""
    for cue in cues:
        if cue["start"] <= at < cue["end"]:
            return cue["value"]
    return "X"


class Host:
    """The disposable soul2d-core MCP host and its njc client calls."""

    def __init__(self, run, core, njc_binary, port, rhubarb):
        """Spawns the host child; call wait_ready() before driving it."""
        self.run = run
        self.port = port
        self.njc = njc_binary
        self.log = (run / "host.log").open("w")
        env = dict(os.environ, SOUL2D_RHUBARB=str(rhubarb))
        try:
            self.child = subprocess.Popen(
                [str(core), "--mcp-port", str(port)],
                cwd=ROOT, env=env, stdout=self.log, stderr=subprocess.STDOUT)
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

    def call(self, label, name, arguments, expected_error=False, timeout=60):
        command = [str(self.njc), "--port", str(self.port),
                   "tools", "call", name, "--json", json.dumps(arguments)]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=timeout)
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


def settle(host, job, label):
    """Polls the job until it leaves 'running' (60s cap)."""
    deadline = time.monotonic() + 60
    while True:
        status = host.call(f"{label}-status", "animation_motionsync_status", {"job": job})
        if status["state"] != "running":
            return status
        assert time.monotonic() < deadline, f"{label} did not settle"
        time.sleep(0.1)


def png_size(path):
    """The (width, height) of a PNG file, from its IHDR."""
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR", path
    return struct.unpack(">II", data[16:24])


def verify(host, run, rhubarb, speech, silence):
    tools = {tool["name"]: tool for tool in host.tools()}
    for name in ["animation_motionsync_start", "animation_motionsync_status",
                 "animation_motionsync_cancel", "animation_motionsync_bake"]:
        assert name in tools, name
    bake_schema = tools["animation_motionsync_bake"]["inputSchema"]
    assert bake_schema["required"] == ["name", "job", "audio_index", "mappings"], bake_schema
    properties = bake_schema["properties"]
    for key in ["audio_index", "from", "to"]:
        assert properties[key]["type"] == "integer" and properties[key]["minimum"] == 0, \
            (key, properties[key])
    assert properties["job"]["minimum"] == 1 and "maximum" not in properties["job"]
    shapes = properties["mappings"]["items"]["properties"]["shapes"]
    assert shapes["required"] == ["A", "B", "C", "D", "E", "F", "G", "H", "X"], shapes
    assert shapes["additionalProperties"] is False, shapes
    for name in ["animation_motionsync_status", "animation_motionsync_cancel"]:
        assert tools[name]["inputSchema"]["properties"]["job"]["minimum"] == 1

    voice = run / "voice.wav"
    shutil.copyfile(speech, voice)
    fixture = run / "fixture.inx"
    fixture.write_bytes(inx(model(str(voice))))
    host.call("open", "open", {"path": str(fixture)})

    started = host.call("start", "animation_motionsync_start",
                        {"path": str(voice)})
    assert started["state"] == "running" and started["backend"] == "rhubarb", started
    assert started["recognizer"] == "phonetic", started
    job = started["job"]
    status = settle(host, job, "speech")
    (run / "speech-status.json").write_text(json.dumps(status, indent=2))
    assert status["state"] == "finished", status
    assert status["duration"] > 0
    assert any(cue["value"] != "X" for cue in status["cues"]), "no non-X cues from real speech"

    direct = analyze_directly(rhubarb, speech)
    assert [dict(c) for c in direct["mouthCues"]] == status["cues"], "session cues differ from direct run"

    before = host.call("snapshot", "snapshot", {})
    assert host.call("cancel-finished", "animation_motionsync_cancel", {"job": job}) == status
    assert host.call("after-cancel", "snapshot", {}) == before

    mappings = [
        {"parameter": 10, "axis": 0, "shapes": MOUTH_SHAPES},
        {"parameter": 11, "axis": 1, "shapes": JAW_SHAPES},
    ]
    baked = host.call("bake", "animation_motionsync_bake",
                      {"name": "Talk", "job": job, "audio_index": 0, "mappings": mappings})
    (run / "bake-result.json").write_text(json.dumps(baked, indent=2))
    end_frame = AUDIO_FRAME + math.ceil(status["duration"] / TIMESTEP)
    assert baked["from"] == AUDIO_FRAME and baked["to"] == end_frame, baked
    assert baked["keys"] == 2 * (end_frame - AUDIO_FRAME + 1), baked

    animation = host.call("after-bake", "snapshot", {})["model"]["animations"]["Talk"]
    for lane in animation["lanes"]:
        assert lane["interpolation"] == "Linear" and lane["merge_mode"] == "Forced", lane
    assert sorted((lane["uuid"], lane["target"]) for lane in animation["lanes"]) == [(10, 0), (11, 1)]
    read = subprocess.run(
        [str(host.njc), "--port", str(host.port), "resources", "read", "resource://vsoul/snapshot"],
        cwd=ROOT, capture_output=True, text=True, timeout=30)
    (run / "snapshot-resource.json").write_text(read.stdout)
    if read.returncode != 0:
        raise RuntimeError(f"njc resources read exited {read.returncode}: {read.stderr.strip()}")
    envelope = json.loads(read.stdout)
    assert "error" not in envelope and "result" in envelope, envelope
    contents = envelope["result"]["contents"]
    assert isinstance(contents, list) and contents and isinstance(contents[0].get("text"), str)
    observed = json.loads(contents[0]["text"])
    assert observed["model"]["animations"]["Talk"] == animation

    for frame in range(baked["from"], baked["to"] + 1):
        at = (frame - AUDIO_FRAME) * TIMESTEP
        shape = shape_at(direct["mouthCues"], at)
        sampled = host.call("sample", "animation_sample", {"name": "Talk", "frame": frame})
        close(sampled["values"]["10"][0], MOUTH_SHAPES[shape])
        close(sampled["values"]["11"][1], JAW_SHAPES[shape])

    host.call("undo", "undo", {})
    assert host.call("snapshot-undo", "snapshot", {})["model"] == before["model"]
    host.call("redo", "redo", {})
    assert (host.call("snapshot-redo", "snapshot", {})["model"]["animations"]["Talk"]
            == animation)
    saved = run / "baked.inx"
    host.call("save", "save", {"path": str(saved), "mark_saved": True})
    host.call("reopen", "open", {"path": str(saved)})
    reopened = host.call("snapshot-reopened", "snapshot", {})
    assert reopened["model"]["animations"]["Talk"] == animation

    with voice.open("ab") as mutated:
        mutated.write(b"\x00" * 8)
    still = host.call("snapshot-before-refusal", "snapshot", {})
    refusal = host.call("bake-refusal", "animation_motionsync_bake",
                        {"name": "Talk", "job": job, "audio_index": 0, "mappings": mappings},
                        expected_error=True)
    (run / "refusal.json").write_text(json.dumps(refusal, indent=2))
    assert host.call("snapshot-after-refusal", "snapshot", {}) == still

    voiced_frame = max(range(baked["from"], baked["to"] + 1),
                       key=lambda frame: MOUTH_SHAPES[shape_at(direct["mouthCues"],
                                                               (frame - AUDIO_FRAME) * TIMESTEP)])
    voiced_value = MOUTH_SHAPES[shape_at(direct["mouthCues"], (voiced_frame - AUDIO_FRAME) * TIMESTEP)]
    assert voiced_value > 0.0, "no voiced frame to render"
    host.call("seek-rest", "animation_seek", {"name": "Talk", "frame": 0})
    host.call("rest-image", "export_image", {"path": str(run / "mouth-rest.png")})
    host.call("seek-voiced", "animation_seek", {"name": "Talk", "frame": voiced_frame})
    host.call("voiced-image", "export_image", {"path": str(run / "mouth-voiced.png")})
    rest_png, voiced_png = run / "mouth-rest.png", run / "mouth-voiced.png"
    assert png_size(rest_png) == (128, 128), png_size(rest_png)
    assert png_size(voiced_png) == (128, 128), png_size(voiced_png)
    assert rest_png.read_bytes() != voiced_png.read_bytes(), \
        f"frame {voiced_frame} (Mouth={voiced_value}) renders identically to the rest frame"

    hush = run / "silence.wav"
    if silence is None:
        silence_wav(hush)
    else:
        shutil.copyfile(silence, hush)
    silent = host.call("start-silence", "animation_motionsync_start", {"path": str(hush)})
    quiet = settle(host, silent["job"], "silence")
    (run / "silence-status.json").write_text(json.dumps(quiet, indent=2))
    assert quiet["state"] == "finished", quiet
    assert quiet["cues"] and all(cue["value"] == "X" for cue in quiet["cues"]), quiet["cues"]

    return {"job": job, "cues": len(status["cues"]),
            "nonXCues": sum(1 for cue in status["cues"] if cue["value"] != "X"),
            "keys": baked["keys"], "frames": baked["frames"],
            "voicedFrame": voiced_frame, "voicedMouth": voiced_value,
            "silenceCues": len(quiet["cues"]), "silenceGenerated": silence is None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, default=ROOT / "out/rust/debug/soul2d-core")
    parser.add_argument("--njc", type=Path, default=ROOT / "out/rust/debug/njc")
    parser.add_argument("--rhubarb", type=Path, default=os.environ.get("SOUL2D_VERIFY_RHUBARB"),
                        help="the Rhubarb executable (or env SOUL2D_VERIFY_RHUBARB)")
    parser.add_argument("--wav", type=Path, default=os.environ.get("SOUL2D_VERIFY_WAV"),
                        help="a WAV with speech to analyze (or env SOUL2D_VERIFY_WAV)")
    parser.add_argument("--silence", type=Path, default=None,
                        help="optional silent WAV; a 2 s PCM16 silence is generated when absent")
    args = parser.parse_args()
    if args.rhubarb is None or args.wav is None:
        parser.error("--rhubarb and --wav are required (or SOUL2D_VERIFY_RHUBARB/SOUL2D_VERIFY_WAV)")
    core = args.core.resolve()
    njc = args.njc.resolve()
    rhubarb = args.rhubarb.resolve()
    for binary in (core, njc):
        assert binary.is_file(), f"build first: ./tools/build-rust-core.sh ({binary} missing)"
    assert rhubarb.is_file(), f"no Rhubarb executable at {rhubarb}"
    assert args.wav.is_file(), f"missing WAV {args.wav}"
    assert args.silence is None or args.silence.is_file(), f"missing WAV {args.silence}"
    EVIDENCE_ROOT.mkdir(parents=True, exist_ok=True)
    run = EVIDENCE_ROOT / f"motionsync-{time.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}"
    run.mkdir()
    print(run, flush=True)
    port = free_port()
    results = {"core": str(core), "njc": str(njc), "rhubarb": str(rhubarb), "port": port,
               "run": str(run)}
    host = None
    try:
        host = Host(run, core, njc, port, rhubarb)
        results["hostPid"] = host.child.pid
        host.wait_ready()
        silence = args.silence.resolve() if args.silence is not None else None
        results.update(verify(host, run, rhubarb, args.wav.resolve(), silence))
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
