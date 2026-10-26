import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import traceback


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("physics_proof", ROOT / "tools/verify-animation-physics-bake.py")
PHYSICS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PHYSICS)
EXPECTED = {
    "Nearest": 1.0,
    "Stepped": 2.0,
    "Linear": 1.375,
    "Cubic": 1.287109375,
    "Bezier": 1.345703125,
}


def close(actual, expected, tolerance=1e-6):
    assert abs(actual - expected) <= tolerance, (actual, expected)


def model(mode):
    document = PHYSICS.model()
    body = document["nodes"]["children"][0]
    body["children"] = []
    body["mesh"]["verts"] = [-12.0, -12.0, -12.0, 12.0, 12.0, -12.0, 12.0, 12.0]
    camera = {
        "uuid": 9, "name": "Cam", "type": "Camera", "enabled": True, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": False, "pinToMesh": False, "viewport": [128.0, 128.0], "children": [],
    }
    document["nodes"]["children"] = [camera, body]
    document["param"] = document["param"][:1]
    document["param"][0]["max"] = [4.0, 0.0]
    document["animations"] = {
        "Loop": {
            "length": 8, "timestep": 0.125, "additive": False, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1,
            "lanes": [{
                "uuid": 10, "target": 0, "interpolation": mode, "merge_mode": "Forced",
                "keyframes": [
                    {"frame": 1, "value": 1.0, "tension": 0.25, "sourceNote": "canonical"},
                    {"frame": 3, "value": 3.0, "tension": 0.5},
                    {"frame": 5, "value": 0.0, "tension": 0.5},
                    {"frame": 7, "value": 2.0, "tension": 0.5},
                ],
            }],
        },
    }
    return document


def sample(host, label, frame, expected):
    result = host.call(label, "animation_sample", {"name": "Loop", "frame": frame})
    close(result["values"]["10"][0], expected)


def resource(host, run, label):
    command = [
        str(host.njc), "--port", str(host.port), "resources", "read",
        "resource://vsoul/snapshot",
    ]
    response = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
    (run / f"{label}-resource.json").write_text(response.stdout)
    (run / f"{label}-resource.stderr").write_text(response.stderr)
    with (run / "commands.jsonl").open("a") as wire:
        wire.write(json.dumps({"label": label, "command": command[2:], "exit": response.returncode}) + "\n")
    response.check_returncode()
    envelope = json.loads(response.stdout)
    assert "error" not in envelope, envelope
    return json.loads(envelope["result"]["contents"][0]["text"])


def png_size(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR", path
    return struct.unpack(">II", data[16:24])


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def verify(host, run):
    tools = host.tools()
    setting = next(tool for tool in tools if tool["name"] == "animation_set")
    assert setting["inputSchema"]["properties"]["loopEditing"]["type"] == "boolean", setting
    for mode, expected in EXPECTED.items():
        fixture = run / f"{mode}.inx"
        fixture.write_bytes(PHYSICS.inx(model(mode)))
        base = host.call(f"{mode}-open", "open", {"path": str(fixture)})
        sample(host, f"{mode}-default", 0.25, 1.0)
        host.call(f"{mode}-default-false", "animation_set", {"name": "Loop", "loopEditing": False})
        unchanged = host.call(f"{mode}-after-default-false", "snapshot", {})
        for key in ["model", "revision", "history"]:
            assert unchanged[key] == base[key], (mode, key, unchanged[key], base[key])
        if mode == "Linear":
            host.call("before-loop-seek", "animation_seek", {"name": "Loop", "frame": 0.25})
            host.call("before-loop-image", "export_image", {"path": str(run / "before-loop.png")})
        host.call(f"{mode}-enable", "animation_set", {"name": "Loop", "loopEditing": True})
        enabled = host.call(f"{mode}-enabled-snapshot", "snapshot", {})
        expected_model = copy.deepcopy(base["model"])
        expected_model["animations"]["Loop"]["loopEditing"] = True
        assert enabled["model"] == expected_model, mode
        observed = resource(host, run, mode)
        assert observed["model"] == enabled["model"], mode
        sample(host, f"{mode}-periodic", 0.25, expected)
        for frame, value in [(1, 1.0), (3, 3.0), (5, 0.0), (7, 2.0)]:
            sample(host, f"{mode}-authored-{frame}", frame, value)
        seam = 2.0 if mode in ["Nearest", "Stepped"] else 1.5
        sample(host, f"{mode}-seam-zero", 0.0, seam)
        sample(host, f"{mode}-seam-end", 8.0, seam)
        if mode == "Linear":
            host.call("after-loop-seek", "animation_seek", {"name": "Loop", "frame": 0.25})
            host.call("after-loop-image", "export_image", {"path": str(run / "after-loop.png")})
            for image in ["before-loop.png", "after-loop.png"]:
                assert png_size(run / image) == (128, 128), image
            assert (run / "before-loop.png").read_bytes() != (run / "after-loop.png").read_bytes()
            host.call("edit-source-key", "animation_key_set", {
                "name": "Loop", "parameter": 10, "axis": 0, "frame": 1, "value": 4.0, "tension": 0.25,
            })
            sample(host, "source-key-edit-result", 0.25, 3.25)
            host.call("undo-source-key", "undo", {})
            sample(host, "source-key-edit-undone", 0.25, 1.375)
        before_error = host.call(f"{mode}-before-error", "snapshot", {})
        for field, value in [("leadOut", 6), ("loopEditing", "yes"), ("loopEditing", None)]:
            host.call(f"{mode}-reject-{field}-{value}", "animation_set",
                      {"name": "Loop", field: value}, expected_error=True)
            assert host.call(f"{mode}-after-error", "snapshot", {}) == before_error, (mode, field)
        host.call(f"{mode}-undo-enable", "undo", {})
        assert host.call(f"{mode}-undone", "snapshot", {})["model"] == base["model"], mode
        sample(host, f"{mode}-nonloop-restored", 0.25, 1.0)
        host.call(f"{mode}-redo-enable", "redo", {})
        restored = host.call(f"{mode}-redone", "snapshot", {})
        assert restored["model"] == enabled["model"], mode
        saved = run / f"{mode}-roundtrip.inx"
        host.call(f"{mode}-save", "save", {"path": str(saved), "mark_saved": True})
        assert saved.is_file() and saved.stat().st_size > 0, saved
        host.call(f"{mode}-reopen", "open", {"path": str(saved)})
        assert resource(host, run, f"{mode}-reopened")["model"] == enabled["model"], mode
        sample(host, f"{mode}-persisted", 0.25, expected)
        host.call(f"{mode}-disable-custom", "animation_set",
                  {"name": "Loop", "loopEditing": False, "leadOut": 6})
        off = host.call(f"{mode}-disabled", "snapshot", {})
        assert "loopEditing" not in off["model"]["animations"]["Loop"]
        assert off["model"]["animations"]["Loop"]["leadOut"] == 6
        sample(host, f"{mode}-disabled-default-sample", 0.25, 1.0)
    clock = run / "clock.inx"
    clock.write_bytes(PHYSICS.inx(PHYSICS.model()))
    host.call("clock-open", "open", {"path": str(clock)})
    host.call("clock-seek", "animation_seek", {"name": "Run", "frame": 10})
    for index in range(20):
        advanced = host.call(f"clock-step-{index}", "animation_advance", {"dt": 0.001, "loop": True})
    close(advanced["animation"]["frame"], 10.32, 1e-4)
    host.call("clock-before-boundary", "animation_seek", {"name": "Run", "frame": 10.49})
    advanced = host.call("clock-not-rounded", "animation_advance", {"dt": 0.002, "loop": True})
    close(advanced["animation"]["frame"], 10.522, 1e-4)
    host.call("clock-at-end", "animation_seek", {"name": "Run", "frame": 10.99})
    wrapped = host.call("clock-wrap", "animation_advance", {"dt": 0.001, "loop": True})
    close(wrapped["animation"]["frame"], 0.0)
    host.call("clock-nonloop-seek", "animation_seek", {"name": "Run", "frame": 10})
    stopped = host.call("clock-nonloop", "animation_advance", {"dt": 0.001, "loop": False})
    close(stopped["animation"]["frame"], 10.0)
    return {"modes": list(EXPECTED), "canonicalFrames": [1, 3, 5, 7],
            "rendered": ["before-loop.png", "after-loop.png"], "clockRegression": "passed",
            "nativeGui": "NOT_RUN", "vendorAcceptance": "NOT_RUN"}


def main():
    parser = argparse.ArgumentParser(description="Verify INX loop editing through the production MCP host and njc.")
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--njc", type=Path, required=True)
    args = parser.parse_args()
    core, njc = args.core.resolve(), args.njc.resolve()
    assert core.is_file() and njc.is_file(), (core, njc)
    evidence = ROOT / "build/verification/ani229"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    sources = [
        "rust/src/animation.rs", "rust/src/lib.rs", "rust/src/mcp.rs", "rust/src/editor_ui.rs",
        "doc/commands.manifest.json", "tools/verify-animation-loop-edit.py",
        "tools/verify-animation-physics-bake.py",
    ]
    result = {
        "run": str(run), "core": str(core), "njc": str(njc), "passed": False,
        "scope": "INX parameter loop editing over MCP. Native GUI and vendor acceptance are not exercised.",
        "sha256": {"core": sha256(core), "njc": sha256(njc),
                   "sources": {name: sha256(ROOT / name) for name in sources}},
    }
    host = None
    try:
        host = PHYSICS.Host(run, core, njc, PHYSICS.free_port())
        result["hostPid"] = host.child.pid
        host.wait_ready()
        result.update(verify(host, run))
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
            result["passed"] &= result["hostExited"]
        (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"passed": result["passed"], "run": str(run)}), flush=True)
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
