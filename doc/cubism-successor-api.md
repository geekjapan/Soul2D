# Cubism successor external API editing inventory

Status: **DOCUMENTED INVENTORY AND RELATED TOOL MAPPING; VENDOR ACCEPTANCE UNTESTED**.
Reviewed on 2026-11-09 against Soul2D commit `eef7147`.
This reference fills NEXT05 in the [successor backlog](cubism-feature-ledger.md#successor-feature-backlog).
The compatibility baseline remains Cubism Editor 5.3.04.

## Versioned sources

The official [alpha1 developer manual](https://cubism.live2d.com/editor-alpha/doc/manual/alpha1/ja/external-api-intergration/index.html)
is dated 2026-07-14. Its 36 added method and event headings all name API version
`1.1.0`. This is a documentation inventory, not a protocol-conformance result.
The [alpha2 user manual](https://cubism.live2d.com/editor-alpha/doc/manual/alpha2/ja/index.html),
dated 2026-09-07, links to that same developer document through the
[external API redirect](https://cubism.live2d.com/link/manual5_4_alpha_external-api-intergration).
No separately revised alpha2 method inventory was established by that link.

The [5.3 API mapping](cubism-external-api.md) separately records 20 existing
instructions and events. `SetParameterValues` and `ClearParameterValues` belong
to that earlier list. They are not additions in the inventory below.
API01's accepted mapping does not accept these successor methods.

The [alpha announcement](https://www.live2d.com/en/information/cubism-5_4-alpha/)
states that downloading and using the alpha ended on 2026-10-18 at 23:59 JST.
No expired alpha was run. Source-file acceptance, runtime support, and vendor
compatibility remain separate outcomes.

## Editing permission and cancellation

The documented workflow requires editing permission in addition to connection
permission. The Editor locks its own operations during external editing.
[Editing starts](https://cubism.live2d.com/editor-alpha/doc/manual/alpha1/ja/external-api-intergration/index.html#_aks5hy9hy7ke)
with `EditBegin` and finishes with `EditEnd`. Editing calls fail before a
successful start. A long operation can force the progress dialog to appear even
when the request uses `Silent=true`.

[`EditEnd`](https://cubism.live2d.com/editor-alpha/doc/manual/alpha1/ja/external-api-intergration/index.html#_cg1ub3n8403j)
with `Cancel=true` restores the pre-edit history state without adding history.
Its result reports whether restoration succeeded. With `Cancel=false`, the
result reports normal completion. The method returns false without editing
permission. The manual also describes the common-method category as not needing
editing permission. These statements do not establish a broader permission
exception for `EditEnd`.

A [user cancellation](https://cubism.live2d.com/editor-alpha/doc/manual/alpha1/ja/external-api-intergration/index.html#_4a7sq555kr0b)
restores the pre-edit state and makes later editing calls fail.
`NotifyUndoCancel` reports cancellation through the dialog.
Disconnect recovery, crash recovery, multiple-client exclusion, and the Undo unit
after successful completion were not established by this documentation review.
The general editing section requires `EditBegin`; getter error tables alone do
not establish that getters are exempt.

Soul2D's native edits use document revisions and object identities. Individual
calls validate a candidate model before committing it to the shared Session.
`undo`, `redo`, and `history_goto` navigate retained history. These operations
do not create a client-owned transaction or isolate intervening edits by another
client. `expectedRevision` detects stale state; it does not grant edit permission.
HTTP bearer authentication and Host/Origin checks do not implement the vendor's
per-client editing approval.

## Complete added-method inventory and related native tools

All 36 entries below come from the versioned developer manual. `NotifyEvent` is
a category heading, not a 37th method. `GetSelectedObjecs` retains the official
spelling. The documented editing target is Modeling mode.

**RELATED** identifies implemented Soul2D operations with narrower behavior.
It does not mean equivalent requests, responses, identifiers, or transactions.
**MISSING** identifies a native operation or contract absent from the reviewed
manifest and native allowlist. Every successor vendor workflow is **UNTESTED**.
Tool definitions are in [the command manifest](commands.manifest.json).

| Added method or event | Related Soul2D operation and remaining difference |
| --- | --- |
| `GetIsEditApproval` | **MISSING**. No vendor-style per-client editing approval query. |
| `EditBegin` | **MISSING**. No client-owned multi-call edit session or matching Editor lock. |
| `EditEnd` | **MISSING**. History navigation is not an owned commit or cancellation boundary. |
| `EditSendLog` | **MISSING**. No log method attached to an external edit dialog. |
| `EditSendProgress` | **MISSING**. No progress method attached to that dialog. |
| `NotifyUndoCancel` | **MISSING**. No equivalent dialog-cancellation event. |
| `AddParameterKey` | **RELATED**. `live2d_keyform_add` adds a key to an existing supported ArtMesh or Warp parameter axis. It does not provide the vendor request contract. |
| `DeleteParameterKey` | **RELATED**. `live2d_keyform_delete` removes a supported axis key. It refuses the last key and unsupported interpolation or blend-shape cases. |
| `MoveParameterKey` | **MISSING**. No native key-position move tool was identified. |
| `GetParameterKeys` | **RELATED**. `live2d_authoring_read` exposes stored grids and forms, not the vendor's dedicated key response. |
| `GetObjectsByParameterKeys` | **RELATED**. `live2d_authoring_read` exposes source/form associations. No matching key-filtered query is exposed. |
| `GetParameterStructure` | **RELATED**. Native reads expose parameter records. A matching parameter/group tree response is missing. |
| `AddParameter` | **MISSING**. No donor-free native parameter creation. `live2d_duplicate` requires an existing donor. |
| `AddParameterGroup` | **MISSING**. INX `parameter_group_create` does not create a native CMO3 group. |
| `EditParameter` | **RELATED**. `live2d_parameter_edit`, `live2d_rename`, and `live2d_set_id` edit bounded stored fields. They do not implement the full vendor method. |
| `EditParameterGroup` | **MISSING**. No native group-edit method. |
| `DeleteParameter` | **RELATED**. `live2d_parameter_delete` refuses parameters used by source keyforms or physics. |
| `DeleteParameterGroup` | **MISSING**. No native group-delete method. |
| `MoveParameter` | **MISSING**. No native parameter-order or group-move method. |
| `MoveParameterGroup` | **MISSING**. No native group-order or hierarchy-move method. |
| `GetSelectedObjecs` | **RELATED**. `resource://soul2d/editor/state` describes Soul2D editor selection. Vendor object-ID and selection-type equivalence is untested. |
| `AddSelectedObjects` | **RELATED**. Editor-only `editor_selection_set` replaces the node selection. It does not expose the same additive request. |
| `ClearSelectedObjects` | **RELATED**. `editor_selection_set` with an empty node list clears node selection. Vendor selection-category equivalence is untested. |
| `GetPartStructure` | **RELATED**. `live2d_authoring_read` exposes native sources and Part membership, not the vendor tree response. |
| `GetObject` | **RELATED**. `live2d_authoring_read` exposes source records and stored forms. Vendor object schemas and identifiers differ. |
| `DeleteObject` | **RELATED**. `live2d_delete` applies source, storage, and reference guards. It is not unrestricted subtree deletion. |
| `MoveObjectOnPartsPalette` | **RELATED**. `live2d_reparent` appends under an existing Part. It does not expose indexed insertion. |
| `AddPart` | **MISSING**. Native `live2d_duplicate` copies an existing empty Part; it is not donor-free creation. |
| `EditPart` | **RELATED**. Identity tools, `live2d_source_set`, and `live2d_form_set` edit supported stored Part fields and primary forms. |
| `EditArtMesh` | **RELATED**. Identity tools, `live2d_source_set`, `live2d_form_set`, and `live2d_user_data_set` edit supported fields. Full vendor field equivalence is untested. |
| `EditGlue` | **RELATED**. Identity tools and `live2d_form_set` edit bounded Glue properties. Target pairs, vertex bindings, and weights have no exposed edit operation. |
| `GetDeformerStructure` | **RELATED**. `live2d_authoring_read` exposes deformer targets, not the vendor hierarchy response. |
| `AddRotationDeformer` | **MISSING**. No donor-free native creation. INX `deformer_create_rotation` does not fill this gap. |
| `AddWarpDeformer` | **MISSING**. No donor-free native creation. INX grid-definition operations do not fill this gap. |
| `EditRotationDeformer` | **RELATED**. `live2d_form_set` edits existing primary angle, origin, scale, and reflection fields. General hierarchy editing is missing. |
| `EditWarpDeformer` | **RELATED**. `live2d_form_set_vertex`, identity tools, and existing-key operations edit bounded stored data. Native division/resolution changes and general reattachment are missing. |

## Mapping limits

The vendor's [`GetObject`](https://cubism.live2d.com/editor-alpha/doc/manual/alpha1/ja/external-api-intergration/index.html#_gn2b5hkdi2qq)
documents ArtMesh, Part, WarpDeformer, RotationDeformer, and Glue records.
It explicitly excludes ArtPath retrieval. The added inventory has no ArtMesh or
Glue creation method and no vertex-coordinate editing method. Soul2D's separate
ArtPath and vertex tools do not establish additional vendor API methods.

Soul2D's `live2d_duplicate` copies an existing donor and retains supported stored
relationships. `parameter_create`, `node_create`, and ordinary deformer creation
tools operate on INX. The native allowlist refuses those operations.
Native parameter edits require finite values, a valid minimum/default/maximum
order, and existing keys inside the proposed range. Native object edits also
refuse unsupported shared storage, references, generated forms, or source kinds.

`live2d_reparent_deformer` supports a static ArtMesh moving to an existing static
Rotation chain with coordinate checks. It refuses Warp, keyed, Glue-bound,
reflected, and other unsupported cases. It is not general deformer reparenting.
`live2d_form_set_opacities` can commit one validated batch as one Undo step.
Separate successful calls still do not form one multi-call transaction.

The mapping was checked against `rust/src/mcp.rs`, the native allowlist and
history operations in `rust/src/lib.rs`, field guards in
`rust/src/authoring_fields.rs`, and donor copying in `rust/src/authoring_copy.rs`.
This source review does not execute those tools or establish complete coverage
of vendor fields. Native successor-schema fixtures, actual vendor workflows,
and matching runtime behavior remain **UNTESTED**. Issue #1 stays open.
