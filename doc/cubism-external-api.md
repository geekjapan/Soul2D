# Cubism external application API: decision and mapping

Status: **PROPOSED**, 2026-10-12 (issue #97, ledger row API01). Target: Cubism Editor 5.3.04.
No vendor Editor was available, so nothing here is a vendor acceptance result.

## Decision

Soul2D does **not** implement a wire-compatible endpoint for the Cubism Editor
external application integration API. It documents, per vendor method, the Soul2D
MCP tool or JSONL operation that gives the same information or effect, and says
where none exists (table below). The maintainer accepted this on 2026-10-12, the explicit decision
[acceptance-scope.md](acceptance-scope.md) requires; it is recorded in [ADR 0002](adr/0002-cubism-external-api-mapping.md).

Reasons:

- Soul2D already reaches one `Session` through JSONL, stdio MCP, HTTP MCP and ACP
  ([ARCHITECTURE.md](ARCHITECTURE.md), `rust/src/main.rs`). ACP reaches it through
  the MCP server. A fifth surface would add a second protocol to keep in step.
- A vendor-style endpoint needs its own authentication and permission model. The
  vendor model is a token plus a per-client "Allow" box in the Editor. Soul2D HTTP
  MCP checks loopback `Host`/`Origin` and an optional bearer token
  (`SOUL2D_MCP_BEARER`, required for non-loopback binds; `rust/src/mcp_http.rs`).
  It has no per-client approval and no WebSocket server (no `websocket` or
  `tungstenite` match in `rust/src` or `rust/Cargo.toml`).
- Conformance cannot be tested. No Cubism Editor is installed in the reference
  environment ([cubism-53-research.md](cubism-53-research.md)).
- No client that needs wire compatibility has been identified.

Cost of this choice: a program written for the vendor API does not run against
Soul2D unchanged. It needs an adapter that speaks MCP.

## Vendor API facts

Pages read on 2026-10-12. Page dates are the "Updated" lines on each page.

| Page | URL | Page date |
| --- | --- | --- |
| Overview and protocol | https://docs.live2d.com/en/cubism-editor-manual/external-application-integration-api/ | 2024-10-24 |
| List of functions | https://docs.live2d.com/en/cubism-editor-manual/external-application-integration-api-list/ | 2025-08-26 |
| User settings page | https://docs.live2d.com/en/cubism-editor-manual/external-application-integration/ | 2024-12-17 |
| Update history (5.3) | https://docs.live2d.com/en/cubism-editor-manual/updates-history5-3 | lists 5.3.04 (2026-09-03) |
| Sample repository | https://github.com/Live2D-Garage/CubismExternalAppPluginSamples | not dated; 5 commits seen |

- Transport: WebSocket, JSON as UTF-8 text. The Editor is the server. Default port
  22033, changeable in File > External Application Integration Settings.
- Remote clients are refused unless "Allow remote connections" is on. The page
  warns about attacks over the network.
- Envelope: `Version` (optional, required in 0.9.0), `Timestamp`, `RequestId`
  (echoed if sent), `Type` (`Request`, `Response`, `Event` or `Error`), `Method`,
  `Data`. Errors carry `ErrorType` in `Data`.
- Registration: the client calls `RegisterPlugin` and gets a token. The token alone
  grants nothing. The user must tick "Allow" in the Editor. A later connection that
  sends the old token and is allowed gets the same token back and works at once.
  Until then most methods return `PluginNotRegistered`.
- Values set through the API go through a temporary buffer that is dropped about
  0.5 seconds after use. Clients should wait for each response before the next call.
- `ModelUID` and `DocumentUID` are temporary and may change per connection.
  Parameter IDs such as `ParamAngleY` are stable.
- Events must be switched on first with an `Enabled` request.
- Error types (all since 0.9.0): `InvalidJson`, `UnsupportedVersion`,
  `MethodNotFound`, `InvalidType`, `InvalidData`, `PluginNotRegistered`,
  `InvalidParameter`, `InvalidModel`, `InvalidDocument`, `InvalidView`.
- Method list: 15 instructions and 5 events, as in the table. API versions seen
  on the list page: 0.9.0, 0.9.1, 0.9.2, 0.9.3, 1.0.1.
- GetParameters and keyform data: the list page shows a `Keyform` list of
  `{ Value }` entries in the response, and only in the 1.0.1 table. The 5.3 update
  history says that 5.3.00 beta1 (2025-08-26) made keyform information available
  from `GetParameters`. Joining "1.0.1" to "Editor 5.3" is our inference from two
  pages. No page states it.
- Page oddities, recorded as read: the `GetParameters` field list says "Max,Max";
  the 0.9.0 `GetParameters` text mentions keyform values but its table has no
  `Keyform`; `GetPhysicsInfo` lists `Fps` in its request column;
  `GetCurrentDocumentUID` lists a `DocumentUID` request field.
- Out of the 5.3.04 target: the 5.4 alpha manual describes editing methods. They
  are not listed here.
- The sample repository holds three samples (parameter control, a nizima LIVE
  bridge, parameter store and restore). It has no protocol document.

## Mapping table

Status: **Equivalent** (same information and effect), **Partial** (the difference is
named), **None** (no counterpart). Tool names were checked against
`doc/commands.manifest.json`. `INX` means a Soul2D project; `CMO3` means a
native Cubism source document opened in the session.

| Vendor method or event | What it does | Soul2D tool or operation | Status |
| --- | --- | --- | --- |
| RegisterPlugin | Registers a client, returns a token | No method. Transport level: loopback checks and optional bearer token, fixed at launch | None |
| GetIsApproval | Reports whether the user allowed this client | No method. There is no per-client approval | None |
| GetParameterValues | Reads current values, all or by ID | `snapshot` (`values`, every parameter). `evaluate` for given values | Partial: no ID filter. INX keys are numeric UUIDs, CMO3 keys are string IDs |
| SetParameterValues | Sets values for several parameters | `value_set` (INX, one UUID, a two-number value), `live2d_parameter_set` (CMO3, one string ID, within range) | Partial: one parameter per call. Values stay until changed (`values_reset` resets INX values only); there is no 0.5 s buffer |
| GetParameters | Lists parameters: ID, name, group, default, min, max, repeat, type, and (1.0.1) keyform values | INX: `parameters` and `parameter_get` (records hold `uuid`, `name`, `min`, `max`, `defaults`, `groupUUID`, `axis_points`, and `repeat` when a 1D parameter loops (set with `parameter_set_repeat`); `axis_points` is the keyform position list). CMO3: `live2d_authoring_inspect`, `live2d_authoring_read` | Partial: CMO3 `inspect` returns id, name, minimum, maximum, default only. Keyform grids come from `live2d_authoring_read`, not the same call. `live2d_authoring_read` returns `repeat` and `parameterType` per parameter |
| GetParameterGroups | Lists parameter groups | INX: `parameter_get` reads a group by UUID; `parameter_group_create` and related tools edit groups. `parameters` carries `groupUUID` on members | Partial: a separate group list was not confirmed; none for CMO3 |
| GetDocuments | Lists open documents by kind, with views | `resource://soul2d/document/state`, `snapshot.documentFormat` (`inx`, `live2d.cmo3`, `live2d.can3`) | Partial: one document per session. No list and no file path in the state resource; the state resource lists the views of the document (`views`) |
| GetDocument | Details of one document by UID | Same resource | Partial: no document UID |
| GetCurrentDocumentUID | UID of the current document | None for the UID. `revision` in the state resource changes on edit but is not an identity | Partial: no UID |
| GetCurrentModelUID | UID of the current model | `nodes_list` gives node UUIDs. The model has no UID of its own | Partial: no model UID |
| GetCurrentEditMode | Returns Physics, Modeling, Animation, ModelingMeshEdit or FormAnimation | `snapshot.documentFormat` tells model from animation document | Partial: no mesh-edit, form-animation or physics-editor mode |
| SetGlobalVersion | Pins the API version for this client | No method. MCP `initialize` reports `protocolVersion` (`2024-11-05`). The tool list is read with `tools/list` | None |
| ClearParameterValues | Drops buffered values so the Editor's own take over | `values_reset` (INX only) | Partial: sets every INX parameter to its default. It does not release a buffer and does not reset CMO3 values |
| GetPhysicsInfo | Returns the calculated physics FPS | None. `physics_step` and `live2d_physics_step` advance by a given `dt` and return no FPS | None |
| SendCubismLog | Writes a line to the Editor log palette | None. No tool writes to a log view | None |
| NotifyPhysicsFileExported (event) | Pushes the path of an exported physics file | None. No physics export tool in the manifest, and no server push | None |
| NotifyMocFileExported (event) | Pushes the path of an exported MOC3 and related files | None. Soul2D does not read or write `.moc3` (`document.rs` refuses it) | None |
| NotifyMotionFileExported (event) | Pushes the path of an exported motion file | `motion3_export` returns its result to the caller | Partial: reply to the caller only. No push for exports made in the GUI or by another client |
| NotifyMotionSyncFileExported (event) | Pushes the path of an exported motion-sync file | None. No motion-sync tool in the manifest | None |
| NotifyChangeEditMode (event) | Pushes the new edit mode | None. `notifications/*` handling in `rust/src/mcp.rs` only accepts `initialized` and `cancelled`; HTTP is POST only | None |

Count: 20 rows. Equivalent 0, Partial 11, None 9. Clients that poll can use
`snapshot` and the state resource to detect changes through `revision`.

## Related products

The Cubism manual lists these next to the Editor (index read on 2026-10-12 from the
5.3 update history page). They are separate products. Soul2D does not ship, embed
or replace any of them.

| Product | What it is (manual, read 2026-10-12) | Soul2D position |
| --- | --- | --- |
| Cubism Viewer (for OW) | Previews `.moc3` runtime data with pose and expression files ([page](https://docs.live2d.com/en/cubism-editor-manual/selection-of-viewer/), 2023-08-17) | No counterpart. Soul2D does not read `.moc3`. For its own and CMO3 sources use the editor viewport, `render` and `view_screenshot` |
| Cubism Viewer for Unity | Previews model data inside Unity (same page) | No counterpart. No Unity runtime is in this tree |
| Material separation Photoshop plug-in | Cuts recognised objects out of a selection into new layers ([page](https://docs.live2d.com/en/cubism-editor-manual/material-separation-ps-plugin-manual/), 2025-10-28) | No counterpart. Separate layers in Photoshop, then use `import_psd` |
| Scripts for Photoshop | Two scripts that clean a PSD before import ([page](https://docs.live2d.com/en/cubism-editor-manual/script-download/), 2025-08-05) | No counterpart. Run the scripts in Photoshop, then `import_psd` |
| Cubism AE plug-in | Shows Cubism models and edits motion in After Effects ([page](https://docs.live2d.com/en/cubism-editor-manual/cubism-ae-plugin-about/), 2020-10-29) | No counterpart. `motion3_export` writes Motion3 JSON; whether the plug-in accepts that file was not tested |
| Verification tool (MOC3 consistency checker) | Checks that a `.moc3` is well formed and unmodified ([page](https://docs.live2d.com/en/cubism-editor-manual/moc3-consistency-checker/), 2025-08-26) | No counterpart. Soul2D does not read `.moc3` |

## When to revisit

Reopen the decision when all of these hold:

1. A named client needs the vendor wire protocol and cannot use MCP or a small
   adapter. The three Live2D samples are known examples; none has asked.
2. A Cubism Editor 5.3.04 or later is available to test against, so conformance
   can be checked rather than assumed.
3. A permission model for a WebSocket surface is agreed, covering per-client
   approval and remote connections.

A vendor adapter would be a thin client of the existing MCP surface, not a new
owner of document state.
