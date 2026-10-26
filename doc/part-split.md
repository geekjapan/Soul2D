# Part split

Status: first slice of issue #100 ("Split an illustration into parts automatically"). A headless agent that only
speaks MCP turns one flattened RGBA illustration into several Parts. Deterministic Rust does the pixel work; the agent
supplies the prompts (points, boxes, thresholds). Code: `rust/src/part_split.rs`; tests: `rust/tests/part_split.rs`;
agent loop: `.cursor/skills/split-illustration/SKILL.md`; proof: `tools/verify-part-split.py`.

## Model

The draft is held by the editing `Session` outside the document and its undo history, like DepthDraw
(`Session.part_split`). It is dropped when the session replaces its document (`new`, `open`, `import.*`).
`snapshot.partSplit` summarises it: `{revision, open, width, height, parts}`. Draft operations never change the
document `revision`, the history or `modified`.

- **Source**: one image, straight alpha. PNG, TGA and baseline JPEG load (the decoders `merge.images` uses), and so
  does a PSD (see "PSD input"), up to 8192 pixels a side and 16 megapixels.
- **Part spec**: `{name, steps, padding}`. `parts` is the draw order, back to front; `index` of `split.part.set` and
  `split.part.order` move a part.
- **Mask**: one coverage byte per pixel, derived by evaluating the steps in order, never stored. The plan holds
  steps only.
- **Claimed** means coverage of at least 128. Every statistic and the grow/shrink steps use it.

### Steps

Selection steps take `mode`: `add` (default), `subtract` or `intersect`. The running mask starts empty.

| op | fields | selects |
| --- | --- | --- |
| `alpha` | `threshold` (0) | pixels with alpha above `threshold` |
| `component` | `point`, `threshold` (0) | the 4-connected alpha component containing `point` |
| `raster` | `path`, `crc32`, `mode` (add) | literal coverage from a source-sized grayscale-8 PNG inspected with `split.mask.inspect`; reloads and checks decoded coverage on every evaluation |
| `flood` | `point`, `tolerance` (16) | 4-connected pixels whose largest channel difference to the seed (alpha included) is within `tolerance` |
| `boundedFill` | `point`, `lineThreshold` (80) | like `flood` but stops at transparent pixels and at line art, pixels with luminance below `lineThreshold`; the seed must not be line art |
| `polygon` | `points` | even-odd polygon; vertices are pixel corners, `[0,0]` to `[width,height]` spans the image |
| `box` | `rect` = `[x, y, w, h]` | a rectangle |
| `part` | `name` | the mask of another part of the plan (acyclic) |

Morphology steps have no mode and act on the running mask:

| op | fields | effect |
| --- | --- | --- |
| `grow` | `radius` 1-32 | Euclidean grow of the claimed pixels, result binary; the mask box grown by `radius` must be at most 4 194 304 pixels, else clip first with a `box` |
| `shrink` | `radius` 1-32 | Euclidean shrink of the claimed pixels, result binary; same area limit |
| `feather` | `radius` 1-32 | box blur of the coverage, which makes soft edges |
| `side` | `axisX`, `keep` (`left`/`right`) | keeps `x < axisX` (`left`) or `x >= axisX` (`right`): the symmetry split |

A seed that fails its own test (a transparent pixel for `component`, a line pixel for `boundedFill`) selects nothing;
the statistics show it as 0 pixels.

### Validation

Checked at the boundary before any state changes: points inside the image, rects inside the image and non-empty,
polygons of 3-1024 vertices inside the image, radius 1-32, `padding` 0-64, `axisX` 1 to width-1, name 1-128 bytes
without control characters and unique, 1-64 steps, at most 32 parts, `part` references existing and acyclic,
unknown fields and unknown ops refused. After every `split.part.set` the textures of ALL parts are checked, not only
the edited one, because a part that refers to another changes when that one changes. A texture (visible box plus
`padding`, rounded to even) may be at most 8192 pixels a side and 16 megapixels, and all textures together at most
33 554 432 pixels (twice the source limit). An edit that breaks either limit is refused and leaves the draft as it
was, so a draft that passed `split.part.set` can be applied; `split.apply {plan}` runs the same checks.
`split.part.remove` refuses a part another part refers to. A failed request leaves the session unchanged. Messages
name the path the caller passed, never a resolved one.

### Limits on work and memory

Masks are derived on every `split.part.set`, `split.plan` and `split.overlay`, so evaluation is bounded:

- **Work**: one request has 512 work units per source pixel, counted over everything it does (`WORK_PER_PIXEL`;
  16 MP gives 8.6 billion units). Pixel visits and raster encoded bytes consume units. The limit bounds work, not seconds:
  the time per unit depends on the step and
  the machine, and it has not been measured here. Costs per pixel: a selection step 2 visits per pixel, a `flood`
  adds 4 per filled pixel, `grow`/`shrink` 3 per pixel plus 5 per pixel of their working area, `feather` 4, `side` 1,
  `polygon` 2 per pixel plus rows x vertices; the texture check 1 per part; the statistics 4 per part plus, for each
  pixel claimed by c parts, the number of parts plus c (c - 1) / 2 for the overlap pairs; the overlay 1 plus 5 per
  drawn part. 32 parts that all claim every pixel cost 32 x 4 + 32 + 496 = 656 visits per pixel in the statistics
  alone and are refused. A 16 MP image with a few dozen steps over a handful of parts uses 100 to 300 visits per
  pixel. When the limit is hit the request fails with "split work limit reached" and changes nothing. Overlap counts
  live in one flat `n x n` table, with no allocation per pixel.
- **Morphology area**: `grow` and `shrink` transform only the bounding box of the claimed pixels grown by the radius,
  at most 4 194 304 pixels. The distance transform holds about 37 bytes per pixel, which is 155 MB at the limit.
- **Stored masks**: parts x pixels at most 256 MiB.
- **Raster input**: at most 32 MiB encoded per PNG. Before reading file data, each load spends one budget unit
  per encoded byte plus two per source pixel (decode and CRC); normal selection/combination adds two per pixel.
  Inspection has its own budget; setting a part validates the complete prospective plan first, then evaluates
  all raster steps within the shared request budget. One conservative transient allowance is 32 MiB encoded
  + 64 MiB PNG decoder allowance + 16 MiB coverage at the source limit, in addition to existing masks/accumulator.
  Each decoded input is dropped after combination. These are allocation allowances, not measured process memory.
- **Textures**: all textures of a plan together at most 33 554 432 pixels, which is 128 MiB of RGBA.
- **Transient ceiling** (estimated from buffer sizes, not measured):
  - draft operations: source 64 MiB (16 MP RGBA) + masks 256 MiB + the largest single step, which is the
    `grow`/`shrink` transform (155 MB plus its 20 MB of masks) or a flood (a 16 MiB mask and a stack of 4 bytes a
    pixel, 64 MiB at most). A 6 MP image with ten parts stays under 150 MiB.
  - `split.apply`: while rendering, source 64 MiB + masks 256 MiB + the PNG data so far (at most 128 MiB, a PNG is
    smaller for flat art) + one texture buffer (at most 64 MiB), about 0.5 GiB. After the masks are dropped the
    merge keeps up to about five copies of the PNG data (the rendered images, the slot list, the compacted list, the file
    blobs and the document sections), at most 0.7 GiB with the 64 MiB source.
  - `split.overlay`: masks 256 MiB + source 64 MiB + the RGBA buffer 64 MiB + its PNG (at most 64 MiB) + base64 text
    (85 MiB), about 0.5 GiB. The MCP layer then decodes and re-encodes that base64 text and clones the result, so
    five copies of 85 MiB plus the draft source (64 MiB) are alive for a moment, and serialising the response adds
    one more: about 0.6 GiB for a 16 MP source. A 4 MP source needs a quarter of that.

## PSD input

`split.open {path}` takes a `.psd` (matched on the extension, case-insensitively). The draft works on the flattened
document, built the cheapest correct way the crate offers: the layers go through the PSD importer and the CPU
renderer draws the result at the document size. One thing differs from `import.psd`: each layer's quad covers
every texel. A layer import halves the size with integer division, as D does, so a layer of odd width or height is
drawn one pixel short there; the split source keeps the last row and column
(`an_odd_sized_psd_keeps_its_last_row_and_column`).
That is the compositing path of the editor itself, so groups, layer opacity, clipping, hidden layers and the blend
modes the importer maps (Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, LinearDodge, ColorBurn, HardLight,
SoftLight, Difference, Exclusion, Subtract, LinearBurn, LinearLight, Hue, Color) behave as they do after an import.
What it means:

- The merged image stored in the file is **never read**. A PSD saved without a merged image (Maximize
  Compatibility off) works, and so does one whose stored composite is stale. The price is that the picture is the
  importer's rendering, not Photoshop's: a blend mode the importer does not map (Dissolve, Saturation, Luminosity,
  Vivid light, Pin light, Hard mix, ...) draws as Normal, and adjustment or text layers show only their stored
  pixels.
- A PSD **without any layer record** (a flattened file, whose only pixels are the merged image) is refused with "the
  PSD has no layer with pixels". Export it as PNG instead. A single-layer or under-separated PSD is the case this is
  for: its layers composite into one picture and the split cuts the parts the artist did not.
- The PSD reader's limits apply unchanged (version 1, 8-bit RGB, 100 million pixels a document, 32 768 layers, the
  512 MiB file limit), and the document must also pass the PNG checks: at most 8192 pixels a side and 16 megapixels.
  The size check runs before any layer is imported.
- The picture is drawn in **horizontal strips** when one render cannot hold it. The renderer allows 100 million pixel
  visits a render and a layer costs two visits for every pixel of its box, so an ordinary layered PSD goes past that
  at full size. It also allows 640 MB of target-sized layers, one for the picture and one for every nested group
  being drawn, which a large canvas with nested groups goes past. Both shrink with the strip.
  `split.open` starts with one strip and doubles the count until every strip fits, up to 64; a file
  that needs more is refused with "the layers need more render work than 64 strips of the renderer's limit allow".
  The decoded layers count against the same 640 MB and no strip shrinks them, so a PSD whose decoded layers exceed
  640 MiB cannot be flattened. After the first memory refusal `split.open` draws one row; if that is refused too, it
  stops there without doubling and answers "the layers need more working memory than the renderer's 640-megabyte
  limit allows", as it does when the strip-sized layers are still over the limit at 64 strips.
  A strip is the same render with other bounds, so the rows are the rows of the whole picture (see "Real files").
- **Clipping preserves the base alpha.** For the split picture, each base and its clipped layers form one
  offscreen stack. Clipped colour replaces the colour within the base's fractional alpha, while the stack keeps
  that alpha. The base's opacity and blend mode apply once to the stack, including a group used as its base.
  This follows [Adobe's clipping-mask description](https://helpx.adobe.com/photoshop/using/revealing-layers-clipping-masks.html).
  The stack counts toward the renderer's existing memory and work limits. Where the stacks would nest deeper than
  the renderer's 16 Composites, the whole picture is drawn with the binary masks of `import.psd` instead. Ordinary
  `import.psd` models retain their existing binary masks. Previously saved PSD split plans whose pixel checksum
  differs under this correction are refused by the existing checksum check; reopen the PSD to regenerate them.
- A refusal reads `Could not load <file name>: <reason>`. The directory of the file is never in it.
- The flattening is rendered at 8 bits through premultiplied textures, so a soft edge can differ from a PNG of the
  same layers by up to 2 in a channel (see the quality measure).

## Operations

One MCP tool per operation; the tool name replaces `.` with `_` (`split_part_set`).

| op | arguments | result |
| --- | --- | --- |
| `split.open` | `path` | `{width, height, revision}`; loads the source (PNG, TGA, JPEG or PSD), clears parts |
| `split.part.set` | `name`, `steps`, `padding?`, `index?` | the part's statistics, `index`, `parts`, plan-wide counts, `revision`. Upserts by name; evaluates every part, so it costs one full evaluation |
| `split.part.remove` | `name` | `{removed, parts, revision}` |
| `split.part.order` | `names` | the new order; `names` must be a permutation |
| `split.plan` | | `{plan, parts: [statistics], unclaimedPixels, multiplyClaimedPixels}` (read-only) |
| `split.components` | `threshold?`, `offset?` | read-only source islands; `{components: [{seed, pixels, bbox}], totalComponents, nextOffset}`. See below |
| `split.mask.inspect` | `path` | read-only; `{step: {op: "raster", path, crc32, mode: "add"}, width, height}` for a source-sized grayscale-8 PNG. The returned path is absolute |
| `split.overlay` | `part?` | image result (`png` base64, then the MCP image content) of the source with each part tinted and outlined; with `part`, only that one. Full source size, no downscale |
| `split.apply` | `parent?`, `plan?`, `automesh?` | creates one Part per part; `{created: [{id, name, size, trans}], parent, automesh: {meshed, skipped}}` |
| `split.close` | | drops the draft |

`split.components` enumerates four-connected source pixels with alpha strictly greater than `threshold`
(integer 0..255, default 0), regardless of the current masks. Seeds are the first pixels in row-major order;
`bbox` is `[x,y,width,height]`, including every selected pixel. Use the returned seed as a `component` step's
`point` with the same threshold. It discovers islands, not semantic parts, and creates no masks or Parts.
Results are capped at 32 per page. Start with `offset: 0`, then follow `nextOffset` with the source and threshold
unchanged; null means no further entries. An offset beyond the exact `totalComponents` returns an empty page
and null. Every page scans the whole source to obtain the exact total, so exhaustive paging of noisy art can
be expensive (a 16 MP checkerboard requires 262,144 pages). There is no cache or persistent label map.
One request charges one visit per source pixel and four per accepted pixel under the existing budget. It uses
one visited byte and at most one `u32` stack entry per pixel: an estimated 80 MiB of extra buffers at 16 MP,
plus bounded page metadata. These are allocation bounds, not measured runtime or memory usage.

`split.mask.inspect` validates a local mask without changing draft, document, revisions or history. Use the returned
`step` in `split.part.set`. Raster steps require `crc32`; missing, null or malformed fingerprints are refused.
The fingerprint is CRC32 of decoded L8 samples, like the source fingerprint: stale-content detection, not authentication.
The original PNG must be grayscale, 8 bits per sample and have no alpha; palette, RGB, sub-byte, 16-bit and grayscale
transparency are refused. Dimensions must exactly match the loaded source. Coverage is not multiplied by source
alpha until texture extraction, and combines/references/morphology work as for other selections.

Keep referenced mask files with the source and saved plan. No mask bytes are stored in the plan or retained in a
cache; every evaluation and apply/replay reloads the PNG and refuses a missing or changed decoded mask. Encoded
metadata changes that preserve coverage do not change the fingerprint. Replay calls no model. Plan version 1
remains valid for old steps; an older binary refuses the new raster step. The editor caches review previews by
draft revision, so an external file edit may leave an older preview visible. Apply always evaluates again and
refuses changed input. `applied` records plan lineage, not the present validity of external files.

Statistics per part: `pixels` (claimed), `bbox` (`[x, y, w, h]` of the pixels whose texture alpha is not zero, or
`null`), `meanCoverage` and `meanAlpha` (mean mask value and mean source alpha over the pixels the mask touches, 0-1),
`overlaps` (other parts sharing claimed pixels, with the count). Plan-wide: `unclaimedPixels` is the pixels with
source alpha above 0 that no part claims, `multiplyClaimedPixels` the pixels that more than one part claims.

### Apply

`split.apply` needs an INP/INX document (like `merge.images`) and creates the Parts through `add_image_parts` in ONE
commit, so one `undo` removes all of them and one `redo` restores them. Parts are appended under `parent` (default
the root) in plan order; the renderer draws later siblings of equal `zsort` on top. For each part:

- The texture is source RGBA times the mask, cropped to the box where the result is not transparent, padded by
  `padding` pixels on every side and rounded up to an even size (the Part quad is `size / 2` per side, so an odd
  texture would lose a pixel). It is stored premultiplied, like every imported image.
- `padding` is transparent: every texel with alpha 0 is `[0, 0, 0, 0]`. It gives automesh and texture filtering room
  around the edge. It is deliberately not colour bleed: the renderer treats textures as premultiplied and adds the
  colour of an alpha-0 texel to what is behind it, so a bled ring draws as a bright halo around the Part.
- The Part is translated like a layer import: `trans = topLeft + size / 2 - sourceSize / 2` with integer halves,
  y down. It therefore sits where it was in the source when `parent` is at the origin (the root is); a parent with its
  own translation shifts the whole result.
- A part that selects no visible pixel fails the whole apply and nothing is created.
- `automesh: true` runs `automesh.generate` with the `contour` algorithm on each new Part inside the same commit; an
  object is used as that request (`{"algorithm": "grid"}` meshes every part), and its `preset` may name a preset
  saved with `automesh.preset.save`. `contour` gives no mesh to very small or
  thin parts (a 6x6 eye): they keep their quad and are listed in `skipped`.
- With `plan`, the source named by `plan.source.path` is loaded, its size and its CRC-32 (over the decoded RGBA
  bytes, `plan.source.crc32`) must equal the recorded ones, and the plan is applied without touching an open draft.
  The checksum catches a different or edited file at the same path; it is not tamper proof. Applying the same plan from a draft and from `plan` gives identical
  documents.

## Plan format

```json
{
  "version": 1,
  "source": {"path": "/abs/character.png", "width": 96, "height": 128, "crc32": 2864434397},
  "parts": [
    {"name": "Head", "padding": 0, "steps": [{"op": "flood", "point": [48, 60], "tolerance": 8, "mode": "add"}]},
    {"name": "HairBack.L", "padding": 0, "steps": [
      {"op": "flood", "point": [26, 50], "tolerance": 8, "mode": "add"},
      {"op": "side", "axisX": 48, "keep": "left"}]}
  ]
}
```

The `box` step is written `{"op": "box", "rect": [x, y, w, h]}`. `split.plan` returns this object under `plan`; save
it and pass it to `split.apply` as `plan` to replay without an agent.

## Defaults decided for this slice

- Inputs are PNG, TGA, JPEG and PSD files (flattened, see "PSD input").
- Hidden areas (what a front layer covers) stay transparent; no inpainting, no colour bleed, no segmentation model,
  no provider interface, no network. To avoid a hole when a front Part moves, grow the lower Part under it. The agent's points and boxes are the only model input.
- `grow`/`shrink` binarise at coverage 128; `feather` is a clamped-edge box blur done with running sums, so its cost
  does not depend on the radius. `polygon` is an even-odd scanline fill, O(rows x vertices + pixels).
- The overlay is full resolution; there is no zoom or crop.
- Quality measure: per-part IoU against the visible region of each original layer on a generated layered fixture
  (`rust/tests/part_split.rs`, `tools/verify-part-split.py`), plus the stacking check (the applied Parts, composited
  at their offsets, equal the premultiplied source pixel for pixel when masks are disjoint and there is no feather),
  and the applied document drawn by the CPU renderer matches the source picture.
- `split.*` is refused on native Live2D and animation documents by the existing allowlists.
- The draft is not persisted and not part of undo.

## Quality measure

`rust/tests/part_split_psd.rs` (`cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust --test
part_split_psd`) generates a 64 x 64 layered picture, so no third-party artwork is involved. Back to front it has
`Back` (a large rectangle), `Hidden` (entirely behind `Mid`), `Mid`, `Touch` (the same colour as `Mid`, sharing an
edge with it), `Cap` (inside `Back`), `SoftAlone` (a disc with a 3 pixel soft rim on a transparent canvas) and
`SoftOver` (a disc whose rim blends with a corner of `Back`). The ground truth of a layer is the pixels where it is
the topmost layer with alpha of at least 128 (the claimed threshold). The test flattens the layers, writes the result
as a PNG (and as a PSD with the same layers plus a hidden full-canvas layer), opens it with `split.open`, builds the
parts with `split.part.set` only, calls `split.apply`, and measures:

- **IoU** of each Part against its layer: the pixels where the applied texture has alpha of at least 128, read back
  with `texture.get` and placed by the Part's translation, against the ground truth. Nothing was added to the
  session for this.
- **Reproduction**: the applied document drawn by the `render` op against the flattened picture, as the mean and the
  largest difference of a premultiplied 8-bit channel (alpha included), over all 4096 pixels.

| Layer | steps of the plan | IoU | |
| --- | --- | --- | --- |
| `Mid`, `Touch` | `flood` tolerance 0, then `side` at the shared edge | 1.000 | same colour, touching: geometry cuts them |
| `Cap` | `flood` tolerance 0 | 1.000 | overlapped by nothing, inside `Back` |
| `SoftAlone` | `component` | 1.000 | the soft rim stays soft in the texture |
| `Back` | `flood` tolerance 10 | 0.984 | loses the faint rim pixels that `SoftOver` blends into it |
| `SoftOver` | `flood` tolerance 0, then `grow` 4 | 0.921 | the grow over the rim also claims `Back` pixels beside it |

Reproduction: mean 0.022 and largest 1.12 from the PNG source, mean 0.041 and largest 1.98 from the PSD source. The
plan leaves no pixel unclaimed. The asserted floors are IoU 0.97 for `Back`, 0.90 for `SoftOver` and 0.99 for the
rest, and for reproduction a mean of at most 0.06 and a largest difference of at most 2.5.

Limits the measure shows (not hidden by the thresholds):

- **A layer that is entirely hidden cannot be recovered.** `Hidden` owns no pixel of the flattened picture and its
  colour is not in it. A part over its area (`box` or `polygon`) claims exactly the pixels of the layer in front of
  it (the test sees 64 shared pixels with `Mid`), so there is no ground truth to score and no pixel to give the part.
  Hidden areas stay transparent, see "Defaults". The test leaves `Hidden` out of the plan.
- **A soft rim that blends with another layer cannot be un-mixed.** A part is a binary-ish mask over the source
  pixels; the blended pixels belong to the front layer in the picture and to the back layer in the layers. Growing
  the front part far enough to reproduce the picture (no unclaimed pixels) costs IoU on the front part (0.921 with
  `grow` 4); a smaller grow scores higher IoU and leaves faint rim pixels unclaimed, which the reproduction check
  then shows as holes of up to 255 in alpha. The test picks the reproducing recipe.
- Colours cannot be told apart by colour alone when layers share one: the test needed the geometric `side` cut.

## Real files

`split.open` was run on the eight PSDs of the Live2D sample models kept under the ignored `build/` directory
(2026-10-13, release build, macOS arm64). The files are third-party artwork: nothing from them is in the repository
and only numbers are recorded here.

- One file is CMYK and is refused by the PSD reader ("Only RGB PSD documents are supported").
- The seven RGB files open. Four need one render, and the others 2, 4 and 16 strips; the last is 2400 x 4500 pixels
  in a 44 MB file. Before the strips, those three were refused with the renderer's work limit.
- Time for `split.open`: 0.2 to 1.5 s for six files, 8.2 s for the one that needs 16 strips.
- Seams: each imported file was rendered once whole and once in five strips through the `render` op, and the two
  pictures compared. All seven are identical in every pixel. Four were compared at full size; the three that do not
  fit one render were compared at half or quarter size, where one render holds them.

No part plan was made for these files, so this says the source opens and is the same picture in strips. It does not
measure split quality on real artwork.

### Artist-layer baseline

`tools/verify-part-split-quality.py` measures the pinned, creator-declared CC0 Alien T-Rex PSD acquired locally
under `build/verification/issue100-fixture-inventory/cc0-alien/`. Only the pinned Alien and Soldier PSD hashes are
accepted; the verifier does not fetch
artwork or call a provider. Run it with `--core`, `--njc` and `--psd` paths. Images and receipts stay under
`build/verification/issue100-quality/`.

The independent Python PSD decoder extracts 18 artist-layer alpha masks. All layers are visible, normal blend,
opacity 255, with no masks or clipping. The harness pins their paths and file order and creates its own source-over
PNG in RGB byte values. That generated PNG is the benchmark input; equivalence to the PSD's stored or displayed
appearance is unverified. Both head layers remain as authored; no human part taxonomy is inferred.

The production MCP baseline finds 49 alpha-positive four-connected islands. To fit the 32-Part cap, it keeps the
largest 31 individually and unions the others. Predictions receive no reference layer masks or names.
For each artist layer, the score is its best alpha-at-least-128 IoU among these fixed masks. The measured mean is
0.0896064 over 18 layers. Matching is nonexclusive: it bounds one-to-one matching of these predictions, not the
quality another segmentation method could attain. This baseline does not recover the artist's parts.

Applying the plan and CPU rendering preserves alpha exactly. The largest premultiplied RGB error is 1.1844/255
(rounded up here), with mean 0.00618/255. Every channel is checked against the existing texture quantization:
`P = floor(C*A/255)`, followed by nearest-integer unpremultiplied PNG output. Thus
`abs(R*A/255 - P) <= A/510`, allowing `1e-5` for floating-point ties. The worst pixel and both RGBA values are
retained. This is a reconstruction check of the chosen input, not a semantic-quality acceptance.

The receipt reports `BASELINE_MEASURED` and `qualityAcceptance: UNSET`. Human bust/full-body quality targets,
semantic segmentation, hidden-content reconstruction and real provider/agent workflows remain unfinished.

### Fixed operator comparison

The same verifier also accepts `--method flood` or `--method bounded_fill`; omitting `--method` keeps the
alpha-island baseline. Operator runs give the product only the generated flattened PNG. Artist masks, names,
rectangles and layer count stay outside the predictor. The predictor reads the product's loaded RGBA resource.

Each profile chooses the most frequent exact RGB among eligible uncovered pixels. Color ties use the first
row-major occurrence; within that color, the highest-alpha pixel wins, with row-major ties. Flood eligibility
is alpha above zero and its tolerance is pinned to 16. Bounded fill additionally requires integer luminance
`(299R + 587G + 114B) / 1000 >= 80`, and its line threshold is pinned to 80. Bounded fill ignores color after
choosing a seed. These are deterministic program operations, not semantic or model predictions.

The product computes each raw selection and intersects it with alpha support. The predictor removes pixels
already claimed, writes a binary grayscale PNG, asks `split_mask_inspect` for its CRC-bound raster step, and
reads back the product mask. It keeps up to 31 regions plus a nonempty alpha residual. Materialized raster
steps avoid a quadratic chain of Part references within the existing work budget. Final masks must be disjoint
and exhaustive, and applying them must pass the same quantized reconstruction check as the baseline.

Before real scoring, freeze the code, seed policy and parameters in a commit and retain its hash. Do not tune
them after seeing artist-layer scores. `--check-operators` replaces `--psd` and checks the baseline and both
operator profiles on generated inputs, including the 32-Part cap, residual coverage and transparent-RGB exclusion;
it never opens a PSD or computes artist-layer IoU. Operator receipts stay under
`build/verification/issue100-operator-quality/`. Repeat fixed real runs to check deterministic masks and scores.
The per-layer best-IoU metric, one nonhuman fixture and unset semantic acceptance retain the limits above.

The profiles were frozen before six real trials on
the pinned Alien PSD. Each method ran twice with identical predicted masks and scores, and all six runs
passed alpha-support partition and quantized reconstruction checks. Each produced 32 predictions.

| Fixed method | Mean best artist-layer IoU | Operator residual pixels / 8,655 source-support pixels |
| --- | ---: | ---: |
| Alpha components | 0.0896064 | n/a |
| Flood, tolerance 16 | 0.1747113 | 1,993 |
| Bounded fill, threshold 80 | 0.0939103 | 8,308 |

The cap and seed policy limit these results: Flood produced 18 one-pixel parts, and bounded fill left about
96% of source-support pixels in its residual. Repeated deterministic runs establish repeatability on this
input, not statistical uncertainty or semantic superiority. Parameters were not tuned after scoring.
Receipts and decoded-mask checks are retained in `build/verification/issue100-operator-quality/`.

### Pinned human fixture contract

The verifier also recognizes [DeadKir's CC0 2D soldier](https://opengameart.org/content/2d-soldier), pinned by
SHA-256 `505b9b3a670ca14e0cf2e1bb953698791fdce08ef79e56383e1891549cb53dcd`. It is a 101×161 RGB8 PSD.
All 14 visible normal artist layers are retained in original order, including the uniform opaque black
background. Original names and file-layer indices are pinned; no anatomical relabeling or background exclusion
is performed. The reference is therefore opaque across the canvas. The stored merged RGB diagnostic receives
alpha 255; it remains separate from the independently composited reference and artist masks.

The same fixed prediction profiles and per-layer best-IoU metric apply. Soldier receipts identify the fixture,
creator page, layer indices and background contract and use `issue100-soldier-quality/` or
`issue100-soldier-operator-quality/` under `build/verification/`. Keep its results separate from Alien results.
Freeze this expanded protocol before Soldier scoring, repeat each method twice, and retain an Alien regression.
This small side-view human fixture does not establish an accepted bust/full-body taxonomy or quality target.

The expanded protocol was fixed before Soldier scoring.
Three Alien regressions exactly reproduced the prior masks, scores and per-layer best matches. Each Soldier
method then ran twice with identical masks and scores. All nine runs passed partition and reconstruction checks.

| Fixed method on Soldier | Predictions | Mean best artist-layer IoU | Residual pixels / 16,261 source-support pixels |
| --- | ---: | ---: | ---: |
| Alpha components | 1 | 0.0981489 | n/a |
| Flood, tolerance 16 | 32 | 0.3463671 | 1,079 |
| Bounded fill, threshold 80 | 32 | 0.1128515 | 15,967 |

The opaque background accounts for the single alpha component. Bounded fill left about 98% of source-support
pixels in its residual and produced 10 one-pixel parts. These fixed results do not establish anatomical naming,
hidden-content recovery, semantic superiority or an agreed IoU target. No parameters were tuned after scoring.
Receipts and decoded-mask checks are retained in `build/verification/issue100-human-quality/`.

## Draft context resource

`resource://soul2d/part-split/plan` exposes the open split draft as a read-only MCP resource. Its JSON contains the same `plan`, `parts`, `unclaimedPixels`, and `multiplyClaimedPixels` as `split_plan`. `documentRevision` and `draftRevision` identify the Session state at the read.

The resource is listed for INP/INX documents. A read fails with JSON-RPC error `-32002` if no split source is open or the document is not INP/INX. Reading the resource changes neither the draft nor document history. The nested `plan` replays through `split_apply`; the whole resource object is not a replay plan.

### Source and mask images

`resource://soul2d/part-split/source` returns the loaded source as an RGBA PNG.
Its pixels include RGB under transparent alpha. Reading does not reload the file; `split_open` does.
The PNG is re-encoded from loaded pixels and does not preserve the original file's encoding or metadata.

`resource://soul2d/part-split/masks/{name}` returns a Part's canonical mask as an 8-bit grayscale PNG.
Percent-encode the name once, including reserved characters. The mask has the source dimensions and coverage
values from 0 to 255, including feathered edges and Part references. It is not cropped or padded and does not
multiply coverage by source alpha. Use the plan resource to identify document and draft revisions.

Both reads return MCP binary resource contents (`mimeType: image/png`, base64 `blob`) and change no state.
`resources/list` includes the source descriptor for INP/INX documents and concrete mask descriptors for the
open draft; it contains no image bytes. `resources/templates/list` includes the mask template.
Malformed mask selectors return `-32602`; a missing Part, closed source, or native document returns `-32002`.
Closing the draft or opening another source clears its masks. Images are not persisted in the plan or document.
`tools/verify-part-split-context.py` checks these resources through the production `njc` client and an owned MCP host.

## Editor review

For an INP/INX document, expand **Part split review** in the Outline after preparing a draft through MCP.
The panel shows source dimensions, ordered Parts, pixel counts, bounding boxes, and coverage warnings.
Select a Part to preview its mask, move it forward or backward, or remove it. **Preview all parts** shows the combined overlay.
The preview shrinks to the GPU texture limit without changing the source, masks, or statistics.

**Apply reviewed split** adds the current draft as one undo step. A changed document or draft must appear for one frame before editing is enabled.
Empty masks cannot be applied. Removing a Part referenced by another Part reports an error and keeps the draft.
The panel blocks a repeated application of the same plan while any original Part remains. A different reviewed plan can append Parts with the same names.
To replace an earlier result, undo that application before applying the revised draft. Raw `split_apply` calls always append.

`snapshot.partSplit.applied`, `split_plan.applied`, and the plan resource report the same advisory boolean.
It compares the source path, dimensions, pixel checksum, and ordered Part specifications against applications on the current history branch.
Renaming created Parts preserves it. Undoing their application clears it, and Redo restores it.
Deleting every original Part clears it permanently on that branch, even if later Parts reuse their IDs.
History pruning, clearing history, and saving preserve live application records in the Session. New, open, and import reset them.
Records are never saved in the document. A closed draft reports false; reopening an identical draft can match a surviving application.

## ACP entry

Open a split source through `split_open`, then expand **Coding agent**, enable it and start the configured ACP
program. The initialized agent must advertise HTTP MCP support. **Split this image** prepares a prompt in an
empty message box, including the embedded split Skill and references to the current source and plan resources.
It starts no process, sends no prompt and changes no document. Inspect the text, then press **Send**.

The agent can propose and revise the draft through the editor's existing authenticated MCP endpoint.
Its `split_apply` calls are refused, including replay plans and notifications. A person must inspect the current
draft in **Part split review** and press **Apply reviewed split**. Other MCP clients retain their existing apply path.
This gate covers `split.apply` only; other mutating MCP operations retain the Coding agent panel's existing access.

The entry, ACP wire and apply gate are tested with a fixture agent and headless egui input.
A native-window check also verified Send, draft creation, apply refusal, human review/apply, viewport rendering,
and Undo/Redo with a deterministic local fixture agent. Real AI segmentation, provider calls and character-quality
acceptance remain pending. The configured ACP program remains
an arbitrary process with the panel's existing MCP access; this is not a per-provider image-consent interface.
The prepared prompt prohibits provider calls. Segmentation/inpainting provider integration and enforced consent
for those calls remain unfinished.

## Not built yet

Provider interface and per-provider consent for segmentation or
inpainting, persistent per-pixel component labels, per-part z-order beyond sibling order.

Built since: `texture_set` (op `texture.set`) replaces the pixels of an existing texture slot from a file or
base64 PNG as one undo step, see `doc/mcp_api.md` (Texture Tools).

## Soft edges and seams

`feather` makes coverage fall off across an edge. Two parts that share an edge and are both feathered do not add up:
coverage c on the lower part and 255-c on the upper one composite to about 75% alpha at c=128, so the picture behind
shows through a seam. Cut a shared edge with one hard side: grow the lower part under the upper one (an under-layer,
which `multiplyClaimedPixels` counts on purpose) and feather only the upper part, or leave both hard. Never feather
both sides of one edge.

## Part taxonomy

Name Parts so `rig.fit_human` can use them. It matches a Part (or GridDeformer) whose `name` equals a bone name
exactly and places that bone from the combined bounds of the matches; unmatched bones keep their rest pose. The
standard bones are `Pelvis, Spine, Chest, Neck, Head, Clavicle.L, UpperArm.L, Forearm.L, Hand.L, Clavicle.R,
UpperArm.R, Forearm.R, Hand.R, Thigh.L, Shin.L, Foot.L, Thigh.R, Shin.R, Foot.R`. In the skeleton, `.L` is the
character's side that is at negative x, which is the image left, so `side {keep: "left"}` gives `.L`.
A Part named like a bone should cover that bone's segment only: put hair and accessories under other names so they do
not stretch the bounds.

Create a DepthRigRoot and add the standard skeleton, then register each moving Part with `rig.add_source`.
Fitting stores weights only for registered targets. Assign face and hair Parts to a chosen bone explicitly;
their taxonomy names do not register bindings. `rig.add_standard_parameters` then adds the four Face/Body
yaw-pitch and roll parameters. Preview their runtime values, render, and reset to neutral before saving.
`Face::Roll` drives Neck. The standard Head has `allowParentToTargets: false`; explicitly enable that flag
through `rig.set_constraint` when Head-bound Parts should follow Neck/Body motion. The default keeps those
targets independent of parent motion. Adding parameters alone does not change that choice.
`tools/verify-part-split.py` checks this chain on its generated fixture, including parameter Undo/Redo and
preservation of the split's applied record. The existing large-grid dirty-queue test also exercises the record
through partial deformation, staged save, Undo/Redo and reopening without persisted origins.

Face and hair names are not bone-matched; they follow the same `.L`/`.R` style.

| | bust-up (draw order, back to front) | full body adds |
| --- | --- | --- |
| Back | `HairBack`, `HairBack.L`, `HairBack.R` (a symmetric hair mass: one `flood` then two `side` splits) | `Skirt.Back`, `Cape` |
| Body | `Chest`, `Neck`, `UpperArm.L`, `UpperArm.R` (visible shoulders) | `Pelvis`, `Spine`, `Clavicle.L/R`, `Forearm.L/R`, `Hand.L/R`, `Thigh.L/R`, `Shin.L/R`, `Foot.L/R`, `Skirt.Front` |
| Face | `Head` (skin base, eyes cut out or covered), `Ear.L`, `Ear.R`, `Eyewhite.L/R`, `Iris.L/R`, `Eyelash.L/R`, `Brow.L/R`, `Nose`, `Mouth` | |
| Front | `HairSide.L`, `HairSide.R`, `HairFront` (fringe), `Accessory.*` | `Weapon`, `Accessory.*` |

Full body also wants the standard bone parts split at the joints; cut a limb with a `polygon` or `box` at the joint
and keep the pieces adjacent so the stack still equals the source. Parts that must move independently of the face
(eyes, brows, mouth) are cut out on their own and the `Head` part either excludes them (`subtract` a `part`) or sits
under them (`add` a `part`, drawn first) so there is no hole when they move.
