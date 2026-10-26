# Depth and bone contracts

Depth maps and depth rigs are implemented in the Rust Session.
This document describes their current data and transaction boundaries.
It replaces the historical migration schedule.

## Depth data

`depths` stores one value per target vertex.
The depth commands support `GridDeformer`, `PathDeformer`, and mesh `Part`
targets. Writes validate the target and vertex count.
Non-finite float samples normalize to zero. Clear removes the stored array.

Depth operations remain GridDeformer-only.
They store ordered operations and optional `depth-op-base-depths`.
Applying operations writes the resulting depths.
Grid edits resample depth fields and remap attached-point operations together
with the geometry edit.

PSD depth import has its own supported input restrictions.
An import must not imply support for every PSD encoding or compositing feature.

## Rig data and evaluation

`DepthRigRoot` owns rig bindings. `DepthBone` nodes hold the hierarchy and pose.
Bindings identify targets, source bones, source settings, and influence rules.
Optional `vertexWeights` store explicit source weights.
Without explicit weights, generation uses the configured distance weighting.

Evaluation reads target vertices and depths.
A missing depth sample contributes zero.
Source settings apply depth scale, depth offset, rotation, and weight.
The generated two-dimensional offset is the posed position minus the rest
position after projection to XY.

Preview offsets are transient.
Applying deformation writes the appropriate parameter `"deform"` binding.
The core also implements rig editing, picking, dragging, IK, mirroring,
and fitting operations. Their presence does not prove complete GUI coverage
or parity with another editor.

## Refresh and history

A rig edit marks either the current keypoint or all keypoints for refresh.
An all-keypoints request absorbs a current-keypoint request for the same rig
and parameter. Refresh failure prevents the causing candidate from committing.

Generated offsets join the edit's history entry.
Structural edits update relevant bindings before generation.
Save preserves rig authoring data. INP export removes rig nodes and lifts
their non-rig children according to the export contract.

An ordinary refresh processes at most 262144 vertex samples.
Remaining cells belong to a session continuation cursor.
`deform.continue` and save finish pending work within the existing history.
A cell larger than the ordinary budget is assembled in transient chunks.
The document receives the cell only when all its offsets are available.
A target above 1000000 vertices is rejected.

## CPU and GPU probes

CPU offsets remain authoritative.
`gpu_deform.rs` can compare software or registered OpenGL probe results
against CPU output. Probe packets are transient and are not saved in INP.

Software agreement does not prove hardware execution.
Device backend acceptance requires evidence from the actual backend and device.

## Ownership and remaining boundaries

The shared [Rust core](rust-migration.md) owns revisions, transactions,
persistence, and history. GUI and protocol adapters call that core.

Depth and rig support does not establish complete physics, native Live2D
conversion, vendor rendering equivalence, or distribution acceptance.
Unsupported combinations must return an explicit failure.

Implementation starts in `rust/src/depths.rs`, `depth_ops.rs`,
`depth_resample.rs`, `bone_deform.rs`, `bone_edit.rs`, and `dirty.rs`.
The [architecture](ARCHITECTURE.md) and
[acceptance scope](acceptance-scope.md) describe the wider contracts.
