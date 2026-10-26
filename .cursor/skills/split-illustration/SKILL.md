---
name: split-illustration
description: Split one flattened RGBA illustration (a bust-up or full-body character) into named Parts in a soul2d document through the split_* MCP tools, then check the result by stacking and rendering. Use when asked to cut an illustration into parts, separate hair, face, eyes or limbs, or prepare an image for rigging.
---

# Split an illustration into parts

Deterministic Rust does the pixels; you supply the prompts (points, boxes, thresholds). The design, every step and the
plan format are in [doc/part-split.md](../../../doc/part-split.md). Read it once before the first split.

## Setup

```bash
./tools/build-rust-core.sh
./out/rust/debug/soul2d-core --mcp-port 18088      # disposable headless host, or --mcp for stdio
./out/rust/debug/njc --port 18088 tools list       # the split_* tools must be listed
```

Call tools with `./out/rust/debug/njc --port 18088 tools call NAME --json 'JSON'`. Check JSON-RPC errors and `isError`;
exit code zero alone proves nothing. A failed call changes nothing. `split_apply` needs an INP/INX document: call
`new` first (or open a project). Native Live2D documents refuse every `split_*` tool.

## Loop

1. **Open**: `split_open {path}` (PNG, TGA, JPEG, or a PSD, which is flattened from its visible layers). Note `width` and `height`; every point is a pixel `[x, y]` with the
   origin at the top left.
2. **Inspect**: `split_overlay {}` returns an image content item (`content[type=image].data`, base64 PNG). Decode it
   and look at it. With no parts it is the source on grey. Read coordinates off it; do not guess them.
   For islands on transparency, call `split_components {threshold: 0}`. It returns up to 32
   `{seed, pixels, bbox: [x,y,w,h]}` records in row-major seed order, the exact `totalComponents` and `nextOffset`.
   Follow a non-null `nextOffset` as `offset`, keeping the source and threshold unchanged. An exhausted or
   out-of-range offset returns an empty page and null. Each page rescans the whole image; noisy art with many
   isolated pixels makes exhaustive paging expensive. An island is four-connected with alpha strictly above
   the integer threshold (0..255); it is not a semantic body-part label. Use its `seed` as a `component` step's
   `point`, with the same `threshold`, after deciding its name and role.
3. **Propose parts**: one `split_part_set {name, steps}` per part, in draw order from the back (hair back, body, head,
   face features, front hair). Prefer the cheapest step that is exact:
   - separated flat colours: `flood {point, tolerance}`; tolerance 0 to 8 for flat art;
   - an island on transparency: `component {point}`;
   - an existing pixel mask: call `split_mask_inspect {path}` on a source-sized, 8-bit grayscale PNG without
     alpha, then use the returned `step` (`raster {path, crc32, mode: "add"}`). Change `mode` to subtract or
     intersect when needed. Values are literal coverage, including soft edges; source alpha is applied later.
     Retain the PNG at the returned path: evaluation and plan replay reload it and verify the decoded checksum.
     Inspection creates no mask state or Parts and grants no provider consent. Older binaries cannot replay raster steps;
   - a region bounded by line art: `boundedFill {point, lineThreshold}`, then `grow {radius}` to take the outline
     with it, then `box {rect, mode: "intersect"}` to clip;
   - anything else: `polygon` or `box`, combined with `alpha {mode: "intersect"}` to keep only painted pixels;
   - "A except B": `part {name: "B", mode: "subtract"}`; "A under B": `part {name: "B"}` and order B after A;
   - a symmetric pair joined in the picture: one `flood`, then `side {axisX, keep: "left"}` for `.L` and
     `keep: "right"` for `.R` in a second part. `axisX` is the symmetry column.
4. **Read the statistics** that `split_part_set` returns for the part: `pixels` should match what you expect, `bbox`
   should be where the part is, `overlaps` lists parts sharing pixels. A part with 0 pixels means the seed point was
   on a transparent pixel or on line art.
5. **Overlay again**: `split_overlay {part: "Name"}` for one part, `{}` for all. Fix with another `split_part_set`
   (same name replaces it), `split_part_order`, `split_part_remove`.
6. **Plan**: `split_plan {}`. Require `unclaimedPixels` 0 (every visible pixel belongs to a part) unless you left
   something out on purpose, and `multiplyClaimedPixels` 0 unless you made an under-layer deliberately. Keep the
   returned `plan`; it replays without you.
7. **Apply**: `split_apply {}`, or `{automesh: true}` to mesh each new Part in the same undo step (`skipped` lists tiny
   parts that keep their quad; `{automesh: {"algorithm": "grid"}}` meshes all). `{parent: ID}` puts the Parts under a
   node whose own translation is zero. One `undo` removes all of them.
8. **Verify by render**: `render {width: W, height: H, bounds: [-W/2, -H/2, W - W/2, H - H/2]}` (integer halves) gives
   a PNG at the source scale; compare it with the source by eye, or `ViewCommand_CaptureLiveScreenshot`. The Parts
   must stack back to the picture. Set `padding: 2` to `4` on parts that will be automeshed or deformed, before the
   final apply: it adds transparent room around the texture (never colour), so mesh edges and filtering have space.
9. **Rig**: create a `DepthRigRoot` with `node_create`, then call `rig_add_skeleton {root}`.
   Read `rig_list_bones {root}` and use `rig_add_source {root, target: PART_ID, bone: BONE_ID}` for each moving Part.
   Match body Parts to their named bones; map face and hair Parts explicitly to `Head` or another intended bone.
   `Face::Roll` drives `Neck`. The standard `Head` stops parent motion from reaching its targets; if those Parts
   should follow Neck/Body motion, explicitly call `rig_set_constraint {bone: HEAD_ID,
   constraint: {allowParentToTargets: true}}`. Keep the default when independent Head targets are intended.
   `rig_fit_human {root}` places bones from matching Part names and computes weights for the registered targets.
   `rig_add_standard_parameters {root}` adds `Face::Yaw-Pitch`, `Face::Roll`, `Body::Yaw-Pitch` and `Body::Roll`.
   Read the parameters and rig bindings back, preview a value with `value_set`, render, then reset the value to zero.
   Skeleton, source registration, fitting and standard parameters are separate undo steps after the split apply.

To replay a saved plan with no agent: `split_apply {plan: <plan object>}` in any session with a document. The source
file named by the plan must still exist with the same size and the same pixels (the plan records a checksum).

## Taxonomy

Bone-matched names (exact, case sensitive; `.L` is the image left): `Pelvis, Spine, Chest, Neck, Head, Clavicle.L,
UpperArm.L, Forearm.L, Hand.L, Clavicle.R, UpperArm.R, Forearm.R, Hand.R, Thigh.L, Shin.L, Foot.L, Thigh.R, Shin.R,
Foot.R`. A bone-named Part should cover that segment only.

Bust-up, back to front: `HairBack`, `HairBack.L`/`.R`, `Chest`, `Neck`, `UpperArm.L`/`.R`, `Head`, `Ear.L`/`.R`,
`Eyewhite.L`/`.R`, `Iris.L`/`.R`, `Eyelash.L`/`.R`, `Brow.L`/`.R`, `Nose`, `Mouth`, `HairSide.L`/`.R`, `HairFront`,
`Accessory.*`. Full body adds `Pelvis`, `Spine`, `Clavicle`, `Forearm`, `Hand`, `Thigh`, `Shin`, `Foot` (each `.L`/`.R`),
`Skirt.Back`, `Skirt.Front`. Give moving face features their own Parts and make `Head` either exclude them
(`subtract` a `part`) or sit under them (`add` a `part`, ordered before it).

## Failure cases

- **0 pixels / empty bbox**: the seed is transparent, on line art, or tolerance is too small for shaded art. Move the
  seed into flat colour, or raise `tolerance`, or use `component`.
- **The fill leaks into a neighbour**: tolerance too high or no outline between regions. Lower it, use
  `boundedFill`, or clip with `box {mode: "intersect"}`.
- **Shaded or gradient areas split into pieces**: flood is flat-colour only. Use `component` plus `box`/`polygon`, or
  select the region with `alpha` and subtract what is not it.
- **Hair strands, anti-aliased edges lost**: edge pixels differ from the seed colour. `grow {radius: 1}` then
  `alpha {mode: "intersect"}`; do not expect soft alpha to survive `flood` exactly.
- **`unclaimedPixels` > 0**: a pocket the seed cannot reach (an island enclosed by another part). Add a second
  `flood` or `component` with `mode: "add"` for the pocket.
- **Large `multiplyClaimedPixels`**: parts overlap. Fine for an under-layer; otherwise `subtract` the other part.
- **Apply fails with "selects no visible pixels"**: a part is empty; fix or `split_part_remove` it.
- **A Part is doubled after a second apply**: `split_apply` adds Parts every time and keeps the draft; `undo` first. A
  parent with a non-zero translation shifts the Parts.
- **Draft gone after `new`/`open`/`import`**: those replace the document and drop the draft; reopen the image.
- **Occlusion**: hidden areas stay transparent, with no inpainting. Big moves of a front Part reveal a hole; grow
  the lower Part under it first (an under-layer).
- **A see-through seam where two feathered parts meet**: coverage c and 255-c composite to about 75% alpha at c=128.
  Do not feather both sides of a shared edge; grow the lower part under the upper one and feather only the upper.
- **"split work limit reached"**: too many or too costly steps for this image. Drop redundant steps, use `box` to
  clip before `grow`/`shrink`, and split the plan across fewer parts.
- **"clip the mask with a box first"**: a `grow`/`shrink` would work on more than 4 194 304 pixels (the mask box plus
  the radius). Intersect with a `box` around the part first.
- **"needs a WxH texture"**: the visible box plus `padding` exceeds 8192 pixels a side or 16 MP; lower `padding` or
  cut the part smaller.
- **"checksum mismatch" on replay**: the source file changed since the plan was made; restore it or redo the split.
- **Raster checksum mismatch / missing file**: restore the exact inspected mask, or inspect the replacement and
  set the part again. Cached editor previews can remain visible after an external edit; Apply rechecks the file.
  Retain referenced mask PNGs with saved plans. No model is called on replay.

For agent context, `./out/rust/debug/njc --port PORT read resource://soul2d/part-split/plan` returns the plan and statistics with `documentRevision` and `draftRevision`. This read changes no state. It fails if no split source is open or the document is not INP/INX. Save the nested `plan`, not the whole resource object, for `split_apply`.

Read `resource://soul2d/part-split/source` for the loaded RGBA PNG and a mask URI returned by `resources/list`
for its source-sized grayscale coverage. Each response uses MCP binary resource `blob`, not text.
The mask template is `resource://soul2d/part-split/masks/{name}`; percent-encode the name once.
Masks include feathering and references before source alpha, cropping or padding. Source reads do not reload disk.
Keep PNGs as image artifacts and log their hashes rather than embedding base64 image data in text transcripts.

## ACP editor entry

With an INP/INX split source open, enable **Coding agent** and start the configured ACP program.
The agent must advertise HTTP MCP support. **Split this image** fills an empty prompt with this Skill and
the current source/plan resource references; inspect it and press **Send**. Preparing the prompt sends nothing.
For this ACP path, pause after reviewing the proposed draft. Ask the person to use **Part split review** and
**Apply reviewed split** instead of step 7 above: the editor refuses `split_apply` from its ACP endpoint.
Other MCP mutations keep their existing access. Do not invoke external image providers; a provider interface
and enforced per-provider consent have not been implemented. This entry has fixture-agent tests; real AI-agent
and native GUI acceptance remain pending.

## Evidence

For human review in the editor, expand **Part split review** in the INP/INX Outline. Inspect the statistics and overlays, then reorder, remove, or explicitly apply the proposed Parts. A changed draft must be displayed for one frame before editing is enabled.

Read `applied` from `snapshot.partSplit`, `split_plan`, or the plan resource before applying. It describes the current plan's surviving original Parts on this history branch. Renames preserve it; Undo and deletion can clear it. Save keeps the in-session record, but reopening resets it. The panel blocks repeated application of that plan; raw `split_apply` still appends. See [editor review](../../../doc/part-split.md#editor-review).

Keep the tool transcript, the `plan`, the overlays and the render under `build/verification/<run>/`.
[tools/verify-part-split.py](../../../tools/verify-part-split.py) does this end to end on a generated fixture.
