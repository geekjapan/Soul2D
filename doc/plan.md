# Depth Bone Auto Refresh

Status: implemented in `rust/src/dirty.rs`, with the marks assigned in `rust/src/lib.rs`. A change to a Depth Bone, a
binding, a target's depths or a target's geometry marks a refresh. The refresh recomputes the bound deform keys once,
inside the command, and commits the edit and the refreshed keys as one undo step. Not implemented: a frame loop, a
deferred "flush at the end of the UI update", and a separate refresh action. The editor has no queue of its own. It
sends session operations, and each operation flushes before it returns.

## Goal

Depth Bone rigging refreshes all bound deformers when a relevant bone, binding, target depth or target mesh setting
changes. Nothing runs when nothing changed. One edit refreshes a given rig, parameter and scope at most once.

## Core design

A dirty request queue, flushed once per edit.

Each session operation that can change rig inputs builds a list of `dirty::Mark` values and passes it to
`Session::commit_marked` (`lib.rs`). `commit_marked_keeping` calls `dirty::flush` on the candidate model and commits
the flushed model. The mark list and flush live inside one call:

```text
no marks:
  dirty::flush -> absorb(marks) is empty -> returns the model unchanged, no evaluation

marks:
  dirty::flush -> absorb -> preview_undriven -> plan_cells -> write_cells
  Session::commit_model_keeping(flush.model)   one history entry: the edit and the refreshed keys
```

If the flush returns an error, `commit_marked_keeping` returns it before `commit_model_keeping`, so the edit that
caused the refresh is not committed. The test `failed_convert_refresh_is_not_retried` covers that path. The refresh
therefore never leaves a half-written state to undo: the edit and its deform keys are one snapshot.

## Mark and scope

```text
Mark { root, parameter: Option<u32>, keypoint: [usize; 2], target: Option<u32>, scope }
Scope::Keypoint       one keypoint of one parameter
Scope::AllKeypoints   every keypoint of the parameter's axes (all x * all y)
```

`Scope::Keypoint` is made only by `dirty::mark_pose`. Its callers are the edits that write a pose into one keypoint of
one parameter: `binding.set_trs` (`op_set_trs`), `rig.apply_mmd_pose` and `rig.bake_mmd_motion`. A mark is made only
for `DepthBone` nodes whose rig has a binding target.

`Scope::AllKeypoints` is made by every other source, with `parameter = None`, which means "every parameter that drives
this rig":

| Source | Mark |
| --- | --- |
| add source, remove source, source settings, influence rule, vertex weights set/clear | `mark_root` |
| standard skeleton, standard parameters, `rig_fit_human`, `rig_fit_root_z`, `rig_mirror`, `rig_solve_ik` | `mark_root` |
| `rig_import_gltf`, glTF mesh import into a bound rig | `mark_root` |
| `rig_set_rest`, `rig_set_constraint`, `rig_fit_bone_z` | `mark_bone` (the rig root of the bone) |
| `rig_drag_bone`, `node.transform` on a `DepthBone` (request carries `translation`, `rotation` or `scale`, or a TRS `component`) | `mark_bone` |
| `node.transform` on a `DepthRigRoot` | `mark_root` |
| target vertices or grid axes, mesh edit, automesh, `deformer.extend_grid`, node move or property set | `mark_target` |
| depths: set, clear, depth-ops edits and apply, `depth.edit.apply`, PSD import | `mark_target` |
| delete, cut, paste, convert, `deformer.apply_to_children` | `marks_for_structure` (diff of `bindings` and bone set) |

`mark_target` creates a mark for each rig whose binding targets include the node or have it as an ancestor
(`affects_root`). The mark carries `target`, so only that target and its bound descendants refresh
(`scoped_targets`). A `z-sort` change on a bone is not a TRS change and does not refresh
(`transform_changes_trs`).

A bone pose edit through `rig_drag_bone` or `node.transform` therefore refreshes every keypoint. Only a pose written
into a parameter keypoint refreshes that keypoint alone. A shared input (rest, source list, rule, depths, mesh)
always refreshes every keypoint, because every keypoint is generated from the same inputs; refreshing one would leave
the others stale.

## Deduplication

`dirty::absorb` collapses marks that overlap:

- Same root and parameter, and the same target or either target is `None`: if either is `AllKeypoints`, the result is
  `AllKeypoints`. A `None` target absorbs a specific target.
- Two `Keypoint` marks with different keypoints are both kept. Equal ones merge.
- A root-level `AllKeypoints` mark (`parameter: None`) drops every parameter-specific mark for the same root unless
  that mark is also `AllKeypoints`.
- A parameter-specific `AllKeypoints` drops `Keypoint` marks for the same root and parameter.

The refresh covers every binding of the rig that passes the target filter. It does not refresh a single target
by dependency analysis, because influence can spread through the source list, the bone hierarchy,
`allowParentToTargets`, distance-based scoring, source depth settings, ancestor grids and the target mesh.

## Refresh

`dirty::flush`:

1. `absorb(marks)`.
2. `preview_undriven`: a mark whose rig has no driving parameter generates offsets with no parameter and stores them
   only in the session preview (`Session::preview_deformation`). The document gets no deform key.
3. `plan_cells`: for each mark, find the targets (`scoped_targets`) and the parameters (`mark_parameters`), then list
   `Cell { root, parameter, keypoint, target }`. For `AllKeypoints`, the keypoints come from `all_keypoints` (the
   parameter's axis points). Within one key, cells are ordered ancestor first (`ancestor_depth`) so a child reads the
   offsets already stored for its bound ancestor grid.
4. `write_cells`: cells of one key written together use `bone_deform::run_with_key` and write the target's `"deform"`
   key (`write_deform_key`). The key closest to the parameter's current value also updates the session preview, so the
   viewport shows the result. A cell larger than the budget is assembled in vertex chunks
   (`cell_offset_range`, `commit_cell_offsets`).

The parameters of a mark are those whose bindings drive a bone of that rig (`parameter_drives_rig`): any
`transform.t.*`, `transform.r.*` or `transform.s.*` binding on a `DepthBone` below the root. A mark with an explicit
parameter refreshes only it, and only if it drives the rig. Rust has no "armed parameter" in the dirty queue. The
`parameter` argument of `rig_apply_deform` and `rig_preview_deform` is the explicit counterpart for those tools, and
a missing one fails with "No armed parameter".

## Budget and the pending cursor

`ALL_KEYPOINTS_BUDGET` is 262,144 samples per call; a cell is one target at one key, so its cost is the target's vertex
count. `write_cells` stops when the budget is used. The rest is returned as `Flush.pending` and kept on the session
(`deform_cursor`, `deform_partial`, reported as `deformPending` in the snapshot). `deform_continue`
(`Session::op_deform_continue`, `dirty::continue_cells`) writes the next cells on the same history entry
(`rewrite_current`). While cells are pending, other operations fail with "a deform refresh is still pending" unless
`allows_pending_deform` lists them (`deform.continue`, `save`, `undo`, `redo`, history operations, and read-only,
export-status and preset operations such as `snapshot`, `rig.list_bones`, `rig.preview_deform` and `rig.overlay`).
A cell with more than `bone_deform::MAX_VERTICES` (1,000,000) vertices fails in `plan_cells` before any write (`reject_cell_size`). Tests:
`over_budget_flush_continues_on_the_same_history_entry`, `over_budget_all_keypoints_leaves_revision_unchanged`,
`failed_save_keeps_the_pending_cursor`.

## Undo

The edit and its refresh are one history step (`commit_model_keeping`). When cells are still pending, the step is kept
even if the model is unchanged, and `deform_continue` rewrites that step. The test
`pose_edit_updates_one_key_and_depth_edit_updates_every_key_in_one_undo` covers a keypoint pose edit and an
all-keypoint depth edit, each in one undo. There is no later "merge the refresh into the configuration action"
step. It is already the design.

## UI drag behavior

The egui inspector holds a draft value while a number control is dragged or focused, and sends one request when the
drag stops or focus leaves (`editor_inspector::draw_numbers`). One drag of a bone rest field is one `rig.set_rest`
request and one refresh. Not verified in the Rust port: bone dragging in the viewport, since no pointer handler for
`rig.drag_bone` exists in `editor_viewport.rs`.

## Related

- `bone.md` describes the deform itself.
- `depthbone-conversion-recovery.md` covers delete, convert and paste.
