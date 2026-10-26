# Depth map and depth draw

Status: run live against the headless host (`depth_set`, `depth_list`, `DepthMapCommand_AddDepthOp` / `ApplyDepthOps` / `ListDepthOps`, all eight `depth_ops_*`,
`depth_draw_load_png`, `depth_draw_binding_update`, `depth_draw_compose`, `depth_draw_apply`). PSD import is written from the manifest only.

Per-vertex depth values on a GridDeformer (or depth-mapped PathDeformer, or the mesh vertices of a Part, which a rig may bind: `depth_set`, `depth_list` and `depth_clear` only). Three ways to get them: set them directly, bake a list of
saved operations, or compose DepthDraw layers (grey PNG planes bound to targets). Depths feed the depth rig (depth-bones-rig.md).

## Sequence

1. Explore: `nodes_list {}` for GridDeformer UUIDs; set the grid axes first (`deformer_define_grid`, deformers.md).
   `depth_list {target}` answers `{count, depths}`; a grid with no depths gives `count:-1, depths:null`.
2. Act, by route:
   - direct: `depth_set {target, depths:[...]}`, one value per grid vertex (3x3 axes give 9). `depth_clear {target}` removes them.
   - operations: `DepthMapCommand_AddDepthOp {target, index, operation}` (or `DepthMapCommand_SetDepthOps {target, operations}`), then
     `DepthMapCommand_ApplyDepthOps {target}` bakes them into `depths`. Operation objects have `type` `plane` (`center`, `radiusX`, `radiusY`,
     `angle`, `targetDepth`, `flattenStrength`), `ring` (`p0`, `p1`, `amount`, `width`, `hardness`, `p0Angle`, `p1Angle`) or `attached-point` (`index`, `amount`).
   - DepthDraw layers: `depth_draw_load_png {path, id?}` loads a grey PNG as a layer; `depth_draw_binding_update {layer, target}` binds it (or
     `depth_draw_auto_bind` matches layers to Parts and grids by name); `depth_draw_compose {target}` previews; `depth_draw_apply {target}` writes depths as one undo step.
   - PSD: `depth_import_psd_plan {path}` (dry run), then `depth_import_psd {path, ...}`.
3. Read: `depth_list {target}`; `DepthMapCommand_ListDepthOps {target}`; `depth_draw_manifest_get {}` for layers, bindings and selection.
4. Confirm: `depth_list` shows the new values; `depth_draw_compose` reports `layerStats`, `missingVertices` and what apply would change.
   `depth_draw_diagnostics {}` lists layer planes and gaps. The depth values are data; to see their effect wire them to a rig (depth-bones-rig.md) and `render`.

## Tools: depth values and operations

| Tool | Purpose and minimum arguments |
| --- | --- |
| `depth_list` / `DepthMapCommand_ListDepths` | `{target}` |
| `depth_set` / `DepthMapCommand_SetDepths` | `{target, depths}` length must equal the vertex count of the target (grid points, path vertices or the mesh vertices of a Part). |
| `depth_clear` / `DepthMapCommand_ClearDepths` | `{target}` |
| `depth_ops_list` / `DepthMapCommand_ListDepthOps` | `{target}` saved operations with `indexInList`. |
| `depth_ops_add` / `DepthMapCommand_AddDepthOp` | `{target, index, operation}` |
| `depth_ops_update` / `DepthMapCommand_UpdateDepthOp` | `{target, index, operation}` |
| `depth_ops_move` / `DepthMapCommand_MoveDepthOp` | `{target, fromIndex, toIndex}` |
| `depth_ops_remove` / `DepthMapCommand_RemoveDepthOp` | `{target, index}` |
| `depth_ops_set` / `DepthMapCommand_SetDepthOps` | `{target, operations}` replaces the list. |
| `depth_ops_clear` / `DepthMapCommand_ClearDepthOps` | `{target}` |
| `depth_ops_apply` / `DepthMapCommand_ApplyDepthOps` | `{target}` bake into depths; answers the depth list. |
| `depth_edit_apply` | `{target, operations, directEdits?, depths?}` the depth editor's pending edits as one undo step. |
| `depth_import_psd_plan` / `depth_import_psd` / `DepthMapCommand_ImportPSDDepths` | `{path, ...}` sample a PSD depth map onto grids; the plan changes nothing. |

## Tools: DepthDraw (session state, separate from the document history)

| Tool | Purpose and minimum arguments |
| --- | --- |
| `depth_draw_manifest_get` / `depth_draw_manifest_set` | Read the session; replace it with `{manifest}` (an object, a JSON string, or `null` to clear) or `{path}`. A call with neither fails with "manifest or path is required". Layers then have no pixels until `depth_draw_load_png`. |
| `depth_draw_load_png` | `{path, id?}`; a relative `path` is looked up beside the open document first, then in the working directory (so a copy from `export_collect` reloads `assets/...`). |
| `depth_draw_layer_update` | `{layer, ...}` offsets, depth range, channel, convolution, visibility. |
| `depth_draw_binding_update` | `{layer, target, enabled?, order?, mergePolicy?, coverageThreshold?}` creates or changes a layer binding. |
| `depth_draw_auto_bind` | `{matchDirectGridName?}` |
| `depth_draw_select` | `{layer?, target?, point?, clear?}` |
| `depth_draw_compose` | `{target?}` read only. |
| `depth_draw_apply` | `{target?}` one undo step. |
| `depth_draw_diagnostics` | `{target?}` read only. |
| `depth_draw_cleanup` | `{layer, kind: gap_fill\|contour_repair, ...}` |
| `depth_draw_fit_z` | `{layer, gap?, target?, margin?, apply?, ...}` fit a layer into a depth gap; `gap` defaults to `adjacent`, `target` to the selected one; `apply:false` only reports. |
| `depth_draw_display` | Display flags only. |
| `depth_draw_export_png` | `{dir, manifest?, overwrite?}` writes one grey PNG per layer plus a manifest. ACP requires host approval for the full output list; pending approval creates nothing. Input files and duplicate output destinations are refused. |

## Gotchas

- Verified: `depth_draw_binding_update` replies `created:true` with `mergePolicy: Frontmost`, `targetGridUuid` and `targetNodeUuid` equal.
- Verified: a 64x64 horizontal grey ramp bound to a 3x3 grid composed to `[-0.62, 0.004, 0.63]` per row and applied as such; the ramp's `backDepth -1` / `frontDepth 1` and
  the PNG's grey scale decide the range.
- Verified: `depth_draw_apply` overwrote values that `depth_set` had written before; the last write wins, with no merge.
- Verified: the snapshot reports DepthDraw counts as `depthDraw {layers, bindings, revision}`; its own `revision` is separate from the document `revision`.
- Verified for `import_folder`: the snapshot's `depthDraw` went from `layers:1` to `layers:0`. `new` and `open` replace the document the same way but were not run with a loaded layer.
- Verified: the depth tools of the first table, other than the `depth_import_psd*` row (`path`, no target), read `target`, then `id`, then a `nodes` or `ids` of one UUID, so the manifest requires none of them (run: `depth_list {id}`); with no target at all they fail with "target must be a node UUID"
  and change nothing. The `DepthMapCommand_*` names also take `context:{nodes:[target]}` when `target` is absent; two nodes there answer "exactly one depth target is required".
- Not run: `depth_import_psd*`, `depth_draw_cleanup`, `depth_draw_fit_z`, `depth_draw_auto_bind`, `depth_draw_export_png`, `depth_edit_apply`, and the ring and attached-point operations.
