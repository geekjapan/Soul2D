#!/usr/bin/env python3
"""Render bounded Composite fixtures through the Rust core and check the rendered pixels."""
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import struct
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
RUST_TARGET = ROOT / "out/rust/debug"
RUST_LIBRARY = RUST_TARGET / ("libsoul2d_core.dylib" if sys.platform == "darwin" else
                              "soul2d_core.dll" if os.name == "nt" else "libsoul2d_core.so")
sys.dont_write_bytecode = True
CORE_SPEC = importlib.util.spec_from_file_location("rust_fixture", ROOT / "tools/verify-rust-core.py")
CORE = importlib.util.module_from_spec(CORE_SPEC)
CORE_SPEC.loader.exec_module(CORE)


def transform(translation=(0, 0, 0)):
    return {"trans": list(translation), "rot": [0, 0, 0], "scale": [1, 1]}


def node(uuid, name, node_type, children=(), translation=(0, 0, 0)):
    return {"uuid": uuid, "name": name, "type": node_type, "enabled": True, "zsort": 0,
            "transform": transform(translation), "children": list(children)}


def part(template, *, verts=None, origin=(0, 0)):
    result = copy.deepcopy(template)
    result.update({"uuid": 2, "name": "Pixel Fixture", "type": "Part", "enabled": True,
                   "zsort": 0, "transform": transform(), "children": [], "lockToRoot": False,
                   "pinToMesh": False, "opacity": 1, "mask_threshold": 0})
    result["mesh"] = {"verts": verts or [2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25],
                      "uvs": [0, 0, 0, 1, 1, 0, 1, 1],
                      "indices": [0, 1, 2, 2, 1, 3], "origin": list(origin)}
    for key in ("vertices", "uvs", "indices"):
        result.pop(key, None)
    return result


def composite(uuid, name, children, translation=(0, 0, 0)):
    return {**node(uuid, name, "Composite", children, translation=translation), "blend_mode": "Normal",
            "tint": [1, 1, 1], "screenTint": [0, 0, 0], "mask_threshold": 0,
            "opacity": 1, "propagate_meshgroup": True}


def deform_binding(target, offsets):
    zeros = [[0, 0]] * 4
    return {"node": target, "param_name": "deform", "interpolate_mode": "Linear",
            "values": [[zeros], [offsets]], "isSet": [[True], [True]]}


def camera():
    return {**node(99, "ParityCamera", "Camera", translation=(8, 8, 0)), "viewport": [16, 16]}


def replace_first_texture(texture_tail, image):
    assert texture_tail[:8] == b"TEX_SECT"
    old_size = struct.unpack(">I", texture_tail[12:16])[0]
    old_end = 17 + old_size
    assert texture_tail[16] == 0
    return (texture_tail[:12] + struct.pack(">IB", len(image), 0) + image
            + texture_tail[old_end:])


def fixture_model(label, children, bindings):
    nodes = [*children, camera()]
    return {
        "meta": {"name": f"Composite pixel proof {label}", "version": "0.0.1"},
        "physics": {"pixelsPerMeter": 1000, "gravity": 9.8},
        "nodes": node(1, "Root", "Node", nodes),
        "param": ([{"uuid": 9, "name": "Composite Offset", "is_vec2": False,
                    "min": [0, 0], "max": [1, 0], "defaults": [1, 0],
                    "axis_points": [[0, 1], [0]], "bindings": bindings}] if bindings else []),
        "automation": [], "animations": {}, "groups": [],
    }


def fixtures(template, texture_tail):
    direct = part(template)
    stretched = composite(10, "Composite Stretch", [part(template)])
    nested_part = part(template)
    inner = composite(11, "Nested Composite", [nested_part])
    nested = composite(10, "Composite Stretch", [inner])
    origin_part = part(template, verts=[10.25, 10.25, 10.25, 14.25, 14.25, 10.25, 14.25, 14.25],
                       origin=(10, 10))
    origin_composite = composite(10, "Origin Composite", [origin_part])
    sibling_part = part(template)
    sibling_part["uuid"] = 3
    scaled_ancestor = composite(10, "Scaled Ancestor Composite", [
        composite(11, "Scaled Nested Composite", [part(template)])])
    scaled_ancestor["transform"]["scale"] = [2, 1]

    cases = {
        "baseline-direct": ([direct], []),
        "composite-stretch": ([stretched], [deform_binding(10, [[2, 0]] * 4)]),
        "composite-asymmetric-stretch": (
            [composite(10, "Asymmetric Stretch", [part(template)])],
            [deform_binding(10, [[0, 0], [0, 0], [2, 0], [2, 0]])]),
        "nested-composite": ([nested], [deform_binding(10, [[2, 0]] * 4),
                                        deform_binding(11, [[1, 0]] * 4)]),
        "composite-origin": ([origin_composite], [deform_binding(10, [[1, 0]] * 4)]),
        "composite-deformed-part": (
            [composite(10, "Deformed Part Composite", [part(template)])],
            [deform_binding(2, [[0, 0], [0, 0], [2, 0], [2, 0]]),
             deform_binding(10, [[1, 0]] * 4)]),
        "nested-composite-asymmetric": (
            [composite(10, "Outer Asymmetric Composite",
                       [composite(11, "Inner Asymmetric Composite", [part(template)])])],
            [deform_binding(10, [[2, 0]] * 4),
             deform_binding(11, [[-1, 0], [1, 1], [1, -1], [-1, 0]])]),
        "nested-composite-ancestor-scale": (
            [scaled_ancestor], [deform_binding(10, [[2, 0]] * 4),
                                deform_binding(11, [[-1, 0], [1, 1], [1, -1], [-1, 0]])]),
        "nested-composite-subpixel": (
            [composite(10, "Outer Subpixel Composite",
                       [composite(11, "Inner Subpixel Composite", [part(template)])])],
            [deform_binding(10, [[2, 0]] * 4), deform_binding(11, [[0.1, 0]] * 4)]),
        "nested-composite-siblings": (
            [composite(10, "Sibling Composite Parent", [
                composite(11, "Left Composite", [part(template)]),
                composite(12, "Right Composite", [sibling_part], translation=(7, 0, 0))])],
            [deform_binding(10, [[1, 0]] * 4), deform_binding(11, [[-0.6, 0]] * 4),
             deform_binding(12, [[0.6, 0]] * 4)]),
    }
    for suffix, scale, rotation, translation in (
            ("rotated-scale", [1.5, 0.75], 0.3, [3, 0, 0]),
            ("mirror", [-1, 1], 0, [10, 0, 0]),
            ("zero-scale", [0, 1], 0, [6, 0, 0])):
        ancestor = copy.deepcopy(scaled_ancestor)
        ancestor["transform"] = {"trans": translation, "rot": [0, 0, rotation], "scale": scale}
        cases[f"nested-composite-ancestor-{suffix}"] = (
            [ancestor], [deform_binding(10, [[2, 0]] * 4),
                         deform_binding(11, [[-1, 0], [1, 1], [1, -1], [-1, 0]])])
    for label, (children, bindings) in cases.items():
        yield label, fixture_model(label, children, bindings), texture_tail

    red_4x4 = CORE.png(4, 4, bytes([255, 0, 0, 255]) * 16)
    regression_tail = replace_first_texture(texture_tail, red_4x4)
    regression_part = part(template, verts=[2, 2, 2, 6, 6, 2, 6, 6])
    origin_regression = composite(
        10, "Origin Seeded Composite", [regression_part], translation=(2, 0, 0))
    negative_part = part(template, verts=[-6, 2, -6, 6, -2, 2, -2, 6])
    translated_regression = composite(10, "Translated Composite", [negative_part])
    nested_regression = composite(
        10,
        "Outer Composite",
        [composite(11, "Nested Composite", [part(template, verts=[-6, 2, -6, 6, -2, 2, -2, 6])])],
    )
    regressions = {
        "composite-origin-seeded-asymmetric": (
            [origin_regression],
            [deform_binding(10, [[0, 0], [0, 0], [2, 0], [2, 0]])],
        ),
        "composite-offscreen-translation": (
            [translated_regression], [deform_binding(10, [[8, 0]] * 4)]),
        "nested-composite-offscreen-translation": (
            [nested_regression],
            [deform_binding(10, [[4, 0]] * 4), deform_binding(11, [[4, 0]] * 4)],
        ),
    }
    for label, (children, bindings) in regressions.items():
        yield label, fixture_model(label, children, bindings), regression_tail


def write_fixture(path, model, texture_tail):
    encoded = json.dumps(model, separators=(",", ":")).encode()
    path.write_bytes(b"TRNSRTS\0" + struct.pack(">I", len(encoded)) + encoded + texture_tail)
    actual, actual_tail = CORE.split_container(path.read_bytes())
    assert actual == model and actual_tail == texture_tail


def render_rust(core, fixture_path, label, evidence):
    core.request("open", path=str(fixture_path))
    args = {"width": 16, "height": 16, "bounds": [0, 0, 16, 16]}
    if label != "baseline-direct":
        args["values"] = {"9": [1, 0]}
    rendered = core.request("render", **args)
    png_data = base64.b64decode(rendered["png"], validate=True)
    width, height, pixels = CORE.decode_rgba(png_data)
    assert (width, height) == (16, 16), (label, width, height)
    visible = [index // 4 for index in range(0, len(pixels), 4) if pixels[index + 3] > 0]
    red_pixels = sum(1 for index in range(0, len(pixels), 4)
                     if pixels[index + 3] > 0 and pixels[index] > pixels[index + 1]
                     and pixels[index] > pixels[index + 2])
    if label == "nested-composite-ancestor-zero-scale":
        assert not visible, "A zero-width source must remain transparent"
    else:
        assert visible and red_pixels, (label, len(visible), red_pixels)
    bounds = ([min(pixel % width for pixel in visible), min(pixel // width for pixel in visible),
               max(pixel % width for pixel in visible) + 1, max(pixel // width for pixel in visible) + 1]
              if visible else None)
    path = evidence / f"rust-{label}.png"
    path.write_bytes(png_data)
    return {"path": path.name, "width": width, "height": height, "pixels": pixels,
            "visiblePixels": len(visible), "redPixels": red_pixels, "alphaBounds": bounds}


def main():
    stamp = time.strftime("%Y%m%d-%H%M%S")
    evidence = ROOT / "build/verification" / f"composite-pixels-{stamp}-{os.getpid()}"
    evidence.mkdir(parents=True, exist_ok=False)
    result_path = evidence / "result.json"
    result = {"status": "IN_PROGRESS", "evidence": str(evidence), "surface": "Rust FFI",
              "rustLibrary": str(RUST_LIBRARY), "camera": {"name": "ParityCamera", "uuid": 99,
              "viewport": [16, 16], "translation": [8, 8, 0], "aabb": [0, 0, 16, 16]},
              "check": "visible red coverage only", "cases": {}}
    core = None
    try:
        assert RUST_LIBRARY.is_file(), f"Current Rust core library missing: {RUST_LIBRARY}"
        seed = evidence / "texture-seed.inx"
        model, _ = CORE.fixture(seed)
        template = model["nodes"]["children"][0]
        _, texture_tail = CORE.split_container(seed.read_bytes())
        core = CORE.Core(RUST_LIBRARY)
        results = {}
        for label, fixture_model, tail in fixtures(template, texture_tail):
            rust_model = copy.deepcopy(fixture_model)
            rust_model["nodes"]["children"] = [child for child in rust_model["nodes"]["children"]
                                                  if child["type"] != "Camera"]
            rust_fixture = evidence / f"rust-fixture-{label}.inx"
            write_fixture(rust_fixture, rust_model, tail)
            rendered = render_rust(core, rust_fixture, label, evidence)
            results[label] = rendered
            result["cases"][label] = {"rustFixture": rust_fixture.name,
                                      "rustPng": rendered["path"],
                                      "dimensions": [rendered["width"], rendered["height"]],
                                      "rustVisiblePixels": rendered["visiblePixels"],
                                      "rustRedPixels": rendered["redPixels"],
                                      "rustAlphaBounds": rendered["alphaBounds"],
                                      "status": "PASS"}
            result_path.write_text(json.dumps(result, indent=2))
        result["status"] = "PASS"
        result_path.write_text(json.dumps(result, indent=2))
        print(json.dumps({"status": result["status"], "evidence": str(evidence),
                          "cases": result["cases"]}, indent=2))
    except BaseException as exc:
        result.update({"status": "FAIL", "error": str(exc)})
        result_path.write_text(json.dumps(result, indent=2))
        raise
    finally:
        if core is not None:
            core.close()
    if result["status"] == "FAIL":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
