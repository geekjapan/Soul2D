# PSD Depth Map Import Design

Status: implemented in the Rust crate. `rust/src/psd_depth.rs` imports a layered PSD (or a single PNG) whose layers hold
grayscale depth, and writes the result to the `depths` of mapped `GridDeformer` nodes. The editor window is
`rust/src/editor_psd_depth.rs` (`PsdDepthWindow`), reached from `File > Import > Import Depth Map...` and from a
side-panel button. Not offered: the 3D Adjust tab, layer and depth-mask preview images or tooltips, color-source
selection, contour repair, wavy-surface smoothing, alpha-depth gap fill, GPU composition (the options
`useGpuComposition`, `colorSource`, `repairContourBand` and `smoothWavySurface` are rejected by `settings_from`), and a
window-local undo. A `PathDeformer` target is rejected; it never receives a `depths` array through this feature.

## Purpose

The PSD stores per-layer depth as RGB grayscale. Layers correspond to existing model layers. Matching uses layer paths
and names, groups the matched nodes by the deformer that controls them, and where several layers give a depth at the
same grid vertex, the frontmost depth wins.

## Model Integration

Depth values live on the `depths` array of a `GridDeformer` node (`depths` module; `depth_ops` owns the saved depth
operations). The Rust files are:

- `rust/src/psd_depth.rs`: parsing, matching, sampling, compositing, the apply step, the plan report.
- `rust/src/psd.rs`: the PSD reader (`psd::parse`, `psd::group_states`, `Document::layer_rgba`). Version 1, 8-bit RGB only.
- `rust/src/editor_psd_depth.rs`: the import window (draft, plan table, remap, per-layer options).
- `rust/src/lib.rs`: session ops `depth.import_psd` (`op_import_psd`) and `depth.import_psd.plan` (`op_import_psd_plan`).
- `doc/commands.manifest.json`: MCP tools `depth_import_psd`, `depth_import_psd_plan` and the command wrapper
  `DepthMapCommand_ImportPSDDepths`.

Undo uses the session's history. `op_import_psd` hands the whole candidate model to
`commit_targets(model, changed_grids)`, so all changed grids, the removed depth operations and the rig refresh are one
history entry.

## Data Flow

```text
PSD (or PNG) file
  -> psd::parse, psd::group_states, assign_paths      (layer paths, visibility, opacity, clipping)
  -> match_layer or the layer's remap choice          (Part / deformer node -> containing GridDeformer)
  -> per grid vertex: world position -> document pixel
  -> sample_layer per layer                           (channel, invert, alpha, convolution)
  -> frontmost layer wins                             (strict ">" over layers, front layer first)
  -> missing-vertex policy
  -> write "depths", drop "depth-ops" and "depth-op-base-depths"
  -> reset rig depthScale/depthOffset on applied targets
  -> commit_targets: one undo step, rig deform refresh
```

`depth.import_psd.plan` runs the same function (`plan_psd` calls `import_psd`) and returns the report without
committing.

## Layer Paths

`parse_psd` reads layers in file order (bottom to top), keeps layers that have pixel data, and reconstructs the path
`/group/layer` from group folders (`psd::group_states`). Duplicate paths get a ` [#n]` suffix, numbered front to back, so
the front layer keeps the plain `/Name`. Group visibility and cumulative opacity are folded into each layer. A clipped
layer takes the alpha, visibility and opacity of the nearest unclipped layer below it in the same group
(`apply_clipping_base`). Layers are then reversed so front layers are sampled first.

A `.png` source is read as one layer named after the file (`parse_png`); other extensions are rejected
(`unsupported depth source format`). A retained-memory budget of 256 MiB applies to decoded layers
(`MAX_PSD_RETAINED_BYTES`; the report carries `budget`).

## Layer Matching

`match_layer` collects candidates and keeps the lowest priority number.

1. `Part` whose path equals the PSD layer path (priority 0). The path is the node's `layerPath`, else `/<name>`.
2. `Part` whose path base name equals the PSD layer name (priority 1).
3. `Part` whose node name equals the PSD layer name (priority 2).
4. `Part` whose path or name equals the PSD layer name after `normalize_layer_name` (lower case, separators and extension
   removed) (priority 3).
5. With `matchDirectGridName` (default `true`), a `GridDeformer` or `PathDeformer` whose name equals the layer name
   (priority 5).

Each candidate resolves to its containing deformer. Candidates that resolve to different deformers make the layer
`Ambiguous`. A candidate with no containing deformer gives `UnmatchedWithoutGrid`. No candidate gives `Unmatched`.
The first implementation preference for `Part` matches, because imported and merged PSD assets keep `layerPath`, is
preserved by the priority order.

## Finding The Target Deformer

`containing_target` returns the matched node itself when it is a `GridDeformer` or `PathDeformer`; otherwise the nearest
enclosing one found by walking the tree from the root. The original proposal said "first parent that is a
GridDeformer"; the Rust walk accepts either deformer type and then fails the import if the result is a `PathDeformer`
(`PSD depth import does not assign depths to PathDeformer`). Several `Part` layers under one `GridDeformer` contribute to
one depth map.

## Coordinate Mapping

The PSD document center is the model origin, as in PSD merge. For each grid vertex (`grid_vertices`, row-major):

```text
world        = world_affine(posed model, grid) applied to the vertex
document x   = world x + document.width  / 2
document y   = world y + document.height / 2
layer x/y    = document x/y - layer.left/top, rounded to the nearest pixel
```

`world_affine` composes each node's `transform` (translation, Z rotation, XY scale) over the evaluated (posed) model
that the session passes in (`Session::posed_model`). A node with X or Y rotation in that walk makes the
import fail (`PSD depth import cannot sample X/Y rotation`). The mapping is covered by the test
`import_samples_the_evaluated_transform`.

## Depth Value Conversion

White is front, black is back by default. `pixel_depth01` computes `depth01` from the chosen channel and applies the
invert option; `sample_pixel` then maps it:

```text
depth = (backDepth + (frontDepth - backDepth) * depth01) * depthScale
```

Defaults: `backDepth = -1`, `frontDepth = 1`, `depthScale = 1`, `invert = false`. `depthScale` must be non-negative
(error otherwise); use `invert` to reverse white and black. Channel modes: `AverageRGB` (default), `R`, `G`, `B` and
`Luminance` (weights 0.2126, 0.7152, 0.0722).

A pixel with `depth01 <= 0` is treated as missing, not as depth zero (D's `zeroDepthIsMissing`, documented in the module
header). This goes beyond the original proposal, which only used alpha to mark missing data.

A layer may carry its own transform on the sampled depth: `value * depthScale + depthOffset` (`Prepared::value`), plus a
per-layer `invert` that is XORed with the global one.

## Valid Pixel Rules

A pixel is valid when its alpha, times the layer opacity, is greater than `alphaThreshold` (default `0.01`, must be in
`[0, 1]`). A transparent pixel never counts as black. When the matched `Part` (or, for a
deformer-name match, the active Parts under that grid) has a texture, the alpha comes from that artwork instead
(`attach_coverage`, `coverage_alpha`, using the Part mesh and UVs when present), so depth outside the artwork is dropped
(test `part_coverage_rejects_depths_outside_the_artwork`). A texture that cannot be decoded fails the import.

## Convolution Sampling

`sample_layer` samples per layer before layers are combined. Modes (`convolution`): `Nearest`, `Box3x3`, `Box5x5`,
`Gaussian3x3` (default), `Gaussian5x5`, `Median3x3`, `Frontmost3x3`, `Backmost3x3`, and the `Custom` variants of Box,
Gaussian, Median, Frontmost and Backmost. `customRadius` is an integer in `[1, 64]` (default 3).

- Only valid pixels take part. A kernel with no valid pixel yields no depth for that layer.
- Box and Gaussian kernels normalize by the sum of weights. In Rust each weight is also multiplied by the pixel alpha.
  Gaussian3x3 uses 1-2-1, Gaussian5x5 uses 1-4-6-4-1, `GaussianCustom` uses `sigma = max(radius / 2, 1)`.
- Median sorts the valid depths and takes the element at `len / 2`.
- Frontmost takes the largest and Backmost the smallest converted depth, so both stay consistent when `invert` is on.

## Layer Compositing

For each grid vertex the layers of that grid are tried front first and the largest value wins; equal depths keep the
front layer. Layers are never averaged. A layer with `depthEnabled = false` follows the nearest enabled depth layer
below it of the same grid wherever it has pixels (`resolved_value`).

## Missing-Data Policy

`missingPolicy` (default `KeepExisting`):

- `KeepExisting`: the depth of the nearest sampled vertex of the same grid (`NearestSampled`; ties go to the lowest index),
  then the grid's existing depth at that vertex, then `0`. The nearest-vertex fill goes beyond the original proposal,
  which fell back to the existing depth only.
- `SetZero`: `0`.
- `SetBack`: `backDepth * depthScale`.
- `SkipGrid`: the whole grid is left unchanged when any vertex has no sample.

## Apply Step

For each applied grid the importer writes `depths` (non-finite values become 0, `finite_depth_or_zero`) and removes the
saved depth operations (`depth-ops`, `depth-op-base-depths`). After the grids are written,
`reset_applied_source_settings` resets `depthScale` to 1 and `depthOffset` to 0 on every rig source of an applied target
(D did the same in `ngApplyPsdDepthImportResult`). `validate_model` runs on the candidate before it is returned. The
grids whose node changed, plus the targets whose source settings were reset, are returned as `changed_grids`; the session
marks them dirty and flushes the DepthBone deform keys in the same commit (test
`import_with_a_driven_rig_refreshes_deform_in_one_undo`).

## Command Design

`depth.import_psd` and `depth.import_psd.plan` take `path` and these options; every key is optional except `path`:

```text
invert, backDepth, frontDepth, depthScale, channel, convolution, customRadius, alphaThreshold,
missingPolicy, matchDirectGridName,
layers:        [{layerPath, target ("auto" | "ignore" | node id), enabled, visible, depthEnabled,
                 invert, depthScale, depthOffset}],
disabledGrids: [node id]
```

A `layers` entry for a path the PSD does not contain is an error, as is an unknown layer option. The plan and the apply
report share one shape: `document`, `changedGrids`, `matchedLayers`, `unmatchedLayers`, `ambiguousLayers`,
`skippedGrids`, `budget`, `diagnostics`, `targets`, `grids` (sampled, missing, coverage sources, min and max depth,
skipped) and `layers` (status, target, size, visibility, `sampledVertices`, `selectedVertices`, `maskedPixels`,
`zeroPixels`, `depthRange01`). Layer statuses: `Matched`, `Unmatched`, `UnmatchedWithoutGrid`, `UnmatchedManualGrid`,
`Ambiguous`, `Ignored`, `Manual`.

## UI Design

`PsdDepthWindow` ("PSD Depth Map") keeps a draft of the dialog and never edits the document. Every change to the draft
asks for a new plan (`depth.import_psd.plan`), so the tables always show the effect of the draft; Apply sends
`depth.import_psd`. The window also re-plans when the document revision changes.

Contents: PSD path with Browse, Plan, Apply and Close; a Settings group (Invert Depth, Back Depth, Front Depth, Depth
Scale, Channel, Sampling, Custom Radius, Alpha Threshold, Missing Vertex Pixel, Direct Grid Name Match); a memory budget
line and the match counts; diagnostics; a Targets table (Use, Sampled, Missing, Coverage, Min, Max, Status `Will Apply`
or `Skipped`); a Depth Layers table (use box, layer, matched node, Remap `Auto` / `Ignore` / a GridDeformer, Status,
Sampled, Selected, Mask px, Zero px, Range) with an "Only show problem layers" filter; and a detail pane for the selected
layer (Show, Enable Depth, Invert Layer Depth, Z Scale, Z Offset, Reset Z). Remap and per-layer edits are session-local
draft state and are sent as the `layers` option that the MCP tool also takes.

Entry points: `File > Import > Import Depth Map...` (`editor_windows.rs`, `Request::OpenPsdDepth`) and the button
"Import PSD depth map..." in the editor side panel (`editor_ui.rs`). The original idea of also exposing the import in
the Depth Edit tool settings is not implemented: `editor_depth.rs` has no reference to the import.

## Testing

Present in the source; not run for this revision of the document:

- `rust/src/psd_depth.rs` tests: grouped, clipped and masked PSD paths and pixels,
  `nearest_channels_and_frontmost_match_d_float`,
  `psd_round_trip_and_import_are_one_undo`, `import_samples_the_evaluated_transform`,
  `part_coverage_rejects_depths_outside_the_artwork`, `import_with_a_driven_rig_refreshes_deform_in_one_undo`,
  `import_resets_source_depth_and_keeps_sibling_scopes`, and the nearest-vertex search tests.
- `rust/tests/psd_depth_import.rs`: plan and apply reports, remap and ignore overrides, and more.
- `rust/src/editor_psd_depth.rs` tests: request building, plan accessors, headless window frames.

## Resolved Choices

- Vertex-to-document mapping uses the posed world transform and the document-center origin (test
  `import_samples_the_evaluated_transform`).
- Direct deformer-name matching is on by default and exposed as an option.
- Manual remapping is part of the window (`Auto`, `Ignore`, or an explicit grid).
- Named convolution presets and custom-radius variants are both available.
