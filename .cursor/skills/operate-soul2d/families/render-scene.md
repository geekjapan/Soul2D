# Render and scene reads

Status: run live against the headless host (`render` to a PNG file and viewed, `bounds`, `evaluate`, `scene_geometry`, `scene_ambient_set`, `render_packet`, `ViewCommand_CaptureLiveScreenshot`, `view_screenshot`). The `context` form of `ViewCommand_CaptureLiveScreenshot` was not run.

How to see and measure the document. This is the main way to confirm a visual result; none of these tools adds history or moves `revision`.

## Sequence

1. Frame: `bounds {}` answers `{bounds:[x0,y0,x1,y1]}` of the evaluated geometry with a margin.
2. Render: `render {width, height, bounds, path?}` returns an image content item (`content[type=image].data`, base64 PNG) and, with `path`, also writes the PNG.
   Use integer sizes that match the aspect of `bounds`. To pose the model, `value_set {id, value:[x,y]}` first or pass `values` (below), then render.
3. Look at it: read the saved PNG with an image viewer or decode the base64. A render is the evidence, not the tool's `isError:false`.
4. Measure instead of looking: `scene_geometry {}` (Parts in painter order with world-space vertices), `scene_evaluate {}` (world matrices and offsets), `evaluate {values}`.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `render` | `{width, height, bounds:[x0,y0,x1,y1], path?, values?, solo?, solo_cover?}` INX or bounded native CMO3 scene to PNG. `values` is `{ "<param uuid>": [x, y] }` and overrides parameter values for this render only. `solo:[uuid, ...]` (INX only) draws just those nodes and the nodes below them, with the Composites that hold them and every mask still clipping; an unknown UUID is refused, `[]` draws nothing (with `solo_cover`, only the covered picture); without `solo_cover` a Multiply or Clip to Lower solo Part has nothing below it and draws nothing. `solo_cover:[r,g,b,a]` (0-255, needs `solo`) draws the rest of the picture, covers it with that colour (background too), and draws the solo nodes on top (each Part once; a Composite holding solo and other Parts draws in both passes with the children of each), as the editor's Solo view does. Native output reports approximation warnings. |
| `ViewCommand_CaptureLiveScreenshot` | `{width?, height?, bounds?, context?}` D-named render, 640x480 by default; `context:{parameters:[uuid], parameterValue:[x,y]}` renders that value without changing the session; the result `_meta` has `bounds` and `worldCapture`. `overlayObjects` is refused in the Rust host. |
| `view_screenshot` | `{path, size?}` writes the whole puppet as a PNG file; longest side `size` (default 1024). |
| `sketch_snapshot` | `{opacity?}` keeps the puppet as it shows now as a guide image: a Part named `Snapshot` under the root at `opacity` (default 0.25), drawn in front of every node. One undo step; the reply has `created`, `parent`, `width`, `height`. INX only. |
| `ViewCommand_SaveScreenshot` | `{filename}` same, D-named. |
| `bounds` | `{values?}` |
| `evaluate` | `{values?}` the model evaluated at the given parameter values. |
| `scene_geometry` | `{values?, deformations?}` read only. Each Part has `vertices`, `indices`, `zsort` and its evaluated `opacity`. A Part whose node, or a node above it, is locked (`node_lock_set`) carries `locked: true`. |
| `model_statistics` | `{}` read only: `{nodes, maskReferences, maxOffscreenDepth, offscreenDepthLimit}` over the tree that is rendered. `maxOffscreenDepth` is the deepest nesting of offscreen passes (`Composite` / `DynamicComposite` nodes; CMO3 Parts with offscreen drawing), 0 when there is none. A hidden node is counted, as it is for the limit (run: one `Composite` hidden with `node_visibility_set` still answers 1). Same object at `resource://soul2d/document/statistics`. Any argument is refused. |
| `scene_overlap` | `{values?, all?}` read only. ArtMeshes whose own triangles overlap in the current pose (flicker risk under advanced blending): by default only meshes with an advanced blend (not Normal, Multiply or LinearDodge; Cubism53 not NORMAL, ADD or MULTIPLY; or alpha not over). Answers `{meshes:[{id, name, blendMode, pairs, triangles}]}`; INX only. |
| `scene_evaluate` | `{values?, deformations?}` read only. |
| `render_packet` | `{values?, deformations?}` immutable render packet (`draws` with indices, vertices, textures). Refuses a native document with a Cubism 5.3 blend mode and a node whose `blend_mode` is `LinearBurn`, `LinearLight`, `Hue` or `Color` or whose `alphaBlend` is not `Over`; `scene_geometry` and `render` still read it. |
| `view_list` | `{}` the views (Cubism multiview): `{views:[{id, active}], active, synchronized}`. Each view keeps its own parameter values, and `value_set`, `render`, `snapshot.values` and the other tools use the active view's. A document starts with view 1; a new or opened one starts over. Not saved, no undo step, no revision. Also in `snapshot` and `resource://soul2d/document/state` as `views`. INX and native CMO3; CAN3 refuses. |
| `view_add` | `{}` a view at the default forms, made active. At most 16. Its id is the highest id plus one. |
| `view_select` | `{id}` makes that view active. The values of the view left are kept with it; the values of `id` come back with removed parameters dropped, and a value outside its current range falls back to the default. The view's Depth Bone pose preview comes back with it (made again after an edit). A switch ends the random pose, motion and physics previews. While synchronized, a switch changes only the active view. |
| `view_remove` | `{id}` the last view is refused; removing the active view activates the next one (the previous one for the last). |
| `view_sync_set` | `{synchronized}` while on, a view switch keeps the values, so every view shows the same ones; turning it off leaves every view with those values; `false` while off changes nothing. |
| `scene_ambient_set` | `{color:[r,g,b]}` 0..1, session-only light that `render` applies; not an undo step. |

## Gotchas

- Verified: the `render` result carries the image inline and, with `path`, a file; a 64x64 render of a 50% opacity Part showed a pale disc, and after `binding_set_trs` plus `value_set` the disc moved by the bound offset.
- Verified: `render {values:{"3":[1,0]}}` drew the bound offset while `snapshot.values` stayed `{"3":[0,0]}`; the override is for that render only.
- Verified: `bounds` of a 64x64 Part answered about `[-33.12, -33.12, 33.12, 33.12]`, a margin of about 3.5 percent per side.
- Verified: `ViewCommand_CaptureLiveScreenshot {width:64,height:64}` answered an image with `_meta.bounds`, `_meta.width` and `_meta.height`.
- `export_image` renders through a Camera node and writes a file; use that for deliverables (atlas-export.md).
