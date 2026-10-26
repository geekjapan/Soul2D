#!/usr/bin/env python3
"""Check the PSD, KRA and JPEG importers against real files with independent decoders.

Usage: tools/verify-real-files.py [options] DIR_OR_FILE...

Directories are searched recursively for *.psd, *.kra and *.jpg/*.jpeg. The files are read in place and never copied.
Everything is compared through the JSONL protocol of out/rust/debug/soul2d-core.

PSD: the layer records are parsed here (count, names, bounds, blend keys, visibility, group nesting) and compared
with the imported tree; a few layer bitmaps are decoded here and compared byte for byte with the stored textures;
the composite render is compared with the PSD's merged image.
KRA: the ZIP container, maindoc.xml (canvas, colour space, layer tree) and the LZF tile files of the paint layers are
read here (zipfile and a small LZF decoder); the tree is compared with the imported one, layers small enough to decode
are compared with their stored textures, and the composite render is compared with Krita's own mergedimage.png. A
refusal must name the limit or feature, and this script checks that the named limit is really exceeded.
JPEG: a marker scan classifies each stream, a stratified sample is imported through merge.images, and the stored
texture is compared with a reference decode by macOS `sips` (skipped when sips is missing).

Exit status: 0 when every threshold holds, 1 on a regression, 2 on a usage or environment error, 3 when nothing
regressed but a KRA comparison was not comparable (shown content the importer does not read).
Standard library only; run with `python3 -I`.
"""
import argparse
import base64
import json
import os
import queue
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
import xml.etree.ElementTree as ET
import zipfile
import zlib
from itertools import accumulate

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# The importer's texture limits (model_import.rs MAX_TEXTURE_SIDE, render.rs MAX_TEXTURE_PIXELS).
TEXTURE_SIDE_LIMIT, TEXTURE_PIXEL_LIMIT = 8192, 16 * 1024 * 1024

# ---------------------------------------------------------------------------------------------------------------
# JSONL client
# ---------------------------------------------------------------------------------------------------------------


class Core:
    """One soul2d-core process speaking one JSON request per line."""

    def __init__(self, binary, timeout):
        self.binary, self.timeout = binary, timeout
        self.proc = None
        self.lines = None
        self.start()

    def start(self):
        self.proc = subprocess.Popen([self.binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
        self.lines = queue.Queue()
        threading.Thread(target=self.read_lines, args=(self.proc, self.lines), daemon=True).start()

    @staticmethod
    def read_lines(proc, lines):
        for line in proc.stdout:
            lines.put(line)
        lines.put(None)  # EOF sentinel, always: a crash after some replies must not leave call() waiting

    def call(self, request):
        """Returns (result, error). A timeout or crash restarts the process and reports an error."""
        try:
            self.proc.stdin.write(json.dumps(request) + "\n")
            self.proc.stdin.flush()
            line = self.lines.get(timeout=self.timeout)
        except (queue.Empty, BrokenPipeError):
            line = None
        if line is None:
            self.proc.kill()
            self.proc.wait()
            self.start()
            return None, "core did not answer (timeout or crash)"
        reply = json.loads(line)
        if reply.get("succeeded") is False or "result" not in reply:
            return None, str(reply.get("message") or reply.get("error") or reply)
        return reply["result"], None

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except Exception:  # best effort during shutdown
            self.proc.kill()


# ---------------------------------------------------------------------------------------------------------------
# PNG
# ---------------------------------------------------------------------------------------------------------------


def _add_bytes(a, b):
    """Per-byte (a + b) mod 256 of two equally long byte strings, without a Python-level loop."""
    n = len(a)
    mask7, mask8 = int.from_bytes(b"\x7f" * n, "big"), int.from_bytes(b"\x80" * n, "big")
    x, y = int.from_bytes(a, "big"), int.from_bytes(b, "big")
    return (((x & mask7) + (y & mask7)) ^ ((x ^ y) & mask8)).to_bytes(n, "big")


def decode_png(data, max_rows=None):
    """Returns (width, height, channels, 8-bit bytes). 16-bit samples keep their high byte."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, header, trns = 8, [], None, None
    while pos + 8 <= len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"PLTE":
            palette = body
        elif kind == b"IEND":
            break
    width, height, depth, ctype, _, _, interlace = header
    if interlace or depth not in (8, 16) or ctype not in (0, 2, 3, 4, 6) or (ctype == 3 and depth != 8):
        raise ValueError("unsupported PNG layout")
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    bpp, stride = channels * depth // 8, width * channels * depth // 8
    raw = zlib.decompress(b"".join(idat))
    rows = height if max_rows is None else min(height, max_rows)
    out, prev = [], bytes(stride)
    for y in range(rows):
        kind, line = raw[y * (stride + 1)], raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)]
        if kind == 0:
            cur = line
        elif kind == 1:
            cur = bytearray(line)
            for c in range(bpp):
                cur[c::bpp] = bytes(accumulate(line[c::bpp], lambda p, v: (p + v) & 255))
            cur = bytes(cur)
        elif kind == 2:
            cur = _add_bytes(line, prev)
        elif kind == 3:
            cur = bytearray(line)
            for i in range(stride):
                left = cur[i - bpp] if i >= bpp else 0
                cur[i] = (cur[i] + ((left + prev[i]) >> 1)) & 255
            cur = bytes(cur)
        elif kind == 4:
            cur = bytearray(line)
            for i in range(stride):
                a = cur[i - bpp] if i >= bpp else 0
                b = prev[i]
                c = prev[i - bpp] if i >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                cur[i] = (cur[i] + (a if pa <= pb and pa <= pc else (b if pb <= pc else c))) & 255
            cur = bytes(cur)
        else:
            raise ValueError("bad PNG filter")
        out.append(cur)
        prev = cur
    pixels = b"".join(out)
    if depth == 16:
        pixels = pixels[0::2]
    if ctype == 3:
        pixels = b"".join(palette[v * 3:v * 3 + 3] for v in pixels)
        channels = 3
    return width, rows, channels, pixels


def to_rgb(width, height, channels, pixels):
    """Drops alpha and expands gray to RGB; returns three planes (bytes each)."""
    if channels in (1, 2):
        gray = pixels[0::channels]
        return gray, gray, gray
    return pixels[0::channels], pixels[1::channels], pixels[2::channels]


# ---------------------------------------------------------------------------------------------------------------
# PSD (independent parser)
# ---------------------------------------------------------------------------------------------------------------

BLEND_KEYS = {b"norm": "Normal", b"mul ": "Multiply", b"scrn": "Screen", b"over": "Overlay", b"dark": "Darken",
              b"lite": "Lighten", b"div ": "ColorDodge", b"lddg": "LinearDodge", b"idiv": "ColorBurn",
              b"hLit": "HardLight", b"sLit": "SoftLight", b"diff": "Difference", b"smud": "Exclusion",
              b"fsub": "Subtract", b"lbrn": "LinearBurn", b"lLit": "LinearLight", b"hue ": "Hue", b"colr": "Color"}


def parse_psd(buf):
    if buf[:4] != b"8BPS" or struct.unpack(">H", buf[4:6])[0] != 1:
        raise ValueError("not a PSD (version 1)")
    channels, height, width, depth, mode = struct.unpack(">HIIHH", buf[12:26])
    pos = 26
    pos += 4 + struct.unpack(">I", buf[pos:pos + 4])[0]
    pos += 4 + struct.unpack(">I", buf[pos:pos + 4])[0]
    lm_len = struct.unpack(">I", buf[pos:pos + 4])[0]
    lm_start, lm_end = pos + 4, pos + 4 + lm_len
    doc = {"width": width, "height": height, "channels": channels, "depth": depth, "mode": mode, "layers": [],
           "merged_at": lm_end, "buf": buf, "transparency_first": False}
    if lm_len == 0:
        return doc
    li_len = struct.unpack(">I", buf[lm_start:lm_start + 4])[0]
    pos = lm_start + 4
    if li_len == 0:
        return doc
    count = struct.unpack(">h", buf[pos:pos + 2])[0]
    pos += 2
    doc["transparency_first"] = count < 0
    for _ in range(abs(count)):
        top, left, bottom, right, nch = struct.unpack(">iiiiH", buf[pos:pos + 18])
        pos += 18
        chans = []
        for _ in range(nch):
            chans.append(struct.unpack(">hI", buf[pos:pos + 6]))
            pos += 6
        key, opacity, clip, flags = buf[pos + 4:pos + 8], buf[pos + 8], buf[pos + 9], buf[pos + 10]
        extra_len = struct.unpack(">I", buf[pos + 12:pos + 16])[0]
        pos += 16
        extra, pos = buf[pos:pos + extra_len], pos + extra_len
        mask_len = struct.unpack(">I", extra[:4])[0]
        at = 4 + mask_len
        at += 4 + struct.unpack(">I", extra[at:at + 4])[0]
        name_len = extra[at]
        name = extra[at + 1:at + 1 + name_len].decode("latin-1")
        at += (1 + name_len + 3) // 4 * 4
        section, unicode_name = None, None
        while at + 12 <= len(extra):
            tag, size = extra[at + 4:at + 8], struct.unpack(">I", extra[at + 8:at + 12])[0]
            body = extra[at + 12:at + 12 + size]
            if tag == b"lsct" and size >= 4:
                section = struct.unpack(">I", body[:4])[0]
            elif tag == b"luni" and size >= 4:
                n = struct.unpack(">I", body[:4])[0]
                unicode_name = body[4:4 + 2 * n].decode("utf-16-be", "replace")
            at += 12 + size + (size & 1)  # blocks are padded to an even length, as rust/src/psd.rs does
        doc["layers"].append({
            "name": unicode_name if unicode_name else name, "top": top, "left": left, "bottom": bottom,
            "right": right, "channels": chans, "blend": key, "opacity": opacity, "clip": clip,
            "hidden": bool(flags & 2), "section": section, "has_mask": mask_len > 0 or any(c[0] in (-2, -3) for c in chans),
            "irrelevant": bool(flags & 16), "data_at": 0})
    for layer in doc["layers"]:
        layer["chan_at"] = []
        for cid, length in layer["channels"]:
            layer["chan_at"].append((cid, pos, length))
            pos += length
    return doc


def decode_psd_channel(buf, at, length, width, height):
    """Raw (0) and RLE (1) channels; returns bytes or None for other compressions."""
    comp = struct.unpack(">H", buf[at:at + 2])[0]
    if comp == 0:
        return buf[at + 2:at + 2 + width * height]
    if comp != 1:
        return None
    return unrle(buf, at + 2, width, height)


def unrle(buf, at, width, height):
    counts = struct.unpack(">%dH" % height, buf[at:at + 2 * height])
    pos, rows = at + 2 * height, []
    for n in counts:
        data, row, i = buf[pos:pos + n], bytearray(), 0
        while i < len(data) and len(row) < width:
            c = data[i]
            if c < 128:
                row += data[i + 1:i + 2 + c]
                i += 2 + c
            elif c > 128:
                row += data[i + 1:i + 2] * (257 - c)
                i += 2
            else:
                i += 1
        rows.append(bytes(row[:width]))
        pos += n
    return b"".join(rows)


def psd_merged(doc):
    """Merged image planes (R, G, B, optional A) as 8-bit bytes, or None when unsupported."""
    if doc["depth"] != 8 or doc["mode"] != 3:
        return None
    buf, at = doc["buf"], doc["merged_at"]
    w, h = doc["width"], doc["height"]
    comp = struct.unpack(">H", buf[at:at + 2])[0]
    at += 2
    planes = []
    nplanes = min(doc["channels"], 4)
    if comp == 0:
        for p in range(nplanes):
            planes.append(buf[at + p * w * h:at + (p + 1) * w * h])
    elif comp == 1:
        counts = struct.unpack(">%dH" % (h * doc["channels"]), buf[at:at + 2 * h * doc["channels"]])
        pos = at + 2 * h * doc["channels"]
        for p in range(doc["channels"]):
            rows = []
            for n in counts[p * h:(p + 1) * h]:
                data, row, i = buf[pos:pos + n], bytearray(), 0
                while i < len(data) and len(row) < w:
                    c = data[i]
                    if c < 128:
                        row += data[i + 1:i + 2 + c]
                        i += 2 + c
                    elif c > 128:
                        row += data[i + 1:i + 2] * (257 - c)
                        i += 2
                    else:
                        i += 1
                rows.append(bytes(row[:w]))
                pos += n
            if p < nplanes:
                planes.append(b"".join(rows))
    else:
        return None
    return planes


def expected_tree(doc):
    """Group tree from the section dividers; children keep file order (bottom-most first), as the importer does."""
    stack, top = [], []

    def node(kind, layer):
        return {"kind": kind, "layer": layer, "children": []}

    for layer in doc["layers"]:
        section = layer["section"]
        if section == 3:
            stack.append([])
        elif section in (1, 2):
            kids = stack.pop() if stack else []
            n = node("group", layer)
            n["children"] = kids
            (stack[-1] if stack else top).append(n)
        else:
            w, h = layer["right"] - layer["left"], layer["bottom"] - layer["top"]
            if w > 0 and h > 0:
                (stack[-1] if stack else top).append(node("part", layer))
    return top


def walk_expected(nodes, depth=0):
    for n in nodes:
        yield depth, n
        yield from walk_expected(n["children"], depth + 1)


def walk_actual(node, depth=0, origin=(0.0, 0.0)):
    """Yields (depth, node, world_origin_x, world_origin_y) for the children of `node`."""
    for child in node.get("children", []):
        trans = child["transform"]["trans"]
        world = (origin[0] + trans[0], origin[1] + trans[1])
        yield depth, child, world
        yield from walk_actual(child, depth + 1, world)


def box_average(plane, width, factor, ow, oh, x0, y0):
    """Average of factor x factor blocks of an 8-bit plane, starting at (x0, y0); returns ow*oh floats."""
    out = []
    for y in range(oh):
        slices = []
        for dy in range(factor):
            row = plane[(y0 + y * factor + dy) * width + x0:(y0 + y * factor + dy) * width + x0 + ow * factor]
            slices.extend(row[dx::factor] for dx in range(factor))
        out.extend(sum(vals) / len(slices) for vals in zip(*slices))
    return out


def compare_render(core, doc, merged, args):
    W, H = doc["width"], doc["height"]
    longest = max(W, H)
    factor = max(1, -(-longest // args.render_side))
    ow, oh = W // factor, H // factor
    render, err = core.call({"op": "render", "width": ow, "height": oh,
                             "bounds": [-ow * factor / 2, -oh * factor / 2, ow * factor / 2, oh * factor / 2]})
    rc = {}
    if err:
        rc["error"] = err
    else:
        rw, rh, rch, rpix = decode_png(base64.b64decode(render["png"]))
        alpha = rpix[3::rch] if rch in (2, 4) else b"\xff" * (rw * rh)
        rc["painted_fraction"] = round(sum(1 for a in alpha if a) / (rw * rh), 4)
        rc["render_size"] = [rw, rh]
        if merged is None or len(merged) < 3:
            rc["compare"] = "merged image unavailable (depth/mode/compression)"
        else:
            x0, y0 = (W - ow * factor) // 2, (H - oh * factor) // 2
            ref = [box_average(merged[c], W, factor, ow, oh, x0, y0) for c in range(3)]
            has_alpha = len(merged) > 3 and doc["transparency_first"]
            ref_a = box_average(merged[3], W, factor, ow, oh, x0, y0) if has_alpha else None
            rr, rg, rb = to_rgb(rw, rh, rch, rpix)
            total, big, n = 0.0, 0, ow * oh * 3
            for c, plane in enumerate((rr, rg, rb)):
                for i in range(ow * oh):
                    a = alpha[i] / 255
                    got = plane[i] * a + 255 * (1 - a) if rch in (2, 4) else plane[i]  # over white
                    want = ref[c][i]
                    if ref_a is not None:
                        want = want * ref_a[i] / 255 + 255 * (1 - ref_a[i] / 255)
                    d = abs(got - want)
                    total += d
                    big += d > 32
            rc["mean_abs_diff"] = round(total / n, 2)
            rc["fraction_over_32"] = round(big / n, 4)
            rc["merged_has_alpha"] = has_alpha
    return rc


def verify_psd(core, path, args):
    rec = {"file": os.path.basename(path), "size": os.path.getsize(path)}
    started = time.time()
    with open(path, "rb") as handle:
        buf = handle.read()
    doc = parse_psd(buf)
    rec.update(canvas=[doc["width"], doc["height"]], depth=doc["depth"], mode=doc["mode"])
    exp_tree = expected_tree(doc)
    exp_flat = list(walk_expected(exp_tree))
    rec["expected"] = {
        "records": len(doc["layers"]),
        "parts": sum(1 for _, n in exp_flat if n["kind"] == "part"),
        "groups": sum(1 for _, n in exp_flat if n["kind"] == "group"),
        "hidden": sum(1 for _, n in exp_flat if n["layer"]["hidden"]),
        "with_mask": sum(1 for _, n in exp_flat if n["layer"]["has_mask"]),
        "clipped": sum(1 for _, n in exp_flat if n["layer"]["clip"]),
        "non_normal_blend": sum(1 for _, n in exp_flat if n["layer"]["blend"] != b"norm"),
        "unmapped_blend": sorted({n["layer"]["blend"].decode("latin-1") for _, n in exp_flat
                                  if n["layer"]["blend"] not in BLEND_KEYS and n["layer"]["blend"] != b"pass"}),
    }
    result, err = core.call({"op": "import.psd", "path": path})
    rec["import_seconds"] = round(time.time() - started, 1)
    if err:
        rec["status"], rec["error"] = "refused", err
        return rec
    rec["status"] = "imported"
    actual = list(walk_actual(result["model"]["nodes"]))
    parts = [a for a in actual if a[1].get("type") == "Part"]
    groups = [a for a in actual if a[1].get("type") != "Part"]
    rec["actual"] = {"parts": len(parts), "groups": len(groups), "textures": result.get("imported", {}).get("textures")}
    mismatches = {"structure": 0, "name": 0, "enabled": 0, "opacity": 0, "blend": 0, "position": 0, "size": 0}
    if len(actual) != len(exp_flat):
        mismatches["structure"] += abs(len(actual) - len(exp_flat)) or 1
    W, H = doc["width"], doc["height"]
    for (edepth, exp), (adepth, act, world) in zip(exp_flat, actual):
        layer = exp["layer"]
        if edepth != adepth or (exp["kind"] == "part") != (act.get("type") == "Part"):
            mismatches["structure"] += 1
            continue
        if layer["name"] != act["name"]:
            mismatches["name"] += 1
        if layer["hidden"] == act["enabled"]:
            mismatches["enabled"] += 1
        if abs(layer["opacity"] / 255 - act["opacity"]) > 1e-4:
            mismatches["opacity"] += 1
        if exp["kind"] == "part":
            if BLEND_KEYS.get(layer["blend"], "Normal") != act["blend_mode"]:
                mismatches["blend"] += 1
            w, h = layer["right"] - layer["left"], layer["bottom"] - layer["top"]
            cx, cy = layer["left"] + w // 2 - W // 2, layer["top"] + h // 2 - H // 2
            if abs(world[0] - cx) > 0.01 or abs(world[1] - cy) > 0.01:
                mismatches["position"] += 1
            verts = act["mesh"]["verts"]
            if abs(verts[4] - (w // 2)) > 0.01 or abs(verts[3] - (h // 2)) > 0.01:
                mismatches["size"] += 1
    rec["mismatches"] = mismatches
    # Layer bitmaps: unmasked layers with raw or RLE channels, spread over the area range.
    pairs = [(act, exp["layer"]) for (_, exp), (_, act, _) in zip(exp_flat, actual) if exp["kind"] == "part"]
    pairs = [(a, l) for a, l in pairs if not l["has_mask"] and 64 <= (l["right"] - l["left"]) * (l["bottom"] - l["top"])
             <= args.max_layer_pixels]
    pairs.sort(key=lambda p: (p[1]["right"] - p[1]["left"]) * (p[1]["bottom"] - p[1]["top"]))
    picks = [pairs[i] for i in spread_picks(len(pairs), args.layers)]
    bitmaps = []
    for act, layer in picks:
        w, h = layer["right"] - layer["left"], layer["bottom"] - layer["top"]
        slot = act["textures"][0]
        tex, err = core.call({"op": "texture.get", "slot": slot})
        entry = {"name": layer["name"], "size": [w, h]}
        if err:
            entry["error"] = err
            bitmaps.append(entry)
            continue
        tw, th, tc, tpix = decode_png(base64.b64decode(tex["base64"]))
        planes = {}
        for cid, at, length in layer["chan_at"]:
            if cid in (0, 1, 2, -1):
                planes[cid] = decode_psd_channel(buf, at, length, w, h)
        if (tw, th) != (w, h) or tc != 4 or any(planes.get(c) is None for c in (0, 1, 2)):
            entry["error"] = "size/channel mismatch or unsupported compression"
            bitmaps.append(entry)
            continue
        alpha = planes.get(-1) or b"\xff" * (w * h)
        worst = 0
        for c in range(3):
            mine = bytes(v * a // 255 for v, a in zip(planes[c], alpha))
            theirs = tpix[c::4]
            worst = max(worst, max((abs(x - y) for x, y in zip(mine, theirs) if x != y), default=0))
        worst = max(worst, max((abs(x - y) for x, y in zip(alpha, tpix[3::4]) if x != y), default=0))
        entry["max_diff"] = worst
        bitmaps.append(entry)
    rec["layer_bitmaps"] = bitmaps
    # The default import (DynamicComposite groups, clipping masks) rendered against the merged image.
    rec["render"] = compare_render(core, doc, psd_merged(doc), args)
    return rec


# ---------------------------------------------------------------------------------------------------------------
# JPEG
# ---------------------------------------------------------------------------------------------------------------


def spread_picks(count, wanted):
    """Indexes of min(wanted, count) items spread evenly over range(count), each item at most once."""
    picked = min(wanted, count)
    return [int(i * (count - 1) / max(1, picked - 1)) for i in range(picked)]


def classify_jpeg(b):
    """Marker scan: returns a dict of stream properties, or None when b is not a JPEG with a frame header."""
    if b[:2] != b"\xff\xd8":
        return None
    info = {"process": None, "orientation": None, "icc": False, "restart": 0, "scans": 0, "adobe": None,
            "thumbnail": False, "components": 0, "subsampling": ""}
    i = 2
    while i + 4 <= len(b) and b[i] == 0xFF:
        m = b[i + 1]
        if m == 0xFF:
            i += 1
            continue
        if m == 0x01 or 0xD0 <= m <= 0xD8:
            i += 2
            continue
        if m == 0xD9:
            break
        n = struct.unpack(">H", b[i + 2:i + 4])[0]
        p = b[i + 4:i + 2 + n]
        if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
            if info["process"] is None and len(p) >= 6:
                prec, h, w, c = struct.unpack(">BHHB", p[:6])
                comps = [(p[7 + 3 * k] >> 4, p[7 + 3 * k] & 15) for k in range(c) if len(p) >= 9 + 3 * k]
                info.update(process={0: "baseline", 1: "extended", 2: "progressive"}.get(m - 0xC0, "SOF%d" % (m - 0xC0)),
                            precision=prec, width=w, height=h, components=c)
                if c == 1:
                    info["subsampling"] = "gray"
                elif c >= 3 and comps:
                    info["subsampling"] = {(1, 1): "4:4:4", (2, 1): "4:2:2", (2, 2): "4:2:0", (4, 1): "4:1:1",
                                           (1, 2): "4:4:0", (4, 2): "4:1:0"}.get(comps[0], "h%dv%d" % comps[0]) \
                        if all(x == (1, 1) for x in comps[1:3]) else "other"
                    if c == 4:
                        info["subsampling"] += "+4th"
        elif m == 0xE1 and p[:6] == b"Exif\0\0":
            t = p[6:]
            info["thumbnail"] = b"\xff\xd8" in t[8:]
            e = "<" if t[:2] == b"II" else ">"
            try:
                off = struct.unpack(e + "I", t[4:8])[0]
                for k in range(struct.unpack(e + "H", t[off:off + 2])[0]):
                    ent = t[off + 2 + 12 * k:off + 14 + 12 * k]
                    if struct.unpack(e + "H", ent[:2])[0] == 0x112:
                        info["orientation"] = struct.unpack(e + "H", ent[8:10])[0]
            except (struct.error, IndexError):
                pass
        elif m == 0xE2 and p[:11] == b"ICC_PROFILE":
            info["icc"] = True
        elif m == 0xEE and p[:5] == b"Adobe" and len(p) > 11:
            info["adobe"] = p[11]
        elif m == 0xDD and len(p) >= 2:
            info["restart"] = struct.unpack(">H", p[:2])[0]
        elif m == 0xDA:
            info["scans"] += 1
            i += 2 + n
            while i + 1 < len(b) and not (b[i] == 0xFF and b[i + 1] not in (0, 0xFF) and not 0xD0 <= b[i + 1] <= 0xD7):
                i += 1
            continue
        i += 2 + n
    return info if info["process"] else None


def stream_class(info):
    tags = [info["process"], info["subsampling"] or "?", "%d-bit" % info.get("precision", 8)]
    tags += [t for t, on in (("restart", info["restart"]), ("icc", info["icc"]), ("thumbnail", info["thumbnail"]),
                             ("orientation%s" % info["orientation"], info["orientation"] not in (None, 1)),
                             ("multi-scan", info["scans"] > 1 and info["process"] != "progressive"),
                             ("adobe%s" % info["adobe"], info["adobe"] is not None)) if on]
    return " ".join(tags)


def pick_jpegs(infos, count):
    """Round-robin over stream classes, rarest class first, spreading each class over its file sizes."""
    groups = {}
    for path, info in infos:
        groups.setdefault(stream_class(info), []).append((info.get("width", 0) * info.get("height", 0), path, info))
    order = sorted(groups.values(), key=len)
    for g in order:
        g.sort(key=lambda t: t[0])
    chosen, taken = [], {id(g): 0 for g in order}
    while len(chosen) < count and any(taken[id(g)] < len(g) for g in order):
        for g in order:
            if len(chosen) >= count:
                break
            k = taken[id(g)]
            if k < len(g):
                # alternate between the small and the large end so sizes differ
                idx = (len(g) - 1 - k // 2) if k % 2 else k // 2
                chosen.append(g[idx][1:])
                taken[id(g)] += 1
    # a class' picks must be distinct; the alternation can repeat the middle element
    seen, unique = set(), []
    for path, info in chosen:
        if path not in seen:
            seen.add(path)
            unique.append((path, info))
    return unique


def sips_reference(path, workdir):
    """RGB planes decoded by macOS ImageIO through `sips`, or None."""
    out = os.path.join(workdir, "reference.png")
    if os.path.exists(out):
        os.unlink(out)
    done = subprocess.run(["sips", "-s", "format", "png", path, "--out", out], capture_output=True, timeout=120)
    if done.returncode != 0 or not os.path.exists(out):
        return None
    with open(out, "rb") as handle:
        width, height, channels, pixels = decode_png(handle.read())
    return width, height, to_rgb(width, height, channels, pixels)


def djpeg_reference(path):
    """RGB planes decoded by libjpeg's `djpeg` without fancy upsampling (this decoder replicates chroma)."""
    done = subprocess.run(["djpeg", "-nosmooth", "-pnm", path], capture_output=True, timeout=120)
    data = done.stdout
    if done.returncode != 0 or data[:2] not in (b"P5", b"P6"):
        return None
    fields, at = [], 2
    while len(fields) < 3:  # width, height, maxval; comments do not occur in djpeg output
        while data[at:at + 1].isspace():
            at += 1
        start = at
        while not data[at:at + 1].isspace():
            at += 1
        fields.append(int(data[start:at]))
    at += 1
    width, height = fields[0], fields[1]
    pixels = data[at:]
    planes = (pixels, pixels, pixels) if data[:2] == b"P5" else (pixels[0::3], pixels[1::3], pixels[2::3])
    return width, height, planes


def compare_planes(mine, theirs, width, height):
    sums, peak, over = 0, 0, 0
    for a, b in zip(mine, theirs):
        diffs = [abs(x - y) for x, y in zip(a, b)]
        sums += sum(diffs)
        peak = max(peak, max(diffs))
        over += sum(1 for d in diffs if d > 16)
    n = width * height * 3
    return {"mean_abs_diff": round(sums / n, 3), "max_diff": peak, "fraction_over_16": round(over / n, 5)}


def verify_jpeg(core, path, info, args, workdir, compare):
    rec = {"file": os.path.basename(path), "class": stream_class(info), "process": info["process"],
           "components": info["components"], "subsampling": info["subsampling"], "size": [info.get("width"), info.get("height")]}
    core.call({"op": "new"})
    _, err = core.call({"op": "merge.images", "paths": [path]})
    if err:
        rec["status"], rec["error"] = "refused", err
        rec["leaks_directory"] = os.path.dirname(path) in err
        return rec
    tex, err = core.call({"op": "texture.get", "slot": 0})
    if err:
        rec["status"], rec["error"] = "no-texture", err
        return rec
    rec["status"] = "decoded"
    if not compare:
        rec["compare"] = "skipped (no reference decoder, or over --max-compare-pixels)"
        return rec
    ours = decode_png(base64.b64decode(tex["base64"]))
    rec["decoded_size"] = [ours[0], ours[1]]
    mine = to_rgb(*ours)
    references = {}
    if shutil.which("sips"):
        references["sips"] = sips_reference(path, workdir)
    if shutil.which("djpeg"):
        references["djpeg"] = djpeg_reference(path)
    for name, ref in references.items():
        if ref is None:
            rec[name] = {"error": "reference decoder could not read the file"}
        elif (ref[0], ref[1]) != (ours[0], ours[1]):
            rec[name] = {"error": "size %dx%d differs from the reference %dx%d" % (ours[0], ours[1], ref[0], ref[1])}
        else:
            rec[name] = compare_planes(mine, ref[2], ours[0], ours[1])
    # ImageIO reads Adobe transform 0 as YCbCr and libjpeg-style decoders as RGB; sips is not a reference there.
    if info["adobe"] == 0 and info["components"] == 3 and "sips" in rec:
        rec["sips"]["unreliable"] = "Adobe transform 0"
    return rec


# ---------------------------------------------------------------------------------------------------------------
# KRA (independent reader)
# ---------------------------------------------------------------------------------------------------------------

# The importer's limits (rust/src/kra.rs `Limits`, built from psd.rs `Limits::default()` by `import.kra`).
KRA_MAX_PIXELS, KRA_MAX_LAYERS = 100_000_000, 32_768
KRA_MAX_ENTRY_BYTES, KRA_MAX_TOTAL_BYTES = 128 * 1024 * 1024, 800_000_000
KRA_MAX_XML_BYTES = 16 * 1024 * 1024
KRA_MAX_ENTRIES, KRA_MAX_GROUP_DEPTH = 16_384, 48
KRA_MAX_COMPOSITE_DEPTH = 16  # rust/src/render.rs: a group nested below this many Composites imports as a plain Node
# Krita composite ids the importer maps (kra.rs `engine_blend`: D's kra-d list plus Krita's `lighten`); every other id
# imports as Normal.
KRA_BLEND = {"normal": "Normal", "multiply": "Multiply", "screen": "Screen", "overlay": "Overlay", "darken": "Darken",
             "lighten": "Lighten", "lighter color": "Lighten", "dodge": "ColorDodge", "linear_dodge": "LinearDodge", "burn": "ColorBurn",
             "hard_light": "HardLight", "soft_light": "SoftLight", "diff": "Difference", "exclusion": "Exclusion",
             "subtract": "Subtract"}
KRA_SUPPORTED_NODES = ("paintlayer", "grouplayer")


def _local(tag):
    return tag.rsplit("}", 1)[-1]


def _kids(element, name):
    return [c for c in element if _local(c.tag) == name]


def _kra_nodes(holder):
    """Layer nodes below the `layers` child of `holder`, in file order (Krita writes the top-most layer first)."""
    out = []
    for el in [e for holder_layers in _kids(holder, "layers") for e in _kids(holder_layers, "layer")]:
        kind = el.get("nodetype")
        node = {"name": el.get("name", ""), "type": kind, "visible": int(el.get("visible", "1")) != 0,
                "opacity": int(el.get("opacity", "255")), "op": el.get("compositeop", "normal"),
                "x": int(el.get("x", "0")), "y": int(el.get("y", "0")), "file": el.get("filename"),
                "cs": el.get("colorspacename"), "passthrough": el.get("passthrough") == "1", "children": []}
        if kind == "grouplayer":
            node["children"] = _kra_nodes(el)
            node["nested"] = bool(_kids(el, "layers"))  # the reader descends even when the list is empty
        out.append(node)
    return out


def kra_shown(nodes, shown=True):
    """(node, shown) for every node. A node is shown when it and every ancestor is visible with opacity above 0; any other
    node cannot contribute to Krita's merged image."""
    for node in nodes:
        on = shown and node["visible"] and node["opacity"] > 0
        yield node, on
        yield from kra_shown(node["children"], on)


def kra_walk(nodes, depth=0):
    for node in nodes:
        yield depth, node
        yield from kra_walk(node["children"], depth + 1)


class KraHeaderTooLarge(ValueError):
    """mimetype or maindoc.xml is over the reader's limit by its declared size; it is not inflated, and `zf` is the archive."""

    def __init__(self, zf, name, limit):
        super().__init__("'%s' is larger than the reader's limit of %d bytes; not inflated here" % (name, limit))
        self.zf = zf


def parse_kra(path):
    """Container and layer tree from `maindoc.xml`; the tile files are read later, one layer at a time."""
    zf = zipfile.ZipFile(path)
    # The reader refuses these two by the declared size before it inflates them (kra.rs); this script stops the same way.
    for name, limit in (("mimetype", 64), ("maindoc.xml", KRA_MAX_XML_BYTES)):
        if zf.getinfo(name).file_size > limit:
            raise KraHeaderTooLarge(zf, name, limit)
    if zf.read("mimetype") != b"application/x-krita":
        raise ValueError("not a Krita document (mimetype)")
    root = ET.fromstring(zf.read("maindoc.xml"))
    image = _kids(root, "IMAGE")[0]
    doc = {"zf": zf, "name": image.get("name"), "width": int(image.get("width")), "height": int(image.get("height")),
           "cs": image.get("colorspacename"), "profile": image.get("profile"), "version": root.get("kritaVersion"),
           "layers": _kra_nodes(image), "merged_name": "mergedimage.png" if "mergedimage.png" in zf.namelist() else None}
    for _, node in kra_walk(doc["layers"]):
        if node["type"] == "paintlayer" and node["file"]:
            node["entry"] = "%s/layers/%s" % (doc["name"], node["file"])
            info = zf.getinfo(node["entry"]) if node["entry"] in zf.namelist() else None
            node["inflated"] = info.file_size if info else None
    return doc


def lzf_decode(data, size):
    """liblzf: control < 32 is a literal run of control + 1 bytes, otherwise a back reference."""
    out, i, n = bytearray(), 0, len(data)
    while i < n:
        control = data[i]
        i += 1
        if control < 32:
            if i + control + 1 > n:
                raise ValueError("LZF literal run is truncated")
            if len(out) + control + 1 > size:
                raise ValueError("LZF output exceeds the tile size")
            out += data[i:i + control + 1]
            i += control + 1
            continue
        length = control >> 5
        if length == 7:
            length += data[i]
            i += 1
        start = len(out) - ((control & 31) << 8) - data[i] - 1
        i += 1
        length += 2
        if start < 0:
            raise ValueError("LZF back reference before the start")
        if len(out) + length > size:
            raise ValueError("LZF output exceeds the tile size")
        if start + length <= len(out):
            out += out[start:start + length]
        else:  # overlapping copy repeats the pattern
            pattern = out[start:]
            out += (pattern * (length // len(pattern) + 1))[:length]
    if len(out) != size:
        raise ValueError("LZF output is %d bytes, expected %d" % (len(out), size))
    return bytes(out)


def kra_tile_table(data):
    """((tile width, tile height, pixel size), [(x, y, block offset, block length)]). Krita's length counts the flag byte."""
    pos, head = 0, {}
    for _ in range(5):
        end = data.index(b"\n", pos)
        key, _, value = data[pos:end].decode("ascii").partition(" ")
        head[key] = value
        pos = end + 1
    tiles = []
    for _ in range(int(head["DATA"])):
        end = data.index(b"\n", pos)
        x, y, codec, length = data[pos:end].decode("ascii").split(",")
        pos, length = end + 1, int(length)
        if codec != "LZF":
            raise ValueError("tile codec %r" % codec)
        tiles.append((int(x), int(y), pos, length))
        pos += length
    if pos != len(data):
        raise ValueError("tile table does not end at the end of the file (%d bytes left)" % (len(data) - pos))
    return (int(head["TILEWIDTH"]), int(head["TILEHEIGHT"]), int(head["PIXELSIZE"])), tiles


def kra_tile_planes(data, size, tile):
    """The four planes (B, G, R, A) of one tile; flag byte 1 means LZF, 0 stored."""
    _, _, offset, length = tile
    block = data[offset:offset + length]
    area = size[0] * size[1]
    payload = lzf_decode(block[1:], area * size[2]) if block[0] == 1 else bytes(block[1:])
    if block[0] not in (0, 1) or len(payload) != area * size[2]:
        raise ValueError("bad tile block")
    return [payload[k * area:(k + 1) * area] for k in range(size[2])]


def kra_box(size, tiles, canvas=None):
    """Bounding box of the tiles, joined with the rectangle `canvas` when there is one."""
    boxes = [(t[0], t[1], t[0] + size[0], t[1] + size[1]) for t in tiles] + ([tuple(canvas)] if canvas else [])
    if not boxes:
        return 0, 0, 0, 0
    return min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes)


def kra_extent_tiles(data, size, tiles, fill):
    if not fill or not (0 < size[0] <= 1024 and 0 < size[1] <= 1024 and size[2] == 4):
        return tiles
    area = size[0] * size[1]
    if sum(bool(t[3]) for t in tiles) * area > KRA_MAX_PIXELS:
        return tiles
    red, green, blue, alpha = fill[0]
    expected = [bytes([v]) * area for v in (blue, green, red, alpha)]
    return [tile for tile in tiles if tile[3] and kra_tile_planes(data, size, tile) != expected]


def kra_layer_pixels(data, size, tiles, fill=None):
    """Layer-local rect of the non-transparent pixels and its R, G, B, A planes (straight alpha); (None, None) if empty.
    `fill` is ((r, g, b, a), canvas rect) for a layer with a default pixel: every pixel of the box (the tiles' and the
    canvas) that no tile covers takes that pixel."""
    left, top, right, bottom = kra_box(size, kra_extent_tiles(data, size, tiles, fill), fill and fill[1])
    cols, rows = right - left, bottom - top
    planes = [bytearray(cols * rows) for _ in range(4)]
    if fill:
        for plane, value in zip(planes, fill[0]):
            plane[:] = bytearray([value]) * (cols * rows)
    for tile in tiles:
        tile_planes = kra_tile_planes(data, size, tile)
        x0, x1 = max(0, left - tile[0]), min(size[0], right - tile[0])
        y0, y1 = max(0, top - tile[1]), min(size[1], bottom - tile[1])
        if x1 <= x0 or y1 <= y0:
            continue
        for plane, source in zip(planes, (tile_planes[2], tile_planes[1], tile_planes[0], tile_planes[3])):
            for row in range(y0, y1):
                at = (tile[1] - top + row) * cols + tile[0] + x0 - left
                plane[at:at + x1 - x0] = source[row * size[0] + x0:row * size[0] + x1]
    alpha = planes[3]
    min_x, min_y, max_x, max_y = cols, rows, 0, 0
    for y in range(rows):
        row = bytes(alpha[y * cols:(y + 1) * cols])
        lstripped = row.lstrip(b"\0")
        if lstripped:
            min_x, max_x = min(min_x, cols - len(lstripped)), max(max_x, len(row.rstrip(b"\0")))
            min_y, max_y = min(min_y, y), y + 1
    if max_x == 0:
        return None, None
    cropped = [b"".join(bytes(p[y * cols + min_x:y * cols + max_x]) for y in range(min_y, max_y)) for p in planes]
    return (left + min_x, top + min_y, left + max_x, top + max_y), cropped


def kra_default_pixel(doc, node):
    """RGBA of the layer's default pixel, or None when it is transparent, missing or not one 4-byte pixel (Krita writes it
    in `<tile file>.defaultpixel` in memory order: B, G, R, A)."""
    info = doc["zf"].NameToInfo.get(node["entry"] + ".defaultpixel")
    if info is None or info.file_size != 4:
        return None
    blue, green, red, alpha = doc["zf"].read(info)
    return (red, green, blue, alpha) if alpha else None


def kra_layer_info(doc, node):
    """Tile statistics, with bounded pixel decoding when a default pixel can make a tile irrelevant to the bounds.
    Record tiles, bounding box, inflated size, and the fill: the default
    pixel with the canvas in layer-local pixels, when the pixel is not transparent. The box joins non-default tiles
    with the canvas when there is a fill. The reader refuses an entry over its limit by the declared size, so
    this script does not inflate one either and leaves no statistics."""
    if (node.get("inflated") or 0) > KRA_MAX_ENTRY_BYTES:
        return None
    data = doc["zf"].read(node["entry"])
    size, tiles = kra_tile_table(data)
    pixel = kra_default_pixel(doc, node)
    fill = (pixel, (-node["x"], -node["y"], doc["width"] - node["x"], doc["height"] - node["y"])) if pixel else None
    box = kra_box(size, kra_extent_tiles(data, size, tiles, fill), fill and fill[1])
    # The reader counts the tiles that have data (it drops a tile of length zero) times the tile area, wherever they lie.
    node.update(tiles=len(tiles), fill=fill, tile_box=box, tile_pixels=(box[2] - box[0]) * (box[3] - box[1]),
                tile_size=size, tile_area=sum(1 for t in tiles if t[3]) * size[0] * size[1])
    return data


def kra_reads(node):
    """Whether the reader makes a layer of a paint layer: it has tiles, or a default pixel that is not transparent."""
    return bool(node.get("tiles") or node.get("fill"))


def kra_decode(doc, node):
    """(rect, planes) of kra_layer_pixels for a paint layer, with its default pixel."""
    data = doc["zf"].read(node["entry"])
    size, tiles = kra_tile_table(data)
    return kra_layer_pixels(data, size, tiles, node.get("fill"))


def kra_cropped_box(doc, node):
    """Exact layer-local rect of the non-transparent pixels (decodes the tiles), or None when the layer is empty."""
    return kra_decode(doc, node)[0]


def kra_reached(doc):
    """(layer tree, stopped): what the reader gets through before it refuses a colour model (kra.rs `parse`, `walk`). The
    document's colour space is checked before the canvas, a paint layer's before it opens the layer's tile file, so nothing
    of the tree is reached when the document is not RGBA, and otherwise the nodes before the first paint layer that is not.
    A group that holds that layer is `open`: its end record has not been written yet."""
    def cut(nodes):
        out = []
        for n in nodes:
            if n["type"] == "paintlayer" and n["cs"] != "RGBA":
                return out, True
            if n["type"] == "grouplayer":
                children, stopped = cut(n["children"])
                n = dict(n, children=children, open=stopped)
            out.append(n)
            if n.get("open"):
                return out, True
        return out, False
    return ([], True) if doc["cs"] != "RGBA" else cut(doc["layers"])


def kra_paint(doc):
    """The paint layers the reader opens: it reads the tile file of each one that is RGBA, up to a colour refusal."""
    return [n for _, n in kra_walk(kra_reached(doc)[0]) if n["type"] == "paintlayer" and n["cs"] == "RGBA" and n.get("entry")]


def kra_default_entries(doc):
    """The `.defaultpixel` entries the reader opens: the one of each paint layer it reads, when that entry is 4 bytes
    (kra.rs `default_pixel` leaves an entry of any other size unread)."""
    infos = (doc["zf"].NameToInfo.get(n["entry"] + ".defaultpixel") for n in kra_paint(doc))
    return [info for info in infos if info is not None and info.file_size == 4]


def kra_zip_features(doc):
    """(label, regex) for each archive feature the reader refuses by name (kra.rs `Zip::open` and `Zip::read`). Python's
    zipfile opens all of them, so the end record and the sizes of the directory entries are read as the reader reads them,
    the encrypted flag comes from zipfile, and a compression method counts only on the entries the reader opens."""
    zf, hit = doc["zf"], []
    zf.fp.seek(0, 2)
    zf.fp.seek(max(0, zf.fp.tell() - 22 - 0xFFFF))
    tail = zf.fp.read()
    end = next((at for at in range(len(tail) - 22, -1, -1) if tail[at:at + 4] == b"PK\5\6"
                and at + 22 + struct.unpack_from("<H", tail, at + 20)[0] == len(tail)), None)
    infos = zf.infolist()
    if end is not None:
        disk, directory_disk, on_disk, count, size, offset = struct.unpack_from("<HHHHII", tail, end + 4)
        if disk or directory_disk or on_disk != count:
            hit.append(("multi-disk archive", r"multi-disk ZIP archives are not supported"))
        zip64 = count == 0xFFFF or 0xFFFFFFFF in (size, offset)
        if not zip64:
            # zipfile swaps a 0xFFFFFFFF size for the real one from the ZIP64 extra field, which may be small; the reader
            # tests the stored fields of every directory entry, so they are read from the file here.
            zf.fp.seek(offset)
            directory, at = zf.fp.read(size), 0
            while directory[at:at + 4] == b"PK\1\2" and at + 46 <= len(directory):
                zip64 = zip64 or 0xFFFFFFFF in struct.unpack_from("<II", directory, at + 20)
                at += 46 + sum(struct.unpack_from("<HHH", directory, at + 28))
        if zip64:
            hit.append(("ZIP64 archive", r"ZIP64 archives are not supported"))
    if any(i.flag_bits & 1 for i in infos):
        hit.append(("encrypted entries", r"encrypted ZIP entries are not supported"))
    read = ["mimetype", "maindoc.xml"] + [n["entry"] for n in kra_paint(doc)]
    read += [info.filename for info in kra_default_entries(doc)]
    methods = {zf.getinfo(name).compress_type for name in read if name in zf.NameToInfo}
    for method in sorted(methods - {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}):
        hit.append(("compression method %d" % method, r"uses unsupported ZIP compression method %d\b" % method))
    return hit


def kra_limits_hit(doc):
    """(label, regex) for each limit of the importer that this file exceeds, found from the container and tile statistics.
    The regex is the part of the refusal (kra.rs, model_import.rs) that names that limit and its value; the canvas
    and the tile boxes share the number 100,000,000, so the words matter as much as the number. A document that the reader
    refuses for its colour model is judged on what the reader checks before that (see `kra_reached`), so every hit here
    comes before the colour refusal and the colour message is not accepted beside one."""
    hit = kra_zip_features(doc)
    layers, stopped = kra_reached(doc)
    paint = kra_paint(doc)
    if len(doc["zf"].infolist()) > KRA_MAX_ENTRIES:
        hit.append(("archive entries (%d)" % KRA_MAX_ENTRIES, r"the archive has more than %d entries" % KRA_MAX_ENTRIES))
    # Top-level layers are at depth 0; a group at depth 48 that holds a layer list takes the reader to depth 49.
    if any(n["type"] == "grouplayer" and n["nested"] and depth >= KRA_MAX_GROUP_DEPTH for depth, n in kra_walk(layers)):
        hit.append(("group depth (%d)" % KRA_MAX_GROUP_DEPTH, r"nested deeper than %d levels" % KRA_MAX_GROUP_DEPTH))
    if doc["cs"] == "RGBA" and doc["width"] * doc["height"] > KRA_MAX_PIXELS:
        hit.append(("canvas pixels (%d)" % KRA_MAX_PIXELS, r"canvas size is out of range \(limit %d pixels\)" % KRA_MAX_PIXELS))
    # The reader counts records, not nodes: a group is two (its start and its end), a paint layer one, any other node
    # none. A paint layer without tiles is dropped before it is counted, so this is high by the number of those.
    groups = sum(1 if n.get("open") else 2 for _, n in kra_walk(layers) if n["type"] == "grouplayer")
    if groups + len(paint) > KRA_MAX_LAYERS:
        hit.append(("layer count (%d)" % KRA_MAX_LAYERS, r"more than %d layers" % KRA_MAX_LAYERS))
    # mimetype and maindoc.xml are read first and count towards the archive budget; each has its own cap. So does the
    # default pixel entry of a layer, four bytes each.
    header = [doc["zf"].getinfo(name).file_size for name in ("mimetype", "maindoc.xml")]
    if header[0] > 64:
        hit.append(("mimetype size (64 bytes)", r"'mimetype' is larger than the supported size \(limit 64 bytes inflated\)"))
    if header[1] > KRA_MAX_XML_BYTES:
        hit.append(("maindoc.xml size (%d bytes)" % KRA_MAX_XML_BYTES,
                    r"'maindoc\.xml' is larger than the supported size \(limit %d bytes inflated\)" % KRA_MAX_XML_BYTES))
    inflated = [n.get("inflated") or 0 for n in paint]
    if any(v > KRA_MAX_ENTRY_BYTES for v in inflated):
        hit.append(("inflated entry size (%d bytes)" % KRA_MAX_ENTRY_BYTES,
                    r"larger than the supported size \(limit %d bytes inflated\)" % KRA_MAX_ENTRY_BYTES))
    if sum(header) + sum(inflated) + 4 * len(kra_default_entries(doc)) > KRA_MAX_TOTAL_BYTES:
        hit.append(("inflated archive size (%d bytes)" % KRA_MAX_TOTAL_BYTES,
                    r"contents exceed the memory budget \(limit %d bytes inflated\)" % KRA_MAX_TOTAL_BYTES))
    boxes = [n["tile_pixels"] for n in paint if "tile_pixels" in n]
    # Per layer, the tiles that have data times the tile area (overlapping or repeated tiles keep the box small).
    if any(v > KRA_MAX_PIXELS for v in boxes) or sum(boxes) > KRA_MAX_PIXELS or any(
            n.get("tile_area", 0) > KRA_MAX_PIXELS for n in paint):
        hit.append(("layer tile pixels (%d)" % KRA_MAX_PIXELS,
                    r"(?:tile box exceeds|tiles exceed) the memory budget \(limit %(n)d pixels\)|cover \d+ pixels, limit %(n)d\b"
                    % {"n": KRA_MAX_PIXELS}))
    # The texture check runs after the container is read, so a limit hit above refuses the file first. That also keeps a
    # tile box of any size (two small tiles far apart) from being decoded exactly here.
    for n in [] if hit or stopped else paint:
        if n.get("tile_pixels", 0) > TEXTURE_PIXEL_LIMIT or (n.get("tile_box") and max(
                n["tile_box"][2] - n["tile_box"][0], n["tile_box"][3] - n["tile_box"][1]) > TEXTURE_SIDE_LIMIT):
            rect = kra_cropped_box(doc, n)  # the texture is the non-transparent crop, so decode to know its size
            if rect and ((rect[2] - rect[0]) > TEXTURE_SIDE_LIMIT or (rect[3] - rect[1]) > TEXTURE_SIDE_LIMIT
                         or (rect[2] - rect[0]) * (rect[3] - rect[1]) > TEXTURE_PIXEL_LIMIT):
                hit.append(("texture size (%d per side, %d pixels)" % (TEXTURE_SIDE_LIMIT, TEXTURE_PIXEL_LIMIT),
                            r"larger than the supported texture size \(at most %d per side and %d pixels\)"
                            % (TEXTURE_SIDE_LIMIT, TEXTURE_PIXEL_LIMIT)))
                break
    return hit


def kra_has_content(doc, node):
    """False only for a vector layer whose content.svg is an empty file; other unsupported types count as content."""
    entry = "%s/layers/%s.shapelayer/content.svg" % (doc["name"], node["file"])
    return not (node["type"] == "shapelayer" and node["file"] and entry in doc["zf"].namelist()
                and doc["zf"].getinfo(entry).file_size == 0)


def classify_kra(doc):
    flat = [n for _, n in kra_walk(doc["layers"])]
    left_out = [n for n, on in kra_shown(doc["layers"]) if on and n["type"] not in KRA_SUPPORTED_NODES]
    types = {}
    for n in flat:
        types[n["type"]] = types.get(n["type"], 0) + 1
    unsupported = [n for n in flat if n["type"] not in KRA_SUPPORTED_NODES]
    paint = [n for n in flat if n["type"] == "paintlayer"]
    ops = {}
    for n in flat:
        ops[n["op"]] = ops.get(n["op"], 0) + 1
    return {
        "krita": doc["version"], "canvas": [doc["width"], doc["height"]], "colorspace": doc["cs"], "profile": doc["profile"],
        "depth": "8-bit integer" if doc["cs"] == "RGBA" else "other (%s)" % doc["cs"],
        "nodes": len(flat), "types": dict(sorted(types.items())),
        "hidden": sum(1 for n in flat if not n["visible"]), "paint_colorspaces": sorted({n["cs"] for n in paint}),
        "composite_ops": dict(sorted(ops.items())),
        "unmapped_ops": sorted(o for o in ops if o not in KRA_BLEND),
        "unsupported_nodes": {t: sum(1 for n in unsupported if n["type"] == t) for t in sorted({n["type"] for n in unsupported})},
        "unsupported_visible": len(left_out),  # shown in the merged image: the node and its ancestors are visible
        "unsupported_with_content": sum(1 for n in left_out if kra_has_content(doc, n)),
        "passthrough_groups": sum(1 for n in flat if n["passthrough"]),
        "groups_with_blend": sum(1 for n in flat if n["type"] == "grouplayer" and n["op"] != "normal"),
        "has_merged_image": doc["merged_name"] is not None,
    }


def kra_expected_skipped(doc):
    """What `imported.skippedLayers` must say: counts by node type of every node that is not a paint or group layer;
    whether any of them has its own `visible` set. None when nothing is skipped. A paint layer without tiles and without a
    default pixel to show is empty, not skipped."""
    counts, visible = {}, False
    for _, n in kra_walk(doc["layers"]):
        if n["type"] not in KRA_SUPPORTED_NODES:
            counts[n["type"]] = counts.get(n["type"], 0) + 1
            visible = visible or n["visible"]
    return {"visible": visible, "types": counts} if counts else None


def kra_reported_skipped(imported):
    """`imported.skippedLayers` in the shape of `kra_expected_skipped`. The importer names 16 types and counts the rest
    under `other`, which is kept so that such a report never compares equal."""
    skipped = imported.get("skippedLayers")
    if skipped is None:
        return None
    out = {"visible": skipped["visible"], "types": {t["type"]: t["count"] for t in skipped["types"]}}
    if skipped["other"]:
        out["other"] = skipped["other"]
    return out


def kra_expected_blends(doc):
    """What `imported.blendModesNotApplied` must say: the layers and groups the reader keeps whose composite id is not in
    `KRA_BLEND`, by id as the importer names it (control characters dropped, 64 characters), in file order; 16 names, the
    layers of the rest under `other`. A pass-through group's id has no effect. None when there are none."""
    counts = {}
    for _, n in kra_walk(doc["layers"]):
        if n["op"] not in KRA_BLEND and ((n["type"] == "grouplayer" and not n["passthrough"]) or (n["type"] == "paintlayer" and kra_reads(n))):
            name = "".join(c for c in n["op"] if unicodedata.category(c) != "Cc")[:64]
            counts[name] = counts.get(name, 0) + 1
    named = list(counts)[:16]
    return {"count": sum(counts.values()), "modes": {k: counts[k] for k in named},
            "other": sum(counts.values()) - sum(counts[k] for k in named)} if counts else None


def kra_reported_blends(imported):
    """`imported.blendModesNotApplied` in the shape of `kra_expected_blends`."""
    said = imported.get("blendModesNotApplied")
    return said and {"count": said["count"], "modes": {m["mode"]: m["count"] for m in said["modes"]}, "other": said["other"]}


def kra_expected(nodes):
    """Expected node tree, children bottom-most first as the importer writes them. Paint layers without tiles and without
    a default pixel to show are dropped."""
    out = []
    for n in reversed(nodes):
        if n["type"] == "grouplayer":
            out.append({"kind": "group", "node": n, "children": kra_expected(n["children"])})
        elif n["type"] == "paintlayer" and kra_reads(n):
            out.append({"kind": "part", "node": n, "children": []})
    return out


def kra_expects_paint(expected, empty, merged):
    """True when Krita's merged image has a visible pixel, or the expected tree has a shown paint layer that is not empty."""
    if merged is not None and (len(merged) < 4 or any(merged[3])):
        return True

    def shown(nodes, on):
        for exp in nodes:
            node = exp["node"]
            here = on and node["visible"] and node["opacity"] > 0
            if (exp["kind"] == "part" and here and not empty(node)) or shown(exp["children"], here):
                return True
        return False

    return shown(expected, True)


def kra_blends_below(expected):
    """The importer's `blends_below`: a layer or group among `expected` (or below it) has a blend mode other than Normal.
    A pass-through group reads as Normal, so only what is inside it counts."""
    return any(KRA_BLEND.get(exp["node"]["op"], "Normal") != "Normal" and not exp["node"]["passthrough"]
               or kra_blends_below(exp["children"]) for exp in expected)


def kra_compare_level(doc, expected, actual_nodes, origin, mism, state, composites=0):
    """Pairs expected and actual nodes of one level. An expected paint layer the importer did not create must be fully
    transparent (the importer crops to the non-transparent pixels and makes no node for an empty layer). `composites` is
    the number of Composites above this level."""
    W, H = doc["width"], doc["height"]
    i = 0
    for exp in expected:
        node = exp["node"]
        act = actual_nodes[i] if i < len(actual_nodes) else None
        akind = None if act is None else ("part" if act.get("type") == "Part" else "group")
        # Before the names are compared: the next layer may have the same name, and that one has the node.
        if exp["kind"] == "part" and state["empty"](node):
            state["empty_dropped"] += 1
            continue
        if act is not None and akind == exp["kind"] and act["name"] == node["name"]:
            matched = True
        elif act is not None and akind == exp["kind"]:
            mism["name"] += 1
            matched = True
        else:
            mism["structure"] += 1
            i += 1 if act is not None else 0
            continue
        i += 1
        trans = act["transform"]["trans"]
        world = (origin[0] + trans[0], origin[1] + trans[1])
        if node["visible"] != act["enabled"]:
            mism["enabled"] += 1
        if abs(node["opacity"] / 255 - act["opacity"]) > 1e-4:
            mism["opacity"] += 1
        if exp["kind"] == "group":
            # Krita does not composite a pass-through group's own projection, so its op is not applied.
            want = "Normal" if node["passthrough"] else KRA_BLEND.get(node["op"], "Normal")
            # A full-opacity pass-through group that holds a blended layer is a plain Node (no layer is clipped in a Krita
            # file); so is a group below the Composite nesting limit. Any other group is a Composite.
            plain = (node["passthrough"] and node["opacity"] >= 255 and kra_blends_below(exp["children"])
                     or composites >= KRA_MAX_COMPOSITE_DEPTH)
            if plain != (act.get("type") == "Node"):
                mism["structure"] += 1
            if act.get("type") == "Node":  # a plain node has no blend mode
                state["group_blend_dropped"] += want != "Normal"
            elif want != act.get("blend_mode"):
                mism["blend"] += 1
            kra_compare_level(doc, exp["children"], act.get("children", []), world, mism, state,
                              composites + (act.get("type") != "Node"))
            continue
        if KRA_BLEND.get(node["op"], "Normal") != act["blend_mode"]:
            mism["blend"] += 1
        verts = act["mesh"]["verts"]
        half = (verts[4], verts[3])  # half width, half height of the quad
        cx, cy = world[0] + W // 2, world[1] + H // 2  # centre in canvas pixels
        rect = state["rect"](node)
        if rect is not None:
            left, top, right, bottom = node["x"] + rect[0], node["y"] + rect[1], node["x"] + rect[2], node["y"] + rect[3]
            w, h = right - left, bottom - top
            if abs(cx - (left + w // 2)) > 0.01 or abs(cy - (top + h // 2)) > 0.01:
                mism["position"] += 1
            if abs(half[0] - w // 2) > 0.01 or abs(half[1] - h // 2) > 0.01:
                mism["size"] += 1
            state["exact"] += 1
        else:  # too big to decode here: the centre must at least lie inside the layer's tile box
            box = node["tile_box"]
            if not (node["x"] + box[0] <= cx <= node["x"] + box[2] and node["y"] + box[1] <= cy <= node["y"] + box[3]):
                mism["position"] += 1
            state["bounds_only"] += 1
    mism["structure"] += max(0, len(actual_nodes) - i)


def kra_merged(doc):
    """(planes R, G, B[, A], has_alpha) of mergedimage.png, or None with the reason."""
    if not doc["merged_name"]:
        return None, "no mergedimage.png"
    w, h, ch, pix = decode_png(doc["zf"].read(doc["merged_name"]))
    if (w, h) != (doc["width"], doc["height"]):
        return None, "mergedimage.png is %dx%d, not the canvas size" % (w, h)
    planes = list(to_rgb(w, h, ch, pix))
    if ch in (2, 4):
        planes.append(pix[ch - 1::ch])
    return planes, None


def kra_self_test():
    """Hand-made data for the LZF decoder and the tile reader (no real file involved)."""
    assert lzf_decode(bytes([2, 97, 98, 99]), 3) == b"abc"  # literal run
    assert lzf_decode(bytes([2, 97, 98, 99, 3 << 5, 2]), 8) == b"abcabcab"  # overlapping back reference of 5 bytes
    assert lzf_decode(bytes([0, 7, (7 << 5), 1, 0]), 11) == bytes([7]) * 11  # long form copies 7 + 1 + 2 = 10 bytes
    try:
        lzf_decode(bytes([0, 1, 1 << 5, 5]), 4)  # distance 6 reaches before the start
        raise AssertionError("a back reference before the start was accepted")
    except ValueError:
        pass
    for data in (bytes([2, 97, 98, 99]), bytes([0, 7, 7 << 5, 255, 0])):
        try:
            lzf_decode(data, 2)
            raise AssertionError("LZF output exceeded the declared tile size")
        except ValueError as error:
            assert "exceeds the tile size" in str(error)
    # one 2x1 tile at (64, 0), stored (flag 0): planes B, G, R, A of two pixels; only the second pixel is opaque
    block = b"\x00" + bytes([30, 31]) + bytes([20, 21]) + bytes([10, 11]) + bytes([0, 255])
    data = b"VERSION 2\nTILEWIDTH 2\nTILEHEIGHT 1\nPIXELSIZE 4\nDATA 1\n64,0,LZF,%d\n" % len(block) + block
    size, tiles = kra_tile_table(data)
    rect, planes = kra_layer_pixels(data, size, tiles)
    assert (size, rect) == ((2, 1, 4), (65, 0, 66, 1)), (size, rect)
    assert [bytes(p) for p in planes] == [b"\x0b", b"\x15", b"\x1f", b"\xff"], planes  # R, G, B, A of the opaque pixel
    # the same tile with an opaque default pixel over a 4x1 canvas: the gap from 4 to 64 takes that pixel too
    rect, planes = kra_layer_pixels(data, size, tiles, ((1, 2, 3, 255), (0, 0, 4, 1)))
    assert rect == (0, 0, 66, 1), rect
    assert (bytes(planes[0]), bytes(planes[3])) == (b"\x01" * 64 + b"\x0a\x0b", b"\xff" * 64 + b"\x00\xff"), planes
    kra_self_test_checks()


def kra_self_test_checks():
    """The verdict rules on small documents built here from bytes: tile comparison, shown layers, limits, the judge."""
    import functools
    import io
    import stat

    def tiles(*placed):
        """Tile file of 2x1 tiles; placed = (x, y, alpha) with one colour. Stored blocks: B, G, R, A planes."""
        body = b"".join((b"%d,%d,LZF,%d\n" % (x, y, 9)) + b"\x00" + bytes([3, 3, 2, 2, 1, 1, a, a]) for x, y, a in placed)
        return b"VERSION 2\nTILEWIDTH 2\nTILEHEIGHT 1\nPIXELSIZE 4\nDATA %d\n" % len(placed) + body

    def node(kind, name, inner="", **attrs):
        values = {"colorspacename": "RGBA", "visible": "1", "opacity": "255", "compositeop": "normal", "x": "0", "y": "0",
                  "filename": name}
        values.update(attrs)
        text = " ".join('%s="%s"' % item for item in values.items())
        return '<layer nodetype="%s" name="%s" %s>%s</layer>' % (kind, name, text, inner and "<layers>%s</layers>" % inner)

    def kra(layers, members, size=(8, 8), bzip2=(), colour="RGBA"):
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as zf:
            zf.writestr("mimetype", b"application/x-krita")
            zf.writestr("maindoc.xml", '<DOC kritaVersion="5"><IMAGE name="d" width="%d" height="%d" colorspacename="%s" '
                        'profile="p"><layers>%s</layers></IMAGE></DOC>' % (size + (colour, "".join(layers))))
            for name, data in members.items():
                zf.writestr(name, data, zipfile.ZIP_BZIP2 if name in bzip2 else zipfile.ZIP_STORED)
        doc = parse_kra(out)
        for _, n in kra_walk(doc["layers"]):
            if n["type"] == "paintlayer" and n.get("entry") and n["cs"] == "RGBA":  # as verify_kra
                kra_layer_info(doc, n)
        return doc

    # Tile comparison: a tileless layer is not printed by the reader, a transparent one is printed with an empty rectangle
    # and the CRC-32 of no bytes (the rectangle alone would let a decoder difference through).
    doc = kra([node("paintlayer", "layer1"), node("paintlayer", "layer2"), node("paintlayer", "layer3")], {
        "d/layers/layer1": tiles((0, 0, 255)), "d/layers/layer2": tiles((64, 0, 0)), "d/layers/layer3": tiles()})
    data = doc["zf"].read("d/layers/layer1")
    size, placed = kra_tile_table(data)
    rect, planes = kra_layer_pixels(data, size, placed)
    crc = zlib.crc32(b"".join(bytes(p[k:k + 1]) for k in range(len(planes[0])) for p in planes))
    with tempfile.TemporaryDirectory(prefix="verify-real-files-self-test-") as work:
        def decode(empty_crc):
            script = os.path.join(work, "reader")
            with open(script, "w") as handle:
                handle.write("#!/bin/sh\necho '0 %d %d %d %d %08x'\necho '1 64 0 64 0 %s'\n" % (rect + (crc, empty_crc)))
            os.chmod(script, os.stat(script).st_mode | stat.S_IXUSR)
            return check_kra_decode(doc, "unused.kra", script)

        assert decode("00000000") == {"layers": 2, "pixels": 2, "mismatching_layers": []}, decode("00000000")
        assert decode("deadbeef")["mismatching_layers"] == [1], decode("deadbeef")

    # Default pixel (Krita writes B, G, R, A): a layer with a pixel that is not transparent covers the canvas, in the layer's
    # own pixels, joined with its tiles; every pixel no tile covers takes the pixel. Without one the layer is empty.
    default = {"d/layers/layer1": tiles((4, 0, 255)), "d/layers/layer1.defaultpixel": bytes([30, 20, 10, 255])}
    doc = kra([node("paintlayer", "layer1")], default)
    layer = doc["layers"][0]
    assert (layer["fill"], layer["tile_box"], layer["tile_pixels"]) == (((10, 20, 30, 255), (0, 0, 8, 8)), (0, 0, 8, 8), 64), layer
    rect, planes = kra_decode(doc, layer)
    assert rect == (0, 0, 8, 8) and [bytes(p[0:3]) for p in planes] == [b"\x0a\x0a\x0a", b"\x14\x14\x14", b"\x1e\x1e\x1e", b"\xff" * 3]
    assert [p[4] for p in planes] == [1, 2, 3, 255] and [p[5] for p in planes] == [1, 2, 3, 255]  # the tile's two pixels
    assert [p[6] for p in planes] == [10, 20, 30, 255]  # the next one is the default pixel
    for placed in [((-8192, 0, 255), (8192, 0, 255)), ((3, 0, 255), (2, 0, 128)),
                   ((2, 0, 128), (3, 0, 255))]:
        repeated = kra([node("paintlayer", "layer1")], {
            "d/layers/layer1": tiles(*placed), "d/layers/layer1.defaultpixel": bytes([3, 2, 1, 255])}, size=(4, 4))
        repeated_layer = repeated["layers"][0]
        assert repeated_layer["tile_box"] == (0, 0, 4, 4) and repeated_layer["tile_area"] == 4
        assert kra_limits_hit(repeated) == []
        repeated_rect, repeated_planes = kra_decode(repeated, repeated_layer)
        assert repeated_rect == (0, 0, 4, 4) and all(len(p) == 16 for p in repeated_planes)
        expected = 128 if placed[-1][2] == 128 else 255
        assert repeated_planes[3][3] == expected
    assert kra_expected_skipped(doc) is None and [e["kind"] for e in kra_expected(doc["layers"])] == ["part"]
    with tempfile.TemporaryDirectory(prefix="verify-real-files-self-test-") as work:  # the CRC path sees the pixel too
        rgba = bytearray(len(planes[0]) * 4)
        for k, plane in enumerate(planes):
            rgba[k::4] = plane
        script = os.path.join(work, "reader")
        with open(script, "w") as handle:
            handle.write("#!/bin/sh\necho '0 0 0 8 8 %08x'\n" % zlib.crc32(rgba))
        os.chmod(script, os.stat(script).st_mode | stat.S_IXUSR)
        assert check_kra_decode(doc, "unused.kra", script) == {"layers": 1, "pixels": 64, "mismatching_layers": []}
    # A layer at (3, 2) with a tile at (-3, -2): the canvas is (-3, -2) to (5, 6) in its pixels.
    moved = kra([node("paintlayer", "layer1", x="3", y="2")], {"d/layers/layer1": tiles((-3, -2, 255)),
                                                              "d/layers/layer1.defaultpixel": bytes([30, 20, 10, 255])})
    assert kra_decode(moved, moved["layers"][0])[0] == (-3, -2, 5, 6) and moved["layers"][0]["tile_pixels"] == 64
    # No tiles: the pixel alone makes the layer. Transparent, missing and wrong-sized pixels leave it empty and unreported.
    for pixel, node_made in ((bytes([30, 20, 10, 255]), True), (bytes([30, 20, 10, 128]), True), (bytes([30, 20, 10, 0]), False),
                             (None, False), (bytes([30, 20, 10]), False), (bytes([30, 20, 10, 255, 1]), False)):
        members = {"d/layers/layer1": tiles()}
        if pixel is not None:
            members["d/layers/layer1.defaultpixel"] = pixel
        bare = kra([node("paintlayer", "layer1")], members)
        assert [e["kind"] for e in kra_expected(bare["layers"])] == (["part"] if node_made else []), pixel
        assert kra_expected_skipped(bare) is None and bare["layers"][0]["tile_pixels"] == (64 if node_made else 0), pixel
    # Two layers of the canvas size exceed a pixel limit of 100 together, as in the reader's budget.
    saved_pixels = KRA_MAX_PIXELS
    globals()["KRA_MAX_PIXELS"] = 100
    try:
        both = kra([node("paintlayer", "layer1"), node("paintlayer", "layer2")], dict(default, **{
            "d/layers/layer2": tiles(), "d/layers/layer2.defaultpixel": bytes([1, 2, 3, 255])}))
        assert [label for label, _ in kra_limits_hit(both)] == ["layer tile pixels (100)"], kra_limits_hit(both)
        assert kra_limits_hit(kra([node("paintlayer", "layer1")], default)) == []
    finally:
        globals()["KRA_MAX_PIXELS"] = saved_pixels

    # Shown layers: an unsupported node under a hidden or fully transparent group cannot reach the merged image.
    svg = {"d/layers/layer9.shapelayer/content.svg": b"<svg/>", "d/layers/layer8.shapelayer/content.svg": b""}
    vector = node("shapelayer", "layer9") + node("shapelayer", "layer8")
    for outer, shown in ((node("grouplayer", "g", vector, visible="0"), 0), (node("grouplayer", "g", vector, opacity="0"), 0),
                         (node("grouplayer", "g", vector), 2)):
        info = classify_kra(kra([outer], svg))
        assert (info["unsupported_visible"], info["unsupported_with_content"]) == (shown, shown // 2), (shown, info)

    # Refusals must name the limit they belong to, not just a number that is also another limit's number.
    def refused(error, hits):
        rec = {"file": "x.kra", "status": "refused", "error": error, "colour_unsupported": False, "limits_exceeded": hits}
        return judge({"psd": [], "jpeg": [], "kra": [rec], "summary": {}}, argparse.Namespace())
    canvas = kra_limits_hit(kra([], {}, (20000, 20000)))
    assert [label for label, _ in canvas] == ["canvas pixels (100000000)"], canvas
    assert not refused("Invalid KRA document: the canvas size is out of range (limit 100000000 pixels)", canvas)
    assert refused("the layer's tile box exceeds the memory budget (limit 100000000 pixels)", canvas)

    # Layer count: every group is two records for the reader, and a node it does not read is none.
    labels = lambda doc: [label for label, _ in kra_limits_hit(doc)]
    assert labels(kra([node("grouplayer", "g")] * (KRA_MAX_LAYERS // 2), {})) == []
    assert labels(kra([node("grouplayer", "g")] * (KRA_MAX_LAYERS // 2 + 1), {})) == ["layer count (32768)"]
    assert labels(kra([node("shapelayer", "s")] * (KRA_MAX_LAYERS + 1), {})) == []

    # Group depth: 49 nested groups are read, the 50th takes the reader past its 48 levels.
    nest = lambda count: kra([functools.reduce(lambda inner, _: node("grouplayer", "g", inner), range(count), "")], {})
    assert labels(nest(KRA_MAX_GROUP_DEPTH + 1)) == [], labels(nest(KRA_MAX_GROUP_DEPTH + 1))
    assert labels(nest(KRA_MAX_GROUP_DEPTH + 2)) == ["group depth (48)"], labels(nest(KRA_MAX_GROUP_DEPTH + 2))

    # Pairing: a fully transparent layer has no node, also when the layer beside it has its name and does have one.
    for alphas in ((0, 255), (255, 0)):
        pair = kra([node("paintlayer", "same", filename="layer%d" % k) for k in (1, 2)],
                   {"d/layers/layer%d" % k: tiles((0, 0, alpha)) for k, alpha in zip((1, 2), alphas)})
        rects = {}
        for _, n in kra_walk(pair["layers"]):
            data = pair["zf"].read(n["entry"])
            rects[id(n)] = kra_layer_pixels(data, *kra_tile_table(data))[0]
        mism = dict.fromkeys(("structure", "name", "enabled", "opacity", "blend", "position", "size"), 0)
        state = {"empty_dropped": 0, "group_blend_dropped": 0, "exact": 0, "bounds_only": 0,
                 "empty": lambda n: rects[id(n)] is None, "rect": lambda n: rects[id(n)]}
        part = {"type": "Part", "name": "same", "enabled": True, "opacity": 1.0, "blend_mode": "Normal",
                "transform": {"trans": [-3.0, -4.0]}, "mesh": {"verts": [0, 0, 0, 0, 1]}}
        kra_compare_level(pair, kra_expected(pair["layers"]), [part], (0.0, 0.0), mism, state)
        assert (state["empty_dropped"], state["exact"], set(mism.values())) == (1, 1, {0}), (alphas, state, mism)
        # The bitmap pairing skips it too, so the Part is paired with the painted layer and its texture is checked.
        paired = kra_match_parts(pair, {"children": [part]}, state["empty"])
        painted = "layer%d" % (alphas.index(255) + 1)
        assert [n["file"] for _, n in paired] == [painted], (alphas, [n["file"] for _, n in paired])

    # Bitmap picks: each eligible layer once when there are fewer than asked for, an even spread otherwise.
    for count, want in ((0, []), (1, [0]), (2, [0, 1]), (3, [0, 1, 2]), (4, [0, 1, 2, 3]), (9, [0, 2, 5, 8])):
        assert spread_picks(count, 4) == want, (count, spread_picks(count, 4), want)

    # Archive budget: maindoc.xml has its own cap and, with mimetype, counts towards the total.
    one = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255))})
    header_bytes = sum(one["zf"].getinfo(name).file_size for name in ("mimetype", "maindoc.xml"))
    saved = KRA_MAX_XML_BYTES, KRA_MAX_TOTAL_BYTES, KRA_MAX_ENTRIES
    try:
        globals()["KRA_MAX_ENTRIES"] = len(one["zf"].infolist()) - 1
        entry_hits = kra_limits_hit(one)
        assert [label for label, _ in entry_hits] == ["archive entries (%d)" % KRA_MAX_ENTRIES], entry_hits
        assert not refused("Invalid KRA document: the archive has more than %d entries" % KRA_MAX_ENTRIES, entry_hits)
        globals()["KRA_MAX_ENTRIES"] = saved[2]
        globals()["KRA_MAX_XML_BYTES"] = one["zf"].getinfo("maindoc.xml").file_size - 1
        xml_hits = kra_limits_hit(one)
        assert [label for label, _ in xml_hits] == ["maindoc.xml size (%d bytes)" % KRA_MAX_XML_BYTES], xml_hits
        assert not refused("'maindoc.xml' is larger than the supported size (limit %d bytes inflated)" % KRA_MAX_XML_BYTES, xml_hits)
        assert refused("'d/layers/layer1' is larger than the supported size (limit 134217728 bytes inflated)", xml_hits)
        globals()["KRA_MAX_XML_BYTES"] = saved[0]
        globals()["KRA_MAX_TOTAL_BYTES"] = header_bytes + one["layers"][0]["inflated"]
        assert labels(one) == []
        globals()["KRA_MAX_TOTAL_BYTES"] -= 1
        assert labels(one) == ["inflated archive size (%d bytes)" % KRA_MAX_TOTAL_BYTES], labels(one)
        # The reader also opens a 4-byte default pixel entry, and leaves one of any other size unread.
        for pixel, counted in ((bytes([30, 20, 10, 255]), 4), (bytes(5), 0)):
            filled = kra([node("paintlayer", "layer1")],
                         {"d/layers/layer1": tiles((0, 0, 255)), "d/layers/layer1.defaultpixel": pixel})
            globals()["KRA_MAX_TOTAL_BYTES"] = header_bytes + filled["layers"][0]["inflated"] + counted
            assert labels(filled) == [], (len(pixel), labels(filled))
            globals()["KRA_MAX_TOTAL_BYTES"] -= 1
            assert labels(filled) == ["inflated archive size (%d bytes)" % KRA_MAX_TOTAL_BYTES], (len(pixel), labels(filled))
    finally:
        globals()["KRA_MAX_XML_BYTES"], globals()["KRA_MAX_TOTAL_BYTES"], globals()["KRA_MAX_ENTRIES"] = saved

    # An entry over the reader's limit: the declared size explains the refusal, and the entry is not inflated here.
    big, saved_entry = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255))}), KRA_MAX_ENTRY_BYTES
    assert big["layers"][0].pop("tiles") == 1
    globals()["KRA_MAX_ENTRY_BYTES"] = big["layers"][0]["inflated"] - 1
    try:
        assert kra_layer_info(big, big["layers"][0]) is None and "tiles" not in big["layers"][0]
        assert check_kra_decode(big, "unused", "unused")["skipped_layers"] == 1
        assert labels(big)[0] == "inflated entry size (%d bytes)" % KRA_MAX_ENTRY_BYTES, labels(big)
    finally:
        globals()["KRA_MAX_ENTRY_BYTES"] = saved_entry

    # A header entry over the reader's limit is not inflated: the parse stops on the declared size.
    huge = io.BytesIO()
    with zipfile.ZipFile(huge, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("mimetype", b"\0" * 65)
        zf.writestr("maindoc.xml", b"<DOC/>")
    try:
        parse_kra(huge)
        raise AssertionError("an oversized mimetype was read")
    except ValueError as error:
        assert "'mimetype' is larger than the reader's limit of 64 bytes" in str(error), error

    # Two small tiles far apart: the refusal is explained by the tile box, which must not be decoded here.
    far = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255), (4_000_000, 4_000_000, 255))})
    real_decode, real_pixels = globals()["kra_cropped_box"], globals()["kra_layer_pixels"]
    globals()["kra_cropped_box"] = lambda *_: (_ for _ in ()).throw(AssertionError("decoded an oversized tile box"))
    try:
        hits = kra_limits_hit(far)
    finally:
        globals()["kra_cropped_box"] = real_decode
    assert [label for label, _ in hits] == ["layer tile pixels (100000000)"], hits
    assert not refused("the layer's tile box exceeds the memory budget (limit 100000000 pixels)", hits)
    assert refused("layer \"p\" is 9000x9000, larger than the supported texture size (at most 8192 per side and 16777216 pixels)", hits)

    # The same file on the CRC path: neither the reader example nor this script's decoder may compose the tile box.
    with tempfile.TemporaryDirectory(prefix="verify-real-files-self-test-") as work:
        script = os.path.join(work, "reader")
        with open(script, "w") as handle:
            handle.write("#!/bin/sh\necho '0 0 0 2 1 00000000'\n")
        os.chmod(script, os.stat(script).st_mode | stat.S_IXUSR)
        globals()["kra_layer_pixels"] = lambda *_: (_ for _ in ()).throw(AssertionError("composed an oversized tile box"))
        try:
            skipped = check_kra_decode(far, "unused.kra", script)
        finally:
            globals()["kra_layer_pixels"] = real_pixels
        assert skipped == {"layers": 0, "pixels": 0, "mismatching_layers": [], "skipped_layers": 1}, skipped

    # Archive features the reader refuses by name; zipfile opens every one of them.
    def end_record(zdoc, field, raw):
        with zdoc["zf"].fp.getbuffer() as buf:
            at = bytes(buf).rfind(b"PK\5\6")
            buf[at + field:at + field + len(raw)] = raw
        return zdoc
    plain = lambda **kw: kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255))}, **kw)
    features = lambda zdoc: [label for label, _ in kra_zip_features(zdoc)]
    assert features(plain()) == []
    packed = plain(bzip2=("d/layers/layer1",))
    bzip_hits = kra_limits_hit(packed)
    assert [label for label, _ in bzip_hits] == ["compression method 12"], bzip_hits
    assert not refused("'d/layers/layer1' uses unsupported ZIP compression method 12", bzip_hits)
    assert refused("'d/layers/layer1' uses unsupported ZIP compression method 99", bzip_hits)
    assert refused("Invalid KRA document: bad ZIP local header", bzip_hits)
    # a method on an entry the reader does not open explains nothing
    unread = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255)), "mergedimage.png": b"x"},
                 bzip2=("mergedimage.png",))
    assert features(unread) == []
    # the default pixel entry is opened when it is 4 bytes, so its method counts then and only then
    for pixel, want in ((bytes([30, 20, 10, 255]), ["compression method 12"]), (bytes(5), [])):
        filled = kra([node("paintlayer", "layer1")],
                     {"d/layers/layer1": tiles((0, 0, 255)), "d/layers/layer1.defaultpixel": pixel},
                     bzip2=("d/layers/layer1.defaultpixel",))
        assert features(filled) == want, (len(pixel), features(filled))
    assert features(end_record(plain(), 4, b"\1\0")) == ["multi-disk archive"]
    assert features(end_record(plain(), 16, b"\xff" * 4)) == ["ZIP64 archive"]
    encrypted = plain()
    encrypted["zf"].infolist()[-1].flag_bits |= 1
    assert features(encrypted) == ["encrypted entries"]
    # 65535 entries: the reader checks ZIP64 before the entry count, so it names ZIP64 although the count is also over
    many = kra([], {"x%d" % k: b"" for k in range(0xFFFF - 2)})
    many_hits = kra_limits_hit(many)
    assert sorted(label for label, _ in many_hits) == ["ZIP64 archive", "archive entries (16384)"], many_hits
    assert not refused("Invalid KRA document: ZIP64 archives are not supported", many_hits)
    assert not refused("Invalid KRA document: the archive has more than 16384 entries", many_hits)

    # Crafted archives that the reader refuses, each run through verify_kra with the reader's refusal as the reply. The
    # messages are what `import.kra` of a debug core answered for the same bytes; the judge must accept them and not the
    # others. `tiles_of(count)` is a tile file of `count` tiles of 1024x1024 with one flag byte of data each, all at
    # (0, 0): the box is one tile, the tile area is count * 1,048,576 pixels.
    tiles_of = lambda count: (b"VERSION 2\nTILEWIDTH 1024\nTILEHEIGHT 1024\nPIXELSIZE 4\nDATA %d\n" % count
                             + b"0,0,LZF,1\n\x00" * count)

    def verdict(content, message, work):
        """judge() of verify_kra on an archive (parsed, or its bytes) when the reader answers `message`, and the record."""
        path = os.path.join(work, "crafted.kra")
        with open(path, "wb") as handle:
            handle.write(content if isinstance(content, bytes) else content["zf"].fp.getvalue())
        rec = verify_kra(argparse.Namespace(call=lambda request: (None, message)), path, argparse.Namespace(kra_crc_example=None))
        return judge({"psd": [], "jpeg": [], "kra": [rec], "summary": {}}, argparse.Namespace()), rec

    # An archive this script cannot read fails the run by name and does not stop it: not a ZIP, and no maindoc.xml.
    with tempfile.TemporaryDirectory(prefix="verify-real-files-self-test-") as work:
        broken, no_xml = os.path.join(work, "broken.kra"), io.BytesIO()
        with zipfile.ZipFile(no_xml, "w") as zf:
            zf.writestr("mimetype", b"application/x-krita")
        for content in (b"not a zip", no_xml.getvalue()):
            with open(broken, "wb") as handle:
                handle.write(content)
            rec = verify_kra_file(None, broken, argparse.Namespace(kra_crc_example=None))
            assert rec["status"] == "unchecked" and work not in rec["error"], rec
            problems = judge({"psd": [], "jpeg": [], "kra": [rec], "summary": {}}, argparse.Namespace())
            assert len(problems) == 1 and problems[0].startswith("KRA broken.kra could not be checked by this script: "), problems

    with tempfile.TemporaryDirectory(prefix="verify-real-files-self-test-") as work:
        # 1. A ZIP64 entry with small sizes: the directory stores 0xFFFFFFFF (the real sizes are in the ZIP64 extra field,
        # which is where zipfile takes them from, so it shows an ordinary entry) and the reader refuses on the stored value.
        info = zipfile.ZipInfo("mergedimage.png")
        info.extra = struct.pack("<HHQQ", 1, 16, 1, 1)
        sentinel = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255)), info: b"x"})
        with sentinel["zf"].fp.getbuffer() as buf:
            at = bytes(buf).rfind(b"PK\1\2")  # the last directory entry
            buf[at + 20:at + 28] = b"\xff" * 8
        problems, rec = verdict(sentinel, "Invalid KRA document: ZIP64 archives are not supported", work)
        assert not problems and [label for label, _ in rec["limits_exceeded"]] == ["ZIP64 archive"], (problems, rec)
        assert zipfile.ZipFile(os.path.join(work, "crafted.kra")).getinfo("mergedimage.png").file_size == 1  # zipfile is fooled

        # 2. Tile count times tile area: 96 tiles of 1,048,576 pixels at one place are over 100,000,000 pixels, 95 are not,
        # and a tile of length zero is not counted.
        zero = tiles_of(95).replace(b"DATA 95", b"DATA 100") + b"0,0,LZF,0\n" * 5
        for count, body, hits in ((96, tiles_of(96), ["layer tile pixels (100000000)"]), (95, tiles_of(95), []), (95, zero, [])):
            doc = kra([node("paintlayer", "layer1")], {"d/layers/layer1": body})
            assert [label for label, _ in kra_limits_hit(doc)] == hits, (count, kra_limits_hit(doc))
        problems, rec = verdict(kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles_of(97)}),
                                "layer \"layer1\": the layer's tiles exceed the memory budget (limit 100000000 pixels)", work)
        assert not problems and rec["layers"][0]["tile_pixels"] == 1024 * 1024, (problems, rec["layers"])

        # 3. A colour model the reader refuses, together with something it checks first: the first check is the one whose
        # message must appear. Entries are counted before the XML is read, so before any colour model; the canvas comes after
        # the document's colour space and before the layers; a layer's colour is checked before its tile file is opened.
        colour = "Support for 8-bit Integer documents only."
        entries = "Invalid KRA document: the archive has more than 16384 entries"
        canvas = "Invalid KRA document: the canvas size is out of range (limit 100000000 pixels)"
        tile = "layer \"layer1\": the layer's tiles exceed the memory budget (limit 100000000 pixels)"
        many = {"x%d" % k: b"" for k in range(KRA_MAX_ENTRIES + 1)}
        first, grey = {"d/layers/layer1": tiles_of(97)}, node("paintlayer", "layer2", colorspacename="GRAYA")
        for label, archive, expected in (
                ("document colour, entries", kra([], dict(many), colour="GRAYA"), entries),
                ("layer colour, entries", kra([grey], dict(many)), entries),
                ("layer colour, canvas", kra([grey], {}, (20000, 20000)), canvas),
                ("limit before the layer colour", kra([node("paintlayer", "layer1"), grey], first), tile),
                ("document colour, canvas", kra([], {}, (20000, 20000), colour="GRAYA"), colour),
                ("limit after the layer colour", kra([grey, node("paintlayer", "layer1")], first), colour),
                ("layer colour alone", kra([grey], {}), colour)):
            for message in (entries, canvas, tile, colour):
                problems, rec = verdict(archive, message, work)
                assert bool(problems) == (message != expected), (label, message, problems, rec["limits_exceeded"])
            assert bool(rec["limits_exceeded"]) == (expected != colour), (label, rec["limits_exceeded"])
        # The group that holds the colour refusal has not written its end record: 2 * 16383 + 1 records are no more than
        # the limit, one more closed group is.
        closed = KRA_MAX_LAYERS // 2 - 1
        assert labels(kra([node("grouplayer", "g")] * closed + [node("grouplayer", "g", grey)], {})) == []
        assert labels(kra([node("grouplayer", "g")] * (closed + 1) + [node("grouplayer", "g", grey)], {})) == ["layer count (32768)"]

        # 4. A header entry over the reader's limit is recorded and judged, and the run goes on.
        def archive_with(mimetype, xml):
            out = io.BytesIO()
            with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
                zf.writestr("mimetype", mimetype)
                zf.writestr("maindoc.xml", xml)
            return out.getvalue()
        for label, content, message in (
                ("mimetype", archive_with(b"application/x-krita" + b" " * 46, b"<DOC/>"),
                 "'mimetype' is larger than the supported size (limit 64 bytes inflated)"),
                ("maindoc.xml", archive_with(b"application/x-krita", b"<DOC/>" + b" " * KRA_MAX_XML_BYTES),
                 "'maindoc.xml' is larger than the supported size (limit 16777216 bytes inflated)")):
            problems, rec = verdict(content, message, work)
            assert not problems and rec["status"] == "refused" and len(rec["limits_exceeded"]) == 1, (label, problems, rec)
            assert verdict(content, "Invalid document: no file 'x'", work)[0], label  # another refusal is a problem

    # The judge: a render difference over the limit is a failure unless shown unsupported content explains it, and then it
    # is reported as not comparable; a blank document must not be asked for painted pixels.
    args = argparse.Namespace(max_render_diff=3.5, max_layer_diff=0)

    def imported(diff, with_content, expects, painted, skipped=None, blends=None):
        rec = {"file": "x.kra", "status": "imported", "expected": {"paint_with_tiles": 0, "groups": 0}, "mismatches": {},
               "skipped_layers": {"expected": skipped, "reported": None},
               "blend_modes_not_applied": {"expected": blends, "reported": None},
               "actual": {"parts": 0, "groups": 0}, "empty_layers_without_node": 0, "layer_bitmaps": [],
               "classify": {"unsupported_with_content": with_content}, "expects_paint": expects,
               "render": {"painted_fraction": painted, "mean_abs_diff": diff}}
        report = {"psd": [], "jpeg": [], "kra": [rec], "summary": {}}
        return judge(report, args), report.get("not_comparable", [])
    assert imported(9.0, 0, True, 0.5) == (["KRA x.kra render differs from mergedimage.png by 9.0 (limit 3.5) and no shown layer "
                                            "with content was left out"], [])
    bad, unclear = imported(9.0, 1, True, 0.5)
    assert not bad and len(unclear) == 1, (bad, unclear)
    assert imported(0.0, 0, False, 0.0) == ([], [])  # blank: nothing to paint
    assert imported(0.0, 0, True, 0.0)[0] == ["KRA x.kra render painted no pixels"]
    assert imported(0.0, 0, False, 0.0, {"visible": True, "types": {"shapelayer": 1}})[0] == [
        "KRA x.kra reports skipped layers None, the layer tree gives {'visible': True, 'types': {'shapelayer': 1}}"]
    assert imported(0.0, 0, False, 0.0, blends={"count": 1, "modes": {"hue": 1}, "other": 0})[0] == [
        "KRA x.kra reports blend modes not applied None, the layer tree gives {'count': 1, 'modes': {'hue': 1}, 'other': 0}"]

    # Skipped layers: every node that is not a paint or group layer, by type. A paint layer without tiles is not.
    mixed = kra([node("shapelayer", "v", visible="0"), node("grouplayer", "g", node("filelayer", "f") + node("paintlayer", "e")),
                 node("paintlayer", "p")], {"d/layers/e": tiles(), "d/layers/p": tiles((0, 0, 255))})
    for _, n in kra_walk(mixed["layers"]):
        if n["type"] == "paintlayer":
            kra_layer_info(mixed, n)
    want = {"visible": True, "types": {"shapelayer": 1, "filelayer": 1}}
    assert kra_expected_skipped(mixed) == want, kra_expected_skipped(mixed)
    assert kra_expected_skipped(one) is None and kra_reported_skipped({}) is None
    said = {"visible": True, "other": 0, "types": [{"type": t, "count": 1} for t in ("shapelayer", "filelayer")]}
    assert kra_reported_skipped({"skippedLayers": said}) == want
    assert kra_reported_skipped({"skippedLayers": dict(said, other=2)}) != want

    # Blend modes the importer lacks: layers and groups it keeps, by id as it names it; a pass-through group and an empty
    # layer do not count; a 100-character id is cut to 64 and a control character dropped.
    odd = kra([node("paintlayer", "a", compositeop="linear_burn"), node("paintlayer", "b", compositeop="linear_burn"),
               node("paintlayer", "e", compositeop="hue"), node("paintlayer", "m", compositeop="multiply"),
               node("grouplayer", "g", node("paintlayer", "c", compositeop="x" * 100), compositeop="hard_mix"),
               node("grouplayer", "t", node("paintlayer", "n", compositeop="a&#9;b"), compositeop="hue", passthrough="1")],
              {"d/layers/a": tiles((0, 0, 255)), "d/layers/b": tiles((0, 0, 255)), "d/layers/m": tiles((0, 0, 255)),
               "d/layers/c": tiles((0, 0, 255)), "d/layers/n": tiles((0, 0, 255)), "d/layers/e": tiles()})
    lost = {"count": 5, "modes": {"linear_burn": 2, "x" * 64: 1, "hard_mix": 1, "ab": 1}, "other": 0}
    assert kra_expected_blends(odd) == lost, kra_expected_blends(odd)
    said = {"count": 5, "other": 0, "modes": [{"mode": m, "count": c} for m, c in lost["modes"].items()]}
    assert kra_reported_blends({"blendModesNotApplied": said}) == lost
    assert kra_reported_blends({"blendModesNotApplied": dict(said, other=1)}) != lost
    assert kra_expected_blends(one) is None and kra_reported_blends({}) is None
    many = kra([node("paintlayer", "a", compositeop="op%02d" % k) for k in range(18)], {"d/layers/a": tiles((0, 0, 255))})
    bounded = kra_expected_blends(many)
    assert (bounded["count"], len(bounded["modes"]), bounded["other"]) == (18, 16, 2), bounded

    # Group blend: a Composite must carry the mapped mode (Normal for a pass-through group), a plain Node is counted.
    def group_blend(op, actual, composites=0, inner=None, **attrs):
        grouped = kra([node("grouplayer", "g", node("paintlayer", "p", **(inner or {})), compositeop=op, **attrs)],
                      {"d/layers/p": tiles((0, 0, 255))})
        for _, n in kra_walk(grouped["layers"]):
            if n["type"] == "paintlayer":
                kra_layer_info(grouped, n)
        mism = dict.fromkeys(("structure", "name", "enabled", "opacity", "blend", "position", "size"), 0)
        state = {"empty_dropped": 0, "group_blend_dropped": 0, "exact": 0, "bounds_only": 0,
                 "empty": lambda n: True, "rect": lambda n: None}
        act = dict({"name": "g", "enabled": True, "opacity": 1.0, "transform": {"trans": [0.0, 0.0]}, "children": []}, **actual)
        kra_compare_level(grouped, kra_expected(grouped["layers"]), [act], (0.0, 0.0), mism, state, composites)
        return mism["blend"], state["group_blend_dropped"], sum(mism.values()) - mism["blend"]
    composite = {"type": "DynamicComposite", "blend_mode": "Multiply"}
    assert group_blend("multiply", composite) == (0, 0, 0)
    assert group_blend("lighten", dict(composite, blend_mode="Lighten")) == (0, 0, 0)
    assert group_blend("multiply", dict(composite, blend_mode="Normal")) == (1, 0, 0)
    assert group_blend("normal", composite) == (1, 0, 0)
    assert group_blend("multiply", composite, passthrough="1") == (1, 0, 0)
    assert group_blend("multiply", dict(composite, blend_mode="Normal"), passthrough="1") == (0, 0, 0)
    # A plain Node is counted, and it is a wrong type unless the group is a pass-through group that holds a blended layer
    # at full opacity, or lies below the Composite nesting limit (the third number counts the other mismatches).
    assert group_blend("multiply", {"type": "Node"}) == (0, 1, 1)
    assert group_blend("normal", {"type": "Node"}) == (0, 0, 1)
    blended = {"compositeop": "multiply"}
    plain = {"type": "Node"}
    assert group_blend("normal", plain, inner=blended, passthrough="1") == (0, 0, 0)
    assert group_blend("multiply", plain, inner=blended, passthrough="1") == (0, 0, 0)  # the group's own op is not applied
    assert group_blend("normal", dict(composite, blend_mode="Normal"), inner=blended, passthrough="1") == (0, 0, 1)
    assert group_blend("normal", plain, passthrough="1") == (0, 0, 1)  # nothing blended below the group
    assert group_blend("normal", dict(composite, blend_mode="Normal"), passthrough="1") == (0, 0, 0)
    assert group_blend("normal", plain, inner=blended) == (0, 0, 1)  # an isolated group is a Composite
    half = {"opacity": "128"}
    assert group_blend("normal", dict(plain, opacity=128 / 255), inner=blended, passthrough="1", **half) == (0, 0, 1)
    assert group_blend("normal", dict(composite, blend_mode="Normal", opacity=128 / 255), inner=blended,
                       passthrough="1", **half) == (0, 0, 0)
    assert group_blend("normal", plain, composites=KRA_MAX_COMPOSITE_DEPTH) == (0, 0, 0)
    assert group_blend("normal", dict(composite, blend_mode="Normal"), composites=KRA_MAX_COMPOSITE_DEPTH) == (0, 0, 1)
    assert group_blend("normal", plain, composites=KRA_MAX_COMPOSITE_DEPTH - 1) == (0, 0, 1)
    multiply = {"op": "multiply", "passthrough": False}
    assert kra_blends_below([{"node": {"op": "normal", "passthrough": False}, "children": [{"node": multiply, "children": []}]}])
    assert not kra_blends_below([{"node": dict(multiply, passthrough=True), "children": []}])

    # What expects_paint looks at: an opaque merged image, or a shown paint layer that is not empty.
    paint = kra([node("paintlayer", "layer1")], {"d/layers/layer1": tiles((0, 0, 255))})
    blank = kra([node("paintlayer", "layer1"), node("grouplayer", "g", node("paintlayer", "layer2"), visible="0")],
                {"d/layers/layer1": tiles((0, 0, 0)), "d/layers/layer2": tiles((0, 0, 255))})
    transparent = [b"\0"] * 4
    assert kra_expects_paint(kra_expected(paint["layers"]), lambda n: False, transparent) is True
    assert kra_expects_paint(kra_expected(blank["layers"]), lambda n: n["name"] == "layer1", transparent) is False
    assert kra_expects_paint(kra_expected(blank["layers"]), lambda n: n["name"] == "layer1", None) is False
    assert kra_expects_paint(kra_expected(blank["layers"]), lambda n: True, [b"\0"] * 3 + [b"\xff"]) is True  # visible reference


def check_kra_decode(doc, path, example):
    """Every paint layer decoded here against the reader with raised limits (examples/kra-layer-crc.rs): rectangle and
    CRC-32 of the straight-alpha RGBA bytes. This checks the tile decoder alone, so the importer's limits do not apply."""
    paint = [n for _, n in kra_walk(doc["layers"]) if n["type"] == "paintlayer" and kra_reads(n)]
    # The example raises the pixel limit and composes each layer's whole tile box, as this script does below, and it
    # refuses the archive features above whatever the limits are; neither is run on a file that has one of them.
    if kra_zip_features(doc) or any(n["tile_pixels"] > KRA_MAX_PIXELS for n in paint) or any(
            (n.get("inflated") or 0) > KRA_MAX_ENTRY_BYTES for n in kra_paint(doc)):
        # An entry that was not inflated has no tile count here, so it is counted as a layer that was not compared.
        unread = sum(1 for n in kra_paint(doc) if "tiles" not in n)
        return {"layers": 0, "pixels": 0, "mismatching_layers": [], "skipped_layers": len(paint) + unread}
    done = subprocess.run([example, path], capture_output=True, text=True, timeout=1800)
    if done.returncode != 0:
        return {"error": "reader failed: %s" % done.stderr.strip().replace(os.path.dirname(os.path.abspath(path)), "<dir>")}
    theirs = [line.split() for line in done.stdout.splitlines()]
    if len(theirs) != len(paint):
        return {"error": "reader found %d paint layers, this script %d" % (len(theirs), len(paint))}
    bad, pixels = [], 0
    for node, line in zip(paint, theirs):
        rect, planes = kra_decode(doc, node)
        got = tuple(int(v) for v in line[1:5]), line[5]
        if rect is None:
            # an empty layer has a zero-area rectangle and no bytes
            want = got[0][2] == got[0][0] and got[0][3] == got[0][1] and int(got[1], 16) == zlib.crc32(b"")
        else:
            rgba = bytearray(len(planes[0]) * 4)
            for k, plane in enumerate(planes):
                rgba[k::4] = plane
            absolute = (node["x"] + rect[0], node["y"] + rect[1], node["x"] + rect[2], node["y"] + rect[3])
            want = got[0] == absolute and int(got[1], 16) == zlib.crc32(rgba)
            pixels += (rect[2] - rect[0]) * (rect[3] - rect[1])
        if not want:
            bad.append(int(line[0]))
    return {"layers": len(paint), "pixels": pixels, "mismatching_layers": bad}


def verify_kra(core, path, args):
    rec = {"file": os.path.basename(path), "size": os.path.getsize(path)}
    started = time.time()
    try:
        doc = parse_kra(path)
    except KraHeaderTooLarge as error:
        # Nothing was inflated. The reader refuses the file by the same declared size, so the refusal is judged and the run goes on.
        _, err = core.call({"op": "import.kra", "path": path})
        stub = {"zf": error.zf, "layers": [], "width": 0, "height": 0, "cs": "RGBA"}
        rec.update(import_seconds=round(time.time() - started, 1), status="refused", error=err or "the importer did not refuse it",
                   leaks_directory=os.path.dirname(os.path.abspath(path)) in (err or ""), colour_unsupported=False,
                   limits_exceeded=kra_limits_hit(stub))
        return rec
    for _, node in kra_walk(doc["layers"]):
        if node["type"] == "paintlayer" and node.get("entry") and node["cs"] == "RGBA":
            kra_layer_info(doc, node)
    rec["classify"] = classify_kra(doc)
    rec["layers"] = [{"depth": d, "type": n["type"], "name": n["name"], "visible": n["visible"], "opacity": n["opacity"],
                      "op": n["op"], "cs": n["cs"], "offset": [n["x"], n["y"]], "inflated": n.get("inflated"),
                      "tiles": n.get("tiles"), "tile_pixels": n.get("tile_pixels")} for d, n in kra_walk(doc["layers"])]
    exp_tree = kra_expected(doc["layers"])
    exp_flat = [n for _, n in kra_walk(doc["layers"]) if n["type"] in KRA_SUPPORTED_NODES]
    rec["expected"] = {"records": len(exp_flat), "paint_with_tiles": sum(1 for n in exp_flat if kra_reads(n)),
                       "groups": sum(1 for n in exp_flat if n["type"] == "grouplayer")}
    rec["colour_unsupported"] = doc["cs"] != "RGBA" or any(
        n["type"] == "paintlayer" and n["cs"] != "RGBA" for _, n in kra_walk(doc["layers"]))
    if args.kra_crc_example and not rec["colour_unsupported"]:
        try:
            rec["tile_decode"] = check_kra_decode(doc, path, args.kra_crc_example)
        except (ValueError, IndexError, KeyError) as error:  # a tile table or block this script cannot follow
            rec["tile_decode"] = {"error": "this script could not decode the tiles: %r" % error}
    result, err = core.call({"op": "import.kra", "path": path})
    rec["import_seconds"] = round(time.time() - started, 1)
    if err:
        rec["status"], rec["error"] = "refused", err
        rec["leaks_directory"] = os.path.dirname(os.path.abspath(path)) in err
        rec["limits_exceeded"] = kra_limits_hit(doc)
        return rec
    rec["status"] = "imported"
    actual = list(walk_actual(result["model"]["nodes"]))
    rec["actual"] = {"parts": sum(1 for a in actual if a[1].get("type") == "Part"),
                     "groups": sum(1 for a in actual if a[1].get("type") != "Part"),
                     "textures": result.get("imported", {}).get("textures")}
    # exact crops for every layer that is small enough to decode in Python
    exact, cache = {}, {}
    for _, n in kra_walk(doc["layers"]):
        if n["type"] == "paintlayer" and kra_reads(n) and n["tile_pixels"] <= args.max_layer_pixels:
            rect, planes = kra_decode(doc, n)
            cache[id(n)] = (rect, planes)
            exact[id(n)] = rect
    mismatches = {"structure": 0, "name": 0, "enabled": 0, "opacity": 0, "blend": 0, "position": 0, "size": 0}
    state = {"empty_dropped": 0, "group_blend_dropped": 0, "exact": 0, "bounds_only": 0,
             "empty": lambda n: id(n) in exact and exact[id(n)] is None,
             "rect": lambda n: exact.get(id(n))}
    # a layer too big to decode cannot be proven empty: it is expected to exist
    kra_compare_level(doc, exp_tree, result["model"]["nodes"].get("children", []), (0.0, 0.0), mismatches, state)
    rec["mismatches"] = mismatches
    rec["empty_layers_without_node"] = state["empty_dropped"]
    rec["groups_with_blend_not_applied"] = state["group_blend_dropped"]
    rec["skipped_layers"] = {"expected": kra_expected_skipped(doc),
                             "reported": kra_reported_skipped(result.get("imported", {}))}
    rec["blend_modes_not_applied"] = {"expected": kra_expected_blends(doc),
                                      "reported": kra_reported_blends(result.get("imported", {}))}
    rec["position_checks"] = {"exact": state["exact"], "bounds_only": state["bounds_only"]}
    # layer bitmaps against the stored textures
    parts = [(a, n) for a, n in kra_match_parts(doc, result["model"]["nodes"], state["empty"]) if cache.get(id(n), (None, None))[0]]
    parts.sort(key=lambda p: (cache[id(p[1])][0][2] - cache[id(p[1])][0][0]) * (cache[id(p[1])][0][3] - cache[id(p[1])][0][1]))
    picks = [parts[i] for i in spread_picks(len(parts), args.layers)]
    bitmaps = []
    for act, n in picks:
        rect, planes = cache[id(n)]
        w, h = rect[2] - rect[0], rect[3] - rect[1]
        entry = {"name": n["name"], "size": [w, h]}
        tex, terr = core.call({"op": "texture.get", "slot": act["textures"][0]})
        if terr:
            entry["error"] = terr
        else:
            tw, th, tc, tpix = decode_png(base64.b64decode(tex["base64"]))
            if (tw, th) != (w, h) or tc != 4:
                entry["error"] = "size/channel mismatch (%dx%d, %d channels)" % (tw, th, tc)
            else:
                alpha = planes[3]
                worst = max((abs(x - y) for x, y in zip(alpha, tpix[3::4]) if x != y), default=0)
                for c in range(3):
                    mine = bytes(v * a // 255 for v, a in zip(planes[c], alpha))  # the importer stores premultiplied
                    worst = max(worst, max((abs(x - y) for x, y in zip(mine, tpix[c::4]) if x != y), default=0))
                entry["max_diff"] = worst
        bitmaps.append(entry)
    rec["layer_bitmaps"] = bitmaps
    merged, why = kra_merged(doc)
    rec["expects_paint"] = kra_expects_paint(exp_tree, state["empty"], merged)
    cdoc = {"width": doc["width"], "height": doc["height"], "transparency_first": bool(merged and len(merged) > 3)}
    rec["render"] = compare_render(core, cdoc, merged, args)
    if why:
        rec["render"]["compare"] = why
    return rec


def kra_match_parts(doc, root, empty):
    """(actual Part node, tree node) pairs for the imported Parts, matched in tree order like kra_compare_level does."""
    pairs = []

    def level(expected, actual_nodes):
        i = 0
        for exp in expected:
            if exp["kind"] == "part" and empty(exp["node"]):
                continue  # no node was made for it; the next layer may have the same name
            act = actual_nodes[i] if i < len(actual_nodes) else None
            if act is None or (act.get("type") == "Part") != (exp["kind"] == "part") or act["name"] != exp["node"]["name"]:
                continue  # an empty layer without a node, or a mismatch counted elsewhere
            i += 1
            if exp["kind"] == "group":
                level(exp["children"], act.get("children", []))
            else:
                pairs.append((act, exp["node"]))

    level(kra_expected(doc["layers"]), root.get("children", []))
    return pairs


# ---------------------------------------------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------------------------------------------


def collect(paths):
    psd, jpg, kra = [], [], []
    for p in paths:
        if os.path.isdir(p):
            for d, _, names in os.walk(p):
                for name in sorted(names):
                    full = os.path.join(d, name)
                    low = name.lower()
                    if low.endswith(".psd"):
                        psd.append(full)
                    elif low.endswith(".kra"):
                        kra.append(full)
                    elif low.endswith((".jpg", ".jpeg")) and not os.path.islink(full):
                        jpg.append(full)
        elif p.lower().endswith(".psd"):
            psd.append(p)
        elif p.lower().endswith(".kra"):
            kra.append(p)
        elif p.lower().endswith((".jpg", ".jpeg")):
            jpg.append(p)
    return sorted(psd), sorted(jpg), sorted(kra)


def verify_kra_file(core, path, args):
    """`verify_kra`, or a record that fails the run when this script cannot finish the check: the archive is not a ZIP,
    an entry is missing, a tile table it cannot follow, or a reply of the core it cannot decode. The error text names
    which. The run goes on with the next file."""
    try:
        return verify_kra(core, path, args)
    except (ValueError, KeyError, IndexError, struct.error, zipfile.BadZipFile, ET.ParseError, zlib.error) as error:
        text = "%s: %s" % (type(error).__name__, error)
        return {"file": os.path.basename(path), "status": "unchecked",
                "error": text.replace(os.path.dirname(os.path.abspath(path)), "<dir>")}


def judge(report, args):
    """Collects threshold violations; each is a short sentence."""
    bad = []
    not_compared = 0
    for r in report["psd"]:
        who = "PSD %s" % r["file"]
        if r["status"] != "imported":
            # Anything but 8-bit RGB is a documented refusal, provided the message names what it refuses.
            error = r.get("error", "")
            expected_refusal = (r["depth"] != 8 and "8-bit" in error) or (
                r["mode"] != 3 and ("colour mode" in error or "color mode" in error))
            if not expected_refusal:
                bad.append("%s was refused: %s" % (who, error))
            continue
        e, a = r["expected"], r["actual"]
        if (e["parts"], e["groups"]) != (a["parts"], a["groups"]):
            bad.append("%s has %d parts/%d groups, the PSD records give %d/%d" % (who, a["parts"], a["groups"], e["parts"], e["groups"]))
        for field, count in r["mismatches"].items():
            if count:
                bad.append("%s has %d %s mismatches" % (who, count, field))
        for entry in r["layer_bitmaps"]:
            if "error" in entry or entry["max_diff"] > args.max_layer_diff:
                bad.append("%s layer bitmap %r differs: %s" % (who, entry["name"], entry.get("error", entry.get("max_diff"))))
        rc = r["render"]
        if "error" in rc:
            bad.append("%s render failed: %s" % (who, rc["error"]))
        else:
            if "compare" in rc:
                not_compared += 1  # the script could not decode the merged image: this check did not run
            if rc["painted_fraction"] <= 0:
                bad.append("%s render painted no pixels" % who)
            if rc.get("mean_abs_diff", 0) > args.max_render_diff:
                bad.append("%s render differs from the merged image by %s (limit %s)" % (
                    who, rc["mean_abs_diff"], args.max_render_diff))
    kra_imported = kra_limit = kra_colour = 0
    for r in report["kra"]:
        who = "KRA %s" % r["file"]
        if r["status"] == "unchecked":
            bad.append("%s could not be checked by this script: %s" % (who, r["error"]))
            continue
        decode = r.get("tile_decode")
        if decode and ("error" in decode or decode["mismatching_layers"]):
            bad.append("%s tile decode differs from the independent decoder: %s" % (
                who, decode.get("error", "layers %s" % decode["mismatching_layers"])))
        if r["status"] != "imported":
            error = r.get("error", "")
            if r.get("leaks_directory"):
                bad.append("%s refusal leaks a directory path" % who)
            if r["limits_exceeded"]:
                # Limits found for a colour model the reader refuses are the ones it checks first; the colour message is not enough.
                kra_limit += 1
                if not any(re.search(pattern, error) for _, pattern in r["limits_exceeded"]):
                    bad.append("%s exceeds %s but the refusal does not name that limit: %s" % (
                        who, "; ".join(label for label, _ in r["limits_exceeded"]), error))
            elif r["colour_unsupported"]:
                kra_colour += 1
                if "8-bit" not in error:
                    bad.append("%s is refused for its colour model but the message does not say so: %s" % (who, error))
            else:
                bad.append("%s was refused although no limit or colour model explains it: %s" % (who, error))
            continue
        kra_imported += 1
        parts = r["expected"]["paint_with_tiles"] - r["empty_layers_without_node"]
        if r["actual"]["groups"] != r["expected"]["groups"] or r["actual"]["parts"] != parts:
            bad.append("%s has %d parts/%d groups, the layer tree gives %d/%d" % (
                who, r["actual"]["parts"], r["actual"]["groups"], parts, r["expected"]["groups"]))
        skipped = r["skipped_layers"]
        if skipped["expected"] != skipped["reported"]:
            bad.append("%s reports skipped layers %s, the layer tree gives %s" % (who, skipped["reported"], skipped["expected"]))
        blends = r["blend_modes_not_applied"]
        if blends["expected"] != blends["reported"]:
            bad.append("%s reports blend modes not applied %s, the layer tree gives %s" % (who, blends["reported"], blends["expected"]))
        for field, count in r["mismatches"].items():
            if count:
                bad.append("%s has %d %s mismatches" % (who, count, field))
        for entry in r["layer_bitmaps"]:
            if "error" in entry or entry["max_diff"] > args.max_layer_diff:
                bad.append("%s layer bitmap %r differs: %s" % (who, entry["name"], entry.get("error", entry.get("max_diff"))))
        rc = r["render"]
        if "error" in rc:
            bad.append("%s render failed: %s" % (who, rc["error"]))
            continue
        if "compare" in rc:
            not_compared += 1
        if rc["painted_fraction"] <= 0 and r["expects_paint"]:
            bad.append("%s render painted no pixels" % who)
        if rc.get("mean_abs_diff", 0) > args.max_render_diff:
            left_out = r["classify"]["unsupported_with_content"]
            message = "%s render differs from mergedimage.png by %s (limit %s)" % (who, rc["mean_abs_diff"], args.max_render_diff)
            if left_out:
                # The importer skips these layers silently and Krita's image shows them, and the tool has no composite of
                # the supported layers to tell their share from a regression: the render check is inconclusive, not passed.
                report.setdefault("not_comparable", []).append(
                    "%s; %d shown layer(s) of unsupported types with content were left out" % (message, left_out))
            else:
                bad.append(message + " and no shown layer with content was left out")
    report["summary"]["kra_imported"], report["summary"]["kra_refused_by_limit"] = kra_imported, kra_limit
    report["summary"]["kra_refused_for_colour_model"] = kra_colour
    report["summary"]["kra_crc_layers_skipped"] = sum(r.get("tile_decode", {}).get("skipped_layers", 0) for r in report["kra"])
    decoded = refused = 0
    for r in report["jpeg"]:
        who = "JPEG %s [%s]" % (r["file"], r["class"])
        if r["status"] == "refused":
            refused += 1
            if r["leaks_directory"]:
                bad.append("%s refusal leaks a directory path" % who)
            width, height = r["size"]
            if "not a supported texture size" in r["error"] and (
                    width > TEXTURE_SIDE_LIMIT or height > TEXTURE_SIDE_LIMIT or width * height > TEXTURE_PIXEL_LIMIT):
                continue  # refused by the importer's own size limits, which this run does not relax
            supported = (r["process"] in ("baseline", "extended", "progressive") and r["components"] in (1, 3)
                         and "multi-scan" not in r["class"] and r["class"].split()[2] == "8-bit")
            if supported:
                bad.append("%s is a supported stream but was refused: %s" % (who, r["error"]))
            elif not any(word in r["error"].lower() for word in ("component", "bit", "cmyk", "arithmetic", "lossless", "single-scan")):
                bad.append("%s refusal does not name the feature: %s" % (who, r["error"]))
            continue
        if r["status"] != "decoded":
            bad.append("%s: %s" % (who, r.get("error")))
            continue
        decoded += 1
        if "compare" in r:
            continue
        for name, limits in (("djpeg", (args.max_djpeg_mean, args.max_djpeg_peak, args.max_djpeg_outliers)),
                             ("sips", (args.max_sips_mean, None, args.max_sips_outliers))):
            d = r.get(name)
            if d is None or "unreliable" in d:
                continue
            if "error" in d:
                bad.append("%s vs %s: %s" % (who, name, d["error"]))
            elif d["mean_abs_diff"] > limits[0]:
                bad.append("%s mean difference from %s is %s (limit %s)" % (who, name, d["mean_abs_diff"], limits[0]))
            elif limits[1] is not None and d["max_diff"] > limits[1]:
                bad.append("%s largest difference from %s is %s (limit %s)" % (who, name, d["max_diff"], limits[1]))
            elif d["fraction_over_16"] > limits[2]:
                bad.append("%s has %.2f%% of samples more than 16 from %s" % (who, 100 * d["fraction_over_16"], name))
    report["summary"]["psd_not_compared"] = not_compared
    report["summary"]["jpeg_decoded"], report["summary"]["jpeg_refused"] = decoded, refused
    return bad


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0], fromfile_prefix_chars="@")
    ap.add_argument("paths", nargs="*", help="directories or files holding PSD, KRA and JPEG files")
    ap.add_argument("--core", default=os.path.join(ROOT, "out", "rust", "debug", "soul2d-core"))
    ap.add_argument("--out", help="report directory (default build/verification/real-files-<timestamp>)")
    ap.add_argument("--timeout", type=float, default=900, help="seconds allowed per core request")
    ap.add_argument("--max-jpeg", type=int, default=60, help="JPEG files to import (stratified by stream class)")
    ap.add_argument("--layers", type=int, default=4, help="layer bitmaps compared per PSD or KRA")
    ap.add_argument("--max-layer-pixels", type=int, default=1_500_000,
                    help="PSD layers and KRA tile boxes larger than this are not decoded here")
    ap.add_argument("--kra-crc-example", help="built examples/kra-layer-crc binary: also check the tile decoder of every "
                    "KRA paint layer, including files the importer refuses by a limit (out/rust/release/examples/kra-layer-crc)")
    ap.add_argument("--render-side", type=int, default=600, help="long side of the composite render in pixels")
    ap.add_argument("--max-layer-diff", type=int, default=0, help="allowed byte difference of a layer bitmap")
    ap.add_argument("--max-render-diff", type=float, default=3.5, help="allowed mean |render - merged image|")
    ap.add_argument("--max-djpeg-mean", type=float, default=0.15, help="allowed mean |ours - djpeg -nosmooth|")
    ap.add_argument("--max-djpeg-peak", type=int, default=6, help="allowed largest |ours - djpeg -nosmooth|")
    ap.add_argument("--max-djpeg-outliers", type=float, default=0.0005, help="allowed fraction of samples off by > 16")
    ap.add_argument("--max-sips-mean", type=float, default=3.0, help="allowed mean |ours - sips| (ImageIO differs)")
    ap.add_argument("--max-sips-outliers", type=float, default=0.03, help="allowed fraction of samples off by > 16")
    ap.add_argument("--max-compare-pixels", type=int, default=6_000_000, help="larger JPEGs are decoded, not compared")
    ap.add_argument("--self-test", action="store_true", help="check the KRA tile decoder on built-in data and exit")
    args = ap.parse_args()
    if args.self_test:
        kra_self_test()
        print("self-test passed")
        return 0
    if not args.paths:
        ap.error("the following arguments are required: paths")
    if not os.access(args.core, os.X_OK):
        print("missing %s; run tools/build-rust-core.sh first" % args.core, file=sys.stderr)
        return 2
    psd_files, jpg_files, kra_files = collect(args.paths)
    if not psd_files and not jpg_files and not kra_files:
        print("no PSD, KRA or JPEG files found", file=sys.stderr)
        return 2
    references = [name for name in ("sips", "djpeg") if shutil.which(name)]
    infos = []
    for path in jpg_files:
        with open(path, "rb") as handle:
            info = classify_jpeg(handle.read())
        if info:
            infos.append((path, info))
    chosen = pick_jpegs(infos, args.max_jpeg)
    out = args.out or os.path.join(ROOT, "build", "verification", "real-files-" + time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(out, exist_ok=True)
    report = {"psd": [], "kra": [], "jpeg": [], "summary": {"psd_files": len(psd_files), "kra_files": len(kra_files),
                                                   "jpeg_candidates": len(infos),
                                                   "jpeg_selected": len(chosen), "references": references}}
    classes = {}
    for _, info in infos:
        classes[stream_class(info)] = classes.get(stream_class(info), 0) + 1
    report["summary"]["jpeg_candidate_classes"] = dict(sorted(classes.items(), key=lambda kv: -kv[1]))
    core = Core(args.core, args.timeout)
    try:
        for path in psd_files:
            print("PSD", os.path.basename(path), flush=True)
            report["psd"].append(verify_psd(core, path, args))
        for path in kra_files:
            rec = verify_kra_file(core, path, args)
            print("KRA", rec["file"], rec["status"], rec.get("error", rec.get("mismatches", "")), flush=True)
            report["kra"].append(rec)
        with tempfile.TemporaryDirectory(prefix="verify-real-files-") as workdir:
            for path, info in chosen:
                pixels = info.get("width", 0) * info.get("height", 0)
                rec = verify_jpeg(core, path, info, args, workdir, bool(references) and pixels <= args.max_compare_pixels)
                print("JPEG", rec["file"], rec["class"], rec["status"], rec.get("djpeg", rec.get("sips", rec.get("error", ""))),
                      flush=True)
                report["jpeg"].append(rec)
    finally:
        core.close()
    problems = judge(report, args)
    report["problems"] = problems
    with open(os.path.join(out, "report.json"), "w", encoding="utf-8") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=1)
    print("report:", os.path.join(out, "report.json"))
    for line in report.get("not_comparable", []):
        print("NOT COMPARABLE", line)
    for line in problems:
        print("FAIL", line)
    not_compared = report["summary"]["psd_not_compared"]
    if not_compared:
        print("%d PSD/KRA file(s) not compared with the merged image (unsupported compression or none); render check not run" % not_compared)
    if problems:
        print("%d problem(s)" % len(problems))
        return 1
    if report.get("not_comparable"):
        print("NOT COMPARABLE: %d KRA file(s) have a render difference that unsupported layers may explain; this is not a PASS"
              % len(report["not_comparable"]))
        return 3
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
