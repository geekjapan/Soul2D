#!/usr/bin/env python3
"""Deterministically generates the Soul2D depthdraw regression fixtures.

Two independent jobs share one deterministic output set:

1. Every retained ``depthdraw-*.json`` filename is a tombstone: a small
   fresh record that marks the file as a retired external reference and
   names a related synthetic local-contract fixture. Tombstones contain
   no original hashes, statistics, geometries, pixels, or reference
   source paths, and claim no coverage equivalence.

2. Five ``synthetic-depth-*.json`` local-contract fixtures are authored
   here from a uniform 4x4 depth-layer design. Generation reads no source
   data: every output byte derives from the constants in this file.

Usage:
    tools/generate-synthetic-depth-fixtures.py           # read-only check
    tools/generate-synthetic-depth-fixtures.py --check   # same as default
    tools/generate-synthetic-depth-fixtures.py --write   # write all files
"""

import hashlib
import json
import os
import shutil
import sys

REPO_ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
FIXTURE_DIR = "rust/tests/fixtures/depthdraw"
ORIGINALS_ROOT = os.path.join(
    REPO_ROOT, "build", "verification", "content-replacement-20261011", "originals"
)

TOMBSTONE_REASON = (
    "External model/tool output is excluded pending rights clearance; "
    "this record contains no reference output."
)

# Explicit list of the 42 retired external reference records. Tombstones
# keep the historical filenames; ``replacement`` names a related synthetic
# local-contract fixture without claiming equivalent coverage.
TOMBSTONES = {
    # Tool-oracle contract records.
    "depthdraw-alpha-depth-gap-fill-contract.json": "synthetic-depth-transparent.json",
    "depthdraw-alpha-mask-contract.json": "synthetic-depth-transparent.json",
    "depthdraw-api-capabilities.json": "synthetic-depth-frontmost.json",
    "depthdraw-depth-cleanup-contract.json": "synthetic-depth-transparent.json",
    "depthdraw-depth-conversion-contract.json": "synthetic-depth-z-transform.json",
    "depthdraw-depth-prune-contract.json": "synthetic-depth-frontmost.json",
    "depthdraw-depth-split-contract.json": "synthetic-depth-frontmost.json",
    "depthdraw-global-depth-scale-contract.json": "synthetic-depth-z-transform.json",
    "depthdraw-grid-convolution-contract.json": "synthetic-depth-z-transform.json",
    "depthdraw-layer-bounds-sampling-contract.json": "synthetic-depth-black.json",
    "depthdraw-psd-mask-export-contract.json": "synthetic-depth-transparent.json",
    # depth-draw PSD 1:N composition records (external model input).
    "depthdraw-psd-n1-midori-artifacts.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-n1-midori-composite.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-n1-midori-layer-0-depth-raw.json": "synthetic-depth-black.json",
    "depthdraw-psd-n1-midori-layer-0-depth.json": "synthetic-depth-black.json",
    "depthdraw-psd-n1-midori-layer-0-mesh-summary.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-n1-midori-layer-1-depth-raw.json": "synthetic-depth-white.json",
    "depthdraw-psd-n1-midori-layer-1-depth.json": "synthetic-depth-white.json",
    "depthdraw-psd-n1-midori-layer-1-mesh-summary.json": "synthetic-depth-frontmost.json",
    # depth-draw PSD N:N composition records (external model input).
    "depthdraw-psd-nn-midori-artifacts.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-nn-midori-composite.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-nn-midori-layer-0-depth-raw.json": "synthetic-depth-black.json",
    "depthdraw-psd-nn-midori-layer-0-depth.json": "synthetic-depth-black.json",
    "depthdraw-psd-nn-midori-layer-0-mesh-summary.json": "synthetic-depth-frontmost.json",
    "depthdraw-psd-nn-midori-layer-1-depth-raw.json": "synthetic-depth-white.json",
    "depthdraw-psd-nn-midori-layer-1-depth.json": "synthetic-depth-white.json",
    "depthdraw-psd-nn-midori-layer-1-mesh-summary.json": "synthetic-depth-frontmost.json",
    # depth-draw PSD transform records (external model input).
    "depthdraw-psd-transform-midori-artifacts.json": "synthetic-depth-z-transform.json",
    "depthdraw-psd-transform-midori-composite.json": "synthetic-depth-z-transform.json",
    "depthdraw-psd-transform-midori-layer-0-depth-raw.json": "synthetic-depth-z-transform.json",
    "depthdraw-psd-transform-midori-layer-0-depth.json": "synthetic-depth-z-transform.json",
    "depthdraw-psd-transform-midori-layer-0-mesh-summary.json": "synthetic-depth-z-transform.json",
    "depthdraw-psd-transform-midori-set-layer-0.json": "synthetic-depth-z-transform.json",
    # depth-draw raster 1x1 records (external model output).
    "depthdraw-raster-1x1-artifacts.json": "synthetic-depth-white.json",
    "depthdraw-raster-1x1-composite.json": "synthetic-depth-white.json",
    "depthdraw-raster-1x1-layer-0-depth-raw.json": "synthetic-depth-white.json",
    "depthdraw-raster-1x1-layer-0-depth.json": "synthetic-depth-white.json",
    "depthdraw-raster-1x1-load.json": "synthetic-depth-white.json",
    # depth-draw raster transform records (external model output).
    "depthdraw-raster-transform-artifacts.json": "synthetic-depth-z-transform.json",
    "depthdraw-raster-transform-composite.json": "synthetic-depth-z-transform.json",
    "depthdraw-raster-transform-layer-0-depth-raw.json": "synthetic-depth-z-transform.json",
    "depthdraw-raster-transform-layer-0-depth.json": "synthetic-depth-z-transform.json",
}

TOLERANCE = 0.00001
DOC_W = DOC_H = 4
GRID = {"axisX": [-1, 1], "axisY": [-1, 1], "baseDepths": [0.25, 0.25, 0.25, 0.25]}


def layer(layer_id, rgba, zscale=None, zoffset=None):
    entry = {
        "id": layer_id,
        "width": DOC_W,
        "height": DOC_H,
        "bounds": [0, 0, DOC_W, DOC_H],
        "backDepth": -1.0,
        "frontDepth": 1.0,
        "pixels": list(rgba) * 16,
    }
    if zscale is not None:
        entry["zScale"] = zscale
    if zoffset is not None:
        entry["zOffset"] = zoffset
    return entry


def binding(layer_id, order, policy):
    return {"layerId": layer_id, "order": order, "mergePolicy": policy}


def synthetic_fixtures():
    def doc(fid, layers, bindings, depths, winners):
        return {
            "id": fid,
            "generatedBy": "local-contract",
            "tolerance": TOLERANCE,
            "document": {"width": DOC_W, "height": DOC_H},
            "grid": dict(GRID),
            "layers": layers,
            "bindings": bindings,
            "expectedDepths": depths,
            "expectedWinners": winners,
        }

    fixtures = {}

    # Fully transparent panel: alpha zero means a missing depth sample, so
    # the grid keeps its base depths and records no winning layer.
    fixtures["synthetic-depth-transparent.json"] = doc(
        "synthetic-depth-transparent",
        [layer("panel", [255, 255, 255, 0])],
        [binding("panel", 0, "Replace")],
        [0.25, 0.25, 0.25, 0.25],
        ["", "", "", ""],
    )

    # Black panel: gray 0 maps to backDepth -1 under Replace.
    fixtures["synthetic-depth-black.json"] = doc(
        "synthetic-depth-black",
        [layer("panel", [0, 0, 0, 255])],
        [binding("panel", 0, "Replace")],
        [-1.0, -1.0, -1.0, -1.0],
        ["panel", "panel", "panel", "panel"],
    )

    # White panel: gray 255 maps to frontDepth 1 under Replace.
    fixtures["synthetic-depth-white.json"] = doc(
        "synthetic-depth-white",
        [layer("panel", [255, 255, 255, 255])],
        [binding("panel", 0, "Replace")],
        [1.0, 1.0, 1.0, 1.0],
        ["panel", "panel", "panel", "panel"],
    )

    # Gray 51: raw depth = -1 + 2 * 51 / 255 = -0.6, then the layer z
    # transform scales it by 2 and offsets by 0.25 to reach -0.95.
    fixtures["synthetic-depth-z-transform.json"] = doc(
        "synthetic-depth-z-transform",
        [layer("panel", [51, 51, 51, 255], zscale=2.0, zoffset=0.25)],
        [binding("panel", 0, "Replace")],
        [-0.95, -0.95, -0.95, -0.95],
        ["panel", "panel", "panel", "panel"],
    )

    # Low/high panels under Frontmost: the later order wins everywhere.
    fixtures["synthetic-depth-frontmost.json"] = doc(
        "synthetic-depth-frontmost",
        [layer("low", [0, 0, 0, 255]), layer("high", [255, 255, 255, 255])],
        [binding("low", 0, "Frontmost"), binding("high", 1, "Frontmost")],
        [1.0, 1.0, 1.0, 1.0],
        ["high", "high", "high", "high"],
    )

    return fixtures


FIXTURE_README = """\
# DepthDraw regression fixtures

Three kinds of files live in this directory:

* `local-*.json` (`local-frontmost-smoke.json`,
  `local-merge-policies-smoke.json`): pre-existing project local-contract
  fixtures. They are not produced by the generator and are unchanged.
* `synthetic-depth-*.json`: five independently authored local-contract
  fixtures generated by `tools/generate-synthetic-depth-fixtures.py`.
* `depthdraw-*.json`: retired external reference records. Each file is a
  tombstone (`status: retired-external-reference`) whose `replacement`
  field names a related synthetic fixture. The filenames are retained
  for compatibility and history only; no external model or tool output,
  hashes, statistics, geometries, or pixels remain in them.

## Coverage scope

The retired records exercised broad external oracle, mask, mesh-summary,
convolution, and multi-layer composition comparisons against third-party
tool output. That comparison coverage is retired pending rights
clearance. The five synthetic fixtures are a bounded replacement corpus:
they do not reproduce or equal the retired coverage and must not be
treated as vendor-equivalence evidence.

## Synthetic fixture design

Every synthetic case uses a 4x4 document and a 2x2 target grid with
`axisX`/`axisY` of `[-1, 1]` and base depths `[0.25] * 4`. Layers are
uniform 4x4 RGBA fills with explicit `backDepth: -1` and
`frontDepth: 1`.

* `synthetic-depth-transparent`: alpha-0 pixels count as missing depth
  samples, so base depths are kept and no layer wins.
* `synthetic-depth-black`: gray 0 maps to `-1` under Replace.
* `synthetic-depth-white`: gray 255 maps to `1` under Replace.
* `synthetic-depth-z-transform`: gray 51 gives raw depth
  `-1 + 2 * 51 / 255 = -0.6`; layer `zScale = 2` and `zOffset = 0.25`
  produce `-0.95`.
* `synthetic-depth-frontmost`: black (`low`, order 0) and white (`high`,
  order 1) panels under Frontmost, so `high` wins everywhere.

## Regeneration

    python3 tools/generate-synthetic-depth-fixtures.py          # check on-disk bytes
    python3 tools/generate-synthetic-depth-fixtures.py --write  # write all files
"""


def outputs():
    """repo-relative path -> content bytes. Reads no existing files."""
    result = {}
    for name in sorted(TOMBSTONES):
        tombstone = {
            "status": "retired-external-reference",
            "replacement": TOMBSTONES[name],
            "reason": TOMBSTONE_REASON,
        }
        result[FIXTURE_DIR + "/" + name] = (
            json.dumps(tombstone, indent=2) + "\n"
        ).encode("utf-8")
    for name, fixture in sorted(synthetic_fixtures().items()):
        result[FIXTURE_DIR + "/" + name] = (
            json.dumps(fixture, indent=2) + "\n"
        ).encode("utf-8")
    result[FIXTURE_DIR + "/README.md"] = FIXTURE_README.encode("utf-8")
    return result


def _fail(msg):
    sys.stderr.write("error: %s\n" % msg)
    sys.exit(2)


def _assert_safe_parents(root, rel):
    base = os.path.abspath(root)
    if os.path.islink(base):
        _fail("refusing to write through symlinked root: " + root)
    target = os.path.abspath(os.path.join(base, rel))
    if os.path.commonpath([base, target]) != base:
        _fail("unsafe path outside root: " + rel)
    parts = os.path.normpath(rel).split(os.sep)
    d = base
    for part in parts[:-1]:
        d = os.path.join(d, part)
        if os.path.islink(d):
            _fail("refusing to write through symlinked directory: " + d)


def write_outputs(root, items, backup_root=ORIGINALS_ROOT):
    for rel in sorted(items):
        target = os.path.join(root, rel)
        backup = os.path.join(backup_root, rel)
        _assert_safe_parents(root, rel)
        _assert_safe_parents(backup_root, rel)
        if os.path.islink(target):
            _fail("refusing to write through symlink: " + rel)
        if os.path.islink(backup):
            _fail("refusing to touch symlinked backup: " + rel)
        if os.path.exists(target) and not os.path.exists(backup):
            os.makedirs(os.path.dirname(backup), exist_ok=True)
            shutil.copyfile(target, backup)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as f:
            f.write(items[rel])
        print("wrote %s (%d bytes)" % (rel, len(items[rel])))


def check_outputs(root, items):
    errors = []
    for rel in sorted(items):
        target = os.path.join(root, rel)
        if not os.path.exists(target):
            errors.append("missing: " + rel)
            continue
        if os.path.islink(target):
            errors.append("unsafe symlink: " + rel)
            continue
        with open(target, "rb") as f:
            actual = f.read()
        if actual != items[rel]:
            errors.append("mismatch: %s (sha256 %s expected %s)" % (
                rel,
                hashlib.sha256(actual).hexdigest()[:16],
                hashlib.sha256(items[rel]).hexdigest()[:16]))
    return errors


def run_check(root):
    items = outputs()
    errors = check_outputs(root, items)
    if errors:
        for err in errors:
            sys.stderr.write("check: %s\n" % err)
        return 1
    print("check: all %d depthdraw fixture records match expected bytes" % len(items))
    return 0


def main(argv):
    mode = "check"
    for arg in argv[1:]:
        if arg == "--check":
            mode = "check"
        elif arg == "--write":
            mode = "write"
        else:
            _fail("unknown argument: " + arg)
    if mode == "write":
        write_outputs(REPO_ROOT, outputs())
        return 0
    return run_check(REPO_ROOT)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
