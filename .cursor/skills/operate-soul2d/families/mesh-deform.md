# Mesh and deform

Status: run live against the headless host (`mesh_select`, `mesh_edit`, `deform_edit`, `binding_keypoint`, undo, resource read).

Edit the vertices of a Part mesh (rest shape) and write per-keypoint vertex offsets (the `deform` binding). A mesh is
`mesh.verts` (flat `[x0,y0,x1,y1,...]`), `mesh.uvs`, `mesh.indices` on the Part resource.

## Sequence

1. Explore: `nodes_list {}` for the Part UUID; `parameters {}` for the parameter UUID and `axis_points` (keys are indexes
   into them, see parameters-bindings.md).
2. Read: `resource://soul2d/resources/<part>` for the mesh; for offsets `resource://soul2d/bindings/get?parameter=<p>&target=<part>&name=deform`.
3. Select (read only): `mesh_select {id, rect:[x0,y0,x1,y1]}` returns `{selected:[vertex indexes]}`; or `lasso:[[x,y],...]`;
   add `parameter` and `key` to select over rest plus that keypoint's offsets.
4. Act on the rest mesh with `mesh_edit`; act on a keypoint with `deform_edit`. Both are one undo step.
5. Confirm: re-read the resource; for deform set the parameter with `value_set {id, value:[x,y]}` then `render`.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `mesh_select` | `{id, rect\|lasso, mode?: replace\|add\|remove\|toggle, current?}`. Read only. |
| `mesh_edit` | `{id, edits:[{op, ...}]}`. Ops: `add {pos, connect?}`, `remove`, `move {indices, delta}`, `connect`/`disconnect`/`toggle {a,b}`, `cut`, `grid_create`, `grid_axes`, `grid_add_axis`, `grid_remove_axis`, `grid_move`, `triangulate`, `flip`, `path_deform`, `reset`, `merge {distance?}`. `reset` (Cubism "Reset mesh") puts back the quad a default import gives a layer, half the texture size each way rounded down (not the half pixels of an `exact` import), origin (0, 0), stored as mesh.edit stores a grid (row order, UVs of position / size + 0.5), so it renumbers the vertices even on an untouched Part; a Part without a texture fails with "edit N: the Part has no texture to reset its mesh to". `merge` joins vertices at most `distance` apart (default 0, exactly coincident) into the lowest-numbered one, which keeps their edges; it works inside one Part, so the coincident vertices of two glued Parts stay. `triangulate` merges exactly coincident vertices first, since a triangulation cannot hold two points at one place. Deform keyforms are carried to the new vertices as for any topology edit. Each edit object needs an `op` string. |
| `mesh_copy` | `{id}` copies one Part's rest triangles into the session mesh clipboard. The document does not change. A later edit of the source does not change the clipboard. |
| `mesh_paste` | `{id, reflect?: none\|horizontal\|vertical\|both}` appends that clipboard onto the target Part in the target rest space and consumes it. A failed paste keeps the clipboard. One undo step. Reflection is across the target mesh origin. |
| `mesh_mirror` | `{node, axis, origin, vertices:[idx]}` reflect vertices (Part, Path or Grid). |
| `deform_edit` | `{id, parameter, key:[ix,iy], tool, ...}`; `tool` is `translate {delta}`, `reset`, `brush`, `path_deform`, `warp {grid:{columns, rows, points}}`, `perspective {corners}`, `bezier_translate`, `bezier_rotate`. The vertices come from `selected`, `rect` or `lasso`. `warp` and `perspective` are the temporary deform tool: a lattice (row-major from the top left) or four corners (TL, TR, BL, BR) over the selection's bounding box, moved to the given points; an identity lattice changes nothing. `space:"canvas"` with `translate {selected, delta}` moves the vertices by `delta` on the canvas as drawn now (through the deformers above), answering `{id, residual}`; put the parameter on the edited key first. `reset` with none of them returns the whole key to the rest shape (Cubism "Revert to original"). |
| `deform_continue` | Writes the next cells of a pending all-keypoints refresh. Check `snapshot.deformPending`. |
| `part_set_weld` | `{part, target, indices?:[target vertex per part vertex or -1], weight?}`. Without `indices`, each vertex welds to the nearest target vertex within 4 units; weight defaults to 0.5. |
| `part_clear_weld` | `{part, target}`. |
| `node_mask_edit` | `{id, action, source, mode?, index?}`; `action:"mode"` needs `mode`, `action:"move"` needs `index`; see nodes-inspector.md. |

## Gotchas

- Line brush: `deform_edit {id, parameter, key, tool:"brush", brush:{kind:"line", radius:100, angle:0}, stroke:[[0,0],[0,20]]}`.
  `radius` is a positive finite half-width; `angle` is finite radians, zero horizontal in node-local coordinates.
  Weight decreases linearly from 1 on the center line to 0 at either edge; distance along the line does not reduce it.
  `flow:true` recomputes weights at each step; otherwise the initial weights are retained. Read the deform binding and use undo/redo to check the result.
  The GUI exposes Line Brush, radius and angle in degrees. Brush gestures do not apply mirror settings.
  Exact Cubism falloff equivalence is unverified.
- Inflate/contract: `deform_edit {id, parameter, key, tool:"inflate", brush:{kind:"circle",radius:100}, samples:[{center:[0,0],amount:0.1}]}`.
  Each sample applies `p += (p-center)*amount*brushWeight` in order to the current positions. Positive amounts expand; negative amounts contract.
  Centers and amounts must be finite; amounts are in `[-1,1]`, with 1–4096 samples. Empty selection means all vertices.
  The GUI's Inflate / Contract mode holds the primary button to expand and Shift to contract; rate is relative change per second.
  It records 60 samples per second, flushes the final fraction on release, and commits one undo step. Focus loss, Escape, changed target/shape/frame,
  or a timing interruption above 0.25 seconds cancels the hold. At the sample limit it stops extending the preview and asks you to release to apply it.
  Brush shape, selection and options are fixed for the hold. No brush mirroring or native CMO3/CAN3 acceptance is claimed.
  `deform_edit` accepts `expectedRevision` to refuse a stale edit. GUI holds always use their starting revision and cancel if it changes.

- Verified: `mesh_edit {op:"add", pos}` without `connect` stores the point and answers `vertices: N+1`. The point is in no
  triangle, so the triangle count does not change and it draws nothing until edges join it. Pass
  `connect: <vertex index>` (an integer) or add `connect {a,b}` edits to build triangles. Read the mesh again before
  using vertex indexes: when the points form a grid the vertices are sorted and the triangles renumbered (`grid: true`).
- Verified: `deform_edit` rejects a parameter UUID that does not exist with "unknown leaf parameter".
- Verified: after `deform_edit translate delta [5,0]` the `deform` binding holds `[5,0]` at the written key; `binding_keypoint
  {action:"mirror_horizontal"}` on that key turned it into `[-5,0]`.
- A Part with deform bindings, welds or depth data cannot be re-meshed by `automesh_generate`; undo or `binding_remove` first
  (see automesh.md).
- `mesh_edit` that changes vertex count is rejected for Parts with welds, depth data or skinning weights.
