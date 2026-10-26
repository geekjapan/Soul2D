# Split an illustration into Parts

Status: run live against the headless host (`split_open`, `split_part_set`, `split_plan`, `split_apply`, error on a bad step). The full workflow, step choices and failure cases are in
[.cursor/skills/split-illustration/SKILL.md](../../split-illustration/SKILL.md) and [doc/part-split.md](../../../../doc/part-split.md); read the skill before a real split.

Turn one flattened RGBA picture into named Parts. The draft (source image plus part recipes) lives beside the document and is not in its history; `split_apply` is the one
undoable write.

The editor offers **Split this image** inside Coding agent for an open draft and an HTTP-capable ACP agent.
It prepares text only. Send remains explicit, and the ACP endpoint refuses `split_apply`: a person applies the
current draft from **Part split review**. This entry and gate have fixture-agent tests; real AI-agent, provider
consent and native GUI acceptance remain pending. Source and mask PNG resources are documented in the split Skill.

## Sequence

1. Explore: `split_open {path}` answers `{width, height, revision}` (the draft's own revision). `split_overlay {}` returns the picture as an image content item. `split_components {}` lists alpha islands and their seeds.
2. Act: `split_part_set {name, steps:[...]}` once per part, back to front. Steps are objects with an `op` string: `alpha`, `component {point}`, `flood {point, tolerance}`, `boundedFill`,
   `polygon`, `box {rect:[x,y,w,h]}`, `part {name}`, `grow`, `shrink`, `feather`, `side`.
3. Read: the `split_part_set` reply carries `part {pixels, bbox, overlaps}`, `unclaimedPixels`, `multiplyClaimedPixels`; `split_plan {}` returns the replayable `plan` plus the same statistics.
4. Act: `split_apply {automesh?, parent?, plan?}` creates one Part per recipe in one undo step; `plan` replays a saved plan without an open draft.
5. Confirm: `nodes_list {}`; `render` (render-scene.md); `undo` removes every new Part at once.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `split_open` | `{path}` PNG, TGA, JPEG, or a PSD flattened from its visible layers (a PSD without layers is refused; clipping is the importer's hard clip; one whose decoded layers exceed 640 MiB is refused for memory). 8192 px a side, 16 MP. Clears the draft's parts; leaves the document alone. |
| `split_overlay` | `{part?}` read only image. |
| `split_components` | `{threshold?, offset?}` read only; alpha > threshold (default 0), four-connected islands, 32 per page in row-major seed order. Returns `components:[{seed,pixels,bbox}]`, exact `totalComponents`, `nextOffset` or null. Every page rescans the source; keep the same source and threshold. |
| `split_mask_inspect` | `{path}` read only; inspect a source-sized grayscale-8 PNG and return its canonical raster `step`, `width`, `height`. Pass the step to `split_part_set`. Keep the mask file unchanged for every evaluation/replay; this runs no model. |
| `split_part_set` | `{name, steps, index?, padding?}` same name replaces the part. |
| `split_part_order` | `{names}` a permutation, back to front. |
| `split_part_remove` | `{name}` refused while another part refers to it. |
| `split_plan` | `{}` read only. |
| `split_apply` | `{automesh?: true \| {algorithm}, parent?, plan?}` |
| `split_close` | `{}` drop the draft. |

## Gotchas

- Verified: `{"op":"alpha","threshold":0.5}` fails with "invalid type: floating point `0.5`, expected u8"; `threshold` is an integer 0..255. A `component` at `[32,32]` on a 64x64 disc claimed 1789 pixels, bbox `[9,9,47,47]`.
- Verified: `split_apply` answered `created:[{id, name, size, trans}]` and an `automesh {meshed, skipped}` block even without `automesh`.
- Per the split-illustration skill (not re-run here): `new`, `open` and `import_*` drop the draft, and a second `split_apply` adds the Parts again, so `undo` first.
- Bone-named Parts feed `rig_fit_human` (depth-bones-rig.md).
- Raster steps require the `crc32` returned by inspection. Source alpha is applied to textures later; mask coverage remains exact. Missing or changed files refuse. The editor may cache a preview until the draft changes; Apply always rechecks the file.
