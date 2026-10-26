# Real-file verification

The PSD, KRA and JPEG importers were first tested on generated and hand-built inputs. `tools/verify-real-files.py`
runs them on files made by other programs and compares the result with decoders written independently of this
code. The files are read in place; they are never copied into the repository or committed.

```sh
./tools/build-rust-core.sh --locked
python3 -I tools/verify-real-files.py DIR_OR_FILE...   # PSD, KRA and JPEG files, found recursively
python3 -I tools/verify-real-files.py @list.txt        # one path per line
```

The tool needs only the standard library and `out/rust/debug/soul2d-core`. It uses `sips` (macOS) and `djpeg`
(libjpeg) as JPEG references when they are on `PATH`. It writes `build/verification/real-files-<time>/report.json`
(`--out` changes the directory) and exits 1 when a threshold fails, 2 when the setup is wrong. Every threshold is
an option; `--help` lists them. A PSD run takes minutes in a debug build (the largest layered sample imports in
about 75 s in a debug build). A grouped file with many clipped layers is much slower in a debug build, so use
`--core` with a release build of `soul2d-core` for the split files; the flat Haru import gives 1.17 in both.

## PSD

For each file the tool parses the layer records itself (header, layer records, `luni` names, `lsct` section
dividers, channel data, merged image) and then drives `import.psd` and `render` through the JSONL protocol:

- the imported tree must have the same parts and groups, in the same nesting and order, with the same names,
  visibility, opacity, mapped blend mode and canvas position as the records;
- four unmasked layers per file (spread over the area range, raw or RLE channels) are decoded here and must match
  the stored texture byte for byte after premultiplication (`--max-layer-diff 0`);
- the whole canvas is rendered at about 600 px, and compared with the PSD's merged image box-filtered to the same
  size, both over white (`--max-render-diff`, a mean absolute difference per channel).

The default import is the one rendered: groups are `DynamicComposite` nodes and clipped layers carry a Mask binding
to their base. A PSD that is not 8-bit RGB must be refused with a message that names the 8-bit limit or the colour
mode.

A PSD whose merged image the script cannot decode (unsupported compression) is imported and rendered but not
compared; the summary and `report.json` (`psd_not_compared`) count such files, and the run does not fail for them.

Layer masks (the user masks stored with a layer, not clipping) are only checked structurally (a masked layer is
excluded from the bitmap comparison). Layers with blend
modes the importer maps to `Normal` (Dissolve, Vivid Light, Saturation and others) are counted in the report.

## JPEG

A marker scanner classifies each stream (process, sampling, restart intervals, ICC, EXIF thumbnail and
orientation, Adobe transform, scan count). A stratified sample, rarest class first and spread over file sizes
(`--max-jpeg`, default 60), is imported through `merge.images`; the stored texture is read back with `texture.get`.
Streams the decoder supports must decode; the rest must be refused with a message that names the feature and does
not contain the directory of the file.

The texture is compared per RGB sample with:

- `djpeg -nosmooth` (limits: mean 0.15, largest 6, at most 0.05 % of samples off by more than 16). `-nosmooth`
  matches this decoder's chroma replication and switches off libjpeg's block smoothing of progressive scans, so what
  is left is IDCT and colour-conversion rounding.
- `sips` (limits: mean 3.0, at most 3 % off by more than 16). ImageIO is a weak reference. At JPEG quality 50 its
  grayscale output already differs from libjpeg's by a mean of 1.7 (largest 42), and it reads Adobe transform 0 as
  YCbCr; that case is excluded from the `sips` limits.

The progressive decoder reads untrusted files, so it bounds its work and memory (`rust/src/jpeg.rs`):

- At most 256 scans (`MAX_SCANS`); the real corpus peaks at 14, libjpeg's default colour script uses 10. More is
  refused with `JPEG has too many scans`.
- A work budget of 16 passes over every coefficient of the frame (`WORK_PASSES`). A scan is charged blocks x band
  width whatever its entropy data holds, because a block inside an end-of-band run costs no bits but is still
  walked. The usual script needs about 3 passes, a refinement per bit plane 8. At the 16-million-pixel cap the
  budget is 268 million coefficient visits for a grayscale frame and 805 million for 4:4:4 (a few seconds in a
  debug build), and a 1 MB stream of near-empty scans is refused after the 16th scan instead of running for hours.
  The same message is used.
- Peak memory at 16 million pixels, 4:4:4 (the worst case: at most 3 blocks per 64 pixels): 100 MB of `i16`
  coefficients, held until the end, plus 50 MB of component planes and 67 MB of RGBA output, about 220 MB. Measured
  on a 4096x4096 progressive file: 266 MB maximum resident size of the whole `soul2d-core` process (debug build),
  which also holds the 67 MB stored texture. A sequential stream needs no coefficient buffer (about 120 MB).
- A component's quantisation table is the one defined when its first scan starts (T.81 B.2.4.1), not the last one in
  the file.

Images over `--max-compare-pixels` (6 million) are decoded but not compared, because the PNG reader here is plain
Python.

## Results, 2026-10-12, macOS arm64, debug build

Inputs: eight official Live2D sample PSD files, and 1,012 JPEG files shipped with the operating system or with
installed software (60 selected).

JPEG candidates: 1,012 streams; 941 baseline and 62 progressive three-component, 5 baseline and 3 progressive
grayscale, 1 baseline four-component (CMYK). 624 use restart intervals, 446 carry an ICC profile, 79 an EXIF
thumbnail, 472 an Adobe marker. None uses a non-trivial EXIF orientation (195 say 1, the rest have none), none is
multi-scan sequential, 3 are over the importer's size limits.

Selected 60: 58 decoded, 2 refused. The refusals are the CMYK stream (`4-component (CMYK) JPEG is not supported`)
and a 4224x4224 progressive stream, which is over the 16-million-pixel texture limit (the limit was not changed).
The 60 hold 43 baseline and 17 progressive streams: 4:4:4 (23), 4:2:0 (19), 4:2:2 (5), 4:4:0 (2), 4:1:1 (2),
4:1:0 (2), mixed sampling (2), grayscale (4) and the CMYK stream. Against `djpeg -nosmooth` (55 compared): worst
mean 0.044, largest difference 3. Against `sips` (54 compared, one excluded): worst mean 2.5, in low-quality
4:2:2, 4:2:0 and 4:4:0 images; `djpeg` with fancy upsampling differs from this decoder by 0.8 to 1.2 on the same
files, so most of that gap is chroma interpolation and the rest is ImageIO.

### PSD results

| File | Canvas | Parts / groups | Hidden / masked / clipped | Tree and field mismatches | Layer bitmaps (max byte diff) | Render vs merged image (mean diff) |
|---|---|---|---|---|---|---|
| Haru greeter, import | 2400x4500 | 81 / 0 | 2 / 0 / 0 | 0 | 0 on 4 layers | 1.17 (1.17) |
| Haru greeter, split | 2400x4500 | 275 / 67 | 2 / 44 / 120 | 0 | 0 on 4 layers | 2.58 (15.55) |
| Haruto, import | 1600x2000 | 70 / 0 | 1 / 0 / 0 | 0 | 0 on 4 layers | 1.66 (1.66) |
| Haruto, split | 1600x2000 | 147 / 41 | 3 / 4 / 24 | 0 | 0 on 4 layers | 1.71 (16.74) |
| Koharu, import | 1600x2000 | 74 / 0 | 1 / 0 / 0 | 0 | 0 on 4 layers | 1.83 (1.83) |
| Koharu, split | 1600x2000 | 162 / 45 | 4 / 8 / 29 | 0 | 0 on 4 layers | 1.87 (22.3) |
| Unity-chan licence logo | 1238x874 | 2 / 1 | refused: Only RGB PSD documents are supported (colour mode 4, CMYK) | | | |
| Unity-chan | 1600x1600 | 152 / 41 | 19 / 0 / 19 | 0 | 0 on 4 layers | 2.43 (2.62) |

The last column is the mean absolute difference per channel after the clipping and `DynamicComposite` fixes, with
the value before them in brackets. The earlier values were measured on re-imports with `layerGroupNodeType:
"Composite"`, because the default import could not be rendered then. The default import is rendered now. The
new figures come from a release build of the same code.

Every layered file imported with the right tree: parts, groups, nesting, order, names, visibility, opacity,
blend names and positions all matched the independently parsed records, and the 28 sampled layer bitmaps
matched exactly. The refused file is a CMYK document; the message says `Only RGB PSD documents are supported (found
color mode 4)`.

Findings:

- **`DynamicComposite` groups are drawn** (fixed). `import.psd` creates them by default and the renderer refused
  them (`node N has unsupported type "DynamicComposite"`). It now draws one exactly as it draws a `Composite`: the
  subtree goes into an offscreen layer, which is blended with the node's opacity, tint and blend mode and can
  itself be masked. The stored mesh of a `DynamicComposite` (it has none after a PSD import) is not used.
- **Clipping masks are honoured** (fixed). A layer clipped to the layer below it gets `masks:
  [{"source": base, "mode": "Mask"}]`, where the base is the first non-clipped layer below it in the same group;
  consecutive clipped layers share that base, and a clipped layer at the bottom of a group has no base and stays
  unmasked. The base's alpha is the mask, thresholded at the base's `mask_threshold` (0.5), as for every mask in
  this document model. A group as base is the mask source itself when it is a `Composite` or `DynamicComposite`;
  when the group type is `Node` or the structure is flattened, every visible Part below the group is a source (the
  union of their alphas). The three split files went from 15.6, 16.7 and 22.3 to 2.6, 1.7 and 1.9.
  - A clipped group that has no node of its own (type `Node`, or flattened) passes its clip to its Parts. A Part
    that is already clipped inside such a group keeps only its own clip: Mask bindings are a union, so both cannot
    be expressed.
  - A hidden base, a base group with no visible Part, and an empty layer (no node) as base hide the layers clipped
    to them, as Photoshop does. A hidden base keeps the bindings, so showing it again restores the clip.
  - The import refuses a file that needs more than 1,000,000 mask bindings (the render limit
    `MAX_PACKET_MASK_REFERENCES`): a plain-node or flattened base of P Parts with C layers clipped to it needs P x C.
    The message names the constant and suggests a Composite or DynamicComposite group type, where a group is one
    source. Composite-type groups nest 16 deep (`MAX_COMPOSITE_DEPTH`, the renderer's limit); a group nested deeper
    imports as a plain `Node`, which keeps the layers and clipping but loses that group's own opacity and blend mode.
  - A run of adjacent layers clipped to one base reuses the base's mask in the renderer, so the base is rasterised
    once per run. On the real files here the bases are Parts and the render is under 0.2 s at 600 px, so the timing
    did not change (Haru split 0.19 s before and after); the effect shows with a group as base (see
    `a_run_of_clipped_layers_rasterises_its_base_once`, which exceeded the work limit before).
  - Differences from Photoshop that remain: a clipped layer is drawn over the backdrop, not first composited onto
    its base, so a base with a blend mode other than Normal or an opacity below 1 changes the result; the mask is
    binary, so anti-aliased base edges are cut at half alpha instead of being multiplied by the base alpha.
    `BlendMode::ClipToLower` does not fix the second: it multiplies by the alpha of everything already drawn
    below, not of the base layer, and it replaces the layer's own blend mode; the stencil is one byte per pixel.
    A soft clip needs a float mask mode, which was not added.
- **Pass-through groups with a blended layer** (fixed). A full-opacity pass-through group that holds a layer with a
  blend mode other than Normal imports as a plain `Node`, so that layer blends with what lies below the group; as a
  Composite it blended with the group's own buffer (`a_pass_through_group_with_a_blended_layer_blends_against_what_is_below_it`).
  A blended child group counts as a blended layer. The node's `zsort` is 0, so its children keep their place in the
  whole stack. A pass-through group with an opacity below 1 stays a Composite, because a plain node has no opacity,
  and so does one that another layer is clipped to, so that it remains one mask source. The eight
  real files render the same before and after (same mean differences), so they do not exercise this case.
- **Imported groups under a deformer** (fixed). A group is written with `auto_resized: true`, so a deformer moves
  it like a `Composite` (a rigid body, no mesh of its own). A `DynamicComposite` with a mesh that is not a quad
  (converted from a Part) is drawn unwarped under a deformer; it used to fail the render with "deform offset length
  must be 4".
- **Large renders of grouped files** (fixed). A `Composite`/`DynamicComposite` used to cost about two full-target
  passes of the 100-million-pixel raster work limit (`MAX_RASTER_WORK`) however small its content, so the Haru split
  file (66 groups) rendered at 300x562 and was refused at 600x1125 with "render exceeds the 100-million-pixel raster
  work limit". Mask rasters were not the cause (no clipped layer in this file has a group as base). The renderer now
  keeps the pixel box that the rasters of a group wrote to, and the quantise, coverage and blend passes of the group
  visit that box only. A group is charged one target-sized pass for its layer and final blend, plus the box for
  its coverage scan and fill, which is never more than before. The layer is zero outside the box, so the pixels are the same
  (`many_small_composites_on_a_large_target_stay_within_the_work_limit`). The Haru split file renders at 600x1125
  (`--render-side 1200`, mean difference 1.98) and the results at 600 px did not change.
  - Limits that remain: `MAX_RASTER_WORK` is unchanged, and a group whose content spans the target still costs up to
    three target-sized passes besides its children, so a file with many canvas-sized groups can still be refused at a
    large size. Every group still allocates a target-sized layer (17 bytes per pixel, and one more for its stencil)
    per nesting level, counted against the 640-megabyte working-memory limit, and a masked item still fills a
    target-sized stencil; the layer is charged as one pass, the stencil fill is not charged.
- Blend modes: the 23 non-Normal layers of the Haru split file are all mapped modes. Dissolve, Vivid Light, Saturation
  and the other unmapped modes import as Normal by design; no sample used one. Layer masks were imported but not compared
  numerically.

## KRA

A Krita document is a ZIP. For each file the tool reads it with `zipfile` and its own code (no Rust code involved):
`maindoc.xml` (canvas, colour space and profile, the layer tree with type, visibility, opacity, composite op and
offset), the tile file of every paint layer (64x64 tiles, a flag byte, LZF; a small LZF decoder) and Krita's own
flattened `mergedimage.png`. It then drives `import.kra` and `render` through the JSONL protocol.

- Classification: size, canvas, colour model and depth, layer counts by type, hidden layers, composite ops the
  importer does not map, node types it skips (anything but paint and group layers), pass-through groups and groups
  with a blend mode.
- An imported file must give the same nodes as the layer tree: groups and paint layers in the same nesting and order
  (bottom-most first, as the importer writes them), names, visibility, opacity and mapped blend mode. A group must
  carry the mapped mode too (Normal when it is a pass-through group); a group that became a plain `Node` has no blend
  mode and is counted (`groups_with_blend_not_applied`), not failed. `imported.skippedLayers` must give the counts by
  node type that this script takes from the layer tree, and the same `visible` flag. A paint layer
  without a node must be fully transparent (the importer crops to the opaque pixels and makes no node for an empty
  layer). Positions and sizes are compared exactly for layers whose tile box is at most `--max-layer-pixels` (they
  are decoded here) and against the tile box for larger ones.
- The stored texture of up to `--layers` decoded layers must equal the layer pixels premultiplied here, byte for byte.
- The whole canvas is rendered at about `--render-side` px and compared with `mergedimage.png` box-filtered to that
  size, both over white, with the same statistics as the PSD path. The render must paint pixels when
  `mergedimage.png` has a visible pixel or the layer tree has a shown, non-empty paint layer; a blank document (all
  paint layers fully transparent) is imported with no parts and is not asked to paint.
- A render difference over the limit fails, with one exception. A node of an unsupported type (vector, file,
  adjustment, ...) that is *shown* (it and every ancestor group are visible with opacity above 0, so it can reach
  Krita's merged image) and has content is skipped by the importer, and the tool has no composite of the supported
  layers to separate its share of the difference from a regression. Such a file is reported as `NOT COMPARABLE`, the
  run does not end in `PASS` and exits with status 3 (a failure elsewhere still gives status 1). A vector layer
  counts as having no content only when its `content.svg` is an empty file; every other unsupported type counts as
  having content. The layer bitmaps and the tree comparison still fail on their own for such a file.
- A refusal must name the limit or the unsupported feature and must not contain the directory of the file. The
  tool works out from the tile tables which limits the file exceeds (canvas, entry and archive sizes, the sum of the
  layers' tile-box pixels, each layer's tile count times the tile area, the cropped size of each layer against the
  texture limit) and requires the message to
  carry the words and the number of one of them (for example `canvas size is out of range (limit N pixels)`; the
  canvas and the tile boxes share N = 100,000,000, so the number alone is not enough). The texture limit is
  checked only when no limit of the container or the tile tables is exceeded: the importer refuses those first, and
  the tool then does not decode the tile boxes (two small tiles far apart would cost hundreds of megabytes). A file
  that exceeds a limit is reported as refused-by-limit, not as a failure.
- Four archive shapes are predicted from the bytes the importer reads, not from what `zipfile` shows. A ZIP64 entry is
  found in the stored central-directory sizes, as the importer tests them (`zipfile` replaces a `0xFFFFFFFF` size by the
  real one from the ZIP64 extra field, which can be small). The tile area of a layer is its number of tiles with data
  times the tile area, as the importer counts it, so repeated or overlapping tiles are caught although their box is
  small. A document or a layer in a colour model other than RGBA is judged on what the importer checks before that
  (the container, then the document's colour space, the canvas, and the layers in file order up to the first
  unsupported paint layer): such a limit is the refusal that must appear, and only when there is none does the colour
  message. A `mimetype` over 64 bytes or a `maindoc.xml` over 16 MiB by its declared size is not inflated; the file is
  recorded, its refusal is judged against that limit, and the run goes on with the next file.
- A file the tool itself cannot finish checking (not a ZIP, a missing entry, a tile table it cannot follow, a reply
  of the core it cannot decode) is recorded as `unchecked` with the error and fails the run by name; the run goes on
  with the next file.
- `--kra-crc-example PATH` also runs `rust/examples/kra-layer-crc.rs`, which prints each paint layer's rectangle and
  the CRC-32 of its decoded RGBA bytes with the reader's limits raised. The tool decodes every paint layer itself and
  compares both values; an empty layer must give an empty rectangle and the CRC-32 of no bytes. This checks the tile
  decoder on files that the importer refuses by a limit. It imports nothing, and it is the only place where limits
  are raised; the importer's limits are unchanged. The example composes the whole tile box of every layer, so a file
  with a layer whose tile box is over 100,000,000 pixels is not run (two small tiles far apart would cost terabytes),
  and neither is a file with an archive feature the reader refuses by name (ZIP64, multi-disk, encrypted entries, a
  compression method other than stored or deflate) or with a layer entry over the reader's 128 MiB limit, which the
  tool does not inflate either: none of its layers is compared, and the summary counts them in
  `kra_crc_layers_skipped`. Those archive features are also listed with the limits that explain a refusal.
- `--self-test` checks the LZF decoder and the tile reader on small hand-made data, and the verdict rules above on
  small KRA documents it builds from bytes (tile comparison, shown layers, limit messages, the oversized tile box,
  the render verdicts and blank documents, and the four shapes above with the importer's own refusal messages).

```sh
./tools/build-rust-core.sh --locked
./tools/build-rust-core.sh --locked --release --example kra-layer-crc
python3 -I tools/verify-real-files.py --self-test
python3 -I tools/verify-real-files.py --kra-crc-example out/rust/release/examples/kra-layer-crc \
    --max-layer-pixels 20000000 DIR_WITH_KRA_FILES
```

The run takes about four minutes with the debug core (the Python decode of 1.05 billion layer pixels dominates).

### KRA results, 2026-10-13, macOS arm64, debug build

Corpus: 14 `.kra` files from one Krita art pack supplied by the maintainer (E39.kra, E39P00.kra to E39P12.kra; about
1,043 MB in all, 0.4 to 109 MB each). They are third-party artwork and were read in place only. Nothing extracted
from them, no layer name and no pixel is stored in the repository.

- 13 files were saved by Krita 5.2.9; E39P00 carries no version. All are 8-bit integer RGBA documents with the
  `sRGB built-in` profile. 11 pages are 3600x5084 (18.3 million pixels), one is 2975x7435 (22.1 million), and two are
  small: 1100x765 and 2481x663.
- 136 layer nodes: 80 paint layers (all RGBA, 64x64 LZF tiles, transparent default pixel), 20 group layers (none
  pass-through, 3 with Multiply), 23 vector (shape) layers (11 visible), 12 file layers (all hidden) and 1
  adjustment layer (hidden). No mask, clone or filter layers. Composite ops: normal 130, multiply 4, and one each of
  `copy` (the adjustment layer) and `arc_tangent` (a vector layer). 35 nodes are hidden.

| File | Size | Canvas | Paint / group / vector / file / adjustment | Result | Tile decode vs this tool | Render vs `mergedimage.png` (mean diff) |
|---|---|---|---|---|---|---|
| E39 | 3.8 MB | 1100x765 | 2 / 0 / 1 / 0 / 0 | imported, 2 parts, tree matches | 2 of 2 layers equal | 2.56 at 550x382 (1.44 at 1100x765) |
| E39P00 | 0.4 MB | 2481x663 | 2 / 0 / 0 / 0 / 0 | imported, 2 parts, tree matches | 2 of 2 equal | 0.00 at 496x132 |
| E39P01 | 93.9 MB | 3600x5084 | 6 / 2 / 2 / 1 / 0 | refused: tile boxes 102,088,704 px, limit 100,000,000 | 6 of 6 equal | not rendered |
| E39P02 | 100.9 MB | 3600x5084 | 11 / 2 / 1 / 1 / 0 | refused: tile boxes 116,289,536 px, limit 100,000,000 | 11 of 11 equal | not rendered |
| E39P03 | 97.2 MB | 3600x5084 | 8 / 2 / 2 / 1 / 0 | refused: tile boxes 113,635,328 px, limit 100,000,000 | 8 of 8 equal | not rendered |
| E39P04 | 82.2 MB | 3600x5084 | 4 / 1 / 3 / 1 / 0 | refused: a 3625x5100 layer, texture limit (8192 per side, 16,777,216 px) | 4 of 4 equal | not rendered |
| E39P05 | 92.6 MB | 3600x5084 | 5 / 2 / 2 / 1 / 0 | refused: a 3600x5084 layer, texture limit | 5 of 5 equal | not rendered |
| E39P06 | 108.8 MB | 3600x5084 | 8 / 2 / 2 / 1 / 0 | refused: tile boxes 102,342,656 px, limit 100,000,000 | 8 of 8 equal | not rendered |
| E39P07 | 81.5 MB | 3600x5084 | 4 / 1 / 2 / 1 / 1 | refused: a 3600x5084 layer, texture limit | 4 of 4 equal | not rendered |
| E39P08 | 89.8 MB | 3600x5084 | 7 / 2 / 2 / 1 / 0 | refused: tile boxes 102,612,992 px, limit 100,000,000 | 7 of 7 equal | not rendered |
| E39P09 | 89.1 MB | 3600x5084 | 4 / 1 / 2 / 1 / 0 | refused: a 3600x5084 layer, texture limit | 4 of 4 equal | not rendered |
| E39P10 | 90.5 MB | 3600x5084 | 6 / 2 / 2 / 1 / 0 | refused: a 3600x5084 layer, texture limit | 6 of 6 equal | not rendered |
| E39P11 | 96.5 MB | 3600x5084 | 7 / 2 / 2 / 1 / 0 | refused: tile boxes 106,688,512 px, limit 100,000,000 | 7 of 7 equal | not rendered |
| E39P12 | 15.5 MB | 2975x7435 | 6 / 1 / 0 / 1 / 0 | refused: a 2975x7435 layer, texture limit | 6 of 6 equal | not rendered |

The tool agrees with every refusal: the six pages whose message names the pixel budget have tile boxes over it (the
tool then does not look at the texture limit, which the importer checks later), and each of the other six has at
least one layer over the texture limit. For every refusal the message carries the words and the number of the limit
the tool found. No file was refused for its colour model, and no refusal
contained the directory of the file. The two imported files have no mismatch in structure, name, visibility,
opacity, blend, position or size, and the 3 layer bitmaps compared (each eligible layer once) have a maximum byte
difference of 0. All 80 paint
layers (1.05 billion pixels) have the same rectangle and RGBA bytes in the importer's tile reader and in this tool's
decoder. A deliberate error in the tool's plane order, or a flipped bit in its LZF output, makes that comparison fail.

The importer opened all 14 ZIP containers (Krita's archives, up to 109 MB, stored and deflated entries), and every
entry that was read passed its CRC check.

Resources: a page of 80 to 109 MB takes 7 to 11 s and 520 to 620 MB of peak resident memory in the debug core before
it is refused (about 1 s in a release build). That fits the budget: the layers read so far are held until the first
one that crosses a limit. The two imported files take under 1 s and 32 MB.

Findings:

- **Refusals did not name the limit** (fixed: `fix: import: name the limit and its value when a KRA is refused by
  one`). They read `the layers' pixels exceed the memory budget` or `... larger than the supported texture size`.
  Each now carries the limit and its value, and the pixel-budget message says how many pixels the tile boxes cover
  (`refusals_name_the_limit_and_its_value` in `rust/tests/kra.rs`).
- **Page-sized files cannot be imported** (not fixed; a limit, not a bug). The texture limit (8192 per side,
  16,777,216 pixels) is below the canvas of all 12 pages (18.3 and 22.1 million pixels), so each has 2 to 5 layers
  that are refused whole, also when clipped to the canvas. Six files also exceed the 100-million-pixel budget for the
  sum of the layers' tile boxes. Raising a limit would not make these files import. The
  [layered import policy](layered-import.md) retains explicit refusal (#254); automatic tiling and scaling are not
  performed. The limits were not changed and these files remain unsupported.
- **Layers are not clipped to the canvas** (intentional; #252). 11 layers in 5 pages extend past the canvas (the largest is
  28.5 million pixels) and are imported, or refused, at their full size, although Krita's merged image ends at the
  canvas. The [layered import policy](layered-import.md) preserves those editable pixels; a synthetic KRA test
  checks the full imported texture outside the canvas.
- **Vector, file and adjustment layers are dropped without a message** (fixed: `feat: import: report the Krita layers
  that are skipped`). 36 of the 136 nodes are skipped (11 visible vector layers, in E39 and 10 pages). The import
  result now carries `imported.skippedLayers` (`{visible, types: [{type, count}], other}`; the merge plan has the
  same key), which counts every layer-level node that is not a paint or group layer by its `nodetype`.
  `visible` is true when any counted node has its own `visible` set (a hidden group above it is
  not looked at). A paint layer without tiles is not counted: it is empty, unless its default pixel is not transparent
  (next finding). Masks under a layer are never read and are not counted. A
  file that skips nothing has no such key. The editor's import and merge-plan status lines show the count. In the one
  imported file with a visible vector layer, its content file is empty, so the picture does
  not change. A visible, non-empty vector layer would change it; the tool then reports the file as `NOT COMPARABLE`
  instead of failing or passing it (see the rules above). Of the 11 shown vector layers only that of E39 is empty (a
  0-byte `content.svg`); the other 10 are in refused files, so no file in this corpus ends `NOT COMPARABLE`. The tool
  compares the key with the layer tree: E39 reports one visible `shapelayer`, which is what the tree has, and the
  other imported file has no key and skips nothing. More than one type, `other` and the merge plan were checked by
  synthetic tests only.
- **The default pixel of a paint layer was not read** (fixed: `fix: import: read the default pixel of a Krita paint
  layer`, issue 276; checked by synthetic tests only, because every paint layer in the real corpus has a transparent
  default pixel). Krita keeps one pixel per paint device in `<tile file>.defaultpixel` (B, G, R, A for 8-bit RGBA),
  and every pixel that no tile covers has that value (`plugins/impex/libkra/kis_kra_load_visitor.cpp`, Krita master,
  read on 2026-10-13). A raster background layer has an opaque one and no tiles. When its alpha is not 0, the layer's
  box is the non-default tiles' box joined with the canvas (the layer offset applied) and the pixels no tile covers take that
  pixel; the box counts towards the pixel budget before anything is allocated. A gap between the canvas and a tile
  outside it is filled too: the paint device holds the default pixel there, and Krita's own exact bounds for such a
  device are the image rectangle joined with every pixel that differs from the default pixel
  (`KisPaintDevice::calculateExactBounds`, `libs/image/kis_paint_device.cc`, same date). A missing entry, one that
  is not 4 bytes (it is not inflated) or alpha 0 changes nothing, and a layer without tiles is then empty and not
  reported. A canvas over the texture limit (8192 per side, 16777216 pixels) now refuses such a file, where the
  background used to be dropped. `tools/verify-real-files.py` reads the pixel on its own and expects the same box,
  pixels and nodes.
- **Tiles containing only the default pixel widened the layer box** (issue 311; checked by synthetic tests).
  These tiles no longer enlarge the allocation or texture. They still count towards the decoded-tile budget and
  overwrite earlier tiles within the final bounds, so a later default tile can still erase earlier pixels.
- **Group blend modes are not applied** (fixed: `fix: import: apply a Krita group's blend mode and read the lighten
  id`; not exercised on a real file). A Krita group becomes a Composite or DynamicComposite, which carries a blend
  mode and renders it, so a group's composite op is now mapped like a paint layer's (`engine_blend`). 3 groups in the
  corpus use Multiply; no imported file has a group, so this was checked by a synthetic test only. A group that
  becomes a plain Node (`keepStructure` false, or nested deeper than 16 Composites) has no blend mode, and the
  import result lists it in `imported.groupBlendNotApplied`. A pass-through group keeps Normal: Krita does not
  composite the projection of such a group (`libs/image/kis_projection_leaf.cpp`, Krita master, read on 2026-10-13),
  so its composite op has no effect. The group's `passthrough` attribute is also passed on (`fix: import: import a Krita
  pass-through group as the PSD path does`, issue 277), so a full-opacity pass-through group that holds a layer with a
  blend mode other than Normal imports as a plain `Node` by the rule the PSD path uses (see "Pass-through groups with a
  blended layer" above); otherwise it stays a Composite. Krita merges a pass-through group's opacity into its children
  (`libs/image/kis_projection_leaf.cpp`, Krita master, read on 2026-10-13), which a plain node cannot do, so a group
  with an opacity below 1 stays a Composite as in the PSD path. `tools/verify-real-files.py` expects the same node type
  for each group. No real file has such a group, so this was checked by a synthetic test and the tool's self-test only.
  The renderer's Multiply keeps the alpha of what lies below (the legacy mode, see issue 194), so a Multiply group
  shows only where something lies below it, as a Multiply paint layer and a Multiply PSD group already do. Krita
  shows it over transparent pixels too.
- **A blend mode the engine lacks imported as Normal without a word** (fixed: `feat: import: report the blend modes a
  layered import turned into Normal`, issue 199; checked by synthetic tests only). A PSD layer in Dissolve,
  Darker color, Lighter color, Vivid light, Pin light, Hard mix, Divide, Saturation or Luminosity (and, until
  issue 192 gave the engine those modes, in Linear burn, Linear light, Hue or Color), a PSD layer with a key this
  reader does not know, and a Krita layer or group whose `compositeop` is not in `engine_blend` all import as
  Normal. A PSD group in a mode the engine has that becomes no composite (structure not kept, or nested too deep)
  still loses its mode without a report; only the Krita import lists that case, as `groupBlendNotApplied`.
  The import result and the merge plan now have
  `blendModesNotApplied {count, modes:[{mode,count}], other}` when at least one layer or group is affected. `mode` is
  the source's own name: the PSD variant (`VividLight`, `Saturation`), the four key bytes of an unknown key when all are
  printable ASCII (else `unknown`), or the Krita `compositeop` text cut to 64 characters without control characters.
  `modes` keeps at most 16 names in the order the layers are stored in the file (bottom-most first in a PSD, top-most
  first in a Krita file); the layers of any further name are counted in `other`, as in `skippedLayers`. `count` covers
  every affected layer, including those with no pixels, which make no node. Normal, a PSD pass-through group and a
  Krita pass-through group are not lost modes, and a group that is reported by `groupBlendNotApplied` because the mode
  is one the engine has is not listed again. The editor's status line adds `, N blend modes not applied`.
  `tools/verify-real-files.py` compares the Krita value with the layer tree. Not covered: Cubism also maps alpha blend
  modes to Over (issue 193, not implemented here) and warns on a PSD export (there is no PSD export).
- **Odd canvas sizes shift detail by up to half a pixel** (not fixed; intended). The part quad uses integer half
  extents, as D does (`quad_half`), so a layer of odd height is drawn one pixel short. In E39 the picture is the
  top one of two opaque 1100x765 layers; this tool's own composite of the two layers reproduces `mergedimage.png`
  exactly (mean difference 0.0) and both stored textures match byte for byte. The render still differs by 1.44 at 1:1 and 2.56 at half size. The difference is
  smallest in the middle rows (0.2) and grows to 3.8 in the last eighth of the rows, while the columns are flat. That
  fits the one-pixel quad, but it is an inference, not a measurement of the quad. The flat E39P00 gives 0.00.

## What is not verified

- Krita (`.kra`): only two small files import, so everything after the import is checked on two documents: no group,
  no blend mode other than Normal, one hidden layer (opacity 36) and one pair of overlapping opaque full-canvas
  layers. Not exercised by any real file: imported groups (opacity, blend, nesting), pass-through groups (all 20
  groups have `passthrough=0`; the import is checked by a synthetic test only), a non-transparent default pixel
  (synthetic tests only), masks, clone and filter layers,
  GRAYA/16-bit/float/CMYK documents (refused by colour model in synthetic tests only), the `lighten` and `lighter color` composite ids (Krita's
  `libs/pigment/KoCompositeOpRegistry.h`, master, read on 2026-10-13, has them as the ids of two operations,
  Lighten and Lighter Color; the importer maps both to Lighten, the second as kra-d does and as an approximation; no
  file here uses either), animation, tile sizes other than 64, stored (uncompressed) tiles, ZIP64 and any page-sized document
  (none imports, see the findings). Layer order is checked against pixels for one pair only (the two opaque layers of
  E39 differ by a mean of 70 and the render matches the top one); partly transparent overlap is not exercised.
- JPEG: EXIF orientation (no file with a value other than 1), multi-scan sequential streams (none found), 12-bit,
  arithmetic-coded, lossless and hierarchical streams (refused by name, covered by synthetic tests only),
  CMYK/YCCK beyond the one refused stream, restart intervals inside progressive scans (covered by a generated
  stream; none of the 65 real progressive streams uses restart markers), ICC profiles (read past, never applied).
- PSD: 16-bit and CMYK documents (one CMYK file was refused as designed), layer mask (not clipping) feathering and
  density against a reference, blend modes against a reference renderer, pass-through groups with a blended layer (synthetic test only),
  adjustment layers, clipping to a base with a non-Normal blend mode or partial opacity.
- Video export: one real encode only. The `#[ignore]`d test `real_ffmpeg_encodes_a_video_that_ffprobe_can_read` in
  `rust/tests/export.rs` (`cargo test --test export -- --ignored real_ffmpeg`; `SOUL2D_FFMPEG` and `SOUL2D_FFPROBE`
  override the program names) passed on 2026-10-13 with ffmpeg 8.1.2 on macOS arm64. It does not run by default, and
  other codecs, containers and ffmpeg versions were not tried.
- A real ACP agent in the agent panel: not run, by the maintainer's decision of 2026-10-13. The panel is covered by
  `rust/tests/acp.rs` and the `acp-echo` example only.
