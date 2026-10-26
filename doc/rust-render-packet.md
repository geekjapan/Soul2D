# Rust host rendering

`render.packet` is a read-only Rust Session request. JSONL and the C ABI use that
operation name; stdio MCP exposes `render_packet`. It accepts the same optional
`values` and `deformations` overrides as `scene.evaluate`. It does not change the
document, revision, history, or runtime state. Request a new packet for each pose;
runtime animation and physics can change without changing document revision.

Version 1 contains:

| Field | Meaning |
| --- | --- |
| `version` | Integer `1`; other versions must be rejected. |
| `rootScope` | Index `0` in `scopes`. |
| `scopes` | Arrays of drawable UUIDs in the Rust collector's descending cumulative z order and stable traversal order. Composite children stay in their own scope. |
| `draws` | Unique UUID records for visible geometry and required hidden mask or weld sources. |
| `textures` | Referenced document texture slots decoded to RGBA8 (the stored bytes, which every consumer blends as premultiplied; the colour of a texel with alpha 0 is cleared), top row first, and base64 encoded in `rgba`; each record includes `slot`, `width`, and `height`. |

Each draw has `id`, `kind` (`Part`, `Mask`, or `Composite`), world-space
`vertices`, UV pairs, triangle `indices`, three nullable texture slots,
`material`, and ordered resolved `masks`. Material fields are `blendMode`,
`tint`, `screenTint`, `opacity`, `emissionStrength`, and `maskThreshold`.
`blendMode` is one of the 19 INX names. A drawn node in `LinearBurn`,
`LinearLight`, `Hue` or `Color`, with an `alphaBlend` other than `Over`, or in a
Cubism 5.3 mode, refuses the packet:
version 1 has no name for them, and only the CPU `render` path blends them.
A packet read for its vertices only (`scene_geometry`) names a node with such an
`alphaBlend` `Cubism53`.
The Rust core applies node transforms, deformation, mesh origins, and reciprocal
welds before exporting vertices. The host must not evaluate a D node graph.
Geometric Masks need no UVs or texture. Missing albedo means no Part color draw.
Rust rejects a Part with triangles but no albedo; an empty untextured Part is valid.
Mask sources use geometry or raw texture alpha rather than their tint or opacity.

A Composite also has a child `scope`, logical `sourceBounds`, and a physical
`surface` (`width`, `height`, `bounds`). Bounds are `[minX,minY,maxX,maxY]`.
Its four vertices are ordered top-left, bottom-left, top-right, bottom-right.
An empty Composite has empty geometry and null bounds/surface; the host skips it.
Composite mask sources test their child scope's alpha before applying the
Composite's own outer masks or material.

Packet generation reuses the PNG renderer's scene collection, masks, material,
welding, and Composite geometry helpers. PNG `render` remains available to
headless clients. The OpenGL host adapter is transient GPU state only; preview
and live capture share its execution path when a real OpenGL context is available.
GPU failures are reported, rather than retried with a different renderer.

The exporter rejects indices beyond u16, invalid geometry/material/texture data,
more than 100,000 scene nodes, scene depth over 128, Composite depth over 16,
more than one million mask references, and resources or serialized output over
640 MiB. Each texture and Composite surface is limited to 16 megapixels.
The host also validates packet references, dependency cycles, device texture
limits, dimensions, geometry, and allocation budgets before creating resources.

## Verification

```bash
./tools/build-rust-core.sh
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust
python3 tools/verify-composite-pixels.py
```

`tools/verify-composite-pixels.py` renders bounded Composite fixtures through the built
Rust library and checks only that each case shows visible red coverage (or stays
transparent for the zero-scale case). The windowless CGL harness that compared a D OpenGL
host with production sampling (`tools/verify-rust-host-render.py`,
`soul2d:composite-pixels`) and the backend patch it needed were deleted with the D tree.
Nothing replaces its mask, material, edit, Undo/Redo, rejected-packet and GPU error
recovery coverage. A passing Rust unit suite alone
does not prove host GPU behavior. The current integration result is tracked in
[rust-migration.md](rust-migration.md).

This packet covers the existing INX scene subset. It does not establish Live2D
MOC loading, CMO3 authoring, full Cubism model compatibility, or native GUI input.
