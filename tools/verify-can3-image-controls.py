#!/usr/bin/env python3
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("image_control_curves", ROOT / "tools/verify-can3-curves.py")
CURVES = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CURVES)
KEY = CURVES.KEYPROOF


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    assert __debug__
    parser = argparse.ArgumentParser(description="Verify owned Image scalar controls over HTTP MCP; no GUI or vendor acceptance.")
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--njc", type=Path, required=True)
    args = parser.parse_args()
    evidence = ROOT / "build/verification/can3-image-controls"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    fixture = ROOT / "rust/tests/fixtures/can3-image-track.xml"
    xml = fixture.read_text()
    marker = '<CMvEffect_VisualDefault xs.id="image-visual">'
    assert xml.count(marker) == 1
    xml = xml.replace(marker, marker + '<CMvAttrF xs.n="attrOpacity" xs.ref="image-opacity"/>')
    xml = xml.replace('<array xs.n="effectList" count="1">', '<array xs.n="effectList" count="1" type="ICMvEffect">')
    start, end = CURVES.element_span(xml, '<CMvAttrF xs.id="image-opacity">')
    attribute = xml[start:end]
    marker = '<CMutableSequence xs.n="valueData">'
    assert attribute.count(marker) == 1
    attribute = attribute.replace(marker, marker + '<ACValueSequence xs.n="super"><d xs.n="curMin">0</d>'
                                  '<d xs.n="curMax">0</d><i xs.n="keyMin">0</i><i xs.n="keyMax">0</i>'
                                  '<d xs.n="baseValue">1</d></ACValueSequence>')
    xml = xml[:start] + attribute + xml[end:]
    source = run / "source.can3"
    original = KEY.caff_pack(xml.encode())
    source.write_bytes(original)
    inputs = {str(path.resolve()): sha(path.resolve()) for path in (args.core, args.njc, fixture, Path(__file__))}
    sources = set((ROOT / "rust/src").rglob("*.rs")) | {ROOT / "rust/Cargo.toml", ROOT / "rust/Cargo.lock"}
    sources.update(Path(module.__file__) for module in (CURVES, KEY, KEY.DRIVER, KEY.CAFF))
    source_hashes = {str(path.relative_to(ROOT)): sha(path) for path in sources}
    result = {"status": "FAIL", "scope": "Owned Image scalar stored controls over HTTP MCP; GUI/playback/vendor NOT_RUN",
              "sourceRevision": os.environ.get("GITHUB_SHA"), "sourceHashes": source_hashes,
              "inputHashes": inputs, "sourceSha256": sha(source)}
    host = None
    try:
        host = CURVES.DRIVER.Host(run, args.core.resolve(), args.njc.resolve(), CURVES.DRIVER.free_port())
        host.wait_ready()
        host.call("open", "open", {"path": str(source)})
        before, baseline = KEY.state(host, "baseline")
        expected = copy.deepcopy(baseline)
        curve = expected["scenes"][0]["imageTracks"][0]["visualCurves"][0]
        assert curve["id"] == "opacity" and curve["points"] == [] and curve["curveTypes"] == []
        selectors = dict(sceneGuid="scene-guid", trackGuid="image-track-guid", curveId="opacity", anchorFrame=0)
        curve["points"] = [{"anchor": {"frame": 0, "value": 10.0}, "corner": True,
                            "previous": {"frame": 0.0, "integerFrame": 0, "value": 10.0, "positionOptimized": False},
                            "next": {"frame": 0.0, "integerFrame": 0, "value": 10.0, "positionOptimized": False}}]
        curve["curveTypes"] = ["LINEAR"]
        assert KEY.edit(host, "add", "live2d_animation_key_add", **selectors, value=10)[1] == expected
        curve["points"][0]["next"].update(frame=2.5, value=13.5)
        assert KEY.edit(host, "handle", "live2d_animation_handle_set", **selectors, side="next", frame=2.5, value=13.5)[1] == expected
        curve["curveTypes"] = ["STEP"]
        assert KEY.edit(host, "type", "live2d_animation_curve_type_set", **selectors, type="STEP")[1] == expected
        snapshot, _ = KEY.state(host, "before-noop")
        history = host.call("history-before-noop", "history_summary", {})
        host.call("noop", "live2d_animation_handle_set", {**selectors, "side": "next", "value": 13.5,
                                                          "expectedRevision": snapshot["revision"]})
        assert KEY.state(host, "after-noop")[0] == snapshot
        assert host.call("history-after-noop", "history_summary", {}) == history
        KEY.refuse(host, "stale", "live2d_animation_curve_type_set",
                   {**selectors, "type": "LINEAR", "expectedRevision": before["revision"]}, "revision conflict")
        KEY.refuse(host, "part-selector", "live2d_animation_handle_set",
                   {**selectors, "side": "next", "value": 14, "partOpacityMapKey": "wrong"}, "visual keys do not take partOpacityMapKey")
        KEY.refuse(host, "float-overflow", "live2d_animation_handle_set",
                   {**selectors, "side": "next", "frame": 1e300}, "finite 32-bit storage")
        for index in range(3):
            host.call(f"undo-{index}", "undo", {})
        assert KEY.state(host, "undone")[1] == baseline
        undone = run / "undone.can3"
        host.call("save-undone", "save", {"path": str(undone)})
        assert undone.read_bytes() == original
        for index in range(3):
            host.call(f"redo-{index}", "redo", {})
        saved = run / "saved.can3"
        CURVES.save_and_reopen(host, "controls", saved, expected, original)
        assert source.read_bytes() == original
        result.update(status="PASS", savedSha256=sha(saved), operations=3, refusals=3, noop=True, undoBytesEqual=True)
    except BaseException:
        result["error"] = traceback.format_exc()
        raise
    finally:
        try:
            if host:
                host.close()
                result["hostExited"] = host.child.poll() is not None
                assert result["hostExited"]
            for path, digest in inputs.items():
                assert sha(Path(path)) == digest
            for path, digest in source_hashes.items():
                assert sha(ROOT / path) == digest
            assert source.read_bytes() == original
        except BaseException:
            result.update(status="FAIL", cleanupError=traceback.format_exc())
            raise
        finally:
            (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(run)


if __name__ == "__main__":
    main()
