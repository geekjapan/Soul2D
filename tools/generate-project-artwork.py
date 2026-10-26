#!/usr/bin/env python3
"""Deterministically generates the Soul2D project artwork set.

Every pixel is produced from parametric geometry authored for this
generator: two offset quadrilateral planes joined by circular control
points, in a dark-ink / warm-cream / vermilion / cyan palette. No prior
project artwork, mascot image, or wordmark font is traced, sampled, or
embedded; the SOUL2D wordmark uses a tiny self-authored bitmap glyph set.

Regeneration of every listed file is byte-for-byte reproducible and uses
only the Python standard library.

Usage:
    tools/generate-project-artwork.py                   # read-only verify
    tools/generate-project-artwork.py --check           # same as default
    tools/generate-project-artwork.py --write           # write all assets
    tools/generate-project-artwork.py --selftest        # corruption detection test
    tools/generate-project-artwork.py --write --contact-sheet=PATH
"""

import binascii
import hashlib
import json
import math
import os
import shutil
import struct
import sys
import tempfile
import zlib

REPO_ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

# First-run originals are preserved once and never replaced afterwards.
ORIGINALS_ROOT = os.path.join(
    REPO_ROOT, "build", "verification", "content-replacement-20261011", "originals"
)

MANIFEST_PATH = "res/project-artwork.json"

# Palette: dark ink, warm cream, vermilion, cyan.
INK = (36, 32, 43, 255)
CREAM = (245, 238, 222, 255)
CREAM_DIM = (228, 216, 190, 255)
VERMILION = (222, 74, 44, 255)
VERMILION_DARK = (176, 50, 28, 255)
CYAN = (34, 168, 182, 255)
CYAN_DARK = (21, 116, 128, 255)
CYAN_GHOST = (34, 168, 182, 92)


def new_image(width, height, color=(0, 0, 0, 0)):
    return bytearray(bytes(color) * (width * height))


def blend_px(img, width, height, x, y, rgba):
    if x < 0 or y < 0 or x >= width or y >= height:
        return
    sa = rgba[3]
    if sa == 0:
        return
    i = (y * width + x) * 4
    if sa == 255:
        img[i:i + 4] = bytes(rgba)
        return
    da = img[i + 3]
    fa = sa / 255.0
    fb = da / 255.0
    oa = fa + fb * (1.0 - fa)
    if oa <= 0.0:
        img[i:i + 4] = b"\x00\x00\x00\x00"
        return
    for c in range(3):
        img[i + c] = int(round((rgba[c] * fa + img[i + c] * fb * (1.0 - fa)) / oa))
    img[i + 3] = int(round(oa * 255.0))


def fill_poly(img, width, height, pts, color):
    ys = [p[1] for p in pts]
    y0 = max(0, int(min(ys)))
    y1 = min(height, int(max(ys)) + 1)
    n = len(pts)
    for y in range(y0, y1):
        yc = y + 0.5
        xs = []
        for i in range(n):
            x1, yy1 = pts[i]
            x2, yy2 = pts[(i + 1) % n]
            if (yy1 <= yc < yy2) or (yy2 <= yc < yy1):
                xs.append(x1 + (yc - yy1) * (x2 - x1) / (yy2 - yy1))
        xs.sort()
        for k in range(0, len(xs) - 1, 2):
            xa = int(math.ceil(xs[k] - 0.5))
            xb = int(math.ceil(xs[k + 1] - 0.5)) - 1
            for x in range(max(0, xa), min(width - 1, xb) + 1):
                blend_px(img, width, height, x, y, color)


def fill_circle(img, width, height, cx, cy, r, color):
    r2 = r * r
    x0 = max(0, int(cx - r - 1))
    x1 = min(width - 1, int(cx + r + 1))
    for y in range(max(0, int(cy - r - 1)), min(height - 1, int(cy + r + 1)) + 1):
        dy2 = (y + 0.5 - cy) ** 2
        for x in range(x0, x1 + 1):
            if (x + 0.5 - cx) ** 2 + dy2 <= r2:
                blend_px(img, width, height, x, y, color)


def stroke_segment(img, width, height, a, b, thickness, color):
    dx = b[0] - a[0]
    dy = b[1] - a[1]
    length = math.hypot(dx, dy)
    half = thickness / 2.0
    if length == 0.0:
        fill_circle(img, width, height, a[0], a[1], half, color)
        return
    nx = -dy / length * half
    ny = dx / length * half
    fill_poly(
        img, width, height,
        [(a[0] + nx, a[1] + ny), (b[0] + nx, b[1] + ny),
         (b[0] - nx, b[1] - ny), (a[0] - nx, a[1] - ny)],
        color,
    )
    fill_circle(img, width, height, a[0], a[1], half, color)
    fill_circle(img, width, height, b[0], b[1], half, color)


def stroke_poly(img, width, height, pts, thickness, color, closed=True):
    n = len(pts)
    last = n if closed else n - 1
    for i in range(last):
        stroke_segment(img, width, height, pts[i], pts[(i + 1) % n], thickness, color)


def rounded_rect_points(x0, y0, x1, y1, r, seg=8):
    pts = []
    for cx, cy, a0 in (
        (x1 - r, y0 + r, -90.0),
        (x1 - r, y1 - r, 0.0),
        (x0 + r, y1 - r, 90.0),
        (x0 + r, y0 + r, 180.0),
    ):
        for i in range(seg + 1):
            ang = math.radians(a0 + i * 90.0 / seg)
            pts.append((cx + r * math.cos(ang), cy + r * math.sin(ang)))
    return pts


def frame_round_rect(img, width, height, inset, radius, thickness, color, bg):
    fill_poly(img, width, height,
              rounded_rect_points(inset, inset, width - inset, height - inset, radius), color)
    inner = max(radius - thickness, 1.0)
    fill_poly(img, width, height,
              rounded_rect_points(inset + thickness, inset + thickness,
                                  width - inset - thickness, height - inset - thickness,
                                  inner), bg)


def downsample(img, sw, sh, ss):
    if ss == 1:
        return img
    w = sw // ss
    h = sh // ss
    out = bytearray(w * h * 4)
    n = ss * ss
    for y in range(h):
        for x in range(w):
            acc0 = acc1 = acc2 = acc3 = 0
            for dy in range(ss):
                row = ((y * ss + dy) * sw + x * ss) * 4
                for dx in range(ss):
                    i = row + dx * 4
                    acc0 += img[i]
                    acc1 += img[i + 1]
                    acc2 += img[i + 2]
                    acc3 += img[i + 3]
            o = (y * w + x) * 4
            out[o:o + 4] = bytes((acc0 // n, acc1 // n, acc2 // n, acc3 // n))
    return out


def blit(dst, dw, dh, src, sw, sh, ox, oy):
    for y in range(sh):
        ty = oy + y
        if ty < 0 or ty >= dh:
            continue
        for x in range(sw):
            tx = ox + x
            if tx < 0 or tx >= dw:
                continue
            i = (y * sw + x) * 4
            blend_px(dst, dw, dh, tx, ty,
                     (src[i], src[i + 1], src[i + 2], src[i + 3]))


def scale_down(img, sw, sh, factor):
    w = max(1, sw // factor)
    h = max(1, sh // factor)
    out = bytearray(w * h * 4)
    for y in range(h):
        for x in range(w):
            i = (y * factor * sw + x * factor) * 4
            o = (y * w + x) * 4
            out[o:o + 4] = img[i:i + 4]
    return out, w, h


# Self-authored 5x7 bitmap glyphs for the SOUL2D wordmark and labels.
GLYPHS = {
    "S": ["01110", "10001", "10000", "01110", "00001", "10001", "01110"],
    "O": ["01110", "10001", "10001", "10001", "10001", "10001", "01110"],
    "U": ["10001", "10001", "10001", "10001", "10001", "10001", "01110"],
    "L": ["10000", "10000", "10000", "10000", "10000", "10000", "11111"],
    "2": ["01110", "10001", "00001", "00010", "00100", "01000", "11111"],
    "D": ["11110", "10001", "10001", "10001", "10001", "10001", "11110"],
    " ": ["00000"] * 7,
    "-": ["00000", "00000", "00000", "01110", "00000", "00000", "00000"],
    "i": ["00100", "00000", "01100", "00100", "00100", "00100", "01110"],
    "p": ["00000", "00000", "11100", "10010", "10010", "11100", "10000"],
    "n": ["00000", "00000", "11110", "10001", "10001", "10001", "10001"],
    "g": ["00000", "00000", "01111", "10001", "01111", "00001", "11110"],
    "a": ["00000", "00000", "01110", "00001", "01111", "10001", "01111"],
    "e": ["00000", "00000", "01110", "10001", "11110", "10000", "01110"],
    "c": ["00000", "00000", "01110", "10000", "10000", "10001", "01110"],
    "o": ["00000", "00000", "01110", "10001", "10001", "10001", "01110"],
    "m": ["00000", "00000", "11010", "10101", "10101", "10101", "10101"],
    "r": ["00000", "00000", "10110", "11001", "10000", "10000", "10000"],
    "s": ["00000", "00000", "01110", "10000", "01110", "00001", "01110"],
    "u": ["00000", "00000", "10001", "10001", "10001", "10011", "01101"],
    "b": ["10000", "10000", "11100", "10010", "10010", "10010", "11100"],
    "d": ["00001", "00001", "01101", "10011", "10001", "10011", "01101"],
    "x": ["00000", "00000", "10001", "01010", "00100", "01010", "10001"],
    "t": ["00100", "00100", "01110", "00100", "00100", "00101", "00010"],
    "w": ["00000", "00000", "10001", "10101", "10101", "10101", "01010"],
    "f": ["00011", "00100", "01110", "00100", "00100", "00100", "00100"],
    "l": ["01100", "00100", "00100", "00100", "00100", "00100", "01110"],
    "h": ["10000", "10000", "11100", "10010", "10001", "10001", "10001"],
    "k": ["10000", "10000", "10010", "10100", "11000", "10100", "10010"],
    "v": ["00000", "00000", "10001", "10001", "10001", "01010", "00100"],
    "y": ["00000", "00000", "10001", "10001", "01111", "00001", "01110"],
    "0": ["01110", "10001", "10011", "10101", "11001", "10001", "01110"],
    "1": ["00100", "01100", "00100", "00100", "00100", "00100", "01110"],
    "3": ["01110", "10001", "00001", "00110", "00001", "10001", "01110"],
    "4": ["00010", "00110", "01010", "10010", "11111", "00010", "00010"],
    "5": ["11111", "10000", "11110", "00001", "00001", "10001", "01110"],
    "6": ["01110", "10000", "11110", "10001", "10001", "10001", "01110"],
    "7": ["11111", "00001", "00010", "00100", "01000", "01000", "01000"],
    "8": ["01110", "10001", "10001", "01110", "10001", "10001", "01110"],
    "9": ["01110", "10001", "10001", "01111", "00001", "10001", "01110"],
    ".": ["00000", "00000", "00000", "00000", "00000", "00110", "00110"],
    ":": ["00000", "00110", "00110", "00000", "00110", "00110", "00000"],
    "/": ["00001", "00001", "00010", "00100", "01000", "10000", "10000"],
}


def text_width(text, scale):
    return max(0, len(text) * 6 * scale - scale)


def draw_text(img, width, height, x, y, text, scale, color, accent=None, accent_start=None):
    cx = x
    for idx, ch in enumerate(text):
        glyph = GLYPHS.get(ch, GLYPHS.get(ch.lower(), GLYPHS[" "]))
        use = accent if (accent is not None and accent_start is not None and idx >= accent_start) else color
        for gy, row in enumerate(glyph):
            for gx, bit in enumerate(row):
                if bit == "1":
                    for dy in range(scale):
                        for dx in range(scale):
                            blend_px(img, width, height,
                                     int(cx + gx * scale + dx), int(y + gy * scale + dy), use)
        cx += 6 * scale


# The Soul2D mark: two offset quadrilateral planes joined by control points.
# Unit-space quad for the front plane; the back plane is a scaled offset copy.
MARK_QUAD = [(-0.60, -0.52), (0.66, -0.40), (0.52, 0.60), (-0.50, 0.46)]
MARK_ANGLE = -14.0
MARK_BACK_OFFSET = (0.36, -0.40)
MARK_BACK_SCALE = 0.9


def mark_vertices(cx, cy, s, angle=MARK_ANGLE):
    rad = math.radians(angle)
    ca, sa = math.cos(rad), math.sin(rad)

    def tr(px, py):
        return (cx + s * (px * ca - py * sa), cy + s * (px * sa + py * ca))

    front = [tr(px, py) for px, py in MARK_QUAD]
    back = [tr(MARK_BACK_OFFSET[0] + px * MARK_BACK_SCALE,
               MARK_BACK_OFFSET[1] + py * MARK_BACK_SCALE)
            for px, py in MARK_QUAD]
    return front, back


def draw_mark(img, width, height, cx, cy, s, angle=MARK_ANGLE):
    front, back = mark_vertices(cx, cy, s, angle)
    fill_poly(img, width, height, back, CYAN_GHOST)
    stroke_poly(img, width, height, back, 0.055 * s, CYAN_DARK)
    for i in range(4):
        stroke_segment(img, width, height, front[i], back[i], 0.03 * s, INK)
    fill_poly(img, width, height, front, VERMILION)
    stroke_poly(img, width, height, front, 0.055 * s, INK)
    for bx, by in back:
        fill_circle(img, width, height, bx, by, 0.075 * s, INK)
        fill_circle(img, width, height, bx, by, 0.045 * s, CYAN)
    for fx, fy in front:
        fill_circle(img, width, height, fx, fy, 0.095 * s, INK)
        fill_circle(img, width, height, fx, fy, 0.055 * s, CREAM)


def supersample_for(w, h):
    return max(1, min(8, 4096 // max(w, h)))


def render_icon(size):
    ss = supersample_for(size, size)
    w = h = size * ss
    img = new_image(w, h, CREAM)
    if size >= 48:
        inset = w * 0.045
        frame_round_rect(img, w, h, inset, w * 0.16, w * 0.02, INK, CREAM)
    draw_mark(img, w, h, w / 2, h / 2, w * 0.30)
    return downsample(img, w, h, ss), size, size


def render_logo(size):
    ss = supersample_for(size, size)
    w = h = size * ss
    img = new_image(w, h, CREAM)
    draw_mark(img, w, h, w / 2, h * 0.40, w * 0.26)
    scale = max(2, int(w / 250.0 * 7))
    word = "SOUL2D"
    tx = (w - text_width(word, scale)) // 2
    draw_text(img, w, h, tx, int(h * 0.74), word, scale, INK,
              accent=VERMILION, accent_start=4)
    return downsample(img, w, h, ss), size, size


def render_mark_transparent(size):
    ss = supersample_for(size, size)
    w = h = size * ss
    img = new_image(w, h)
    draw_mark(img, w, h, w / 2, h / 2, w * 0.36)
    return downsample(img, w, h, ss), size, size


def render_banner():
    w0, h0 = 2048, 800
    ss = supersample_for(w0, h0)
    w, h = w0 * ss, h0 * ss
    img = new_image(w, h, CREAM)
    draw_mark(img, w, h, w * 0.18, h * 0.5, h * 0.42)
    scale = int(h / 34)
    word = "SOUL2D"
    tx = int(w * 0.37)
    ty = int(h * 0.38)
    draw_text(img, w, h, tx, ty, word, scale, INK, accent=VERMILION, accent_start=4)
    ry = int(h * 0.70)
    fill_poly(img, w, h, [(tx, ry), (w * 0.90, ry), (w * 0.90, ry + 8 * ss), (tx, ry + 8 * ss)], INK)
    fill_circle(img, w, h, w * 0.905, ry + 4 * ss, 11 * ss, VERMILION)
    fill_circle(img, w, h, w * 0.925, ry + 4 * ss, 11 * ss, CYAN)
    return downsample(img, w, h, ss), w0, h0


def render_banner_logo():
    w0, h0 = 1280, 865
    ss = supersample_for(w0, h0)
    w, h = w0 * ss, h0 * ss
    img = new_image(w, h)
    draw_mark(img, w, h, w / 2, h * 0.36, w * 0.26)
    scale = int(h / 46)
    word = "SOUL2D"
    tx = (w - text_width(word, scale)) // 2
    draw_text(img, w, h, tx, int(h * 0.72), word, scale, INK,
              accent=VERMILION, accent_start=4)
    return downsample(img, w, h, ss), w0, h0


def render_grid():
    light = (237, 231, 215, 255)
    dark = (201, 193, 172, 255)
    img = bytearray(bytes(light) + bytes(dark) + bytes(dark) + bytes(light))
    return img, 2, 2


def capsule(img, width, height, cx, cy, half_len, r, color, vertical=True):
    if vertical:
        fill_poly(img, width, height,
                  [(cx - r, cy - half_len), (cx + r, cy - half_len),
                   (cx + r, cy + half_len), (cx - r, cy + half_len)], color)
        fill_circle(img, width, height, cx, cy - half_len, r, color)
        fill_circle(img, width, height, cx, cy + half_len, r, color)
    else:
        fill_poly(img, width, height,
                  [(cx - half_len, cy - r), (cx + half_len, cy - r),
                   (cx + half_len, cy + r), (cx - half_len, cy + r)], color)
        fill_circle(img, width, height, cx - half_len, cy, r, color)
        fill_circle(img, width, height, cx + half_len, cy, r, color)


def render_status(kind):
    size = 1024
    ss = supersample_for(size, size)
    w = h = size * ss
    img = new_image(w, h)
    c = w / 2
    r = w * 0.42
    if kind == "info":
        fill_circle(img, w, h, c, c, r + w * 0.028, INK)
        fill_circle(img, w, h, c, c, r, CYAN)
        fill_circle(img, w, h, c, c - r * 0.52, w * 0.068, CREAM)
        capsule(img, w, h, c, c + r * 0.28, r * 0.36, w * 0.068, CREAM)
    elif kind == "warning":
        outer = [(c, c - r), (c + r * 0.92, c + r * 0.62), (c - r * 0.92, c + r * 0.62)]
        cx_, cy_ = c, c + (r * 0.62 - r) / 3.0
        inner = [(cx_ + (px - cx_) * 0.84, cy_ + (py - cy_) * 0.84) for px, py in outer]
        fill_poly(img, w, h, outer, INK)
        fill_poly(img, w, h, inner, VERMILION)
        capsule(img, w, h, c, c - r * 0.18, r * 0.30, w * 0.055, CREAM)
        fill_circle(img, w, h, c, c + r * 0.36, w * 0.06, CREAM)
    else:  # error
        fill_circle(img, w, h, c, c, r + w * 0.028, VERMILION)
        fill_circle(img, w, h, c, c, r, INK)
        arm = r * 0.42
        t = w * 0.062
        d = arm / math.sqrt(2.0)
        stroke_segment(img, w, h, (c - d, c - d), (c + d, c + d), t * 2, CREAM)
        stroke_segment(img, w, h, (c - d, c + d), (c + d, c - d), t * 2, CREAM)
    return downsample(img, w, h, ss), size, size


def render_dmgbg():
    w0, h0 = 800, 600
    ss = supersample_for(w0, h0)
    w, h = w0 * ss, h0 * ss
    img = new_image(w, h, CREAM)
    scale = int(h / 130)
    word = "SOUL2D"
    tx = (w - text_width(word, scale)) // 2
    draw_text(img, w, h, tx, int(h * 0.075), word, scale, INK,
              accent=VERMILION, accent_start=4)
    front, back = mark_vertices(w * 0.70, h * 0.55, w * 0.20)
    fill_poly(img, w, h, back, CYAN_GHOST)
    stroke_poly(img, w, h, back, 0.055 * w * 0.20, CYAN_DARK)
    for i in range(4):
        stroke_segment(img, w, h, front[i], back[i], 0.03 * w * 0.20, CREAM_DIM)
    fill_poly(img, w, h, front, (222, 74, 44, 120))
    stroke_poly(img, w, h, front, 0.055 * w * 0.20, INK)
    for bx, by in back:
        fill_circle(img, w, h, bx, by, 0.075 * w * 0.20, INK)
        fill_circle(img, w, h, bx, by, 0.045 * w * 0.20, CYAN)
    for fx, fy in front:
        fill_circle(img, w, h, fx, fy, 0.095 * w * 0.20, INK)
        fill_circle(img, w, h, fx, fy, 0.055 * w * 0.20, CREAM)
    return downsample(img, w, h, ss), w0, h0


def render_wix_banner():
    w0, h0 = 493, 58
    ss = 4
    w, h = w0 * ss, h0 * ss
    img = new_image(w, h, CREAM)
    scale = 3 * ss
    draw_text(img, w, h, int(w * 0.035), (h - 7 * scale) // 2, "SOUL2D", scale, INK,
              accent=VERMILION, accent_start=4)
    draw_mark(img, w, h, w * 0.885, h * 0.5, h * 0.42)
    fill_poly(img, w, h, [(0, h - 2 * ss), (w, h - 2 * ss), (w, h), (0, h)], INK)
    return downsample(img, w, h, ss), w0, h0


def render_wix_dialog():
    w0, h0 = 493, 312
    ss = 4
    w, h = w0 * ss, h0 * ss
    img = new_image(w, h, CREAM)
    panel = int(w * 0.33)
    fill_poly(img, w, h, [(0, 0), (panel, 0), (panel, h), (0, h)], INK)
    draw_mark(img, w, h, panel / 2, h * 0.42, panel * 0.34)
    scale = 2 * ss
    word = "SOUL2D"
    tx = (panel - text_width(word, scale)) // 2
    draw_text(img, w, h, tx, int(h * 0.72), word, scale, CREAM,
              accent=VERMILION, accent_start=4)
    fill_poly(img, w, h, [(panel, 0), (panel + 3 * ss, 0), (panel + 3 * ss, h), (panel, h)], CYAN)
    front, back = mark_vertices(w * 0.80, h * 0.16, w * 0.09)
    fill_poly(img, w, h, back, CYAN_GHOST)
    fill_poly(img, w, h, front, (222, 74, 44, 110))
    stroke_poly(img, w, h, front, 0.05 * w * 0.09, INK)
    return downsample(img, w, h, ss), w0, h0


def encode_png(img, w, h):
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data +
                struct.pack(">I", binascii.crc32(tag + data) & 0xFFFFFFFF))

    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)
        raw += img[y * stride:(y + 1) * stride]
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


def encode_bmp24(img, w, h):
    row_bytes = (w * 3 + 3) // 4 * 4
    pixel_off = 14 + 40
    size = pixel_off + row_bytes * h
    out = bytearray()
    out += b"BM" + struct.pack("<IHHI", size, 0, 0, pixel_off)
    out += struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, row_bytes * h, 2835, 2835, 0, 0)
    pad = row_bytes - w * 3
    for y in range(h - 1, -1, -1):
        for x in range(w):
            i = (y * w + x) * 4
            out += bytes((img[i + 2], img[i + 1], img[i]))
        out += b"\x00" * pad
    return bytes(out)


def encode_ico(png_entries):
    header = struct.pack("<HHH", 0, 1, len(png_entries))
    offset = 6 + 16 * len(png_entries)
    directory = b""
    body = b""
    for size, png in png_entries:
        bsize = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", bsize, bsize, 0, 0, 1, 32, len(png), offset)
        body += png
        offset += len(png)
    return header + directory + body


ICONSET_SIZES = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]

ICO_ENTRY_SIZES = [256, 128, 64, 48, 32, 16]


def png_asset(render):
    img, w, h = render()
    return encode_png(img, w, h), w, h


def build_assets():
    """Returns dict[repo-relative path] = (bytes, format, width, height, detail)."""
    assets = {}

    pngs = {
        "res/icon.png": lambda: render_icon(512),
        "res/logo.png": lambda: render_logo(512),
        "res/logo_256.png": lambda: render_logo(256),
        "res/ui/banner.png": render_banner,
        "res/ui/banner-logo.png": render_banner_logo,
        "res/ui/grid.png": render_grid,
        "res/ui/ui-ada.png": lambda: render_mark_transparent(1024),
        "res/ui/ui-info.png": lambda: render_status("info"),
        "res/ui/ui-warning.png": lambda: render_status("warning"),
        "res/ui/ui-error.png": lambda: render_status("error"),
        "build-aux/osx/dmgbg.png": render_dmgbg,
    }
    for name, size in ICONSET_SIZES:
        pngs["build-aux/osx/soul2d.iconset/" + name] = (lambda s=size: render_icon(s))

    for path, render in pngs.items():
        data, w, h = png_asset(render)
        assets[path] = (data, "png", w, h, None)

    ico_entries = []
    for size in ICO_ENTRY_SIZES:
        img, w, h = render_icon(size)
        ico_entries.append((size, encode_png(img, w, h)))
    ico = encode_ico(ico_entries)
    assets["build-aux/windows/soul2d.ico"] = (
        ico, "ico", 256, 256,
        [{"width": s, "height": s, "bytes": len(p)} for s, p in ico_entries],
    )

    for path, render in (
        ("build-aux/windows/wix/res/banner.bmp", render_wix_banner),
        ("build-aux/windows/wix/res/dialog.bmp", render_wix_dialog),
    ):
        img, w, h = render()
        assets[path] = (encode_bmp24(img, w, h), "bmp", w, h, None)

    return assets


def build_manifest(assets):
    entries = []
    for path in sorted(assets):
        data, fmt, w, h, detail = assets[path]
        entry = {
            "path": path,
            "format": fmt,
            "width": w,
            "height": h,
            "sha256": hashlib.sha256(data).hexdigest(),
        }
        if detail is not None:
            entry["icoEntries"] = detail
        entries.append(entry)
    doc = {
        "description": "Generated Soul2D project artwork; deterministic output of "
                       "tools/generate-project-artwork.py. Self-authored parametric "
                       "geometry; no prior artwork content is reused.",
        "generator": "tools/generate-project-artwork.py",
        "files": entries,
    }
    return (json.dumps(doc, indent=2, sort_keys=False) + "\n").encode("utf-8")


def outputs():
    result = {}
    assets = build_assets()
    for path, (data, _fmt, _w, _h, _detail) in assets.items():
        result[path] = data
    result[MANIFEST_PATH] = build_manifest(assets)
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
        parent = os.path.dirname(target)
        if parent:
            os.makedirs(parent, exist_ok=True)
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
        expected = items[rel]
        if actual != expected:
            errors.append("mismatch: %s (sha256 %s expected %s)" % (
                rel,
                hashlib.sha256(actual).hexdigest()[:16],
                hashlib.sha256(expected).hexdigest()[:16]))
    return errors


def run_check(root):
    items = outputs()
    errors = check_outputs(root, items)
    if errors:
        for err in errors:
            sys.stderr.write("check: %s\n" % err)
        return 1
    print("check: all %d generated artwork files match expected bytes" % len(items))
    return 0


def run_selftest():
    """Writes assets to a scratch dir, proves --check logic catches corruption."""
    items = outputs()
    with tempfile.TemporaryDirectory(prefix="soul2d-artwork-selftest-") as tmp:
        backup_root = os.path.join(tmp, "originals")
        write_outputs(tmp, items, backup_root=backup_root)
        if check_outputs(tmp, items):
            sys.stderr.write("selftest: clean scratch tree reported errors\n")
            return 1

        def only_error_for(rel, label):
            errors = check_outputs(tmp, items)
            if not errors:
                sys.stderr.write("selftest: %s was not detected\n" % label)
                return False
            if len(errors) != 1 or rel not in errors[0]:
                sys.stderr.write(
                    "selftest: unexpected errors after %s: %s\n" % (label, errors))
                return False
            return True

        def restore(rel):
            path = os.path.join(tmp, rel)
            with open(path, "wb") as f:
                f.write(items[rel])
            if check_outputs(tmp, items):
                sys.stderr.write("selftest: scratch tree did not return to clean state\n")
                return False
            return True

        victim_rel = "res/icon.png"
        data = bytearray(items[victim_rel])
        data[len(data) // 2] ^= 1
        with open(os.path.join(tmp, victim_rel), "wb") as f:
            f.write(bytes(data))
        if not only_error_for(victim_rel, "corrupted PNG"):
            return 1
        if not restore(victim_rel):
            return 1

        manifest_rel = MANIFEST_PATH
        data = bytearray(items[manifest_rel])
        data[-20] ^= 1
        with open(os.path.join(tmp, manifest_rel), "wb") as f:
            f.write(bytes(data))
        if not only_error_for(manifest_rel, "corrupted manifest"):
            return 1
        if not restore(manifest_rel):
            return 1

        missing_rel = "res/logo.png"
        os.remove(os.path.join(tmp, missing_rel))
        if not only_error_for(missing_rel, "missing file"):
            return 1
        if not restore(missing_rel):
            return 1

        if not _selftest_symlink_guards(tmp):
            return 1
    print("selftest: corruption, manifest drift, missing files, "
          "and unsafe-path refusals all detected")
    return 0


def _selftest_symlink_guards(tmp):
    def expect_refusal(fn, label):
        try:
            fn()
        except SystemExit as exc:
            if exc.code:
                return True
        sys.stderr.write("selftest: %s was not rejected\n" % label)
        return False

    tiny = {"sub/tiny.bin": b"tiny-bytes"}
    wroot = os.path.join(tmp, "w")
    os.makedirs(os.path.join(wroot, "real-dir"))
    os.symlink(os.path.join(wroot, "real-dir"), os.path.join(wroot, "sub"))
    if not expect_refusal(
            lambda: write_outputs(wroot, tiny, backup_root=os.path.join(tmp, "b0")),
            "symlinked target parent directory"):
        return False
    if os.path.exists(os.path.join(wroot, "real-dir", "tiny.bin")):
        sys.stderr.write("selftest: write escaped through symlinked parent\n")
        return False

    wroot2 = os.path.join(tmp, "w2")
    broot2 = os.path.join(tmp, "b2")
    os.makedirs(os.path.join(wroot2, "sub"))
    os.makedirs(os.path.join(broot2, "real"))
    os.symlink(os.path.join(broot2, "real"), os.path.join(broot2, "sub"))
    seeded = os.path.join(wroot2, "sub", "tiny.bin")
    with open(seeded, "wb") as f:
        f.write(b"old")
    if not expect_refusal(
            lambda: write_outputs(wroot2, tiny, backup_root=broot2),
            "symlinked backup parent directory"):
        return False
    with open(seeded, "rb") as f:
        if f.read() != b"old":
            sys.stderr.write("selftest: target written before backup safety check\n")
            return False
    if os.path.exists(os.path.join(broot2, "real", "tiny.bin")):
        sys.stderr.write("selftest: backup escaped through symlinked parent\n")
        return False

    if not expect_refusal(
            lambda: write_outputs(os.path.join(tmp, "w3"),
                                  {"../escaped.bin": b"x"},
                                  backup_root=os.path.join(tmp, "b3")),
            "path escaping the output root"):
        return False
    return True


def render_contact_sheet(path):
    cells = [
        ("res/icon.png", lambda: render_icon(128)),
        ("res/logo.png", lambda: render_logo(128)),
        ("res/logo_256.png", lambda: render_logo(64)),
        ("res/ui/banner.png", lambda: scale_down(*render_banner(), 4)),
        ("res/ui/banner-logo.png", lambda: scale_down(*render_banner_logo(), 4)),
        ("res/ui/ui-ada.png", lambda: scale_down(*render_mark_transparent(1024), 4)),
        ("res/ui/ui-info.png", lambda: scale_down(*render_status("info"), 4)),
        ("res/ui/ui-warning.png", lambda: scale_down(*render_status("warning"), 4)),
        ("res/ui/ui-error.png", lambda: scale_down(*render_status("error"), 4)),
        ("res/ui/grid.png", render_grid),
        ("build-aux/osx/dmgbg.png", lambda: scale_down(*render_dmgbg(), 3)),
        ("wix/banner.bmp", render_wix_banner),
        ("wix/dialog.bmp", render_wix_dialog),
        ("ico entries", lambda: _ico_sheet_strip()),
    ]
    margin = 24
    label_h = 20
    cols = 5
    cell_w = 300
    cell_h = 300
    rows = (len(cells) + cols - 1) // cols
    sheet_w = cols * (cell_w + margin) + margin
    sheet_h = rows * (cell_h + label_h + margin) + margin
    sheet = new_image(sheet_w, sheet_h, (48, 44, 55, 255))
    for idx, (label, render) in enumerate(cells):
        img, iw, ih = render()
        fit = min(cell_w / iw, cell_h / ih)
        if fit > 2.0:
            factor = int(fit)
            big = bytearray(iw * factor * ih * factor * 4)
            for yy in range(ih * factor):
                for xx in range(iw * factor):
                    i = ((yy // factor) * iw + (xx // factor)) * 4
                    o = (yy * iw * factor + xx) * 4
                    big[o:o + 4] = img[i:i + 4]
            img, iw, ih = big, iw * factor, ih * factor
        elif fit < 1.0:
            factor = int(round(1.0 / fit))
            img, iw, ih = scale_down(img, iw, ih, max(1, factor))
        # Checkerboard behind transparent art so alpha is visible on sheet.
        ox = margin + (idx % cols) * (cell_w + margin) + (cell_w - iw) // 2
        oy = margin + (idx // cols) * (cell_h + label_h + margin)
        for yy in range(ih):
            for xx in range(iw):
                if (xx // 8 + yy // 8) % 2 == 0:
                    blend_px(sheet, sheet_w, sheet_h, ox + xx, oy + yy, (70, 66, 78, 255))
        blit(sheet, sheet_w, sheet_h, img, iw, ih, ox, oy)
        draw_text(sheet, sheet_w, sheet_h, ox, oy + cell_h + 4, label, 1,
                  (200, 196, 210, 255))
    with open(path, "wb") as f:
        f.write(encode_png(sheet, sheet_w, sheet_h))
    print("contact sheet: %s (%dx%d)" % (path, sheet_w, sheet_h))


def _ico_sheet_strip():
    imgs = [render_icon(s) for s in [64, 48, 32, 16]]
    w = sum(iw for _img, iw, _ih in imgs) + 24 * len(imgs)
    h = 72
    out = new_image(w, h)
    x = 8
    for img, iw, ih in imgs:
        blit(out, w, h, img, iw, ih, x, (h - ih) // 2)
        x += iw + 24
    return out, w, h


def main(argv):
    mode = "check"
    contact = None
    for arg in argv[1:]:
        if arg == "--check":
            mode = "check"
        elif arg == "--write":
            mode = "write"
        elif arg == "--selftest":
            mode = "selftest"
        elif arg.startswith("--contact-sheet="):
            contact = arg.split("=", 1)[1]
        elif arg == "--contact-sheet":
            _fail("--contact-sheet requires =PATH")
        else:
            _fail("unknown argument: " + arg)
    if mode == "check":
        return run_check(REPO_ROOT)
    if mode == "selftest":
        return run_selftest()
    if contact:
        render_contact_sheet(contact)
        return 0
    write_outputs(REPO_ROOT, outputs())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
