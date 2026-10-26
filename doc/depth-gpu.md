# DepthBone GPU Offset Calculation Design

Status: partly implemented. The Rust crate (`rust/src`) computes DepthBone offsets on the CPU and stores them. A second
kernel in `gpu_deform.rs` runs the same per-vertex algorithm on a packet and is compared against the CPU result as a
diagnostic; its result is never written to the document. The crate contains no OpenGL code: there is no transform
feedback shader, no fence, no ring of output buffers, no asynchronous readback and no per-frame GPU scheduler. The only
GPU surface is an optional host callback (`vsoul_session_set_gpu_probe`). The sections below say which parts are
implemented and which remain design intent for a host that wants to run the packet on a real GPU.

## Purpose

DepthBone refresh produces target-local XY offsets for the vertices of a `GridDeformer`, `PathDeformer` or `Part` that a
`DepthRigRoot` binds. The expensive part is the per-vertex 3D calculation: lift the vertex into 3D with its depth as
Z, skin it with the source DepthBones, and project back to a target-local XY offset.

The original design moves only that per-vertex step to the GPU and reads the offsets back as CPU data, so everything
else (target resolution, bindings, undo, save/load, export) keeps observing CPU model state. In the Rust port the same
per-vertex step is defined once as a packet (`gpu_deform::Packet`), and the CPU path (`bone_deform::deform_vertex`)
is the authority.

## What the Rust code does

| Concern | Rust symbol | Behavior |
|---|---|---|
| Offsets, one target | `bone_deform::offsets_for_target` | Builds the sources, depths and runtime bones; returns one `[f64; 2]` per vertex. |
| Offsets, one vertex | `bone_deform::deform_vertex`, `deform_vertex_weighted` | CPU kernel. The weighted form is used when the binding stores `vertexWeights`. |
| Runtime bones | `bone_deform::build_runtime_limited` | Rest pose, pose, `skin` and `no_yaw_skin` matrices per bone. |
| Preview / apply | `bone_deform::run_with_key` | `apply = false` returns offsets; `apply = true` writes the `"deform"` key of one keypoint. |
| Writeback | `bone_deform::write_deform_key` | Creates or updates the target's `"deform"` binding at `[x, y]` keypoint. |
| Packet | `gpu_deform::Packet`, `GpuBone`, `GpuSource` | Transient copy of the CPU inputs. Built by `bone_deform::skin_packet`. |
| Parity kernel | `gpu_deform::execute` | Software kernel (`skin_gaussian`, `skin_weighted`) or the host callback. |
| Comparison | `bone_deform::compare_gpu`, `max_abs_offset` | Fails above `ABSOLUTE_TOLERANCE = 1e-4`. |
| Report | `bone_deform::gpu_offsets`, session op `rig.gpu_offsets` | MCP tool `rig_gpu_offsets`; read-only. |
| Host hook | `lib.rs` `vsoul_session_set_gpu_probe`, `gpu_deform::Probe`, `GpuProbeCallback` | C ABI callback, set per `Session`. |

`rig.gpu_offsets` returns `device`, `backend`, `hardware`, `matched`, `maxAbs`, `reason` and the per-target offsets.
`backend` is `"software"`, `"absent"`, or `"opengl-transform-feedback"`. That last value is only a label: it means a
host callback is registered and `device_present()` is true. `device_present()` reads the `VSOUL_GPU` environment
variable (`0`, `off`, `absent`, `false` turn it off) and is not a device probe. `hardware` becomes true only after the
callback returned success and a finite buffer.

## Required Platform

Design intent for a host that implements the callback: macOS OpenGL, which stops at OpenGL 4.1. The required path
should use transform feedback in a vertex shader, with vertex attributes, uniforms and texture buffers for input, and
no compute shaders, SSBOs, image load/store or OpenGL 4.2+ API.

Not verified in the Rust port. The crate does not call OpenGL and does not check what the callback uses.

## Threading

Not applicable to the crate: there is no GPU submission, so there is nothing to poll. The host callback is synchronous.
`execute_probe` rejects a re-entrant call (`reentrant gpu probe callback`). While a callback runs, the FFI entry
points `vsoul_session_request`, `vsoul_session_free` and `vsoul_session_set_gpu_probe` refuse to touch the session
(`callback_is_active`). A host that needs asynchronous GPU work must run it outside the callback.

## Observable Contract

The offsets must keep this shape, and the CPU path and the probe both produce it:

- one `[x, y]` pair per vertex of the target, in vertex order;
- the order is `bone_deform::target_vertices`: a `GridDeformer` grid is row-major (outer loop over `grid_axis_y`, inner
  loop over `grid_axis_x`); a `PathDeformer` uses `vertices`; a `Part` uses `mesh.verts` minus the mesh origin;
- values are target-local XY offsets;
- the array length equals the vertex count, or the write is rejected (`offset length mismatch`).

Preview (`rig.preview_deform`, `op_preview_deform`) keeps offsets in `Session.preview_deformation`, outside the model
and outside undo. Apply (`rig.apply_deform`, `op_apply_deform`) requires a parameter, writes one keypoint of the
`"deform"` binding and commits one history step. A dirty-queue flush writes the same binding through
`dirty::flush` (see the checklist).

## CPU/GPU Responsibility Split

CPU, always: target, root, binding and keypoint resolution; runtime bone pose; packet construction; ancestor-grid
compensation (`compensate_ancestor_grids`); `"deform"` writeback; undo; save/load/export.

Per-vertex work, run by the CPU kernel and mirrored by the probe kernel:

- conversion of `(x, y, depth)` to a 3D rest point per source;
- source influence evaluation;
- weighted skinning;
- conversion back to a target-local XY offset.

## Per-Vertex Algorithm

`deform_vertex` and `gpu_deform::skin_gaussian` implement the same steps. `deform_vertex_weighted` and `skin_weighted`
keep step 3 but replace steps 2 and 4 to 6 with the stored per-vertex weights.

1. `z = raw_depth * world_scale`, where `world_scale = bone_deform::depth_world_scale` (the largest XY extent of the
   bound targets in root space, times `0.42 / 2.9`, at least 1).
2. For each source: `score = weight * multiplier`; skip when `!(score > 0)`.
3. `source_depth = z * depthScale + depthOffset * world_scale`;
   `source_rest = targetToRoot * (x, y, source_depth)`.
4. With more than one source, scale the score by falloff.
   `radius = max(rest_length * 0.85 * radiusScale, radius_floor)`, with
   `radius_floor = max(minimumRadius, 0.18 * target bounds size)`. Falloff is Gaussian
   `exp(-d^2 / radius^2)` or, when the rule says `linear`, `max(0, 1 - d / radius)`, where `d` is the distance to the
   bone's rest segment (`distance_sq_point_segment` clamps the projection).
5. A terminal, lock-to-root source replaces all other candidates when the unclamped projection of its rest point onto
   the segment from the parent's rest head to its own rest head (`point_segment_projection`) is greater than 1 and
   the distance to that segment is within `radius`; the highest score wins (`locked`).
6. Keep the strongest `min(maxInfluences, 16)` candidates in fixed arrays (`insert_influence`), normalize their scores;
   if the total is not above `1e-8`, use the first candidate with weight 1.
7. Pose each candidate rest point with the bone's `skin` matrix (see `psd-depth-rotation.md` for the yaw-radius branch)
   and blend by the normalized weights.
8. `offset = (rootToTarget * deformed).xy - (rootToTarget * rest).xy`.

A vertex with no candidates gets `[0, 0]`. A non-finite value raises `non-finite matrix` (CPU) or
`non-finite gpu buffer` (probe).

## Packet Layout

`skin_packet` copies the inputs, and `execute_probe` flattens them for the callback as `f32`:

- `vertices_xy`: `x, y` per vertex; `depths`: scaled depth per vertex (raw depth times `world_scale`).
- `bones`: 24 floats per bone: `rest_head` (3), `rest_length`, `rest_tail` (3), parent index (`-1` for none), `skin`
  (16, row-major).
- `sources`: 28 floats per source: bone index, `terminal`, `lock_to_root`, `linear`, `weight`, `depth_scale`,
  `depth_offset` (already times `world_scale`), `multiplier`, `sin_rotation`, `cos_rotation`, `pivot_shift`,
  `pose_yaw`, then the bone's `no_yaw_skin` (16, row-major).
- `weights`: `sources x vertices` floats, present only when the binding stores `vertexWeights`; `mode` is then 1.
- `target_to_root`, `root_to_target` (16 floats each), `vertex_count`, `bone_count`, `source_count`, `max_influences`,
  `radius_floor`, `radius_scale`.

`GpuProbePacket` is `#[repr(C)]`; the test `gpu_probe_packet_uses_c_layout` pins its field offsets.

## Fixed Limits

Limits are errors, not silent truncation:

| Limit | Value | Where it fails |
|---|---|---|
| Bones | 64 (`gpu_deform::MAX_BONES`) | `gpu_deform::execute` rejects the packet. The CPU offsets are still produced and stored. |
| Sources | 128 (`bone_deform::MAX_SOURCES`) | `offsets_for_target` returns an error. |
| Influences | 16 (`MAX_INFLUENCES`) | `offsets_for_target` returns an error when the rule asks for more. |
| Vertices | 1,000,000 (`bone_deform::MAX_VERTICES`) | `offsets_for_target` and `dirty::reject_cell_size` return an error. |

`build_runtime_limited(..., limit_bones = true)` also enforces the 64-bone cap, but the deform path passes `false` so a
rig above the cap still deforms on the CPU.

## Scheduling

Implemented in `dirty.rs`, and unrelated to the GPU:

- There is no frame loop. A mark (`mark_root`, `mark_target`, `mark_bone`, `mark_pose`, `marks_for_structure`) is
  flushed once, inside the commit of the edit that caused it (`Session::commit_marked` calls `dirty::flush`). A flush
  failure is returned to the caller and the edit is not committed.
- An all-keypoints flush writes whole cells (one target at one keypoint) up to `ALL_KEYPOINTS_BUDGET = 262,144`
  samples. A cell larger than the budget, up to `MAX_VERTICES`, is assembled in vertex chunks
  (`bone_deform::cell_offset_range`, `commit_cell_offsets`).
- The remainder stays in `Session.deform_cursor`. `deform.continue` runs the next slice, and the editor calls it each
  frame while `deformPending` is true (`editor_ui::drive_deform`). Other edits are refused until the refresh finishes
  (`allows_pending_deform`). Save finishes the refresh on a copy first (`staged_flush`).

Not implemented in the Rust port: submitting jobs to a GPU, fence polling, and a submission or readback budget per
frame.

## Asynchronous Readback

Design intent only. Not implemented in the Rust crate.

A host that runs the packet on a GPU must not block the main loop. Submit the transform feedback pass in one frame and
insert a fence. Poll the fence with a zero timeout in later frames. Read back and apply only completed jobs. Use a small
ring of output buffers so the CPU never reads a buffer the GPU may still write, and the GPU never overwrites a buffer
whose result has not been applied. Do not use `submit -> glFinish -> readback -> apply` on the normal path.

The Rust callback contract is simpler: it is synchronous and fills `out_xy` before it returns.

## CPU Reference And Compatibility

The CPU kernel is the authority, not a fallback.

- `bone_deform::run_with_key` calls `compare_gpu` only when `apply` is true and `device_present()` is true, and drops
  the comparison result (`let _ = compare_gpu(...)`). `commit_cell_offsets` does the same for assembled cells.
- A mismatch above `1e-4`, a non-finite probe buffer or a failed callback never replaces the CPU offsets and never
  blocks the edit. `rig.gpu_offsets` reports it in `matched: false` and `reason`.
- With `device_present()` false, `rig.gpu_offsets` returns `device: false, backend: "absent"` and edits are unaffected.
- A software run (no callback) compares the software kernel with the CPU kernel, so the offsets of `rig.gpu_offsets`
  and the stored deform keys should agree within the tolerance.

This inverts one rule of the original design: a probe failure is reported, but the CPU result is written regardless.

## Validation

Present as Rust tests; not run for this revision of the document:

- `gpu_deform::tests::weighted_ik_pose_matches_and_a_bad_gpu_keeps_cpu_offsets`: weighted pose matches within
  tolerance; injected mismatch, non-finite buffer and an absent device leave the stored deform values unchanged.
- `gpu_deform::tests::sixty_five_bones_fail_the_kernel_without_a_short_buffer`: 65 bones are an error.
- `gpu_deform::tests::callback_failure_is_reported_and_nested_callbacks_are_rejected`.
- `gpu_deform::tests::gpu_probe_packet_uses_c_layout`.
- `lib.rs` `gpu_host_tests::callback_cannot_reenter_replace_or_free_the_active_session` and
  `host_registration_survives_document_replacement_and_stays_session_local`.
- `dirty::tests::chunked_offsets_match_the_small_cell_path`, `large_deform_cell_matches_the_small_cell_path`,
  `over_budget_flush_continues_on_the_same_history_entry`.

Not verified in the Rust port: GridDeformer versus PathDeformer parity on a real GPU, and OpenGL 4.1-only API use by a
host callback; neither is exercised by code in this repository.
