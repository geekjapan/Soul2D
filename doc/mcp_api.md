# MCP API Roles

This document defines the public MCP surface of `soul2d` in terms that a client can follow without reading server code.

## Core Split

- `resources` are for exploration and reading.
- `tools` are for actions and mutation.

If an endpoint can change app state or trigger side effects, it belongs under `tools`.
If an endpoint only returns current state, it belongs under `resources`.

## Explore / Read / Act

Use this flow consistently:

1. List or explore with `resources/list` or `resource://soul2d/resources/find?selector=...`
2. Read with `resource://soul2d/resources/{uuid}`
3. Act with `tools/call`

In `soul2d`, `resources/list` is the main state traversal entrypoint. It lists currently readable resource instances for the active puppet.
For broad discovery, the recommended first query is `resource://soul2d/resources/find?selector=*`.

## Resource Endpoints

- `resource://soul2d/resources/find?selector=...`
  - Explores current resource instances by selector.
  - The `selector` query parameter uses URL-encoded `soul2d.core.selector` syntax.
  - Returns a hierarchical tree with basic fields.
  - For broad discovery, start with `resource://soul2d/resources/find?selector=*`.
  - The response includes `guidePrompt: "selectors/guide"` and `usagePrompt: "resources/find"` so clients can discover the matching prompts.
- `resource://soul2d/resources/{uuid}`
  - Reads one resource instance by UUID.
  - Nodes include richer serialized state than parameters.
- `resource://soul2d/bindings/get?parameter=<parameter-uuid>&target=<target-uuid>&name=<binding-name>`
  - Reads one parameter binding by stable descriptor.
  - Returns the binding target, interpolation mode, set count, and serialized `data`.
  - `axisValues` maps `data.values[x][y]` indexes to parameter-axis values.
  - `data.values` contains the binding value grid. For `deform` bindings this is the deformation offset data.
  - Use Binding URIs returned by `resources/list` or `resources/find`; do not use Binding pseudo-UUIDs as input.

- `resource://soul2d/part-split/plan` returns the active INP/INX split plan, per-part statistics, coverage counts, `documentRevision`, and `draftRevision` as JSON. It is listed for INP/INX documents. A closed source or a native document returns error `-32002`. Reads change no state. The nested `plan` is the version1 object accepted by `split_apply`.
- `resource://soul2d/part-split/source` returns the loaded source pixels as an RGBA PNG binary resource (`mimeType: image/png`, base64 `blob`). It does not reload the source file.
- `resource://soul2d/part-split/masks/{name}` returns the canonical mask as a source-sized grayscale PNG binary resource. Percent-encode the Part name once. Coverage includes feathering and references, before source alpha, cropping or padding. Concrete mask URIs are listed for the open draft; the template is discoverable. Malformed names return `-32602`; unavailable source/Part or native documents return `-32002`. Image reads change no state. See [source and mask images](part-split.md#source-and-mask-images).

- `resource://soul2d/skill/operate-soul2d` returns the operating skill's `SKILL.md` as `text/markdown`, and `resource://soul2d/skill/operate-soul2d/families/{family}` one of its 23 family maps (`resources/list` names them). Every host serves them, so an ACP agent reaches the skill without a checkout of this repository.

## Discovery Endpoints

- `resources/list`
  - Lists currently readable resource instances for the active puppet.
  - Includes concrete UUID-based URIs, Binding descriptor URIs, and the selector exploration endpoint.
- `resources/templates/list`
  - Lists parameterized read definitions such as `resource://soul2d/resources/{uuid}`.
  - This is a capability-discovery endpoint.

## Prompt Endpoints

- `selectors/guide`
  - Describes selector syntax and how hierarchy is returned from `resources/find`.
- `resources/find`
  - Describes how to call `resource://soul2d/resources/find?selector=...`
  - Recommends `selector=*` as the first-pass discovery query.
  - Links clients back to `selectors/guide` for the selector language itself and `resources/guide` for the workflow.

## Tool Endpoints

- `tools/list`
  - Lists action definitions.
- `tools/call`
  - Executes an action.
  - Inputs may reference resources by UUID, but the tool remains an action endpoint, not a read endpoint.
  - A name that `tools/list` does not declare is refused with `unknown tool: <name>`.
  - A call that leaves out an argument the tool's input schema lists in `required` is refused before the operation runs, with an error that names the tool and the missing arguments. A `null` value counts as given.
  - `required` lists what the operation cannot run without. An argument for which the operation reads another name is not required: the description of that property names what is read in its place (`nodes` of `node_copy`: `id`, then `ids`; `parent` of `node_move`: `newParent`; `target` of `depth_list`: `id`), and the other names are not declared as properties (the D-named `NodeCommand_PasteNode` and `NodeCommand_MoveNode` keep both `parent` and `newParent`). One exception: a tool that creates a node requires `className` (`node_create`, `node_insert`, `NodeCommand_AddNode`) and `_suffix` (`NodeCommand_AddNode`, `Node_Add_*`, `Node_Insert_*`), which the operation could default, so that a call says what it creates. A `className` that is not a string (`null`, a number) is refused by the operation with `className must be a string`.
  - A refusal repeats at most 64 characters of a tool or argument name the caller wrote, and at most 8 names.
  - A D-named tool that sets a value itself (the class of `Node_Insert_GridDeformer`, the property of `Inspector_Apply_PartOpacity`, the nodes taken from `context`) refuses an argument that would replace that value, with `<tool> sets <key> itself: do not pass the argument <name>`.
  - A tool whose input schema sets `additionalProperties: false` refuses undeclared arguments with an error that names them. Most tools declare `additionalProperties: true` and forward unlisted arguments to the session operation; the `op` argument is always dropped.

## Tool Context

Most tools take explicit arguments and ignore `context`. Four kinds of tool read it:

- Node-targeting D-named tools (`NodeCommand_*`, `Node_*`, `Inspector_Apply_*`, `VertexCommand_*` and similar) take `context.nodes`, a Node UUID array, as their targets. Those with no other way to name a target list `context` in `required` and are refused with `missing required argument(s) for <tool>: context` without it. Exceptions: `NodeCommand_SetNodeName` also takes `{id, name}`, `NodeCommand_PasteNode` also takes `parent` or `newParent`, and `NodeCommand_AddNode` and `Node_Add_*` create under the root when no context is given. The depth and rig aliases read `context.nodes` when the explicit argument is absent. `DepthMapCommand_*` take it as `target` and need exactly one node (`DepthMapCommand_ImportPSDDepths` does not read it). `DepthBoneCommand_AddDepthBone` takes all of it as the parents, so give one node. The others read the first node: `DepthBoneCommand_CreateDepthRigRoot` for `parent`, `DepthBoneCommand_AddStandardDepthParameters`, `_AddStandardDepthSkeleton`, `_ListDepthBones` and `_FitDepthRigRootZToDepth` for `root`, `DepthBoneCommand_FitDepthBoneZToDepth` for `bone`. Every tool that reads `context` declares the members it reads in its schema.
- `ViewCommand_CaptureLiveScreenshot` takes `context.parameters` (one Parameter UUID) with `context.parameterValue` (`[x, y]`) to render that value without changing the session.
- `NodeCommand_ToggleVisibility` toggles the first of `context.nodes`; `FileCommand_MergeImageFiles` adds the Parts under the first of `context.nodes` when no `parent` is given.
- `DepthBoneCommand_PreviewDepthBoneDeform` and `DepthBoneCommand_ApplyDepthBoneDeform` take the parameter from `context.armedParameters`, then `context.parameters`, when no explicit `parameter` is given.

Apart from these, `context.armedParameters`, `context.bindings` and `context.parameterValue` are not read by the Rust host. A parameter key is chosen with the explicit `key` argument: a pair of indexes into the parameter's
`axis_points` (`[ix, iy]`; `[ix, 0]` for a 1D parameter), as in `binding_set_trs`, `binding_set_deform`, `binding_keypoint` and `ModelCommand_SetTRSBinding`. Read the indexes off `parameter_get`.

Bindings are named by `{ "node": <Node UUID>, "param_name": "<binding name>" }` in the `bindings` argument of `binding_remove`, `binding_set_interpolation`, `binding_copy`, `binding_paste`, `binding_copy_to` and `binding_swap_with`, together with an explicit
`parameter`. Binding resource pseudo-UUIDs are inspection-only and are not tool input.

The same triple is used for reading binding data as a resource:

```text
resource://soul2d/bindings/get?parameter=123&target=456&name=deform
```

## Editor State

`context` on a tool call is input only. What the user has selected and is looking at is read from one resource and set with the `editor_*` tools. None of it is document state: reading or setting it adds no history entry and changes neither `modified` nor `revision`.

### `resource://soul2d/editor/state`

Every host lists and serves it.

- `editor`: `true` in `soul2d-editor` (its MCP listener, stdin MCP, command palette and agent panel), `false` in `soul2d-core`.
- `document`: `{ path, format, modified, revision }`. The session holds these, so every host reports them. `path` is `null` for a document that has no file yet.
- The keys below exist only when `editor` is `true`. A host without a window leaves them out; it does not send empty values.

| Key | Value |
| --- | --- |
| `quit` | Conditional external Quit attempt: `{state, attempt?, reason?}`. States are `idle`, `waiting_for_reply`, `cancelled` and `closing`. Acceptance and local delivery are distinct from observed process exit. |
| `codingAgent` | Exactly seven booleans derived from the existing ACP panel owner: `enabled`, `has_client`, `closed`, `ready`, `turn_active`, `cancelling`, `awaiting_permission`. Their meanings and observation boundary are below. No command, prompt, endpoint, token, status text or identifier is included. |
| `selection.nodes` | Selected node UUIDs in selection order. The first is the primary node, which the inspector, the viewport and the mesh tools follow. |
| `selection.bindings` | The Parameters panel's selected binding rows as `{ node, channel }` pairs, in selection order; `[]` when none. They belong to the armed parameter at its key. |
| `selection.vertices` | `null` outside a mesh edit, else `{ node, indices }`: the node the active Edit Mesh or deform tool targets and the selected working-point indices, in selection order. |
| `selection.timeline` | `null`, or `{ animation, parameter, axis, frame }`: the keyframe the Timeline panel has selected (animation name, parameter UUID, axis index, frame). |
| `armed` | `null`, or `{ parameter, key, keyValue }`: the armed parameter's UUID, the keypoint index pair closest to its current value (the `key` that binding operations take), and the parameter value at that keypoint. |
| `tool` | `{ mode, tool, offered }`. `mode` is `model` (no edit tool), `vertex` (Edit Mesh), `deform` (a mesh node is selected while a parameter is armed) or `depth`. `tool` is the active tool id, `null` in `model` mode. `offered` lists the ids `editor_tool_set` accepts now. |
| `camera` | `{ centre, zoom, mirror, viewSize }`. `centre` is the world point in the middle of the viewport (the toolbar readout shows its negation). `zoom` is in points per world unit. `mirror` is Mirror View. `viewSize` is the viewport size in points. |
| `viewCameras` | `{ active, views: [{ id, camera }] }` for INX and CMO3. Each `camera` has `centre` and `zoom`, or is `null` if an inactive view has never had a camera assigned. A new active view inherits the outgoing position. View changes are reconciled between MCP calls without waiting for a frame. Mirror View is global in `camera.mirror`. CAN3 has no multiview and reports `null`. Replacing the document clears the cache; the current camera stays until fitting. |
| `parameterPanel` | INX only: `{ filter, animation, bookmark }`. `filter` matches parameter and group names case-insensitively. The armed parameter remains visible separately. `animation` is the window-selected clip, or null without a clip; it can differ from the Session's preview animation. `bookmark` is null or the resolved transient `{ animation, label, point, parameters }` selection. This selection narrows ordinary rows further. It clears on clip change or document replacement; deleting a stored label alone keeps the copied selection. The filter survives replacement. Native formats report null and keep their separate name-or-ID filter. |
| `nativeParameterPanel` | CMO3/CAN3 only: `{ filter }`, matching names or vendor string IDs case-insensitively. Empty shows all rows. Available without an attached model, with no rows. Separate from INX `parameterPanel`; retained across document replacement, reset on restart. INX reports null. |
| `viewport` | `{ vertices, bounds, orientation, background, partSort }`: selection overlay flags and opaque RGB background `[r,g,b]`. Defaults are all true and black. INX `partSort` is `z_sort` (default, front-to-back) or `size_sort` (ascending bounding-box area); native formats report null. It sorts only the context menu's unselected, unlocked Part candidates whose bounds contain the pointer. Direct click and parameter-bookmark hit tests remain separate. Values survive document replacement and reset on restart. Stored overlays remain readable while a parameter is armed; selection and edit mode determine which are drawn. Window session only. |
| `physicsPreview` | `{ enabled, available }`: the stored Enable physics flag, and whether this INX document supports the preview. The window keeps the flag across document replacements, so `enabled` can be true while `available` is false. A failed step or successful physics bake turns it off. |
| `windows` | `nerdStats` is the saved Stats for Nerds preference. `guideSettings` is whether Guide Settings is open, kept only for this window session and independent of `guides.show`. `about` is the session-only About window visibility. `textureViewer` is Texture Viewer visibility; repeated open retains its payload, while close discards it. `unsaved`, `restorePrompt` and `can3Delete` are the dialogs that wait for the user's answer at the window, `null` when closed: `unsaved` is `{ action, hasPath, saveFailed }` (`action` is what the answer carries out: `new`, `open`, `import`, `close` or `replace`; `hasPath` whether the document has a path for Save; `saveFailed` whether the last Save from the dialog did not finish), `restorePrompt` is `{ backups }` for the "Restore Autosave" prompt, `can3Delete` is `{ kind }` (`scene`, `track`, `curve`). While one is open, calls that replace the document are refused with "A dialog in the editor waits for the user's answer"; no tool answers a dialog, by the security review decision of 2026-10-13 (the answer discards, writes or replaces the user's work and belongs to the person at the screen). No path is reported. |
| `grid` | `{ show, spacing, colour }`: the canvas grid. `spacing` is in world units, `colour` is unmultiplied `[r, g, b, a]`. View-only, kept for the window and not saved. |
| `solo` | `{ show, colour }`: the Solo view. `colour` is the unmultiplied `[r, g, b, a]` inactive colour; its alpha is the opacity. View-only, kept for the window and not saved. |
| `guides` | `{ show, snap, colour, lines }`: the guide lines. `colour` is the default colour of a new guide. `lines` holds `{ axis, position, colour, memo }` per guide: `axis` is `horizontal` (a line at y = `position`) or `vertical` (at x = `position`), in world units. View-only, kept for the window and not saved. |
| `panels` | `{ outline, timeline }`: the View > Panels switches. The timeline is drawn only for INX documents. |
| `workspaces` | The names of the saved workspaces, sorted (`editor_workspace_save`, View > Workspace). Kept in the editor settings. |
| `drafts` | `{ panels, tools }`. `panels`: a rename, anchor, Motion3, Live2D form or binding value draft is open, which makes the file tools refuse. `tools`: the mesh or depth tool holds edits that Apply has not committed; an edit taken back with undo does not count, even while redo is offered. |

Tool ids: `point`, `connect`, `grid`, `lasso`, `edge_cutter`, `brush`, `path_deform`, `bezier` (mesh tools); `direct_depth`, `ring`, `attached_point`, `plane` (depth tools).

`codingAgent.enabled` is the panel's opt-in flag, independent of visibility. `has_client` means the panel owns a run/client, not that an OS process is alive. `closed` is the retained run's closed flag, so `closed` and `has_client` can both be true. `ready` means a valid ACP initialization was processed and the client is not marked disconnected; it does not mean a session was created. `turn_active` includes a prompt waiting for session creation or its response, and does not replace Quit's narrower initiating-prompt check. `cancelling` is the processed conversation cancellation state. `awaiting_permission` means the processed ACP permission queue is nonempty; it excludes the separate host operation approval.

These are observations of processed owner state when the serving editor view is constructed. Ordinary HTTP/stdio and ACP serving can observe different moments within a frame: ACP's view is constructed after the existing second lifecycle pump. Approved requests resumed on a later frame use that frame's view. Several requests served by one view share its observation; newly arriving events wait for an existing pump. The added projection does not poll, send, start, stop, cancel or answer permissions. Existing editor-state reconciliation remains. No wire freshness, process health or cross-transport atomicity is promised. The input-derived `prompt_present` flag and prompt/briefing text, command/arguments/workdir, credentials, endpoint/session identifiers, permission contents, transcripts and error/status text are excluded. There is no new control tool or permission category; headless hosts omit the entire object.

```json
{
  "editor": true,
  "codingAgent": { "enabled": false, "has_client": false, "closed": false, "ready": false, "turn_active": false, "cancelling": false, "awaiting_permission": false },
  "document": { "path": "/models/aka.inx", "format": "inx", "modified": false, "revision": 4 },
  "selection": { "nodes": [456], "bindings": [], "vertices": null, "timeline": null },
  "armed": { "parameter": 123, "key": [1, 0], "keyValue": [1.0, 0.0] },
  "tool": { "mode": "deform", "tool": "point", "offered": ["point", "brush", "lasso", "path_deform"] },
  "camera": { "centre": [0.0, -120.0], "zoom": 0.5, "mirror": false, "viewSize": [1280.0, 720.0] },
  "viewport": { "vertices": true, "bounds": true, "orientation": true, "background": [0, 0, 0] },
  "grid": { "show": false, "spacing": 100.0, "colour": [128, 128, 128, 128] },
  "solo": { "show": false, "colour": [128, 128, 128, 192] },
  "guides": { "show": true, "snap": false, "colour": [0, 200, 255, 255], "lines": [{ "axis": "vertical", "position": 0.0, "colour": [0, 200, 255, 255], "memo": "" }] },
  "animationOverlays": { "onion": false, "trajectories": false },
  "playback": { "lightweight": false, "snapToFramerate": false },
  "physicsPreview": { "enabled": false, "available": true },
  "windows": { "nerdStats": false, "guideSettings": false, "about": false, "welcome": false, "textureViewer": false, "recoveryOffer": { "pending": false, "drawEligible": false }, "unsaved": null, "restorePrompt": null, "can3Delete": null },
  "panels": { "outline": true, "timeline": true },
  "workspaces": ["Modeling"],
  "drafts": { "panels": false, "tools": false }
}
```

The editor answers MCP between frames. `tool` and `camera.viewSize` are as of the last drawn frame: the frame derives the edit mode from the selection and the armed parameter. After `editor_selection_set` or `editor_armed_set`, read the state again before `editor_tool_set`. Before the first frame `viewSize` is 512 by 512.

### Editor tools

The view tools below return the content of the state resource after the change. The recovery and settings tools, which follow them, return what their entry says. All schemas refuse undeclared arguments.

- `editor_quit`
  - In the native Merge PSD/KRA panel, **Cancel merge plan** releases an unapplied plan, including an empty plan. It preserves the source path and options without changing the document or undo history; New/Open and Quit can then proceed when their other guards permit.
  - `{}` requests a non-discarding exit through the editor's external HTTP or stdio MCP host. A non-null request ID and a reply writer are required. Headless, internal command-browser and ACP calls refuse; ACP lifecycle approval remains unsupported.
  - Returns `quit {state:"accepted", attempt, policy:"close_after_reply_flush_and_final_safety_check"}`. Acceptance is conditional. The editor resource reports `quit {state, attempt?, reason?}`: `idle`, `waiting_for_reply`, `cancelled` or `closing`. A duplicate pending request refuses.
  - The writer must emit the exact successful matching-ID reply completely and flush it before the App can close. HTTP also finishes its write half-close; stdio includes the newline. This proves local output completion, not peer application consumption. Missing/error replies, failed output, a lost writer or the five-second deadline cancel exit. Late completion cannot revive it.
  - Admission and the final frame check refuse unsaved document changes, unapplied authored drafts, active mesh/depth or PSD-depth editors, waiting human dialogs, an unapplied split or merge plan, playback, background export/probe/speech-analysis/deformation work and any retained ACP client until explicit Stop. Drafts include parameter names/axes/split choices, sequence placement/timing/exposure, Flip Pairing, animation/object/physics edits and pending viewport or binding transforms. A path decision, shortcut capture or unapplied MCP configuration also refuses. Ordinary filters, paths and navigation text are not document drafts. Active editing modes are conservatively refused to protect held previews. A recovery offer alone does not delete its offered files or require refusal.
  - Document replacement or fresh work while output is pending cancels the attempt. Resolve the reason and request Quit again. A blocked stdout worker can remain unavailable after cancellation; the editor stays responsive through its existing UI or HTTP listener.
  - No Save/Discard option or dialog answer is exposed. Native human/OS close remains independent and can cancel the pending remote attempt. Once remote Quit is accepted, EOF fallback stays disarmed even after cancellation; output/read/channel failure is never genuine EOF. A fresh explicit Quit or native close is required. Normal App exit cleanup remains authoritative.
  - `python3 -I tools/verify-editor-quit.py --self-test` checks the verification client's framing and identity handling only. For an owned native run, use its `--stdin-pipe`, `--stdout-pipe` and new `--record` paths to keep private FIFO transport open, then launch the app through the native UI harness. Send identified RPC lines to the bridge. Observe the complete response and the owned process's natural exit before sending `{"bridge":"stop"}`. The helper neither launches an app nor proves exit, and its records contain private document data.

- `editor_selection_set`
  - `{ "nodes": [uuid, ...] }` replaces the node selection, in the order given. An empty list clears it. INX documents only; every UUID must exist.
- `editor_armed_set`
  - `{ "parameter": uuid }` arms a parameter, `{ "parameter": null }` disarms. The parameter value is not moved: the key is the keypoint closest to the current value. To arm at another keypoint, call `value_set` first.
- `editor_camera_set`
  - `{ "centre": [x, y], "zoom": z, "mirror": bool }`, at least one. `zoom` must be within the editor's limits (0.05 to 12). `mirror` sets the same Mirror View state as the viewport toolbar. Omitted fields stay unchanged. A mirror-only call keeps a pending initial camera fit. These view settings belong to the window session and do not change the document or its history.
- `editor_camera_fit`
  - `{ "nodes": [uuid, ...] }` centres on the combined bounds of the nodes, as Focus Selected does. Without `nodes` it fits the whole model, as Fit preview does.
- `editor_camera_sync`
  - `{}` gives every INX or CMO3 view the active view's centre and zoom, through the same operation as Same display position. Returns editor state with `viewCameras`; Mirror View remains global. A single view succeeds unchanged. The explicit position suppresses any pending initial fit. CAN3 is refused. This is window-session state, with no document edit, history entry or settings write.
- `editor_parameters_set`
  - `{ "filter": string, "animation": name, "bookmark": selector }`, at least one. Omitted fields stay unchanged. An empty filter clears it. `bookmark: null` runs Show all parameters without clearing the filter. A selector is either `{ "labelIndex": index }`, using the stored zero-based bookmark index of the selected clip, or `{ "part": uuid, "point": [x,y] }`, resolving a Part's related parameters at a model-space bookmark point. Points are finite and within +/-1e9; raw parameter lists and extra selector keys are refused. A label must be one the viewport can display. Read stored labels from `model.animations[name].bookmarks` in the Session snapshot.
  - `animation` selects the existing clip in the window, like the INX clip combo: a changed choice pauses controller playback, resets its local seek frame to zero and clears the old bookmark. It does not seek or replace the Session preview. A bookmark in the same call resolves against the new choice. The complete call is validated before any requested field changes. Invalid or deleted selected clips fall back to the first clip as the GUI does, even while Outline is hidden; their bookmark clears.
  - Returns editor state with `parameterPanel`. INX editor only; native formats and headless hosts refuse it. Selection is session-only, with no model edit, history entry or settings write. Keep as label and Delete label remain the separate document operations `animation_bookmark_set` and `animation_bookmark_delete`.
- `editor_native_parameters_set`
  - `{ "filter": string }` changes the native preview's case-insensitive substring filter on name or vendor string ID. An empty string clears it. Returns editor state with `nativeParameterPanel {filter}`, null on INX and absent on headless hosts. CMO3/CAN3 editor only, including CAN3 without an attached CMO3 (no parameter rows). INX and headless hosts refuse it. Shares the GUI operation; no parameter value, playback, document, history or settings change. Independent of INX `parameterPanel`; retained across document replacement, reset on editor restart. Lightweight playback freezes values, not this filter.
- `editor_tool_set`
  - `{ "tool": id }` switches the tool of the current edit mode. The id must be in `tool.offered`.
- `editor_about_set`
  - `{ "open": bool }` opens or closes About through the same operation as Help > About and its close button. Read `windows.about` in the response and editor state resource. Repeated values are harmless; missing, null, non-boolean and unknown fields are refused before changing visibility.
  - Available for every document format in the editor, including while a draft is pending; headless hosts refuse it. Visibility survives document replacement, Reset Layout and listener restart, and starts false in a new process. No document, history or settings write, and no ACP host approval is required.
- `editor_welcome_set`
  - `{ "open": bool }` opens or closes the Welcome window through the same operation as Help > Welcome. Read `windows.welcome` in the response and editor state resource. Opening shows Quick Setup until the quick setup is done and the start page after it; an open window keeps its page. Repeated values are harmless; missing, null, non-boolean and unknown fields are refused before changing visibility.
  - Closing through the tool does not mark the quick setup done, so the editor opens the window at its next start until it is. The window's choices are the existing tools: `editor_settings_set` (`DarkMode`, `lang`, `UIScale`), `new`, `open` and `import_psd`; the first-run flags are not settings. Available for every document format in the editor; headless hosts refuse it. Visibility survives document replacement, Reset Layout and listener restart. No document, history or settings write, and no ACP host approval is required.
- `editor_command_browser_set`
  - `{ "open": bool }` opens or closes Command Browser through the same operation as Edit > Command Browser, its shortcut, the window's close button and Escape. Read `windows.commandBrowser` in the response and editor state resource. Repeated open preserves search, selection, argument draft, list and result; close clears them and reopening starts fresh. Missing, null, non-boolean and extra fields refuse before mutation.
  - Editor only, all document formats, including pending authored drafts and inactive or denied ACP prompts. Visibility survives document replacement, Reset Layout and listener restart, and starts false in a new process. No document, history or settings write or ACP host approval. Native close/Escape suppresses activation and refresh from that same draw; previously accepted requests are unaffected. Existing `tools/list` and `tools/call` provide discovery and execution; this tool exposes visibility, not browser input/result read/set or ACP Quit acceptance.
- `editor_texture_viewer_set`
  - `{ "open": bool }` opens or closes Texture Viewer through the same operation as Tools > Texture Viewer and its native close button. Read `windows.textureViewer` in the response and editor state resource. Repeated open retains Slot, Zoom, loaded images, loading and errors. Close drops the viewer; reopen starts unloaded at Slot 0, Zoom 1. Missing, null, non-boolean and extra fields refuse before mutation.
  - Editor only, all document formats, including pending drafts and inactive or denied ACP prompts. Visibility survives document replacement, Reset Layout and listener restart; a new process starts closed. Existing selected-Part `texture_get` loading remains unchanged; no Slot, Zoom or load API is added. Pending slot aggregation retains its existing lifetime, and completed replies never create a closed viewer. No document, history or settings write or new ACP approval. Ordinary dispatch reconciliation and pending Quit cancellation still apply.
- `editor_address_dialog`
  - `{ "action": "open", "target": "configuration" | "online_documentation" | "soul2d_live_documentation" | "bug_report" | "feature_request" }` opens or replaces the fixed address dialog. Configuration uses the settings file's parent path; an unavailable path fails without replacing the current dialog. No browser or file manager is launched, and arbitrary URLs, titles and paths are refused.
  - `{ "action": "copy" }` queues the original stored address for the clipboard; it fails if no dialog is open. `{ "action": "close" }` closes it idempotently. Copy and Close forbid `target`, including null. Read `windows.addressDialog`, null or `{target,title,address}`, in the returned editor state and state resource. The title and address are snapshots from Open; edits in the selectable text field are discarded. The Copy button and tool use the stored address; keyboard selection copying follows the text field's ordinary behavior. A Copy response acknowledges the egui output queue, not OS clipboard delivery.
  - Available for every document format in the editor; headless hosts refuse it. Document replacement, Reset Layout and listener restart retain the dialog; editor restart closes it. No document, history or settings write. The manifest's `clipboard-write` effect describes Copy only.
- `editor_viewport_set`
  - `{ "vertices": bool, "bounds": bool, "orientation": bool, "background": [r, g, b] }`, at least one. The three selection overlay switches and background colour next to the viewport zoom use this same operation. Every RGB channel must be an integer from 0 to 255; `[0,0,0]` is Reset background. Omitted fields stay unchanged. The entire call is validated before applying it, including combined updates. Returns editor state with `viewport`. This is window-session state, with no document edit, history entry or settings-file write. The overlay switches are hidden while a parameter is armed, but agents can still read and set the stored values.
  - Optional `partSort: "z_sort"` or `"size_sort"` sets the INX Part menu's Z-Sort / Size-Sort through the same shared operation. It may be the only field. Native documents refuse a sort update before applying any other supplied viewport fields, while overlay/background updates alone remain available. The stored mode remains accessible while the menu is hidden by mesh/depth tools. Document replacement retains it; editor restart restores Z-Sort.
- `editor_grid_set`
  - `{ "show": bool, "spacing": units, "colour": [r, g, b, a] }`, any of them. Shows or hides the canvas grid (Cubism "Show Grid", issue #204) and sets the space between its lines in world units (1 to 1,000,000) and their colour. The lines run through the world origin, over the picture and under the selection overlays. Lines closer together than 4 points are not drawn. The toolbar toggle "Show Grid" and the "Grid spacing" and "Grid colour" controls next to the zoom set the same state.
- `editor_solo_set`
  - `{ "show": bool, "colour": [r, g, b, a] }`, either or both. The Solo view (Cubism "Solo", issue #202). With it on and nodes selected in an INX document, the viewport draws the rest of the picture, covers it with the inactive `colour`, and draws the selected nodes and the nodes below them on top (each Part once; a Composite holding selected and other Parts draws in both, with the children of each), in one `render` with its `solo` and `solo_cover` arguments. The toolbar toggle "Solo" and the "Inactive colour" control next to the grid colour set the same state. A click on the canvas, and the right-click Part list, pick only a Part drawn in front.
- `editor_panel_set`
  - `{ "panel": "outline" | "timeline", "visible": bool }` shows or hides one panel through the same operation as View > Panels. Read `panels` from `resource://soul2d/editor/state` to confirm it. The other panel, panel sizes, drafts, document revision and history stay unchanged. The visibility key is saved to the settings file when available; a failed write reports that the change holds only for this session. Without a settings file, the change is session-only.
  - The first prompt of an ACP session carries a second text block, the editor's briefing: the MCP server is offered to the session, the skill resource URIs, and the document format, path, revision and selected node UUIDs at that moment, with the instruction to read `resource://soul2d/editor/state` again before acting. The panel's log shows it as "Soul2D". It goes once per run; a restarted agent is briefed again.
  - `tools/acp-claude.py` is an ACP agent for the installed `claude` CLI (Claude Code): set `ACP.Command` to `python3 -I tools/acp-claude.py`. It advertises HTTP MCP, hands the offered server to `claude -p` as its only MCP configuration with the built-in tools off but the two that list and read MCP resources (the briefing points at the skill and state resources), and the user's settings, hooks, skills and session persistence off, streams the assistant's text, tool calls and usage as `session/update`, and ends each turn with `stopReason`. `ACP_CLAUDE_LOG` names a file that receives every `session/update` and stop reason, which is how a run in the agent panel is checked. `tools/verify-acp-agent.py` runs it headless against `soul2d-core --mcp-port 0` for one workflow per family (edit, its reversal, edit, save, render) in 22 families, from nodes to Live2D physics, expression and pose playback on synthetic CMO3 data, with assets the script generates. Each workflow runs three times, the setup alone (a negative control the check must fail), a scripted MCP client and the agent, and the outcome is checked over the same endpoint with `njc` from resources (tools where no resource holds the value), the reopened file and the render; the three Live2D playback families check runtime state, which no file keeps, and save nothing. `--editor <soul2d-editor>` adds the editor-state family: its scripted client uses the editor's stdio MCP, since a headless host refuses the editor tools, its agent runs in the editor's agent panel (driving the panel needs an unlocked screen), and its check also requires that the revision did not move (#99 section 5, #83).
  - On the ACP agent's endpoint, persistent panel, `editor_settings_set`, `editor_workspace_save/apply/delete` and `editor_layout_reset` changes wait for **Coding agent operation approval**. The window names the tool, exact setting or workspace target, `file-write` effect and settings destination. Session-only changes without a settings destination need no file-write approval.
  - `save` / `FileCommand_SaveFile` and pathful `render` / `ViewCommand_CaptureLiveScreenshot` also wait for approval, independently of settings persistence. Their prompt names the resolved output destination, source document and revision. Editing or replacing the document, changing draft state or resolving the destination to a different file refuses the parked call. Save refuses dirty drafts. Capture uses the current preview when execution resumes. Pathless capture needs no approval.
  - **Allow once** completes the exact original HTTP call once, without an agent retry. **Deny changes this turn** refuses covered file writes and document replacement for the rest of that ACP prompt. Approval expires after two minutes, cancellation, prompt completion, disconnect or session replacement. A changed settings destination refuses a parked settings write. Read-only calls remain usable while a call waits; a second write cannot replace its target. Malformed calls, invalid or read-only preferences, and absent/null-ID notifications cannot request or consume approval. The window remains reachable with Outline hidden. Other MCP clients keep their existing behavior. Other file writers and real-agent acceptance remain separate requirements.
  - `new`, `open` / `FileCommand_OpenFile`, `editor_recovery_restore`, and the four replacing imports (`import_psd`, `import_kra`, `import_inp` / `FileCommand_ImportINP`, `import_folder` / `FileCommand_ImportImageFolder`) require the same exact-call approval. The effect is `project-reset` or `import`. Targets distinguish a new document, input file, input folder and recovery snapshot ID/path. Dirty drafts, unsaved changes, pending document dialogs and closing refuse ACP replacement before prompting. Recovery IDs resolve through the Restore listing, excluding symlinks and this session's snapshot. The resolved source path and current document stamp are rechecked on execution; a changed or missing target refuses the old approval. Input files and folder contents are read when execution resumes, so consent binds a path rather than a byte snapshot. Parsing still succeeds before the document is replaced.
  - Single-file exports also require exact-call ACP consent: `export_inp`, `export_image`, `export_gif`, `view_screenshot` / `ViewCommand_SaveScreenshot`, `object_export_csv`, `motion3_export` / `Motion3Command_Export`, and pathful `animation_onion` or Live2D CDI3/physics3/userdata3 export. The prompt names the resolved effective filename, after extension forcing (`.inp`, image format, `.gif` or `.png`). JPEG uses `.jpeg` even for an input `.jpg` name. Export targets refuse the open document before prompting; source-document and destination changes invalidate consent. Existing overwrite flags and unused-destination rules still apply after consent. Pathless runtime JSON exports and absent/null-path onion requests need no approval; a null path remains invalid for the runtime JSON exports. Real-agent acceptance remains separate work.
  - `export_frames` requires exact-call ACP consent for the ordered list of numbered output files. The scrollable prompt lists every entry; document or resolved destination changes invalidate approval. Numbered-file symlinks are replaced by the existing atomic writer rather than followed. The source is rendered when the approved call runs; consent does not freeze external assets. Invalid output extensions or animation plans are refused before prompting; camera, pixel-limit and overwrite checks still run before writing.
  - `export_video_start` asks for external-encoder execution consent. It shows the resolved executable, exact argument vector, working directory, primary output or sequence pattern, overwrite choice and captured animation dimensions/timing. Presets, arbitrary encoder tags and host options remain supported; the encoder may create additional outputs, and failure or cancellation may leave partial files. This is not a guarantee that only the displayed primary output is written. The reviewed command and captured document/manual values/ambient light are used for launch. Changing the destination, host executable/options or captured source invalidates consent; a preparation error also expires the old grant. Linked executable/input contents remain external mutable data, not byte-attested dependencies. The default primary filename/numbered-name checks reject symlinks and the open document; host options can override naming and create other outputs.
- `editor_playback_set`
  - `{ "lightweight": bool, "snapToFramerate": bool }`, either or both. Lightweight playback (Cubism "Lightweight playback", issue #223). With it on, while the window's Play runs an INX animation, a CAN3 scene or a Motion3 clip, the parameter panels keep the values they showed before Play; pausing shows the session's values again. Playback driven over MCP (`animation_advance`, `motion3_advance`) is not the window's and is unaffected. The "Lightweight playback" checkbox above the INX parameters, and the one in "Model parameters / facial controls", set the same state. `snapToFramerate` is the timeline check box "Lock playback to animation framerate": while it is on, the window's Play advances an INX animation in whole frames of its `timestep` and starts from the whole frame below a fractional one; `animation_advance` is unaffected. State: `playback {lightweight, snapToFramerate}`. Session only.
- `editor_timeline_zoom_set`
  - `{ "zoom": number }`, required and finite. Sets the absolute Timeline display multiplier through the same owner as native Zoom out, slider, Zoom in and hover zoom gesture. Rounds to f32 and clamps to `0.5..10`; the tool reply and editor-state resource expose the actual stored value at `timeline.zoom`. Native buttons/slider affect the current draw; the hover gesture affects the next draw. It does not seek, pause, change document/history or save settings. Existing generic request reconciliation, refusal status and pending remote-Quit cancellation remain.
  - The value belongs to the editor window, survives animation/document/layout/workspace changes, and can be read/set while Timeline is hidden, no animation is selected, or the document is not INX. A fresh editor starts at `1`. This adds no fit/reset operation or scroll anchoring. Editor-only: the headless resource omits it, and headless calls refuse for both valid and malformed arguments. ACP requires no new host approval.
- `editor_workspace_save`, `editor_workspace_apply`, `editor_workspace_delete`
  - All three write the settings file when a destination is available, including the panel visibility changed by `apply`. ACP calls require the exact-call host approval described above. A failed write returns `workspace changed for this session, but the settings file could not be written`; the editor state reflects the session-only change and the prior settings file is kept. Without a settings destination these operations remain session-only.
  - `{ "name": string }`. A workspace (Cubism "Workspace", issue #200) is the layout of the window: whether the
    Outline and Timeline panels are shown, and their sizes (the Outline width, the Timeline height, as last drawn or
    applied; a panel with neither in this run, such as the Timeline of a CMO3 document, is saved at its default size).
    `save` stores the current layout under the name and replaces one of the same name; `apply` shows or hides the
    panels and gives them the saved sizes; `delete` removes it. Names are trimmed and must be 1 to 64 characters
    without control characters; at most 16 workspaces. An unknown name is refused, and the state lists only names the
    tools accept. They are kept in the editor settings file. A `workspaces` entry of the file in another shape makes
    the three tools refuse rather than write over it, and a damaged layout is refused by `apply`. Floating windows
    keep their places, and View > Reset Layout still resets them as well as the panel sizes. View > Workspace lists
    the saved names (a click applies one), "Save Workspace..." asks for a name, and "Delete Workspace" removes one.
- `editor_layout_reset`
  - `{}` runs the same operation as View > Reset Layout: show Outline and Timeline where their visibility keys already exist, clear their stored sizes and reset floating window positions. A visibility key never written stays absent (shown by default). Each window's open/collapsed state, Outline collapsing headers, Stats for Nerds, saved workspaces, camera, drafts and document history are kept. Returns editor state; read `panels`, or save a workspace and inspect its default panel sizes. This is editor-only, with manifest effect `file-write`: existing visibility keys are saved once when a settings destination exists. A failed write reports `layout reset for this session, but the settings file could not be written`. With no stored visibility keys no settings write is needed. ACP calls use the exact-call host approval above, with a Reset Layout target; notifications cannot write or consume a grant. Without a settings destination the reset is session-only.
- `editor_guides_set`
  - Optional `open: bool` opens or closes Guide Settings and returns `windows.guideSettings`. It is independent of drawing the lines with `show`, and can be combined atomically with the guide-data fields below. The toolbar toggle, context-menu entry and window close control use this shared operation. Window session only, with no settings write.
  - `{ "show": bool, "snap": bool, "colour": [r, g, b, a], "lines": [{ "axis", "position", "colour"?, "memo"? }, ...] }`, any of them. Guide lines of the modeling view (Cubism "Guide Line", issue #203). `lines` replaces every guide: at most 256, `position` within +/-1e9 world units, `memo` at most 256 characters, and a guide without `colour` takes the default `colour`. `[]` removes them all. `snap` makes a vertex dragged with the Point tool adhere to a shown guide within 8 points, and the guide turns green while it holds one. The toolbar toggle "Guide Settings" opens a window with the same settings, and a right click on the canvas offers "Guide line" with a horizontal, a vertical or both guides through the click.

- `editor_physics_preview_set`
  - `{ "enabled": bool }` changes Enable physics in the viewport toolbar. Enabling requires an INX document; disabling is allowed for every format. The window keeps this flag across document replacement, as it does for Guide Settings. Read `physicsPreview.enabled` and `physicsPreview.available`. Preview stepping still uses the existing frame-delta limit, failure shutdown and playback synchronization. Disabling stops future steps and does not reset the simulation. This does not edit the document or settings file.

`editor_selection_set` and `editor_armed_set` refuse while `drafts.tools` is true or a binding value list is unapplied. A new selection or armed parameter makes the mesh and depth tools drop their unapplied edits and the binding value editor reload over its list; a click in the window is the user's own choice, a remote call is not.

Replacing the document (`new`, `open`, an import) disarms the parameter: the armed UUID belongs to the old document.

#### Crash recovery

`windows.recoveryOffer` is `{ "pending": boolean, "drawEligible": boolean }`. Pending means this window retains startup offer rows; it is separate from the files that `editor_recovery_list` reads from disk. Draw eligibility uses the window's predicate: rows remain and neither the unsaved-changes dialog nor Restore Autosave prompt hides the offer. It does not prove that the window is unobscured on screen. No separate visibility flag is stored.

The editor writes a recovery snapshot of a document that has no path yet and offers the snapshots earlier sessions left when it starts. `editor_recovery_restore` does what the offer window's Restore button does. Deleting a snapshot cannot be undone, so only the offer window's Delete button does it; there is no tool for it. A snapshot is named by its id, which is its file name in the recovery directory. The tools take no path: an id is only compared with the names in the directory listing, and anything else is refused with ``there is no recovery snapshot "<id>"``. Only regular files are snapshots: a symbolic link in the recovery directory is not listed and cannot be restored. The snapshot of the running session is not listed.

The offer window hides while the unsaved-changes dialog or the "Restore Autosave" prompt waits for the user's answer, so its buttons cannot be pressed then. `editor_recovery_restore` is refused for as long with `A dialog in the editor waits for the user's answer; have the user answer it there first`. So is every tool that replaces the document (`new`, `open` and the imports), because the user's answer acts on the document the dialog was raised for. `editor_recovery_list` and the other tools are still answered.

Once the user has answered the close dialog (or the setting skips it), the editor is closing, and the exit releases the snapshot of the running session and deletes it. `editor_recovery_restore` is refused then with `The editor is closing; its recovery snapshots can no longer be changed`.

While the offer window is on screen, `editor_recovery_restore` is still answered: the restored snapshot leaves the offer, so the window never lists an adopted snapshot and its buttons act on the rows it shows.

- `editor_recovery_list`
  - `{}` returns `{ "snapshots": [{ "id": "rust-recovery-1700000000-123.inx", "format": "inx" }, ...] }`, newest first. The window shows each snapshot by its file name, which is the id.
- `editor_recovery_keep`
  - `{}` dismisses this window's startup recovery offer through the same operation as Keep for later. It returns editor state with `windows.recoveryOffer` equal to `{ "pending": false, "drawEligible": false }`; repeating the call succeeds. Files, the document and its full history, project path, drafts, settings and owned snapshot are unchanged. Disk listing still finds the files, and the next startup offers them again. A temporarily hidden offer may be dismissed without answering the unsaved, Restore Autosave or CAN3 deletion dialogs. Dirty documents, drafts and closing add no Keep-specific refusal while dispatch remains available. ACP uses this same editor-only operation without a new approval category; Restore's approval and guards and native-only Delete remain intact. Ordinary dispatch bookkeeping, including pending Quit cancellation, still applies.
- `editor_recovery_restore`
  - `{ "id": id }` opens the snapshot as an untitled, modified document (Restore). It returns the editor state. The snapshot stays on disk and becomes this session's snapshot, so a save deletes it. The window then resets what the button's path resets: the notice, the armed parameter, the Motion3 and Live2D form panels, the Live2D asset fields and the camera fit. It is a document replacement and is refused like `open`: while a draft is open (`drafts.panels`) or the document has unsaved changes. A dialog that waits for an answer and a closing editor refuse it as well. A snapshot that cannot be opened is refused by its id; neither the message nor the editor's status line names the settings directory.

#### Editor preferences

- `editor_settings_get`
  - `{}` returns `{ "settings": [...], "shortcuts": [...] }`. Each preference is `{ key, label, type, value, writable }` plus `min` and `max` for an integer, or `allowed` for a string. `label` is its GUI control's label in Settings or View. `value` is the effective preference; `DarkMode` is `null` until the user chose a theme. The keys fall into three groups:
    - Listed with `writable: true`: `DarkMode` (`true` is dark), `lang`, `UIScale` (percent, not on macOS, where the window hides it), `useOpenDyslexic`, `MaxUndoHistory`, `KeepLayerFolder`, `LayerGroupReplacement` and `NerdStats` (View > Stats for Nerds; a visibility boolean, off by default).
    - Listed with `writable: false`, so only the Settings window changes them: `AutosaveEnabled`, `AutosaveInterval`, `AutosaveFileLimit` and `SaveProjectOnClose` (`Ask` or `dontSave`), because they guard the user's unsaved changes and the backups on disk; and the Integration pane's `MCP.Name`, `MCP.Enabled`, `MCP.Host`, `MCP.Port` and `MCP.authEnabled`, because they choose the address the editor serves.
    - Not listed, neither readable nor writable through these tools, because they can hold a secret: `ACP.Command` (an argument line), `ACP.Workdir` (an absolute directory) and `MCP.Headers` (credentials). The bearer token is not a setting and is not listed either.
  - `shortcuts` is the Shortcuts pane: `{ command, label, shortcut, spec, repeat }`, with `shortcut` the chord as the window shows it, `spec` its plain `Ctrl+Shift+S` form (what `editor_shortcut_set` takes) and `repeat` whether a held key repeats; all three `null` when none is bound.
- `editor_settings_set`
  - `{ "key": key, "value": value }` sets one preference with `writable: true`. The value is checked as the control constrains it, stored, applied (the undo limit, the import defaults and the language follow at once) and saved through the shared GUI preference setter. It returns `{ "setting": <the entry as listed>, "saved": bool }`; `saved` is `false` when the editor could not read its settings file at start, so it writes none. A key that is not listed is refused with `"<key>" is not a setting; editor_settings_get lists them`. A read-only key is refused, whatever the value, with `<key> can only be changed in the Settings window`. A value of the wrong type or outside the range or the allowed values is refused with a message that starts with the key. Nothing is stored or written for a refusal. The ACP endpoint validates these constraints before staging host approval, then checks them again at execution.
  - `NerdStats` opens or closes the same stats window as View > Stats for Nerds and its close button. Confirm the result with `windows.nerdStats` in `resource://soul2d/editor/state` and the preference readback. It does not edit the document or participate in Reset Layout.
  - A change whose write to the settings file fails is not a refusal: the answer has `isError` and says `<key> changed for this session, but the settings file could not be written`. The new value, a new language included, stays in effect until the editor closes. The message names no path.
  - `MaxUndoHistory` written here is saved like any other key, so it survives a restart: the editor applies the stored value at startup. Lowering it drops the oldest undo steps at once, and the steps are not recovered by raising it again. `history_limit_set` changes the limit for the running session only; it writes nothing, and the stored `MaxUndoHistory` applies again at the next start.

- `editor_shortcut_set`
  - `{ "command": id, "shortcut": "Ctrl+Shift+S" | null, "repeat"?: bool }` binds one command to a chord, or unbinds it with `null`, as the Shortcuts pane's capture does, then saves the settings file. `command` is a `command` of the `shortcuts` list; `shortcut` is that list's `spec` form (`Ctrl`, `Alt`, `Shift`, `Super` and one key `A`-`Z`, `0`-`9`, `F1`-`F12`, `Left`, `Right`, `Up`, `Down`, joined by `+`), the same on every platform, while `shortcut` in the list stays the window's display text. A chord another command held moves to this one and `displaced` names that command, now unbound. `repeat` defaults to the command's current flag. It returns `{ binding, displaced, saved }` with `binding` the command's updated list row. A refused call (unknown command, unknown key or modifier) changes nothing; a failed file write is reported like `editor_settings_set`'s.

### Hosts without a window

`soul2d-core` (`--mcp`, `--mcp-port`) has no selection, camera, tools, recovery offer or Settings window. It serves the state resource with `"editor": false` and the `document` key only. It lists the `editor_*` tools and answers each call with `isError` and `<tool> needs the editor: this host has no window`. `doc/commands.manifest.json` marks these tools with `"headless_ok": false`.

### Not yet exposed

- Parameter selection. The editor has none; the armed parameter is its only parameter focus.
- The binding selection of the armed parameter, the vertex selection of the mesh and depth tools (the indices belong to the tool's working copy), and the timeline selection.
- Entering or leaving Edit Mesh and depth mode. `deform` mode is reached by selecting a mesh node and arming a parameter.
- Mirror View, the overlay switches and the background colour.
- Rebinding shortcuts, and changing the autosave settings, `SaveProjectOnClose` and the Integration pane's settings (the agent command and the MCP endpoint): they guard unsaved work, choose a program to start and choose the address the editor serves, so only the Settings window changes them.
- The answers to the dialogs: unsaved changes, an external file change, and a draft's Apply or Reload.
- The open state of other windows (Settings, the agent panel).
- Which drafts are open; `drafts` reports only whether one is.

## Deform Binding Tools

- `binding_set_deform`
  - Sets raw `deform` binding offsets at `key` of `parameter` for `nodes`.
  - `values` is a flattened `[dx0, dy0, dx1, dy1, ...]` array and must match the target vertex count.
- `ModelCommand_SetTRSBinding`
  - Sets node transform `ValueParameterBinding`s, not `deform`.
  - `translation` writes `transform.t.x` and `transform.t.y`.
  - `scale` writes `transform.s.x` and `transform.s.y`.
  - `rotationDegrees` writes `transform.r.z` in radians after degree conversion.
  - `applyRotation=true` is required to write an explicit zero rotation.
  - `parameter`, `nodes` and `key` are explicit arguments (`key` is `[ix, iy]` axis indexes).

- `ModelCommand_SetRotationXBinding` / `ModelCommand_SetRotationYBinding`
  - Sets `transform.r.x` / `transform.r.y` value bindings on target nodes, including Parts. D enables these channels on every node.
  - Rust Core mode routes both commands to `binding.set_trs` (`rotation_x` / `rotation_y` in radians). `translation_z` on that same op writes `transform.t.z`.
  - `rotationRadians` is in radians; zero explicitly resets the selected key's rotation binding.
  - `parameter`, `nodes` and `key` are required and choose the targets and the keypoint.
  - Creates missing bindings and supports undo/redo.
  - Example njc call (replace UUIDs and the key indexes with your model's values):

```powershell
njc tools call ModelCommand_SetRotationXBinding --json '{"rotationRadians":0.2,"parameter":123,"nodes":[456],"key":[1,0]}'
```

## Rig Pose Tools

- `rig_apply_mmd_pose`
  - Writes an MMD pose into one keypoint of a parameter on a standard-skeleton DepthRigRoot: a `.vpd` file, or a `.vmd` file sampled at `frame` (30 fps). Rust Core only; native CMO3/CAN3 documents refuse it.
  - `root`, `parameter` and `path` are required. `key` is an explicit `[x, y]` keypoint; without it the keypoint closest to the parameter's current value is used. `translationScale` (rig units per MMD unit) overrides the scale derived from the rig. `dryRun` returns the report and changes nothing.
  - `boneTable` overrides the name table per MMD bone name: `{"<MMD name>": "<rig boneId>" | null}`, at most 64 entries. A boneId (`"Head"`, `"UpperArm.R"`, ...) reads a non-standard name as that bone's standard MMD counterpart; `null` ignores a name. Each rig bone keeps one source, so the standard name being replaced must be mapped to `null` in the same table, and a standard name cannot be redirected to another bone. `storeBoneTable: true` saves the table with the document in the same undo step (`{}` removes it); a stored table is the default of later calls, and an explicit `boneTable` replaces it for that call.
  - Writes `transform.r.x/y/z` (and Pelvis `transform.t.*`) through the `binding.set_trs` path, refreshes the keypoint deform, and is one undo step.
  - The result reports mapped bones with their angles, unmapped bone and morph names (text and hex), skipped bones, the frame and keypoint used, and which bone table applied (`boneTable`: `argument`, `document` or `default`). Details: `doc/mmd-motion.md`.
- `rig_import_gltf`
  - Adds the 19 standard bones below an empty DepthRigRoot at the rest positions of the humanoid in a glTF 2.0 or VRM file (`.gltf`, `.glb`, `.vrm`). Only the JSON of the file is read. INX documents only.
  - `root` and `path` are required. `unitsPerSourceUnit` is the number of canvas units for one metre of the file. `lossy` accepts that the mesh, textures, morphs, physics and animation are not read. `acknowledgeRights` accepts the terms a VRM states in `meta` or a plain glTF in `asset.copyright`, which the answer shows under `rights` (`truncated` names `rights` when that view is not all the file states). A file whose terms prohibit modification is refused. `dryRun` writes nothing.
  - The answer is the loss report: `source`, `counts`, `items`, `truncated`, `rights`, `needs` and `applied`. While `needs` names an argument, nothing is written. After a write it also holds `unitsPerSourceUnit` and `bones`. One undo step. Details: `doc/3d-rig-import.md`.
  - The last applied report is the resource `resource://soul2d/rig/import-report`.
- `mesh_import_gltf`
  - Prototype (issue #244). Imports the meshes of a glTF 2.0 or VRM file (`.gltf`, `.glb`, `.vrm`) as Parts seen from the front, one for each triangle primitive with a base colour PNG inside the file. z becomes the depth of each vertex, triangles that turn away are not drawn, and a skinned mesh is bound to the standard bones below `root` with its skin weights. INX documents only.
  - `root` and `path` are required. `rig_import_gltf` must have added the bones of the same file below `root` with the same `unitsPerSourceUnit`, and it keeps the rights a file states. The call is refused when a standard bone is not where the file puts it, or when `root` is moved or turned out of the picture plane. `unitsPerSourceUnit`, `lossy`, `acknowledgeRights` and `dryRun` are those of `rig_import_gltf`.
  - The answer is the same loss report. After a write it also holds `unitsPerSourceUnit`, `depthScale` and `parts` (`id`, `name`, `vertices`, `triangles`, `bones`). One undo step. A file without a primitive to import writes nothing and answers `applied: false`. Details and the measured result: `doc/3d-mesh-import-prototype.md`.
  - The last applied report is the resource `resource://soul2d/mesh/import-report`.
- `export_gltf`
  - Writes the open INX document as layered planes in glTF 2.0 without changing the document: one mesh for each drawn Part at its rest shape, in draw order along Z, with its texture as an embedded PNG. DepthBones become nodes, and each key of a one-axis parameter becomes a morph target.
  - `path` is required and ends in `.glb` or `.gltf`. `unitsPerMetre` is the number of canvas units for one metre of the file. `lossy` accepts what the report lists as approximated and dropped (bone weights, masks, blend modes, composites, physics, animations, two-axis parameters). `acknowledgeRights` accepts the rights data an import kept, which the answer shows under `rights` and the file holds in `asset.extras`. `overwrite` replaces an existing file. `dryRun` writes nothing.
  - The answer is the loss report: `direction`, `target`, `counts`, `items`, `truncated`, `rights`, `lossy`, `needs` and `applied`. While `needs` names an argument, nothing is written. After a write it also holds `path`, `bytes` and `unitsPerMetre`. No glTF validator was run on the output. Details: `doc/3d-layered-export.md`.
  - The last applied report is the resource `resource://soul2d/export/gltf-report`.
  - ACP calls prepare the report before requesting host approval. A `dryRun` or nonempty `needs` result stays read-only and remains available after denial. Notifications can request these reports but cannot write or consume approval. A ready write shows the resolved output file, byte count, overwrite choice and conversion report; it writes the prepared bytes only after exact-call approval. Document or destination changes expire the grant. Missing units, `lossy` and rights acknowledgement are never supplied automatically. A failed write keeps the previous applied report.
- `automesh_preset_save`, `automesh_preset_delete`, `automesh_preset_list`
  - Keep the option fields of a contour or optimum `automesh_generate` call under a name (`{name, algorithm, preset?, option fields}`), delete one (`{name}`) and list them (`{presets: {name: {algorithm, preset?, option fields}}}`). `automesh_generate` and the `automesh` request of `split_apply` take a saved name as `preset`; a field of the call replaces that field of the preset, and a preset belongs to the algorithm it was saved for.
  - The presets belong to the user of the machine, not to the document: they are the file `automesh-presets.json` in the settings directory (`INOCHI_CONFIG_PATH`, else where the editor keeps `settings.json`). Saving and deleting change no document and add no undo step. A save under a saved name needs `overwrite: true` and then answers `replaced: true`. Listing creates nothing. The three tools answer with any document open.
  - Refused: a built-in name, a name that is not 1 to 64 characters or has a control character anywhere or white space at either end, presets that would make the file larger than 1 MiB, the algorithms grid and skeleton, a field `automesh_generate` refuses, a `preset` that is not built in, a 65th preset, and a host without a settings directory. A file that is not valid (over 1 MiB, more than 64 presets, an entry that could not have been saved) refuses all three tools and a generate that names a preset that is not built in; it is kept as it is. A key of the file that is no preset field is not read, and the next save or delete writes the file without it.
  - ACP save/delete calls require exact, single-use host approval for the resolved store file, entry and action. Preparation creates nothing and captures the complete replacement JSON. Config-path, store-link or raw-file changes expire approval, including an unrelated entry changed by another host. Model-derived values must still match the approved content. Reads and document-only application stay available without store-write approval. Missing config remains an error; it never becomes a session-only save.
  - Execution rechecks the original file before atomic replacement. The comparison and write are not locked together, so concurrent writers can still race. Reading follows a store-file symlink; saving replaces that link itself, not its referent.
  - The list is the resource `resource://soul2d/automesh/presets`.
- `physics_preset_save`, `physics_preset_delete`, `physics_preset_list`
  - Keep SimplePhysics settings under a name (`{name, node?, settings, overwrite?}`), delete one (`{name}`) and list them (`{presets: {name: {settings}}}`). The settings are those of `physics_set` without `param`, `output_scale_x` and `output_scale_y`. With `node`, a setting the call leaves out, or gives as null, is the one that SimplePhysics node holds. A save refuses `nodes`, `output_scale_x`, `output_scale_y` and `preset`, which a preset cannot keep, and drops `param`. In `physics_set` a null setting other than `param` is refused, as without a preset, and `preset: null` is no preset. `physics_set {node | nodes, preset}` writes the settings of the preset first and the settings of the call after them, in one undo step, so a setting of the call wins.
  - Like the automesh presets, they belong to the user of the machine: the file `physics-presets.json` in the settings directory. The names, the limits, `overwrite`, a file that is not valid and the missing lock are the same as there. There are no built-in presets, and a preset without a setting is refused.
  - These are presets for INX SimplePhysics. The presets of the physics groups of a CMO3 are `live2d_physics3_preset_*`, below.
  - The list is the resource `resource://soul2d/physics/presets`.
- `live2d_physics3_preset_save`, `live2d_physics3_preset_delete`, `live2d_physics3_preset_list`
  - Presets for the physics groups of a CMO3 (PHY02, issue #236), in two kinds: `list: "inputs"` keeps the inputs of a group and `list: "vertices"` keeps its physical model, the pendulum vertices. `save {list, name, group | items, overwrite?}` keeps the list of a group (`group`, an index of `live2d_physics3_read`; the active CMO3 is read) or the given `items`, which work with any document open, and answers `{name, preset: {items}, replaced}`. `delete {list, name}` answers `{name}`. `list {}` answers `{inputs: {name: {items}}, vertices: {name: {items}}}`.
  - An input item keeps `source` (the parameter ID, so a preset goes from one document to another), `type` (`X` or `Angle`), `weight`, `reflect` and `scale` when the group has one. A vertex item keeps `x`, `y`, `mobility`, `delay`, `acceleration` and `radius`, with the limits of `live2d_physics3_edit`: a preset has 1 to 1024 inputs or 2 to 1024 vertices, and a radius of 0 only on the first vertex. Numbers keep the digits they were written with. Outputs are in neither kind, since an output names a vertex of the strand it belongs to, and the normalization ranges and the name of a group stay with the group.
  - `live2d_physics3_edit {group, list, preset, expectedRevision}` makes that list of the group the preset, in one undo step and with no other field. It sets the items both have field by field, inserts the preset's extra items at the end and removes the group's extra items from the end, each as the single edit would, so an unknown parameter, shared storage or an output that names a removed vertex refuses the whole call and changes nothing. An item that already matches is not rewritten, and a preset the group already holds is no undo step. An inserted input without `scale` gets 10, like an inserted item of `live2d_physics3_edit`.
  - `live2d_physics3_edit {expectedRevision, detach:true}` copies the complete known physics settings into private inline storage. It retains original shared definitions, parameter UUIDs and existing parameter-GUID references. The selected-group GUID follows its copied group; ambiguous or missing identity refuses the copy. It is one undo step, or a no-op when already private. No other edit arguments are accepted. See [the storage limits and refusal rules](live2d-authoring.md) before using it on shared source. Native vendor acceptance is separate.
  - The presets belong to the user of the machine, like the other presets: the files `physics3-input-presets.json` and `physics3-model-presets.json` in the settings directory. The names, the limits, `overwrite`, a file that is not valid, the missing lock and the ACP approval of saves and deletes are the same as there, for each file. There are no built-in presets. A save with `group` needs the active CMO3; the three tools otherwise answer with any document open.
  - The list is the resource `resource://soul2d/physics3/presets`.
- `animation_template_save`, `animation_template_apply`, `animation_template_delete`, `animation_template_list`
  - Keep the keys of an INX animation from frame `from` to `to` under a template name (`{name, template, from, to, parameters?, overwrite?}`), insert them into an animation from `frame` on (`{name, template, frame}`), delete one (`{template}`) and list them (`{presets: {template: {length, lanes: [{parameter, axis, keyframes}]}}}`). A lane is kept under its parameter's name, since a template goes from one model to another, and applies to the plain parameter with that name and an axis it has. Saving answers `{template, entry, replaced}` and deleting `{template}`. Applying is one undo step, none when nothing changes, and answers `{applied, lanes, unmatched, dropped}`: `lanes` lists the parameter UUIDs and axes written, `unmatched` the template's parameter names and axes no parameter takes, and `dropped` the keys past the end of the animation, which are left out. A new lane is Linear and Forced; an existing lane keeps its own interpolation and merge mode.
  - Like the physics presets, they are the file `animation-templates.json` in the settings directory, with the same names, limits, `overwrite`, invalid-file rule and missing lock. There are no built-in templates, and a template holds at most 100000 frames.
  - The list is the resource `resource://soul2d/animation/templates`.
- `animation_controller_add`, `animation_controller_set`, `animation_controller_delete`, `animation_controller_read`, `animation_controller_move`, `animation_controller_bake`
  - Parameter controllers of an INX animation. `add {name, controller, x, y, end?, frame?, parameters?}` pins a single-point controller to the frontmost Part drawn at model point (x, y) on `frame` (default 0, before eye blinking), or a direction controller from there to `end: [x, y]`. The point is kept as three vertices of a mesh triangle and their weights, so it moves with the mesh. `parameters` defaults to every parameter that can move the Part, or either Part of a direction controller (the rule of `animation_bookmark_parameters`); a listed parameter must be one of them, each once, at most 16 axes in all, and an empty list is refused. It answers the stored controller.
  - `set {name, controller, parameters?, target?, extend?, weight?}` changes what a controller moves and follows. `target` names another controller of the clip, or null for none; `parameters: null` changes nothing. `extend` (default true) uses the whole line of a direction instead of only the part between its ends: the target's line when a point follows a direction, the controller's own when a direction follows a point. `weight` is 0 to 100 (default 100). `delete {name, controller?}` deletes one controller or all of the clip and answers `{deleted}`; a controller that followed a deleted one follows nothing.
  - `read {name, frame}` is read only. It answers `{name, frame, controllers: [{name, kind, parameters, target, extend, weight, start, end?}]}`, with `kind` `point` or `direction` and `start` and `end` the model points on that frame before eye blinking, or `broken` when the Part is not drawn, the pinned vertex is gone or the target is no controller of the clip.
  - `move {name, controller, frame, x, y}` solves the controller's parameters, starting from the values the frame draws with before eye blinking, which plays on top of the keys, so that a point controller reaches (x, y) or a direction controller turns toward it. It keys them on `frame` in one undo step and answers `{reached, distance, keys: [{parameter, axis, value}]}`. Values stay inside each parameter's range, so a point out of reach is keyed as near as the range allows, with `reached` false. The frame must then draw the keyed value from the current parameter values, or the move is refused: for example a Stepped lane on a later key, which draws the key before, a Multiplicative lane on a value other than 1, or an Additive lane on a value other than 0. A frozen lane is refused too while freeze is on.
  - `bake {name, from?, to?}` makes every controller with a target follow it on each integer frame from `from` to `to` (default the whole clip, at most 2000 frames), in list order, keys the solved values blended by the weight with the frame's values so far (the clip's values, changed by the controllers before it in the list), and then removes the clip's controllers, all in one undo step. It answers `{from, to, keys, unreached: [{controller, frame, distance}]}`. A lane that would not draw its key refuses the bake as it refuses `move`. Refused while the clip's freeze is on, and when no controller follows another controller of the clip.
  - The clip keeps them as `controllers: [{name, start: {node, vertices, weights}, end?, parameters, target?, extend?, weight?}]`, checked by `animation_set`. INP export leaves them out, merging a model renumbers their parameters and Parts, and deleting a parameter takes it off every controller.
- `animation_form_key`, `animation_form_delete`, `animation_form_time`, `animation_form_read`
  - Form animation of an INX animation: form edits move the vertices of one node on keyed frames without parameters, and change no model data. `key {name, node, mode?, frame, offsets? | reset? | delta?, vertices?, space?}` keys the form edit of `node` on `frame`, adding it when the node has none in that adjustment `mode`. Mode 1 (default) adds the offsets to the node's own vertices before its parents deform it, as a deform binding does, so the edit follows the parent's angle and glue applies after it. Mode 2 adds them in model coordinates after all deformation and glue, so the edit ignores the parent's angle; only a drawn Part or Mask takes it. A Part or Mask has one offset per mesh vertex, a GridDeformer one per grid point and a PathDeformer one per control point; other nodes are refused, and so is mode 2 for a Part inside a Composite or DynamicComposite, which draws its children into a surface warped before glue. The key holds `offsets`, or zeros with `reset` (Reset Form Edit), or else the offsets the form edit shows on that frame moved by `delta: [dx, dy]` at the listed `vertices` (default all); `delta` goes with neither `offsets` nor `reset`, and `vertices` only with `delta`. With `space: "model"` (default `"form"`, and only with `delta`) the delta is how far the vertices move in the picture the session shows, as a viewport drag gives it: mode 2 takes it as it is, and mode 1 turns it into the node's own offsets by moving each vertex one unit along each axis of its offsets and measuring where it goes in the picture, so a frame of the clip that shows form frame `frame` must be sought first, and the node must be a Part drawn outside composites. That is exact under parents that move, turn and scale, and a first-order answer through a deformer that bends the mesh. Key frames are form frames: the timing curve below maps each frame of the clip to one, and without it they are the clip's frames. It answers `{node, mode, frame, keys}`. A key that changes nothing writes no history. Like every edit it ends the preview, so seek again to see it.
  - `delete {name, node?, mode?, frame?}` deletes the form edits of `node` (in `mode` only, if given), or all of the clip without `node`; with `frame`, only their key on that frame. A form edit left without keys goes. It answers `{deleted}`, the number of keys, and is refused when nothing matches.
  - `time {name, keyframes, interpolation?}` (`keyframes` required, null to clear) sets the form timing curve (the manual's time remapping): each key `{frame, value, tension?}` maps a frame of the clip to the form frame shown there, sampled as a lane is (Linear by default). Null or no keys clears it.
  - `read {name, frame}` is read only: `{name, frame, formFrame, forms: [{node, mode, keys, offsets}]}`, with `broken` in place of `offsets` when the node is gone, has no vertices to edit, takes no form edit in that mode (mode 2 on a node that is no drawn Part or Mask, or that sits inside a composite), or no longer has as many vertices as the form edit holds. Such a form edit is not drawn.
  - Before its first key and after its last a form edit holds that key, and between keys each offset moves linearly. The preview, renders, onion skins and exports of the clip draw the form edits; physics does not see them, and a PathDeformer whose shape physics drives while physics runs loses its mode 1 edit. The clip keeps them as `forms: [{node, mode, keys: [{frame, offsets}]}]` and `formTime: {interpolation?, keyframes}`, checked by `animation_set`. INP export leaves both out, and merging a model renumbers the nodes.
- `object_export_csv`
  - Writes the UUID, type and name of every node, parameter and parameter group to a `.csv` file without changing the session: the header `uuid,type,name`, then one row for each object, nodes first in document order, then parameters and groups in list order. UTF-8, comma separated, a cell in double quotes when it holds a comma, a quote, a line break, a semicolon or a tab.
  - `path` is required and ends in `.csv`. `overwrite` replaces a file that exists. The answer holds `path`, `rows` and `bytes`.
  - A type or a name that starts with `=`, `+`, `-`, `@`, a tab, a carriage return or an apostrophe is written after an apostrophe, so that a spreadsheet does not take it for a formula. The guard is for a reader that splits a row on the comma; a spreadsheet set to split on the semicolon alone can still find a cell that starts with one of these inside a name.
  - The format is Soul2D's own. It was not checked against a CSV that Cubism Editor writes, whose columns the manual does not list in full. An INX object has no ID string and no description, so the file holds names only.
  - The same text is the resource `resource://soul2d/object/names-csv`, as `{csv, rows}`.
- `object_import_csv`
  - Renames nodes, parameters and parameter groups from a CSV of `object_export_csv` that somebody edited, in one undo step. The text is the file `path` or the argument `csv`, exactly one of them, at most 1 MiB and 4096 rows after the header.
  - A row names its object by `uuid` and gives it the name of the row. One leading apostrophe of a type or a name is taken off. An object without a row keeps its name. Every name is judged against the finished document, so two rows may swap names.
  - The whole call is refused when a `uuid` names nothing or has two rows, when the `type` of a row is not the type of its object, when a name would be empty, taken or longer than 4096 characters, or when the text is not UTF-8.
  - The answer holds `renamed` (`kind`, `uuid`, `from`, `to`), `unchanged` and `applied`. With `dryRun`, or when no name differs, nothing is written. No object is created and no value is changed.
- `rig_bake_mmd_motion`
  - Samples a `.vmd` motion at the VMD frames in `frames` (1 to 256 strictly increasing whole numbers) and writes sample `i` into keypoint `i` along `axis` of `parameter` (`other` fixes the other axis of a 2D parameter). The parameter must already have enough keypoints.
  - `animation` creates or replaces an animation of that name with one linear lane that moves the parameter through those keypoints, one animation frame per VMD frame.
  - `morphs` (needs `animation`) maps MMD morph names to parameter axes: `{"<morph name>": {"parameter": <id>, "axis": 0 | 1, "range": [<value at weight 0>, <value at weight 1>]}}`, at most 64 entries. `axis` defaults to 0 and `range` to that axis's minimum and maximum. Each mapped morph the file holds becomes one more linear lane of the animation, sampled at the same frames (morph keys carry no curve; the weight is linear between keys). A mapped name the file lacks is reported with `inFile: false`. The parameter the motion is baked on cannot be a morph target, and two morphs cannot share a parameter axis.
  - `boneTable` and `storeBoneTable` work as in `rig_apply_mmd_pose`. `translationScale` and `dryRun` too.
  - The result reports one entry per sampled frame, the animation, the morph values per frame (`morphs`), counts of unmapped bones and morphs, and which bone table applied. One undo step. Details: `doc/mmd-motion.md`.

## Screenshot Tools

- `ViewCommand_CaptureLiveScreenshot`
  - Returns the current live viewport as direct MCP `image/png` content.
  - `width` and `height` default to 640x480 and `bounds` to the scene bounds; `context.parameters` with `context.parameterValue` renders one parameter value without changing the session. The result `_meta` carries `bounds`, `width`, `height` and `worldCapture`.
  - Optional `overlayObjects` is the D debug overlay. The Rust host refuses a non-empty list with `overlayObjects are not supported in Rust Core mode`; the shapes below are kept as the D contract.
  - Use `{ "uuid": <Node UUID>, "overlay": "bounds" }` to draw the node's combined bounds.
  - Use `{ "uuid": <Node UUID>, "overlay": "mesh" }` to draw a drawable node's mesh lines and points.
  - The compact form `{ "<Node UUID>": "bounds" }` or `{ "<Node UUID>": "mesh" }` is also accepted.

D example (refused by the Rust host):

```json
{
  "overlayObjects": [
    { "uuid": 123456, "overlay": "bounds" },
    { "uuid": 789012, "overlay": "mesh" }
  ]
}
```

## Export Tools

These write files from the open INP/INX document and change nothing in the session (no history entry, no
`modified`, no `revision`). They are not available for native Live2D documents (CMO3/CAN3); use `render` there.
Paths are caller-supplied; the parent directory must already exist, an existing file is kept unless
`overwrite` is true, files are published atomically (where the file system has no hard links a new file is
created and then written, so a reader can see it incomplete), and the open project file is never overwritten.
A symlinked name is resolved first and the files go where the link points. Image export keeps that resolved filename intact even if its extension differs from the requested image format. `export_frames` and a video
export seek the animation and reset physics; a physics pose stepped before the export is not kept. When
`export_frames` fails part way, the frames it created are removed and files it replaced stay replaced. The
editor's Tools panel runs these same operations.

`render` with `path`, `animation_onion` with `path`, and `view_screenshot` also refuse the resolved open
project destination, including a symlink to it. Other output files retain their existing overwrite rules.
Pathless or null-path render and onion requests return PNG content without writing. Motion3 and the
three Live2D runtime JSON exports protect the open document path even if its source file was removed
externally; they still refuse any existing destination and never overwrite it.

- `export_image`
  - Renders one camera to an image: `path` (the extension is forced to the format's: `.png`, `.tga` or `.jpeg`), `format`
    (`png` default, `tga`, `jpeg`), `camera` (name; the first camera by default), `transparency`
    (ignored for `jpeg`), `background` (`[r, g, b]`, 0 to 255, used when not transparent; black by
    default), `overwrite`.
  - Returns `{ path, width, height }`. The size is the camera viewport, rounded to even numbers.
  - TGA image and frame outputs are uncompressed so readers can decode each scanline independently.
    They can be larger than compressed TGA files.
- `export_frames`
  - Renders the animation as numbered stills: `path` must end in `.png` or `.tga`, and frame N is written
    to `<stem>-N.<ext>` in the same directory, N from 1. Options: `animation` (first by default), `camera`,
    `loops`, `framerate` (1 to 240; the animation's own by default), `snap_to_framerate`, `transparency`,
    `background`, `overwrite`.
  - The clip's image tracks (`animation_image_add`) are drawn behind the model, or in front of it with `front`,
    the top track over the lower ones, and the model is placed by its group (`animation_group_set` with
    `model`); this holds for `export_gif` and `export_video_start` too. A hidden track or group is not drawn. A
    track whose file is gone, not a local absolute path or not an image, or past the first 8 shown, is left out and
    named in the reply's `imagesLeftOut`; the request checks each file, and the render decodes them once. `export_image` draws only the model.
  - Runs to completion before it replies and returns `{ frames, first, last }`, plus `imagesLeftOut` when an image
    track was left out. At most 100000 frames, and at
    most 1,000,000,000 pixels in total (frames x width x height, about 480 frames of 1080p) because the call
    cannot be cancelled; use `export_video_start` with an image codec (`png`, `tga`) for larger runs.
- `export_gif`
  - Renders the animation as one animated GIF: `path` (the extension is forced to `.gif`), the options of
    `export_frames`, `colors` (2 to 256, default 256: the palette of each frame, one slot of which is the
    transparent colour when `transparency` is true; no dithering) and `repeat` (0 to 65535: how many times the clip
    plays again after the first time; absent loops forever, 0 plays once).
  - A GIF shows a frame for whole hundredths of a second, so the rate is rounded: `delay` in the reply is that
    time (8 fps becomes 13, 30 fps becomes 3). The shortest delay is 2, because browsers show a frame of 0 or 1
    hundredth for a tenth of a second, so a clip faster than 50 fps is rendered at 50 fps and keeps its length
    (`frames` in the reply counts the frames written). Texels below half opacity become
    transparent. The whole file is built in memory before it is written.
  - Runs to completion before it replies and returns `{ path, frames, width, height, delay }`, plus `imagesLeftOut`. Same limits as
    `export_frames`, and at most 65535 pixels a side.
- `depth_draw_export_png`
  - `{ dir, manifest?, overwrite? }` writes the retained DepthDraw pixels to one grey PNG per layer and a manifest. The default manifest is `dir/manifest.json`; an explicit relative manifest is relative to the working directory. An external manifest parent must already exist. Open documents, DepthDraw source files and duplicate output destinations are protected from replacement. Case-equivalent output names are treated as duplicates on every platform.
  - ACP requests require exact, single-use host approval for the directory and complete PNG/manifest output list. Preparation writes nothing. DepthDraw changes, document changes and changed input/output path bindings expire approval. Execution uses the captured bytes. A failed batch removes newly created files, but directories and earlier replacements may remain.
- `export_collect`
  - `{ dir, overwrite? }` writes a copy of the open INX document into `dir` and copies every image its
    DepthDraw layers name (`sourcePath`) to `dir/assets/`. In the copy each layer names its image by a path
    relative to the copied document (`assets/back.png`); two different sources with one file name become
    `assets/back.png` and `assets/back-2.png`. The file is named after the open document, `model.inx` when it
    has no file. Returns `{ path, files }`.
  - A relative `sourcePath` names a file beside the open document when one is there, and otherwise a file in the
    working directory, where `depth_draw_load_png` took it from. `depth_draw_load_png`, `depth_draw_manifest_get`
    and `depth_draw_diagnostics` resolve it the same way, so the collected copy finds its images from any
    working directory. A source that is missing refuses the whole copy. Names that differ only in case count as
    one name, as on a case-insensitive file system. Existing files are
    only replaced with `overwrite`; the open document and the sources themselves are never written. Nothing
    in the open document changes. CMO3 and CAN3 are refused: their links are not rewritten.
  - ACP calls require host approval for the resolved directory, source-to-destination copy list, final INX file and overwrite choice. Preparation creates no directories or files. The approved INX bytes and copy mapping are used at execution. A changed document, source alias or output parent expires approval; missing sources also expire an existing grant. Source file contents are read at execution. Failure removes newly created files, but can leave directories and files already replaced under `overwrite`. Output file symlinks are refused; parent directory symlinks are resolved before approval.
- `export_video_probe` / `export_video_probe_status`
  - Both take `{}`. Probe starts one background worker using the host-configured encoder and fixed
    `-codecs` arguments. A second running probe is refused. Status is `idle`, `running`, `finished`
    with `codecs:[{tag,name}]`, or `failed` with `error`; the captured `program` identifies the result.
    The same status/result is readable at `resource://soul2d/export/video-codecs` for every document format.
    Existing five-second timeout, bounded stdout and direct-child kill/wait behavior are retained.
    No new cancellation or request-supplied executable/options; session drop does not join the worker.
- `export_video_command`
  - Takes the same options as `export_video_start`, but only plans the command. Returns `program`,
    `args`, `directory`, `path`, `overwrite`, `frames`, `width`, `height`, `fps`, and omission warnings.
    No process, render, output write or video job is created. The last successful result is available
    at `resource://soul2d/export/video-command`; failure keeps it and replacing the document clears it.
  - Shares launch planning, including host program/options, presets/audio and `-n`/`-y`. Requires a
    resolvable parent and protects the open document, but an ordinary existing destination is previewable.
    Motion3 preview is refused; an active video job does not block planning. A preview neither pins
    executable resolution nor grants later execution consent. Actual video start keeps its ACP approval.
  - Both GUI controls use these Session operations and display agent-started results. GUI Show applies
    its local Extra ffmpeg options through the existing host setter, like Run; later agent starts use them.
    Periodic GUI reads of probe status and the cached command do not rebuild the document snapshot.
    Polling also discovers probes started through MCP; unchanged status still permits command-cache updates.
    Resource polling never changes that setting. Probe and cached preview are session-state effects,
    leaving document revision/history/modified unchanged.
- `export_video_start`
  - Takes the options of `export_frames` plus `path` (the output file) and `codec` (an ffmpeg encoder tag,
    `auto` by default). A request cannot carry `ffmpeg_options`: extra encoder arguments are a host setting
    (the editor's "Extra ffmpeg options" field) because any token that is not an option value would name
    another input or output file. `path` must not contain `%`; ffmpeg gets the resolved absolute path. It
    plans the video, then encodes on a background thread with `ffmpeg` found on `PATH` (arguments only, no
    shell) and returns `{ job, path, frames, width, height, fps }` (plus `audioLeftOut` and `imagesLeftOut`) once
    the clip's image tracks are checked. `path` is the output as the status
    reports it; `width` and `height` are the camera's size.
  - `preset` picks the encoder in place of `codec` (giving both is refused) and replaces the extension of
    `path`. `mp4` is H.264 (`libx264`, CRF 18, fast start) and refuses `transparency`. `webm` is VP9
    (`libvpx-vp9`, CRF 30) and keeps alpha under `transparency`. `mov` is ProRes 4444 with alpha under
    `transparency` and ProRes 422 HQ without it. H.264 and ProRes need an even size, so `mp4` and `mov` add a
    row or column of (transparent) black to an odd width or height. The host's extra ffmpeg options follow the
    preset's and can override them; a host `-vf` replaces that padding, so an odd size then fails the job.
  - `audio` names a sound file (any format ffmpeg reads; its first audio stream is used) to mux in. A relative
    name is resolved against the working directory, like `path`. The sound is padded with silence and ended
    with the video (`-t`), so the video keeps every frame; an `-af` among the host's options replaces the
    padding and then only shortens the sound. The audio codec is the container's default: AAC in MP4 and MOV
    (the `mp4` and `mov` presets), Opus or Vorbis in WebM as the ffmpeg build has them, and whatever ffmpeg picks
    for other containers. ffmpeg gets the resolved absolute name and may read only local files for it; a
    missing file, a folder and an image codec (`png`, `jpeg`, `tga`) are refused. Without `audio` the clip's audio
    tracks that are not muted (`animation_audio_add`) are used, each a WAV file checked again and started
    where its frame first shows in the video; two or more are mixed at full volume in a filter graph, which a
    host `-af` cannot be combined with (a host `-af` also drops a single track's delay). A track that fails the
    check (only the header is read), is not absolute, never shows in the video or is past the first 32 is left
    out and named in the reply's `audioLeftOut`. An image codec, GIF, APNG, WebP, still-image and Y4M outputs
    leave the tracks out. A video with neither has no sound track (`-an`).
  - One encode runs at a time. Missing `ffmpeg` or an encoder failure shows up in the job status; `error`
    carries at most the last 4096 bytes of ffmpeg's error output. An encoder that has not exited 120 seconds
    after the last frame is killed and the job fails.
  - A failed or cancelled encode can leave a partial output file. With `overwrite`, ffmpeg (`-y`) has
    already replaced an existing file when the encode starts; without it an existing file fails the job.
- `export_video_status`
  - `{ job }` returns `{ job, state, path, framesDone, framesTotal }` and never waits. `state` is `running`,
    `finished` (adds `frames`), `failed` (adds `error`) or `cancelled` (adds `partialFileKept`). The last
    three are final. The server keeps the last 8 jobs. Status and cancel also work while a native Live2D
    document is open, since jobs outlive `open`.
- `export_video_cancel`
  - `{ job }` asks the encode to stop: ffmpeg is closed and the partial file is kept. The state turns
    `cancelled` when the encoder has stopped; poll `export_video_status`.

## Texture Tools

- `texture_get`
  - `{ slot }` reads the stored bytes (base64 PNG or TGA) and decoded size of one texture slot.
- `texture_set` (op `texture.set`)
  - Replaces the pixels of one existing texture slot with an image, for every Part that uses the slot. INP/INX
    documents only. One undo step; undo restores the old bytes.
  - `{ slot, path | png_base64, premultiplied?, allowResize? }`. Give exactly one image source: `path` (a PNG, TGA or
    baseline JPEG file, read like the other image importers: at most 256 MiB) or `png_base64` (a PNG, at most 7 MiB
    of base64 text, below the 8 MiB MCP HTTP body limit; use `path` for bigger images). The image is limited to 8192 pixels a
    side and 16 megapixels. The slot must exist; the tool does not add slots.
  - Stored textures are premultiplied. The input is taken as straight alpha and premultiplied
    (`rgb * alpha / 255`) unless `premultiplied` is true, which stores it as given.
  - An image of another size than the old texture is refused with both sizes named unless `allowResize` is true.
    The Part meshes and UVs are normalised and are not rescaled, so a Part drawn from a resized texture keeps its
    mesh but shows the new pixels stretched over it; a quad sized from the texture would look scaled. Resize only
    when that is what you want.
  - The slot keeps its format (PNG or TGA). Returns `{ slot, width, height, changed }` (the size of the stored
    texture). When the decoded pixels equal the old ones, `changed` is false and no history entry is made.
  - Masks are not a separate input: build the RGBA image (alpha as the mask) and pass that.
  - The tool is refused on native Live2D documents and while a deform refresh is pending.
- `texture_export` (op `texture.export`)
  - `{ directory, overwrite? }` writes every INX texture slot as `slot-N.png` in an existing directory. Each PNG
    is the stored premultiplied pixels. The document does not change. An existing `slot-N.png` is refused before
    any write unless `overwrite` is true. A failed write can leave files already written.
  - `resource://soul2d/textures/{slot}` returns the same JSON as `texture_get` for an INP/INX document.
- `texture_replace` (op `texture.replace`)
  - `{ directory, allowResize? }` reads `slot-N.png` back into that slot, in the slot's existing PNG or TGA
    format, as one undo step for every slot whose pixels differ. A missing file leaves the slot and is listed
    in `missing`. Identical pixels record no undo step.
  - The PNG is stored as decoded, with no second premultiply. `texture_export` wrote stored premultiplied
    pixels, so a straight-alpha file belongs in `texture_set`. A different size refuses the whole batch unless
    `allowResize` is true. The Part meshes and UVs are not rescaled.
  - Refused on native Live2D documents, same as `texture_set`.

## Part Split Tools

`snapshot.partSplit`, `split_plan`, and `resource://soul2d/part-split/plan` include `applied`.
It is true when the open draft matches an application on the current history branch with a surviving original Part.
Renames and unrelated edits preserve it; deleting all original Parts clears it without resurrection on ID reuse.
Save preserves the in-session record, while new, open, and import reset it. The record is not serialized.
This field is advisory: `split_apply` still appends on every accepted call as one undo step.
The INP/INX Outline **Part split review** panel uses it to block repeated application of the same plan.
See [Part split](part-split.md#editor-review) for review controls and freshness behavior.
The editor's ACP endpoint refuses `split_apply` and directs the person to **Apply reviewed split**.
This restriction uses the shared normalized-operation guard; ordinary MCP clients retain their existing apply path.
It is specific to split application, not a general permission gate for MCP mutations. See [ACP entry](part-split.md#acp-entry).

- `split_open` (op `split.open`)
  - `{ path }` opens the draft source of a part split and clears its parts. `path` is a PNG, TGA or baseline JPEG
    file, or a `.psd`. A PSD is flattened by compositing its visible layers (the merged image stored in the file is
    not read); a PSD without layers is refused. Every source is limited to 8192 pixels a side and 16 megapixels.
    Returns `{ width, height, revision }`. A refusal names the file, not its directory.
  - The other `split_*` tools and the plan format are described in `doc/part-split.md`.

- `split_components` (op `split.components`) reads the loaded source's four-connected alpha islands, without
  changing the draft or document. `{threshold?: 0..255, offset?: 0..4294967295}` returns at most 32
  `{seed, pixels, bbox}` entries, exact `totalComponents`, and `nextOffset` or null. The default threshold is 0;
  pixels must have alpha strictly above it. Every page rescans the source; keep the same source and threshold.

- `split_mask_inspect` (op `split.mask.inspect`) accepts `{path}` to a source-sized grayscale-8 PNG without alpha.
  It reads only and returns `{step: {op:"raster", path, crc32, mode:"add"}, width, height}`. Use this step in
  `split_part_set`; source alpha is preserved separately. Every evaluation and replay reloads the mask and checks
  decoded coverage against the CRC. Retain the file. This does not run a provider or grant image-upload consent.

## Binding Selection Tools

- `binding_remove`
  - Removes only the bindings listed in `bindings` from `parameter`; answers `{ parameter, removed }`.
  - Example: `{ "parameter": 123, "bindings": [{ "node": 456, "param_name": "deform" }] }`.
- `binding_set_interpolation`
  - Changes interpolation (`mode`: `Nearest`, `Linear` or `Cubic`) only for the bindings listed in `bindings`; answers `{ parameter, mode, changed }`.
  - Binding resource pseudo-UUIDs are inspection-only and must not be used as tool input.
- A `{ "target", "name" }` descriptor or a `context.bindings` array is refused; the resource URI keeps the `target` and `name` query names.

## HTTP Transport

`soul2d-core --mcp-port` and the editor's MCP listener answer `POST /mcp` only. The request head is checked before any body byte is read: route, method, `Host` (loopback names only on a loopback listener), `Origin` (loopback only when present) and, when a bearer token is configured, `Authorization: Bearer <token>`. A refused request is answered (`401`, `403`, `404`, `405`) without its body being read. A non-loopback `--mcp-host` or `MCP.Host` is refused at startup unless `SOUL2D_MCP_BEARER` is set. Without `SOUL2D_MCP_BEARER`, `soul2d-core --mcp-port` generates a random token and writes it, readable by the owner only, with the address it listens on, to `mcp-<port>.token` in the config directory (on Windows, or with `INOCHI_CONFIG_PATH`, the file is only as private as that directory); when no token is given, `njc` uses the file's token only for that address (or `localhost`, which it then replaces by that address). `--no-auth` serves a loopback host without a token instead. The editor's listener generates a token only with `MCP.authEnabled` and shows it in its settings rather than writing a file. Tool arguments that a tool's schema does not declare are refused when the schema closes its properties, and on every tool when the key starts with `_` (the prefix of internal request keys such as `_suffix`).

## Default Form Lock

- `default_form_lock_set`
  - `{ "locked": bool }` locks or unlocks the default forms of an INX document (Cubism "Lock default forms", issue #186). The Parameters panel's "Lock default forms" check box sets the same flag. Native Live2D documents refuse it.
  - While locked, any edit that changes or unsets a keyform set at a parameter's default value is refused and names the parameter, the node and the binding. The check runs on the model each edit commits, so it covers the tools that write bindings. Values are compared as numbers, so `0` in a file and `0.0` written back are the same.
  - Only a keyform that exists is locked: a new binding, a default key set for the first time, a removed binding or parameter and an edit of the parameter's range or default are allowed. Inochi2D fills a key that is not set from the keys that are, so an edit at another keypoint can still move an unset default key. A default between keypoints (within f32 precision) has no keyform; a 1D parameter is evaluated on its one Y point whatever its Y default.
  - Geometry edits pass. When an edit changes a node's own data (`mesh_edit`, `deformer_define_grid`, `deformer_define_vertices`, `depth_set`, ...), that node's `deform` bindings are re-expressed on the new geometry and are not compared; this is judged on the bound node only, so a rig edit that changes a bone or an ancestor and refreshes a set default deform of its target is refused when the refreshed value differs. Node meshes and base transforms are not keyforms.
  - When only the axis points change (`parameter_flip`, or `parameter_set_axes` adding or dropping points), the keyform at the old default key is compared with the one at the new default key. A flip that brings another keyform to the default is refused, and so is an axis edit that leaves the default between keypoints, since that drops its keyform. The parameter's range and default are compared as numbers too. A range or default edit, and an axis edit that brings a default from between keypoints onto one, are not compared, so they can change which keyform the default uses.
  - A Depth Bone refresh too large for one request is checked on its first part; `deform_continue` and `save` finish the rest of an edit already accepted without a check.
  - Undo and redo are not edits and are never refused. While a Depth Bone refresh is pending the lock cannot be changed; finish it with `deform_continue` first.
  - Not saved and not an undo step; a new or opened document starts unlocked. `snapshot` and `resource://soul2d/document/state` read it as `defaultFormLocked`.

## Views

- `view_list`, `view_add`, `view_select {id}`, `view_remove {id}`, `view_sync_set {synchronized}` (Cubism multiview,
  issue #201).
  - A view keeps its own parameter values. The active view's values are the ones `value_set`, `live2d_parameter_set`,
    `render`, `snapshot.values` and the other tools use. A document starts with view 1; a new or opened document
    starts over with one view.
  - `view_add` makes a view at the default forms and activates it. At most 16 views. Its id is the highest id plus
    one, so the id of a removed highest view is given again.
  - `view_select` keeps the values of the view it leaves with that view. The values of the view it enters come back
    with removed parameters dropped, and a value outside its parameter's current range falls back to the default.
    The Depth Bone pose preview the view had comes back with it, or is made again as `value_set` makes it when the
    document was edited since. A switch ends the random pose, motion and physics previews. While synchronized, a
    switch changes only the active view.
  - `view_remove` refuses the last view; removing the active view activates the next one, or the previous one for
    the last.
  - `view_sync_set {synchronized: true}` (Cubism "Synchronize parameter values across tabs") makes a switch keep the
    values, so every view shows the same ones. Turning it off leaves every view with those values and their pose
    preview; sending `false` while it is off changes nothing.
  - Not saved, not an undo step, and the revision does not move. INX and native CMO3 documents; CAN3 refuses.
    `snapshot` and `resource://soul2d/document/state` read the list as `views`.
  - The editor shows the views as tabs above the viewport. Each tab keeps its own display position and zoom, and
    "Same display position" copies the active one to every tab. Views side by side are not available.

## History Tools

- `history_limit_set`
  - Sets how many undo states the session retains (integer `limit`, 2 to 1000, default 100); the editor applies the stored `MaxUndoHistory` at startup and when the Settings value changes.
  - Shrinking drops the oldest states first and then the newest redo states, never the current one, and bumps the revision when anything was dropped. The limit survives `new` and `open`.

## Naming Rules

- Resource descriptions should start with `Read:` or `Explore:` where practical.
- Tool descriptions should start with `Action:`.
- Do not describe a resource endpoint as a tool.
- Do not use `list` alone for both definition lists and instance lists without qualification.
