# PSD Depth BoneSource Rotation Design

Status: implemented in the Rust crate for the CPU path, the probe packet and Fit Z. `BoneSource.rotation` is the
`rotation` field of a binding's source settings (`rig.set_source_settings`). It shifts the pivot of the yaw rotation per
source (`bone_deform::rotation_pivot_shift`) and never moves the rest mesh. Not verified in the Rust port: an editor
Inspector control for the source rotation (no editor code calls `rig.set_source_settings`), and persistence of the
field in saved INX files. Not applicable: the D GPU stale-input fingerprints and embedded GLSL checks (the crate has no
asynchronous GPU path and no GLSL). "PSD" in the file name is historical: the feature belongs to the DepthBone rig, not
to the PSD importer.

## Purpose

`BoneSource.rotation` sets, per source, the depth radius of the yaw rotation. The radius comes from the angle of a
tangent that runs from the DepthBone origin to the displayed mesh.

The value never changes the rest position of the displayed mesh. It changes only the difference caused by the yaw
rotation, which is `transform.r.y` in the code.

Required properties:

```text
transform.r.y == 0
    => the displayed deformation is identical for every BoneSource.rotation

transform.r.y != 0 and depth != 0
    => BoneSource.rotation changes the yaw radius and therefore how the mesh turns

any two vertices i, j of one source
    => changing BoneSource.rotation leaves the deformed relative vector, and therefore the relief shape, equal to plain yaw
```

These hold when `transform.r.x` or `transform.r.z` are non-zero as well.

In the Rust port, yaw is `transform.r.y` of the DepthBone: `pose_of` reads the `transform.r.x/y/z` channels of the
parameter binding at the current keypoint (or the node's `rot` when no parameter is given) and builds the pose with
`depth_edit_rotation(pitch, yaw, roll)`. A bone with `lockRotation` has yaw 0. The test
`bone_deform::tests::yaw_of_a_unit_grid_matches_a_y_rotation` covers that a changed source rotation changes the offsets,
and that with yaw keyed to 0 the offsets are zero.

Not verified in the Rust port: that the standard Head bone of the face and hair parameters receives only X and Y
rotation (`rig.add_standard_parameters` was not read for this).

## Coordinates And Units

The pivot shift is computed and applied in rig-root space. `target_to_root` maps target-local XY and depth into root
space before any of the formulas below.

```text
X: horizontal direction of the displayed plane
Y: vertical direction of the displayed plane
Z: depth direction at rotation = 0
```

`BoneSource.rotation = R` is an angle around the +Y axis. Let `d0` be the perpendicular Z distance from the bone origin
to the surface sample nearest to it. The shift of the common rotation center is

```text
deltaX = d0 * tan(R)          (the tangent length would be D = d0 / cos(R); the code stores only deltaX)
```

Units:

- Stored values and command values are radians.
- `bindings::normalize_rotation` (called from `read_source` and `apply_settings`) wraps the value into `[-pi, pi)` in
  `f32`; a non-finite or non-numeric value becomes 0. `bone_deform::source_records` applies the same wrap.
- The default, and the value for a source without the field, is 0.
- Degree display and editing in an Inspector: Not verified in the Rust port.

## Rest Points And The Shared Rotation Pivot

Depth is scaled and offset per source (`source_records`, `offsets_for_target`):

```text
d = raw_depth * world_scale * depthScale + depthOffset * world_scale
```

`world_scale` is `bone_deform::depth_world_scale` (the packet carries the depth already multiplied by it).

The rest point used for influence and for the deformation is not moved by `rotation`:

```text
p_i = target_to_root * (x_i, y_i, d_i)
```

`rotation_pivot_shift` computes `deltaX` once per source and target, not per vertex. It transforms every vertex with its
source depth into root space, picks the sample whose XY is nearest to the bone's `rest_head`, takes that sample's root Z
as the plane depth (if no vertex gives a finite sample, the root Z of `(0, 0, depthOffset * world_scale)` is used),
and returns

```text
deltaX = (plane_depth - rest_head.z) * tan(R)
```

`tangent_slope` floors `|cos R|` at `1e-4`. The virtual rotation point of every vertex is the same shift of its rest
point:

```text
q_i = p_i + (deltaX, 0, 0)
```

`q_i` is not a displayed position. It stands in for a source-wide rotation center. A per-vertex `D_i = d_i / cos(R)` must
not be used: it would give every vertex its own rotation center and stretch the relief between the surface and a bump as
yaw changes. `BoneSource.rotation` changes only the source-wide center, not the relief the depth map describes.

Consequently changing `rotation` alone does not move the mesh, and differences appear only when `transform.r.y` moves.

## Deformation Formula

Per DepthBone the runtime holds two skin matrices (`build_runtime_limited`):

```text
M0 = skin with the yaw removed from the pose        (BoneRuntime.no_yaw_skin, from pose_without_yaw)
M1 = the ordinary skin matrix with r.x, r.y, r.z     (BoneRuntime.skin)
```

The deformed point of one source is (`deform_vertex`, `deform_vertex_weighted`, `gpu_deform::posed_point`):

```text
base            = M0 * p_i
rotationWithYaw = M1 * q_i
rotationNoYaw   = M0 * q_i

pDeformed_i = base + rotationWithYaw - rotationNoYaw
            = M0*p_i + (M1*q_i - M0*q_i)
```

Only the bracket is the yaw difference at radius `D`. The ordinary `r.x` and `r.z` deformation stays inside `M0*p`, so
`rotation` cannot leak into other axes.

Special cases, all handled by one early branch that applies `M1` straight to `p` (`pose_yaw == 0 || deltaX == 0 ||
(sin R == 0 && cos R == 1)`):

- `transform.r.y == 0`: `M1 = M0`, so the result is `M0*p` for every `R`. The branch also avoids rounding error.
- `R == 0`: `deltaX = 0`, `q = p`, the result is `M1*p`, equal to the pre-rotation behavior.
- `deltaX == 0` for any other reason (for example the nearest sample is at the bone's Z) gives `M1*p` too.

Relief preservation: all vertices of a source share one `deltaX`, so `q_i - q_j = p_i - p_j` and
`pDeformed_i - pDeformed_j = M1 * (p_i - p_j)`. The rotation affects the source's trajectory, not distances between
vertices or the relief against the surface.

## Influence And Several Sources

Distance, falloff, terminal and lock tests use the rest point `p` (`source_rest`), never `q`. Changing which sources win
by `rotation` could change other-axis results even with `transform.r.y == 0`.

Only the sources that are kept use their own `deltaX` and `M0` for the difference. The rest point and deformed point are
then blended and projected back:

```text
rest     = sum(p * w)
deformed = sum(pDeformed * w)
offset   = (rootToTarget * deformed).xy - (rootToTarget * rest).xy
```

## Fit Z To Depth

`fit_z.rs` (`rig.fit_bone_z`, `rig.fit_root_z`) places the DepthBone's local translation Z from the mapped depth. It
keeps the angle of the tangent in the sample position (`source_local`):

```text
d       = (raw_depth * depthScale + depthOffset) * world_scale
sampleX = vertex.x + d * tan(R)
sampleY = vertex.y
sampleZ = d
```

The sample nearest in XY to the bone's world XY (after the target's world matrix) gives the world Z to fit to. The
tangent length would be `d / cos R`, but the Z component stays the perpendicular distance `d`. When the bone is a source
of a binding, that binding's `depthScale`, `depthOffset` and `rotation` are used; a fit that finds no sample that way
falls back to the default settings (scale 1, offset 0, rotation 0) for every binding. Only `GridDeformer` targets with a
`depths` array take part.

## Data Model And Commands

`sourceSettings` entries of a rig binding (`bindings::source_stored`):

```json
{ "bone": 12, "weight": 1.0, "depthOffset": 0.0, "depthScale": 1.0, "rotation": 0.5235988 }
```

`rig.set_source_settings` (MCP tool `rig_set_source_settings`, command `DepthBoneCommand_SetDepthBoneSourceSettings`)
takes `root`, `target`, `bone` and a `settings` object with any of these keys. `weight` below 0 becomes 0 and
`depthScale` below 0.01 becomes 0.01. The op commits with `dirty::mark_root`, so the deform keys of the rig are refreshed
in the same undo step. `rig.list_sources`, which `rig.set_source_settings` returns, lists `rotation` for each source.

`rig.overlay` with a `selected` bone reports `effectivePivots` (`bone_deform::effective_pivots`): for each target where
the source has `|deltaX| > 1e-4`, `bonePoint`, `effectivePoint`, `rotationPivotXShift` and `rotation`.

## Probe Packet

The probe packet in `gpu_deform::execute_probe` has 28 floats per source:

```text
 0 boneIndex
 1 isTerminal
 2 lockToRoot
 3 linearFalloff
 4 weight
 5 depthScale
 6 depthOffset (world_scale already applied)
 7 multiplier
 8 sinRotation
 9 cosRotation
10 rotationPivotXShift  (deltaX)
11 poseYaw              (transform.r.y)
12..27 M0               (no_yaw_skin, row-major)
```

`M1` is in the bone record (`skin`). The software kernel (`gpu_deform::posed_point`) and the CPU kernel use the same
branch. The packet is a transient diagnostic copy (see `depth-gpu.md`); there is no embedded shader to validate.

## Compatibility And Checks

- A source without the field reads as rotation 0 (`read_source`, `source_records`); the result equals the old one.
- Persistence of `rotation` in saved files, save/load round trip: Not verified in the Rust port.
- With `ry = 0` the offsets do not depend on `rotation`; the test cited above checks this for a rotated source.
- Undo/redo and the all-keypoints refresh run through `commit_marked` and `dirty::flush`.
- The pivot shift is computed once per source and target, not per vertex (`offsets_for_target`), so `D_i = d_i / cos R`
  does not occur.
- Fit Z uses `x + d*tan R` and `z = d`.
- Not verified in the Rust port: an automated check that `+ry` and `-ry` keep the 3D distance between a surface and a
  bump equal; the formula above implies it, but no test was read for it.

## Out Of Scope

- Three-axis BoneSource orientation.
- Moving the displayed rest mesh with `BoneSource.rotation`.
- Rotation and resampling of the PSD depth map (see `doc/psd-depth/design.md` for the import itself).
- Binding `BoneSource.rotation` to a parameter.
