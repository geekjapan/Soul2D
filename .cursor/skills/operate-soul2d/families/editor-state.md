# Editor state

Status: verification varies by tool. The recorded window checks below cover grid, guides, Solo, Onion Slice,
Lightweight playback and workspaces over editor stdin MCP. Individual panel visibility has focused Rust tests
and the dedicated production-editor MCP verifier below. These checks do not establish complete GUI or real ACP-agent coverage.

What the editor window holds outside the document: node selection, armed parameter, edit tool, camera, selection overlays, RGB background, canvas grid, guide lines, Solo view, Onion Slice and Trajectories, Lightweight playback, the framerate lock, saved workspaces, the
crash-recovery snapshots, nonsecret coding-agent lifecycle observations and the editor settings. The view tools do not edit the document, add history or raise `revision`. `editor_address_dialog` Copy also queues a clipboard write. The others reach further:
`editor_recovery_restore` replaces the document like `open` (so `revision` moves), and `editor_panel_set`, `editor_settings_set` and the `editor_workspace_*` tools write the settings file without touching
the document. A recovery snapshot cannot be deleted from here: only the offer window's Delete button does that. They need `soul2d-editor` (its MCP
listener, stdin MCP, command palette or agent panel). `soul2d-core` lists them and refuses each call.

## Sequence

`editor_quit {}` requests non-discarding editor exit through an identified HTTP/stdio call. Headless, notifications and internal command-browser calls refuse. Acceptance returns `quit {state:"accepted",attempt,policy}`; it does not prove exit. Read editor state `quit {state,attempt?,reason?,origin?}` if the editor remains available. Only the exact successful reply's complete local framing/flush can permit exit, followed by a final safety check. New work, replacement, failed/lost delivery or the five-second reply deadline cancel it; resolve blockers and send a fresh request. A pending duplicate refuses. Local flush does not prove peer consumption.

ACP Quit requires an actually transmitted uncancelled prompt and human lifecycle review of the original authenticated request. `awaiting_human` expires after two minutes or when that prompt ends. **Allow once** consumes that original call only; **Deny changes this turn** blocks covered protected operations and Quit for that prompt. After acceptance, `waiting_for_reply_and_turn`/`waiting_for_turn` require the identified reply and matching explicit uncancelled `end_turn` within two minutes of admission. Permissions, Cancel, Stop, new sessions, replacement, new work or stale document/drafts invalidate it. Only the same owned client can be released; a fresh full safety check then decides Close. If release succeeds but the final check refuses, the agent is stopped and the editor stays open. This does not authorize Save/Discard, answer dialogs or carry consent into another call/turn. Retry requires a fresh request and human consent.

Save the document, apply/cancel authored drafts, finish mesh/depth and PSD-depth editors, stop playback and, for external Quit, explicitly Stop any retained ACP client first. ACP-origin Quit releases only its own initiating client after the gates above. Active export/probe/deformation work, an unapplied split plan or waiting human decision also refuse. Ordinary filter/path text and recovery offers alone do not block. No tool answers Save/Discard or restore decisions. Genuine EOF uses the nondiscarding guard; transport failure is not EOF, and automatic EOF exit stays disarmed after an accepted Quit even if cancelled. A blocked stdout worker can remain unavailable; continue through the native UI or an already-enabled HTTP listener. Native human close and existing App cleanup remain independent.

1. Explore: read `resource://soul2d/editor/state`. `editor: false` means this host has no window: stop here and work through the document tools instead.
2. Read: from the same resource take `selection.nodes`, `armed`, `tool {mode, tool, offered}`, `camera`, `viewCameras`, `parameterPanel`, `grid`, `guides`, `solo`, `animationOverlays`, `playback`, `panels`, `windows` (its `unsaved`, `restorePrompt` and `can3Delete` are the dialogs waiting for the user; while one is non-null, document-replacing calls are refused and no tool answers it), `physicsPreview`, `workspaces`, `drafts {panels, tools}`.
3. Act: `editor_selection_set {nodes}`, `editor_armed_set {parameter}`, then read the state again before `editor_tool_set {tool}`: the edit mode follows the selection and the armed
   parameter one drawn frame later. `editor_camera_fit {}` or `{nodes}` and `editor_camera_set {centre?, zoom?, mirror?}` move or mirror the view;
   `editor_grid_set {show?, spacing?, colour?}` shows the canvas grid; `editor_guides_set {open?, show?, snap?, colour?, lines?}` sets the guide lines; `editor_solo_set {show?, colour?}` turns the Solo view on; `editor_playback_set {lightweight?, snapToFramerate?}` freezes the parameter panels, or plays INX animations in whole frames, while the window plays;
   `editor_viewport_set {vertices?, bounds?, orientation?, background?}` changes selection overlays or the opaque RGB background. `[0,0,0]` resets the background to black. These values live in `viewport` in the state resource.
   `editor_panel_set {panel, visible}` shows or hides `outline` or `timeline` independently through the same operation as View > Panels.
   `editor_workspace_save {name}`, `editor_workspace_apply {name}` and `editor_workspace_delete {name}` keep and restore the panel layout.
4. Confirm: every tool answers the state after the change; read the resource again when a frame has to pass (tool mode, `camera.viewSize`).
5. Recovery: `editor_recovery_list {}` for the ids, then `editor_recovery_restore {id}` (replaces the document). `editor_recovery_keep {}` dismisses only this window's startup offer, retaining the files for the next startup. Read `windows.recoveryOffer {pending,drawEligible}`: pending means rows remain; drawEligible is false while an unsaved dialog or Restore Autosave prompt hides them, and does not prove an unobscured window. Keep may dismiss a hidden offer without answering that dialog. Restore is refused in the states the rows below
   name; ask the user to answer the dialog in the editor, then call again. To delete a snapshot, ask the user to press Delete in the offer window.
6. Settings: `editor_settings_get {}` to see the keys and `writable`, then `editor_settings_set {key, value}` for a writable one. Shortcuts: `editor_shortcut_set {command, shortcut}` with the `spec` form from the same listing, `null` to unbind.

Read `codingAgent` from the same resource for exactly seven booleans:

- `enabled`: Coding-agent opt-in, independent of panel visibility.
- `has_client`: The panel owns a run/client; this does not prove OS process liveness.
- `closed`: The retained run was marked closed. It can coexist with `has_client:true`.
- `ready`: Valid ACP initialization processed and client not disconnected; session creation is separate.
- `turn_active`: Prompt waiting for session creation or its response; not Quit's narrower initiating-prompt qualification.
- `cancelling`: Processed conversation cancellation state.
- `awaiting_permission`: Processed ACP permission queue is nonempty, excluding host operation approval.

The serving view observes existing processed owner state. ACP captures after the frame's second lifecycle pump; ordinary HTTP/stdio can observe the earlier phase. A resumed approved request uses its new serving view; multiple calls in one view share that observation. Reads add no lifecycle processing or actions, although existing editor-state reconciliation remains. Events arriving afterward wait for an existing pump. These values do not prove wire freshness, process health or cross-transport atomicity. Headless hosts omit `codingAgent` entirely. The input-derived `prompt_present` flag, prompts, briefings, commands, workdirs, credentials, endpoint/session identifiers, permission contents, transcripts and error/status text are excluded. No new Start/Stop/Send/session/permission tool is provided. Lifecycle verification is separate from real-agent acceptance.

Verify individual panels with `python3 tools/verify-editor-panel-state.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It creates its own INX and settings directory, checks both panel setters,
repeat calls, invalid-input refusals, unchanged document state, persistence after restarting the editor, and
headless refusal. It does not drive native UI controls; compare View > Panels with the tool in a separate GUI check.

Verify camera and Mirror View with `python3 tools/verify-editor-camera.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It checks partial and combined updates, refusals, unchanged document state,
headless refusal and the session-only restart. Native toolbar parity is a separate GUI check.

Verify per-view cameras with `python3 tools/verify-editor-view-cameras.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It exercises view transitions, Same display position,
ID reuse, document replacement, session restart, CMO3 support and CAN3/headless refusal. Rust egui tests cover
the button's shared request and transitions without a drawn frame. Native GUI, GPU and real-agent acceptance remain separate.

Verify the INX Parameters panel with `python3 tools/verify-editor-parameter-panel.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It checks read/set, partial updates, invalid-input atomicity, selected clip
versus Session preview, stored and Part bookmarks, deletion/replacement/restart, unchanged document/settings
for view operations, and native/headless refusal. Outline stays hidden. Rust tests cover no-frame lifecycle,
GUI filter editing, Show all and clip choice. Native UI and real-agent acceptance remain separate.

Verify selection overlays, background and Part menu sorting with `python3 tools/verify-editor-viewport.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It checks `viewport` defaults, combined and partial updates, RGB black reset,
invalid-input atomicity, unchanged document and settings files, native format boundaries, headless refusal,
replacement retention and session-only restart.
Native controls, GPU output and real ACP-agent workflows require separate checks.

ACP panel, preference, workspace and layout-reset persistence use the host's **Coding agent operation approval** window, outside Outline. The original
`editor_panel_set`, `editor_settings_set`, `editor_shortcut_set`, `editor_workspace_save/apply/delete` or `editor_layout_reset` call waits while the person chooses **Allow once** or **Deny changes this turn**.
Do not retry a waiting call or imitate approval with an agent-supplied permission message. Allow completes the
original call once; Deny blocks covered file writes and document replacement for that prompt. The window names the tool, its exact panel/visibility,
preference key/value, workspace name or Reset Layout target, `file-write` effect and settings destination. It expires after two minutes, cancellation, prompt
completion, disconnection or session replacement. A changed destination refuses the old call. Read-only calls
still work while approval waits. Unknown, read-only or invalid preferences are rejected before prompting.
Notifications with no request ID or a null ID cannot write through this
ACP route. No settings destination means session-only behavior without a file-write prompt. This gate covers
panel and preference setters, all three workspace writes and `editor_layout_reset`.
`save` / `FileCommand_SaveFile` and `render` / `ViewCommand_CaptureLiveScreenshot` with an output path also
require this approval, even without a settings destination. The window names the resolved output file, source
document and revision. A document edit, replacement, draft-state change or resolved-target change refuses
the old call. Dirty drafts block Save. Pathless captures remain usable without approval. Capture uses the
preview at execution time. `new`, `open` / `FileCommand_OpenFile`, `editor_recovery_restore`, `import_psd`, `import_kra`,
`import_inp` / `FileCommand_ImportINP` and `import_folder` / `FileCommand_ImportImageFolder` also require
approval before replacing the document. The prompt distinguishes a new document, an input file, an input
folder and a listed recovery snapshot. Dirty drafts, unsaved changes, a pending document dialog or closing refuse
replacement before prompting. The exact call, resolved input path and current document stamp are checked
again when allowed. Input contents are read at execution time; consent binds the path, not a byte snapshot.
Recovery IDs use the same regular-file listing as Restore and exclude this session's own snapshot.
Single-file ACP output also requires consent for `export_inp`, `export_image`, `export_gif`, `view_screenshot` /
`ViewCommand_SaveScreenshot`, `object_export_csv`, `model_template_export`, `motion3_export` / `Motion3Command_Export`, and pathful
`animation_onion` or Live2D CDI3/physics3/userdata3 export. The prompt names the effective resolved output,
with the writer's forced extension; JPEG always uses `.jpeg`. Export targets refuse the open document.
The exact call, destination and document stamp are checked again before writing. Existing overwrite rules
still apply. Pathless runtime JSON exports and absent/null-path onion requests stay usable without consent;
runtime JSON exports still reject a supplied null path. Fanout exports, named presets/templates, other
writers and real-agent acceptance remain separate work.
Applying a workspace writes its panel visibility settings too. A failed workspace write reports that the
change holds for this session; read the editor state to see it. Do not treat that error as persisted success.

`tools/acp-panel-approval-fixture.py` is a deterministic protocol peer for owned GUI verification. It advertises
HTTP MCP, takes the editor-provided token in memory, and runs scripted calls from its own control directory.
It records no authorization headers. It is a fixture, not a real AI agent.

Verify the address dialog with `python3 tools/verify-editor-address-dialog.py --editor "$TARGET/debug/soul2d-editor"
--core "$TARGET/debug/soul2d-core"`. It checks fixed targets, argument refusals, headless refusal, document/settings
invariance, document replacement, Reset Layout and process restart over production MCP. Rust egui tests inspect
menu/Copy/Close requests and the original address in `CopyText`. The Copy button and tool use the stored address;
keyboard selection copying follows the text field's ordinary behavior. Native UI and OS clipboard delivery remain separate checks.

## Tools

`python3 -I tools/verify-editor-quit.py --self-test` checks the Quit verification client's pipe framing and identity handling. Its FIFO bridge keeps editor stdin open while recording real identified RPC replies; launch only an owned private app through the native UI harness and separately verify natural exit. Keep records private and stop the bridge only after the app exits. Client self-test success is not product or ACP acceptance.

| Tool | Purpose and minimum arguments |
| --- | --- |
| `editor_command_browser_set` | `{open:bool}` opens or closes Command Browser; confirm `windows.commandBrowser`. Edit > Command Browser, its shortcut, native close/Escape and browser-origin calls share the operation. Repeated open retains search/selection/argument draft/list/result; close clears them, reopening starts fresh. Strict required boolean, no extra fields, atomic refusal. Editor only, all formats, including pending drafts and inactive or denied ACP prompts. Retained across document replacement, Reset Layout and listener restart; false on process restart. No document/history/settings write or host approval. Native close suppresses same-draw activation/refresh, not accepted requests. Existing tools/list and tools/call supply discovery/execution; browser input/result read/set and ACP Quit remain outside this visibility tool. |
| `editor_texture_viewer_set` | `{open:bool}` opens or closes Texture Viewer; confirm `windows.textureViewer` from the response and editor state resource. Tools > Texture Viewer and native close share the operation. Repeated open retains Slot/Zoom/images/loading/errors; close discards the viewer and reopen starts unloaded at Slot 0/Zoom 1. Strict required boolean, no extra fields. Editor only, all formats, usable with pending drafts and inactive or denied ACP prompts. Retained across document replacement, Reset Layout and listener restart; false on process restart. Existing selected-Part texture_get loading and pending aggregation stay unchanged; replies never reopen a closed viewer. No document/history/settings write or host approval; ordinary dispatch/Quit cancellation still applies. |
| `editor_quit` | `{}` requests conditional non-discarding exit after exact local reply output and a fresh full safety check. ACP additionally needs exact human lifecycle consent, its initiating uncancelled `end_turn` and confirmed owned-client release. External callers must Stop retained ACP clients first. Read `quit` state if still running; resolve drafts/dialogs/work. No Save/Discard or dialog answers; notifications, headless and internal calls refuse. Five-second reply expiry; ACP review/turn limits are separate two-minute windows. Failed output/new work cancels; fresh request/consent required. Release can leave the agent stopped and editor open. |
| `editor_about_set` | `{open:bool}` opens or closes About; confirm `windows.about` from the editor state resource. Help > About and Close window use the same operation. Repeated values are harmless; missing/null/non-boolean/extra fields refuse atomically. Editor only, all document formats, usable with pending drafts and inactive or denied ACP prompts. Survives document replacement, Reset Layout and listener restart; false on process restart. No document/history/settings write or host approval. |
| `editor_welcome_set` | `{open:bool}` opens or closes Welcome; confirm `windows.welcome` from the editor state resource. Help > Welcome uses the same operation. Opening shows Quick Setup until the quick setup is done and the start page after it; an open window keeps its page. Closing through the tool does not mark the quick setup done (the editor opens the window at start until it is). The window's choices are `editor_settings_set` (DarkMode, lang, UIScale), `new`, `open` and `import_psd`. Repeated values are harmless; missing/null/non-boolean/extra fields refuse atomically. Editor only, all document formats. Survives document replacement, Reset Layout and listener restart. No document/history/settings write or host approval. |
| `editor_address_dialog` | Open with `{action:"open", target:configuration\|online_documentation\|soul2d_live_documentation\|bug_report\|feature_request}`. Copy with `{action:"copy"}`; Close with `{action:"close"}`. Copy/Close forbid target, including null. Read `windows.addressDialog`, null or `{target,title,address}`. Configuration resolves the settings parent; an unavailable path preserves the previous dialog. Copy requires an open dialog and queues its original address, acknowledging egui output rather than OS clipboard delivery. Selectable text edits are discarded. No launcher, arbitrary URL, document/history/settings write. All editor document formats; survives replacement, Reset Layout and listener restart, resets on process restart. |
| `editor_selection_set` | `{nodes?:[uuid, ...], bindings?:[{node, channel}, ...], vertices?:[index, ...]}`, at least one. `nodes` replaces the node selection in this order, the first is the primary node; `[]` clears it; every UUID must exist. `bindings` selects binding rows of the armed parameter at its key (arm first; each pair must be a row of `parameter_get`'s bindings there). `vertices` selects working points of the active mesh edit (needs `tool.mode` vertex or deform; indices below the point count; with `nodes`, the first node must be the edit's node). Omitted fields stay; a refused call changes nothing. INX documents only. |
| `editor_armed_set` | `{parameter: uuid \| null}` arm a parameter or disarm. The value is not moved; the key is the keypoint closest to it, so call `value_set` first to arm at another keypoint. |
| `editor_camera_set` | `{centre?:[x,y], zoom?, mirror?:bool}` at least one. `centre` is the world point in the middle of the view; `zoom` 0.05 to 12, points per world unit. `mirror` sets the toolbar's Mirror View. Omitted fields stay unchanged; mirror alone preserves a pending initial fit. Session only. |
| `editor_camera_fit` | `{nodes?:[uuid, ...]}` centre on the combined bounds of the nodes (Focus Selected); without `nodes` fit the whole model. |
| `editor_camera_sync` | `{}` copies the active view's centre and zoom to every INX/CMO3 view (Same display position). Read `viewCameras {active, views:[{id,camera:{centre,zoom}\|null}]}`; null cameras are unassigned inactive views. New views inherit the outgoing position. Switching views preserves each camera immediately between calls. Mirror View stays global. Document replacement clears the cache; restart resets it. CAN3 reports `viewCameras:null` and refuses sync. No settings or document write. |
| `editor_parameters_set` | `{filter?:string, animation?:name, bookmark?:null\|{labelIndex:index}\|{part:uuid,point:[x,y]}}`, at least one. INX Parameters panel only; read `parameterPanel {filter,animation,bookmark}`. Filter matches parameter/group names case-insensitively; armed rows stay visible. Empty filter clears it; null bookmark shows all without clearing the filter. `animation` chooses an existing window clip, pauses playback and resets local frame on change, without seeking the Session preview. `labelIndex` is the stored index in `model.animations[name].bookmarks` from the snapshot; malformed labels are not selectable. Part parameters are resolved, never supplied by the caller. Points finite, within +/-1e9. Combined updates validate atomically. The copied bookmark clears on clip change/replacement, survives stored-label deletion; filter survives replacement. Native formats report null and refuse this tool, leaving their separate name-or-ID filter intact. No document/settings write. |
| `editor_native_parameters_set` | `{filter:string}` sets the CMO3/CAN3 name-or-vendor-ID substring filter, case-insensitively. Empty clears it. Read `nativeParameterPanel {filter}`, including CAN3 without a model (empty rows); INX reports null and refuses this tool. Separate from INX `parameterPanel`. Filter survives document replacement, clears on restart, and remains editable during Lightweight playback. No parameter value, playback, document or settings write. Verify with `tools/verify-editor-native-parameters.py`; native UI and real ACP checks remain separate. |
| `editor_tool_set` | `{tool}` switch the tool of the current edit mode; the id must be in `tool.offered` of the state. |
| `editor_recovery_list` | `{}` the recovery snapshots earlier sessions left, newest first, each as `id` and `format` only. The running session's own snapshot is not listed. Answered even while a dialog waits. |
| `editor_recovery_keep` | `{}` Keep for later: dismiss the current startup offer, including a temporarily hidden one; repeated calls succeed. Returns editor state with `windows.recoveryOffer {pending:false,drawEligible:false}`. No snapshot, document, draft or settings write; disk listing still includes the files and next startup offers them again. Does not answer protected dialogs. |
| `editor_recovery_restore` | `{id}` open that snapshot as an untitled, modified document, replacing the current one like `open`; it returns the editor state. Refused over unsaved changes or an open draft, while a dialog waits for the user, and while the editor is closing. |
| `editor_settings_get` | `{}` listed editor preferences (Settings controls and View > Stats for Nerds), each with `key`, `label`, `type`, `value` and `writable`, plus read-only shortcut bindings. It never lists a token, header, agent command or path. |
| `editor_settings_set` | `{key, value}` set one preference and save the settings file. Writable keys: `DarkMode`, `lang`, `UIScale` (except macOS), `useOpenDyslexic`, `MaxUndoHistory`, `KeepLayerFolder`, `LayerGroupReplacement`, `NerdStats`. `NerdStats` is the stats window's visibility boolean, off by default; confirm `windows.nerdStats` in editor state and the settings readback. Read-only: the autosave keys, `SaveProjectOnClose` and the `MCP.*` keys. |
| `editor_shortcut_set` | `{command, shortcut, repeat?}` bind a command (a `command` of the `shortcuts` list of `editor_settings_get`) to a chord in that list's `spec` form (`Ctrl+Shift+S`: Ctrl/Alt/Shift/Super and one key A-Z, 0-9, F1-F12, Left/Right/Up/Down), or unbind it with `null`; saves the settings file. The result's `displaced` names a command that lost the chord. Refusals (unknown command, key or modifier) change nothing. ACP calls wait for host approval like `editor_settings_set`. |
| `editor_viewport_set` | `{vertices?, bounds?, orientation?, background?, partSort?:z_sort\|size_sort}`, at least one. Boolean selection overlay flags and RGB `[r,g,b]` with integer channels 0-255. `[0,0,0]` resets black. INX Part menu `partSort` orders unselected/unlocked candidates under the pointer front-to-back or by ascending bounding-box area; it does not alter direct click or bookmark hit tests. Native formats report `viewport.partSort:null` and refuse sort updates atomically, including combined calls; other viewport fields remain available. Omitted fields stay unchanged; invalid calls change nothing. Read `viewport` in the answer and state resource. Replacement retains values; restart resets them. Window session only, no settings write. Available while armed or tools hide the menu; drawing depends on selection and edit mode. |
| `editor_grid_set` | `{show?, spacing?, colour?}`, at least one. Canvas grid: lines every `spacing` world units (1 to 1e6) through the origin, `colour` unmultiplied `[r,g,b,a]` 0-255. Answers the state with `grid {show, spacing, colour}`. Not saved. |
| `editor_solo_set` | `{show?, colour?}`, at least one. Solo view: with nodes selected in an INX document, the rest of the picture is covered by `colour` (unmultiplied `[r,g,b,a]` 0-255, alpha the opacity) and the selection is drawn on top; a canvas click picks only a Part drawn on top. Answers the state with `solo {show, colour}`. Not saved. |
| `editor_onion_set` | `{onion?, trajectories?}`, at least one. Turns the viewport's Onion Slice and Trajectories on or off; answers the state with `animationOverlays {onion, trajectories}`. With a frame previewed (`animation_seek`) and the animation still (no playback, no physics), Onion Slice makes the picture the `animation_onion` one, and Trajectories draws the `animation_trajectory` paths of the selected Part. INX documents. Not saved, not a document edit. |
| `editor_playback_set` | `{lightweight?, snapToFramerate?}`, either or both. Turns Lightweight playback (Cubism) and the timeline's "Lock playback to animation framerate" on or off; answers the state with `playback {lightweight, snapToFramerate}`. While the window's Play runs an INX animation, a CAN3 scene or a Motion3 clip, the parameter panels keep the values they showed before Play. Playback driven over MCP (`animation_advance`, `motion3_advance`) is not the window's and is unaffected. With the lock on, the window's Play moves an INX animation in whole frames of its `timestep` and starts from the whole frame below a fractional one. Session only. |
| `editor_timeline_zoom_set` | `{zoom:number}`, required finite absolute multiplier. Rounds to the window's f32, then clamps to 0.5–10; read the stored value at `timeline.zoom`. Shares native Zoom out/slider/Zoom in/hover-gesture storage. Retains the value across animations, document replacement, hidden Timeline and layout/workspace changes, including non-INX or no selected animation. Fresh editor default is 1. No seek, pause, document/history edit or settings write. Headless refuses. Existing request reconciliation, refusal status and pending-Quit cancellation still apply. |
| `editor_workspace_save` | `{name}` save the panel layout (Outline and Timeline shown or hidden, their sizes as last drawn or applied) under `name` (trimmed, 1-64 characters, no control characters); a saved name is replaced; at most 16. Kept in the editor settings. Answers the state with `workspaces`. |
| `editor_panel_set` | `{panel, visible}` sets exactly one panel's visibility; `panel` is `outline` or `timeline`, and `visible` is a boolean. Returns editor state; confirm `panels` in the resource. Preserves the other panel, sizes, drafts and document history. Saves to the settings file when available; a write failure reports that only the session changed. |
| `editor_layout_reset` | `{}` View > Reset Layout: show panels with stored visibility keys, clear Outline/Timeline sizes and floating window positions. Absent visibility keys stay absent; open windows, collapsed state, Stats visibility, saved workspaces and drafts are kept. Returns editor state; confirm `panels`, then save a workspace to check default sizes. A settings-write error reports a session-only reset. ACP calls require exact-call host approval when there is a settings destination. |
| `editor_workspace_apply` | `{name}` show or hide the panels and give them the saved sizes; floating windows stay where they are. An unknown name is refused. |
| `editor_workspace_delete` | `{name}` remove a saved workspace. An unknown name is refused. |
| `editor_physics_preview_set` | `{enabled}` switches Enable physics. Enabling requires INX; false is allowed for any format. Read `physicsPreview {enabled, available}`. The flag survives document replacement within the window; stepping runs only when available. Disabling stops steps without resetting the simulation. No document edit or settings write. |
| `editor_guides_set` | `{open?, show?, snap?, colour?, lines?}`, at least one. `lines:[{axis: horizontal\|vertical, position, colour?, memo?}]` replaces every guide (at most 256, `position` within +/-1e9 world units, `memo` at most 256 characters); a guide without `colour` takes the default `colour`. `snap` makes a vertex dragged with the Point tool adhere to a shown guide within 8 points. `open` independently shows or closes Guide Settings; confirm `windows.guideSettings`. Answers the state with `guides`. Not saved. |

## Gotchas

- Verified (headless): the resource answers `{"document":{"format":"inx","modified":false,"path":null,"revision":0},"editor":false}` and nothing else; the window keys are left out, not sent empty.
- Verified (headless): `editor_selection_set {nodes:[]}` and `editor_camera_fit {}` fail with "<tool> needs the editor: this host has no window", and so do `editor_recovery_list`,
  `editor_recovery_restore`, `editor_settings_get` and `editor_settings_set` (one call each against `soul2d-core --mcp`). The manifest marks these editor tools `headless_ok: false`.
- Run (real `soul2d-editor --mcp`, isolated settings directory, the window not looked at): `editor_grid_set` answers the state with
  the new `grid`, the next read holds it, `revision` does not move, and `spacing` below 1 is refused without a change.
- Run (the same way): `editor_guides_set` answers the state with the new `guides`, a line without `colour` takes the
  default `colour`, the next read holds them, `revision` does not move, a wrong `axis`, a position past 1e9, 257 lines,
  a 257-character memo and `{}` are refused without a change, and `lines: []` removes every guide.
- Run (the same way, a synthetic INX open): `editor_solo_set` answers the state with the new `solo`, the next read holds it,
  `revision` does not move and a three-channel colour is refused. `render` with `solo` drew only the named nodes and
  the nodes below them, on `soul2d-editor` and on `soul2d-core`; with `solo_cover` it drew the covered picture too,
  and `solo_cover` without `solo` was refused.
- Run (the same way, an isolated settings directory holding one workspace): `editor_workspace_apply` hides the Outline,
  `editor_workspace_save` adds the trimmed name and writes the layout to the settings file, an unknown or empty name is
  refused, `editor_workspace_delete` removes a name, and a second run reads the saved workspace with the Outline still
  hidden.
- `editor_selection_set` and `editor_armed_set` refuse while `drafts.tools` is true or a binding value list is unapplied: the user has edits in the window that the change would drop. Ask the user to apply or discard them.
- Replacing the document (`new`, `open`, an import) clears the selection and disarms the parameter.
- `drafts.panels: true` makes the file tools (session.md) refuse too.
- `editor_recovery_restore` replaces the document and discards the editor-side panel state of the old one (Motion3 and Live2D form panels, armed parameter, toolbar path). The snapshot stays on disk and becomes
  the session's own, so a save deletes it. Save or discard the current document first.
- A snapshot id is compared with the listing only; a path or a name outside it is refused with "there is no recovery snapshot".
- A dialog in the editor ("A dialog in the editor waits for the user's answer ...") and a closing editor ("The editor is closing ...") refuse restore. The dialog refuses `new`, `open` and the imports too. The user answers there; do not retry in a loop.
- `editor_settings_set` writes the settings file. `MaxUndoHistory` is saved and so survives a restart; lowering it drops undo steps at once. `history_limit_set` (history.md) changes the limit for the session only. `editor_shortcut_set` writes the same file's `Shortcuts` map; the editor applies it at startup.
- A change whose write fails is an error that says "<key> changed for this session, but the settings file could not be written": the value is in effect until the editor closes.
- Entering Edit Mesh or depth mode cannot be set from here; `deform` mode is reached by selecting a mesh node and arming a parameter.

Verify Guide Settings open/close and physics preview with `python3 tools/verify-editor-preview-controls.py --editor "$TARGET/debug/soul2d-editor" --core "$TARGET/debug/soul2d-core"`. It checks strict refusals, unchanged document/settings, INX and native-format behavior, headless refusal and defaults after restart. Native GUI and real ACP-agent acceptance remain separate.
