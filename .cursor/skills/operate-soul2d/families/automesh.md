# Automesh

Status: run live against the headless host (all four algorithms on a generated 64x64 disc, `cv_analyze distance`).

Generate a Part mesh from the alpha of its texture. The Part must already exist with a texture (see import-merge.md).

## Sequence

ACP preset save/delete calls need host approval for the exact store file and entry. A changed file or config path expires approval; listing and applying a preset do not write the store.

1. Explore: `nodes_list {}` for a Part UUID.
2. Read: `resource://soul2d/resources/<part>` and note `mesh.verts`, `mesh.indices` (rest mesh, flat arrays).
3. Act: `automesh_generate {id, algorithm}`. Add tuning fields only when the default is wrong (`x_segments`, `y_segments`,
   `mask_threshold`, `margin`, `target_point_count`, `min_distance`, `max_distance`, `preset`, ...; the manifest lists them all
   and clamps them to range).
4. Confirm: the reply has `{changed:true, vertices, triangles}` when a mesh was made. Read the Part again and `render` to see the mesh result. One `undo`
   restores the previous mesh. `{changed:false, message:"no mesh generated"}`, without the counts, is a success, not an error: the algorithm found
   nothing to mesh and the Part keeps its mesh. Try another algorithm.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `automesh_generate` | `{id, algorithm: grid\|contour\|optimum\|skeleton}`. Replaces `mesh.verts`, `uvs`, `indices` in one undo step. |
| `automesh_preset_save` | `{name, algorithm: contour\|optimum, preset?, option fields}`. Keeps the fields under `name` for the user of this machine. A saved name needs `overwrite:true`. No undo step. |
| `automesh_preset_delete` | `{name}`. A name that is not saved is refused. |
| `automesh_preset_list` | `{}`. Read only. `{presets: {name: {algorithm, preset?, fields}}}`, the same as `resource://soul2d/automesh/presets`. |
| `cv_analyze` | `{method: distance\|thin\|path\|contours, size:[w,h], pixels:[0\|1, ...]}`. Read only query on a binary mask you supply; use it to inspect what the contour and skeleton steps see. |

## Gotchas

- Verified on a 64x64 disc: `grid` gave 25 vertices, `contour` 5 (a diamond), `optimum` 14, `skeleton` "no mesh generated"
  with `changed:false` and `isError:false`. That is the contract (issue 249, pinned by a test): a disc thins to a point, so
  there is no skeleton to follow. Always check `changed`, not only `isError`.
- Verified: `grid` with `x_segments: 4, y_segments: 4` gave 49 vertices and with 2 and 2 gave 25; the counts are not
  `(x+1)*(y+1)`. Read the reply.
- Verified: a Part that has a `deform` binding is refused with "has a deform parameter binding and is outside Rust Core mode".
  Welds and depth data are refused too (manifest). Mesh first, then rig.
- Texture alpha drives the result; a fully opaque rectangle gives a rectangle. Hand edits afterwards: mesh-deform.md.
- `split_apply {automesh:true}` meshes new Parts in the same step (split.md).
- A saved preset is used like a built-in one: `automesh_generate {id, algorithm, preset: name}`. A field of the call
  replaces that field of the preset. The preset belongs to the algorithm it was saved for; with the other algorithm
  the name is unknown. The built-in names (`Normal parts`, `Detailed mesh`, `Large parts`, `Small parts`,
  `Thin and minimum parts`, `Preserve edges`) cannot be saved over and are not in the list.
- Saved presets are a file of the user, not part of the document. They are there after `new` and `open`, and another
  machine does not have them. The editor's Auto mesh panel reads them on each click of its preset list and then
  lists them below the built-in ones. It cannot save one: it has no option fields.
