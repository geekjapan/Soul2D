---
name: operate-soul2d
description: Operate a soul2d document through its MCP tools (nodes, meshes, deformers, parameters and bindings, animation, depth, rig, physics, import, export, Live2D) from an agent such as Claude Code, Codex or Cursor. Use when asked to build, edit, inspect or render a soul2d/INX/INP puppet over MCP, or when you need to know which tool does a job and how to confirm it. Not for verifying changes to the soul2d code itself (use verify-soul2d).
---

# Operate soul2d

soul2d exposes every operation as an MCP tool and every readable thing as an MCP resource. One session owns one open document. This skill gives the loop, the
revision rules and one map per tool family. The tool list itself is [doc/commands.manifest.json](../../../doc/commands.manifest.json) (name,
description, input schema, effect); [doc/mcp_api.md](../../../doc/mcp_api.md) is the protocol note. Do not copy descriptions from them; read the manifest entry when
the map says "see manifest".

## Reach a host

Headless host (disposable, one session, loopback only):

```bash
./tools/build-rust-core.sh --locked
./out/rust/debug/soul2d-core --mcp-port 18088 [--project <path>]   # --mcp-port 0 picks a free port; --mcp is stdio
./out/rust/debug/njc --port 18088 tools list
./out/rust/debug/njc --port 18088 tools call NAME --json '{"k":"v"}'
./out/rust/debug/njc --port 18088 resources read 'resource://soul2d/document/state'
```

The editor (`soul2d-editor`) runs the same session behind its own listener: Settings, "MCP server (this editor)": enable it, set host (default `127.0.0.1`) and port (default 8088),
optionally require a bearer token, press "Apply MCP settings"; the panel then shows `Serving on <url>`. Point `njc --port <port>` or any MCP HTTP client at `POST /mcp`.
A bearer token is `SOUL2D_MCP_BEARER` or `njc --bearer`. Kill only the host you started.

The editor's agent panel speaks ACP; `python3 -I tools/acp-claude.py` as `ACP.Command` runs Claude Code headless with the panel's MCP server as its only tool source. `python3 -I tools/verify-acp-agent.py --core <soul2d-core> --njc <njc> [--editor <soul2d-editor>] [--families history,textures,...]` checks one workflow per family three ways (the setup alone as a negative control, a scripted MCP client, the agent): 22 families against a headless host, and with `--editor` the editor-state family, scripted over the editor's stdio MCP (a headless host refuses the editor tools) and run by the agent in the editor's agent panel, which needs an unlocked screen.

Every call returns JSON-RPC `result`. A failed tool call is `result.isError: true` with the message in `content[0].text`; `njc` still exits 0, so always read `isError`.
Successful JSON results are in `content[0].text` (an object result is also `structuredContent`; a list such as `nodes_list` or `parameters` is only in the text, since MCP allows only an object there); images (`render`, `split_overlay`) are `content[type=image].data`, base64 PNG.
A failed document edit changes nothing and does not move `revision`. A file-writing tool can leave side effects when it fails: `export_frames` removes the frames it created but does not
restore the ones it overwrote, and a failed or cancelled video export can leave a partial file. Look at the destination after a failure (doc/mcp_api.md, Export Tools).

## The loop: Explore, Read, Act, Confirm

1. **Explore**: `nodes_list {}`, `parameters {}`, or `resources/list` and `resource://soul2d/resources/find?selector=*` (selector syntax: `resource://soul2d/guides/selectors`).
2. **Read**: `resource://soul2d/resources/<uuid>` for a node, `resource://soul2d/bindings/get?parameter=<id>&target=<node>&name=<binding>` for one binding,
   `resource://soul2d/document/state` or `snapshot {}` for the whole state. Resources never change anything; tools are the only writers.
3. **Act**: one `tools/call` per edit. Edits are one undo step each. Use UUIDs read in steps 1 and 2; they are not predictable (after `node_create`, paste, import, rig creation, read them back).
4. **Confirm** from state, not from the reply: read the resource again, or `render {width, height, bounds, path}` and look at the PNG (render-scene.md). For files, read the file back.

The `Explore`, `Read`, `Act` split is the contract in doc/mcp_api.md. Two spellings of many tools exist: the snake_case name (`node_rename`) and a D-era name (`NodeCommand_SetNodeName`,
`Inspector_Apply_*`, `ModelCommand_*`). Both reach the same operation, but the argument shape is not the same and a few behave differently (`NodeCommand_ToggleVisibility` toggles only the first context node).
Take the shape from the row of the name you call, never by analogy with its twin (for example `translation` has 2 numbers in `binding_set_trs` and 3 in `node_transform`). A row that lists two names with `/`
either has one shape for both or shows both. Three rules cover whole classes of D names:

- A D name that edits nodes and shows `context:{nodes}` (`NodeCommand_ConvertTo`, `_CopyNode`, `_CutNode`, `_MoveNode`, `_DeleteNode`, `Node_Insert_*`, `Node_ConvertTo_*`, `Inspector_Apply_*`, `VertexCommand_*`)
  reads its targets from `context.nodes` only, lists `context` as required, and is refused with "missing required argument(s) for <tool>: context" when given a bare `nodes` or `id`. The snake_case node tools read explicit arguments only (`node_paste {context:{nodes}}` fails with "paste parent is required").
- A D name that fixes a class or a property in its name (`Node_Add_*`, `Node_Insert_*`, `Node_ConvertTo_*`, `Inspector_Apply_*`) must be called with the row's arguments and nothing else:
  the host refuses an argument that would replace a value the alias sets (a stray `className`, `property`, `component`, `nodes` or `id`) with "<tool> sets <key> itself: do not pass the argument <name>" (shown in nodes-inspector.md, Gotchas).
- The host refuses a call that leaves out an argument the manifest lists in `required`, before anything runs, with "missing required argument(s) for <tool>: <names>". An argument the host can default is
  not listed, and neither is one it also reads under another name: the snake_case node tools take `id`, then `ids`, in place of `nodes`, `node_move` and `node_paste` take `newParent` for `parent`, the depth tools take `id` for `target`
  (such a call with no target at all fails in the operation: "node UUIDs are required", "target must be a node UUID"). The tools that create a node are the exception: `className` (`node_create`, `node_insert`, `NodeCommand_AddNode`) and
  `_suffix` (`NodeCommand_AddNode`, `Node_Add_*`, `Node_Insert_*`) are required although the operation could default them; `_suffix: ""` names the node `Unnamed <Class>`.

## Revision check

`revision` is an unsigned counter owned by the session. Read it from `snapshot.revision`, `history_summary.revision` or `resource://soul2d/document/state` `revision`. Most edit tools do not return
it (those that answer a full snapshot do: `undo`, `redo`, `new`, `open`, `animation_create`, `animation_set`, `animation_key_set`, `animation_seek`, `physics_step`, `motion3_seek`).

- It starts at 0 in a fresh host and only goes up inside one host process. It rises by one for each committed edit, `undo`, `redo`, `history_goto`, `history_clear`, `history_limit_set` that drops states, `new`, `open`
  and `import_*`. It does not move for a failed call, an edit that leaves the document unchanged, reads, renders, exports, or previews (`value_set`, `animation_seek`, `motion3_seek`, `physics_step`, `live2d_parameter_set`).
- A tool whose live input schema requires `expectedRevision` is revision-guarded. Supply the current document `revision`.
  This includes `history_goto`, CAN3 key, handle, curve-type, parameter-curve, track and scene edits, CMO3 form, name and user-data edits, and `live2d_physics3_edit`.
  A missing or stale revision fails without changing the document. Read `revision` immediately before the call and re-read before retrying.
- Every other tool is unconditional: the last write wins and a stale UUID or index fails only if it no longer exists. If something else (the editor, another client) may be editing, read
  `revision` before and after your sequence; a jump larger than your own edits means the state moved, so re-read before the next Act.
- DepthDraw and the split draft have their own counters (`snapshot.depthDraw.revision`, `snapshot.partSplit.revision`; `split_open` answers the draft's). They are not the document `revision`.

## Undo and redo

`undo {}` and `redo {}` (D twins `EditCommand_Undo`, `EditCommand_Redo`) step the shared history; `history_summary {}` shows `position`, `count`, `limit` and every state's label; `history_goto` jumps with a
revision check. Any new edit drops the redo states. `new`, `open` and `import_*` reset history to one state. Preview tools add no history. After an undo, re-read: UUIDs created by the undone edit are gone.
Details and verified behaviour: [families/history.md](families/history.md).

## Family maps

Each map has the Explore/Read/Act/Confirm sequence for its family, a table of tools with minimum arguments taken from the manifest, and gotchas. "Run" means the sequence was executed against
`soul2d-core --mcp-port` on a scratch document under `build/verification/operate-skill/` (generated PNGs and hand-written files only), or by `tools/verify-acp-agent.py` on synthetic fixtures in a temporary directory, and the map corrected to match; "manifest only" means it was written
from the manifest and source descriptions and not executed. A "Not run" line inside a map lists the tools of a run map that were not exercised.

| Map | Covers | Tool rows | Status |
| --- | --- | --- | --- |
| [nodes-inspector](families/nodes-inspector.md) | `node_*`, `NodeCommand_*`, `Node_*`, `Inspector_Apply_*` for nodes | 82 | run |
| [mesh-deform](families/mesh-deform.md) | mesh edit and select, keypoint deform edit, welds | 10 | run |
| [automesh](families/automesh.md) | `automesh_generate`, `automesh_preset_*`, `cv_analyze` | 5 | run |
| [deformers](families/deformers.md) | Grid, Path, MeshGroup shape and properties | 28 | run |
| [parameters-bindings](families/parameters-bindings.md) | parameters, groups, links, bindings, keypoints, `value_set`, default form lock | 48 | run |
| [animation](families/animation.md) | INX animations and keys | 74 | run |
| [depth-map-draw](families/depth-map-draw.md) | depth values, depth operations, DepthDraw layers, PSD depth import | 40 | run (PSD import and several DepthDraw tools not) |
| [depth-bones-rig](families/depth-bones-rig.md) | DepthRigRoot, bones, sources, posing, MMD, glTF rig and mesh import | 47 | run for create, skeleton, bones, sources, influence rule, drag; rest manifest only |
| [physics](families/physics.md) | SimplePhysics (INX) | 18 | run for `physics_set`, `physics_step`, `physics_reset`; two `Inspector_Apply_SimplePhysics*` names run, the rest inferred |
| [import-merge](families/import-merge.md) | `import_*`, `merge_*`, PSD and Krita plans, model templates | 18 | run for images and INP; PSD and Krita manifest only |
| [split](families/split.md) | `split_*` (pointer to the split-illustration skill) | 10 | run |
| [atlas-export](families/atlas-export.md) | atlas planning, INP/image/frame/GIF/video/glTF export, codec probe, command preview, collect | 19 | run except video; shared reads have focused tests |
| [textures](families/textures.md) | `texture_get`, `texture_set`, `texture_export`, `texture_replace`, texture clean-up, `image_process` | 9 | run except premultiply, `texture_set`, `texture_export` and `texture_replace` |
| [history](families/history.md) | undo, redo, goto, limit, clear | 8 | run |
| [session](families/session.md) | `new`, `open`, `save`, `snapshot`, metadata, extension data, repair | 16 | run except repair and fake layer names |
| [render-scene](families/render-scene.md) | `render`, `bounds`, `evaluate`, `scene_*`, `model_statistics`, screenshots, `view_*`, `sketch_snapshot` | 18 | run |
| [motion3](families/motion3.md) | Motion3 import, bind, sample, seek, edit, fades, pose switches, export (`motion3_*`, `Motion3Command_*`) | 26 | run on INX; CMO3 paths manifest only |
| [live2d-authoring](families/live2d-authoring.md) | CMO3 source reads, form vertex, opacity, draw order, colour, rotation, blend, ArtPath point and brush edits, reparenting, copying, deletion, renaming, ID changes, parameter ranges, keyform keys, user data, display information, physics and user data export, INX conversion, runtime model reads | 45 | run for `live2d_parameter_edit`, `live2d_physics3_read` and `live2d_physics3_edit` (tools/verify-acp-agent.py); rest manifest only |
| [live2d-animation](families/live2d-animation.md) | CAN3 read, attach, preview, key insertion/deletion, Image/Guide point edits and insertion/deletion, handle, curve type, parameter curve, resource, track and scene edit | 30 | key, parameter-curve and track MCP proofs; `live2d_animation_key_add` run by tools/verify-acp-agent.py; other entries narrower tests |
| [live2d-expression](families/live2d-expression.md) | expressions and `live2d_parameter_set` | 6 | run (tools/verify-acp-agent.py) except `fades: true` and `Live2DCommand_ParameterSet` |
| [live2d-physics](families/live2d-physics.md) | Physics3 runtime | 5 | run (tools/verify-acp-agent.py) |
| [live2d-pose](families/live2d-pose.md) | Pose3 runtime | 3 | run (tools/verify-acp-agent.py) |
| [editor-state](families/editor-state.md) | editor window state: selection, armed parameter, tool, camera and per-view sync, Parameters filter/bookmark/clip selection, native parameter name-or-ID filter, selection overlays, background, Part menu sorting, About, Welcome, Command Browser and Texture Viewer visibility, address dialog and clipboard queue, grid, guides, physics preview, Solo, animation overlays, Lightweight playback, Timeline zoom, panels, layout reset, workspaces, crash recovery and Keep for later, settings, shortcuts, nonsecret coding-agent lifecycle observations and conditional external Quit (`editor_*`) | 33 | See the family map for verification scope; individual panel checks use `tools/verify-editor-panel-state.py` |

Choices of grouping. The requested list had no home for file and session tools, rendering, or the `split_*` tools, so `session`, `render-scene` and `split` are extra maps. "Mesh and deform" is one map;
"textures" and "history" are separate. `editor-state` is its own map because its tools act on the editor window, its recovery snapshots and its settings file, and need `soul2d-editor`. The Live2D families are the five requested maps plus Motion3, which serves INX and CMO3 with one tool set, so it has its own map. `live2d_parameter_set`
sits in live2d-expression because it is the same session-only runtime state; `live2d_form_set_*` sit in live2d-authoring, `live2d_model_attach` in live2d-animation. Tool rows count table rows, so
a cross-listed tool (for example `node_mask_edit`, `mesh_mirror`) counts in each map that lists it.

## Keeping the maps true

[tools/verify-operating-skill.py](../../../tools/verify-operating-skill.py) exits non-zero and lists names when a manifest tool is in no map, a map's table lists a name that is not in the manifest, or the "Tool rows" number above is not the number of tools the map lists. It runs from
`tools/verify-command-manifest.sh`. When you add a tool, put it in the table of its map (first cell, backticked, aliases separated by ` / `) and run both scripts. When you change what a map says,
run the sequence against a headless host and keep the "Verified" lines honest; do not mark a map run if it was not.

Related skills: `verify-soul2d` (prove changes to the soul2d code, GUI-free), `split-illustration` (the full split workflow).
