# Depth bones and rig

Status: run live against the headless host (`DepthBoneCommand_CreateDepthRigRoot`, `rig_add_skeleton`, `rig_list_bones`, `rig_overlay`,
`DepthBoneCommand_AddDepthBoneSource`, `DepthBoneCommand_ListDepthBoneSources`, `rig_drag_bone`, `rig_pick_bone` error, and the `rig_*` name of every row that pairs one with a `DepthBoneCommand_*` name; `rig_apply_deform`, `rig_fit_root_z` and `rig_fit_bone_z` only as far as
their precondition errors). The rest is written from the manifest only.

A DepthRigRoot holds DepthBones. Bones deform Grid, Path and Part targets through "sources" (bone, weight, depth offset, depth scale, rotation) and an
influence rule; the result is written as a `deform` keypoint of a parameter. Targets get their depth from depth-map-draw.md.

## Sequence

1. Explore: `DepthBoneCommand_CreateDepthRigRoot {}` (answers `created[0].uuid`; the root is saved with the project and dropped on export), then
   `rig_add_skeleton {root}` or `DepthBoneCommand_AddStandardDepthSkeleton {root}` for the 19 standard bones (Pelvis, Spine, Chest, Neck, Head, Clavicle.L ...).
   `rig_list_bones {root}` gives `uuid`, `boneId`, `parent`, `restHead`, `restTail`.
2. Read: `rig_overlay {root}` (points, lines, pivots), `DepthBoneCommand_ListDepthBoneSources {root, target}`,
   `DepthBoneCommand_GetDepthBoneInfluenceRule {root, target}`, `rig_pick_bone {root, point:[x,y,z], radius}`.
3. Act: bind targets to bones (`DepthBoneCommand_AddDepthBoneSource {root, target, bone}`), choose weights, then pose bones; a pose edit refreshes the deform in the same undo step.
   `rig_fit_human {root}` places the standard bones on Parts named like bones and stores gaussian vertex weights (naming: split.md).
4. Confirm: `rig_list_bones` / `rig_overlay` for bone positions; parameter bindings and `render` for the deform (parameters-bindings.md).

## Tools: bones and sources

| Tool | Purpose and minimum arguments |
| --- | --- |
| `DepthBoneCommand_CreateDepthRigRoot` | `{}`, or `{name?, parent?}`; the first of `context:{nodes}` is the parent when `parent` is absent, else the root. |
| `rig_add_skeleton` / `DepthBoneCommand_AddStandardDepthSkeleton` | `{root}` append the standard skeleton; existing bones stay. |
| `rig_add_standard_parameters` / `DepthBoneCommand_AddStandardDepthParameters` | `{root}` standard face and body parameters with their binding rows. |
| `DepthBoneCommand_AddDepthBone` | `{parent, restHead:[x,y,z], restTail:[x,y,z], boneId?, name?, restRoll?}`; the node is named `name`, or `boneId` when no `name` is given. `context:{nodes:[parent]}` is read when `parent` is absent. |
| `rig_set_rest` / `DepthBoneCommand_SetDepthBoneRest` | `{bone, restHead, restTail, restRoll}` |
| `rig_set_constraint` / `DepthBoneCommand_SetDepthBoneConstraint` | `{bone, constraint}` locks affect the pose; hinge fields are stored only. |
| `rig_list_bones` / `DepthBoneCommand_ListDepthBones` | `{root}` |
| `rig_overlay` | `{root}` read only. |
| `rig_pick_bone` | `{root, point:[x,y,z], radius}` read only. `point` must be three numbers. |
| `rig_add_source` / `DepthBoneCommand_AddDepthBoneSource` | `{root, target, bone}` `target` is a GridDeformer, a PathDeformer or a Part. A Part is deformed at its own mesh vertices, where they are drawn (`mesh.verts` minus `mesh.origin`). As for a grid, the rig writes the target's `deform` keys for each parameter that drives a bone (hand-made keys of those parameters are replaced), and while a parameter drives the rig `binding.set_deform` and `deform.edit` refuse the target for every parameter. Keys of other parameters that exist stay, but the live preview of the rig hides them: a rig refresh and a value set on a driving parameter start that preview. A Part without mesh vertices is refused. |
| `rig_remove_source` / `DepthBoneCommand_RemoveDepthBoneSource` | `{root, target, bone}` |
| `rig_list_sources` / `DepthBoneCommand_ListDepthBoneSources` | `{root, target}` |
| `rig_set_source_settings` / `DepthBoneCommand_SetDepthBoneSourceSettings` | `{root, target, bone, settings}` weight, depth offset, depth scale, rotation. |
| `rig_set_influence_rule` / `DepthBoneCommand_SetDepthBoneInfluenceRule` | `{root, target, rule}` |
| `rig_get_influence_rule` / `DepthBoneCommand_GetDepthBoneInfluenceRule` | `{root, target}` |
| `rig_preview_influence` / `DepthBoneCommand_PreviewDepthBoneInfluence` | `{root, target, bone}`; the preview writes nothing. |
| `rig_set_vertex_weights` / `rig_clear_vertex_weights` | `{root, target, weights:[{bone, values:[...]}, ...]}` one row object per source bone of the binding, each listed once, with one value per target vertex; the values of a vertex sum to 1 / `{root, target}`. |
| `rig_fit_human` | `{root}` |

## Tools: posing and deform

| Tool | Purpose and minimum arguments |
| --- | --- |
| `rig_drag_bone` | `{root, bone, translation?:[x,y,z], rotation?}` one undo step including the deform refresh. |
| `rig_mirror` | `{root, bones, axis}` reflect closed subtrees through the rig-local YZ plane. |
| `rig_solve_ik` | `{root, endEffector, target:[x,y,z]}` CCD toward a target. |
| `rig_apply_deform` | `{root, parameter, targets?}` write offsets into the current keypoint of `parameter`, which must drive the rig ("Armed parameter does not drive this DepthRigRoot" otherwise). |
| `DepthBoneCommand_ApplyDepthBoneDeform` | `{root, parameter, targets?}` as above, or `{root, context:{armedParameters:[parameter]}, targets?}`: the first of `context.armedParameters`, else of `context.parameters`, is read when `parameter` is absent. With none it fails with "No armed parameter". |
| `rig_preview_deform` / `DepthBoneCommand_PreviewDepthBoneDeform` | `{root, parameter?, targets?}` the same offsets without history; needs no parameter. The D name also reads `context` as above. |
| `rig_fit_bone_z` / `DepthBoneCommand_FitDepthBoneZToDepth` | `{bone}` fit translation Z to the mapped depth. |
| `rig_fit_root_z` / `DepthBoneCommand_FitDepthRigRootZToDepth` | `{root}` the same for every descendant bone. |
| `rig_gpu_offsets` | `{root, parameter?}` compare the software kernel with the CPU offsets; read only. |
| `rig_apply_mmd_pose` | `{root, parameter, path, key?, frame?, boneTable?, storeBoneTable?, dryRun?}` write an MMD `.vpd` or `.vmd` pose into a keypoint. `boneTable {"<MMD bone name>": "<rig boneId>" \| null}` reads a non-standard name as that rig bone or ignores a name; `storeBoneTable: true` saves it with the document (`{}` removes it) and later calls use it. Standard skeleton only; native CMO3 refuses. Details: doc/mmd-motion.md. Last report, dry run or not: `resource://soul2d/rig/mmd-report`. |
| `rig_import_gltf` | `{root, path, unitsPerSourceUnit?, lossy?, acknowledgeRights?, dryRun?}` add the 19 standard bones below an empty DepthRigRoot at the rest positions of the humanoid in a `.gltf`, `.glb` or `.vrm` file. Answers a loss report `{source, counts, items, truncated, rights, needs, applied}`. Nothing is written while `needs` is not empty: it names `unitsPerSourceUnit` (canvas units for one metre), `lossy` (the mesh, morphs and physics are not read) or `acknowledgeRights` (the file states terms under `rights`; show them to the user, do not set the flag on your own; when `truncated` names `rights`, tell the user that the view is not all the file states). A file that prohibits modification is refused. Last applied report: `resource://soul2d/rig/import-report`. |
| `mesh_import_gltf` | `{root, path, unitsPerSourceUnit?, lossy?, acknowledgeRights?, dryRun?}` prototype: import the meshes of a `.gltf`, `.glb` or `.vrm` file as Parts seen from the front, one for each triangle primitive with a base colour PNG inside the file. z becomes the depth of each vertex, triangles that turn away are not drawn, and a skinned mesh is bound to the standard bones below `root` with its skin weights. Run `rig_import_gltf` on the same file with the same `unitsPerSourceUnit` first: it adds the bones and keeps the rights, and the mesh import is refused when the bones are somewhere else. Answers the same loss report as `rig_import_gltf`, plus `parts` and `depthScale` when applied. `applied` stays false when the file has no primitive to import. `needs` names `unitsPerSourceUnit`, `lossy` or `acknowledgeRights` (show the rights to the user, do not set the flag on your own). Last applied report: `resource://soul2d/mesh/import-report`. |
| `rig_bake_mmd_motion` | `{root, parameter, path, frames, animation?, morphs?, boneTable?, storeBoneTable?}` bake a `.vmd` into one axis of a parameter. `morphs {"<morph name>": {parameter, axis?, range?}}` adds one animation lane per mapped morph and needs `animation`. Use `dryRun` first. Last report, dry run or not: `resource://soul2d/rig/mmd-report`. |

## Gotchas

- Verified: `rig_add_skeleton` produced 19 bones; their UUIDs follow the root's, so read them back (`rig_list_bones`); they are not predictable.
- Verified: `rig_drag_bone {translation:[3,0,0]}` answered the bone tree; `translation` is three numbers.
- Verified: `DepthBoneCommand_AddDepthBoneSource` answers `{sourceBoneUuids, sources:[{uuid, weight:1.0, depthOffset, depthScale, rotation}], target}`.
- The deform lands in the parameter's current keypoint; pick the parameter and keypoint first (parameters-bindings.md) and use `dryRun` for the MMD tools.
- Verified in the row's shape on a document with one Part (the standard skeleton fails "cannot match D parent bounds" on an empty document): `DepthBoneCommand_AddStandardDepthSkeleton`, `_AddStandardDepthParameters`, `_ListDepthBones`, `_AddDepthBone` (with `parent` and with `context:{nodes:[parent]}`), `_SetDepthBoneRest` (all four keys; without `restRoll` it fails "missing required argument(s) for DepthBoneCommand_SetDepthBoneRest: restRoll"), `_AddDepthBoneSource`, `_ListDepthBoneSources`, `_SetDepthBoneSourceSettings`, `_Get`/`_SetDepthBoneInfluenceRule`, `_PreviewDepthBoneInfluence`, `_RemoveDepthBoneSource`, `_SetDepthBoneConstraint {bone, constraint:{}}`, `_PreviewDepthBoneDeform`.
  `_ApplyDepthBoneDeform` answered "No armed parameter" and the two `_Fit...ZToDepth` names "No usable mapped depth sample" because the scratch document had no armed parameter and no depths.
- Verified: `rig_apply_deform {root}` fails with "missing required argument(s) for rig_apply_deform: parameter"; with a parameter that has no binding on the rig, `rig_apply_deform {root, parameter}` and
  `DepthBoneCommand_ApplyDepthBoneDeform {root, context:{armedParameters:[parameter]}}` both answer "Armed parameter does not drive this DepthRigRoot". `rig_preview_deform {root}` on a rig without targets answers "No targets".
- Not run beyond the errors above: everything under "posing and deform" except `rig_drag_bone`, `rig_fit_human`, weights, IK, mirror, constraints.
