#!/usr/bin/env python3
"""Prove Kei vowel CAN3 projection, save/reopen and existing-key editing."""
import argparse
import copy
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import importlib.util


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


CAN = module("can3_node_source", ROOT / "tools/verify-can3-source.py")
CMO = CAN.CMO
EXPECTED = {"scenes": 8, "tracks": 8, "parameterCurves": 248, "anchors": 23713}
ROUTES = (("ffi", "FFI"), ("jsonl", "JSONL"), ("mcp", "MCP"))


def counts(model):
    scenes = model["scenes"]
    tracks = [track for scene in scenes for track in scene["tracks"]]
    curves = [curve for track in tracks for curve in track["parameterCurves"]]
    return {"scenes": len(scenes), "tracks": len(tracks), "parameterCurves": len(curves),
            "anchors": sum(len(curve["points"]) for curve in curves)}


def key_edit(core, source, original, source_sha, evidence):
    opened = core.request("open", path=str(source.resolve()))
    scene, track, curve = next((scene, track, curve) for scene in opened["model"]["scenes"]
        for track in scene["tracks"] for curve in track["parameterCurves"] if curve["points"])
    point = curve["points"][0]
    identity = {"sceneGuid": scene["guid"], "trackGuid": track["guid"], "curveId": curve["id"],
                "anchorFrame": point["anchor"]["frame"]}
    revision, old_value = opened["revision"], point["anchor"]["value"]
    request = {**identity, "value": max(p["anchor"]["value"] for p in curve["points"]) + 1.125,
               "expectedRevision": revision}

    assert core.request("live2d.animation.key.set", **{**request, "value": old_value}) == opened
    assert core.request("snapshot") == opened
    core.request("live2d.animation.key.set", success=False,
                 **{**request, "expectedRevision": revision + 1})
    assert core.request("snapshot") == opened

    edited = core.request("live2d.animation.key.set", **request)
    wanted = copy.deepcopy(opened["model"])
    selected = next(p for s in wanted["scenes"] if s["guid"] == scene["guid"]
        for t in s["tracks"] if t["guid"] == track["guid"]
        for c in t["parameterCurves"] if c["id"] == curve["id"]
        for p in c["points"] if p["anchor"]["frame"] == point["anchor"]["frame"])
    selected["anchor"]["value"] = request["value"]
    assert edited["model"] == wanted and edited["modified"] and edited["canUndo"]
    core.request("live2d.animation.key.set", success=False, **request)
    assert core.request("snapshot") == edited
    assert core.request("undo")["model"] == opened["model"]
    assert core.request("redo")["model"] == wanted

    saved = evidence / f"{source.stem}-key-edited.can3"
    core.request("save", path=str(saved.resolve()))
    changed_tokens = CAN.check_key_saved(original, saved.read_bytes(), request, request["value"])
    assert changed_tokens > 0
    reopened = core.request("open", path=str(saved.resolve()))
    assert reopened["model"] == wanted
    _, saved_xml = CMO.CAFF.decode(saved.read_bytes())
    CAN.check_xml(wanted, saved_xml)
    assert CMO.sha(source) == source_sha
    return {"identity": identity, "value": request["value"], "saved": str(saved),
            "savedSha256": CMO.sha(saved), "changedScalarTokens": changed_tokens,
            "noOpAndStaleRejectionsPreservedState": True, "undoRedo": True,
            "reopenedProjectionExact": True, "unknownCaffAndXmlPreserved": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", nargs=4, type=Path, help="the four extracted Kei vowel CAN3 files")
    parser.add_argument("--target", type=Path, default=ROOT / "out/rust/release")
    args = parser.parse_args()
    sources = [path.resolve() for path in args.sources]
    target = args.target.resolve()
    binary = target / "soul2d-core"
    library = target / ("libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    evidence = Path(tempfile.mkdtemp(prefix="can3-node-limit-", dir=ROOT / "build/verification"))
    result = {"status": "FAIL", "evidence": str(evidence),
        "scope": "Kei CAN3 source projection, byte-exact save/reopen and existing-anchor scalar edit",
        "expectedCountsPerSource": EXPECTED, "routes": [name for _, name in ROUTES],
        "nativeParserLimits": {"xmlNodes": 1500000, "xmlBytes": 64 * 1024 * 1024,
            "caffBytes": 256 * 1024 * 1024, "limitRejectionProof": "separate parser/corpus checks"},
        "sources": [], "workflows": []}
    try:
        manifest = ROOT / "build/verification/official-can3-compat-20261011/can3-sources-final/result.json"
        fail_hashes = {row["sha256"] for row in json.loads(manifest.read_text())["files"]
                       if row["status"] == "FAIL" and row.get("stage") == "native_read"}
        source_hashes = {CMO.sha(path) for path in sources}
        assert len(fail_hashes) == 4 and len(sources) == len(source_hashes) == 4
        assert source_hashes == fail_hashes and all(path.stem == CMO.sha(path) for path in sources)
        result.update(binarySha256=CMO.sha(binary), librarySha256=CMO.sha(library),
            animationSourceSha256=CMO.sha(ROOT / "rust/src/animation_source.rs"),
            verifierSha256=CMO.sha(Path(__file__)))
        for source in sources:
            original = source.read_bytes()
            source_sha = CMO.sha(source)
            _, xml = CMO.CAFF.decode(original)
            row = {"path": str(source), "sha256": source_sha, "bytes": len(original),
                   "xmlBytes": len(xml), "routes": {}}
            expected_projection = None
            for transport, name in ROUTES:
                core = CMO.CORE.Core(library) if transport == "ffi" else None
                saved = evidence / f"{source.stem}-{transport}-saved.can3"
                requests = [{"op": "live2d.animation.read", "path": str(source)},
                    {"op": "open", "path": str(source)}, {"op": "live2d.animation.read"},
                    {"op": "save", "path": str(saved)}, {"op": "open", "path": str(saved)},
                    {"op": "live2d.animation.read"}]
                try:
                    replies, wire = CAN.EDIT.run_transport(binary, core, requests, transport)
                finally:
                    if core:
                        core.close()
                log = evidence / f"{source.stem}-{transport}.log"
                log.write_text(wire)
                assert len(replies) == len(requests) and all(reply["succeeded"] for reply in replies), replies
                projected = replies[0]["result"]
                assert projected == replies[2]["result"] == replies[5]["result"]
                assert replies[1]["result"]["model"] == replies[4]["result"]["model"] == projected
                counts_seen = counts(projected)
                assert CAN.check_xml(projected, xml) == (EXPECTED["scenes"], EXPECTED["anchors"])
                assert counts_seen == EXPECTED, (source.name, counts_seen)
                assert saved.read_bytes() == original and source.read_bytes() == original
                if expected_projection is not None:
                    assert projected == expected_projection
                else:
                    expected_projection = projected
                row["routes"][name] = {"counts": counts_seen, "saved": str(saved),
                    "savedSha256": CMO.sha(saved), "byteExactSave": True,
                    "openReadProjectionExact": True, "wire": str(log)}
            core = CMO.CORE.Core(library)
            try:
                row["keyEdit"] = key_edit(core, source, original, source_sha, evidence)
            finally:
                core.close()
            result["sources"].append(row)
            result["workflows"].append({"sourceSha256": source_sha, "routes": list(row["routes"]),
                "keyEditRoute": "FFI"})
        assert all(CMO.sha(source) == next(row["sha256"] for row in result["sources"]
            if row["path"] == str(source)) for source in sources)
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        (evidence / "failure.txt").write_text(traceback.format_exc())
    (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "evidence": str(evidence),
        "sources": len(result["sources"]), "error": result.get("error")}, indent=2))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
