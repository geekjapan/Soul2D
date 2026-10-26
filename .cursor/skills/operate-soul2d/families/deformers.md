# Deformers

Status: run live against the headless host (create GridDeformer, PathDeformer; define grid axes and path vertices; `deformer_set`;
`Inspector_Apply_PathDeformCurveType`, and every D-named row below in the shown shape).

GridDeformer (lattice), PathDeformer (curve) and MeshGroup (mesh with children). Create them with the node tools
(nodes-inspector.md: `node_create`, `node_insert`, `Node_Add_*`, `Node_Insert_*`, `Node_ConvertTo_*`), then set their shape and
behaviour here. Depth on a grid is in depth-map-draw.md; bone driven deformation in depth-bones-rig.md.

## Sequence

1. Explore: `nodes_list {}`; pick the deformer UUID (UUIDs are not predictable, read them back after `node_create`).
2. Read: `resource://soul2d/resources/<uuid>`: Grid has `grid_axis_x`, `grid_axis_y`, `formation`, `dynamic`; Path has `vertices`,
   `curve_type`, `dynamic_deformation`.
3. Act: shape with `deformer_define_grid` / `deformer_define_vertices`; behaviour with `deformer_set`.
4. Confirm: read the resource back; `render` for the visual result.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `deformer_define_grid` | `{nodes:[uuid], axisX:[...], axisY:[...]}` set Grid axes. Verified with `axisX:[-20,0,20]`, `axisY:[-20,0,20]`. |
| `VertexCommand_DefineGrid` | `{axisX, axisY, context:{nodes}}`; a bare `nodes` fails with "missing required argument(s) for <tool>: context". |
| `deformer_define_vertices` | `{nodes, vertices:[x0,y0,x1,y1,...]}` control points of a Path or Grid. |
| `VertexCommand_DefineVertices` | `{vertices, context:{nodes}}`; a bare `nodes` fails with "missing required argument(s) for <tool>: context". |
| `deformer_define_mesh` | `{nodes, vertices:[x0,y0,...], indices:[...]}` MeshGroup mesh only. Verified with one triangle (`vertices:[0,0,10,0,0,10]`, `indices:[0,1,2]`). |
| `VertexCommand_DefineMesh` | `{vertices, indices, context:{nodes}}` MeshGroup mesh only; a bare `nodes` fails with "missing required argument(s) for <tool>: context". |
| `deformer_set` | `{nodes, property, value}`. `property` is a name from the `Inspector_Apply_*` list below, not the resource field name: `PathDeformCurveType` works, `curve_type` fails with "no nodes accept deformer property". |
| `deformer_validate` | `{values?}` read only. `overhanging`: `{deformer, child, vertices}` for each GridDeformer child whose rest mesh, or rest mesh plus a keyed deform form, leaves the grid (those vertices clamp to the grid edge). `empty`: Grid, Path and MeshGroup nodes without children. Also `resource://soul2d/deformers/validation`. |
| `deformer_extend_grid` | `{nodes}`: adds a grid line outside each side a child passes (from `deformer_validate`); added lines copy the old edge's keyed offsets. Result `extended: [{node, axisX, axisY}]`, empty when no child leaves. One undo step. |
| `deformer_create_rotation` | `{pivot:[x,y], angle?, handleLength?, parent?, name?, children?}`: Cubism rotation deformer as a Node with `rotationDeformer {handleLength}`; angle is `transform.r.z` (radians); children keep their world transforms and turn about the pivot. Result `{node, children}`. Chain them by passing the previous `node` as `parent`. |
| `deformer_bezier_controls` / `deformer_bezier_warp` | `{nodes:[grid], divisions:[cols,rows], parameter?, key?}` reads the (3·cols+1)×(3·rows+1) control lattice at a key; `..._warp` also takes `controls` and writes the patch surface to `parameter`'s `key` as offsets (one undo step). Handles are not stored. |
| `deformer_generate_face_turn` | `{nodes:[grid], parameter, axis:"x"\|"y", amount?, flip?, replaceKeys?}`: three-key face turn on a 1D parameter (ParamAngleX with `x`, ParamAngleY with `y`; middle key = rest). Inferred rule: the outline stays, the inside shifts `amount·half·(1-along²)·√(1-across²)` (amount default 0.3, below 0.5); high key turns right or up. Apply to the face grid. |
| `deformer_generate_sway` | `{nodes:[grid], parameter, fulcrum?, horizontal?, vertical?, softness?, zoom?, flip?, replaceKeys?}`: three-key sway on a 1D parameter (middle key = rest). Inferred rule: weight `d^(1+softness)` from the fulcrum edge; low/high keys swing `∓horizontal` (default 15 % of the length). Needs `replaceKeys` unless the parameter has exactly 3 keys. |
| `deformer_generate_humanoid` | `{roles:{"Body":[part], "Face":[...], "Eye L":[...], ...}, parent?, margin?, divisions?}`: builds the fixed humanoid GridDeformer tree (`Warp <role>`), each grid sized to the meshes under it; roles with nothing under them come back in `notGenerated`. Run once; a second run fails on the names. |
| `deformer_apply_to_children` | `{nodes:[grid]}`: adds the grid's keyed offsets to its children's deform bindings, moves the children to its place (world transforms kept) and deletes it; every pose looks the same. Refused for dynamic or rig-driven grids, non-deform bindings, children that pass the grid on, and differing interpolation. Result `{deleted, children}`. One undo step. |
| `Inspector_Apply_GridDeformDynamic` / `Inspector_Apply_MeshGroupDynamic` / `Inspector_Apply_MeshGroupTranslateChildren` | `{value:bool, context:{nodes}}`; same effect as `deformer_set` with the name after `Inspector_Apply_`. |
| `Inspector_Apply_PathDeformCurveType` / `Inspector_Apply_PathDeformDamping` / `Inspector_Apply_PathDeformDynamic` / `Inspector_Apply_PathDeformGravity` / `Inspector_Apply_PathDeformInputScale` / `Inspector_Apply_PathDeformPhysicsEnabled` / `Inspector_Apply_PathDeformPropagateScale` / `Inspector_Apply_PathDeformRestoreConstant` | `{value, context:{nodes}}` Path properties. `value` is the string `Spline` or `Bezier` for `CurveType`, a bool for `Dynamic` and `PhysicsEnabled`, a number for the rest (`PropagateScale` too: a bool or an array fails "must contain finite numbers"). `Gravity`, `Damping`, `InputScale` and `RestoreConstant` need `PathDeformPhysicsEnabled` true first ("physics driver is not a pendulum"). |
| `mesh_mirror` | `{node, axis, origin, vertices:[idx]}` reflect Path or Grid vertices; see mesh-deform.md. |

## Gotchas

- Verified: `deformer_set {property:"PathDeformCurveType", value:"Bezier"}` changed the resource's `curve_type`.
- Verified: `VertexCommand_DefineGrid`, `_DefineVertices` and `_DefineMesh` with `context:{nodes}` answered `{nodes}`; the `Inspector_Apply_*` rows above were accepted in their shown shape for `GridDeformDynamic`, `MeshGroupDynamic`, `MeshGroupTranslateChildren` and all eight `PathDeform*` names (the values listed in the row). They accept several context nodes (`PathDeformDynamic` on two Paths set both). The same dispatcher merges an unconsumed argument such as `property` over the alias's own value (nodes-inspector.md, Gotchas), so pass only the row's arguments.
- Verified: `deformer_set`, `deformer_define_grid`, `deformer_define_vertices` and `deformer_define_mesh` read `id` (one UUID) first, then `ids`, then `nodes`, so the manifest requires none of them (run: `deformer_set {id, property, value}`);
  with no target at all they fail with "node UUIDs are required" and change nothing.
- Verified: a new GridDeformer starts with `grid_axis_x:[-0.5,0.5]`; set the axes in document units before using depth or deform.
- Not run: vertex edits on a deformer that already has deform bindings, depths or welds are refused for Parts (see mesh-deform.md and
  automesh.md) and probably here too; define the shape before binding.
