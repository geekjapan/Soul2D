# Depth Bone Rig

Status: implemented in `rust/src`. The model has the `DepthRigRoot` and `DepthBone` node types, a binding table on the
rig root, CPU deform offsets written into parameter `"deform"` keys (Gaussian or linear falloff, or persisted
`vertexWeights`), a refresh that lands in the same undo step as the edit, and the `rig_*` MCP tools (skeleton,
standard parameters, Fit Z, IK, mirror, MMD pose, glTF rig import). The egui editor shows the rig inspector sections
and a posed bone overlay. Not implemented: an inspector UI for Depth Bone Sources and the Influence Rule, viewport
bone handles or dragging, an influence preview overlay, and a Layout/Deform sub-mode split. `rig_preview_influence`
returns data only. Hinge constraint fields are stored, and only `rig_solve_ik` reads them.

This note describes how a Depth Bone rig combines the depths of a GridDeformer, PathDeformer or Part with bone poses
to generate deform offsets for several nodes. It was first written as a plan for the D editor. The text below is
rewritten against the Rust port.

## Design

Bones are nodes in a hierarchy. Bindings are not part of that hierarchy. A binding is a flat table on the rig root
that maps a target node to the bones it follows.

```text
pose, editing, hierarchy = DepthBone node tree
target <-> bone mapping  = bindings[] table on the DepthRigRoot
weights                  = computed from the binding rules (or stored as optional vertexWeights)
```

Why:

- A bone changes angle in 3D and is keyed per parameter keypoint. Making it a node reuses the node transform,
  selection, undo/redo and parameter bindings.
- One target (for example a body grid) takes weight from several bones (`Pelvis`, `Spine`, `Chest`), so the mapping
  cannot be expressed by the bone parent chain.
- Weights are not stored per vertex by default. The binding keeps a few rule parameters and each vertex weight is
  computed when offsets are generated, so a mesh or depth change does not leave stale weights.

Verified in `rust/src`: `bindings.rs` keeps the table on the root (`bindings`), and the targets do not own a source
list. `Session::op_node_create` rejects `bindings`, `displayOptions` and `templateOptions` in a `node.create`
request, and no other use of `displayOptions` or `templateOptions` exists in `rust/src`. The root therefore stores
only the child bones and `bindings`.

Per-vertex weights are optional and explicit. `rig_set_vertex_weights` stores `vertexWeights` on one binding, and
`rig_fit_human` stores Gaussian rows for every bound target. While `vertexWeights` exist, `deform_vertex_weighted`
uses them and source-list edits are refused (`bindings::reject_weight_source_edit`, message "vertexWeights must be
cleared before the source list changes"). `bindings::reject_weight_arrays` refuses array values under the keys
`weights`, `vertexWeights` and `perVertexWeights` in the source-settings, rule, preview and deform requests.
`rig_clear_vertex_weights` returns the binding to the computed score.

## Nodes

`DepthRigRoot` and `DepthBone` have no render output. They are editing aids. Save keeps them. Export drops them
(`depths::model_for_export` calls `nodes::model_without_rig_nodes`, which lifts remaining descendants with their world
transform and prunes parameter bindings that pointed at removed nodes).

`DepthBone` fields in the model (`nodes::stamp_created_rig`, `bone_edit.rs`): `boneId`, `restHead`, `restTail`,
`restRoll`, `hingeAxis` (default `[0,0,1]`), `lockRotation`, `lockTranslation`, `allowParentToTargets` (default true),
plus the common `lockToRoot` flag. `constraintType`, `rotationLimits` and `maxStepRadians` are stored by
`bone_edit::set_constraint` when given.

A bone created through `node.create` with `className` `DepthBone` (`DepthBoneCommand_AddDepthBone` maps to it) also
gets `transform.trans = restHead - parent restHead`; the parent must be a `DepthRigRoot` or `DepthBone`.

### Rest geometry used at run time

`bone_deform::effective_rest` computes the rest head and tail that the deform uses. It does not read `restHead`
directly:

- Head is the sum of `transform.trans` up the bone chain (`rest_position`), stopping at a `lockToRoot` bone.
- The tail direction points to the first child bone's rest position. Without a child it points away from a bone parent.
  Otherwise it uses `restTail - restHead` from the stored fields. The direction is planar (Z dropped). If all of those
  are shorter than `1e-4`, it is `[0, 100, 0]` (`planar_direction`).
- The bone length is the head-tail distance with a floor of `1e-4`. `rest_quat` aligns the bone axis with the
  direction and applies `restRoll`.

`bone_edit::set_rest` writes only `restHead`, `restTail` and `restRoll`. It does not write `transform.trans`.
`rig_fit_human` and the creation path do write `trans` from `restHead`.

### Standard skeleton

`skeleton::add_standard_skeleton` (`rig_add_skeleton`, `DepthBoneCommand_AddStandardDepthSkeleton`) appends 19 bones
below the root. They are the `STANDARD_BONES` rows: `Pelvis`, `Spine`, `Chest`, `Neck`, `Head`, `Clavicle.L/R`,
`UpperArm.L/R`, `Forearm.L/R`, `Hand.L/R`, `Thigh.L/R`, `Shin.L/R`, `Foot.L/R`. Head and tail are ratios of the
parent node's combined bounds (fallback bounds 360 x 600 when the bounds are missing or under `1e-4`), multiplied by
`scale` (zero becomes one). `Head.allowParentToTargets` is false and `Foot.L`/`Foot.R` have `lockToRoot` true.
Existing bones stay. The result is an ordinary bone tree, with no template-specific data. `skeleton::fit_human`
(`rig_fit_human`) later moves the named bones to matching Part and Grid bounds and stores Gaussian `vertexWeights`.

### Standard parameters

`standard_params::add` (`rig_add_standard_parameters`) requires bones named `Head`, `Neck`, `Spine`, `Chest`,
`Clavicle.L`, `Pelvis`, `Shin.L` and `Shin.R`. It creates `Face::Yaw-Pitch`, `Face::Roll`, `Body::Yaw-Pitch`
and `Body::Roll` when they are missing, with range -1 to 1 and the axis `[0, 0.25, 0.5, 0.75, 1.0]` (the roll
parameters are 1D). It adds only the missing
binding rows from the 19-row `BINDINGS` table (for example `Head` `transform.r.y` and `transform.r.x` on
`Face::Yaw-Pitch`, `Neck` `transform.r.z` on `Face::Roll`, and `Pelvis`, `Spine`, `Chest`, `Clavicle.L`, `Shin.L/R`
rows on the body parameters). An existing parameter or binding whose axes or values disagree makes the command fail.

## Bindings

A binding row on the root holds `target`, `targetKind` (`grid`, `path` or `part`), `sourceBoneUuids`,
`sourceSettings` and `influenceRule`. Allowed targets are GridDeformer, PathDeformer and Part
(`bindings::require_target`, `bone_deform::is_binding_target`).

- `sourceSettings[]` per bone: `weight` (clamped to 0 or more, default 1), `depthOffset` (default 0), `depthScale`
  (floor 0.01, default 1) and `rotation` (normalized on write) (`bindings::clamp_source`).
- `influenceRule`: `maxInfluences` (default 4, 0 becomes 1), `radiusScale` (1.0), `minimumRadius` (1.0), `falloff`
  (`"gaussian"` default, `"linear"` is recognised by the deform), `multipliersByBoneUuid`.
- Removing the last source removes the row. A source bone must belong to the same rig root.
- Delete, convert, copy and paste rewrite the table in the same step (`drop_deleted_references`,
  `retarget_converted_node`, `remap_copied_bindings`). See `depthbone-conversion-recovery.md`.

A Part target is moved at its own mesh vertices, `mesh.verts - origin` (`bone_deform::target_vertices`).

## Depth source

`bone_deform::snapshot_depths` reads the target's `depths` array for any target type. A missing array or a non-finite
value is 0. `depths` must match the vertex count when set (`depths::store_depths`). Depth operations (rings, attached
points, planes) and the depth editor are GridDeformer-only (`depth_ops.rs`, `editor_depth.rs`). They bake into
`depths`. Their results enter the rig only through that array. Not verified in the Rust port: any rule that derives a
PathDeformer depth from nearby grids.

Depths are multiplied by `depth_world_scale` (the largest bound size of all bound targets in rig space times
`0.42 / 2.9`, at least 1.0). For each source the depth of vertex `i` is
`z_i * world_scale * depthScale + depthOffset * world_scale`.

## Rig runtime

`bone_deform::build_runtime` builds one `BoneRuntime` per `DepthBone` below the root (depth-first order from
`bone_order`):

1. Rest head, tail, length and `rest_quat` from `effective_rest`. Local rest offset and quaternion against the parent.
2. Pose from `pose_of`. With a parameter and a keypoint, the pose reads the bone's `transform.t.x/y/z` and
   `transform.r.x/y/z` bindings at that keypoint (`binding_scalar`, 0 when there is none): `r.x` is pitch, `r.y` is
   yaw, `r.z` is roll. Without a parameter, the node's own `trans` and `rot` give the pose.
   `lockRotation` and `lockTranslation` zero the corresponding part.
3. World head and quaternion resolve parent first. A bone follows its parent unless `allowParentToTargets` is false or
   `lockToRoot` is set.
4. `skin = compose(world_head, world_quat) * inverse(compose(rest_head, rest_quat))`.

At most 64 bones (`MAX_BONES`), 128 sources per binding (`MAX_SOURCES`) and 16 influences (`MAX_INFLUENCES`) are
accepted for a deform. A target with more than `MAX_VERTICES` (1,000,000) vertices fails. The overlay builds the same
runtime without the bone cap.

## Weights

For a target with more than one source, each vertex scores every source bone (`deform_vertex`):

```text
rest_point = target_to_root * (x, y, source_depth)
d2         = squared distance from rest_point to the bone's rest segment
radius     = max(restLength * 0.85 * radiusScale, radiusFloor)
radiusFloor = max(minimumRadius, 0.18 * target bounds size)
score      = weight * multiplier * exp(-d2 / radius^2)        (gaussian)
score      = weight * multiplier * max(0, 1 - d / radius)     (linear)
```

The top `maxInfluences` scores (at most 16) are kept and normalized. A terminal source bone with `lockToRoot` can take
the whole vertex when the point lies beyond the parent segment and inside the radius (`locked` branch). A target with
one source skips the distance term: the score is `weight * multiplier`. The weights are not stored. Persisted
`vertexWeights` replace this step (see Design).

## Offsets

Each vertex is skinned in rig space: `deformed = sum(w_k * skin_k * rest_k)`. A source with a yaw pose and a rotation
setting gets a pivot shift (`rotation_pivot_shift`, `no_yaw_skin`) so yaw rotates around the source plane. The result
is converted back with `root_to_target` and the offset is the XY difference of the deformed and rest points in the
target space. Z is not stored and there is no perspective projection. The offset array is
one `[x, y]` per vertex and is written to the parameter's `"deform"` binding at the key
(`bone_deform::write_deform_key`), so rig output uses the normal deform binding format.

A bound ancestor grid's inherited offset is subtracted, so a child target does not move twice
(`compensate_ancestor_grids`, `ancestor_influence`, `must_propagate`). Preview (`rig_preview_deform`) keeps the offsets
in the session (`preview_deformation`) and writes nothing. `rig_apply_deform` writes the key selected by the
`parameter` argument and the closest keypoint of its current value (`closest_keypoint`); without a parameter it fails
with "No armed parameter". CPU offsets are the stored result. `gpu_deform.rs` is a diagnostic comparison within `1e-4`
(`rig_gpu_offsets`).

## Refresh

Edits that change the inputs of the offsets mark a refresh that runs once, before the edit is committed. See
`plan.md` for the dirty queue.

## Tools

The editor and MCP use the same session operations. Names are the `rig_*` tool, the
`DepthBoneCommand_*` id where one exists in `doc/commands.manifest.json`, and the core function.

| Tool | Command id | Core |
| --- | --- | --- |
| `rig_add_skeleton` | `AddStandardDepthSkeleton` | `skeleton::add_standard_skeleton` |
| `rig_add_standard_parameters` | `AddStandardDepthParameters` | `standard_params::add` |
| `rig_set_rest` | `SetDepthBoneRest` | `bone_edit::set_rest` |
| `rig_set_constraint` | `SetDepthBoneConstraint` | `bone_edit::set_constraint` |
| `rig_list_bones` | `ListDepthBones` | `skeleton::list_bones` |
| `rig_add_source`, `rig_remove_source` | `AddDepthBoneSource`, `RemoveDepthBoneSource` | `bindings::add_source`, `remove_source` |
| `rig_list_sources` | `ListDepthBoneSources` | `bindings::list_sources` |
| `rig_set_source_settings` | `SetDepthBoneSourceSettings` | `bindings::set_source_settings` |
| `rig_set_influence_rule`, `rig_get_influence_rule` | `SetDepthBoneInfluenceRule`, `GetDepthBoneInfluenceRule` | `bindings::set_rule`, `get_rule` |
| `rig_preview_influence` | `PreviewDepthBoneInfluence` | `bindings::preview_influence` (rule and source ids only) |
| `rig_preview_deform`, `rig_apply_deform` | `PreviewDepthBoneDeform`, `ApplyDepthBoneDeform` | `bone_deform::run` |
| `rig_fit_root_z`, `rig_fit_bone_z` | `FitDepthRigRootZToDepth`, `FitDepthBoneZToDepth` | `fit_z::fit_root`, `fit_bone` |
| `rig_set_vertex_weights`, `rig_clear_vertex_weights` | none | `bindings::set_vertex_weights`, `clear_vertex_weights` |
| `rig_fit_human` | none | `skeleton::fit_human` |
| `rig_drag_bone` | none | `bone_deform::apply_drag_pose` |
| `rig_solve_ik` | none | `bone_deform::solve_ik` |
| `rig_mirror` | none | `bone_deform::mirror_bones` |
| `rig_overlay`, `rig_pick_bone` | none | `bone_deform::overlay_geometry`, `pick_bone` |
| `rig_gpu_offsets` | none | `bone_deform::gpu_offsets` |
| `rig_import_gltf` | none | `gltf_rig.rs` |
| `rig_apply_mmd_pose`, `rig_bake_mmd_motion` | none | `mmd_motion/` |
| `deform_continue` | none | `Session::op_deform_continue`, `dirty::continue_cells` |

`DepthBoneCommand_AddDepthBone` is translated to `node.create` with `className` `DepthBone` (`mcp.rs`).
`DepthBoneCommand_CreateDepthRigRoot` creates a `DepthRigRoot` node. Of the tools above, only `rig_set_vertex_weights`
takes a per-vertex weight array.

Fit Z (`fit_z.rs`) moves a bone's local `trans.z` so its world Z matches the mean mapped depth sampled from the nearest
GridDeformer vertex of each binding that has depths, using the same source settings. A root fit goes parent before
child in one snapshot. It does not generate deform offsets.

`rig_solve_ik` runs up to 16 CCD passes over the chain from the end effector up to the rig root or a `lockToRoot`
bone, skips `lockRotation` bones, and honours `hingeAxis`, `rotationLimits` and `maxStepRadians`. `rig_mirror` reflects
closed bone subtrees through the rig-local YZ plane (`axis` must be `x`) and mirrors persisted vertex weight rows.

## Editor

`editor_inspector.rs`:

- `depth_root_section` ("Depth Rig"): bone and binding counts, "Fit Z to Depth", "Add Standard Skeleton",
  "Add Standard Parameters".
- `depth_bone_section` ("Depth Bone"): `boneId`, "Fit Z to Depth", Rest Head, Rest Tail, Rest Roll, Lock Rotation, Lock
  Translation and Allow Parent to Targets. A number control commits when the drag stops or focus leaves
  (`draw_numbers`), so one drag is one `rig.set_rest` request.
- `grid_deformer_section` and `path_deformer_section` have no Depth Bone Sources or Influence Rule controls.

`editor_viewport.rs`: `rig_of` finds the rig of the selection and `sync_rig` reads `rig.overlay`; `paint_bones` draws
posed joints and parent/child links. No bone handle, influence preview or pointer pick/drag exists in the editor. The
`rig_pick_bone` and `rig_drag_bone` operations exist only on the session.

## Not verified in the Rust port

- Not verified in the Rust port: a PathDeformer depth fallback derived from nearby grids.
- Not verified in the Rust port: editor-side Deform mode behaviour for bone gizmos. Only the session operations were
  read.
