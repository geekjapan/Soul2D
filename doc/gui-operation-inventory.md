# GUI operation inventory

This is the incremental inventory for Issue #99. It holds the fifty-nine Soul2D GUI and related MCP operations below and, in the
section "Cubism Editor menu bar", the 319 entries of the Cubism Editor menus, and in "Cubism Editor palettes and tool details" the
405 palette and tool-detail controls, each with its Soul2D status. The section "nijigenerate-derived D editor" pins the
last D tree of the earlier, unpublished development history (2026-10-11) and lists its menus, panels, viewport tools, inspector fields and the D-era commands
that left the manifest. "Cubism Editor dialogs" holds 339 fields of the modelling-side dialogs. "Soul2D editor menu bar" lists the editor's own 48 menu entries and their shared tool routes, including bounded external and ACP Quit. "Cubism Editor dialogs, animation side" holds 246 more fields. "Cubism Editor view area, toolbar and view features" holds 103 rows and "Soul2D editor panels" groups the editor's 118 control labels into 17 rows. Every slice named by #99 section 1 now has a section; the Cubism half is read from the manual, not from a running Editor. Tool counts and Skill coverage do
not prove that every GUI action is covered.

| Operation | Source GUI entry | Agent action | Read result | Status |
|---|---|---|---|---|
| Soul2D CAN3 key creation | CAN3 Outline > scene > track > curve > New key > Add CAN3 key | `live2d_animation_key_add {sceneGuid,trackGuid,curveId,anchorFrame,value,type,partOpacityMapKey?,expectedRevision}` | Animation-source resource, snapshot and history | All four curve families; no CMO3 attachment; explicit signed frame/value/type; revision-bound draft and shared validation |
| Soul2D CAN3 key deletion | CAN3 Outline > scene > track > curve > key > Delete key | `live2d_animation_key_delete {sceneGuid,trackGuid,curveId,anchorFrame,partOpacityMapKey?,expectedRevision}` | Animation-source resource, snapshot and history | Exact frame and stored underscore map key; one-step Undo; deleting the last key keeps the empty curve |
| Soul2D CAN3 parameter curve creation | CAN3 Outline > New CAN3 curve > Parameter > target > Add CAN3 curve | `live2d_animation_curve_add {sceneGuid,trackGuid,modelPath,parameterId,expectedRevision}` | Animation-source resource and session snapshot | Explicit attached CMO3 must match track resource; frozen draft target/revision; shared Session validation/history |
| Soul2D CAN3 PartOpacity curve creation | CAN3 Outline > New CAN3 curve > PartOpacity > target > Add CAN3 curve | `live2d_animation_curve_add {sceneGuid,trackGuid,modelPath,partId,expectedRevision}` | Animation-source resource and session snapshot | Modern encoding; explicit existing Part choice and matching attachment; legacy/underscore creation unavailable |
| Soul2D CAN3 Part choices | CAN3 Outline > New CAN3 curve > PartOpacity > Refresh Part choices | `live2d_authoring_read {path}` on the explicit attached CMO3 | Source Parts (`sources[].data.type = Part`) | User-triggered read; choices invalidated on attachment/document replacement; stored resource paths are not opened |
| Soul2D CAN3 curve deletion | CAN3 Outline > scene > track > curve > Delete curve > Confirm curve deletion | `live2d_animation_curve_delete {sceneGuid,trackGuid,curveId,partOpacityMapKey?,expectedRevision}` | Animation-source resource, snapshot and history | Parameter, modern, legacy and underscore; exact map key when present; captured key count and stale-consent refusal |
| Soul2D CAN3 track creation | CAN3 Outline > New CAN3 track > Add CAN3 track | `live2d_animation_track_add {sceneGuid,resourceGuid,name,expectedRevision}` | Animation-source resource, snapshot and history | Existing scene/resource only; independent resource choice; dirty form freezes target/revision and refuses stale or changed selection |
| Soul2D CAN3 track deletion | CAN3 Outline > scene > track > Delete track > Confirm track deletion | `live2d_animation_track_delete {sceneGuid,trackGuid,expectedRevision}` | Animation-source resource, snapshot and history | Exact target/resource and captured curve counts; revision-bound confirmation; one undo step |
| Soul2D CAN3 image-track deletion | CAN3 Outline > scene > Image row > Delete image track > Confirm track deletion | `live2d_animation_track_delete {sceneGuid,trackGuid,expectedRevision}` | Animation-source `scenes[].imageTracks`, snapshot and history | Exact image target and retained resource; revision-bound confirmation; one undo step; model controls remain separate |
| Soul2D CAN3 scene creation | CAN3 Outline > New CAN3 scene > Add CAN3 scene | `live2d_animation_scene_add {name,fps,durationFrames,expectedRevision}` | Animation-source resource and session snapshot | Shared Session operation; one undo step; visible with no existing scenes |
| Soul2D CAN3 scene deletion | CAN3 Outline > scene > Delete scene > Confirm scene deletion | `live2d_animation_scene_delete {sceneGuid,expectedRevision}` | Animation-source resource, snapshot and history | Shared Session operation; GUI confirmation freezes revision and refuses stale consent; cancellation changes no document data |
| Soul2D About | Help > About | `editor_about_set {open:true}` | Editor state `windows.about:true` | Shared operation; session-only visibility, all document formats |
| Soul2D close About | About > Close window | `editor_about_set {open:false}` | Editor state `windows.about:false` | Shared operation; idempotent, no settings write |
| Soul2D Command Browser | Edit > Command Browser; command-browser shortcut | `editor_command_browser_set {open:true}` | Editor state `windows.commandBrowser:true` | Shared operation; repeated open and browser self-open retain its current inputs/results |
| Soul2D close Command Browser | Command Browser > Close window; Escape | `editor_command_browser_set {open:false}` | Editor state `windows.commandBrowser:false` | Shared operation; clears browser payload, native dismissal suppresses same-draw activation/refresh |
| Soul2D configuration address | View > Open Configuration Folder | `editor_address_dialog {action:"open", target:"configuration"}` | Editor state `windows.addressDialog` | Shared operation; settings parent path, no file manager launch |
| Soul2D documentation address | Help > Online Documentation | `editor_address_dialog {action:"open", target:"online_documentation"}` | Same dialog state | Shared operation; no browser launch |
| Soul2DLive documentation address | Help > Soul2DLive Documentation | `editor_address_dialog {action:"open", target:"soul2d_live_documentation"}` | Same dialog state | Shared operation; distinct target and title |
| Soul2D bug report address | Help > Report a Bug | `editor_address_dialog {action:"open", target:"bug_report"}` | Same dialog state | Shared operation; fixed destination |
| Soul2D feature request address | Help > Request a Feature | `editor_address_dialog {action:"open", target:"feature_request"}` | Same dialog state | Shared operation; fixed destination |
| Soul2D copy address | Address dialog > Copy | `editor_address_dialog {action:"copy"}` | Same dialog state and successful queue acknowledgment | Shared operation; original address, OS clipboard delivery unverified |
| Soul2D close address | Address dialog > Close window | `editor_address_dialog {action:"close"}` | `windows.addressDialog:null` | Shared operation; idempotent |
| Soul2D Part menu sorting | INX viewport context menu > Z-Sort / Size-Sort | `editor_viewport_set {partSort:"z_sort"}` or `{partSort:"size_sort"}` | Editor state `viewport.partSort` | Shared editor operation; candidate list only; direct click and bookmark picking retain their hit testing |
| Soul2D native parameter filter | Outline > Live2D native preview > Model parameters / facial controls > Native parameter filter | `editor_native_parameters_set {filter:string}` | Editor state `nativeParameterPanel.filter` | Shared editor operation; CMO3/CAN3 name-or-ID matching, including unattached CAN3 |
| Soul2D panel Outline visibility | View > Panels > Outline | `editor_panel_set {panel:"outline", visible:bool}` | `resource://soul2d/editor/state`, `panels.outline` | Covered by the shared editor operation |
| Soul2D panel Timeline visibility | View > Panels > Timeline | `editor_panel_set {panel:"timeline", visible:bool}` | `resource://soul2d/editor/state`, `panels.timeline` | Covered by the shared editor operation |
| Soul2D Mirror View | Viewport toolbar > Mirror View | `editor_camera_set {mirror:bool}` | `resource://soul2d/editor/state`, `camera.mirror` | GUI and tool set the same viewport camera field |
| Soul2D New | File > New | `new {}` | Snapshot, history and editor document path | ACP project-reset approval; dirty/unsaved/dialog/closing refusal before prompting |
| Soul2D Open | File > Open | `open {path}` or `FileCommand_OpenFile {file}` | Snapshot and editor document path | ACP project-reset approval binds resolved input file and current document |
| Soul2D recovery Restore | Recovery offer > Restore | `editor_recovery_restore {id}` after `editor_recovery_list` | Snapshot and untitled editor document | ACP project-reset approval; ID resolves through regular-file listing, excludes own snapshot |
| Soul2D recovery Keep for later | Recovery offer > Keep for later | `editor_recovery_keep {}` | Editor state `windows.recoveryOffer {pending,drawEligible}` | Shared transient offer dismissal; no file, document, draft or settings change; files offered again at next startup |
| Soul2D PSD import | File > Import > Import PSD... | `import_psd {path}` | Snapshot and import report | ACP import approval replaces document; input contents read after consent |
| Soul2D KRA import | File > Import > Import KRA... | `import_kra {path}` | Snapshot and import report | ACP import approval; shared dirty/unsaved/document-dialog gates |
| Soul2D INP import | File > Import > Import soul2d_live puppet... | `import_inp {path}` or `FileCommand_ImportINP` | Snapshot and untitled editor document | ACP import approval; exact normalized call and input path |
| Soul2D image folder import | File > Import > Import Image Folder... | `import_folder {path}` or `FileCommand_ImportImageFolder` | Snapshot and import report | ACP import approval names resolved input folder; contents read at execution |
| Soul2D Save / Save As | File > Save / Save As | `save {path}` or `FileCommand_SaveFile {file}` | Saved document reopened through `open`; snapshot/history | ACP approval binds exact call, resolved output and document revision/draft state |
| Soul2D preview capture | MCP-only capture alias; no dedicated menu entry | `render {path?,width?,height?,bounds?}` or `ViewCommand_CaptureLiveScreenshot` | Image content; PNG output when path supplied | Pathful ACP capture requires approval; pathless capture remains usable |
| ACP allow a covered operation once | Coding agent operation approval > Allow once | Original covered operation waits for the human choice | Original tool response, editor state or settings readback | Host-owned human permission; no agent approval tool |
| ACP deny covered changes for the prompt | Coding agent operation approval > Deny changes this turn | Original covered operation receives a refusal | Original tool response and unchanged destination | Host-owned human permission; no agent approval tool |
| Soul2D Stats for Nerds visibility | View > Stats for Nerds | `editor_settings_set {key:"NerdStats", value:bool}` | `resource://soul2d/editor/state`, `windows.nerdStats`; `editor_settings_get` | Shared preference setter, off by default |
| Soul2D close Stats for Nerds | Stats for Nerds > Close window | `editor_settings_set {key:"NerdStats", value:false}` | Same state and preference readback | Shared preference setter |
| Soul2D save workspace | View > Workspace > Save Workspace... > Save | `editor_workspace_save {name}` | Editor state `workspaces`; settings file | Shared editor operation; ACP file approval |
| Soul2D apply workspace | View > Workspace > saved name | `editor_workspace_apply {name}` | Editor state `panels`; panel sizes; settings file | Shared editor operation; ACP file approval |
| Soul2D delete workspace | View > Workspace > Delete Workspace > saved name | `editor_workspace_delete {name}` | Editor state `workspaces`; settings file | Shared editor operation; ACP file approval |
| Soul2D Reset Layout | View > Reset Layout | `editor_layout_reset {}` | Editor state `panels`; a saved workspace has default sizes | Shared editor operation; ACP file approval; floating area reset covered by Rust context test |
| Soul2D Show vertices | Viewport controls > Show vertices | `editor_viewport_set {vertices:bool}` | Editor state `viewport.vertices` | Shared editor operation; window session only |
| Soul2D Show bounds | Viewport controls > Show bounds | `editor_viewport_set {bounds:bool}` | Editor state `viewport.bounds` | Shared editor operation; window session only |
| Soul2D Show orientation gizmo | Viewport controls > Show orientation gizmo | `editor_viewport_set {orientation:bool}` | Editor state `viewport.orientation` | Shared editor operation; window session only |
| Soul2D Background colour | Viewport controls > Background colour | `editor_viewport_set {background:[r,g,b]}` | Editor state `viewport.background` | Shared editor operation; opaque RGB |
| Soul2D Reset background | Viewport controls > Reset background | `editor_viewport_set {background:[0,0,0]}` | Editor state `viewport.background` | Shared editor operation; black default |
| Soul2D Guide Settings visibility | Viewport toolbar > Guide Settings | `editor_guides_set {open:bool}` | Editor state `windows.guideSettings` | Shared editor operation; independent of guide line visibility |
| Soul2D open Guide Settings | Viewport context menu > Guide line > Guide Settings | `editor_guides_set {open:true}` | Editor state `windows.guideSettings` | Shared editor operation |
| Soul2D close Guide Settings | Guide Settings > Close window | `editor_guides_set {open:false}` | Editor state `windows.guideSettings` | Shared editor operation |
| Soul2D Enable physics | Viewport toolbar > Enable physics | `editor_physics_preview_set {enabled:bool}` | Editor state `physicsPreview` | Shared editor operation; enabling requires INX, disabling allowed on every format |
| Soul2D Same display position | View tabs > Same display position | `editor_camera_sync {}` | Editor state `viewCameras` | Shared editor operation; INX/CMO3 centre and zoom; Mirror View stays global |
| Soul2D Parameter filter | Outline > Parameters > Parameter filter | `editor_parameters_set {filter:string}` | Editor state `parameterPanel.filter` | Shared editor operation; INX name matching; separate armed row preserved |
| Soul2D Show all parameters | Outline > Parameters > Show all parameters | `editor_parameters_set {bookmark:null}` | Editor state `parameterPanel.bookmark` | Shared editor operation; name filter retained |
| Soul2D select a Part parameter bookmark | Viewport context > Parameter bookmark > Part | `editor_parameters_set {bookmark:{part,point}}` | Editor state `parameterPanel.bookmark` | Shared editor operation; resolves related parameters |
| Soul2D select a stored parameter label | Viewport > bookmark label | `editor_parameters_set {bookmark:{labelIndex}}` | Editor state `parameterPanel.bookmark` | Shared editor operation; uses stored index of selected clip |
| Soul2D select an INX animation | Outline > INX animation controls > clip combo | `editor_parameters_set {animation:name}` | Editor state `parameterPanel.animation` | Shared editor operation; pause and local frame reset on change; Session preview stays separate |

Parameters panel actions keep the existing `ParameterPanelState`, selected clip and local seek frame. The
shared setter resolves a stored-label index or a Part's related parameters before assigning a compound patch.
It reconciles clip deletion and selection even with Outline hidden. The name filter persists across document
replacement; the copied bookmark clears on clip change/replacement and survives deletion of its stored label.
The native runtime's separate name-or-ID filter is outside this operation. `tools/verify-editor-parameter-panel.py`
checks the production MCP route and lifecycle with Outline hidden; Rust tests cover the GUI routes and
no-draw transitions. Native UI, GPU and real-agent acceptance remain separate.

Per-view cameras remain in the viewport. Command boundaries reconcile selection and removal before another
camera operation, even when several MCP calls arrive before a frame. The Same display position button emits
the validated editor call. `tools/verify-editor-view-cameras.py` checks the production MCP transitions,
sync, removed-ID reuse, document replacement, session restart and format boundaries. Rust tests cover the
egui button request and dispatch. These checks do not establish native GUI, GPU or real-agent acceptance.

The panel entries originate in `PANELS` and `Windows::menu_bar` in
[`editor_windows.rs`](../rust/src/editor_windows.rs). The menu emits the same tool request parsed by
[`mcp.rs`](../rust/src/mcp.rs) and handled by `EditorView::handle` in
[`editor_ui.rs`](../rust/src/editor_ui.rs). The editor endpoint is shared by stdin MCP, the listener,
the command palette and the ACP panel. A headless host refuses the operation. Its manifest effect is
`file-write` because it persists an editor setting when a settings file is available.

The [operating map](../.cursor/skills/operate-soul2d/families/editor-state.md) describes the call and result.

The five viewport presentation controls edit local widget values and emit `editor_viewport_set` through the
same validated MCP path as remote calls. Painting reads the stored RGB background beneath the texture and
the overlay flags for the selection; mesh tools can suppress Part overlays. The overlay checkboxes are
hidden while a parameter is armed, without clearing their state. Rust tests cover widget dispatch and
egui paint output. `tools/verify-editor-viewport.py` checks production MCP updates, invalid-input atomicity,
unchanged settings and document state, headless refusal and session-only restart. Native GUI and real-agent
acceptance remain separate checks.
[`verify-editor-panel-state.py`](../tools/verify-editor-panel-state.py) exercises the real editor MCP path,
configuration persistence and restart. Rust tests cover menu dispatch, unchanged document/draft state,
invalid-argument refusals and explicit session-only behavior when saving fails or no settings file is available.
Native GUI evidence, real ACP-agent execution and whole-Issue acceptance are separate checks.

Guide-window visibility and physics preview keep the existing `ViewportState` flags. They survive document
replacement within the window, without becoming persistent settings. Guide line visibility remains the
separate `guides.show` field. Physics preview advances only INX documents; its readable `available` flag
distinguishes that capability from the stored `enabled` flag. Frame-step failure and a successful physics
bake retain their existing shutdown behavior. `tools/verify-editor-preview-controls.py` exercises the
production MCP route, invalid-input preservation, native-format refusal, headless refusal and defaults
after restart. Rust tests cover toolbar dispatch, context-menu dispatch, window closing and lifecycle behavior.

Mirror View is held in `ViewportState.camera.mirror` in `editor_viewport.rs`; the toolbar and editor tool
both set this field. It is view-only, session-only and editor-only. A mirror-only tool call keeps any pending
initial camera fit; other camera fields are unchanged when omitted. `verify-editor-camera.py` checks the
production MCP setter, invalid-input refusals, unchanged document state and the session-only restart.

The ACP approval window is drawn outside Outline. It binds the original authenticated HTTP call to the exact
normalized covered operation and its destination. A person approves one attempt or denies covered changes
for that prompt. Cancellation, prompt completion, disconnection, session replacement, a changed destination
and the two-minute approval timeout refuse the held call. Session-only changes do not ask to write a file.
Preference keys, types and write permissions are validated before an approval can be staged. Notifications
cannot stage or consume a grant. The ordinary MCP route is unchanged. This gate covers ACP panel and
preference setters, workspace save/apply/delete, Reset Layout, Save and pathful preview capture. Save and
capture bind the resolved output file, source document, revision and draft state; a changed target or document
refuses the held call. Dirty drafts block Save. Captures use the preview at execution time. Pathless capture
needs no approval. New, Open, recovery Restore and the four replacing imports also require consent, with distinct input-file,
input-folder and recovery targets. Shared dirty/unsaved/dialog/closing checks run before prompting and at
execution. A source path that resolves differently or is no longer valid refuses the held call; input contents
are read at execution rather than frozen. Other writers and real-agent workflows remain separate work. The permission buttons are intentionally human-owned, as #99
requires approval for file writes.

Single-file ACP exports require consent for `export.inp`, `export.image`, `export.gif`, `view.screenshot`,
`object.export_csv`, `motion3.export`, and pathful onion or Live2D runtime JSON exports. The screenshot
and Motion3 aliases share that gate. The resolved target uses the writer's effective extension; JPEG writes
`.jpeg` even when the requested name ends in `.jpg`. After following a link, image export writes that exact
resolved filename; it does not force the extension a second time. The open document cannot be an export destination.
Document and resolved-target changes invalidate consent; existing overwrite rules remain in force.
Pathless runtime JSON and absent/null-path onion requests remain usable without approval. Runtime JSON
still rejects a supplied null path. Fanout exports, named presets/templates and other effects remain work.

Workspace approval names the exact tool and workspace name. Applying a workspace also writes panel
visibility to the settings file, so it requires the same consent as saving or deleting one. A failed write
reports that the change holds for this session; the prior settings file remains intact. Rust editor tests
cover all three operations, approval choices and failures. Authenticated TCP tests cover the held-call
lifecycle. These checks do not establish native GUI or real-agent acceptance.
`tools/verify-editor-workspaces.py` exercises the production stdio MCP route, saved layouts across a restart,
headless refusal and all three settings-write failures.

Reset Layout uses that same editor endpoint from the View menu, command palette and MCP. It clears the two
resizable panel sizes and egui floating areas, while preserving window open/collapsed state, Stats visibility,
saved workspaces and drafts. Stored visibility keys are set to true and saved once; absent keys stay absent.
The host approval names the Reset Layout target. The production workspace verifier also checks reset,
default sizes, persistence across a restart, absent-key preservation and a failed reset settings write.
Native floating-window geometry and real-agent acceptance remain separate checks.

The NerdStats preference is separate from the docked panels: it defaults to hidden and Reset Layout does
not turn it on. The View checkbox and the floating window's close button dispatch the existing preference
setter. Its existing persistence response distinguishes a saved change from a session-only change and a
failed settings-file write.
`tools/verify-editor-stats.py --editor <editor-binary> --core <core-binary>` checks the production MCP
read/set/refusal path, unchanged source and document state, and settings persistence across an editor restart.
Native menu/window parity and ACP approval are separate checks.

## Cubism Editor menu bar

Read from the Cubism Editor manual on 2026-10-20 (the manual's menu pages and its shortcut list; the pages also describe 5.3.05 beta1, so this is not a 5.3.04 parity claim). One row per menu entry, 319 rows: ファイル (File) 24, 編集 (Edit) 12, 表示 (View) 30, モデリング (Modeling) 107, アニメーション (Animation) 53, フォームアニメーション (Form Animation) 28, ウィンドウ (Window) 22, ヘルプ (Help) 17, メッシュ編集モード (Mesh edit mode) 26.

Status values: `covered (<command>)` names the Soul2D command that does it; `tracked (<ledger ID>[, #issue])` points at the open obligation in `doc/cubism-feature-ledger.md`; `absent` has neither; `no counterpart (Editor product)` is a function of the Editor as a program (links, licences, display quality). Where a ledger ID exists it is listed even when the row is covered, so the ledger carries the verdict. Clients are expected to combine reads and one action (for example `nodes_list` then `editor_selection_set`); such rows say so.

Counts: covered 203, tracked 57, absent 36, no counterpart 23.

Palettes and tool details follow in the next section; dialogs and the viewport context menus are the slice after. The nijigenerate-derived inventory is pinned to the last D tree below.

### ファイル (File)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| 新規作成 > モデル | Ctrl+N |  | new | covered (new) |
| 新規作成 > アニメーション | Ctrl+Shift+N | ANI05 | animation_create, live2d_animation_scene_add | covered (animation_create) |
| ファイルを開く | Ctrl+O |  | open, import_inp | covered (open) |
| 前回開いたファイルを開く | Ctrl+Shift+O |  | open | covered (open); the editor's Recent list supplies the path |
| 最近開いたファイルを開く |  |  | open | covered (open); the editor's Recent list supplies the path |
| 閉じる | Ctrl+W |  | new | covered (new); one document per Session |
| すべてを閉じる |  |  | new | covered (new); one document per Session |
| 保存 | Ctrl+S |  | save | covered (save) |
| 別名保存 | Ctrl+Shift+S |  | save | covered (save); `path` names the new file |
| 画像/動画書き出し > 画像 |  |  | export_image, render | covered (export_image) |
| 画像/動画書き出し > GIFアニメ |  | EXP02 | export_gif | covered (export_gif) |
| 画像/動画書き出し > 静止画(連番) | Ctrl+I | EXP01 | export_frames | covered (export_frames) |
| 画像/動画書き出し > PSD画像(β) | Ctrl+P | IO05 | texture_export | absent; texture_export writes texture slots, not a layered PSD |
| 画像/動画書き出し > 動画 | Ctrl+Shift+M | EXP03, EXP04, EXP05 | export_video_start, export_video_status, export_video_cancel | covered (export_video_start) |
| 組み込み用ファイル書き出し > moc3ファイル書き出し | Ctrl+Alt+S | EXP08 |  | absent; Soul2D reads moc3 (#398) and does not write it |
| 組み込み用ファイル書き出し > 物理演算設定の書き出し |  | PHY01 | live2d_physics3_export | covered (live2d_physics3_export); tracked (PHY01, #236) |
| 組み込み用ファイル書き出し > モーションファイル書き出し | Ctrl+Alt+Shift+S | EXP08 | motion3_export | covered (motion3_export) |
| 環境設定 |  | UI01 | editor_settings_get, editor_settings_set | covered (editor_settings_set) |
| キーボードショートカット | Ctrl+Alt+Shift+K | UI11 |  | absent as a dialog; Soul2D binds in Settings > Shortcuts, `editor_settings_get` lists and `editor_shortcut_set` rebinds (#99 section 2) |
| 外部アプリケーション連携設定 |  | API01 |  | no counterpart (Editor product); API01 is an accepted mapping onto MCP tools |
| モデルの統計情報 |  |  | model_statistics | covered (model_statistics) |
| キャンバス設定 |  |  | bounds, editor_camera_fit | absent; INX has no canvas size, the camera frames the model |
| モデルのリサイズ |  |  | node_transform | covered (node_transform); scale of the root node |
| 終了 | Alt+F4 |  |  | no counterpart (Editor product); the host owns the process |

### 編集 (Edit)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| 元に戻す | Ctrl+Z |  | undo, history_goto | covered (undo) |
| やり直す | Ctrl+Y (Ctrl+Shift+Z is an unlabelled row of the shortcut page) |  | redo | covered (redo) |
| 切り取り | Ctrl+X |  | node_cut | covered (node_cut) |
| コピー | Ctrl+C |  | node_copy, parameter_copy, mesh_copy | covered (node_copy) |
| 貼り付け | Ctrl+V |  | node_paste, parameter_paste, mesh_paste | covered (node_paste) |
| 削除 | Delete (Backspace is an unlabelled row of the shortcut page) |  | node_delete, parameter_delete, live2d_delete | covered (node_delete) |
| パラメータ値をコピー |  | PAR10 | snapshot | covered (snapshot); the client keeps the values |
| パラメータ値を貼り付け |  | PAR10 | value_set, live2d_parameter_set | covered (value_set) |
| 選択を解除 | Esc |  | editor_selection_set | covered (editor_selection_set) with an empty list |
| 非表示およびロックの選択を解除 |  | UI12 | editor_selection_set, nodes_list | covered (editor_selection_set); the client drops hidden and locked nodes |
| すべて選択 | Ctrl+A |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| 検索と置換 | Ctrl+F | UI10 | object_find, object_replace_name | covered (object_find) |

### 表示 (View)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| グリッド > グリッドを表示(モデリングビュー) |  | UI06 | editor_grid_set | covered (editor_grid_set) |
| グリッド > グリッド設定 |  | UI06 | editor_grid_set | covered (editor_grid_set); spacing only, no colour |
| ガイド > ガイドを表示(モデリングビュー) |  | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド > ガイドを表示(アニメーションビュー) |  | UI05 | editor_guides_set | covered (editor_guides_set); one viewport, no per-view guides |
| ガイド > 中央にガイドを追加 |  | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド > 選択されている頂点の座標にガイドを追加 |  | UI05 | editor_guides_set, scene_geometry | covered (editor_guides_set); the client reads the vertex from scene_geometry |
| ガイド > ガイド(モデリングビュー)の設定 |  | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド > 移動する頂点をガイドへ吸着する |  | UI05 |  | absent; no snapping option |
| アートメッシュの頂点を表示 |  |  | editor_viewport_set | covered (editor_viewport_set) |
| アートメッシュの頂点番号を表示 |  |  |  | absent |
| 親デフォーマからはみ出した頂点をハイライト |  | GEO14 | deformer_validate | covered (deformer_validate) as a read, not a highlight |
| デフォーマの子要素をハイライト | Ctrl+Shift+D |  | editor_selection_set, nodes_list | covered (editor_selection_set); the client selects the children |
| 重なったポリゴンをハイライト |  | REN10 | scene_overlap | covered (scene_overlap) as a read, not a highlight |
| ワープデフォーマリサイズ時の子要素を表示 |  |  |  | absent; viewport hint |
| カーソル位置のオブジェクト名を表示 |  |  |  | absent; viewport hint |
| 選択状態を隠す |  |  | editor_viewport_set | covered (editor_viewport_set) |
| 選択オブジェクトをフォーカス | F | UI04 | editor_camera_fit | covered (editor_camera_fit) |
| 表示方式 > モデル用画像 |  | IO04 |  | absent; the viewport has no atlas mode |
| 表示方式 > テクスチャアトラス |  | IO04 | atlas_build | absent; the atlas is planned, not shown |
| 表示品質 > 原寸 |  |  |  | no counterpart (Editor product); display mipmap level |
| 表示品質 > 1/2 |  |  |  | no counterpart (Editor product); display mipmap level |
| 表示品質 > 1/4 |  |  |  | no counterpart (Editor product); display mipmap level |
| 表示品質 > 1/8 |  |  |  | no counterpart (Editor product); display mipmap level |
| バウンディングボックスの表示/非表示 |  |  | editor_viewport_set | covered (editor_viewport_set); bounds overlay |
| オニオンスキン > モデリングビュー |  | UI08 | editor_onion_set | covered (editor_onion_set) |
| オニオンスキン > アニメーションビュー |  | UI08 | editor_onion_set, animation_onion | covered (editor_onion_set) |
| オニオンスキン > フォームアニメーションビュー |  | UI08, ANI19 | editor_onion_set | covered (editor_onion_set); one viewport |
| オニオンスキン > オニオンスキン設定 |  | UI08 | animation_onion | covered (animation_onion); frame offsets |
| トラックの軌道 > アニメーションビュー |  | ANI14 | editor_onion_set, animation_trajectory | covered (animation_trajectory) |
| トラックの軌道 > トラックの軌道設定 |  | ANI14 | animation_trajectory | covered (animation_trajectory); frame range |

### モデリング (Modeling)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| デフォーマ > ワープデフォーマを作成 |  | GEO10 | node_create, deformer_define_grid | covered (node_create); type GridDeformer |
| デフォーマ > 回転デフォーマを作成 |  | GEO11 | deformer_create_rotation | covered (deformer_create_rotation) |
| デフォーマ > デフォーマの自動生成 |  | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid) |
| デフォーマ > 親のデフォーマを選択 | Ctrl+D |  | editor_selection_set, node_get | covered (editor_selection_set); node_get gives the parent |
| デフォーマ > デフォーマの子要素を選択 | Ctrl+Alt+D |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| デフォーマ > デフォーマを中身ごと削除 | Ctrl+Delete |  | node_delete | covered (node_delete); deletes the subtree |
| デフォーマ > デフォーマを子要素に適用 |  | GEO17 | deformer_apply_to_children | covered (deformer_apply_to_children) |
| デフォーマ > デフォーマの検証 |  | GEO14 | deformer_validate | covered (deformer_validate) |
| デフォーマ > バウンディングボックスの角度を揃える |  |  |  | absent |
| パラメータ > デフォルト設定値に戻す | Ctrl+1 |  | values_reset | covered (values_reset) |
| パラメータ > デフォルトのフォームをロック | Ctrl+Shift+1 | PAR23 | default_form_lock_set | covered (default_form_lock_set) |
| パラメータ > パラメータ値をタブ間で同期 |  | UI02 | view_sync_set | covered (view_sync_set) |
| パラメータ > パラメータ設定 |  | PAR03 | parameter_set_axes, parameter_set_defaults, parameter_rename, live2d_parameter_edit | covered (parameter_set_axes) |
| パラメータ > パラメータグループ設定 |  | UI13 | parameter_group_create, parameter_group_delete, parameter_group_set_color, parameter_move | covered (parameter_group_create) |
| パラメータ > 四隅のフォームを自動生成 | Ctrl+4 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| パラメータ > 動きの反転 |  | PAR20 | parameter_mirror, parameter_duplicate_flip, parameter_mirrored_autofill | covered (parameter_mirror) on INX; tracked (PAR20, #184) on CMO3 |
| パラメータ > 顔の動きを自動生成 > 顔のデフォーマを生成 |  | PAR16 | deformer_generate_humanoid | covered (deformer_generate_humanoid); the face is part of the tree |
| パラメータ > 顔の動きを自動生成 > 顔の動きを生成 |  | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn) |
| パラメータ > 揺れの動きを自動生成 |  | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| パラメータ > 3D回転表現の適用 |  | PAR26 |  | tracked (PAR26) |
| パラメータ > マルチキー編集 |  | PAR19 | binding_multi_edit, live2d_form_set_opacities | covered (binding_multi_edit) |
| パラメータ > 一括反転 |  |  | parameter_flip | covered (parameter_flip) |
| パラメータ > キーに紐付くオブジェクトを選択 |  |  | parameters, editor_selection_set | covered (editor_selection_set); parameters lists the bindings |
| パラメータ > 選択オブジェクトのパラメータ構成を揃える |  |  | binding_copy_to | covered (binding_copy_to); copies the key set with its values |
| パラメータ > まばたき・リップシンク設定 |  | ANI02 | animation_blink_parameters_set, parameter_create_mouth | covered (animation_blink_parameters_set) |
| パラメータ > 拡張補間 |  | PAR06, PAR07 | binding_set_interpolation | covered (binding_set_interpolation) on INX; tracked (PAR06, PAR07, #175) ellipse and SNS refused |
| パラメータ > ブレンドシェイプの重みの制限設定 |  | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| パラメータ > モーションシンク設定 |  | PAR27 | animation_motionsync_start, animation_motionsync_bake | covered (animation_motionsync_start); offline analysis, no settings dialog |
| パラメータ > 対応付けられたパラメータの検証 |  |  | parameters, nodes_list | covered (parameters) as a read |
| アートメッシュ > メッシュ編集 | Ctrl+E | GEO02 | mesh_edit, editor_tool_set | covered (mesh_edit) |
| アートメッシュ > メッシュの自動生成 | Ctrl+Shift+A | GEO01 | automesh_generate | covered (automesh_generate) |
| アートメッシュ > すべてのアートメッシュの描画順を変更 |  | REN01 | node_property_set, live2d_form_set | covered (node_property_set); zsort; on CMO3 the Live2D source forms panel sets the stored draw order of one form (Draw order, Apply form settings) |
| アートメッシュ > アートメッシュの描画順を一括変更 |  | REN01 | binding_multi_edit, live2d_form_set | covered (binding_multi_edit); zSort channel; the stored draw order of one form is set in the Live2D source forms panel (Apply form settings) |
| アートメッシュ > 当たり判定用アートメッシュを生成 |  | EXP09 |  | absent; hit areas are not generated |
| アートパス > アートパスの自動生成 > 単一のアートメッシュから自動生成 |  | GEO06 |  | tracked (GEO06, #159) |
| アートパス > アートパスの自動生成 > 複数のアートメッシュから自動生成 |  | GEO06 |  | tracked (GEO06, #159) |
| アートパス > 色を反映 |  | GEO08 |  | tracked (GEO08, #159) |
| アートパス > 不透明度を反映 |  | GEO08 |  | tracked (GEO08, #159) |
| アートパス > 線幅を反映 |  | GEO08 |  | tracked (GEO08, #159) |
| アートパス > すべてを反映 |  | GEO08 |  | tracked (GEO08, #159) |
| アートパス > 線画のアートメッシュに沿わせる |  | GEO07 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アートパス > 塗りのアートメッシュに沿わせる |  | GEO07 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アートパス > アートメッシュに沿わせるようにコントロールポイントを追加 |  | GEO07 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アートパス > すべてのコントロールポイントの中間に点を追加 |  | GEO07 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アートパス > 選択コントロールポイントの中間に点を追加 |  | GEO07 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アートパス > アートパスの自動生成ダイアログを開く |  | GEO06 |  | tracked (GEO06, #159) |
| アートパス > アートパスから変形パスを生成 |  | GEO19 |  | tracked (GEO19, #169) |
| アートパス > アートメッシュの変形をアートパスにベイク |  | GEO09 |  | tracked (GEO09, #159) |
| アートパス > アートパスを接続 |  | GEO07 |  | tracked (GEO07, #159) |
| アートパス > アートパスを切断 |  | GEO07 |  | tracked (GEO07, #159) |
| アートパス > 追従設定 > すべて追従 |  | GEO09 |  | tracked (GEO09, #159) |
| アートパス > 追従設定 > すべて解除 |  | GEO09 |  | tracked (GEO09, #159) |
| アートパス > 追従設定 > 選択コントロールポイントを追従 |  | GEO09 | live2d_artpath_point_set | tracked (GEO09, #159) |
| アートパス > 追従設定 > 選択コントロールポイントを解除 |  | GEO09 | live2d_artpath_point_set | tracked (GEO09, #159) |
| アートパス > アートパスのブラシの設定ダイアログを開く |  | GEO08 |  | tracked (GEO08, #159) |
| グルー > グルーのオン/オフ切り替え |  | GEO18 | part_set_weld, part_clear_weld | covered (part_set_weld) on INX; CMO3 Glue is read only (#142) |
| スキニング > スキニング |  | GEO20 |  | tracked (GEO20) |
| スキニング > 変形パスからスキニング |  | GEO20, GEO19 |  | tracked (GEO20); deformation paths #169 |
| スキニング > 回転デフォーマ用のパラメータを生成 |  | GEO20 | parameter_create | tracked (GEO20); parameter_create makes them one by one |
| フォームの編集 > フォームをコピー | Ctrl+Shift+C | PAR11 | binding_copy | covered (binding_copy) |
| フォームの編集 > フォームを貼り付け | Ctrl+Shift+V | PAR12 | binding_paste, live2d_form_paste | covered (binding_paste) |
| フォームの編集 > フォームをブレンド | Ctrl+Shift+B | PAR13 | live2d_form_paste | covered (live2d_form_paste) on CMO3; tracked (PAR13) on INX |
| フォームの編集 > フォームの特殊な貼り付けオプション |  | PAR14 | binding_paste | tracked (PAR14); this dialog holds the option set the ledger still has to enumerate |
| フォームの編集 > 複数フォームの貼り付け時に適用先を確認 |  | PAR12 |  | no counterpart (Editor product); tools name their targets |
| フォームの編集 > 反転 |  | GEO03, PAR20 | binding_keypoint, mesh_mirror, parameter_mirror | covered (binding_keypoint); mirror_horizontal and mirror_vertical |
| フォームの編集 > 倍率を指定して拡大縮小 |  |  | node_transform | covered (node_transform); scale |
| フォームの編集 > ワープデフォーマをベジェハンドルに沿って整形 |  |  | deformer_bezier_warp, deformer_bezier_controls | covered (deformer_bezier_warp) |
| フォームの編集 > ワープデフォーマを子がはみ出さないように広げる |  | GEO15 | deformer_extend_grid | covered (deformer_extend_grid) |
| フォームの編集 > 元の形状に戻す |  | GEO16, GEO05 | binding_keypoint | covered (binding_keypoint); reset |
| フォームの編集 > デフォーマの元の形状を更新 |  | GEO16 |  | tracked (GEO16) |
| フォームの編集 > すべてのデフォーマの元の形状を削除 |  | GEO16 |  | tracked (GEO16) |
| 一時変形ツール > 拡大・縮小 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 回転 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 自由変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 遠近変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > ゆがみ変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時ワープ変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 列を増やす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 列を減らす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 行を増やす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 行を減らす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時パス変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時変形ツールを解除 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| クリッピング > クリッピングマスクを選択 |  | REN02 | node_get, editor_selection_set | covered (editor_selection_set); node_get lists the masks |
| クリッピング > クリッピングを逆引き |  | REN02 | nodes_list, node_get, editor_selection_set | covered (editor_selection_set); the client searches the mask lists |
| モデルテンプレート > テンプレートを適用 |  | IO09 | model_template_apply | covered (model_template_apply) |
| モデルテンプレート > テンプレート書き出し |  | IO08 | model_template_export | covered (model_template_export) |
| テクスチャアトラス > テクスチャアトラス編集 | Ctrl+T | IO04 | atlas_build, atlas_layout_set, atlas_layout_clear, atlas_pack | covered (atlas_layout_set) |
| テクスチャアトラス > モデル用画像とテクスチャアトラスを切り替え | T | IO04 |  | absent; the viewport has no atlas mode |
| テクスチャアトラス > テクスチャアトラスからモデル用画像に反映(beta) |  | IO05 | texture_replace, texture_set | covered (texture_replace) |
| ロック > 選択オブジェクトをロック/解除 | Ctrl+2 | UI12 | node_lock_set | covered (node_lock_set) |
| ロック > 未選択オブジェクトをロック | Ctrl+Shift+2 | UI12 | node_lock_set, nodes_list | covered (node_lock_set) |
| ロック > すべてのロックを解除 | Ctrl+Alt+2 | UI12 | node_lock_set | covered (node_lock_set) |
| 表示/非表示 > 選択オブジェクトを表示/非表示 | Ctrl+3 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 表示/非表示 > 未選択オブジェクトを隠す | Ctrl+Shift+3 | UI12 | node_visibility_set, nodes_list | covered (node_visibility_set) |
| 表示/非表示 > すべてを表示 | Ctrl+Alt+3 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 表示/非表示 > 下絵を表示/非表示 | Ctrl+Alt+Shift+3 | UI07 | sketch_snapshot | absent; sketch_snapshot writes the sketch image, nothing hides it |
| 表示/非表示 > 直前の表示状態を復元 |  | UI12 | undo | covered (undo); node_visibility_set is one undo step |
| 編集レベル > レベル1(詳細) | 1 |  |  | absent; Soul2D has no edit levels |
| 編集レベル > レベル2(通常) | 2 |  |  | absent; Soul2D has no edit levels |
| 編集レベル > レベル3(大枠) | 3 |  |  | absent; Soul2D has no edit levels |
| モデルのIDを変換 |  |  |  | no counterpart (Editor product); Cubism 2 ID conversion |
| モデルのオブジェクトの一括設定 > エクスポート |  | IO06 | object_export_csv | covered (object_export_csv) |
| モデルのオブジェクトの一括設定 > インポート |  | IO07 | object_import_csv | covered (object_import_csv) |
| モデルのパラメータの一括設定 > エクスポート |  | IO06 | object_export_csv | covered (object_export_csv); parameters are rows of the same file |
| モデルのパラメータの一括設定 > インポート |  | IO07 | object_import_csv, parameter_set_axes | covered (object_import_csv); names only, ranges go through parameter_set_axes |
| 物理演算設定 |  | PHY01 | physics_set, live2d_physics3_edit, live2d_physics3_read | covered (physics_set) on INX; tracked (PHY01, #236) on CMO3 |

### アニメーション (Animation)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| 素材 > 選択トラックの素材を再読み込み |  | ANI01, ANI23 | animation_image_sequence | covered (animation_image_sequence) for image sequences; linked INX model tracks are absent |
| 素材 > 選択トラックの素材を差し替え |  | ANI01 | animation_image_set | covered (animation_image_set); image tracks |
| 素材 > 選択トラックの素材を差し替え(全シーン) |  | ANI01 | animation_image_set | covered (animation_image_set); once per animation |
| シーン > シーンを新規作成 |  | ANI05 | animation_create, live2d_animation_scene_add | covered (animation_create) |
| シーン > シーンをインポート |  | ANI05 | merge_inp, motion3_import | covered (merge_inp) |
| シーン > 音声ファイルから自動的にシーンを生成 |  | ANI26 | animation_create, animation_audio_add, animation_motionsync_start, animation_motionsync_bake | covered (animation_motionsync_bake); four calls |
| シーン > ループ編集補助の有効/無効 |  | ANI28 | animation_set | covered (animation_set) |
| トラック > パラメータコントローラのアニメーションベイク |  | ANI18 | animation_controller_bake | covered (animation_controller_bake) |
| トラック > 物理演算のアニメーションベイク |  | ANI25 | animation_physics_bake | covered (animation_physics_bake) |
| トラック > モーションシンクのアニメーションベイク |  | ANI26 | animation_motionsync_bake | covered (animation_motionsync_bake) |
| トラック > 音声ファイルからリップシンクを生成 |  | ANI26 | animation_motionsync_start, animation_motionsync_bake | covered (animation_motionsync_start) |
| トラック > 現在の位置をトラックの始めに | Shift+B | ANI22 | animation_set | covered (animation_set); start offset |
| トラック > 現在の位置をトラックの終わりに | Shift+N | ANI22 | animation_set | covered (animation_set); length |
| トラック > 現在の位置にトラックを移動 | Shift+M | ANI22 | animation_set, animation_image_set, animation_audio_set | covered (animation_set) |
| タイムライン <インジケーター> > 始めに移動 | Home | ANI22 | animation_seek | covered (animation_seek); frame 0 |
| タイムライン <インジケーター> > 終わりに移動 | End | ANI22 | animation_seek | covered (animation_seek); the last frame |
| タイムライン <インジケーター> > ワークエリアの開始点に移動 | Shift+Home | ANI22 | animation_seek | covered (animation_seek); the loop start |
| タイムライン <インジケーター> > ワークエリアの終了点に移動 | Shift+End | ANI22 | animation_seek | covered (animation_seek); the loop end |
| タイムライン <インジケーター> > 1フレーム戻る | PageUp | ANI22 | animation_seek | covered (animation_seek); the client computes the frame |
| タイムライン <インジケーター> > 1フレーム進む | PageDown | ANI22 | animation_seek | covered (animation_seek); the client computes the frame |
| タイムライン <インジケーター> > 1キーフレーム戻る | J | ANI22 | animation_seek | covered (animation_seek); the client takes the key list from snapshot |
| タイムライン <インジケーター> > 1キーフレーム進む | K | ANI22 | animation_seek | covered (animation_seek); the client takes the key list from snapshot |
| タイムライン <インジケーター> > 1キーフレーム戻る(トラック) | Shift+J | ANI22 | animation_seek | covered (animation_seek); the client takes the key list from snapshot |
| タイムライン <インジケーター> > 1キーフレーム進む(トラック) | Shift+K | ANI22 | animation_seek | covered (animation_seek); the client takes the key list from snapshot |
| タイムライン <インジケーター> > 前のマーカーに戻る | Ctrl+J | ANI22 | animation_seek | covered (animation_seek); markers come from snapshot |
| タイムライン <インジケーター> > 次のマーカーに進む | Ctrl+K | ANI22 | animation_seek | covered (animation_seek); markers come from snapshot |
| タイムライン <インジケーター> > 前のオニオンスキンマーカーに戻る | Ctrl+Shift+J | ANI22 | animation_seek | covered (animation_seek); onion frames come from editor_onion_set |
| タイムライン <インジケーター> > 次のオニオンスキンマーカーに進む | Ctrl+Shift+K | ANI22 | animation_seek | covered (animation_seek); onion frames come from editor_onion_set |
| タイムライン <インジケーター> > ワークエリアの開始点に設定 |  | ANI22 | animation_set | covered (animation_set); loop start |
| タイムライン <インジケーター> > ワークエリアの終了点に設定 |  | ANI22 | animation_set | covered (animation_set); loop end |
| タイムライン > ワークエリアの全体表示 | Shift+F |  |  | absent; timeline zoom is panel state |
| タイムライン > シーンの全体表示 |  |  |  | absent; timeline zoom is panel state |
| タイムライン > 拡大・縮小をリセット |  |  |  | absent; timeline zoom is panel state |
| フェード > フェードイン・アウトの設定 |  | ANI16 | motion3_fade_set | covered (motion3_fade_set) on Motion3; tracked (ANI16) on INX |
| フェード > 他のシーンにフェードイン・アウトの設定を反映 |  | ANI16 | motion3_fade_set | covered (motion3_fade_set); once per motion |
| パラメータコントローラ > パラメータコントローラの計算ユニット設定 |  | ANI18 | animation_controller_set | covered (animation_controller_set) |
| パラメータコントローラ > 方向コントローラを反転 |  | ANI18 |  | tracked (ANI18) |
| パラメータコントローラ > 方向コントローラを2つの単点コントローラに変換 |  | ANI18 |  | tracked (ANI18) |
| パラメータコントローラ > 2つの単点コントローラを方向コントローラに変換 |  | ANI18 |  | tracked (ANI18) |
| パラメータコントローラ > ターゲットを追従 |  | ANI18 | animation_controller_set | covered (animation_controller_set); target |
| パラメータコントローラ > 再生前にまとめて計算 |  | ANI18 | animation_controller_bake | covered (animation_controller_bake) |
| パラメータコントローラ > コントローラを表示 |  | ANI18 |  | absent; viewport hint |
| パラメータコントローラ > コントローラの表示設定 |  | ANI18 |  | absent; viewport hint |
| パラメータブックマーク > 選択中のパラメータで表示 |  | ANI09 | animation_bookmark_parameters, animation_bookmark_set | covered (animation_bookmark_set) |
| パラメータブックマーク > 選択中のトラックからラベルを削除 |  | ANI09 | animation_bookmark_delete | covered (animation_bookmark_delete) |
| パラメータブックマーク > ラベルを一括で複製(同一モデルのトラック) |  | ANI09 | animation_bookmark_copy | covered (animation_bookmark_copy) |
| パラメータブックマーク > ラベルを個別に複製 |  | ANI09 | animation_bookmark_copy | covered (animation_bookmark_copy); `to` |
| パラメータブックマーク > すべてのラベルを隠す |  | ANI09 |  | absent; panel display |
| パラメータブックマーク > パラメータブックマークの表示設定 |  | ANI09 |  | absent; panel display |
| マーカーを追加 |  | ANI13 | animation_marker_set | covered (animation_marker_set) |
| 再生 | Enter | ANI22 | animation_advance, editor_playback_set, animation_reset | covered (animation_advance) |
| 編集モードの切り替え | C | ANI07, ANI08 | animation_key_set, animation_lane_set | absent; panel mode, keys and interpolation are edited directly |
| ファイルを収集 |  | EXP07 | export_collect | covered (export_collect) |

### フォームアニメーション (Form Animation)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| デフォーマ > 親のデフォーマを選択 | Ctrl+D |  | editor_selection_set, node_get | covered (editor_selection_set); node_get gives the parent |
| デフォーマ > デフォーマの子要素を選択 | Ctrl+Alt+D |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| 一時変形ツール > 拡大・縮小 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 回転 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 自由変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 遠近変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > ゆがみ変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時ワープ変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 列を増やす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 列を減らす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 行を増やす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 行を減らす |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時パス変形 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| 一時変形ツール > 一時変形ツールを解除 |  | GEO21 | deform_edit | tracked (GEO21); deform_edit keys a form, nothing is temporary |
| クリッピング > クリッピングマスクを選択 |  | REN02 | node_get, editor_selection_set | covered (editor_selection_set); node_get lists the masks |
| クリッピング > クリッピングを逆引き |  | REN02 | nodes_list, node_get, editor_selection_set | covered (editor_selection_set); the client searches the mask lists |
| ロック > 選択オブジェクトをロック/解除 | Ctrl+2 | UI12 | node_lock_set | covered (node_lock_set) |
| ロック > 未選択オブジェクトをロック | Ctrl+Shift+2 | UI12 | node_lock_set, nodes_list | covered (node_lock_set) |
| ロック > すべてのロックを解除 | Ctrl+Alt+2 | UI12 | node_lock_set | covered (node_lock_set) |
| 表示/非表示 > 選択オブジェクトを表示/非表示 | Ctrl+3 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 表示/非表示 > 未選択オブジェクトを隠す | Ctrl+Shift+3 | UI12 | node_visibility_set, nodes_list | covered (node_visibility_set) |
| 表示/非表示 > すべてを表示 | Ctrl+Alt+3 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 表示/非表示 > 直前の表示状態を復元 |  | UI12 | undo | covered (undo); node_visibility_set is one undo step |
| 編集レベル > レベル1(詳細) | 1 |  |  | absent; Soul2D has no edit levels |
| 編集レベル > レベル2(通常) | 2 |  |  | absent; Soul2D has no edit levels |
| 編集レベル > レベル3(大枠) | 3 |  |  | absent; Soul2D has no edit levels |
| フォーム編集キーを追加 |  | ANI19 | animation_form_key | covered (animation_form_key) |
| アニメーションプレビュー設定 |  | ANI20, ANI21 | animation_form_time, animation_frame_step | covered (animation_form_time) |

### ウィンドウ (Window)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| ワークスペース > モデリング |  | UI01 | editor_workspace_apply | covered (editor_workspace_apply) |
| ワークスペース > アニメーション |  | UI01 | editor_workspace_apply | covered (editor_workspace_apply) |
| ワークスペース > フォームアニメーション |  | UI01 | editor_workspace_apply | covered (editor_workspace_apply) |
| ワークスペース > 現在のレイアウトをリセット |  | UI01 | editor_layout_reset | covered (editor_layout_reset) |
| ワークスペース > 初期レイアウトを更新 |  | UI01 | editor_workspace_save | covered (editor_workspace_save) |
| ワークスペース > ワークスペース設定 |  | UI01 | editor_workspace_save, editor_workspace_delete | covered (editor_workspace_save) |
| パレットを隠す／パレットを表示する |  | UI01 | editor_panel_set | covered (editor_panel_set) |
| タブを複製 |  | UI02 | view_add | covered (view_add) |
| 前のタブに切り替え |  | UI02 | view_select, view_list | covered (view_select) |
| 次のタブに切り替え |  | UI02 | view_select, view_list | covered (view_select) |
| すべてのビューの表示位置を合わせる |  | UI02 | editor_camera_sync | covered (editor_camera_sync) |
| ビューエリアの状態をリセット |  |  | editor_camera_fit | covered (editor_camera_fit) |
| デフォーマ |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| インスペクタ |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| ツール詳細 |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| ログ |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| パラメータ |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| パーツ |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| プロジェクト |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| タイムライン |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| シーン |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |
| テンプレート |  | UI01 | editor_panel_set | covered (editor_panel_set); panel names follow the Soul2D editor |

### ヘルプ (Help)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| 使い方マニュアル |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| Live2Dホームページ |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| 製品ページ |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| ダウンロードページ |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| サンプルモデル |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| クリエイターズフォーラム |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| Live2D ヘルプ |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| 動画チュートリアル |  |  |  | no counterpart (Editor product); Soul2D's own addresses go through editor_address_dialog |
| ホーム |  |  |  | no counterpart (Editor product) |
| ライセンスの登録 |  | IO11 |  | no counterpart (Editor product); no licence |
| ライセンスの解除 |  | IO11 |  | no counterpart (Editor product); no licence |
| ライセンスのオンライン認証 |  | IO11 |  | no counterpart (Editor product); no licence |
| 購入ページを開く |  |  |  | no counterpart (Editor product) |
| 操作を可視化 |  |  |  | no counterpart (Editor product) |
| ログファイルを開く |  |  |  | no counterpart (Editor product); the host's stderr holds the log |
| 自動バックアップフォルダを開く |  | IO10 | editor_recovery_list, editor_recovery_restore | covered (editor_recovery_list) |
| 更新履歴 |  |  | editor_about_set | covered (editor_about_set); the version, the history is the repository's |

### メッシュ編集モード (Mesh edit mode)

| Menu entry | Shortcut | Ledger | Soul2D command | Status |
|---|---|---|---|---|
| メッシュ編集を完了 | Ctrl+E | GEO02 | mesh_edit, editor_tool_set | covered (mesh_edit); every edit is one call, there is no mode to leave |
| メッシュの自動生成(メッシュ編集モード) |  | GEO01 | automesh_generate | covered (automesh_generate) |
| エッジ(線)を自動接続 | Ctrl+R | GEO02 | mesh_edit | covered (mesh_edit); Connect |
| メッシュをリセット |  | GEO05 | mesh_edit, automesh_generate | covered (mesh_edit) |
| コピーした形状で貼り付け |  | GEO03 | mesh_paste | covered (mesh_paste) |
| 左右反転 |  | GEO03 | mesh_mirror | covered (mesh_mirror) |
| 上下反転 |  | GEO03 | mesh_mirror | covered (mesh_mirror) |
| 軸に対して反転 |  | GEO03 | mesh_mirror | covered (mesh_mirror) |
| 頂点をマージ |  | GEO05 |  | tracked (GEO05) |
| 選択頂点をグルー・バインド | Ctrl+G | GEO18 | part_set_weld | covered (part_set_weld) |
| 選択頂点のグルー・バインド解除 | Ctrl+Shift+G | GEO18 | part_clear_weld | covered (part_clear_weld) |
| ストロークによるメッシュ割り(β) |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > メッシュ幅を増やす |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > メッシュ幅を減らす |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > 分割を増やす |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > 分割を減らす |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > メッシュ幅の頂点数を変える |  |  |  | absent; stroke meshing |
| ストロークによるメッシュ割り(β) > ストロークの確定 | Shift+E |  |  | absent; stroke meshing |
| サブツール > 矢印選択 | A | GEO02 | mesh_select, editor_tool_set | covered (mesh_select) |
| サブツール > 投げ縄選択 | L | GEO02 | mesh_select | covered (mesh_select); lasso |
| サブツール > 変形ブラシ | B | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| サブツール > 頂点の追加 |  | GEO02 | mesh_edit | covered (mesh_edit); Point |
| サブツール > 頂点の削除 |  | GEO02 | mesh_edit | covered (mesh_edit) |
| サブツール > 消しゴム |  | GEO02 | mesh_edit | covered (mesh_edit); one call per vertex |
| サブツール > 点を増やす |  | GEO02 | mesh_edit | covered (mesh_edit); Edge cutter |
| サブツール > ストロークによるメッシュ割り(β) |  |  |  | absent; stroke meshing |

## Cubism Editor palettes and tool details

Read from the Cubism Editor manual on 2026-10-20 (the palette pages and the tool-detail page with its tool sub-pages). One row per control, option or context-menu entry, 405 rows: ツール詳細 (Tool detail) 73, インスペクタ (Inspector) 90, パーツパレット (Parts palette) 62, デフォーマパレット (Deformer palette) 25, パラメータパレット (Parameter palette) 61, タイムラインパレット (Timeline palette) 62, シーンパレット (Scene palette) 5, プロジェクトパレット (Project palette) 9, ログパレット (Log palette) 6, パレットの基本操作 (Palette basics) 12. Same status vocabulary as the menu bar above.

Counts: covered 248, tracked 66, absent 78, no counterpart 13.

Dialogs follow below; the viewport context menus are the slice after.

### ツール詳細 (Tool detail)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| メッシュの手動編集 > 選択・編集 | GEO02 | mesh_select | covered (mesh_select) |
| メッシュの手動編集 > 投げ縄選択 | GEO02 | mesh_select | covered (mesh_select); lasso |
| メッシュの手動編集 > 点を増やす | GEO02 | mesh_edit | covered (mesh_edit); Edge cutter |
| メッシュの手動編集 > メッシュの自動生成 | GEO01 | automesh_generate | covered (automesh_generate) |
| メッシュの手動編集 > メッシュをリセット | GEO05 | mesh_edit | covered (mesh_edit) |
| メッシュの手動編集 > 自動接続 | GEO02 | mesh_edit | covered (mesh_edit); Connect |
| メッシュの手動編集 > 自動接続の方法を選択 | GEO02 |  | absent; one connect rule |
| メッシュの手動編集 > 自動接続の方法を選択 > メッシュの再構成 | GEO02 |  | absent; one connect rule |
| メッシュの手動編集 > 自動接続の方法を選択 > エッジの外周を保つ | GEO02 |  | absent; one connect rule |
| メッシュの手動編集 > メッシュの4分割 | GEO02 |  | absent |
| メッシュの手動編集 > グルーのバインド | GEO18 | part_set_weld | covered (part_set_weld) |
| メッシュの手動編集 > グルーのバインド解除 | GEO18 | part_clear_weld | covered (part_clear_weld) |
| メッシュの手動編集 > クリック時の選択範囲 |  |  | absent; pointer option |
| メッシュの手動編集 > 変形ブラシ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > ブラシ > 重み | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > ブラシ > ブラシ形状 | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > ブラシ > サイズ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > ブラシ > 角度 | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > ブラシ > 硬さ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| メッシュの手動編集 > 消しゴム | GEO02 | mesh_edit | covered (mesh_edit); one call per vertex |
| メッシュの手動編集 > 消しゴム > サイズ | GEO02 |  | absent; pointer option |
| メッシュの手動編集 > 頂点の追加 | GEO02 | mesh_edit | covered (mesh_edit); Point |
| メッシュの手動編集 > 頂点とエッジ(線)の削除 | GEO02 | mesh_edit | covered (mesh_edit) |
| メッシュの手動編集 > ミラー編集 | GEO03 | mesh_mirror | covered (mesh_mirror) |
| メッシュの手動編集 > ミラー編集 > 軸 | GEO03 | mesh_mirror | covered (mesh_mirror); axis argument |
| メッシュの手動編集 > ミラー編集 > 軸の位置 | GEO03 | mesh_mirror | covered (mesh_mirror); axis position |
| メッシュの手動編集 > ストロークによるメッシュ割り(β) |  |  | absent; stroke meshing |
| メッシュの手動編集 > メッシュ割り設定 > メッシュ幅 |  |  | absent; stroke meshing |
| メッシュの手動編集 > メッシュ割り設定 > 繰り返しの間隔 |  |  | absent; stroke meshing |
| メッシュの手動編集 > メッシュ割り設定 > メッシュ幅の頂点数 |  |  | absent; stroke meshing |
| メッシュの手動編集 > メッシュ割り設定 > メッシュを反転 |  |  | absent; stroke meshing |
| メッシュの手動編集 > メッシュ割り設定 > 既存のメッシュを維持する |  |  | absent; stroke meshing |
| 変形パスツール > コントロールポイントの追加 | GEO19 |  | tracked (GEO19, #169); the INX PathDeformer is another construct |
| 変形パスツール > コントロールポイントを削除 | GEO19 |  | tracked (GEO19, #169); the INX PathDeformer is another construct |
| 変形パスツール > パスの削除 | GEO19 |  | tracked (GEO19, #169); the INX PathDeformer is another construct |
| 変形パスツール > 折れ線 | GEO19 |  | tracked (GEO19, #169); the INX PathDeformer is another construct |
| 変形パスツール > 現在の編集レベルにある変形パスを削除 | GEO19 |  | tracked (GEO19, #169); the INX PathDeformer is another construct |
| ブラシ選択ツール・変形ブラシツール > 変形ブラシ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > 膨張ブラシ | GEO24 |  | tracked (GEO24) |
| ブラシ選択ツール・変形ブラシツール > ワープデフォーマの整形ブラシ |  | deformer_bezier_warp | covered (deformer_bezier_warp); whole lattice, no brush |
| ブラシ選択ツール・変形ブラシツール > 重み | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > ブラシ形状 | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > サイズ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > 角度 | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > 硬さ | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| ブラシ選択ツール・変形ブラシツール > 重みカラー |  |  | absent; display colour |
| グルーツール > グルーの重みA：B | GEO18 |  | tracked (GEO18); weights are read only (live2d_authoring_morph_forms) |
| グルーツール > グルーの重みA | GEO18 |  | tracked (GEO18) |
| グルーツール > グルーの重みB | GEO18 |  | tracked (GEO18) |
| グルーツール > グルーの吸着 | GEO18 | part_set_weld | covered (part_set_weld) |
| グルーツール > 重み | GEO18 |  | absent; brush option |
| グルーツール > ブラシ形状 | GEO18 |  | absent; brush option |
| グルーツール > サイズ | GEO18 |  | absent; brush option |
| グルーツール > 角度 | GEO18 |  | absent; brush option |
| グルーツール > 硬さ | GEO18 |  | absent; brush option |
| グルーツール > 重みカラー | GEO18 |  | absent; display colour |
| アートパスツール > コントロールポイントの追加 | GEO07 | live2d_artpath_point_add | covered (live2d_artpath_point_add); stored points on CMO3, every keyform |
| アートパスツール > コントロールポイントの削除 | GEO07 | live2d_artpath_point_delete | covered (live2d_artpath_point_delete); stored points on CMO3, every keyform |
| アートパスツール > 折れ線 | GEO07 |  | tracked (GEO07, #159) |
| アートパスツール > コントロールポイントに設定されている不透明度を調整する | GEO08 | live2d_artpath_point_set | covered (live2d_artpath_point_set); stored values on CMO3 |
| アートパスツール > コントロールポイントに設定されている太さを調整する | GEO08 | live2d_artpath_point_set | covered (live2d_artpath_point_set); stored values on CMO3 |
| アートパスツール > ストロークでアートパスを作成 | GEO09 |  | tracked (GEO09, #159) |
| アートパスツール > ストロークでアートパスを編集 | GEO09 |  | tracked (GEO09, #159) |
| アートパスツール > アートパスの自動生成ダイアログを開く | GEO06 |  | tracked (GEO06, #159) |
| アートパスツール > アートパスを接続 | GEO07 |  | tracked (GEO07, #159) |
| アートパスツール > アートパスを切断 | GEO07 |  | tracked (GEO07, #159) |
| アートパスツール > アートパスの自動生成 | GEO06 |  | tracked (GEO06, #159) |
| アートパスツール > 追従設定 | GEO09 |  | tracked (GEO09, #159) |
| アートパスツール > デフォルトブラシ設定 | GEO08 |  | tracked (GEO08, #159) |
| パラメータコントローラ > コントローラの選択・編集 | ANI18 | animation_controller_move | covered (animation_controller_move) |
| パラメータコントローラ > 単点コントローラの追加 | ANI18 | animation_controller_add | covered (animation_controller_add) |
| パラメータコントローラ > 方向コントローラの追加 | ANI18 |  | tracked (ANI18) |
| パラメータコントローラ > コントローラの削除 | ANI18 | animation_controller_delete | covered (animation_controller_delete) |

### インスペクタ (Inspector)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| アートメッシュ > 名前 |  | node_rename, live2d_rename | covered (node_rename) |
| アートメッシュ > ID |  | live2d_set_id | covered (live2d_set_id) on CMO3; INX nodes have UUIDs |
| アートメッシュ > パーツ |  | node_move, live2d_reparent | covered (node_move) |
| アートメッシュ > デフォーマ |  | node_move, live2d_reparent | covered (node_move) |
| アートメッシュ > クリッピング | REN02 | node_mask_edit, live2d_source_set | covered (node_mask_edit); on CMO3 the Live2D source forms panel edits the stored mask list (Clipping masks, Apply source settings) |
| アートメッシュ > マスクを反転 | REN03 | node_mask_edit | covered (node_mask_edit); mode |
| アートメッシュ > 描画順 | REN01 | node_property_set, live2d_form_set | covered (node_property_set); zsort; on CMO3 the Live2D source forms panel edits the stored value (Draw order, Apply form settings) |
| アートメッシュ > 不透明度 |  | node_property_set, live2d_form_set_opacity | covered (node_property_set) |
| アートメッシュ > 乗算色 | PAR24 | node_property_set | covered (node_property_set); tint |
| アートメッシュ > スクリーン色 | PAR25 | node_property_set | covered (node_property_set); screenTint |
| アートメッシュ > カラーブレンド |  | Inspector_Apply_PartBlendingMode, live2d_source_set | covered (Inspector_Apply_PartBlendingMode); on CMO3 the Live2D source forms panel has Color composition and Apply source settings |
| アートメッシュ > アルファブレンド |  | live2d_source_set | covered (live2d_source_set) on CMO3 (Live2D source forms panel: Alpha composition, Apply source settings); INX has one blend mode |
| アートメッシュ > カリング |  |  | absent |
| アートメッシュ > ユーザデータ | EXP09 | live2d_user_data_set | covered (live2d_user_data_set) |
| アートメッシュ > 頂点情報 | PAR18 | binding_set_deform, live2d_form_set_vertex | covered (binding_set_deform) |
| ワープデフォーマ > 名前 |  | node_rename, live2d_rename | covered (node_rename) |
| ワープデフォーマ > ID |  | live2d_set_id | covered (live2d_set_id) on CMO3; INX nodes have UUIDs |
| ワープデフォーマ > パーツ |  | node_move, live2d_reparent | covered (node_move) |
| ワープデフォーマ > デフォーマ |  | node_move, live2d_reparent | covered (node_move) |
| ワープデフォーマ > 不透明度 |  | node_property_set, live2d_form_set_opacity | covered (node_property_set) |
| ワープデフォーマ > 乗算色 | PAR24 | node_property_set | covered (node_property_set); tint |
| ワープデフォーマ > スクリーン色 | PAR25 | node_property_set | covered (node_property_set); screenTint |
| ワープデフォーマ > 変換の分割数 | GEO10 | deformer_define_grid | covered (deformer_define_grid) |
| ワープデフォーマ > ベジェ分割数 | GEO10 | deformer_bezier_controls | covered (deformer_bezier_controls) |
| ワープデフォーマ > ベジェ編集のタイプ | GEO10 |  | absent |
| ワープデフォーマ > 頂点を自動整形 |  | deformer_bezier_warp | covered (deformer_bezier_warp) |
| ワープデフォーマ > ベジェ制御点をリセット |  |  | absent |
| ワープデフォーマ > 頂点情報 | GEO10 | deformer_define_vertices | covered (deformer_define_vertices) |
| ワープデフォーマ > 元の形状 | GEO16 |  | tracked (GEO16) |
| ワープデフォーマ > 互換設定 > デフォーマ変換(3.2方式) |  |  | no counterpart (Editor product); legacy algorithm |
| ワープデフォーマ > 互換設定 > ベジェ変換(5.2方式) |  |  | no counterpart (Editor product); legacy algorithm |
| 回転デフォーマ > 名前 |  | node_rename, live2d_rename | covered (node_rename) |
| 回転デフォーマ > ID |  | live2d_set_id | covered (live2d_set_id) on CMO3; INX nodes have UUIDs |
| 回転デフォーマ > パーツ |  | node_move, live2d_reparent | covered (node_move) |
| 回転デフォーマ > デフォーマ |  | node_move, live2d_reparent | covered (node_move) |
| 回転デフォーマ > 角度 | GEO11 | node_transform, binding_set_trs | covered (node_transform) |
| 回転デフォーマ > 基準角度 | GEO11 |  | absent |
| 回転デフォーマ > 倍率 | GEO11 | node_transform | covered (node_transform); scale |
| 回転デフォーマ > 不透明度 |  | node_property_set, live2d_form_set_opacity | covered (node_property_set) |
| 回転デフォーマ > 乗算色 | PAR24 | node_property_set | covered (node_property_set); tint |
| 回転デフォーマ > スクリーン色 | PAR25 | node_property_set | covered (node_property_set); screenTint |
| 回転デフォーマ > 頂点情報 | GEO11 | node_transform | covered (node_transform); the pivot |
| 回転デフォーマ > 元の形状 | GEO16 |  | tracked (GEO16) |
| アートパス > 名前 | GEO07 | live2d_rename | covered (existing source name API; native control/rendering/vendor acceptance separate) |
| アートパス > ID | GEO07 | live2d_set_id | covered (live2d_set_id) |
| アートパス > パーツ | GEO07 | live2d_reparent | covered (live2d_reparent) |
| アートパス > デフォーマ | GEO07 | live2d_reparent | covered (live2d_reparent) |
| アートパス > クリッピング | GEO08 | live2d_source_set | covered (Live2D source forms > Clipping masks > Apply source settings; rendering/vendor acceptance separate) |
| アートパス > マスクを反転 | GEO08 | live2d_source_set | covered (Live2D source forms > Invert clipping mask > Apply source settings; existing private boolean storage required) |
| アートパス > 描画順 | GEO08 | live2d_form_set | covered (Live2D source forms > Draw order > Apply form settings; existing private primary-form token required) |
| アートパス > 不透明度 | GEO08 | live2d_form_set_opacity, live2d_form_set_opacities | covered (existing private primary-form opacity token; GUI: Live2D source forms > Opacity / Apply opacity, actual coordinate identity; native GUI/rendering/vendor acceptance separate) |
| アートパス > 乗算色 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > スクリーン色 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > 分割数 | GEO08 | live2d_source_set | covered (Live2D source forms > Division count > Apply source settings; existing private integer storage required) |
| アートパス > 端点をつなぐ | GEO08 | live2d_source_set | covered (Live2D source forms > Closed > Apply source settings; existing private boolean storage required) |
| アートパス > 線の生成方式 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > ブラシ | GEO08 | live2d_source_set | covered (Live2D source forms > Brush > Apply source settings; listed GUID choices, current unknown retained; rendering/vendor acceptance separate) |
| アートパス > コントロールポイント > 線幅0を維持 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > コントロールポイント > 線幅 | GEO08 | live2d_artpath_point_set | covered (live2d_artpath_point_set) |
| アートパス > コントロールポイント > 線色 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > コントロールポイント > 不透明度 | GEO08 | live2d_artpath_point_set | covered (live2d_artpath_point_set) |
| アートパス > コントロールポイント > 折れ線 | GEO08 |  | tracked (GEO08, #159) |
| アートパス > コントロールポイント > 頂点情報 | GEO08 | live2d_authoring_read | covered (live2d_authoring_read) as a read |
| パーツ > 名前 |  | node_rename, live2d_rename | covered (node_rename) |
| パーツ > ID |  | live2d_set_id | covered (live2d_set_id) on CMO3; INX nodes have UUIDs |
| パーツ > パーツ |  | node_move, live2d_reparent | covered (node_move) |
| パーツ > グループ化 | REN01 |  | tracked (REN01) |
| パーツ > 下絵 | UI07 |  | absent |
| パーツ > オフスクリーン描画 | REN04 | node_convert | covered (node_convert); Composite |
| パーツ > 描画順 | REN01 | node_property_set | covered (node_property_set); zsort |
| パーツ > (オフスクリーン描画設定時) クリッピング | REN06 | node_mask_edit | covered (node_mask_edit) |
| パーツ > (オフスクリーン描画設定時) マスクを反転 | REN06 | node_mask_edit | covered (node_mask_edit); mode |
| パーツ > (オフスクリーン描画設定時) 不透明度 | REN05 | Inspector_Apply_CompositeOpacity | covered (Inspector_Apply_CompositeOpacity) |
| パーツ > (オフスクリーン描画設定時) 乗算色 | REN07 | Inspector_Apply_CompositeTint | covered (Inspector_Apply_CompositeTint) |
| パーツ > (オフスクリーン描画設定時) スクリーン色 | REN08 | Inspector_Apply_CompositeScreenTint | covered (Inspector_Apply_CompositeScreenTint) |
| パーツ > (オフスクリーン描画設定時) カラーブレンド |  | Inspector_Apply_CompositeBlendingMode | covered (Inspector_Apply_CompositeBlendingMode) |
| パーツ > (オフスクリーン描画設定時) アルファブレンド |  |  | absent |
| 変形パス > 線変形の幅 | GEO19 |  | tracked (GEO19, #169) |
| 変形パス > 線変形の硬さ | GEO19 |  | tracked (GEO19, #169) |
| 変形パス > 折れ線 | GEO19 |  | tracked (GEO19, #169) |
| 変形パス > メイン | GEO19 |  | tracked (GEO19, #169) |
| 変形パス > サブ | GEO19 |  | tracked (GEO19, #169) |
| グルー > 名前 | GEO18 | live2d_rename | covered (live2d_rename) |
| グルー > ID | GEO18 | live2d_set_id | covered (live2d_set_id) |
| グルー > パーツ | GEO18 | live2d_reparent | covered (live2d_reparent) |
| グルー > 適用度 | GEO18 |  | tracked (GEO18); read only through live2d_authoring_morph_forms |
| その他 > ブレンドシェイプ | PAR21 | binding_weight_limits_set | tracked (PAR21) |
| その他 > ロック時の挙動 > オブジェクトのロック時にインスペクタをロック |  |  | no counterpart (Editor product) |
| その他 > アニメーションビュー > シーン | ANI05 | animation_set, live2d_animation_scene_set | covered (animation_set) |
| その他 > アニメーションビュー > パラメータコントローラ | ANI18 | animation_controller_set | covered (animation_controller_set) |

### パーツパレット (Parts palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| 上部ツールバー > テキストによる絞り込み |  | nodes_list, object_find | covered (object_find) |
| 上部ツールバー > テキストによる絞り込みの入力ボックス |  | object_find | covered (object_find) |
| 上部ツールバー > アートメッシュの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set); the client filters nodes_list by type |
| 上部ツールバー > アートパスの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set); the client filters nodes_list by type |
| 上部ツールバー > ワープデフォーマの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set); the client filters nodes_list by type |
| 上部ツールバー > 回転デフォーマの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set); the client filters nodes_list by type |
| 上部ツールバー > グルーの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set); the client filters nodes_list by type |
| 上部ツールバー > すべてを表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 上部ツールバー > すべてをロック／解除 | UI12 | node_lock_set | covered (node_lock_set) |
| 上部ツールバー > すべて展開／すべて折りたたみ |  |  | absent; panel state |
| 上部ツールバー > 選択状態に連動してツリーを開く |  |  | absent; panel state |
| 上部ツールバー > ドラッグ＆ドロップを禁止 |  |  | absent; panel state |
| 上部ツールバー > パレットメニュー |  |  | covered (see the palette menu rows) |
| 上部ツールバー > 下絵パーツ | UI07 |  | absent |
| 上部ツールバー > パーツを新規作成 |  | node_create | covered (node_create) |
| 上部ツールバー > 選択したパーツ／オブジェクトを削除 |  | node_delete | covered (node_delete) |
| ツリー項目 > パーツ |  | nodes_list | covered (nodes_list) |
| ツリー項目 > 描画順グループ | REN01 |  | tracked (REN01) |
| ツリー項目 > オブジェクトの種別を表すアイコン／オブジェクト名／描画順 |  | nodes_list, node_get | covered (node_get) as a read |
| ツリー項目 > オフスクリーン描画 | REN04 | node_get | covered (node_get) as a read |
| 個別・複数操作 > 各オブジェクトのボタン(表示・ロック・展開) | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); expansion is panel state |
| 個別・複数操作 > 目・鍵アイコンのドラッグ | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); several nodes in one call |
| 個別・複数操作 > Ctrl＋クリック(目・鍵アイコン) | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); the client lists the siblings |
| 個別・複数操作 > Ctrl＋クリック(すべてを表示／非表示・すべてをロック／解除) | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); recursive |
| オブジェクトのロック > ビューエリア | UI12 | node_lock_set | covered (node_lock_set); locked nodes refuse viewport edits |
| オブジェクトのロック > パーツ／デフォーマパレット | UI12 | node_move | covered (node_move) |
| オブジェクトのロック > インスペクタパレット | UI12 | node_property_set | covered (node_property_set) |
| オブジェクトのロック > [オブジェクトのロック時にインスペクタをロック] |  |  | no counterpart (Editor product) |
| 新規パーツ作成ダイアログ > 名前 |  | node_create | covered (node_create) |
| 新規パーツ作成ダイアログ > パーツID |  |  | no counterpart (Editor product); INX nodes have UUIDs |
| 新規パーツ作成ダイアログ > 描画順 | REN01 | node_property_set | covered (node_property_set); zsort |
| 新規パーツ作成ダイアログ > 選択オブジェクトを子に入れる |  | node_insert, node_move | covered (node_insert) |
| 新規パーツ作成ダイアログ > [IDのルールを表示] |  |  | no counterpart (Editor product) |
| コンテキストメニュー > ラベル色 |  |  | absent; nodes have no label colour |
| コンテキストメニュー > ラベル色 > 詳細設定 |  |  | absent; nodes have no label colour |
| コンテキストメニュー > ラベル色 > 子への影響度 |  |  | absent; nodes have no label colour |
| コンテキストメニュー > ラベル色 > 子への影響度設定 |  |  | absent; nodes have no label colour |
| コンテキストメニュー > 子のオブジェクトに対する操作 | UI12 | editor_selection_set, node_visibility_set, node_lock_set | covered (node_visibility_set); recursive |
| コンテキストメニュー > 子のオブジェクトに対する操作 > 全て選択 |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| コンテキストメニュー > 子以下のパーツに対する操作 | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); recursive |
| コンテキストメニュー > 子以下のパーツに対する操作 > 全て選択 |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| コンテキストメニュー > 空のパーツを削除 |  | nodes_list, node_delete | covered (node_delete); the client finds the empty Parts |
| コンテキストメニュー > 同じ階層のオブジェクトに対する操作 | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set) |
| コンテキストメニュー > 描画順グループ化 | REN01 |  | tracked (REN01) |
| コンテキストメニュー > 下絵設定を有効／無効にする | UI07 |  | absent |
| コンテキストメニュー > アートメッシュの描画順を一括変更 | REN01 | binding_multi_edit | covered (binding_multi_edit); zSort |
| コンテキストメニュー > パーツ書き出し | IO08 | model_template_export | absent; no .cmp3, model_template_export writes the whole tree |
| コンテキストメニュー > 切り取り |  | node_cut | covered (node_cut) |
| コンテキストメニュー > コピー |  | node_copy | covered (node_copy) |
| コンテキストメニュー > 貼り付け |  | node_paste | covered (node_paste) |
| コンテキストメニュー > 移動の対象にする |  | node_move | covered (node_move) |
| コンテキストメニュー > ここに移動 |  | node_move | covered (node_move) |
| コンテキストメニュー > クリッピングを逆引き | REN02 | nodes_list, node_get, editor_selection_set | covered (editor_selection_set); the client searches the mask lists |
| パレットメニュー > 列を表示 |  |  | absent; panel state |
| パレットメニュー > 列を表示 > アルファブレンド |  |  | absent; panel state |
| パレットメニュー > パーツ設定 |  | node_rename | covered (node_rename) |
| パレットメニュー > パーツ設定 > 名前 |  | node_rename | covered (node_rename) |
| パレットメニュー > パーツ設定 > パーツID |  |  | no counterpart (Editor product); INX nodes have UUIDs |
| パレットメニュー > すべてのアートメッシュの描画順を変更 | REN01 | node_property_set | covered (node_property_set); zsort |
| パレットメニュー > 空のパーツを削除 |  | nodes_list, node_delete | covered (node_delete) |
| パレットメニュー > 空のデフォーマを削除 |  | nodes_list, node_delete | covered (node_delete) |
| 環境設定 > [モデリング] > [オブジェクトの貼り付け時に名前の末尾に番号を付ける] |  | node_paste | absent; node_paste keeps the names |

### デフォーマパレット (Deformer palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| 全体操作 > すべてを表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 全体操作 > すべてをロック／解除 | UI12 | node_lock_set | covered (node_lock_set) |
| 全体操作 > すべて展開／すべて折りたたみ |  |  | absent; panel state |
| 全体操作 > 選択状態に連動してツリーを開く |  |  | absent; panel state |
| 全体操作 > ドラッグ＆ドロップを禁止 |  |  | absent; panel state |
| 個別操作 > 各オブジェクトの表示ボタン | UI12 | node_visibility_set | covered (node_visibility_set) |
| 個別操作 > 各オブジェクトのロックボタン | UI12 | node_lock_set | covered (node_lock_set) |
| 個別操作 > 各オブジェクトの展開ボタン |  |  | absent; panel state |
| 複数操作 > 目・鍵アイコン上でのドラッグ | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); several nodes in one call |
| 複数操作 > ［Ctrl］＋クリック(目・鍵アイコン) | UI12 | node_visibility_set, node_lock_set | covered (node_visibility_set); the client lists the siblings |
| オブジェクトの選択 > リスト内クリック |  | editor_selection_set | covered (editor_selection_set) |
| オブジェクトの選択 > ［Shift］＋クリック |  | editor_selection_set | covered (editor_selection_set); a range |
| オブジェクトの選択 > ［Ctrl］＋クリック |  | editor_selection_set | covered (editor_selection_set); several nodes |
| オブジェクトの削除 > ［Delete］キー／［Back space］キー |  | node_delete | covered (node_delete) |
| デフォーマの親子関係の設定 > ドラッグ＆ドロップ(デフォーマ上) | GEO12 | node_move, live2d_reparent, live2d_reparent_deformer | covered (node_move) on INX; CMO3 has Part-only movement and bounded static ArtMesh rotation-chain retargeting; keyed/Warp/general nested reparenting remains tracked (GEO12, #162) |
| デフォーマの親子関係の設定 > 同じ親を持つ子オブジェクトの順序 |  | node_move | covered (node_move); index |
| コンテキストメニュー > 子を含めて選択 |  | editor_selection_set, nodes_list | covered (editor_selection_set) |
| コンテキストメニュー > このデフォーマ以下を折りたたみ |  |  | absent; panel state |
| コンテキストメニュー > 切り取り |  | node_cut | covered (node_cut) |
| コンテキストメニュー > コピー |  | node_copy | covered (node_copy) |
| コンテキストメニュー > 貼り付け |  | node_paste | covered (node_paste) |
| コンテキストメニュー > 移動の対象にする |  | node_move | covered (node_move) |
| コンテキストメニュー > ここに移動 |  | node_move | covered (node_move) |
| コンテキストメニュー > 子以下の空のデフォーマを削除 |  | nodes_list, node_delete | covered (node_delete) |
| オプション > デフォーマパレットにパーツパレットのラベル色を反映する |  |  | absent; nodes have no label colour |

### パラメータパレット (Parameter palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| キー編集に使用するボタン > キーの2点追加 | PAR01 | parameter_set_axes, binding_keypoint | covered (parameter_set_axes); axis points |
| キー編集に使用するボタン > キーの3点追加 | PAR01 | parameter_set_axes, binding_keypoint | covered (parameter_set_axes); axis points |
| キー編集に使用するボタン > すべてのキーを削除 | PAR02 | binding_remove, live2d_keyform_delete | covered (binding_remove) |
| キー編集に使用するボタン > キーの手動編集 | PAR01 | parameter_set_axes, live2d_keyform_add | covered (parameter_set_axes) |
| その他のボタン > 絞り込み |  | editor_parameters_set | covered (editor_parameters_set) |
| その他のボタン > ドラッグ＆ドロップを禁止 |  |  | absent; panel state |
| その他のボタン > フォームの特殊な貼り付けオプション | PAR14 | binding_paste | tracked (PAR14) |
| パレットメニュー > デフォルト設定値に戻す |  | values_reset | covered (values_reset) |
| パレットメニュー > デフォルトのフォームをロック | PAR23 | default_form_lock_set | covered (default_form_lock_set) |
| パレットメニュー > パラメータ値をタブ間で同期 | UI02 | view_sync_set | covered (view_sync_set) |
| パレットメニュー > パラメータ設定 | PAR03 | parameter_set_axes, parameter_rename, parameter_set_repeat | covered (parameter_set_axes) |
| パレットメニュー > パラメータグループ設定 | UI13 | parameter_group_create, parameter_move | covered (parameter_group_create) |
| パレットメニュー > すべてのパラメータ値をデフォルトとして設定 | PAR28 | parameter_set_all_defaults | covered (parameter_set_all_defaults) |
| パレットメニュー > 四隅のフォームを自動生成 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| パレットメニュー > 動きの反転 | PAR20 | parameter_mirror, parameter_duplicate_flip | covered (parameter_mirror) on INX; tracked (PAR20, #184) on CMO3 |
| パレットメニュー > 顔の動きを自動生成 > 顔のデフォーマを生成 | PAR16 | deformer_generate_humanoid | covered (deformer_generate_humanoid) |
| パレットメニュー > 顔の動きを自動生成 > 顔の動きを生成 | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn) |
| パレットメニュー > 揺れの動きを自動生成 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| パレットメニュー > 3D回転表現の適用 | PAR26 |  | tracked (PAR26) |
| パレットメニュー > マルチキー編集 | PAR19 | binding_multi_edit | covered (binding_multi_edit) |
| パレットメニュー > 一括反転 |  | parameter_flip | covered (parameter_flip) |
| パレットメニュー > キーに紐付くオブジェクトを選択 |  | parameters, editor_selection_set | covered (editor_selection_set) |
| パレットメニュー > 選択オブジェクトのパラメータ構成を揃える |  | binding_copy_to | covered (binding_copy_to) |
| パレットメニュー > まばたき・リップシンク設定 | ANI02 | animation_blink_parameters_set | covered (animation_blink_parameters_set) |
| パレットメニュー > 拡張補間 | PAR06, PAR07 | binding_set_interpolation | tracked (PAR06, PAR07, #175); ellipse and SNS refused |
| パレットメニュー > ブレンドシェイプの重みの制限設定 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| パレットメニュー > モーションシンク設定 | PAR27 | animation_motionsync_start | covered (animation_motionsync_start) |
| パレットメニュー > 対応付けられたパラメータの検証 |  | parameters | covered (parameters) as a read |
| パレットメニュー > 空のパラメータを削除 |  | parameters, parameter_delete | covered (parameter_delete); the client finds the unkeyed ones |
| パレットメニュー > 空のパラメータグループを削除 |  | parameters, parameter_group_delete | covered (parameter_group_delete) |
| パラメータバー > 下のパラメータと結合(鎖マーク) | PAR05 | parameter_convert_to_2d, parameter_link | covered (parameter_convert_to_2d) |
| パラメータバー > パラメータ名 > パラメータ編集 | PAR03 | parameter_set_axes, parameter_rename, parameter_set_repeat, live2d_parameter_edit | covered (parameter_set_axes) |
| パラメータバー > パラメータ名 > パラメータ作成 |  | parameter_create | covered (parameter_create) |
| パラメータバー > パラメータ名 > パラメータ複製 |  | parameter_duplicate | covered (parameter_duplicate) |
| パラメータバー > パラメータ名 > フォルダ作成 |  | parameter_group_create | covered (parameter_group_create) |
| パラメータバー > パラメータ名 > 削除 |  | parameter_delete, parameter_group_delete | covered (parameter_delete) |
| パラメータバー > パラメータ名 > 移動の対象にする／ここに移動 | UI13 | parameter_move | covered (parameter_move) |
| パラメータバー > パラメータ名の表示領域の調整(［：］) |  |  | absent; panel state |
| パラメータバー > パラメータスライダ |  | parameters | covered (parameters) as a read |
| パラメータバー > パラメータの値(テキストボックス) |  | value_set, live2d_parameter_set | covered (value_set) |
| スライダのポップアップ > キーフォーム追加 | PAR01 | binding_keypoint, live2d_keyform_add | covered (binding_keypoint); set_current |
| スライダのポップアップ > キーフォーム削除 | PAR02 | binding_keypoint, live2d_keyform_delete | covered (binding_keypoint); unset |
| スライダのポップアップ > 選択 / 反転 / 調整 / 変更 |  | editor_selection_set, binding_keypoint | covered (binding_keypoint) |
| 新規作成・削除 > パラメータ作成 |  | parameter_create | covered (parameter_create) |
| 新規作成・削除 > 新規フォルダの作成 |  | parameter_group_create | covered (parameter_group_create) |
| 新規作成・削除 > 削除 |  | parameter_delete | covered (parameter_delete) |
| パラメータグループ > パラメータグループをすべて展開・折りたたみ |  |  | absent; panel state |
| パラメータグループ > ラベル色 |  | parameter_group_set_color | covered (parameter_group_set_color) |
| パラメータグループ > パラメータ作成 |  | parameter_create | covered (parameter_create) |
| パラメータグループ > パラメータ複製 |  | parameter_duplicate | covered (parameter_duplicate) |
| パラメータグループ > フォルダ作成 |  | parameter_group_create | covered (parameter_group_create) |
| パラメータグループ > 削除 |  | parameter_group_delete | covered (parameter_group_delete) |
| パラメータグループ > 移動の対象にする／ここに移動 | UI13 | parameter_move | covered (parameter_move) |
| 基本操作 > パラメータにキーを追加 | PAR01 | binding_keypoint | covered (binding_keypoint) |
| 基本操作 > パラメータの選択 |  | editor_armed_set | covered (editor_armed_set) |
| 基本操作 > パラメータの順序の変更 | UI13 | parameter_move | covered (parameter_move) |
| 基本操作 > 現在値の設定 |  | value_set | covered (value_set) |
| 基本操作 > 作成済みのキーの選択 |  | editor_armed_set, value_set | covered (value_set); the key value |
| 基本操作 > グループを展開/折りたたみ |  |  | absent; panel state |
| 基本操作 > ブレンドシェイプ(新規パラメータ作成ダイアログ) | PAR21 |  | tracked (PAR21) |
| 基本操作 > Shift＋ドラッグ(結合時) |  | value_set | covered (value_set); one axis |

### タイムラインパレット (Timeline palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| 再生やタイムラインの移動 > 先頭へ戻る | ANI22 | animation_seek | covered (animation_seek) |
| 再生やタイムラインの移動 > １フレーム戻る | ANI22 | animation_seek | covered (animation_seek) |
| 再生やタイムラインの移動 > 再生 | ANI22 | animation_advance | covered (animation_advance) |
| 再生やタイムラインの移動 > １フレーム進む | ANI22 | animation_seek | covered (animation_seek) |
| 再生やタイムラインの移動 > 末尾に移動 | ANI22 | animation_seek | covered (animation_seek) |
| 再生やタイムラインの移動 > すべて展開/すべて折りたたみ |  |  | absent; panel state |
| 再生やタイムラインの移動 > リピート | ANI22 | animation_set | covered (animation_set); loop |
| 再生やタイムラインの移動 > 速度 | ANI22 |  | absent; playback speed |
| トラックおよびプロパティ > トラック |  | snapshot | covered (snapshot) as a read |
| トラックおよびプロパティ > トラック > トラックの複製 |  |  | absent |
| トラックおよびプロパティ > トラック > トラックの削除 |  | animation_image_delete, animation_audio_delete | covered (animation_image_delete) |
| トラックおよびプロパティ > トラック > トラック名の編集 |  |  | absent |
| トラックおよびプロパティ > トラック > ファイル名を利用 |  |  | absent |
| トラックおよびプロパティ > トラック > モーションを読み込み |  | motion3_import | covered (motion3_import) |
| トラックおよびプロパティ > FAボタン | ANI19 | animation_form_key | covered (animation_form_key) |
| トラックおよびプロパティ > パラメータブックマークのメニュー | ANI09 | animation_bookmark_set | covered (animation_bookmark_set) |
| トラックおよびプロパティ > パラメータコントローラのメニュー | ANI18 | animation_controller_set | covered (animation_controller_set) |
| トラックおよびプロパティ > プロパティグループ |  |  | absent; panel state |
| トラックおよびプロパティ > まばたき | ANI02 | animation_blink_set | covered (animation_blink_set) |
| トラックおよびプロパティ > リップシンク | ANI26 | animation_motionsync_start, animation_motionsync_bake | covered (animation_motionsync_bake) |
| トラックおよびプロパティ > パラメータコントローラ | ANI18 | animation_controller_add | covered (animation_controller_add) |
| トラックおよびプロパティ > Live2D パラメータ | ANI07 | animation_key_set | covered (animation_key_set) |
| トラックおよびプロパティ > Live2D パーツ表示 | ANI06 | live2d_animation_curve_add, motion3_pose_switch | covered (live2d_animation_curve_add); PartOpacity on CAN3 |
| トラックおよびプロパティ > 配置 ＆ 不透明度 | ANI04 | animation_image_set, animation_group_set | covered (animation_image_set) |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > 座標 | ANI04 | animation_image_set, animation_group_set | covered (animation_image_set) |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > 倍率X・Y | ANI04 | animation_image_set, animation_group_set | covered (animation_image_set); one scale |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > 回転(度) | ANI04 |  | absent |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > 傾き | ANI04 |  | absent |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > アンカーポイント | ANI04 |  | absent |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > 不透明度 | ANI04 | animation_image_set, animation_group_set | covered (animation_image_set) |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > アートパスの幅の倍率 | GEO06 |  | tracked (GEO06, #159) |
| トラックおよびプロパティ > 配置 ＆ 不透明度 > コマ打ち | ANI24 | animation_frame_step | covered (animation_frame_step) |
| トラックおよびプロパティ > シャイ | ANI10 | animation_shy_set | covered (animation_shy_set) |
| トラックおよびプロパティ > フリーズ | ANI11 | animation_freeze_set | covered (animation_freeze_set) |
| トラックおよびプロパティ > 新規グループの作成 | ANI04 | animation_group_add | covered (animation_group_add) |
| タイムライン(ドープシート表示) > 現在の位置(時間) | ANI22 | animation_seek | covered (animation_seek) |
| タイムライン(ドープシート表示) > グラフエディタ(またはドープシート)に切り替え | ANI08 |  | absent; panel state |
| タイムライン(ドープシート表示) > 時間表示とフレーム数表示の切り替え |  |  | absent; panel state |
| タイムライン(ドープシート表示) > 再生開始位置の固定 |  |  | absent; panel state |
| タイムライン(ドープシート表示) > マーカーとオニオンスキンマーカーの切り替えボタン | ANI13 |  | absent; panel state |
| タイムライン(ドープシート表示) > 外部アプリケーション連携の録画ボタン | PAR09 | animation_record_start, animation_record_stop | covered (animation_record_start) |
| タイムライン(ドープシート表示) > 目盛り |  |  | absent; panel state |
| タイムライン(ドープシート表示) > ワークエリア | ANI22 | animation_set | covered (animation_set); loop markers |
| タイムライン(ドープシート表示) > トラック |  | snapshot | covered (snapshot) as a read |
| タイムライン(ドープシート表示) > キーフレーム | ANI07 | animation_key_set | covered (animation_key_set) |
| タイムライン(ドープシート表示) > インジケーター | ANI22 | animation_seek | covered (animation_seek) |
| タイムライン(ドープシート表示) > タイムラインのズーム |  |  | absent; panel state |
| タイムライン(ドープシート表示) > 間隔調整 | ANI07 |  | tracked (ANI07) |
| タイムライン(ドープシート表示) > グリッドの表示 |  |  | absent; panel state |
| タイムライン(ドープシート表示) > 0フレームより前を編集 |  |  | absent |
| タイムライン(ドープシート表示) > デュレーション | ANI22 | animation_set | covered (animation_set) |
| タイムライン(ドープシート表示) > パレットメニュー |  |  | absent; panel state |
| タイムライン(グラフエディタ表示) > 選択中のパラメータ |  | editor_armed_set | covered (editor_armed_set) |
| タイムライン(グラフエディタ表示) > キーフレーム | ANI08 | animation_key_set | covered (animation_key_set) |
| タイムライン(グラフエディタ表示) > 選択中のキーフレーム |  |  | absent; panel state |
| タイムライン(グラフエディタ表示) > リサイズ |  |  | absent; panel state |
| タイムライン(グラフエディタ表示) > グリッドの表示 |  |  | absent; panel state |
| タイムライン(グラフエディタ表示) > リニア | ANI08 | animation_lane_set | covered (animation_lane_set); Linear |
| タイムライン(グラフエディタ表示) > オ－トスムーズ | ANI08 | animation_lane_set | covered (animation_lane_set); Cubic |
| タイムライン(グラフエディタ表示) > ステップ | ANI08 | animation_lane_set | covered (animation_lane_set); Stepped |
| タイムライン(グラフエディタ表示) > インバースステップ | ANI08 |  | absent |
| タイムライン(グラフエディタ表示) > ベジェ | ANI08 |  | tracked (ANI08) |

### シーンパレット (Scene palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| シーン | ANI05 | animation_create, live2d_animation_scene_add | covered (animation_create) |
| 新規シーンを作成 | ANI05 | animation_create, live2d_animation_scene_add | covered (animation_create) |
| シーンの複製 | ANI05 |  | absent |
| シーンの挿入 | ANI05 |  | absent; animations have no order |
| シーンを削除 | ANI05 | animation_delete, live2d_animation_scene_delete | covered (animation_delete) |

### プロジェクトパレット (Project palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| モデリング > モデルファイル |  | snapshot, live2d_authoring_inspect | covered (snapshot) as a read |
| モデリング > 原画 | IO03 |  | absent; the PSD is not kept after import |
| モデリング > モデル用画像 |  | texture_get | covered (texture_get) as a read |
| アニメーション > アニメーションファイル |  | live2d_animation_list | covered (live2d_animation_list) as a read |
| アニメーション > 画像ファイル | ANI04 | animation_image_read | covered (animation_image_read) as a read |
| アニメーション > モデルファイル | ANI01 | live2d_animation_list | covered (live2d_animation_list) as a read |
| アニメーション > 連番画像ファイル | ANI23 | animation_image_read | covered (animation_image_read) as a read |
| アニメーション > 音声ファイル | ANI03 | snapshot | covered (snapshot) as a read |
| アニメーション > モデル設定ファイル | ANI27 | live2d_runtime_inspect | covered (live2d_runtime_inspect) as a read |

### ログパレット (Log palette)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| ログ |  |  | no counterpart (Editor product); the host's stderr is the log |
| ログ出力のオン／オフ |  |  | no counterpart (Editor product); the host's stderr is the log |
| 表示されているログをコピー |  |  | no counterpart (Editor product); the host's stderr is the log |
| 表示されているログをクリア |  |  | no counterpart (Editor product); the host's stderr is the log |
| 新しいウィンドウで表示 |  |  | no counterpart (Editor product); the host's stderr is the log |
| ログファイルを開く |  |  | no counterpart (Editor product); the host's stderr is the log |

### パレットの基本操作 (Palette basics)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| パレットの挿入 | UI01 |  | absent; panel docking |
| パレットのドッキング | UI01 |  | absent; panel docking |
| タブの切り替え | UI01 |  | absent; panel docking |
| パレットのフローティング | UI01 |  | absent; panel docking |
| フローティングパレットの長さ調整 | UI01 | editor_workspace_save | covered (editor_workspace_save); sizes |
| フローティングパレットの閉じる | UI01 | editor_panel_set | covered (editor_panel_set) |
| 複数のパレットの移動 | UI01 |  | absent; panel docking |
| ［ウィンドウ］メニューからの表示・非表示 | UI01 | editor_panel_set | covered (editor_panel_set) |
| タブ右クリック →［閉じる］ | UI01 | editor_panel_set | covered (editor_panel_set) |
| ［パレットを隠す］ | UI01 | editor_panel_set | covered (editor_panel_set) |
| ［パレットを表示する］ | UI01 | editor_panel_set | covered (editor_panel_set) |
| ［現在のレイアウトをリセット］ | UI01 | editor_layout_reset | covered (editor_layout_reset) |

## nijigenerate-derived D editor, pinned at its last tree

The upstream inventory of #99 section 1 is taken from the earlier development history, which carries the nijigenerate lineage and is not published with this repository: the last tree with the D editor dates from 2026-10-11, and the D editor was removed on 2026-10-12. No network access was used. `doc/rust-module-migration-map.md` records each module's Rust replacement; the rows below point at it and name the current command.

Command registry: the D-era `doc/commands.manifest.json` of that tree listed 378 commands; 148 of them are still in the current manifest under the same id, and the 230 below are not, each with the command that carries its behaviour now (0 unmatched).

Counts over all rows: covered 292, absent 21, no counterpart 3.

### Main menu (`widgets/mainmenu.d`)

| Menu entry | D command | Soul2D command | Status |
|---|---|---|---|
| File > New File | FileCommand_NewFile | new | covered (new) |
| File > Open... | FileCommand_ShowOpenFileDialog | open | covered (open) |
| File > Recent |  | open | covered (open); the editor keeps the list |
| File > Recent > Autosaves |  | editor_recovery_list, editor_recovery_restore | covered (editor_recovery_restore) |
| File > Save | FileCommand_ShowSaveFileDialog | save | covered (save) |
| File > Save As... | FileCommand_ShowSaveFileAsDialog | save | covered (save) |
| File > Import > PSD | FileCommand_ShowImportPSDDialog | import_psd | covered (import_psd) |
| File > Import > PSD Depth Map | FileCommand_ShowImportPSDDepthMapDialog | depth_import_psd | covered (depth_import_psd) |
| File > Import > KRA | FileCommand_ShowImportKRADialog | import_kra | covered (import_kra) |
| File > Import > INP | FileCommand_ShowImportINPDialog | import_inp | covered (import_inp) |
| File > Import > Image Folder | FileCommand_ShowImportImageFolderDialog | import_folder | covered (import_folder) |
| File > Merge > PSD | FileCommand_ShowMergePSDDialog | merge_psd_apply | covered (merge_psd_apply) |
| File > Merge > KRA | FileCommand_ShowMergeKRADialog | merge_kra_apply | covered (merge_kra_apply) |
| File > Merge > Image Files | FileCommand_ShowMergeImageFileDialog | merge_images | covered (merge_images) |
| File > Merge > INP | FileCommand_ShowMergeINPDialog | merge_inp | covered (merge_inp) |
| File > Export > INP | FileCommand_ShowExportToINPDialog | export_inp | covered (export_inp) |
| File > Export > Image > PNG | FileCommand_ShowExportToPNGDialog | export_image | covered (export_image) |
| File > Export > Image > JPEG | FileCommand_ShowExportToJpegDialog | export_image | covered (export_image) |
| File > Export > Image > TGA | FileCommand_ShowExportToTGADialog | export_image | covered (export_image) |
| File > Export > Video | FileCommand_ShowExportToVideoDialog | export_video_start | covered (export_video_start) |
| File > Close Project | FileCommand_CloseProject | new | covered (new) |
| File > Quit | editor_quit | application-lifecycle | bounded non-discarding exit; ACP requires original human consent, successful initiating turn and owned-client release; human Save/Discard remains separate |
| Edit > Undo | EditCommand_Undo | undo | covered (undo) |
| Edit > Redo | EditCommand_Redo | redo | covered (redo) |
| Edit > Cut | NodeCommand_CutNode | node_cut | covered (node_cut) |
| Edit > Copy | NodeCommand_CopyNode | node_copy | covered (node_copy) |
| Edit > Paste | NodeCommand_PasteNode | node_paste | covered (node_paste) |
| Edit > Settings | EditCommand_ShowSettingsWindow | editor_settings_set | covered (editor_settings_set) |
| Edit > Command Browser | EditCommand_ShowCommandBrowserWindow | editor_command_browser_set | covered (editor_command_browser_set); visibility; tools/list supplies discovery |
| Edit > Style Editor / ImGui Debugger / ImGui Metrics / ImGui Stack Tool |  |  | no counterpart (ImGui debug, egui has none) |
| View > Default Layout | ViewCommand_SetDefaultLayout | editor_layout_reset | covered (editor_layout_reset) |
| View > <panel> (14 toggles) | Panel_Toggle_* | editor_panel_set | covered (editor_panel_set) |
| View > Open Configuration Folder |  | editor_address_dialog | covered (editor_address_dialog); settings parent path |
| View > Save Screenshot... | ViewCommand_ShowSaveScreenshotDialog | view_screenshot | covered (view_screenshot) |
| View > Stats for Nerds | ViewCommand_ShowStatusForNerds | editor_settings_set | covered (editor_settings_set) |
| Tools > Import Session Data... | ToolCommand_ShowImportSessionDataDialog | tool_import_session_data | covered (tool_import_session_data) |
| Tools > Premultiply Textures | ToolCommand_PremultTexture | tool_premultiply_textures | covered (tool_premultiply_textures) |
| Tools > Rebleed Textures | ToolCommand_RebleedTexture | tool_rebleed_textures | covered (tool_rebleed_textures) |
| Tools > Regenerate Mipmaps | ToolCommand_RegenerateMipmaps |  | absent; no mipmaps on the CPU path |
| Tools > Generate Fake Layer Names | ToolCommand_GenerateFakeLayerName | tool_generate_fake_layer_names | covered (tool_generate_fake_layer_names) |
| Tools > Attempt Repair Puppet | ToolCommand_AttemptRepairPuppet | tool_repair | covered (tool_repair) |
| Tools > Regenerate Node IDs | ToolCommand_RegenerateNodeIDs | tool_regenerate_node_ids | covered (tool_regenerate_node_ids) |
| Help > Online Documentation / Soul2DLive Documentation / Report a Bug / Request a Feature |  | editor_address_dialog | covered (editor_address_dialog); addresses copied, no browser launch |
| Help > About |  | editor_about_set | covered (editor_about_set) |

### Panels (`panels/*.d`)

| Panel | Rust replacement | Soul2D command | Status |
|---|---|---|---|
| Nodes (`panels/nodes.d`) | editor_ui.rs node tree | nodes_list, node_get, editor_selection_set | covered (nodes_list); camera focus on click and tree collapse not ported |
| Inspector (`panels/inspector/*`, 14 files) | editor_inspector.rs | node_property_set, node_mask_edit, puppet_meta_set, physics_set, deformer_set | covered (node_property_set); texture slot load/save, welding add UI, colour picker, multi-select, pixel snap not ported |
| Parameters (`panels/parameters.d`) | editor_ui.rs Parameters section | parameter_*, binding_*, editor_parameters_set | covered (parameter_set_axes); drag-and-drop grouping and per-node binding tree not ported |
| Armed Parameters (`panels/armedparam.d`) | editor_ui.rs armed block | editor_armed_set | covered (editor_armed_set) |
| Timeline (`panels/timeline.d`, `widgets/timeline.d`) | editor_ui.rs timeline section | animation_key_set, animation_lane_set, animation_seek | covered (animation_key_set); resizable lane headers not ported |
| Animation List (`panels/animlist.d`, `windows/editanim.d`) | editor_ui.rs animation controls | animation_create, animation_delete, animation_rename, animation_set | covered (animation_create) |
| Scene (`panels/scene.d`) | editor colour control | scene_ambient_set | covered (scene_ambient_set); emissive/bloom term not ported |
| Resources (`panels/resource.d`) | editor_ui.rs node tree | node_create, parameter_create | covered (node_create) |
| Tool Settings (`panels/toolsettings.d`) | editor_mesh_tools.rs | editor_tool_set | covered (editor_tool_set) |
| Viewport (`panels/viewport.d`) | editor_viewport.rs | editor_camera_set, editor_viewport_set, view_screenshot | covered (editor_camera_set) |
| History (`panels/actionhistory.d`) | editor_ui.rs history section | history_summary, history_goto, history_clear | covered (history_goto) |
| Logger (`panels/logger.d`) | none |  | no counterpart (empty D module) |
| Shell (`panels/shell/*`) | editor_controller.rs |  | absent; the D shell ran commands by name, MCP tools replace it |
| Agent (`panels/agent.d`) | editor_agent.rs, acp.rs |  | covered (ACP client in editor_agent.rs); auto-start and raw log not ported, no real agent run |
| Rust core / Motion3 (`panels/rustcore*.d`) | editor_shell.rs, editor_motion3.rs | motion3_* | covered (motion3_import) |

### Viewport tools (`viewport/**`)

| Tool | D command | Soul2D command | Status |
|---|---|---|---|
| Vertex: Points (`mesheditor/tools/point.d`) | VertexToolMode_Points | mesh_edit | covered (mesh_edit) |
| Vertex: Connect (`connect.d`) | VertexToolMode_Connect | mesh_edit | covered (mesh_edit) |
| Vertex: Edge cutter (`vertex/mesheditor/tools/edgecutter.d`) | VertexToolMode_EdgeCutter | mesh_edit | covered (mesh_edit) |
| Vertex: Grid (`grid.d`) | VertexToolMode_Grid | mesh_edit, deformer_define_grid | covered (mesh_edit) |
| Vertex: Select / Lasso (`select.d`, `lasso.d`) | VertexToolMode_LassoSelection | mesh_select | covered (mesh_select) |
| Vertex: Brush (`brush.d`, `brushes/*`) | VertexToolMode_Brush | deform_edit | covered (deform_edit); brush shapes are editor state |
| Vertex: Bezier deform (`bezierdeform.d`) | VertexToolMode_BezierDeform, VertexToolMode_AltBezierDeform | deformer_bezier_warp | covered (deformer_bezier_warp) |
| Vertex: One-time deform (`onetimedeform.d`) | VertexToolMode_AltGridDeform |  | absent; not ported (migration map) |
| Vertex: Path deform (`pathdeform.d`) | VertexToolMode_PathDeform | deformer_define_vertices | covered (deformer_define_vertices) |
| Model: deform mode (`model/deform.d`) |  | deform_edit, binding_set_deform | covered (deform_edit) |
| Model: depth-bone overlay (`model/depthboneoverlay.d`) |  | rig_overlay, rig_pick_bone, rig_drag_bone | covered (rig_overlay); overlay in model view not ported |
| Model: onion slice (`model/onionslice.d`) | ViewportCommand_ToggleOnionSlice | editor_onion_set | covered (editor_onion_set) |
| Anim viewport (`anim/package.d`) |  | animation_seek, animation_advance | covered (animation_seek) |
| Automesh (`vertex/automesh/*`: grid, contours, optimum, skeletonize) | AutoMesh_* | automesh_generate | covered (automesh_generate) |
| Depth tools (`viewport/depth/**`, 35 files) | DepthEditorOperationCommand_*, PsdDepthDialogCommand_* | depth_draw_*, depth_ops_*, depth_import_psd | covered (depth_draw_apply); not audited file by file (migration map) |
| Transform handle (`common/transformhandle.d`) |  | node_transform | covered (node_transform) |

### Inspector fields (`panels/inspector/*.d`, labels read from the source)

| Fields | Soul2D command | Status |
|---|---|---|
| Node: Transform (Translation, Rotation, Scale), Sorting, Lock to Root Node, Pin origin to parent mesh, Snap to Pixel | node_transform, node_property_set | covered (node_property_set); pixel snap not ported |
| Drawable: Texture Offset |  | absent; not ported |
| Part: Textures (Load from File, Save to File, Remove), Mode (blend modes), Masks (Mask, Dodge, Inverse, Add, Delete), Clip to Lower, Slice from Lower, Make Emissive, Resize automatically | texture_set, Inspector_Apply_PartBlendingMode, node_mask_edit, Inspector_Apply_PartAutoResizedMesh, Inspector_Apply_PartEmissionStrength | covered (node_mask_edit); texture slot load/save UI not ported |
| Composite: Mode, Masks, Textures, Propagate MeshGroup | Inspector_Apply_CompositeBlendingMode, node_mask_edit | covered (Inspector_Apply_CompositeBlendingMode) |
| GridDeformer: Grid Information, Dynamic Deformation, Depth Bone Sources (Weight, Depth Offset, Depth Scale, Rotation), Influence Rule, Preview | deformer_set, rig_set_source_settings, rig_set_influence_rule, rig_preview_influence | covered (deformer_set) |
| MeshGroup: Dynamic Deformation, Translate origins | Inspector_Apply_MeshGroupDynamic, Inspector_Apply_MeshGroupTranslateChildren | covered (Inspector_Apply_MeshGroupDynamic) |
| PathDeformer: Bezier/Spline, Dynamic mode, Auto-Physics (Pendulum, SpringPendulum), Depth Bone Sources, Influence Rule | Inspector_Apply_PathDeformCurveType, Inspector_Apply_PathDeformDynamic, Inspector_Apply_PathDeformPhysicsEnabled, rig_set_source_settings | covered (Inspector_Apply_PathDeformCurveType) |
| SimplePhysics: model type, map mode (XY, YX, LengthAngle, AngleLength), Local Transform Lock, Unmap | physics_set | covered (physics_set) |
| DepthBone: Rest Head/Tail Position, Rest Roll Angle, Lock Rotation/Translation, Fit Z to Depth, Allow Parent to Targets; Depth Rig: Add Standard Skeleton/Parameters | rig_set_rest, rig_set_constraint, rig_fit_bone_z, rig_add_skeleton, rig_add_standard_parameters | covered (rig_set_rest) |
| Puppet: General Info (Name, Artist(s), Rigger(s), Contact, Copyright, License URL), Rendering Settings (Use Point Filtering), Physics Globals | puppet_meta_set | covered (puppet_meta_set); physics globals and point filtering are absent |
| Camera | Inspector_Apply_ViewportOrigin, node_transform | covered (node_transform) |

### D-era commands no longer in the manifest

| D command | Soul2D command | Status |
|---|---|---|
| Animation_AddCopyAnimationKeyFrame | animation_key_add, animation_key_copy, animation_key_paste | covered (animation_key_add) |
| Animation_AddKeyFrame | animation_key_add, animation_key_copy, animation_key_paste | covered (animation_key_add) |
| Animation_AddPasteAnimationKeyFrame | animation_key_add, animation_key_copy, animation_key_paste | covered (animation_key_add) |
| AutoMesh_ApplyActive | automesh_generate | covered (automesh_generate) |
| AutoMesh_Apply_contour | automesh_generate | covered (automesh_generate) |
| AutoMesh_Apply_grid | automesh_generate | covered (automesh_generate) |
| AutoMesh_Apply_optimum | automesh_generate | covered (automesh_generate) |
| AutoMesh_Apply_skeleton | automesh_generate | covered (automesh_generate) |
| AutoMesh_GetActive | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetConfig_contour | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetConfig_grid | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetConfig_optimum | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetConfig_skeleton | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetSchema | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_GetValues | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_ListProcessors | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetActive | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetAdvanced_contour | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetAdvanced_grid | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetAdvanced_optimum | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetAdvanced_skeletonExtractor | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetConfig_contour | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetConfig_grid | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetConfig_optimum | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetConfig_skeleton | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetPreset | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetPreset_contour | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetPreset_grid | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetPreset_optimum | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetPreset_skeletonExtractor | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetSimple_contour | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetSimple_grid | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetSimple_optimum | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetSimple_skeletonExtractor | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| AutoMesh_SetValues | automesh_generate, automesh_preset_save, automesh_preset_list | covered (automesh_generate); options are call arguments, presets replace the active-processor state |
| BindingCommand_CopyBinding | binding_copy, binding_paste | covered (binding_copy) |
| BindingCommand_FlipDeform | binding_keypoint, parameter_mirror | covered (binding_keypoint); mirror_horizontal, mirror_vertical |
| BindingCommand_InvertKeyFrame | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_MirrorKeyFrameHorizontally | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_MirrorKeyFrameVertically | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_PasteBinding | binding_copy, binding_paste | covered (binding_copy) |
| BindingCommand_RemoveBinding | binding_remove | covered (binding_remove) |
| BindingCommand_ResetKeyFrame | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SetFrom1DMirror | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SetFromDiagonalMirror | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SetFromHorizontalMirror | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SetFromVerticalMirror | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SetInterpolation | binding_set_interpolation | covered (binding_set_interpolation) |
| BindingCommand_SetKeyFrame | binding_keypoint | covered (binding_keypoint) |
| BindingCommand_SymmetrizeDeform | binding_keypoint, parameter_mirror | covered (binding_keypoint); mirror_horizontal, mirror_vertical |
| BindingCommand_UnsetKeyFrame | binding_keypoint | covered (binding_keypoint) |
| DepthEditorOperationCommand_AddEditorDepthOp | depth_ops_add, depth_ops_remove, depth_ops_update | covered (depth_ops_add) |
| DepthEditorOperationCommand_RemoveEditorDepthOp | depth_ops_add, depth_ops_remove, depth_ops_update | covered (depth_ops_add) |
| DepthEditorOperationCommand_UpdateEditorDepthOp | depth_ops_add, depth_ops_remove, depth_ops_update | covered (depth_ops_add) |
| EditCommand_ShowCommandBrowserWindow | editor_command_browser_set | covered (editor_command_browser_set); visibility only |
| EditCommand_ShowSettingsWindow | editor_settings_get, editor_settings_set | covered (editor_settings_set) |
| FileCommand_CloseProject | new | covered (new) |
| FileCommand_ExportINP | export_inp | covered (export_inp) |
| FileCommand_ExportJPEG | export_image | covered (export_image) |
| FileCommand_ExportPNG | export_image | covered (export_image) |
| FileCommand_ExportTGA | export_image | covered (export_image) |
| FileCommand_ExportVideo | export_video_start | covered (export_video_start) |
| FileCommand_ImportKRA | import_kra | covered (import_kra) |
| FileCommand_ImportPSD | import_psd | covered (import_psd) |
| FileCommand_MergeKRA | merge_kra_plan, merge_kra_apply | covered (merge_kra_apply) |
| FileCommand_MergePSD | merge_psd_plan, merge_psd_apply | covered (merge_psd_apply) |
| FileCommand_NewFile | new | covered (new) |
| FileCommand_OpenPSDDepthMapDialog | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd) |
| FileCommand_ShowExportToINPDialog | export_inp | covered (export_inp) |
| FileCommand_ShowExportToJpegDialog | export_image | covered (export_image) |
| FileCommand_ShowExportToPNGDialog | export_image | covered (export_image) |
| FileCommand_ShowExportToTGADialog | export_image | covered (export_image) |
| FileCommand_ShowExportToVideoDialog | export_video_start | covered (export_video_start) |
| FileCommand_ShowImportINPDialog | import_inp | covered (import_inp) |
| FileCommand_ShowImportImageFolderDialog | import_folder | covered (import_folder) |
| FileCommand_ShowImportKRADialog | import_kra | covered (import_kra) |
| FileCommand_ShowImportPSDDepthMapDialog | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd) |
| FileCommand_ShowImportPSDDialog | import_psd | covered (import_psd) |
| FileCommand_ShowMergeINPDialog | merge_inp | covered (merge_inp) |
| FileCommand_ShowMergeImageFileDialog | merge_images | covered (merge_images) |
| FileCommand_ShowMergeKRADialog | merge_kra_plan, merge_kra_apply | covered (merge_kra_apply) |
| FileCommand_ShowMergePSDDialog | merge_psd_plan, merge_psd_apply | covered (merge_psd_apply) |
| FileCommand_ShowOpenFileDialog | open | covered (open) |
| FileCommand_ShowSaveFileAsDialog | save | covered (save) |
| FileCommand_ShowSaveFileDialog | save | covered (save) |
| GroupCommand_ChangeGroupColor | parameter_group_create, parameter_group_delete, parameter_group_set_color, parameter_move | covered (parameter_group_create) |
| GroupCommand_CreateParamGroup | parameter_group_create, parameter_group_delete, parameter_group_set_color, parameter_move | covered (parameter_group_create) |
| GroupCommand_DeleteParamGroup | parameter_group_create, parameter_group_delete, parameter_group_set_color, parameter_move | covered (parameter_group_create) |
| GroupCommand_MoveParameter | parameter_group_create, parameter_group_delete, parameter_group_set_color, parameter_move | covered (parameter_group_create) |
| Inspector_Apply_OffsetX | node_property_set | absent; pixel snap and texture offset are not ported (migration map) |
| Inspector_Apply_OffsetY | node_property_set | absent; pixel snap and texture offset are not ported (migration map) |
| Inspector_Apply_PixelSnap | node_property_set | absent; pixel snap and texture offset are not ported (migration map) |
| Inspector_Apply_ToggleGridDeformDynamic | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_ToggleLockToRoot | node_property_set | covered (node_property_set) |
| Inspector_Apply_ToggleMeshGroupDynamic | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_ToggleMeshGroupTranslateChildren | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_TogglePartAutoResizedMesh | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_TogglePathDeformDynamic | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_TogglePathDeformPhysicsEnabled | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| Inspector_Apply_TogglePinToMesh | node_property_set | covered (node_property_set) |
| Inspector_Apply_TogglePixelSnap | node_property_set | absent; pixel snap and texture offset are not ported (migration map) |
| Inspector_Apply_ToggleSimplePhysicsLocalOnly | deformer_set, physics_set, node_property_set | covered (deformer_set); the toggle is a set with the other value |
| ModelCommand_SetDeformBinding | binding_set_deform | covered (binding_set_deform) |
| NodeCommand_CentralizeNode |  | absent; Recalculate origin is not ported (migration map) |
| NodeCommand_InsertNode | node_insert | covered (node_insert) |
| NodeCommand_ReloadNode |  | absent; reload from the texture file |
| NodeCommand_VertexMode | editor_tool_set, mesh_edit | covered (mesh_edit) |
| NodeMaskCommand_AddMask | node_mask_edit | covered (node_mask_edit) |
| NodeMaskCommand_ChangeMaskMode | node_mask_edit | covered (node_mask_edit) |
| NodeMaskCommand_RemoveMask | node_mask_edit | covered (node_mask_edit) |
| NodeSimplePhysicsCommand_ClearSimplePhysicsParameter | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsAngleDamping | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsFrequency | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsGravity | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsLength | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsLengthDamping | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsLocalOnly | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsMapMode | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsModelType | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsOutputScaleX | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsOutputScaleY | physics_set | covered (physics_set) |
| NodeSimplePhysicsCommand_SetSimplePhysicsParameter | physics_set | covered (physics_set) |
| NodeWeldingCommand_AddWelding | part_set_weld, part_clear_weld | covered (part_set_weld) |
| NodeWeldingCommand_ChangeWeldingWeight | part_set_weld, part_clear_weld | covered (part_set_weld) |
| NodeWeldingCommand_RemoveWelding | part_set_weld, part_clear_weld | covered (part_set_weld) |
| Node_Insert_DepthBone | node_insert, node_create | absent; Mask, SimplePhysics and rig insert are not ported (migration map) |
| Node_Insert_DepthRigRoot | node_insert, node_create | absent; Mask, SimplePhysics and rig insert are not ported (migration map) |
| Node_Insert_Mask | node_insert, node_create | absent; Mask, SimplePhysics and rig insert are not ported (migration map) |
| Node_Insert_SimplePhysics | node_insert, node_create | absent; Mask, SimplePhysics and rig insert are not ported (migration map) |
| PaletteCommand_ListCommand | editor_command_browser_set, tools/list | covered (editor_command_browser_set); visibility and existing tool discovery |
| Panel_Toggle_Agent | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Animation_List | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Armed_Parameters | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_History | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Inspector | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Logger | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Nodes | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Parameters | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Resources | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Scene | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Shell | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Timeline | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Tool_Settings | editor_panel_set | covered (editor_panel_set) |
| Panel_Toggle_Viewport | editor_panel_set | covered (editor_panel_set) |
| ParamCommand_AddMouthParameter | parameter_create_mouth | covered (parameter_create_mouth) |
| ParamCommand_RemoveParameter | parameter_delete | covered (parameter_delete) |
| ParamPropCommand_ApplyParameterPropsAxes | parameter_set_axes | covered (parameter_set_axes) |
| ParameditCommand_ConvertTo2DParam | parameter_convert_to_2d | covered (parameter_convert_to_2d) |
| ParameditCommand_CopyParameter | parameter_copy, parameter_paste | covered (parameter_paste) |
| ParameditCommand_DeleteParameter | parameter_delete | covered (parameter_delete) |
| ParameditCommand_DuplicateParameter | parameter_duplicate | covered (parameter_duplicate) |
| ParameditCommand_DuplicateParameterWithFlip | parameter_duplicate_flip | covered (parameter_duplicate_flip) |
| ParameditCommand_Flip1D | parameter_flip | covered (parameter_flip) |
| ParameditCommand_FlipX | parameter_flip | covered (parameter_flip) |
| ParameditCommand_FlipY | parameter_flip | covered (parameter_flip) |
| ParameditCommand_LinkTo | parameter_link, parameter_unlink | covered (parameter_link) |
| ParameditCommand_MirrorHorizontally | parameter_mirror | covered (parameter_mirror) |
| ParameditCommand_MirrorVertically | parameter_mirror | covered (parameter_mirror) |
| ParameditCommand_MirroredAutoFillDir1 | parameter_mirrored_autofill | covered (parameter_mirrored_autofill) |
| ParameditCommand_MirroredAutoFillDir2 | parameter_mirrored_autofill | covered (parameter_mirrored_autofill) |
| ParameditCommand_MirroredAutoFillDir3 | parameter_mirrored_autofill | covered (parameter_mirrored_autofill) |
| ParameditCommand_MirroredAutoFillDir4 | parameter_mirrored_autofill | covered (parameter_mirrored_autofill) |
| ParameditCommand_PasteParameter | parameter_copy, parameter_paste | covered (parameter_paste) |
| ParameditCommand_PasteParameterWithFlip | parameter_copy, parameter_paste | covered (parameter_paste) |
| ParameditCommand_SetArmedParameterAndKeypoint | editor_armed_set, value_set | covered (editor_armed_set) |
| ParameditCommand_SetParameterKeypoint | editor_armed_set, value_set | covered (editor_armed_set) |
| ParameditCommand_SetStartingKeyFrame | parameter_set_defaults | covered (parameter_set_defaults) |
| ParameditCommand_ToggleParameterArm | editor_armed_set, value_set | covered (editor_armed_set) |
| PsdDepthDialogCommand_ApplyPsdDepthDialog | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_CancelPsdDepthDialog | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_FillPsdDepthDialogAlphaDepthGaps | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_GetPsdDepthDialogPartData | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_InspectPsdDepthDialog | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_ResetPsdDepthDialogLayerDepthTransform | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogAlphaThreshold | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogBackDepth | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogChannel | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogColorSource | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogContourRepair | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogCustomRadius | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogDepthScale | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogDirectGridMatch | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogFrontDepth | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogGpuComposition | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogInvert | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerDepthEnabled | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerDepthInverted | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerDepthOffset | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerDepthScale | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerEnabled | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerMappingAuto | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerMappingIgnored | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerMappingTarget | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogLayerVisible | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogMissingPolicy | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogProblemFilter | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogSampling | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogSurfaceSmoothing | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| PsdDepthDialogCommand_SetPsdDepthDialogTargetEnabled | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the dialog state became call arguments |
| ToolCommand_AnimEditMode | editor_tool_set | covered (editor_tool_set) |
| ToolCommand_ModelEditMode | editor_tool_set | covered (editor_tool_set) |
| ToolCommand_RegenerateMipmaps |  | absent; no mipmaps on the CPU path |
| ToolCommand_ShowImportSessionDataDialog | tool_import_session_data | covered (tool_import_session_data) |
| VertexToolMode_AltBezierDeform | deformer_bezier_warp, deform_edit | covered (deformer_bezier_warp); one-time grid deform is not ported (migration map) |
| VertexToolMode_AltGridDeform | deformer_bezier_warp, deform_edit | covered (deformer_bezier_warp); one-time grid deform is not ported (migration map) |
| VertexToolMode_BezierDeform | deformer_bezier_warp, deform_edit | covered (deformer_bezier_warp); one-time grid deform is not ported (migration map) |
| VertexToolMode_Brush | deform_edit | covered (deform_edit) |
| VertexToolMode_Connect | mesh_edit | covered (mesh_edit) |
| VertexToolMode_EdgeCutter | mesh_edit | covered (mesh_edit) |
| VertexToolMode_Grid | mesh_edit | covered (mesh_edit) |
| VertexToolMode_LassoSelection | mesh_select | covered (mesh_select) |
| VertexToolMode_PathDeform | deformer_define_vertices | covered (deformer_define_vertices) |
| VertexToolMode_Points | mesh_edit | covered (mesh_edit) |
| ViewCommand_SetDefaultLayout | editor_layout_reset | covered (editor_layout_reset) |
| ViewCommand_ShowSaveScreenshotDialog | view_screenshot, ViewCommand_SaveScreenshot | covered (view_screenshot) |
| ViewCommand_ShowStatusForNerds | editor_settings_set | covered (editor_settings_set) |
| ViewCommand_ToggleDifferenceAggregation |  | absent |
| ViewportCommand_AddFlipPair | extdata_get, extdata_set | covered (extdata_set); the caller adds {uuid1, uuid2} to the list extdata_get reads and stores it under key `com.inochi2d.creator.FlipConfig` |
| ViewportCommand_AutoAddFlipPairs | extdata_get, extdata_set, nodes_list | covered (extdata_set); the name-pattern matching is the flip-pair window's Pair button, not a tool: the caller pairs names read with nodes_list and stores the list |
| ViewportCommand_FitViewportToModel | editor_camera_fit, editor_camera_set | covered (editor_camera_fit) |
| ViewportCommand_ListFlipPairs | extdata_get | covered (extdata_get); key `com.inochi2d.creator.FlipConfig`, base64 of a JSON array of {uuid1, uuid2} |
| ViewportCommand_OpenAutomeshBatching | automesh_generate | absent; automesh batching UI is not ported (migration map) |
| ViewportCommand_OpenFlipPairWindow | extdata_get, extdata_set | covered (extdata_get) as a read; the window itself is editor state |
| ViewportCommand_RemoveFlipPair | extdata_get, extdata_set | covered (extdata_set); the caller drops the pair and stores the rest (base64 null removes the key) |
| ViewportCommand_ResetViewportPosition | editor_camera_fit, editor_camera_set | covered (editor_camera_fit) |
| ViewportCommand_ResetViewportZoom | editor_camera_fit, editor_camera_set | covered (editor_camera_fit) |
| ViewportCommand_ToggleMirrorView | editor_viewport_set | absent |
| ViewportCommand_ToggleOnionSlice | editor_onion_set | covered (editor_onion_set) |
| ViewportCommand_TogglePhysics | editor_physics_preview_set | covered (editor_physics_preview_set) |
| ViewportCommand_TogglePostProcess |  | absent; post-processing toggle is not ported (migration map) |

## Cubism Editor dialogs

Read from the Cubism Editor manual on 2026-10-20, the pages that describe the dialogs reached from the menus and palettes above (eighteen pages; the manual has no page for the model statistics, canvas settings, draw-order batch or linked-parameter check dialogs, and the grid page names its two settings in a sentence only). One row per field, option or button, 339 rows: 環境設定 (Preferences) 54, デフォーマの自動生成 (Automatic deformer generation) 7, 揺れの動きを自動生成 (Automatic sway) 24, 顔の動きを自動生成 (Automatic face movement) 14, 3D回転表現の適用 (3D rotation expression) 11, マルチキー編集 (Multi-key edit) 18, まばたき・リップシンク設定 (Blink and lip sync) 3, 物理演算設定 (Physics settings) 50, テクスチャアトラス編集 (Texture atlas edit) 66, 動きの反転 (Motion reflection) 6, フォームの特殊な貼り付けオプション (Special form paste) 16, 四隅のフォームを自動生成 (Corner forms) 10, ブレンドシェイプの重みの制限設定 (Blend shape weight limits) 27, オニオンスキン設定 (Onion skin) 3, グリッド設定 (Grid) 2, ガイド設定 (Guides) 11, ワークスペース設定 (Workspace settings) 7, キーフォーム編集 (Keyform edit) 10. Same status vocabulary as above.

Counts: covered 210, tracked 24, absent 93, no counterpart 12.

The animation-side dialogs follow in the next section.

### 環境設定 (Preferences)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 全般 > カラーテーマ | UI01 | editor_settings_set | covered (editor_settings_set); dark mode |
| 全般 > 言語 | UI01 | editor_settings_set | covered (editor_settings_set); language |
| 全般 > UIの自動拡大を無効にする | UI01 | editor_settings_set | covered (editor_settings_set); UI scale percent |
| 全般 > ダイアログのサイズと位置をリセット |  |  | no counterpart (Editor product) |
| ウィンドウ > タブの選択に合わせてワークスペースを自動切替 | UI01 |  | absent |
| ウィンドウ > オブジェクトのロック時にインスペクタをロック |  |  | no counterpart (Editor product) |
| ウィンドウ > インスペクタパレットのデフォーマ欄に所属パーツ名を表示する |  |  | absent; panel display |
| ウィンドウ > デフォーマパレットにパーツパレットのラベル色を反映する |  |  | absent; nodes have no label colour |
| ウィンドウ > サムネイルアイコンの背景色 |  |  | absent; display style |
| ビューエリア > キャンバスの背景色 |  |  | absent; display style |
| ビューエリア > 濃い色との差 |  |  | absent; display style |
| ビューエリア > チェック柄サイズ |  |  | absent; display style |
| ビューエリア > 色 |  |  | absent; display style |
| ビューエリア > リセット |  |  | absent; display style |
| ビューエリア > 一時的なスナップショット | UI07 | sketch_snapshot | covered (sketch_snapshot) |
| ビューエリア > モデリングの解像度 | UI07 | sketch_snapshot | covered (sketch_snapshot); size argument |
| ビューエリア > アニメーションの解像度 | UI07 | sketch_snapshot | covered (sketch_snapshot); size argument |
| ビューエリア > フォームアニメーションの解像度 | UI07 | sketch_snapshot | covered (sketch_snapshot); size argument |
| ビューエリア > コンテキストメニュー: デフォーマの親子関係を表示 |  | node_get | covered (node_get) as a read |
| オブジェクトのスタイル > アートメッシュ |  |  | absent; display style |
| オブジェクトのスタイル > 頂点の不透明度 |  |  | absent; display style |
| オブジェクトのスタイル > メッシュの線幅 (アートメッシュ) |  |  | absent; display style |
| オブジェクトのスタイル > メッシュの線色 (アートメッシュ) |  |  | absent; display style |
| オブジェクトのスタイル > アートパス |  |  | absent; display style |
| オブジェクトのスタイル > メッシュの線幅 (アートパス) |  |  | absent; display style |
| オブジェクトのスタイル > メッシュの線色 (アートパス) |  |  | absent; display style |
| オブジェクトのスタイル > ワープデフォーマ |  |  | absent; display style |
| オブジェクトのスタイル > グリッドの色 |  |  | absent; display style |
| オブジェクトのスタイル > リサイズ時の子要素の色 |  |  | absent; display style |
| オブジェクトのスタイル > 回転デフォーマ |  |  | absent; display style |
| オブジェクトのスタイル > ハンドルの色 |  |  | absent; display style |
| モデリング > PSDのインポート: 生成されるアートメッシュの内側の余白 | IO01 | import_psd, automesh_generate | covered (automesh_generate); padding is an automesh option |
| モデリング > メッシュ編集: アートメッシュをダブルクリックして開始 |  |  | absent; pointer option |
| モデリング > 回転デフォーマ: 回転ハンドルのShift+ドラッグ操作 |  |  | absent; pointer option |
| モデリング > 角度の刻み |  |  | absent; pointer option |
| モデリング > 角度の小数点以下を切り捨て |  |  | absent; pointer option |
| モデリング > 重なったポリゴンをハイライト |  |  | absent; display style |
| モデリング > オプション: 選択しているオブジェクトをもとに名前を設定する |  | node_create | covered (node_create); the caller names the node |
| モデリング > デフォーマの種類が重複しないように名前を調整する |  | node_rename | covered (node_rename) |
| モデリング > オブジェクトの貼り付け時に名前の末尾に番号を付ける |  | node_paste | absent; node_paste keeps the names |
| モデリング > パラメータの複製時に名前の末尾に番号を付ける |  | parameter_duplicate | absent; parameter_duplicate keeps the name |
| ファイル > 自動バックアップ | IO10 | editor_settings_set | covered (editor_settings_set); autosave settings |
| ファイル > 自動バックアップ間隔(分) | IO10 | editor_settings_set | covered (editor_settings_set) |
| ファイル > 自動バックアップ容量(MB) | IO10 | editor_settings_set | covered (editor_settings_set) |
| ファイル > 自動バックアップフォルダを開く | IO10 | editor_recovery_list | covered (editor_recovery_list) |
| ファイル > ファイルの操作オプション |  |  | absent |
| ファイル > 最近開いたファイルをフルパスで表示する |  |  | absent; panel display |
| ファイル > ファイルの別名保存時に名前の末尾に番号を付ける |  | save | absent; save takes the path |
| ファイル > 保存・書き出し後に保存先の場所を開く |  |  | no counterpart (Editor product); file manager launch |
| ファイル > 最近開いたファイルをクリア |  |  | absent |
| 通知 > 起動時にエディタの更新通知を表示する |  |  | no counterpart (Editor product) |
| 通知 > マスクに関する警告を常に表示する | REN02 | node_mask_edit | covered (node_mask_edit); refusals are answers, not dialogs |
| 通知 > ブレンドモードの見た目に関する警告を常に表示する | REN10 | scene_overlap | covered (scene_overlap) as a read |
| 通知 > 非表示設定にした全てのダイアログを再表示 |  |  | no counterpart (Editor product) |

### デフォーマの自動生成 (Automatic deformer generation)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| デフォーマの自動生成 > 生成されるデフォーマ構成 | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid); the tree is fixed |
| デフォーマの自動生成 > 使用しないアートメッシュ | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid); `nodes` argument |
| デフォーマの自動生成 > アートメッシュを取り除く | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid); `nodes` argument |
| デフォーマの自動生成 > デフォーマ構成に追加 | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid); `nodes` argument |
| デフォーマの自動生成 > デフォーマの形状と位置を左右対称に生成 | GEO13 |  | absent; no symmetric option |
| デフォーマの自動生成 > OK | GEO13 | deformer_generate_humanoid | covered (deformer_generate_humanoid) |
| デフォーマの自動生成 > Cancel |  |  | no counterpart (Editor product); a call has no cancel |

### 揺れの動きを自動生成 (Automatic sway)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 設定ダイアログ > デフォーマリスト | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); `nodes` |
| 設定ダイアログ > ワープデフォーマを登録 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); `nodes` |
| 設定ダイアログ > パラメータ | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); `parameter` |
| 設定ダイアログ > 選択をパラメータパレットと同期 |  |  | absent; panel state |
| 設定ダイアログ > 推定のタイプ | PAR17 |  | absent; no estimate presets |
| 設定ダイアログ > 推定のタイプ > 髪揺れ | PAR17 |  | absent |
| 設定ダイアログ > 推定のタイプ > 角度Zによる垂れ下がりを生成 | PAR17 |  | absent |
| 設定ダイアログ > 支点の位置 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); pivot side |
| 設定ダイアログ > 揺れを左右反転 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); sign of the amplitude |
| 設定ダイアログ > 揺れの設定の調整 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| 設定ダイアログ > 揺れの設定の調整 > 揺れの大きさ(横) | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| 設定ダイアログ > 揺れの設定の調整 > 揺れの大きさ(縦) | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| 設定ダイアログ > 揺れの設定の調整 > 柔らかさ | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| 設定ダイアログ > 揺れの設定の調整 > 拡大・縮小 | PAR17 |  | absent |
| 設定ダイアログ > 左右の設定値を同期 |  |  | absent; dialog state |
| 設定ダイアログ > 設定値を変更したら自動でキーフォームを更新 |  |  | absent; dialog state |
| 設定ダイアログ > キーフォームを更新 | PAR17 | deformer_generate_sway | covered (deformer_generate_sway) |
| デフォーマリストの右クリックメニュー > 選択をビューと同期 |  |  | absent; dialog state |
| デフォーマリストの右クリックメニュー > 表示 |  |  | absent; dialog state |
| デフォーマリストの右クリックメニュー > 設定をコピー | PAR17 |  | absent; settings are call arguments |
| デフォーマリストの右クリックメニュー > 設定を貼り付け | PAR17 |  | absent; settings are call arguments |
| デフォーマリストの右クリックメニュー > 設定を削除 | PAR17 | binding_remove | covered (binding_remove) |
| キーの自動生成の確認ダイアログ > OK | PAR17 | deformer_generate_sway | covered (deformer_generate_sway); replaces the keys |
| キーの自動生成の確認ダイアログ > 取消 |  |  | no counterpart (Editor product) |

### 顔の動きを自動生成 (Automatic face movement)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 設定ダイアログ > 顔のデフォーマを生成 | PAR16 | deformer_generate_humanoid | covered (deformer_generate_humanoid) |
| 設定ダイアログ > 顔の動きを生成 | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn) |
| 顔のデフォーマの自動生成 > 対応パーツ | PAR16 | deformer_generate_humanoid | covered (deformer_generate_humanoid); `nodes` by role |
| 顔のデフォーマの自動生成 > 変換の分割数 | PAR16 | deformer_define_grid | covered (deformer_define_grid) |
| 顔のデフォーマの自動生成 > ベジェの分割数 | PAR16 | deformer_bezier_controls | covered (deformer_bezier_controls) |
| 顔のデフォーマの自動生成 > OK | PAR16 | deformer_generate_humanoid | covered (deformer_generate_humanoid) |
| 顔の動きの自動生成 > 対象デフォーマの選択 | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn); `nodes` |
| 顔の動きの自動生成 > 対象パラメータの選択 | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn); parameters |
| 顔の動きの自動生成 > 動きの生成 > 角度X／角度Y | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn) |
| 顔の動きの自動生成 > 動きの生成 > 四隅 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| 顔の動きの自動生成 > OK | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn) |
| 顔の動きの調整 > シフトレベル | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn); shift amount |
| 顔の動きの調整 > まゆ毛のシフトを目のシフトに合わせる | PAR16 |  | absent |
| 顔の動きの調整 > 変形レベル | PAR16 | deformer_generate_face_turn | covered (deformer_generate_face_turn); deform amount |

### 3D回転表現の適用 (3D rotation expression)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 3D回転表現の適用 > カメラ位置 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 回転の中心 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > Z方向オフセット | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > パースの強さ | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 角度の範囲 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 回転の方式 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 回転の方式 > X方向-Y方向の順 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 回転の方式 > Y方向-X方向の順 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 回転の方式 > XY方向同時 | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 3D形状 > 左右対称にする | PAR26 |  | tracked (PAR26) |
| 3D回転表現の適用 > 3D形状 > 上下対称にする | PAR26 |  | tracked (PAR26) |

### マルチキー編集 (Multi-key edit)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| マルチキー編集 > 全選択/解除 | PAR19 | binding_multi_edit | covered (binding_multi_edit); `nodes` |
| マルチキー編集 > パラメータチェックボックス | PAR19 | binding_multi_edit | covered (binding_multi_edit); parameters |
| マルチキー編集 > 各パラメータ上のキーフォーム | PAR19 | binding_multi_edit | covered (binding_multi_edit); keys |
| マルチキー編集 > 描画順(チェックボックス) | PAR19 | binding_multi_edit | covered (binding_multi_edit); channel zSort |
| マルチキー編集 > 描画順(数値入力、0〜1000) | PAR19 | binding_multi_edit | covered (binding_multi_edit) |
| マルチキー編集 > 描画順(簡易設定ボックス) |  |  | absent; dialog shortcut |
| マルチキー編集 > 不透明度(チェックボックス) | PAR19 | binding_multi_edit | covered (binding_multi_edit); channel opacity |
| マルチキー編集 > 不透明度(数値入力、0〜100%) | PAR19 | binding_multi_edit | covered (binding_multi_edit) |
| マルチキー編集 > 乗算色 | PAR19 | binding_multi_edit | covered (binding_multi_edit); channel tint |
| マルチキー編集 > スクリーン色 | PAR19 | binding_multi_edit | covered (binding_multi_edit); channel screenTint |
| マルチキー編集 > 上書き | PAR19 | binding_multi_edit | covered (binding_multi_edit); mode |
| マルチキー編集 > 加算 | PAR19 | binding_multi_edit | covered (binding_multi_edit); mode |
| マルチキー編集 > 乗算 | PAR19 | binding_multi_edit | covered (binding_multi_edit); mode |
| マルチキー編集 > スクリーン | PAR19 | binding_multi_edit | covered (binding_multi_edit); mode |
| マルチキー編集 > ヒント情報 | PAR19 | binding_multi_edit | covered (binding_multi_edit); the answer lists before and after |
| マルチキー編集 > 変更されないオブジェクト | PAR19 | binding_multi_edit | covered (binding_multi_edit); the answer lists them |
| マルチキー編集 > 変更されるオブジェクト | PAR19 | binding_multi_edit | covered (binding_multi_edit); the answer lists them |
| マルチキー編集 > OK | PAR19 | binding_multi_edit | covered (binding_multi_edit) |

Native Soul2D acceptance on 2026-10-24 exercised the existing source-form Angle,
Draw order, Clipping masks, Invert clipping mask and Use offscreen controls on
a 2026-10-24 build, including native Apply, change-back no-op, Undo/Redo and
save/reopen. The first four used an existing input; Use offscreen used a small
private synthetic Part fixture. Native access to the larger offscreen input
timed out and remains INCONCLUSIVE. The independent file checks and limits are in
`build/verification/goal-20261018/handoff-20261022/native-fields-result.json`.
This establishes these bounded Soul2D controls, without vendor or pixel parity.

A later run on `c0f431a9` completed native Use offscreen on the larger real input
(true to false). It passed one-edit history, change-back no-op, native Undo/Redo,
save/reopen and independent boolean-only XML/container checks. The Undo file
matched the input bytes. `large-offscreen-native-result.json` in the same evidence
directory supersedes the earlier unattempted case. Native observations still
timed out intermittently; no performance or vendor/pixel parity claim follows.

### まばたき・リップシンク設定 (Blink and lip sync)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| まばたき・リップシンク設定 > 設定したい項目 | ANI02 | animation_blink_parameters_set, parameter_create_mouth | covered (animation_blink_parameters_set) |
| まばたき・リップシンク設定 > まばたき | ANI02 | animation_blink_parameters_set | covered (animation_blink_parameters_set); open and closed values |
| まばたき・リップシンク設定 > 効果 | ANI02 |  | absent; no strength |

### 物理演算設定 (Physics settings)

Native Soul2D acceptance on 2026-10-24 also exercised the existing CMO3 vertices-preset
Apply, Save, Replace and Delete buttons in an isolated macOS configuration. Apply
uses one Undo step; store actions preserve the document snapshot and history.
The receipt is `build/verification/goal-20261018/handoff-20261022/vertices-native-result.json`.
This does not establish the remaining physics controls or Cubism menu semantics.

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 切り替えタブ > 物理演算 | PHY01 | physics_set, live2d_physics3_edit | covered (physics_set) on INX; tracked (PHY01, #236) on CMO3 |
| 切り替えタブ > プレイリスト | PHY01 | animation_playlist_set | covered (animation_playlist_set) |
| 編集 > 元に戻す | PHY01 | undo | covered (undo) |
| 編集 > やり直す | PHY01 | redo | covered (redo) |
| 編集 > 出力行のコピー | PHY01 | live2d_physics3_read | covered (live2d_physics3_read) as a read |
| 編集 > 出力行のペースト | PHY01 | live2d_physics3_edit | covered (live2d_physics3_edit) |
| 物理演算 > 物理演算を有効にする | PHY01 | editor_physics_preview_set | covered (editor_physics_preview_set) |
| 物理演算 > 物理演算設定の読み込み | PHY01 | live2d_physics_attach | covered (live2d_physics_attach) |
| 物理演算 > 物理演算設定の書き出し | PHY01 | live2d_physics3_export | covered (live2d_physics3_export) |
| 物理演算 > プリセットのインポート | PHY01 | physics_preset_save, live2d_physics3_preset_save | covered (physics_preset_save) on INX; covered (live2d_physics3_preset_save) on CMO3 |
| 物理演算 > プリセットのエクスポート | PHY01 | physics_preset_list, live2d_physics3_preset_list | covered (physics_preset_list) on INX; covered (live2d_physics3_preset_list) on CMO3 |
| 物理演算 > 入力プリセットの初期化 | PHY01 | physics_preset_delete, live2d_physics3_preset_delete | covered (physics_preset_delete) on INX; covered (live2d_physics3_preset_delete) on CMO3 |
| 物理演算 > 物理モデルプリセットの初期化 | PHY01 | physics_preset_delete, live2d_physics3_preset_delete | covered (physics_preset_delete) on INX; covered (live2d_physics3_preset_delete) on CMO3 |
| 物理演算 > 計算FPSを初期化 | PHY01 |  | absent |
| プレイリスト > cmo3・can3連携設定 | PHY01 | live2d_animation_attach | covered (live2d_animation_attach) |
| プレビュー > 当たり判定の設定 | PHY01 |  | absent; hit areas |
| プレビュー > カーソル追従 | PHY01 | value_set | covered (value_set); the client drives the parameters |
| プレビュー > 自動まばたき | PHY01 | animation_blink_set | covered (animation_blink_set) |
| プレビュー > 呼吸 | PHY01 |  | absent |
| プレビュー > カーソル追従の設定 > 影響度(％) | PHY01 |  | absent |
| プレビュー > カーソル追従の設定 > 反転 | PHY01 |  | absent |
| プレビュー > カーソル追従の設定 > 種別 | PHY01 |  | absent |
| プレビュー > ランダムポーズ設定 | PHY01 | random_pose_seek | covered (random_pose_seek) |
| プレビュー > 外部アプリケーション連携設定 | PHY01 |  | no counterpart (Editor product); API01 |
| プレビュー > 録画設定 > 新しいアニメーションデータを生成 | PHY01 | animation_record_start | covered (animation_record_start) |
| プレビュー > 録画設定 > 現在のアニメーションデータに追加 | PHY01 | animation_record_start | covered (animation_record_start); `name` |
| ヘルプ > 物理演算設定について |  |  | no counterpart (Editor product) |
| 再生バー > プレイリスト選択 | PHY01 | animation_playlist_seek | covered (animation_playlist_seek) |
| 再生バー > シーン選択 | PHY01 | animation_seek | covered (animation_seek) |
| 再生バー > 再生 | PHY01 | animation_advance | covered (animation_advance) |
| 再生バー > リピート | PHY01 | animation_set | covered (animation_set); loop |
| 再生バー > 連続再生 | PHY01 | animation_playlist_seek | covered (animation_playlist_seek) |
| 再生バー > シャッフル再生 | PHY01 |  | absent |
| 再生バー > 録画 | PHY01 | animation_record_start | covered (animation_record_start) |
| 再生バー > フェード > なし | PHY01 | animation_playlist_set | covered (animation_playlist_set); entry fade |
| 再生バー > フェード > リニア | PHY01 | animation_playlist_set | covered (animation_playlist_set) |
| 再生バー > フェード > イージング(2.1方式) | PHY01 |  | absent |
| 再生バー > フェード時間(ms)の調整 | PHY01 | animation_playlist_set | covered (animation_playlist_set) |
| 再生バー > インターバル | PHY01 | animation_playlist_set | covered (animation_playlist_set) |
| パラメータリスト > パラメータをデフォルト値に戻す | PHY01 | values_reset | covered (values_reset) |
| パラメータリスト > パラメータグループの全展開・集約 | PHY01 |  | absent; panel state |
| パラメータリスト > パラメータ | PHY01 | parameters, value_set | covered (value_set) |
| ビューエリア > 背景色の設定 |  |  | absent; display style |
| ビューエリア > 表示倍率 |  | editor_camera_set | covered (editor_camera_set) |
| ビューエリア > 拡大 |  | editor_camera_set | covered (editor_camera_set) |
| ビューエリア > 縮小 |  | editor_camera_set | covered (editor_camera_set) |
| ビューエリア > 全体 |  | editor_camera_fit | covered (editor_camera_fit) |
| ビューエリア > 中心 |  | editor_camera_set | covered (editor_camera_set) |
| ビューエリア > 原寸 |  | editor_camera_set | covered (editor_camera_set); zoom 1 |
| ビューエリア > 座標 |  |  | absent; pointer readout |

### テクスチャアトラス編集 (Texture atlas edit)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 新規テクスチャ設定 > テクスチャ名 | IO04 |  | absent; atlases are numbered |
| 新規テクスチャ設定 > 幅 | IO04 | atlas_build | covered (atlas_build); size |
| 新規テクスチャ設定 > 高さ | IO04 | atlas_build | covered (atlas_build); size |
| 新規テクスチャ設定 > 初期配置 > 表示状態のモデル用画像 | IO04 | atlas_layout_set | covered (atlas_layout_set); the caller picks the Parts |
| 新規テクスチャ設定 > 初期配置 > すべてのモデル用画像 | IO04 | atlas_build | covered (atlas_build) |
| 新規テクスチャ設定 > 初期配置 > 配置しない | IO04 | atlas_layout_clear | covered (atlas_layout_clear) |
| 新規テクスチャ設定 > 自動レイアウト設定 | IO04 | atlas_pack | covered (atlas_pack) |
| 新規テクスチャ設定 > 自動レイアウトを実行 | IO04 | atlas_pack | covered (atlas_pack) |
| 新規テクスチャ設定 > OK | IO04 | atlas_build | covered (atlas_build) |
| 新規テクスチャ設定 > Cancel |  |  | no counterpart (Editor product) |
| テクスチャアトラス編集 > メニュー | IO04 |  | covered (see the menu rows) |
| テクスチャアトラス編集 > テクスチャ詳細 | IO04 | atlas_layout_set | covered (atlas_layout_set); x, y, scale |
| テクスチャアトラス編集 > テクスチャ詳細 > 角度と倍率をロック | IO04 |  | absent; dialog state |
| テクスチャアトラス編集 > 配置の検証 | IO04 | atlas_build | covered (atlas_build); the plan reports overlaps |
| テクスチャアトラス編集 > 自動レイアウト | IO04 | atlas_pack | covered (atlas_pack) |
| テクスチャアトラス編集 > テクスチャ追加 | IO04 | atlas_layout_set | covered (atlas_layout_set); atlas index |
| テクスチャアトラス編集 > テクスチャ削除 | IO04 | atlas_layout_clear | covered (atlas_layout_clear) |
| テクスチャアトラス編集 > テクスチャの一覧 | IO04 | atlas_build | covered (atlas_build) as a read |
| テクスチャアトラス編集 > テクスチャ | IO04 | atlas_layout_set | covered (atlas_layout_set) |
| テクスチャアトラス編集 > モデル用画像リスト | IO04 | atlas_build | covered (atlas_build) as a read |
| テクスチャアトラス編集 > モデル用画像リスト > すべて | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > モデル用画像リスト > 配置済みのみ | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > モデル用画像リスト > 未配置のみ | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > モデル用画像リスト > 表示状態かつ未配置のみ | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > モデル用画像リスト > 配置済みかつ選択状態のみ | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > モデル用画像リスト > パーツで絞り込み | IO04 |  | absent; list filter |
| テクスチャアトラス編集 > 表示オプション | IO04 |  | absent; display style |
| テクスチャアトラス編集 > OK | IO04 | export_inp | covered (export_inp); the atlas is built on export |
| テクスチャアトラス編集 > Cancel |  |  | no counterpart (Editor product) |
| メニューバー(テクスチャアトラス編集) > ファイル > テクスチャをロック(β) | IO04 |  | absent |
| メニューバー(テクスチャアトラス編集) > ファイル > テクスチャ画像の書き出し(β) | IO04 | texture_export | covered (texture_export) |
| メニューバー(テクスチャアトラス編集) > ファイル > テクスチャ画像の差し替え(β) | IO04 | texture_replace | covered (texture_replace) |
| メニューバー(テクスチャアトラス編集) > 編集 > 元に戻す | IO04 | undo | covered (undo) |
| メニューバー(テクスチャアトラス編集) > 編集 > やり直す | IO04 | redo | covered (redo) |
| メニューバー(テクスチャアトラス編集) > 編集 > すべて選択 | IO04 |  | absent; dialog state |
| メニューバー(テクスチャアトラス編集) > 表示 > 元のテクスチャを表示 | IO04 |  | absent; display style |
| メニューバー(テクスチャアトラス編集) > 表示 > モデル用画像を表示 | IO04 |  | absent; display style |
| メニューバー(テクスチャアトラス編集) > 表示 > メッシュの色 |  |  | absent; display style |
| メニューバー(テクスチャアトラス編集) > 表示 > メッシュの線色 |  |  | absent; display style |
| メニューバー(テクスチャアトラス編集) > テクスチャ > テクスチャ追加 | IO04 | atlas_layout_set | covered (atlas_layout_set) |
| メニューバー(テクスチャアトラス編集) > テクスチャ > テクスチャ削除 | IO04 | atlas_layout_clear | covered (atlas_layout_clear) |
| メニューバー(テクスチャアトラス編集) > テクスチャ > テクスチャのサイズ変更 | IO04 | atlas_build | covered (atlas_build); size |
| メニューバー(テクスチャアトラス編集) > テクスチャ > 自動レイアウト | IO04 | atlas_pack | covered (atlas_pack) |
| メニューバー(テクスチャアトラス編集) > テクスチャ > 角度と倍率一覧 | IO04 | atlas_build | covered (atlas_build) as a read |
| メニューバー(テクスチャアトラス編集) > テクスチャ > パーツで絞り込み | IO04 |  | absent; list filter |
| メニューバー(テクスチャアトラス編集) > テクスチャ > 配置の検証 | IO04 | atlas_build | covered (atlas_build) |
| メニューバー(テクスチャアトラス編集) > テクスチャ > テクスチャアトラスを初期化 | IO04 | atlas_layout_clear | covered (atlas_layout_clear) |
| メニューバー(テクスチャアトラス編集) > テクスチャ > テクスチャ生成方式 | IO04 |  | absent; one quality |
| メニューバー(テクスチャアトラス編集) > 倍率設定 > 倍率 | IO04 | atlas_layout_set | covered (atlas_layout_set); scale |
| メニューバー(テクスチャアトラス編集) > 倍率設定 > バウンディングボックスに倍率を適用 | IO04 |  | absent |
| テクスチャのサイズ変更 > 幅 | IO04 | atlas_build | covered (atlas_build) |
| テクスチャのサイズ変更 > 高さ | IO04 | atlas_build | covered (atlas_build) |
| テクスチャのサイズ変更 > OK | IO04 | atlas_build | covered (atlas_build) |
| 自動レイアウト設定 > マージン | IO04 | atlas_pack | covered (atlas_pack); padding |
| 自動レイアウト設定 > 配置基準 > メッシュ | IO04 | atlas_pack | covered (atlas_pack); rectangles are mesh bounds |
| 自動レイアウト設定 > 配置基準 > モデル用画像 | IO04 |  | absent |
| 自動レイアウト設定 > 倍率 > 自動で設定する | IO04 | atlas_pack | covered (atlas_pack) |
| 自動レイアウト設定 > 倍率 > ユーザー指定 | IO04 | atlas_layout_set | covered (atlas_layout_set); scale |
| 自動レイアウト設定 > 回転を許可 | IO04 |  | absent; no rotation |
| 配置の検証 > 重なりを検出 | IO04 | atlas_build | covered (atlas_build) |
| 配置の検証 > 枠からのはみ出しを検出 | IO04 | atlas_build | covered (atlas_build) |
| 配置の検証 > 指定の条件で検出 | IO04 |  | absent |
| 配置の検証 > 指定の条件で検出 > 角度 | IO04 |  | absent; no rotation |
| 配置の検証 > 指定の条件で検出 > 倍率 | IO04 | atlas_build | covered (atlas_build) as a read |
| 配置の検証 > 選択 | IO04 | editor_selection_set | covered (editor_selection_set) |
| パーツで絞り込み > 子のパーツを含めて選択 | IO04 | nodes_list | covered (nodes_list) |

### 動きの反転 (Motion reflection)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 反転設定 > 基本機能 > 水平方向に反転 | PAR20 | parameter_mirror | covered (parameter_mirror) on INX; tracked (PAR20, #184) on CMO3 |
| 反転設定 > 基本機能 > 垂直方向に反転 | PAR20 | parameter_mirror | covered (parameter_mirror) axis 1 on INX; tracked (PAR20, #184) on CMO3 |
| 反転設定 > OK | PAR20 | parameter_mirror | covered (parameter_mirror) |
| 反転設定 > 反転する軸の設定 > キャンバスの中心を使用 | PAR20 | parameter_mirror | covered (parameter_mirror); the INX axis is the node origin |
| 反転設定 > 反転する軸の設定 > ガイドを使用 | PAR20 |  | tracked (PAR20, #184) |
| 反転設定 > 反転する軸の設定 > 回転デフォーマの中心点を使用 | PAR20 |  | tracked (PAR20, #184) |

### フォームの特殊な貼り付けオプション (Special form paste)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| フォームの特殊な貼り付けオプション > オブジェクト(タブ) | PAR14 | binding_paste | tracked (PAR14); binding_paste takes the target bindings |
| フォームの特殊な貼り付けオプション > プロパティ | PAR14 | binding_paste | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > すべて選択・選択解除 | PAR14 |  | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > アートメッシュ: 不透明度、描画順、乗算色、スクリーン色、頂点情報 | PAR14 | binding_paste, live2d_form_paste | tracked (PAR14); live2d_form_paste pastes the whole form on CMO3 |
| フォームの特殊な貼り付けオプション > アートパス: 不透明度、描画順、乗算色、スクリーン色、頂点情報、線幅、線色、不透明度(コントロールポイント)、折れ線 | PAR14, GEO08 |  | tracked (PAR14, GEO08, #159) |
| フォームの特殊な貼り付けオプション > 回転デフォーマ: 不透明度、乗算色、スクリーン色、角度、頂点情報、倍率 | PAR14 | binding_paste | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > ワープデフォーマ: 不透明度、乗算色、スクリーン色、頂点情報 | PAR14 | binding_paste, live2d_form_paste | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > パーツ: 不透明度、描画順、乗算色、スクリーン色 | PAR14 | binding_paste | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > 頂点情報 > 座標を反転 | PAR14, GEO03 | binding_keypoint | covered (binding_keypoint); mirror_horizontal, mirror_vertical |
| フォームの特殊な貼り付けオプション > 頂点情報 > 軸 > 垂直 | GEO03 | binding_keypoint | covered (binding_keypoint); mirror_horizontal |
| フォームの特殊な貼り付けオプション > 頂点情報 > 軸 > 水平 | GEO03 | binding_keypoint | covered (binding_keypoint); mirror_vertical |
| フォームの特殊な貼り付けオプション > 頂点情報 > 位置 > 自動設定 | GEO03 |  | tracked (PAR14); the INX axis is the node origin |
| フォームの特殊な貼り付けオプション > 頂点情報 > 位置 > 手動設定 | GEO03 |  | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > 頂点情報 > 裏表を逆にする | GEO03 |  | tracked (PAR14) |
| フォームの特殊な貼り付けオプション > 角度 > 角度(回転デフォーマ) | PAR14 | binding_keypoint | covered (binding_keypoint); invert |
| フォームの特殊な貼り付けオプション > 角度 > 正負を反転 | PAR14 | binding_keypoint | covered (binding_keypoint); invert |

### 四隅のフォームを自動生成 (Corner forms)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 四隅のフォームを自動生成 > パラメータ1 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners); the 2D parameter's X |
| 四隅のフォームを自動生成 > パラメータ2 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners); its Y |
| 四隅のフォームを自動生成 > 基準とするパラメータ値 > デフォルト値 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners); the default's row and column |
| 四隅のフォームを自動生成 > 基準とするパラメータ値 > パラメータの中央 | PAR15 |  | absent |
| 四隅のフォームを自動生成 > 上書きするキー > 左上 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners); every corner |
| 四隅のフォームを自動生成 > 上書きするキー > 右上 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| 四隅のフォームを自動生成 > 上書きするキー > 左下 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| 四隅のフォームを自動生成 > 上書きするキー > 右下 | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |
| 四隅のフォームを自動生成 > オプション | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners); `nodes` |
| 四隅のフォームを自動生成 > OK | PAR15 | parameter_synthesize_corners | covered (parameter_synthesize_corners) |

### ブレンドシェイプの重みの制限設定 (Blend shape weight limits)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ブレンドシェイプの重みの制限設定 > ブレンドシェイプ | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); `node` |
| ブレンドシェイプの重みの制限設定 > パラメータ | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); `parameter` |
| ブレンドシェイプの重みの制限設定 > 追加 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| ブレンドシェイプの重みの制限設定 > リスト(追加済みパラメータ一覧) | PAR22 | parameters | covered (parameters) as a read |
| ブレンドシェイプの重みの制限設定 > プリセット | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); the points are given explicitly |
| ブレンドシェイプの重みの制限設定 > プリセット > 直線1 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 直線2 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 直線3 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 折れ線1 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 折れ線2 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 折れ線3 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 折れ線4 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > 折れ線5 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > ステップ1 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > プリセット > ステップ2 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); as points |
| ブレンドシェイプの重みの制限設定 > 重みの制限グラフ | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); points |
| ブレンドシェイプの重みの制限設定 > グラフの編集 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| ブレンドシェイプの重みの制限設定 > 重みの制限設定をしている数 | PAR22 | parameters | covered (parameters) as a read |
| ブレンドシェイプの重みの制限設定 > 編集対象のオブジェクトの数 | PAR22 |  | absent; dialog display |
| ブレンドシェイプの重みの制限設定 > 対象のオブジェクト名 | PAR22 | parameters | covered (parameters) as a read |
| グラフの編集 > 行を追加 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); up to 5 points |
| グラフの編集 > 行を削除 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| グラフの編集 > 表 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set) |
| グラフの編集 > ルールを表示 | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); refusals name the rule |
| 確認・警告ダイアログ > 変更の確認ダイアログ > はい |  |  | no counterpart (Editor product); a call has no confirmation |
| 確認・警告ダイアログ > 警告ダイアログ(変更不可) | PAR22 | binding_weight_limits_set | covered (binding_weight_limits_set); the refusal is the answer |
| 優先順位設定 > オブジェクト一覧(並べ替え) | PAR22 |  | absent; no priority order |

### オニオンスキン設定 (Onion skin)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| オニオンスキン設定 > 画像／選択中の頂点の軌道表示 | UI08 | editor_onion_set, animation_trajectory | covered (editor_onion_set) |
| オニオンスキン設定 > 色・透明度設定 | UI08 |  | absent; display style |
| オニオンスキン設定 > オニオンスキンのレイヤーを前面に表示 | UI08 |  | absent; display style |

### グリッド設定 (Grid)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| グリッド設定 > 間隔 | UI06 | editor_grid_set | covered (editor_grid_set) |
| グリッド設定 > 色 | UI06 |  | absent; display style |

### ガイド設定 (Guides)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ガイド設定 > 元に戻す | UI05 | editor_guides_set | covered (editor_guides_set); not a document edit, no undo |
| ガイド設定 > やり直す | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド設定 > 全てのガイドの色を変更 | UI05 |  | absent; display style |
| ガイド設定 > ガイド線のデフォルト色 | UI05 |  | absent; display style |
| ガイド設定 > No | UI05 | editor_guides_set | covered (editor_guides_set); index |
| ガイド設定 > 種別 | UI05 | editor_guides_set | covered (editor_guides_set); horizontal or vertical |
| ガイド設定 > 位置 | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド設定 > 色 | UI05 |  | absent; display style |
| ガイド設定 > メモ | UI05 |  | absent |
| ガイド設定 > 新規ガイド | UI05 | editor_guides_set | covered (editor_guides_set) |
| ガイド設定 > 削除 | UI05 | editor_guides_set | covered (editor_guides_set) |

### ワークスペース設定 (Workspace settings)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ワークスペース設定 > 新規追加 | UI01 | editor_workspace_save | covered (editor_workspace_save) |
| ワークスペース設定 > 名前を変更 | UI01 | editor_workspace_save, editor_workspace_delete | covered (editor_workspace_save); save under the new name, delete the old |
| ワークスペース設定 > レイアウトを更新 | UI01 | editor_workspace_save | covered (editor_workspace_save) |
| ワークスペース設定 > 上へ移動 | UI01 |  | absent; no order |
| ワークスペース設定 > 下へ移動 | UI01 |  | absent; no order |
| ワークスペース設定 > 削除 | UI01 | editor_workspace_delete | covered (editor_workspace_delete) |
| ワークスペース設定 > タブ選択時の自動切替 | UI01 |  | absent |

### キーフォーム編集 (Keyform edit)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| キーフォーム編集 > キーの手動編集アイコン | PAR01 | parameter_set_axes | covered (parameter_set_axes) |
| キーフォーム編集 > 目盛り | PAR01 | parameter_set_axes | covered (parameter_set_axes); axis points |
| キーフォーム編集 > 赤い点 | PAR02 | parameter_set_axes | covered (parameter_set_axes); remove the point |
| キーフォーム編集 > キーの3点追加 | PAR01 | parameter_set_axes | covered (parameter_set_axes) |
| キーフォーム編集 > 値の欄 | PAR01 | parameter_set_axes | covered (parameter_set_axes) |
| キーフォーム編集 > OK | PAR01 | parameter_set_axes | covered (parameter_set_axes) |
| パラメータ調整用ポップアップ > 黒点 |  | value_set | covered (value_set) |
| パラメータ調整用ポップアップ > 数値欄 |  | value_set | covered (value_set) |
| パラメータ調整用ポップアップ > ＋マーク | PAR01 | binding_keypoint | covered (binding_keypoint); set_current |
| パラメータ調整用ポップアップ > −マーク | PAR02 | binding_keypoint | covered (binding_keypoint); unset |

## Soul2D editor menu bar (`rust/src/editor_windows.rs`, `menu_bar`)

Read from the source on 2026-10-20, one row per entry with the `MenuAction` it dispatches, 48 rows. `covered` means the entry ends in a Session or editor operation that an MCP tool also reaches; `GUI-only` names an entry that does not, which is the list #99 section 2 asks for. Host file dialogs only supply a path; the operation is the same as the tool's. Shared routes do not establish equivalence of every GUI field, dialog or ACP lifecycle operation.

Routing counts updated on 2026-10-25: 48 shared tool routes (47 covered entries and one bounded Quit), GUI-only 0. ACP Quit uses exact human lifecycle consent, identified local reply output, the initiating uncancelled successful turn, owned-client release and a fresh full safety check. Human Save/Discard remains a separate native flow. Shared routes do not establish complete dialog, lifecycle or vendor parity.

| Menu entry | Dispatch | Session operation / tool | Status |
|---|---|---|---|
| File > New | MenuAction::Command(FileCommand.NewFile) | new | covered (new) |
| File > Open | MenuAction::Command(FileCommand.ShowOpenFileDialog), host file dialog | open | covered (open); the host dialog supplies the path |
| File > Recent > <path> | MenuAction::Open(path) | open | covered (open) |
| File > Recent > Autosaves > <record> | MenuAction::OpenAutosave(record) | editor_recovery_list, editor_recovery_restore | covered (editor_recovery_restore) |
| File > Save | MenuAction::Command(FileCommand.ShowSaveFileDialog), host file dialog | save | covered (save) |
| File > Save As... | MenuAction::Command(FileCommand.ShowSaveFileAsDialog), host file dialog | save | covered (save); `path` |
| File > Import > Import PSD... | MenuAction::ImportPsd, host file dialog | import_psd | covered (import_psd) |
| File > Import > Import KRA... | MenuAction::Prompt(ImportKra), host file dialog | import_kra | covered (import_kra) |
| File > Import > Import soul2d_live puppet | MenuAction::Prompt(ImportInp), host file dialog | import_inp | covered (import_inp) |
| File > Import > Import Image Folder | MenuAction::Prompt(ImportFolder), host file dialog | import_folder | covered (import_folder) |
| File > Import > Import Depth Map... | MenuAction::PsdDepth, PSD Depth Map window | depth_import_psd_plan, depth_import_psd | covered (depth_import_psd); the window's fields are the call's arguments |
| File > Import > Export > Export Image... | MenuAction::Prompt(Export(Image)), host file dialog | export_image | covered (export_image) |
| File > Import > Export > Export Frames... | MenuAction::Prompt(Export(Frames)), host file dialog | export_frames | covered (export_frames) |
| File > Import > Export > Export soul2d_live puppet... | MenuAction::Prompt(Export(Inp)), host file dialog | export_inp | covered (export_inp) |
| File > Import > Export > Export Video... | MenuAction::Prompt(Export(Video)), host file dialog | export_video_start | covered (export_video_start); the Tools panel's codec and ffmpeg options apply |
| File > Merge > Merge Image Files | MenuAction::Prompt(MergeImages), host file dialog | merge_images | covered (merge_images) |
| File > Merge > Merge soul2d project | MenuAction::Prompt(MergeInp), host file dialog | merge_inp | covered (merge_inp) |
| File > Close Project | MenuAction::Command(FileCommand.NewFile) | new | covered (new) |
| File > Quit | MenuAction::Quit (ViewportCommand::Close) | editor_quit | bounded non-discarding external exit after exact reply flush and final check; ACP additionally requires exact human consent, successful initiating turn and owned-client release; native Save/Discard remains separate (#99 section 2) |
| Edit > Undo | MenuAction::Command(EditCommand.Undo) | undo | covered (undo) |
| Edit > Redo | MenuAction::Command(EditCommand.Redo) | redo | covered (redo) |
| Edit > Cut | MenuAction::Command(NodeCommand.CutNode) | node_cut | covered (node_cut) |
| Edit > Copy | MenuAction::Command(NodeCommand.CopyNode) | node_copy | covered (node_copy) |
| Edit > Paste | MenuAction::Command(NodeCommand.PasteNode) | node_paste | covered (node_paste) |
| Edit > Settings | MenuAction::Command(EditCommand.ShowSettingsWindow), Settings window | editor_settings_get, editor_settings_set, editor_shortcut_set | covered (editor_settings_set; the Shortcuts pane through editor_shortcut_set) |
| Edit > Command Browser | MenuAction::Command(EditCommand.ShowCommandBrowserWindow), shared editor operation | editor_command_browser_set | covered (editor_command_browser_set); visibility only; tools/list and tools/call retain discovery/execution, browser input/result read/set are separate |
| View > Open Configuration Folder | MenuAction::Address(Configuration), address dialog | editor_address_dialog | covered (editor_address_dialog) |
| View > Save Screenshot... | MenuAction::Prompt(Screenshot), host file dialog | view_screenshot, ViewCommand_SaveScreenshot | covered (view_screenshot) |
| View > Save Snapshot as Guide Image | MenuAction::Tool(sketch.snapshot) | sketch_snapshot | covered (sketch_snapshot) |
| View > Panels > Outline / Timeline | MenuAction::Panel(label, shown) | editor_panel_set | covered (editor_panel_set) |
| View > Stats for Nerds | MenuAction::Stats(visible), MCP layout target | editor_settings_set | covered (editor_settings_set) |
| View > Reset Layout | MenuAction::ResetLayout | editor_layout_reset | covered (editor_layout_reset) |
| View > Workspace > <name> | MenuAction::Workspace(editor_workspace_apply, name) | editor_workspace_apply | covered (editor_workspace_apply) |
| View > Workspace > Save Workspace... | MenuAction::Prompt(Workspace), name prompt | editor_workspace_save | covered (editor_workspace_save) |
| View > Workspace > Delete Workspace > <name> | MenuAction::Workspace(editor_workspace_delete, name) | editor_workspace_delete | covered (editor_workspace_delete) |
| Tools > Puppet Data > Import Inochi Session Data... | MenuAction::Prompt(ImportSession), host file dialog | tool_import_session_data | covered (tool_import_session_data) |
| Tools > Puppet Texturing > Premultiply textures | MenuAction::Tool(tool.premultiply_textures) | tool_premultiply_textures | covered (tool_premultiply_textures) |
| Tools > Puppet Texturing > Bleed textures... | MenuAction::Tool(tool.rebleed_textures) | tool_rebleed_textures | covered (tool_rebleed_textures) |
| Tools > Puppet Texturing > Generate fake layer name info... | MenuAction::Tool(tool.generate_fake_layer_names) | tool_generate_fake_layer_names | covered (tool_generate_fake_layer_names) |
| Tools > Puppet Texturing > Export textures... | MenuAction::Prompt(TextureExport), host folder dialog | texture_export | covered (texture_export) |
| Tools > Puppet Texturing > Replace textures... | MenuAction::Prompt(TextureReplace), host folder dialog | texture_replace | covered (texture_replace) |
| Tools > Puppet Recovery > Attempt full repair... | MenuAction::Tool(tool.repair) | tool_repair | covered (tool_repair) |
| Tools > Puppet Recovery > Regenerate Node IDs | MenuAction::Tool(tool.regenerate_node_ids) | tool_regenerate_node_ids | covered (tool_regenerate_node_ids) |
| Tools > Texture Viewer | MenuAction::Textures, Texture Viewer window | editor_texture_viewer_set, texture_get | covered; shared visibility operation and windows.textureViewer readback; existing selected-Part image reads use texture_get |
| Texture Viewer > Close window | Texture Viewer native close | editor_texture_viewer_set | covered; shared close drops the viewer; repeated open retains Slot/Zoom/images, reopen after close starts fresh |
| Tools > Flip Pairing | MenuAction::FlipPairs, flip-pair window | extdata_get, extdata_set | covered (extdata_set); the window reads the list with extdata_get and Save stores it with extdata_set under key `com.inochi2d.creator.FlipConfig` (base64 of a JSON array of {uuid1, uuid2}); Add as new row, the row buttons and Pair (name patterns) edit the list in the window before that call, so the window's fields are the call's arguments; parameter_duplicate_flip, parameter_mirrored_autofill, parameter_paste and the binding_keypoint set_from_mirror_* actions read the stored list |
| Help > <documentation and bug links> | MenuAction::Address(target), address dialog | editor_address_dialog | covered (editor_address_dialog) |
| Help > About | MenuAction::About, MCP layout target | editor_about_set | covered (editor_about_set) |
| Help > Welcome | MenuAction::Welcome, MCP layout target, Welcome window | editor_welcome_set, editor_settings_set, new, open, import_psd | covered (editor_welcome_set); Quick Setup's theme, language and scale are the Settings window's Look controls (editor_settings_set DarkMode, lang, UIScale); New..., Open..., Import... and a recent project are new, open and import_psd; Next and hasDoneQuickSetup are the window's own first-run state |

## Cubism Editor dialogs, animation side

Read from the Cubism Editor manual on 2026-10-20, fifteen pages of the Animation workspace and the export dialogs (scene management, fade, parameter controller, image sequence track, collect files, parameter bookmarks, track trajectories, loop editing support, animation templates, random pose, lightweight playback, image and video export, runtime data export). The fade overview page describes no dialog. One row per field, option, button or menu entry, 246 rows: シーンの管理と設定 (Scenes) 39, フェード (Fade) 9, パラメータコントローラ (Parameter controller) 38, 連番画像トラック (Image sequence track) 8, ファイルを収集 (Collect files) 2, パラメータブックマーク (Parameter bookmarks) 36, トラックの軌道設定 (Track trajectories) 9, ループ編集補助 (Loop editing support) 3, アニメーションテンプレート (Animation templates) 8, ランダムポーズ設定 (Random pose) 5, 軽量再生 (Lightweight playback) 2, 画像・動画書き出し (Image and video export) 46, 組み込み用データ (Runtime data export) 41. Same status vocabulary as above.

Counts: covered 160, tracked 1, absent 78, no counterpart 7.

The view area, toolbar and view features follow in the next section.

### シーンの管理と設定 (Scenes)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| シーンパレット > 新規シーンの作成 | ANI05 | animation_create, live2d_animation_scene_add | covered (animation_create) |
| シーンパレット > シーンの複製 | ANI05 |  | absent |
| シーンパレット > シーンの挿入 | ANI05 | animation_scene_group_set | covered (animation_scene_group_set); clips played together, not a scene track |
| シーンパレット > 選択しているシーンの削除 | ANI05 | animation_delete, live2d_animation_scene_delete | covered (animation_delete); undoable |
| シーンパレット > 一覧の表示項目: シーン名、長さ、タグ | ANI05 | snapshot, live2d_animation_list | covered (live2d_animation_list) as a read |
| インスペクタ > シーンの設定 > シーン名 | ANI05 | animation_rename, live2d_animation_scene_set | covered (animation_rename) |
| インスペクタ > シーンの設定 > タグ | ANI05 |  | absent |
| インスペクタ > シーンの設定 > ファイル名 | ANI05 | live2d_animation_list | covered (live2d_animation_list) as a read |
| インスペクタ > シーンの設定 > サイズ(幅) | ANI05 |  | absent; INX animations have no canvas |
| インスペクタ > シーンの設定 > サイズ(高さ) | ANI05 |  | absent; INX animations have no canvas |
| インスペクタ > シーンの設定 > 縦横比を固定 |  |  | absent; dialog state |
| インスペクタ > シーンの設定 > 開始位置 | ANI22 | animation_set | covered (animation_set) |
| インスペクタ > シーンの設定 > フレーム数 (0から開始 / 1から開始) |  |  | absent; frames start at 0 |
| インスペクタ > シーンの設定 > 長さ | ANI22 | animation_set, live2d_animation_scene_set | covered (animation_set) |
| インスペクタ > シーンの設定 > ロックボタン |  |  | absent; dialog state |
| インスペクタ > シーンの設定 > フレームレート | ANI22 | animation_set, live2d_animation_scene_set | covered (animation_set) |
| インスペクタ > シーンの設定 > ループ編集補助 | ANI28 | animation_set, live2d_animation_scene_set | covered (animation_set); loop |
| インスペクタ > シーンの設定 > デフォルト補間方式：パラメータ | ANI08 | animation_lane_set | covered (animation_lane_set); per lane, no default |
| インスペクタ > シーンの設定 > デフォルト補間方式：パーツ | ANI08 | live2d_animation_curve_type_set | covered (live2d_animation_curve_type_set); per curve |
| インスペクタ > シーンの設定 > ターゲット設定 | EXP08 | motion3_export | covered (motion3_export); Bezier handles are restricted on export |
| インスペクタ > シーンの設定 > モーション書き出し | EXP08 | motion3_export | covered (motion3_export); per call |
| インスペクタ > シーンの設定 > 歯車ボタン (モーション書き出し設定) | EXP08 | motion3_export | covered (motion3_export) |
| 設定ダイアログ (シーントラック) > 挿入するシーン | ANI05 | animation_scene_group_set | covered (animation_scene_group_set); clips |
| 設定ダイアログ (シーントラック) > 参照範囲 | ANI05 |  | absent |
| 設定ダイアログ (シーントラック) > タイムリマップの適用 | ANI23 | animation_form_time, animation_image_time | covered (animation_image_time); sequences and forms only |
| シーントラックの右クリックメニュー > シーントラック設定の変更 | ANI05 | animation_scene_group_set | covered (animation_scene_group_set) |
| シーントラックの右クリックメニュー > 参照するシーンを開く | ANI05 | animation_seek | covered (animation_seek); `name` |
| シーントラックの右クリックメニュー > シーントラックから連番画像トラックを作成 | ANI23 | export_frames, animation_image_sequence | covered (animation_image_sequence); after export_frames |
| タブ・シーントラックのボタン > タブを追加 |  |  | absent; panel state |
| タブ・シーントラックのボタン > リピート | ANI22 | animation_set | covered (animation_set); loop |
| タブ・シーントラックのボタン > 目玉ボタン |  |  | absent; panel state |
| シーンをインポート > シーンをインポート | ANI05 | merge_inp | covered (merge_inp) |
| シーンをインポート > 確認ダイアログ > はい / いいえ / すべてキャンセル |  |  | no counterpart (Editor product) |
| シーンをインポート > インポート先 | ANI05 | merge_inp | covered (merge_inp); the open document |
| シーンをインポート > インポート対象シーン | ANI05 | merge_inp | covered (merge_inp); every animation of the file |
| シーンをインポート > 全シーンをインポート | ANI05 | merge_inp | covered (merge_inp) |
| シーンをインポート > タグにコメントを追加 | ANI05 |  | absent |
| シーンをインポート > リンク切れ確認ダイアログ | ANI01 | merge_inp | covered (merge_inp); missing files are refused |
| シーンをインポート > シーン削除時の確認ダイアログ | ANI05 | animation_delete | covered (animation_delete); refused while a playlist holds it |

### フェード (Fade)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| フェードイン・アウトの設定 > 設定対象 > シーン | ANI16 | motion3_fade_set | covered (motion3_fade_set); Meta on Motion3, tracked (ANI16) on INX |
| フェードイン・アウトの設定 > 設定対象 > 選択中のパラメータ | ANI16 | motion3_fade_set | covered (motion3_fade_set); `curve` |
| フェードイン・アウトの設定 > 設定対象 > 選択範囲内のパラメータ | ANI16 | motion3_fade_set | covered (motion3_fade_set); one call per curve |
| フェードイン・アウトの設定 > フェードイン・アウトの数値 | ANI16 | motion3_fade_set | covered (motion3_fade_set) |
| フェードイン・アウトの設定 > 設定を解除 | ANI16 | motion3_fade_set | covered (motion3_fade_set); null |
| フェードイン・アウトの設定 > OK | ANI16 | motion3_fade_set | covered (motion3_fade_set) |
| 他のシーンにフェードイン・アウトの設定を反映 > シーン (チェックボックス) | ANI16 | motion3_fade_set | covered (motion3_fade_set); one call per motion |
| 他のシーンにフェードイン・アウトの設定を反映 > 情報欄 | ANI16 | motion3_read | covered (motion3_read) as a read |
| 他のシーンにフェードイン・アウトの設定を反映 > OK | ANI16 | motion3_fade_set | covered (motion3_fade_set) |

### パラメータコントローラ (Parameter controller)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 計算ユニット設定 > 有効 | ANI18 | animation_controller_set | covered (animation_controller_set) |
| 計算ユニット設定 > 順序 | ANI18 | animation_controller_read | covered (animation_controller_read) as a read |
| 計算ユニット設定 > 計算するコントローラ | ANI18 | animation_controller_read | covered (animation_controller_read) |
| 計算ユニット設定 > ターゲット | ANI18 | animation_controller_set | covered (animation_controller_set); `target` |
| 計算ユニット設定 > ヒント | ANI18 | animation_controller_read | covered (animation_controller_read); the answer names overwritten parameters |
| 計算ユニット設定 > ↑ | ANI18 | animation_controller_set | covered (animation_controller_set); order |
| 計算ユニット設定 > ↓ | ANI18 | animation_controller_set | covered (animation_controller_set); order |
| 計算ユニット設定 > 自動 | ANI18 |  | absent |
| 計算ユニット設定 > 削除 | ANI18 | animation_controller_delete | covered (animation_controller_delete) |
| 計算ユニット設定 > トラック名を表示する |  |  | absent; panel state |
| アニメーションベイク (確認) > シーンを複製してベイク | ANI18 |  | absent; no scene duplicate |
| アニメーションベイク (確認) > シーンを複製せずにベイク | ANI18 | animation_controller_bake | covered (animation_controller_bake) |
| 表示設定 > 名前 |  |  | absent; display style |
| 表示設定 > ターゲットの関係性 |  |  | absent; display style |
| 表示設定 > 影響するアートメッシュをハイライト |  |  | absent; display style |
| 表示設定 > 再生中にコントローラを表示 |  |  | absent; display style |
| 表示設定 > トラックが非表示でもコントローラを表示 |  |  | absent; display style |
| 参照するアートメッシュを変更 > プルダウンメニュー | ANI18 | animation_controller_set | covered (animation_controller_set); the Part |
| インスペクタ (パラメータコントローラ設定) > 名前 | ANI18 | animation_controller_set | covered (animation_controller_set) |
| インスペクタ (パラメータコントローラ設定) > 色 |  |  | absent; display style |
| インスペクタ (パラメータコントローラ設定) > 選択をロック |  |  | absent; panel state |
| インスペクタ (パラメータコントローラ設定) > トラック | ANI18 | animation_controller_read | covered (animation_controller_read) |
| インスペクタ (パラメータコントローラ設定) > アートメッシュ | ANI18 | animation_controller_read | covered (animation_controller_read) |
| インスペクタ (パラメータコントローラ設定) > 変更 | ANI18 | animation_controller_set | covered (animation_controller_set) |
| インスペクタ (パラメータコントローラ設定) > ターゲット | ANI18 | animation_controller_set | covered (animation_controller_set) |
| インスペクタ (パラメータコントローラ設定) > パラメータ | ANI18 | animation_controller_set | covered (animation_controller_set); `parameters` |
| インスペクタ (パラメータコントローラ設定) > 計算ユニット | ANI18 | animation_controller_read | covered (animation_controller_read) |
| インスペクタ (パラメータコントローラ設定) > 計算ユニット設定ダイアログを開く | ANI18 | animation_controller_read | covered (animation_controller_read) |
| インスペクタ (パラメータコントローラ設定) > 自動グループ化 | ANI18 |  | absent |
| ツール詳細 > コントローラの選択／移動 | ANI18 | animation_controller_move | covered (animation_controller_move) |
| ツール詳細 > 単点コントローラの追加 | ANI18 | animation_controller_add | covered (animation_controller_add) |
| ツール詳細 > 方向コントローラの追加 | ANI18 |  | tracked (ANI18) |
| ツール詳細 > コントローラの削除 | ANI18 | animation_controller_delete | covered (animation_controller_delete) |
| ツール詳細 > ターゲットの編集 | ANI18 | animation_controller_set | covered (animation_controller_set); `target` |
| ツール詳細 > 回転デフォーマを表示 |  |  | absent; display style |
| プロパティグループ > 重み | ANI18 | animation_controller_set | covered (animation_controller_set); weight |
| 読み込み > パラメータコントローラを読み込みの確認ダイアログ | ANI18 |  | absent; no .paramctrl3.json import |
| 読み込み > 組み込み用モデルトラックの読み込みダイアログ | ANI18 |  | absent; no .paramctrl3.json import |

### 連番画像トラック (Image sequence track)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 連番画像トラック > プロジェクトパレット > データの再読込 | ANI23 | animation_image_sequence | covered (animation_image_sequence); reload |
| 連番画像トラック > プロジェクトパレット > 削除 | ANI23 | animation_image_delete | covered (animation_image_delete) |
| 連番画像トラック > タイムライン > 連番画像トラック | ANI23 | animation_image_sequence | covered (animation_image_sequence) |
| 連番画像トラック > 書き出し設定アイコン | ANI23 | animation_image_set | covered (animation_image_set); hidden |
| 連番画像トラック > グラフエディタ > 縦軸 (タイムリマップ) | ANI23 | animation_image_time | covered (animation_image_time) |
| 連番画像トラック > グラフエディタ > 横軸 (タイムリマップ) | ANI23 | animation_image_time | covered (animation_image_time) |
| 連番画像トラック > グラフエディタ > カーブ (タイムリマップ) | ANI23 | animation_image_time | covered (animation_image_time); keys |
| 連番画像トラック > シーン設定 > フレームレート | ANI22 | animation_set | covered (animation_set) |

### ファイルを収集 (Collect files)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ファイルを収集 > 保存フォルダ | EXP07 | export_collect | covered (export_collect); `dir` |
| ファイルを収集 > 保存 | EXP07 | export_collect | covered (export_collect) |

### パラメータブックマーク (Parameter bookmarks)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 設定ダイアログ > 名前とパラメータ | ANI09 | animation_bookmark_set | covered (animation_bookmark_set) |
| ラベルを個別に複製 > 複製先 > アニメーションファイル、シーン、トラック | ANI09 | animation_bookmark_copy | covered (animation_bookmark_copy); `to` |
| ラベルを個別に複製 > すべてを複製 / すべてをリセット | ANI09 | animation_bookmark_copy | covered (animation_bookmark_copy) |
| ラベルを個別に複製 > 複製 / リセット | ANI09 | animation_bookmark_set | covered (animation_bookmark_set); one label |
| ラベルを個別に複製 > パラメータブックマークの展開/集約 |  |  | absent; panel state |
| 確認ダイアログ > はい |  |  | no counterpart (Editor product) |
| 表示設定 > ラベルの配置 > キャンバス上 |  |  | absent; display style |
| 表示設定 > ラベルの配置 > トラック上 |  |  | absent; display style |
| 表示設定 > ラベルの配置 > ビュー上 |  |  | absent; display style |
| 表示設定 > ラベルの配置 > 左揃え |  |  | absent; display style |
| 表示設定 > ラベルの配置 > 右揃え |  |  | absent; display style |
| 表示設定 > ポップアップの位置を保持 |  |  | absent; panel state |
| 表示設定 > デフォルトでピン留めする |  |  | absent; panel state |
| 表示設定 > トラック複製時も保持 | ANI09 |  | absent; no track duplicate |
| 表示設定 > ポップアップの不透明度 |  |  | absent; display style |
| 表示設定 > ラベルの不透明度 |  |  | absent; display style |
| 表示設定 > ラベルの表示倍率 |  |  | absent; display style |
| ポップアップ > 名前 | ANI09 | animation_bookmark_parameters | covered (animation_bookmark_parameters) |
| ポップアップ > 設定 |  |  | absent; panel state |
| ポップアップ > ラベルの表示/非表示 |  |  | absent; panel state |
| ポップアップ > ピン留め/ピン留め解除 |  |  | absent; panel state |
| ポップアップ > 閉じる |  |  | absent; panel state |
| ポップアップ > パラメータグループ | ANI09 | parameter_get | covered (parameter_get) as a read |
| ポップアップ > パラメータ | ANI09 | animation_bookmark_parameters | covered (animation_bookmark_parameters) |
| ポップアップ > パラメータの現在の値 | ANI09 | value_set | covered (value_set) |
| ポップアップ > パラメータスライダ | ANI09 | value_set | covered (value_set) |
| ポップアップ > 展開/集約 |  |  | absent; panel state |
| 右クリックメニュー > カーソル位置のオブジェクトで表示 | ANI09 | animation_bookmark_parameters | covered (animation_bookmark_parameters); `node` |
| 右クリックメニュー > カーソル位置のオブジェクトから選択 | ANI09 | scene_geometry, editor_selection_set | covered (editor_selection_set); the client picks from scene_geometry |
| 右クリックメニュー > すべてのラベルを隠す |  |  | absent; panel state |
| 右クリックメニュー > ラベルを一括で複製(同一モデルのトラック) | ANI09 | animation_bookmark_copy | covered (animation_bookmark_copy) |
| 右クリックメニュー > トラック[トラック名]からラベルをすべて削除 | ANI09 | animation_bookmark_delete | covered (animation_bookmark_delete) |
| 右クリックメニュー > カーソル位置のラベルを削除 | ANI09 | animation_bookmark_delete | covered (animation_bookmark_delete); `index` |
| 右クリックメニュー > パラメータグループで表示 | ANI09 | animation_bookmark_parameters | covered (animation_bookmark_parameters) |
| 右クリックメニュー > 選択中のパラメータで表示 | ANI09 | animation_bookmark_set | covered (animation_bookmark_set) |
| 右クリックメニュー > オブジェクト選択用ポップアップ > 一覧 | ANI09 | scene_geometry | covered (scene_geometry) as a read |

### トラックの軌道設定 (Track trajectories)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| トラックの軌道設定 > 範囲 > 単位 | ANI14 | animation_trajectory | covered (animation_trajectory); frames |
| トラックの軌道設定 > 範囲 > 前表示範囲 | ANI14 | animation_trajectory | covered (animation_trajectory); `from` |
| トラックの軌道設定 > 範囲 > 後表示範囲 | ANI14 | animation_trajectory | covered (animation_trajectory); `to` |
| トラックの軌道設定 > 表示色 > プリセット |  |  | absent; display style |
| トラックの軌道設定 > 表示色 > 線の色 |  |  | absent; display style |
| トラックの軌道設定 > 表示色 > フレームの色 |  |  | absent; display style |
| トラックの軌道設定 > 表示色 > キーフレームの色 |  |  | absent; display style |
| トラックの軌道設定 > 表示色 > 境界線 |  |  | absent; display style |
| トラックの軌道設定 > 表示色 > 境界線の色 |  |  | absent; display style |

### ループ編集補助 (Loop editing support)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ループ編集補助 > インスペクタ > ループ編集補助 | ANI28 | animation_set | covered (animation_set) |
| ループ編集補助 > タイムライン > 0フレームより前を編集 | ANI28 |  | absent |
| ループ編集補助 > パレットメニュー > シーン範囲外へのインジケーターの移動を有効化 |  |  | absent; panel state |

### アニメーションテンプレート (Animation templates)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 入力 > テンプレート名 | ANI15 | animation_template_save | covered (animation_template_save); `name` |
| 入力 > OK | ANI15 | animation_template_save | covered (animation_template_save) |
| ファイル選択 (2.1形式をインポート) > 開く | ANI15 |  | no counterpart (Editor product); Cubism 2.1 ctmp files |
| テンプレートパレット > 新規 | ANI15 | animation_template_save | covered (animation_template_save) |
| テンプレートパレット > フォルダ | ANI15 |  | absent; templates have no folders |
| テンプレートパレット > 更新 | ANI15 | animation_template_list | covered (animation_template_list) |
| テンプレートパレット > ゴミ箱 | ANI15 | animation_template_delete | covered (animation_template_delete) |
| テンプレートパレット > テンプレートボタン (適用) | ANI15 | animation_template_apply | covered (animation_template_apply) |

### ランダムポーズ設定 (Random pose)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| ランダムポーズ設定 > すべてを展開/すべてを集約 |  |  | absent; panel state |
| ランダムポーズ設定 > 展開/集約 |  |  | absent; panel state |
| ランダムポーズ設定 > 動かすパラメータを選択 | UI09 | random_pose_seek | covered (random_pose_seek); `parameters` |
| ランダムポーズ設定 > すべて選択 | UI09 | random_pose_seek | covered (random_pose_seek); every parameter |
| ランダムポーズ設定 > 選択解除 | UI09 | random_pose_seek | covered (random_pose_seek); an empty list |

### 軽量再生 (Lightweight playback)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| タイムラインパレット > ハンバーガーボタン |  |  | absent; panel state |
| タイムラインパレット > パレットメニュー > 軽量再生 | ANI17 | editor_playback_set | covered (editor_playback_set) |

### 画像・動画書き出し (Image and video export)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 画像 (保存設定) > 拡大率 |  | export_image | covered (export_image); size |
| 画像 (保存設定) > サイズ |  | export_image | covered (export_image) |
| 画像 (保存設定) > 圧縮前容量 |  |  | no counterpart (Editor product); 200 MB limit note |
| GIFアニメ出力設定 > サイズ | EXP02 | export_gif | covered (export_gif) |
| GIFアニメ出力設定 > 縦横比を固定 |  |  | absent; dialog state |
| GIFアニメ出力設定 > フレームレート | EXP02 | export_gif | covered (export_gif) |
| GIFアニメ出力設定 > 書き出し範囲 | EXP02 | export_gif | covered (export_gif); frame range |
| GIFアニメ出力設定 > 全シーンを出力 | EXP02 | export_gif | covered (export_gif); one call per animation |
| GIFアニメ出力設定 > 高品質 | EXP02 | export_gif | covered (export_gif); supersampling |
| GIFアニメ出力設定 > ループ | EXP02 | export_gif | covered (export_gif); loops |
| GIFアニメ出力設定 > 背景を透明化 | EXP02 | export_gif | covered (export_gif); transparent |
| GIFアニメ出力設定 > 誤差拡散 | EXP02 | export_gif | covered (export_gif); dither |
| GIFアニメ出力設定 > Live2Dロゴを表示 |  |  | no counterpart (Editor product) |
| GIFアニメ出力設定 > パラメータコントローラのターゲット追従 | ANI18 | animation_controller_bake | covered (animation_controller_bake); bake first |
| GIFアニメ出力設定 > OK | EXP02 | export_gif | covered (export_gif) |
| 静止画(連番)出力設定 > サイズ | EXP01 | export_frames | covered (export_frames) |
| 静止画(連番)出力設定 > 縦横比を固定 |  |  | absent; dialog state |
| 静止画(連番)出力設定 > 書き出し範囲 > 全てを出力(連番) | EXP01 | export_frames | covered (export_frames) |
| 静止画(連番)出力設定 > 書き出し範囲 > ワークエリアを出力(連番) | EXP01 | export_frames | covered (export_frames); frame range |
| 静止画(連番)出力設定 > 書き出し範囲 > 選択中の1フレームを出力 | EXP01 | export_image | covered (export_image); frame |
| 静止画(連番)出力設定 > 書き出し範囲 > 指定のフレームを出力 | EXP01 | export_frames | covered (export_frames); frame range |
| 静止画(連番)出力設定 > ファイル名設定 > 連番桁数 | EXP01 | export_frames | covered (export_frames); numbering |
| 静止画(連番)出力設定 > ファイル名設定 > 開始番号 | EXP01 | export_frames | covered (export_frames); numbering |
| 静止画(連番)出力設定 > 書き出し形式 (PNG / JPEG) | EXP01 | export_frames | covered (export_frames); .png or .jpg |
| 静止画(連番)出力設定 > 全シーンを出力 | EXP01 | export_frames | covered (export_frames); one call per animation |
| 静止画(連番)出力設定 > 高品質 | EXP01 | export_frames | covered (export_frames); supersampling |
| 静止画(連番)出力設定 > Live2Dロゴを表示 |  |  | no counterpart (Editor product) |
| 静止画(連番)出力設定 > パラメータコントローラのターゲット追従 | ANI18 | animation_controller_bake | covered (animation_controller_bake); bake first |
| 静止画(連番)出力設定 > OK | EXP01 | export_frames | covered (export_frames) |
| PSD画像(β) > すべての原画をPSD書き出し | IO05 |  | absent; no PSD export |
| PSD画像(β) > 現在の表示内容をPSD化して書き出し | IO05 |  | absent; no PSD export |
| 動画出力設定 > 幅 / 高さ | EXP03 | export_video_start | covered (export_video_start) |
| 動画出力設定 > 縦横比を固定 |  |  | absent; dialog state |
| 動画出力設定 > フレームレート | EXP03 | export_video_start | covered (export_video_start) |
| 動画出力設定 > 書き出し範囲 | EXP03 | export_video_start | covered (export_video_start); frame range |
| 動画出力設定 > 全シーンを出力 | EXP03 | export_video_start | covered (export_video_start); one job per animation |
| 動画出力設定 > 高品質 | EXP03 | export_video_start | covered (export_video_start); supersampling |
| 動画出力設定 > 音声あり | EXP06 | export_video_start | covered (export_video_start); audio track |
| 動画出力設定 > 動画フォーマット (MP4 / WebM / MOV) | EXP03, EXP04, EXP05 | export_video_start | covered (export_video_start); container |
| 動画出力設定 > エンコード | EXP03, EXP06 | export_video_start | covered (export_video_start); codec arguments to ffmpeg |
| 動画出力設定 > ビットレート | EXP03 | export_video_start | covered (export_video_start) |
| 動画出力設定 > YUV変換 | EXP03 | export_video_start | covered (export_video_start); pixel format |
| 動画出力設定 > Live2Dロゴを表示 |  |  | no counterpart (Editor product) |
| 動画出力設定 > キャンバスを表示する | EXP03 | export_video_start | covered (export_video_start); background |
| 動画出力設定 > パラメータコントローラのターゲット追従 | ANI18 | animation_controller_bake | covered (animation_controller_bake); bake first |
| 動画出力設定 > OK | EXP03 | export_video_start | covered (export_video_start) |

### 組み込み用データ (Runtime data export)

| Field | Ledger | Soul2D command | Status |
|---|---|---|---|
| 書出し設定 (moc3) > 書き出しバージョン | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 非表示パーツを書き出し | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 下絵パーツを書き出し | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 非表示アートメッシュを書き出し | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 物理演算設定ファイルに計算FPSを書き出す | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > テクスチャに色漏れ防止の処理を適用する | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > モデルの中心X | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > モデルの中心Y | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 中心に設定 | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > キャンバス幅(ユニット単位) | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > キャンバス高さ(ユニット単位) | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > pixelsPerUnit | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 書き出しターゲット | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 書き出し種別 | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > OK | EXP08 |  | absent; Soul2D does not write moc3 (#398) |
| 書出し設定 (moc3) > 物理演算設定ファイル(physics3.json)を書き出す | PHY01 | live2d_physics3_export | covered (live2d_physics3_export) |
| 書出し設定 (moc3) > モーションシンク設定ファイル(.motionsync3.json)を書き出す | PAR27 |  | absent |
| 書出し設定 (moc3) > ユーザデータファイル(userdata3.json)を書き出す | EXP09 | live2d_userdata3_export | covered (live2d_userdata3_export) |
| 書出し設定 (moc3) > 表示補助ファイル (cdi3.json)を書き出す | EXP08 | live2d_cdi3_export | covered (live2d_cdi3_export) |
| 書出し設定 (moc3) > 表示補助ファイルにアートメッシュ情報を書き出す | EXP08 | live2d_cdi3_export | covered (live2d_cdi3_export) |
| 物理演算設定の書き出し > 保存先ダイアログ | PHY01 | live2d_physics3_export | covered (live2d_physics3_export); the caller writes the bytes |
| 物理演算設定の書き出し > FPS情報ダイアログ | PHY01 | live2d_physics3_export | covered (live2d_physics3_export); Fps is in the answer when stored |
| 物理演算設定の書き出し > FPS確認ダイアログ > はい | PHY01 |  | absent; no option to drop Fps |
| 物理演算設定の書き出し > 無効な物理演算設定の確認ダイアログ | PHY01 | live2d_physics3_edit | covered (live2d_physics3_edit); invalid edits are refused |
| モーションデータ設定 (motion3) > 書き出しシーン設定 > 全シーンを出力 | EXP08 | motion3_export | covered (motion3_export); one call per motion |
| モーションデータ設定 (motion3) > 書き出しシーン設定 > 選択中のシーンを出力 | EXP08 | motion3_export | covered (motion3_export) |
| モーションデータ設定 (motion3) > 書き出しシーン設定 > モーション書き出しが有効なシーンを出力 | EXP08 | motion3_export | covered (motion3_export); the caller chooses |
| モーションデータ設定 (motion3) > 書き出しシーン設定 > ワークエリアを出力 | EXP08 |  | absent |
| モーションデータ設定 (motion3) > 書き出し対象 | EXP08 | motion3_export | covered (motion3_export) |
| モーションデータ設定 (motion3) > モデル | EXP08 | motion3_export | covered (motion3_export) |
| モーションデータ設定 (motion3) > パラメータ (全て / 変化のある / キーのある / シャイではない / フリーズではない / 書き出さない) | EXP08 | motion3_export | covered (motion3_export); keyed parameters |
| モーションデータ設定 (motion3) > パーツ表示切替 (全て / 変化のある / キーのある / シャイではない / 書き出さない) | EXP08 | motion3_export, motion3_pose_switch | covered (motion3_pose_switch); keyed PartOpacity |
| モーションデータ設定 (motion3) > 不透明度 (書き出す / キーがある場合のみ / 書き出さない) | EXP08 | motion3_export | covered (motion3_export); keyed opacity |
| モーションデータ設定 (motion3) > リップシンク | EXP08 | motion3_export | covered (motion3_export); as a parameter curve |
| モーションデータ設定 (motion3) > まばたき | EXP08 | motion3_export | covered (motion3_export); as a parameter curve |
| モーションデータ設定 (motion3) > リップシンクとまばたきをモーションに焼き込む | EXP08 | motion3_export | covered (motion3_export); curves are already keys |
| モーションデータ設定 (motion3) > コマ打ちを適用 | ANI24 | motion3_export | covered (motion3_export); stepped keys |
| モーションデータ設定 (motion3) > イベントを書き出し | EXP09 | motion3_export | covered (motion3_export); UserData |
| モーションデータ設定 (motion3) > OK | EXP08 | motion3_export | covered (motion3_export) |
| モーション書き出し設定 > チェックボックスリスト | EXP08 | motion3_export | covered (motion3_export); one call per motion |
| モーション書き出し設定 > OK | EXP08 | motion3_export | covered (motion3_export) |

## Cubism Editor view area, toolbar and view features

Read from the Cubism Editor manual on 2026-10-20 (the view area page with its tab, display-toggle, snapshot, recording, random-pose, draw-order-slider, view-control, basic-operation and right-click entries; the toolbar page; the Solo, snapshot and multi-view pages). The manual has no separate context-menu page; the right-click entries are the ones the view area page names. 103 rows: ビューエリア (View area) 63, ツールバー (Toolbar) 23, Solo 10, スナップショット (Snapshot) 3, マルチビュー (Multi-view) 4.

Counts: covered 74, tracked 4, absent 22, no counterpart 3.

### ビューエリア (View area)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| タブ > タブの複数作成 | UI02 | view_add | covered (view_add) |
| タブ > ドラッグアンドドロップ |  |  | absent; panel state |
| タブ > すべてのビューの表示位置を合わせる | UI02 | editor_camera_sync | covered (editor_camera_sync) |
| タブ > 前のタブに切り替え | UI02 | view_select | covered (view_select) |
| タブ > 次のタブに切り替え | UI02 | view_select | covered (view_select) |
| タブ > タブを複製 | UI02 | view_add | covered (view_add) |
| タブ > タブを閉じる | UI02 | view_remove | covered (view_remove) |
| タブ > このタブ以外を閉じる | UI02 | view_remove | covered (view_remove); one call per view |
| タブ > すべてを閉じる | UI02 | view_remove | covered (view_remove); the last view stays |
| タブ > エクスプローラで開く |  |  | no counterpart (Editor product) |
| タブ > ［×］ | UI02 | view_remove | covered (view_remove) |
| 表示切替 > 描画オブジェクトのロック | UI12 | node_lock_set, nodes_list | covered (node_lock_set); the client filters by type |
| 表示切替 > デフォーマのロック | UI12 | node_lock_set, nodes_list | covered (node_lock_set); the client filters by type |
| 表示切替 > 描画オブジェクトの表示／非表示 | UI12 | node_visibility_set | covered (node_visibility_set) |
| 表示切替 > グリッドの表示／非表示 | UI06 | editor_grid_set | covered (editor_grid_set) |
| 表示切替 > Solo | UI03 | editor_solo_set | covered (editor_solo_set) |
| 表示切替 > グルー状態のオン／オフ切り替え | GEO18 |  | absent; welds are always applied |
| スナップショット > スナップショット | UI07 | sketch_snapshot | covered (sketch_snapshot) |
| スナップショット > スナップショットの表示／非表示 | UI07 |  | absent |
| スナップショット > スナップショットの保存 | UI07 | sketch_snapshot | covered (sketch_snapshot); writes the guide image |
| 録画 > 録画ボタン | PAR09 | animation_record_start, animation_record_stop | covered (animation_record_start) |
| 録画 > 録画設定 | PAR09 | animation_record_start | covered (animation_record_start); `name` |
| 録画 > 録画設定 > 新しいアニメーションデータを生成 | PAR09 | animation_create, animation_record_start | covered (animation_record_start) |
| 録画 > 録画設定 > 現在のアニメーションデータに追加 | PAR09 | animation_record_start | covered (animation_record_start) |
| ランダムポーズ > 開始 | UI09 | random_pose_seek | covered (random_pose_seek) |
| ランダムポーズ > ランダムポーズのメニュー | UI09 | random_pose_seek | covered (random_pose_seek); `parameters` |
| ランダムポーズ > 設定 | UI09 | random_pose_seek | covered (random_pose_seek) |
| フォームアニメーションビュー > ラベル |  |  | absent; panel state |
| フォームアニメーションビュー > アニメーションプレビューの表示／非表示 | ANI19 | animation_form_read | covered (animation_form_read) as a read |
| 描画順スライダー > 描画順スライダー | REN01 | node_property_set | covered (node_property_set); zsort |
| 描画順スライダー > 描画順スライダーをロック |  |  | absent; panel state |
| ビューの制御 > 背景色の変更 |  |  | absent; display style |
| ビューの制御 > 表示倍率 |  | editor_camera_set | covered (editor_camera_set); zoom |
| ビューの制御 > 拡縮スライダー |  | editor_camera_set | covered (editor_camera_set) |
| ビューの制御 > ［-］／［+］ |  | editor_camera_set | covered (editor_camera_set) |
| ビューの制御 > 原寸表示 |  | editor_camera_set | covered (editor_camera_set); zoom 1 |
| ビューの制御 > 全体表示 |  | editor_camera_fit | covered (editor_camera_fit) |
| ビューの制御 > フォーカス表示 | UI04 | editor_camera_fit | covered (editor_camera_fit); `nodes` |
| ビューの制御 > キャンバスの反転 |  |  | absent; no mirrored view |
| ビューの制御 > オニオンスキン | UI08 | editor_onion_set | covered (editor_onion_set) |
| ビューの制御 > マルチビュー設定 | UI02 | view_list | absent; one viewport, views switch with view_select |
| ビューの制御 > ディバイダ |  |  | absent; panel state |
| 基本操作 > ホイールスクロール |  | editor_camera_set | covered (editor_camera_set) |
| 基本操作 > ［Space］+［ドラッグ］ |  | editor_camera_set | covered (editor_camera_set); centre |
| 基本操作 > ［Space］+［Shift］+［ドラッグ］ |  | editor_camera_set | covered (editor_camera_set) |
| 基本操作 > ［Ctrl］+［Space］+［ドラッグ］ |  | editor_camera_set | covered (editor_camera_set); zoom |
| 基本操作 > ［R］ キャンバス反転 |  |  | absent; no mirrored view |
| 基本操作 > ［R］+［ドラッグ］ 回転 |  |  | absent; the viewport does not rotate |
| 基本操作 > ［R］+［Shift］+［ドラッグ］ |  |  | absent; the viewport does not rotate |
| 基本操作 > ［R］+［ダブルクリック］ |  |  | absent; the viewport does not rotate |
| 基本操作 > ［Ctrl］+［0］ 全体表示 |  | editor_camera_fit | covered (editor_camera_fit) |
| 基本操作 > クリック 選択 |  | editor_selection_set | covered (editor_selection_set) |
| 基本操作 > ［Shift］+［クリック］ 複数選択 |  | editor_selection_set | covered (editor_selection_set) |
| 基本操作 > 右クリック > コンテキストメニュー (デフォーマの親子関係) |  | node_get, editor_selection_set | covered (editor_selection_set); node_get gives the parents |
| 基本操作 > デフォーマの親子関係を表示 (環境設定) |  |  | absent; panel display |
| 基本操作 > ［Ctrl］+［右クリック］ ポップリスト |  | scene_geometry, editor_selection_set | covered (editor_selection_set); the client picks from scene_geometry |
| 基本操作 > ［Delete］ |  | node_delete, live2d_delete | covered (node_delete) |
| 基本操作 > ［E］+［ドラッグ］ 3D表示 |  |  | absent |
| 基本操作 > ［W］+［ドラッグ］ 描画順の3D表示 |  |  | absent |
| 基本操作 > ［E］／［W］+［ダブルクリック］ |  |  | absent |
| 基本操作 > 選択状態を隠す |  | editor_viewport_set | covered (editor_viewport_set) |
| 基本操作 > カーソル位置のオブジェクト名を表示 |  |  | absent; viewport hint |
| 基本操作 > ショートカット | UI11 |  | absent as a dialog; the Settings window's Shortcuts pane and `editor_shortcut_set` cover rebinding |

### ツールバー (Toolbar)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| すべてのワークスペース共通 > ターゲットバージョン選択 | EXP08 | motion3_export | covered (motion3_export); per export |
| すべてのワークスペース共通 > ワークスペース切り替え | UI01 | editor_workspace_apply | covered (editor_workspace_apply) |
| すべてのワークスペース共通 > 編集レベル切り替え |  |  | absent; Soul2D has no edit levels |
| すべてのワークスペース共通 > ホーム |  |  | no counterpart (Editor product) |
| すべてのワークスペース共通 > nizimaリンク |  |  | no counterpart (Editor product) |
| モデリングワークスペース > テクスチャアトラス編集 | IO04 | atlas_layout_set | covered (atlas_layout_set) |
| モデリングワークスペース > メッシュの手動編集 | GEO02 | mesh_edit, editor_tool_set | covered (mesh_edit) |
| モデリングワークスペース > メッシュの自動生成 | GEO01 | automesh_generate | covered (automesh_generate) |
| モデリングワークスペース > ワープデフォーマの作成 | GEO10 | node_create | covered (node_create); GridDeformer |
| モデリングワークスペース > ワープデフォーマの作成 > 連続作成 | GEO10 | node_create, node_insert | covered (node_insert); one call per deformer |
| モデリングワークスペース > 回転デフォーマの作成 | GEO11 | deformer_create_rotation | covered (deformer_create_rotation) |
| モデリングワークスペース > 回転デフォーマの作成 > 連続作成 | GEO11 | deformer_create_rotation | covered (deformer_create_rotation); one call per deformer |
| モデリングワークスペース > 回転デフォーマ作成ツール | GEO11 | deformer_create_rotation | covered (deformer_create_rotation) |
| モデリングワークスペース > 矢印ツール |  | editor_tool_set, editor_selection_set | covered (editor_tool_set) |
| モデリングワークスペース > 投げ縄選択ツール |  | mesh_select, editor_selection_set | covered (mesh_select); vertices |
| モデリングワークスペース > ブラシ選択ツール | GEO22, GEO23, GEO24 |  | tracked (GEO22, GEO23, GEO24) |
| モデリングワークスペース > 変形パスツール | GEO19 |  | tracked (GEO19, #169) |
| モデリングワークスペース > 変形ブラシツール | GEO22, GEO23, GEO24 | deform_edit | tracked (GEO22, GEO23, GEO24); deform_edit moves vertices |
| モデリングワークスペース > グルーツール | GEO18 | part_set_weld | covered (part_set_weld) |
| モデリングワークスペース > アートパスツール | GEO06, GEO07, GEO08, GEO09 | live2d_artpath_point_set | tracked (GEO07, #159) |
| アニメーションワークスペース > 選択・編集ツール | ANI18 | animation_controller_move | covered (animation_controller_move) |
| アニメーションワークスペース > パラメータコントローラツール | ANI18 | animation_controller_add, animation_controller_delete | covered (animation_controller_add) |
| フォームアニメーションワークスペース > モデリングワークスペースと同じツール | ANI19 | animation_form_key | covered (animation_form_key); edits are keyed per frame |

### Solo

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| Solo > ［Solo］ボタン | UI03 | editor_solo_set | covered (editor_solo_set) |
| Solo > 不透明度を反映 | UI03 | editor_solo_set | covered (editor_solo_set); the preview keeps opacity |
| Solo > 乗算色・スクリーン色を反映 | UI03 | editor_solo_set | covered (editor_solo_set); the preview keeps tints |
| Solo > 非アクティブカラー | UI03 | editor_solo_set | covered (editor_solo_set); inactive colour |
| Solo > オフスクリーン描画の反映 > クリッピング | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |
| Solo > オフスクリーン描画の反映 > 不透明度 | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |
| Solo > オフスクリーン描画の反映 > 乗算色 | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |
| Solo > オフスクリーン描画の反映 > スクリーン色 | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |
| Solo > オフスクリーン描画の反映 > カラーブレンド | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |
| Solo > オフスクリーン描画の反映 > アルファブレンド | UI03, REN04 | editor_solo_set | covered (editor_solo_set); the Composite renders as usual |

### スナップショット (Snapshot)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| スナップショット機能 > スナップショット | UI07 | sketch_snapshot | covered (sketch_snapshot) |
| スナップショット機能 > スナップショットの表示/非表示 | UI07 |  | absent |
| スナップショット機能 > スナップショットの保存 (パーツパレットへ) | UI07 | sketch_snapshot | covered (sketch_snapshot); the guide image is a node |

### マルチビュー (Multi-view)

| Control | Ledger | Soul2D command | Status |
|---|---|---|---|
| マルチビュー > マルチビュー アイコン > 任意のレイアウト | UI02 |  | absent; one viewport |
| マルチビュー > タブの移動・追加・削除 | UI02 | view_add, view_remove, view_select | covered (view_add) |
| マルチビュー > パラメータ値をタブ間で同期 | UI02 | view_sync_set | covered (view_sync_set) |
| マルチビュー > すべてのビューの表示位置を合わせる | UI02 | editor_camera_sync | covered (editor_camera_sync) |


## Soul2D editor panels (`rust/src/editor_ui.rs`)

Read from the source on 2026-10-20: the 118 distinct control labels of `editor_ui.rs` (buttons, check boxes, selectable labels, menu buttons and collapsing headers), grouped by panel, 20 rows. `covered` means every control of the group ends in a Session operation that an MCP tool also reaches; `GUI-only` names a group that does not.

Counts: covered 19, GUI-only 1.

| Panel | Controls | Session operation / tool | Status |
|---|---|---|---|
| Parameters panel | + Add parameter, Add, Convert To..., Move to group, Group color, Edit, Delete group, Rename group | parameter_create, parameter_convert_to_2d, parameter_move, parameter_group_set_color, parameter_group_delete, parameter_rename | covered (parameter_create) |
| Parameters panel | Apply axes, Apply name, Show all parameters, Parameter usage / Parameter Usage, More Info | parameter_set_axes, parameter_rename, editor_parameters_set, parameters | covered (parameter_set_axes) |
| Parameters panel, armed bindings | Copy keypoint, Paste keypoint, Remove bindings, Edit forms in the viewport, Edit Mesh, Select node, Convert | binding_copy, binding_paste, binding_remove, deform_edit, mesh_edit, editor_selection_set, node_convert | covered (binding_copy) |
| Parameters panel, flip and mirror menus | Edit ▾ (Flip X / Flip Y / Mirror / Mirrored Autofill / Set from mirror / Duplicate and Horizontal Flip / Paste and Horizontal Flip / Split... / Link) | parameter_flip, parameter_mirror, parameter_mirrored_autofill, parameter_duplicate_flip, parameter_paste, parameter_split, parameter_link | covered (parameter_flip) |
| Node tree | Add, Insert, Convert To..., Move, Paste, Split... | node_create, node_insert, node_convert, node_move, node_paste, split_open | covered (node_create) |
| Node tree, visibility and lock columns | eye and lock toggles, Welcome's Open / Import | node_visibility_set, node_lock_set | covered (node_visibility_set) |
| Animation controls | Create animation, Delete animation, Rename animation, Apply animation settings, Play, Pause, Seek, Next frame, Previous frame, Reset preview, Lock playback to animation framerate | animation_create, animation_delete, animation_rename, animation_set, animation_advance, animation_seek, animation_reset, editor_playback_set | covered (animation_create) |
| Timeline | Keyframe, Add, Delete key, Apply key value, Apply key tension, Set marker, Move marker, Lock marker, Delete marker, Shy, Shy marks, Freeze, Freeze marks, Bookmarks, Keep as label | animation_key_add, animation_key_delete, animation_key_set, animation_lane_set, animation_marker_set, animation_marker_delete, animation_shy_set, animation_freeze_set, animation_bookmark_set | covered (animation_key_add) |
| Timeline, images and physics | Image sequences and timing, Remap form timing, Bake physics, Bake target tracking, Delete all controllers | animation_image_sequence, animation_image_time, animation_form_time, animation_physics_bake, animation_controller_bake, animation_controller_delete | covered (animation_image_sequence) |
| Timeline zoom | Zoom out, Timeline zoom slider, Zoom in, hover zoom gesture | editor_timeline_zoom_set; resource://soul2d/editor/state `timeline.zoom` | covered (editor_timeline_zoom_set); absolute app-local multiplier, clamps to 0.5–10; retains native same-draw controls and next-draw gesture geometry, playback and document state; no new fit/reset behavior |
| Tools panel | Autosave settings, Auto mesh (Generate mesh, Extend line), Import PSD/KRA, Import PSD depth map..., Merge PSD/KRA, Export INP, Export image, Export frames, Run ffmpeg, Cancel export, Language | editor_settings_set, automesh_generate, import_psd, import_kra, depth_import_psd, merge_psd_apply, merge_kra_apply, export_inp, export_image, export_frames, export_video_start, export_video_cancel | covered (export_video_start) |
| Tools panel | Probe codecs | export_video_probe, export_video_probe_status | covered (export_video_probe); shared status/result at resource://soul2d/export/video-codecs |
| Tools panel | Show ffmpeg command | export_video_command | covered (export_video_command); last successful preview at resource://soul2d/export/video-command |
| Live2D panels | Linked model resources, Add CAN3 scene / track / curve / key and their Confirm / Cancel / Discard buttons, Refresh Part choices, CAN3 source / edit anchors, CMO3 source summary, Physics groups (with its Presets section: list kind, name, Save or Replace, Apply, Delete), Live2D source forms (Apply vertex, Apply opacity, Apply form settings, Apply source settings), Live2D objects (Parent Part / Move; ArtMesh Current target, Rotation destination / Reparent to Rotation for bounded static edits) | live2d_form_set, live2d_source_set, live2d_reparent, live2d_reparent_deformer, live2d_physics3_preset_save, live2d_physics3_preset_list, live2d_physics3_preset_delete, live2d_animation_resource_add, live2d_animation_scene_add, live2d_animation_track_add, live2d_animation_curve_add, live2d_animation_key_add, live2d_animation_scene_delete, live2d_animation_track_delete, live2d_animation_curve_delete, live2d_authoring_read | covered (live2d_animation_key_add) |
| Live2D runtime panels | Resource binding, Attach linked file, Pose3 (Attach Pose, Reset Pose), Physics3 (Attach Physics3, Step Physics3, Stabilize, Reset Physics3), Expressions (Import expression, Reset expression), Model parameters / facial controls, Runtime motions (Reset motion, Fit preview) | live2d_animation_bindings, live2d_model_attach, live2d_pose_attach, live2d_pose_reset, live2d_physics_attach, live2d_physics_step, live2d_physics_stabilize, live2d_physics_reset, live2d_expression_import, live2d_expression_reset, live2d_parameter_set, motion3_reset | covered (live2d_model_attach) |
| History | Undo history, Clear history | history_summary, history_goto, history_clear | covered (history_goto) |
| Part split review | Part split review, Apply split | split_plan, split_apply | covered (split_apply) |
| Coding agent | Coding agent (ACP session, prompt, permission replies) |  | GUI-only controls; lifecycle is partially observable through `resource://soul2d/editor/state` under `codingAgent` (seven nonsecret processed-state booleans). Start/Stop, sessions, prompts and permission replies remain native controls; ACP tool calls reach the shared Session through the editor's MCP endpoint |
| Viewport | Zoom in, Zoom out, 1: follows the parents, 2: model coordinates | editor_camera_set, editor_viewport_set | covered (editor_camera_set) |
| Dialogs | Save As, Keep for later, Add with suffix, Confirm / Cancel deletions, Discard new ... | save, editor_recovery_restore, editor_recovery_keep | covered (editor_recovery_keep); Keep for later dismisses the startup offer only; other confirmations remain host-owned, the operations refuse or apply in one call |
