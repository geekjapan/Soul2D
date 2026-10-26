# DepthBone GPU Compatibility Checklist

Status: rewritten for the Rust crate. The original checklist compared a D GPU implementation against a D "CPU+Worker" baseline.
That baseline, the D source files it cites and the `dub` build are not part of this repository; no D source remains. The rows below keep each rule that has a Rust
counterpart and say which D rows have none. The Rust GPU path is a diagnostic probe only (see `depth-gpu.md`), so the
main rule changes from "GPU replaces only the offset calculation" to "the CPU result is always the one stored".

Rule: the probe may compare offsets and report a mismatch. It never changes triggers, dirty queue semantics, target
resolution, writeback destinations, action grouping or undo behavior.

## Rust Checks

Each row names the Rust symbol to read and, where one exists, a test. "Not verified in the Rust port." marks a claim of
the D checklist that I could not confirm in `rust/src`.

| # | Rule | Rust location | Check |
|---|---|---|---|
| 1 | A dirty mark is flushed once, before the causing edit commits; a flush failure leaves the edit uncommitted. | `dirty::flush`, `Session::commit_marked_keeping` | `dirty.rs` header ("Flush failure is returned to the caller"); no named test read |
| 2 | An empty queue does not evaluate. | `dirty::flush` (`marks.is_empty()`) | `dirty.rs` header |
| 3 | `AllKeypoints` absorbs `Keypoint` for the same rig and parameter; duplicate marks collapse. | `dirty::absorb` | test `all_keypoints_absorbs_a_keypoint_and_duplicate_marks_collapse` |
| 4 | Pose edits mark one keypoint; depth and structure edits mark all keypoints. | `dirty::mark_pose`, `mark_target`, `mark_bone`, `mark_root`, `marks_for_structure` | test `pose_edit_updates_one_key_and_depth_edit_updates_every_key_in_one_undo` |
| 5 | A binding edit of a DepthBone transform channel marks the rig. | `Session::op_set_trs` calls `dirty::mark_pose` | Op `binding.set_trs`. Other ops that write channel bindings: not verified in the Rust port. |
| 6 | PSD depth import and its refresh are one undo step. | `Session::op_import_psd` -> `commit_targets(model, changed_grids)` | test `psd_depth::tests::import_with_a_driven_rig_refreshes_deform_in_one_undo` |
| 7 | Work not finished in one flush is not lost, and other edits wait for it. | `Session.deform_cursor`, `allows_pending_deform`, `op_deform_continue`, `editor_ui::drive_deform` | test `over_budget_flush_continues_on_the_same_history_entry` |
| 8 | Save and export see finished model state, never a half refresh. | `Session::staged_flush`, `op_save` | test `failed_save_keeps_the_pending_cursor` |
| 9 | Preview writes only session state, not the document. | `Session::op_preview_deform` (`preview_deformation`) | `bone_deform::run_with_key` with `apply = false` |
| 10 | Apply writes the `"deform"` binding at one keypoint and commits one history step. | `Session::op_apply_deform`, `bone_deform::write_deform_key` | `rig_apply_deform` requires `root` and `parameter` |
| 11 | An all-keypoints refresh writes every key of every driving parameter. | `dirty::plan_cells`, `write_cells` | tests `two_by_two_refresh_writes_every_cell_and_previews_the_armed_key`, `chunked_offsets_match_the_small_cell_path` |
| 12 | A cell too large for one flush is assembled in chunks and written only when complete. | `bone_deform::cell_offset_range`, `commit_cell_offsets` | test `large_deform_cell_matches_the_small_cell_path` |
| 13 | A cell above `MAX_VERTICES` fails before any write. | `dirty::reject_cell_size` | test `over_budget_all_keypoints_leaves_revision_unchanged` |
| 14 | A child grid's cell subtracts the offset inherited from a bound ancestor grid. | `bone_deform::compensate_ancestor_grids`, `settle_offsets` | test `child_cells_follow_ancestor_offsets_when_the_child_was_bound_first` |
| 15 | The probe never replaces CPU offsets. A mismatch, a non-finite buffer, a failed callback or an absent device leaves the stored deform values unchanged. | `bone_deform::run_with_key` (`let _ = compare_gpu`), `commit_cell_offsets` | test `gpu_deform::tests::weighted_ik_pose_matches_and_a_bad_gpu_keeps_cpu_offsets` |
| 16 | An unsupported packet is an error, not a short buffer. | `gpu_deform::execute` (64 bones) | test `sixty_five_bones_fail_the_kernel_without_a_short_buffer` |
| 17 | A host callback cannot re-enter, replace or free the session. | `gpu_deform::callback_is_active`, `vsoul_session_*` | tests `callback_cannot_reenter_replace_or_free_the_active_session`, `callback_failure_is_reported_and_nested_callbacks_are_rejected` |
| 18 | The probe packet keeps its C layout. | `gpu_deform::GpuProbePacket` | test `gpu_probe_packet_uses_c_layout` |
| 19 | The terminal-lock test uses the unclamped projection (`> 1`); the distance to the segment clamps internally. | `bone_deform::point_segment_projection`, `distance_sq_point_segment`; `gpu_deform::projection`, `distance_sq_segment` | Read both pairs; they are the same. |
| 20 | A source is terminal when no other source of the same binding has it as parent. | `bone_deform::source_records` (`terminal` from `present`) | Not verified in the Rust port by a named test. |
| 21 | Source depth is `(raw * depthScale + depthOffset) * world_scale`. The packet stores `raw * world_scale` as depth and `depthOffset * world_scale` as offset. | `offsets_for_target` (`scaled`, `depth_offset: source.depth_offset * world_scale`), `skin_gaussian` (`z * depth_scale + depth_offset`) | Read both. |
| 22 | Non-finite scores are rejected with comparisons that are false for NaN (`!(score > 0.0)`, `!(total > 1e-8)`). | `deform_vertex`, `gpu_deform::skin_gaussian` | Read both. |

## D History With No Rust Counterpart

These rows of the old checklist described D files and loops that do not exist in the crate. Do not port them.

- `incUpdate()` / `incUpdateNoEv()` calling `ngFlushDepthBoneDirty()` at the end of the update loop. Rust has no frame
  loop in the core; the editor only calls `deform.continue` while `deformPending` is true.
- "Without a GPU backend, dirty work stays pending". Rust does the opposite: the CPU always writes, and an absent
  or failing probe only changes the report of `rig.gpu_offsets`. `ngFlushDepthBoneDirtyImmediate()` has no equivalent;
  `staged_flush` runs the cursor to the end on a copy.
- `ngBeginDepthBoneRefreshActionSink`, `PsdDepthImportRefreshJob`, `ngPendingDepthBoneRefreshWorkForSink`: replaced by
  the single commit in row 6.
- `ngDepthBoneBindingValueChangeHook`, `DeformationViewport.paramValueChanged()`, the parameter-panel right-click
  notification and the render hook `ngDepthBoneBeforeRenderPlayback()`: not verified in the Rust port. `dirty.rs`
  has no armed-parameter fallback; a mark without a parameter uses every driving parameter (`mark_parameters`).
- `ngCheckDepthBoneFingerprints()`: `dirty.rs` has no fingerprint check; not verified in other files.
- `pendingDepthBoneGpuDispatches`, `applyDepthBoneReadback()`, `refreshDepthBoneGpuTarget()`: no async GPU queue exists.

## Verification

Commands for the Rust tests that back the rows above (from the repository root; add `--features gui` to include the
editor tests):

```text
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust gpu_deform
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust gpu_host_tests
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust dirty::tests
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust bone_deform::tests
```

These tests exist in the source; they were not run for this revision of the document, so no pass result is claimed.
The D-era `dub build` and `soul2d-regression-tests.exe` results of the old checklist are not reproducible.
