# Live2D authoring containers

The supplied four CMO3 files expose model definitions without acquiring Cubism
Core or Editor. The Rust `caff` module reads their CAFF containers, validates the
compressed XML, and replaces that XML while preserving image payloads and unknown
container data. This is a prerequisite for model import and authoring; it is not
yet a complete Cubism model editor.

`live2d.authoring.inspect` is a read-only Session request with a file `path`.
JSONL and the C ABI use that name; stdio MCP exposes
`live2d_authoring_inspect`. It returns container entries, XML format version,
model name, canvas metadata, actual parameter IDs/ranges/defaults, and mesh and
deformer definition counts. It leaves the active INX document, runtime state,
revision, history, and pending deformation unchanged. CAN3 containers can be
inspected too, but this does not import their animation timelines.

## Observed format

The implementation was derived from the user-provided files, without copying
another project's codec. These findings describe the supplied profile, rather
than an official CAFF specification:

- All eight CMO3/CAN3 files use `CAFF` and profile 45. Other profiles are rejected.
- A per-file seed encodes table integers and filename/payload bytes. Each entry
  retains its name, alias, unknown integer, offset, size, flags, and reserved data.
- `main.xml` contains a single ZIP local entry named `contents`, using raw
  DEFLATE and a data descriptor. It has no ordinary ZIP central directory.
- The reader checks exact stream consumption, descriptor CRC and sizes, duplicate
  names, table/payload ranges, and overlaps. Limits are 256 MiB per container,
  64 MiB expanded XML, and 100,000 entries. XML inspection rejects DTDs and limits
  its parsed tree to one million nodes.
- Unchanged encoding is byte-exact. XML replacement relocates subsequent payloads
  and retains untouched assets, opaque fields, gaps, and trailing bytes.

The decoded XML contains shared objects linked by `xs.id`/`xs.ref`, model
definitions, ArtMesh vertices/UVs/triangles, keyforms, deformers, parameter
bindings, image resources, and clipping. Reading these records is only the first
step: default-pose geometry must resolve keyform interpolation, coordinate spaces,
deformer transforms, texture resource transforms, and masks. Copying the first
keyform or raw positions is insufficient for equivalent animation.

## CAN3 parameter-key source reads

`live2d.animation.read` requires a CAN3 file `path`; stdio MCP exposes
`live2d_animation_read`, and the editor's embedded MCP exposes
`Live2DCommand_AnimationRead`. These requests read a separate `CAnimation`
project without opening it or changing the active model, runtime, revision, or
undo/redo history. ACP agents can discover the embedded tool through MCP;
an actual ACP CAN3 workflow has not yet been verified.

The response describes linked model resources, scenes, model tracks, and existing
parameter curves from `keyParamEffect.attrList`. Scene and track GUIDs and the
raw attribute ID identify each channel. `parameterGuid` and `sourceParameter`
retain the two observed source identity conventions: Hiyori's `s` text such as
`live2dParam:PARAM_ANGLE_X`, and Ren/Zundamon's `CParameterId.idstr` such as
`ParamAngleX`. `parameterId` is only the literal suffix of the attribute ID;
it is not a verified binding to a linked CMO3 parameter.
A scene shows `tag` and a parameter curve shows `baseValue` only where the file stores them.

Each scene also has `imageTracks` with `guid`, `name`,
`resourceGuid`, `start`, `internalOffset`, `duration`, `visualCurves` and `pointChannels`. It is separate from the
model-only `tracks` array and does not affect model playback or curve selectors.
The existing track-delete operation accepts both Model and Image tracks. Image deletion removes
the owned track, VisualDefault effect and attributes, and its source/group members;
it retains the linked image resource. Image metadata edits, blank creation and playback
are unsupported. Omitting this list from a rewritten projection does not request
deletion. Shared or externally referenced owned storage is refused before history
changes; referenced attribute-map keys remain unsupported.

`soundTracks` lists existing Sound track `guid`, `name`, `resourceGuid`, `start`,
`internalOffset` and `duration`. `live2d.animation.track.delete` can remove a
track with one SoundDefault effect and one fixed volume attribute. The track,
its GUID, effect and volume definition are removed with the source/group members.
Linked audio resources and shared identity objects stay, even after their final
track is deleted. Additional effects, shared mutable storage, inconsistent
memberships and outside references are refused. Retained Sound rows cannot be
edited or reordered. Sound creation, volume editing and audio playback
are unsupported.

`guideTracks` lists existing GuideImage tracks with the same six identity and timing
fields as `soundTracks`, plus the six supported scalar `visualCurves` described below.
Existing scalar anchor values can be edited through `live2d.animation.key.set`; existing scalar channels
also support `live2d.animation.key.add` and `live2d.animation.key.delete`.
Guide edits require the complete supported ownership layout and consistent inline key/cache storage.
Track deletion supports the validated VisualDefault and
GuideImageDefault layout with nine visual attributes and two distinct image-number
attributes. The list attribute and named attribute remain separate even when their
IDs match. Deletion removes fifteen owned definitions and the source/group members,
retaining the linked guide-directory resource and shared metadata. Empty effect maps
need explicit zero counts and string keys. Unknown effects, shared mutable storage,
outside references and other layouts are refused. Guide directories are never opened.
Guide creation and playback remain unsupported. Scene deletion includes
supported Guide tracks and preserves linked directory resources and shared identities.

Model-track deletion also supports local FormEdit data whose keyform objects and
frame payloads have distinct shared definitions. It checks reciprocal time-remapping
attributes, the frame list and cache, and all incoming references before removing
the owned data. Form, drawable, deformer and Part identities remain. Inline Part
records and inline or shared parameter-group GUIDs are supported. Empty inline
keyforms are also supported when their remapping attributes have distinct private
dummy owners, empty sequences and null root links. A dummy is removed only when
its handlers and those attributes are its complete set of references. Sparse
attribute maps must point only to verified shared-root attributes with matching IDs.
Parameter-group frame-step attributes owned by another Model track remain unchanged;
their references must not point into deleted storage. Same-owner attributes use the
ordinary ownership checks. Global form maps and nonempty inline keyforms remain
unsupported. This enables deletion, not form creation, editing, duplication or playback.

`live2d.animation.scene.delete` accepts scenes containing supported Model, Image,
Sound and Guide tracks. It applies the same ownership checks as track deletion and
removes the scene, its owned groups and track contents together. Linked resources
and shared identities remain. Shared scene GUIDs and their owned playlist items
are removed; stored scene pointers follow the existing replacement rules. Pending
child edits and copies are discarded with their deleted scene. Unsupported
members are refused. Supported FormEdit data uses the same
ownership checks; deleting the last scene is refused when a stored scene pointer
needs a surviving target.

Image and Guide `visualCurves` list supported scalar channels in the order `scalex`, `scaley`,
`rotate`, `shear`, `opacity`, `artPathWidth`, each with `id`, `points` and `curveTypes`.
Empty sequences remain visible. Unsupported or malformed channels are omitted;
omission does not establish that a native channel is absent. Resource-budget
exhaustion fails the read rather than returning a partial result.
`live2d.animation.key.set` accepts these raw IDs to change an existing anchor value.
The target must have owned inline storage and consistent list, map and named-field
references. Image/Guide channels reject `partOpacityMapKey`. Existing Image/Guide channels also accept
`live2d.animation.key.add` and `live2d.animation.key.delete`. A new key uses controls
at the anchor, with `corner=true`, `positionOptimized=false`, and the requested type
(default `LINEAR`). Retained keys keep their controls and interpolation types.
Deleting the last key retains the immutable template's extrema and base value and
invalidates stored evaluation caches. Adding then deleting a key before saving can
restore the original bytes; saving and reopening between these edits establishes a
new template, so inverse edits need not restore the earlier extrema.
Image handle and existing interpolation-type edits, and integer-channel edits, remain
unsupported. Unrelated native data is preserved. No Image playback equivalence is claimed.

Image and Guide `pointChannels` list readable `xy` and `anchor` channels, each with `id` and
`points: [{frame, x, y, corner}]`. Unsupported or malformed siblings are omitted;
resource-budget exhaustion fails the read. `live2d.animation.point.set` changes
both coordinates of an existing point, selected by scene/track GUID, `curveId`
and `anchorFrame`, with `expectedRevision`. Coordinates round to finite f32;
equivalent f32 values preserve the original token and do not add history.
Both stored coordinate copies change together. Frames, corner flags, base points,
point order/count, other channels and unrelated data stay unchanged. Shared or
inconsistent storage is refused. Point interpolation remains unsupported.
`live2d.animation.point.delete` removes one existing point using the same scene, track,
channel, frame and revision selectors, without coordinate arguments. It preserves the
channel even when empty, the base point, retained point order and unrelated bytes.
Deletion requires a verified inline XY sequence with points, base point and attribute
backlink only, an explicit matching array count and point type. Unknown structural
fields, shared point storage and ambiguous ownership are refused. Coordinate edits
and deletions use the same revision, Undo/Redo and native save path.
`live2d.animation.point.add` requires explicit `anchorFrame` (signed i32), finite
f32-compatible `x`/`y`, boolean `corner` and the same selectors/revision. Coordinates
round to f32. The frame must be unused; insertion requires increasing frames in the
stored and requested sequence and the verified private point-record shape, without
unknown fields or shared storage. The new point is inserted in numeric frame order;
base point and retained records stay unchanged. Empty channels are supported.
Delete/add compositions are reconciled by frame identity even when counts match.
Recreating an original frame may change its corner using a canonical point and the
same strict structure/order guards; otherwise retained fields remain protected.
If recreation keeps the original corner, endpoint reconciliation treats it as a
coordinate edit (or no-op), preserving the original record and its unknown metadata.
Stored-field save/reopen does not establish interpolation or Cubism playback parity.
Point attributes may use a stored empty attribute map when the member, named-field
and sequence references still establish exclusive ownership. A populated Image map must
contain the matching entry; Image scalar channels still require that entry. Guide point edits require
the complete supported Guide ownership layout, including both effects and distinct image-number attributes.

`imageResources` separately lists linked image GUIDs in their stored order. `name`
and `sourceFile` appear only when stored; missing and empty values remain distinct.
Paths are returned literally and are not opened. These resource records are read-only.

`live2d.animation.track.duplicate {sceneGuid, trackGuid, name, expectedRevision}`
copies an existing Image, supported Guide or supported Sound track into the same scene and parent group. It returns
the new `trackGuid` and snapshot. Image donors require an owned VisualDefault effect;
Guide donors require the fifteen-definition layout described above. Sound donors require the four-definition
SoundDefault layout with one fixed-volume attribute. Their stored speed, default volume and volume caches
are preserved, shared identities remain, and audio paths are never opened. Other layouts,
unknown references and shared mutable storage are refused. The copy
has fresh private identities and serial numbers, while keeping its linked resource,
timing, full keys and cache bytes. Duplication, including a copy of an unsaved copy,
is one undo step. Copies survive deletion of their donor and save/reopen.

CAN3 recovery through `open {path, as}` accepts a saved baseline only if the opened
document can reproduce its bytes with the baseline's projected model. Unprojected
Image keys or other native content that differ keep the recovered document modified
until it is saved. A different compression or layout can conservatively do the same.
These checks do not establish Cubism Editor or playback equivalence.

Anchor frames/values, control fractional and integer frame positions/values,
corner/position flags, and curve-type strings remain in stored source units.
Negative frames and keys beyond the scene duration are retained. The reader checks
declared list counts, GUID/attribute/frame uniqueness, resolved owner backlinks,
point types, and finite exposed numbers. Unknown curve-type strings are retained
without claiming that they can be evaluated.

Run `python3 tools/verify-can3-source.py` after building the Rust core. It compares
all 8,514 parameter keys in the four supplied projects against an independent XML
reader through FFI, JSONL, and stdio MCP, and checks eight malformed-source controls
through the same routes. With `VSOUL_LIVE2D_INPUTS` set to the extracted samples,
the `api.rust-live2d` regression also reads all four projects through embedded HTTP
MCP while a CMO3 is active and verifies unchanged session state and source bytes.

A parameter curve shows `baseValue` where it stores one. In the supplied files a stored `baseValue`
equals the parameter's default in 3,586 of 3,586 curves. `live2d_animation_curve_add` stores the
parameter's default as `baseValue` on a new parameter curve; a re-added original keeps the value it
stored, and an edit of a stored one is refused. A donor curve that stores no `baseValue` is not copied:
a curve added to a stored track gets the minimal template, and inside an added track or scene the save
is refused. Part curves show none.

This response omits non-parameter channels, other non-model tracks, sequence caches
other than a stored `baseValue`, and auxiliary attributes. It does not load linked models, evaluate/play curves,
edit keys, convert animation to INX, or establish Cubism Editor compatibility.
Those remain part of the native animation phase.

### Active CAN3 documents

`open` recognizes CAN3 by its resolved main `CAnimation`, separately from CMO3
models and INX. The same Rust Session owns its document, file baseline, saved
state and history. Snapshot `documentFormat` is `live2d.can3`; runtime `values`
are empty until a CMO3 is explicitly attached, and Motion3 preview is null.
Opening a project clears the previous
document's history and preview state; failed opens preserve them.

An unchanged CAN3 can be saved to a `.can3` destination and reopened with every
original container byte retained, including omitted XML fields, non-parameter
channels, caches, opaque entries and trailing bytes. Existing atomic-save and
file-conflict checks apply. `live2d.animation.key.set` edits one existing raw
parameter anchor value by `sceneGuid`, `trackGuid`, full `curveId`, signed
`anchorFrame`, finite `value`, and current `expectedRevision`. It shares the
Session's no-op, conflict, undo/redo and save behavior. Negative frames and values
outside declared channel ranges remain valid source data; this operation does
not clamp them or move control handles. This is not curve regeneration or a
verified Cubism editing gesture.

The writer verifies anchor extrema against existing `curMin`/`curMax`, and patches
changed anchor values and extrema tokens. Value edits preserve frames and controls.
Stored evaluation caches are invalidated (`keyPts2=null`, `lastValue=NaN`,
`lastPos=-1`), while `keyMin`/`keyMax` follow the remaining key-frame bounds.
Other XML bytes remain unchanged. Inline native double scalar storage is
required; shared or aliased target storage and mismatched extrema caches are
rejected before history changes. The rewritten CAFF must read back as exactly
the requested projection before it can enter the Session.

The GUI has a dedicated source inspector for resources, scenes, model tracks,
parameter channels, anchors and controls. It does not enter the INX node,
binding, animation or physics editors, the CMO3 form editor, or the Motion3 panel.
Separate Image rows offer revision-bound deletion confirmation and identify the
linked image resource that will remain. Model-track creation and curve controls
continue to use only model tracks.
Each anchor's Edit value button loads a double-precision draft. Apply uses its
captured revision; a concurrent document change requires discarding and selecting
the key again. Unapplied drafts block save/open/new/close. The right pane explicitly
reports pending CAN3 playback/preview. `.can3` recovery names are recognized;
automatic recovery after an animation edit still requires live GUI verification.

Embedded MCP advertises `resource://soul2d/document/animation-source` for
the active CAN3 projection. Read it after `FileCommand_OpenFile`, and read
`resource://soul2d/document/state` for format/revision/save status. INX
numeric node discovery is not advertised for a CAN3. The path-based animation
read tool remains available for inspecting another file without replacing the
active document. Embedded MCP exposes `Live2DCommand_AnimationKeySet`; Rust stdio
MCP exposes `live2d_animation_key_set` with the same fields. `live2d_animation_scene_set` and
`live2d_animation_scene_add` rename, retime and add scenes (see `doc/rust-migration.md`, ANI05).
`live2d_animation_resource_add` links a CMO3 as a new linked-model resource (ANI01, #210); save copies the
file's own resource entry with the file name and absolute path, so a CAN3 without one refuses it, and
`live2d_animation_track_add` then places the model in a scene.
`live2d_animation_image_resource_add {imagePath, expectedRevision}` adds a linked Image resource
using an existing owned Image resource as its donor. It validates a bounded static PNG or JPEG,
stores its canonical path and basename, and returns `resourceGuid`. It neither embeds the image
nor creates a track. Ambiguous memberships, unknown donor references and non-null custom names
are refused. Model and Image additions can coexist before saving. Each addition is one undo step;
save/reopen retains the resource. Cubism Editor acceptance remains unverified.
`live2d_animation_image_resource_relink {resourceGuid, imagePath, expectedRevision}` changes only
an existing Image resource's stored path to a validated static PNG or JPEG. All tracks sharing
the resource use the new link. Its name, custom name, keys and transforms stay unchanged, including
when the file name, format or dimensions differ. This does not resize or recenter anything.
A missing or shared path is refused. A matching canonical path is a no-op; otherwise this is one
undo step. Relinking also works before a newly added resource is first saved.
`live2d_animation_scene_set` also sets a scene's stored loop flag (`isLoopMotion`). The flag is
shown as `scene.loop` only where the scene stores it, and is refused elsewhere. It does not change
how Soul2D plays the scene.
It also sets a scene's stored tag text, shown as `scene.tag` only where the scene stores one (all 49
scenes of the four supplied projects do: 15 with text, 34 empty) and refused elsewhere; an empty `tag`
clears it. `live2d_animation_scene_add` shows a copy of the document's last scene's tag on the new
scene, since save copies a stored scene, tag included; no measured rule resets it.
`live2d_animation_handle_set` and `live2d_animation_curve_type_set` move one control handle and set
one segment's curve type (ANI08). Guide visualCurves also accept these operations for the six
supported scalar IDs, without a Part selector. Complete Guide ownership is required. Only the
requested control frame/value leaves or stored type change, with evaluation-cache invalidation;
anchor values/extrema, integer control positions, optimization/corner flags and other channels stay.
The existing segment check runs when the prior affected segment was valid. Endpoint controls and
the final stored type are editable without an interval. Same-frame canonical recreation and new
keys can receive later handle/type edits before saving. Image and Sound controls remain unsupported.
The four retained vendor inputs have one scalar key per channel; authored multi-key fixtures and
key-add composition provide separate evidence. This does not establish vendor multi-key interpolation
or Guide playback equivalence.

The five key operations (`key_set`, `key_add`, `key_delete`, `handle_set`, `curve_type_set`)
also edit existing modern `partOpacityCurves`, legacy-colon `legacyPartOpacityCurves` and
`underscorePartOpacityCurves` through the same revision, history and native-save path.
Without a selector, `curveId` must identify exactly one curve across all four arrays.
For underscore-map channels, pass the stored `mapKey` as optional `partOpacityMapKey` to select the exact
map entry and attribute ID together. Duplicate attribute IDs and GUIDs under different map keys remain distinct.
An empty, malformed, wrong or mismatched selector is refused without fallback.
Key edits preserve PartOpacity membership, order, Part ID/GUID, source identity, range and other metadata.
Modern writes require the attribute's map value, attribute-list member and sequence backlink to be its only
references. Legacy writes allow the map value and sequence backlink alone, or those plus one bare member.
An absent legacy member list is kept absent. Legacy map suffix, attribute ID, GUID and typed/text source identity
remain independent and unchanged; no CMO3 binding or identity translation is required to edit stored keys.
Underscore writes use the same two or three owned references and preserve absent membership.
Their unique `KEY_PARTS_VISIBLE_ID` may coexist with opaque option fields, which remain unchanged.
The effect may also have one bare registration in the same track's inline `effectManager.effectList`.
That registry is preserved. Shared managers/lists, duplicate registrations, decorated references and references
outside those owning fields are refused.
Inline inherited identity and cache fields are supported; additional owners, aliases and shared mutable storage
are refused. The editor's source tree offers the same anchor-value draft for all three PartOpacity arrays,
with the exact map key retained in underscore drafts and displayed beside the selected attribute ID.
`live2d_animation_curve_add` accepts exactly one of `parameterId` and `partId`, plus `modelPath` naming
the CMO3 source. A new Part curve uses its Part GUID, typed `CPartId` identity and range `[0,1]`.
Re-adding an original Part curve preserves its stored identity representation, range and opaque fields;
the CMO3 Part GUID must still match. An absent or inline null parts-visible effect is created as owned storage.
A new parameter curve stores the parameter's default as `baseValue`; a re-added original keeps what it stored.
If an owned existing effect has no member list, modern addition creates one beside its validated track
backlink, under the effect or its inline `ICMvEffect`. Only the new modern members are enrolled; existing
legacy and underscore membership stays absent. Existing map entries, definitions and effect registration
remain unchanged. Shared or unknown insertion owners and mismatched backlinks are refused.
An empty unmapped Part attribute is reused when its ID, GUID,
source identity and range match the requested curve. It must belong exclusively to the track, with at most one bare member;
the existing member keeps its position. Only missing map/member references are added. Opaque fields
and empty-sequence caches are retained, and subsequent unsaved key edits use that same attribute.
Conflicting identities, multiple candidates, existing keys and outside ownership are refused.
When the effect is absent or null, a detached attribute must have only its own sequence backlink.
The new effect lists reused and newly allocated attributes together in the requested curve order.
`live2d_animation_curve_delete` removes parameter or any of the three Part curve families through the same
history path. Underscore deletion accepts optional `partOpacityMapKey` with the same exact selector rules as
key editing. It removes the selected map entry, optional mapped member and owned attribute, preserving
siblings, the effect, its registry and unrelated native data. Remaining order is unchanged; mapped creation
and reordering are unsupported. Shared, aliased, nested shared and externally referenced removed storage is
refused. Unsaved deletions can accumulate with modern curve and track edits; map/list counts and the
historical legacy counter are checked against the resulting channels. Underscore playback binds only by an
exact Part GUID in the explicitly attached CMO3, with each curve identified by scene GUID, track GUID and
raw map key. Multiple populated channels for one Part are refused; no winning channel is inferred.
New tracks and scenes may contain these curves; deleting a saved track or scene also removes its owned
parameter and modern, legacy-colon and underscore-map Part definitions. Deletion resolves the exact stored
map identities, including duplicate underscore attribute IDs under distinct map keys; it does not require a
CMO3 attachment. Modern members must exist; each legacy or underscore member may be absent or one bare
reference. Inline unshared inheritance is supported. Unknown map/list members, shared definitions or
sequences, nested shared IDs, shared inheritance and outside references prevent the whole deletion.
Unrelated XML, linked-model resources and CAFF payloads remain unchanged. Legacy and underscore curve
creation remains unsupported.
These source edits do not establish Cubism Editor acceptance.

`tools/verify-can3-mapped-delete.py` exercises generated CAFF and pinned local Rice/Ren/Hiyori copies
through production HTTP MCP. It checks exact curve selection, cumulative deletion and modern additions,
history, refusal atomicity, save/reopen and XML byte spans outside removed nodes and count tokens.
It requires `--core`, `--njc`, `--rice-can3`, `--ren-can3` and `--hiyori-can3`; originals are read and hashed,
and edits use owned copies. Native GUI interaction and Cubism Editor acceptance remain separate.

`tools/verify-can3-native-curves.py --core <binary> --njc <client> --can3 <ren_t01.can3>
--cmo3 <ren_t01.cmo3>` verifies a pinned local Ren pair through production HTTP MCP. It restores an
unmapped PartNose attribute, checks cancellation and accumulated keys, combines a mapped-orphan
deletion, and checks history, save/reopen and unrelated XML/CAFF preservation. Existing attribute
storage and member order must be retained. This is source-edit evidence, not vendor acceptance.

`tools/verify-can3-source.py` additionally opens, saves a recovery copy, saves
another destination and reopens each supplied CAN3, requiring byte-exact outputs
and unchanged originals. It also edits keys in all four projects, including a
negative Ren frame, verifies no-op/stale/history behavior and save/reopen, and
uses an independent XML token oracle to require that only the anchor and extrema
tokens change. The real-corpus `api.rust-live2d` regression verifies
active-source resource readback and save/reopen through embedded HTTP MCP.

### CAN3-to-CMO3 parameter bindings

`live2d.animation.bindings` is a read-only Session query; stdio MCP exposes
`live2d_animation_bindings` and the editor's embedded MCP and command browser
expose `Live2DCommand_AnimationBindings`. It requires a nonempty `modelPath` to
an explicit CMO3 file and a nonempty `resourceGuid` selecting one linked-model
resource. An optional `path` reads an explicit CAN3 file; omitting it reads the
active CAN3 document's canonical projection, so a path-less query fails while
INX or CMO3 is active. An explicitly present empty, null, or non-string `path`
also fails. Stored `sourceFile` paths, basenames, and names are never searched
or used to guess a model; there is no persistent linked-model cache or second
Session. The same Session's document, revision, history, runtime values, file
bytes, and pending work are unchanged on success and rejection.

Binding succeeds only when every parameter channel on the selected resource's
tracks matches by exact `parameterGuid` and by the full `live2dParam_`
attribute-ID suffix equalling the CMO3 parameter ID; result `bindingBasis` is
`parameterGuid+attributeId`. Wrong or missing GUIDs, same-GUID/different-ID,
ambiguous model identities, unknown resources, unsupported identity kinds, and
non-CMO3 models reject the whole query with no partial fallback. Tracks for
other resources are skipped even if their channels would fail. `sourceParameter`
is auxiliary metadata, not an alternative target: each binding reports it
verbatim with `sourceParameterStatus`. Its values are `matchesId`, `differentId`,
or `legacyText`, because real CAN3 `optionParam` typed IDs can disagree with the
resolved CMO3 ID on keyed channels. A resource with no tracks returns empty
`bindings`; that is inspection output, not a compatibility pass. Only decoded
channels on the selected resource undergo identity matching; the existing CAN3
reader still validates the entire input file.

This is metadata identity matching only. It does not verify the stored linked
file's provenance, display the model, play animation, map renamed parameters,
or establish Cubism Editor equivalence. The GUI command browser exposes the
same query; no playback panel exists yet.

Run `python3 tools/verify-can3-bindings.py` after building the Rust core. Its
independent XML oracle binds all four original CAN3/CMO3 pairs through FFI,
JSONL, and stdio MCP, checking read-only state preservation plus dirty-edit
undo/redo history through FFI. Accepted evidence is
`build/verification/can3-bindings-ctfjgym0/result.json`. This verifies metadata
identity agreement only, not view, playback, or vendor completeness.

## Native preview and playback

The Rust core now provides a session-only link between the active native
document and its companion file. These operations are also advertised over
stdio MCP with dots replaced by underscores. See
[Rust editor controls](rust-migration.md#native-preview-controls) for the new
Rust GUI; the historical D GUI evidence below is unchanged.

| Operation | Request | Behavior |
| --- | --- | --- |
| `live2d.animation.attach` | `path`, optional `resourceGuid` | Attach CAN3 to active CMO3 |
| `live2d.model.attach` | `path`, optional `resourceGuid` | Attach a CMO3, or a runtime `.model3.json` read by the clean-room reader of `doc/moc3-layout.md`, to the active CAN3 |
| `live2d.animation.list` | — | Read linked resource and scene metadata |
| `live2d.animation.seek` | `sceneGuid`, `frame` | Sample an absolute scene frame |
| `live2d.animation.advance` | finite nonnegative `dt` in seconds, `loop` | Advance at the selected scene's FPS |
| `live2d.animation.reset` | — | Clear the timeline preview |
| `live2d.expression.import` | `name`, `path` | Import a bounded `.exp3.json` with known parameter IDs |
| `live2d.expression.apply` | `name`, optional `fades` | Apply one expression layer to current base values; with `fades: true` it fades in over its `FadeInTime` as motion, timeline or physics playback advances |
| `live2d.expression.reset` | — | Remove the expression layer |
| `live2d.expression.list` | — | Read the session expression library (`fadeIn` of each) and the active expression's `weight` |
| `live2d.parameter.set` | `parameterId`, `value` | Set a runtime native parameter |

Multiple linked model resources require an explicit `resourceGuid`. No stored
absolute source path is followed automatically. CAN3/CMO3 parameter GUIDs and IDs
must match exactly. Scene selection uses GUIDs, not potentially duplicate names.
`nativePlayback` in `snapshot` exposes the shared attachment, scene clock,
parameters, expressions and the active expression's `expressionWeight`; `values` exposes the effective runtime
pose.
Attachments and runtime values do not add history entries or rewrite source
keyforms. Native source save/edit restrictions are unchanged.

For CAN3 playback, `active.clockFrame` preserves the fractional timeline clock
through `[0, durationFrames]`, while `active.frame` is the authored frame
sampled for pose evaluation and clamps to the final authored frame. An
expression record with no `Blend` key defaults to `Add`; explicit `Add`,
`Multiply`, and `Overwrite` values remain supported.

CAN3 `partsVisibleEffect` channels are read separately as `partOpacityCurves`,
bound by both Part ID and GUID, and sampled with the same track timing and curve
rules. The active preview exposes `poseSignals` by Part ID and `partOpacities`
by GUID. Without Pose, sampled opacity replaces that Part's authored opacity;
with Pose, only grouped and linked Parts receive the Pose result instead.
These values never enter the authored morph-parameter map or source history.
This native authoring preview contract follows the separate CAN3 channel and
[Editor pose-switching workflow](https://docs.live2d.com/en/cubism-editor-manual/change-pose/).
The exact fallback/override order is an integration inference, not a published
CAN3 specification or verified Cubism equivalence. Exported Motion3 retains its
separate virtual-parameter/Pose routing.

Legacy colon-style channels are exposed separately as `legacyPartOpacityCurves`.
The map token, attribute suffix and raw source identity remain distinct fields;
none is treated as the current model's Part ID. A unique exact Part GUID match
in the explicitly selected CMO3 supplies the runtime Part ID. The binding report
exposes `legacyPartBindings` and `unboundLegacyPartCurves`; unmatched GUIDs are
preserved and warned about, without name aliases or playback writes. Bound
legacy channels use the same opacity sampler and duplicate-writer guard as
modern channels. This GUID identity rule is an implementation contract supported
by the stored identity type, not verified historical Cubism migration behavior.
The historical `skippedLegacyPartOpacityCount` field counts all legacy channels
for compatibility; it does not indicate the runtime binding result. The supplied
Hiyori archive has no matching legacy GUIDs in its current models, so those
channels remain unapplied.

Underscore-map channels use the same exact Part GUID rule. `underscorePartBindings` and
`unboundUnderscorePartCurves` also retain each `mapKey`, attribute `curveId` and source identity, even when
attribute IDs or Part GUIDs repeat. A bound channel supplies the CMO3's canonical Part ID for pose signals
and its GUID for rendered opacity; map tokens and display text are not aliases for that target. Unmatched
GUIDs are preserved and warned about without writing a signal or opacity. The historical legacy counter
still includes these retained channels; use the binding arrays to determine runtime support.

`tools/verify-can3-underscore-playback.py --core <core-binary> --njc <njc-binary>` exercises generated
CAFF CAN3/CMO3 through the production HTTP MCP host: exact identities, both empty/populated orders,
collision refusal, opacity renders and native save/reopen preservation. It is a generated runtime check;
native UI interaction and Cubism vendor acceptance remain separate.

The sampler uses `localFrame = sceneFrame - start + internalOffset` on each
track's half-open active interval. It supports linear, step and monotonic
cubic-time smooth interpolation. Unknown curve types and overlapping active
channels for one parameter or Part reject rather than silently choosing a winner. A PartOpacity channel
with empty points and curve-type arrays produces no value and is not a writer, across modern, legacy and
underscore forms. A malformed bound sequence is an error. Two populated PartOpacity channels targeting
the same Part conflict within or across active tracks. Sampling builds local outputs before committing a
preview, so a refusal leaves document, history and prior runtime state unchanged.
Expression blending supports Add, Multiply and Overwrite with parameter limits;
it is immediate, not a Cubism expression-fade implementation.

`bounds`, `render` and `render.packet` evaluate CMO3 ArtMeshes and nested warp /
rotation transforms through the Rust renderer without an editable INX copy.
ArtMeshes inherit evaluated Warp/Rotation opacity along their deformer chain,
separately from Part opacity and offscreen group opacity. Nested deformer opacity
uses multiplication in this preview; the public manuals establish inheritance,
but this composition has not been compared with Cubism Core.
Equal draw-order values use the ordered Part child hierarchy: higher Parts
palette entries appear in front, as specified by the
[Parts palette manual](https://docs.live2d.com/en/cubism-editor-manual/partspalatte/)
and [draw-order manual](https://docs.live2d.com/en/cubism-editor-manual/draworder/).
Numeric draw order retains priority within each ordering scope. Missing palette metadata uses the source
list fallback with a warning; unparsed palette effects remain unsupported.
Warp deformers, rotation deformers and Glue follow the rules that Cubism Core
vertex output shows. They are not taken from a specification:

- A warp grid is first mapped to the canvas through its parent deformers. A
  point is then interpolated in that mapped grid. Mapping the interpolated point
  through the parent instead gives another result under a bent parent.
- A grid cell is two triangles, cut from its second corner to its third. A
  quad-transform deformer is bilinear in the cell.
- Outside the grid the four grid corners define one affine map. It is used as it
  is beyond -2..3. Between the grid and that range lies a ring of border quads,
  each cut into two triangles.
- A rotation deformer under a warp turns by the angle the warp turns the
  direction from 0.1 above its origin to its origin. The warp does not scale it;
  the rotation deformers above the warp do, by the product of their scales, as
  they scale the warp's own grid (a third-party model holds 137 drawables under
  such chains, scaled 0.6707 and 1.0413, at up to 136 px before).
- A repeating parameter at its maximum is at its minimum, one period on: Core
  shows the keyform at 0 for a 0..30 parameter set to 30 (the third-party
  model's two repeating parameters, keyed at 0, 4, 25.3 and 30; 299 px before).
- After deformation each Glue moves its bound pairs: vertex A goes
  `weightA * intensity` of the way to vertex B, and B goes `weightB * intensity`
  of the way to A. Glues run from the last source to the first.

`tools/capture-cubism-core.py` records Core vertex positions for the default
pose, for each parameter at its minimum and maximum, and for twelve poses with
every parameter at a pseudo-random point of its range.
`tools/compare-cubism-core.py <run directory>` compares `scene.geometry` with
that record, within 0.02 px by default. On 2026-10-13, with a locally supplied
Cubism Core 6.0.1, every drawable of the four sample models passed: 81,255
drawable poses in 495 poses, and no pose was refused. The worst error is
0.012 px. At a tolerance of 0.01 px, 14 drawable poses fail, between 0.010 and
0.012 px: 6 drawables of one model, under four nested rotation deformers inside
warps, in one single-parameter pose, and 4 of them in each of two combined
poses. Its cause was not found. Rounding positions and angles to single precision in a
local re-implementation of these rules did not reproduce it.
This compares with Core output only. It is not Cubism Editor acceptance.
`tools/compare-cubism-core.py <run directory> --runtime` attaches each sample's
`.model3.json` to its CAN3 instead, so the clean-room `.moc3` reader of
`doc/moc3-layout.md` is measured against the same capture. On 2026-10-20, with
the same Core 6.0.1, every drawable of the four runtime models passed at 0.02 px:
the same 81,255 drawable poses, worst error 0.012 px.
The capture also records Core's render orders, and the comparison checks the
preview's painter order against them. Core reports the whole part of an
interpolated draw order (hiyori_pro's 636.51 as 636), and sorting by that whole
order with palette-order ties reproduces every captured render order; whether
Core sorts by the fraction first is untested, since no sample pose ties a
fractional order against a whole one. The preview truncates before ordering,
since interpolation noise on equal keyform orders (500.00000000000006 against
500) had reordered up to 148 drawables of a combined pose. On 2026-10-20 every
pose of the four models was painted in Core's order on both paths.

Core keeps state between updates: a drawable whose parameter leaves its keyed
span is not re-evaluated and keeps the geometry and draw order of the preceding
update (Rice's ArtMesh120 has keys at -10 and 10 on a parameter that reaches
-20). The capture therefore settles every pose from an updated default pose, so
such a drawable shows its default-cell form there, and the preview holds the end
keyform, which coincides only when the default is that key.
`tools/compare-cubism-core.py <run directory> --runtime --host-can3` synthesizes
a CAN3 without curves so that a runtime model with no CAN3 of its own can be
attached; `tools/capture-cubism-core.py --inputs <directory>` captures every
`.model3.json` below a directory. On 2026-10-20 the eight runtime-only sample
models of the Cubism SDK for Native 5-r.5 (moc3 versions 1, 3, 5 and 6; 30 to
262 drawables) were compared this way: 175,348 drawable poses in 1,037 poses.
Five models passed at 0.02 px. Mao has 44 drawable poses (14 drawables)
between 0.02 and 0.082 px, Natori 16 between 0.02 and 0.03 px, Rice 4 between
0.02 and 0.029 px plus its ArtMesh120 at the stale poses above (one key pose,
two combined poses and their painter order). Those residuals are not explained. The comparison
found and fixed six reader defects: the draw-sequence unit count (counts entry
19), the quad-transform flag in version 3 files, Part blend shapes (counts entry
32, slots 143 to 145), the rotation blend-shape owner (slot 146), colour
keyforms outside the drawables' grids, and parameter ranges widened from single
precision (1.3 read as 1.2999999523). The preview now blends a Part's draw order
and a rotation deformer's scale through morph targets.

A third-party runtime model (moc3 version 5; 617 drawables, 279 parameters, 506
deformers, 142 Glues; local file, counts only) was compared the same way on
2026-10-20: 333,797 drawable poses in 541 poses. Its textures are 8192 by 8192,
which the 16-megapixel cap refuses by decision (#151), so the comparison ran on
a scratch copy with the textures halved; geometry does not read texture pixels.
After the two fixes below every drawable pose is within 0.066 px: 18,593 drawable
poses lie between 0.02 and 0.066 px, in every pose, from about 35 drawables under
one chain of two rotation deformers below two warps (the model's
`TemporaryDeformer` pair).

That model also measures the preview's speed at Pro size. A release build on
this machine (Apple Silicon, 2026-10-20) attaches it in 0.33 s and answers
`scene.geometry` in 0.18 s (Mao 0.021 s, Hiyori 0.010 s), the 3.4 MB JSON
answer over stdio included; before the deformer frames were cached and the
packet arrays moved instead of re-serialized it took 0.40 s (Mao 0.042 s).
What remains is the `serde_json` value pipeline: with `arbitrary_precision`
every number is a string, so each array that passes through a value is
formatted and parsed again (model nodes, packet, answer). A typed geometry
path from the native scene to the packet would remove that; it changes the
render packet contract and is not done here.

The residuals of Mao, Natori and that model were fitted drawable by drawable as
a rotation about the centroid plus a translation. Each fits to 0.0001 to
0.0003 px: the drawables under one chain share the same tiny rotation (Natori's
ArtMesh195, 197 and 198 at ParamAllRotate -10: -0.00135 degrees with 0.02 px of
translation; Mao's ArtMesh174 to 176 at ParamBodyAngleZ -10: -0.0006 to
-0.0007 degrees; the third-party model's chain at rest: +0.00066 degrees). The
offset does not grow with the parameter (Natori: -0.00135 degrees at -10,
-0.00035 at -5, -0.00038 at -2.5, +0.00017 at +10), so it is not a constant
in the angle conversion, and it is 400 times what single-precision rounding of
the angle or of the positions could cause. Probing the warp 0.01, 0.05, 0.2 or
0.5 above the origin, probing symmetrically or below, and mapping the origin by
triangles instead of bilinearly all leave the residuals unchanged (the last
makes them worse). What remains is Core's own arithmetic in its rotation
chains, which the preview, computing in double precision, does not reproduce;
reproducing it would mean copying Core, which the clean-room rule excludes.
The residuals stay below 0.09 px.

What that comparison does not cover:

- No compared model reflects a rotation deformer. The preview warns when it
  meets one. Scaled rotation deformers match: 23 in the SDK models and the
  third-party model's chains above.
- No quad-transform warp had a point outside its grid.
- Every compared warp carries Bezier settings. Core output matches without
  reading them, so the preview does not read them either. What the Cubism Editor
  shows while such a warp is edited is not compared.
- Glue intensity is applied as it is. The compared Glues have intensity 1, and
  one has 0.5. An intensity above 1 is untested.
- Only one model binds a vertex to two Glues. It matches in last-to-first order
  and not in source order; any other ordering rule is untested. Its `.moc3`
  lists those Glues in the opposite order and Core applies them in file order,
  which the runtime reader reproduces by listing them in reverse.
- A Glue with a target that is not drawn, or without an intensity, is left out
  with a warning. A pair that binds a deleted vertex is left out with a warning.
- The combined poses are twelve pseudo-random points per model, not a sweep.

Part colours are inherited: multiply colours multiply, and screen colours combine as
`s + p - s * p` per RGB channel (inferred, not measured against Cubism Core).
Unknown mesh composition rejects.
Physics3 and Pose3 sidecars support session-only preview as described below.
Generated-cache regeneration and vendor pixel equivalence are not implemented or
certified by this preview. Source-reader failures remain failures; support for
the full official sample corpus is not claimed.

An ArtPath is read from `drawableSourceSet` as a source of type `ArtPath`:
whether it is closed, its division count, line algorithm, line alignment and
brush GUID, and for each keyform its control points with position, start and
end velocity, width, opacity, colour and corner flag. That is all. Its keyforms
are not interpolated: the weight and form queries return neither weights nor an
evaluated form for it, so nothing in its keying can refuse a preview. A render
does not draw it and reports "native ArtPath is read but not drawn" when a
visible one exists. The brush list of the file is read as the model key
`brushes`: each brush's GUID, ID and name, empty when the file stores none,
and the drawing settings the file stores for it: the drawing curve type, the
width coefficient and the numbers of `textureUvData`, `fixWidthData` and
`spriteData`. `textureInput` gives the referenced texture's existing metadata,
stored texture state and colour composition, and the current input's model image
GUID and `optionalTransformOnCanvas`. The transform contains the stored `m00`,
`m01`, `m02`, `m10`, `m11` and `m12` attributes. Missing attributes are omitted;
a stored null transform remains null. `live2d_brush_set` accepts those finite
coefficients under `textureInput.currentTextureInputData.optionalTransformOnCanvas`
and refuses absent coefficients and writes to texture metadata. It does not replace
images, select another input or infer the transform's visual meaning. `live2d_brush_set` edits
those settings; it accepts only the curve types `SPRITE` and `TIP_USE`, the two
the supplied files store, and refuses a setting the file does not store for the
brush. `live2d_rename` and `live2d_set_id` change a brush's name and ID. Each
edit rewrites its one stored token; an ArtPath names
its brush by GUID, which neither edit changes. On 2026-10-22 all 42 brushes of
the 21 distinct documents that store any, among the local samples and the
official sample archives, were renamed and given a new ID on scratch copies:
every file saved, reopened equal to the edited model, was otherwise unchanged,
and read the same brushes through the independent XML oracle. The same 42
brushes then had their curve type switched and their width coefficient,
`repeatEnd`, `center`, `upFixedWidth` and sprite density changed through
`live2d_brush_set`, with the same four results, and the oracle reads the
settings of all 90 distinct documents as the Rust reader does. The binding of path points to an
ArtMesh is not read. `live2d_rename` changes its display name; on the samples
that was checked in the session only, without saving.

### Shared brush image replacement

`live2d_brush_replace_image {guid, path, expectedRevision}` replaces the existing
image entry selected by a brush with a static PNG of the same dimensions. Every
brush and mesh referencing that entry receives the new pixels. The response gives
`imageEntry`, `affectedBrushes` and `affectedTextures`; these counts cover the
projected brushes and mesh textures.

The operation checks chunk framing, every chunk CRC, a final IEND and bounded
pixel decoding. Animated PNG, missing entries, mismatched dimensions and inconsistent
shared resource dimensions are refused. It preserves the supplied PNG bytes
without premultiplication, and leaves XML metadata and unrelated payloads intact.
Resizing, choosing another input and allocating a new image entry are not supported.

Different bytes make one revision-guarded undo step. Identical bytes do nothing.
Image changes participate in dirty state, undo/redo, autosave and ordinary saving,
including when combined with unsaved geometry edits or brush copies. Native preview
caches are invalidated when image bytes change. These operations do not establish
Cubism Editor acceptance or ArtPath drawing support.

`live2d_artpath_point_set` changes the stored values of one control point;
`live2d_artpath_point_add` and `live2d_artpath_point_delete` add a point to, or
remove one from, every keyform of the ArtPath at one index. A new point is a
copy of the stored point it goes before, with the values the request gives, so
the file's own layout is kept; the last point cannot be deleted, and no
midpoint is computed. No ArtPath can be created. In the 28 ArtPaths of the
local samples every keyform stores the same number of points. Adding a point,
saving, rereading and deleting it again restored the forms of 22; the other 6
have extended interpolation forms, which every authoring edit of a source
refuses. A point's GUID object can also be named by a point-to-ArtMesh
binding: the new point gets a clone of it, and the binding keeps naming the
original.
A drawable that names an ArtPath as its clipping mask is refused by the preview;
no sample has one. How Cubism turns the control points into a stroke was not
measured: no available sample has an ArtPath together with an exported MOC3.
Before this, a CMO3 with an ArtPath did not open at all.

Glue is read from `affecterSourceSet` as a source with two ArtMesh GUIDs,
paired `bindVertexUids`, independent weights and raw form intensity. ArtMesh
`vertexUids` come from its editable mesh extension; Glue endpoints resolve by
UID, never by vertex array index. Referenced meshes must expose unique UIDs
matching their vertex count. Intensity uses the existing primary/morph scalar
interpolation. The public
[Glue manual](https://docs.live2d.com/en/cubism-editor-manual/glue/) does not
specify its serialized intensity formula or repeated-endpoint evaluation order.
`tools/verify-glue-source.py` checks the source projection, the refused vertex
edit and the unchanged save; it does not check Glue rendering.

Visible meshes can use `TEXTURE_ATLAS` or `MODEL_IMAGE` coordinates. Both resolve
their texture GUID and take their UVs as coordinates of the stored image; the
texture's image-to-logical transform only names the content box inside the padded
image and is not applied (ren_ja's and hiyori_pro's textures and runtime atlases
confirmed this). Unknown texture states are rejected. Face culling is not applied: a culled ArtMesh draws its back
faces too and the render reports a warning. A preview holds the end keyform when a
parameter value lies outside an axis's keyed span, and takes a value closer than
0.001 to a key as that key (`clampToKeys` on the morph query); the weight and
form queries still reject samples outside the span and do not snap. Cubism Core
6.0.1 snaps this way: 0.0009 from a key gives the keyform and 0.0011 does not,
with keys 1 apart and with keys 30 apart. The preview snaps keyform axes of
non-repeating parameters only: Core was not measured on a repeating parameter,
on a morph-target key or with keys closer than 0.002. Whether Cubism holds or extrapolates
outside the keyed span was not checked against the editor. A texture whose
`minFilter` names a mipmap mode (`LINEAR_MIPMAP_LINEAR` in every sample CMO3) is
minified through a box-filtered mip chain with linear mixing between levels; a
magnified one and every other texture use base-level bilinear sampling. No
picture of Cubism was compared, and clipping masks stay a 0/1 stencil (soft
masks are out of scope, #198). Normal, legacy Multiply and Screen
keep their existing renderer behavior. The Add and Multiply of before 5.3 (`ADD`,
`MULTIPLY`) draw the same under each of the five alpha compositions, which is what
the Cubism manual says of them; no picture of Cubism was compared. Advanced
ArtMesh color and alpha composition
uses a separate typed CPU path, including Ren's `MULTIPLY_R2`, `ADD_R2`,
`HSL_COLOR`, `HARDLIGHT`, `ATOP` and `OUT`. GPU render packets reject this advanced
path explicitly. Offscreen Parts now produce nested CPU Composite scopes, applying
group opacity, color/alpha composition and clipping after their children overlap.
Native material color tint applies Multiply before Screen, using the premultiplied
RGB equation from the
[pinned Native Framework shader](https://raw.githubusercontent.com/Live2D/CubismNativeFramework/45de21229a9faab9675525bc59deddbe089b0e95/src/Rendering/OpenGL/Shaders/Standard/FragShaderSrcPremultipliedAlpha.frag).
Opacity applies afterward to RGB and alpha. Native packet materials carry
`nativeTint: true`; their normalized texture slots carry `premultiplied: true`
so the OpenGL host preserves the same tint and alpha convention. Legacy INX
packets retain their existing color convention. These shader semantics do not
establish how multiple serialized ancestor colors aggregate.
Composite edge sampling differs between the CPU preview and OpenGL host;
the tint equation is verified separately from full-image sampling parity.
The preview uses viewport-sized RGBA8 layers and binary mask thresholds; Cubism
target precision and mask sampling have not been matched. Non-offscreen draw-order
groups use the evaluated Part order globally and child orders locally, as
described in the [group manual](https://docs.live2d.com/en/cubism-editor-manual/drawing-order-group/).
Plain groups do not add offscreen compositing. Nested ordering and ties across
group boundaries use recursive palette ordering inferred from these rules;
they have not been compared with Cubism Editor/Core. Inherited
part screen color is warned but not reproduced. Native manual deformation
overrides are rejected. These restrictions do not affect byte-preserving
source save or the native source-inspection operations.

The editor's Pose sliders select positive signals exclusively within their
Pose group, clearing peer signals in the same request. The lower-level signal
API retains first-positive selection when callers provide multiple positives.
Linked Parts inherit their owner's opacity and are excluded from writable
Pose selection signals. Attaching Pose while a CAN3 or Motion3 preview is active
replays its current timeline before publishing the new runtime; a failed replay
preserves the previous runtime and preview.
Pose selection never overwrites an authored Parameter with the same textual ID.
Parameter edits, expressions and Physics outputs likewise never become Pose
selection inputs. Pose consumes explicit Part signals and preserves its current
selection when those signals are absent.

`live2d.pose.attach` accepts an explicit `.pose3.json` path for the active or
attached CMO3; it does not follow manifest or linked paths automatically. The
bounded parser requires known Part IDs, supports groups, links and FadeInTime,
and rejects ambiguous source Part IDs, duplicate group members, links with multiple
owners and Parts used as both selection members and linked targets. Pose selection
signals and Part opacity are session state, separate from morph parameters and
source history. `live2d.pose.step` advances the fade and accepts overrides for
known signals; `live2d.pose.reset` restores first-member selection and opacity.
The Rust editor exposes attach/reset and selection controls in its Pose3 section.

Native Motion3 parameter curves follow `CubismModel::SetParameterValue`: a sampled
value outside the authored range clamps to it, or wraps when the parameter
repeats. `motion3.pose` reports the curve value as `raw` and the applied one as
`value`. Explicit `live2d.parameter.set` calls and render overrides reject
out-of-range input for a parameter that does not repeat, because that is caller
input rather than file data. For a repeating parameter they wrap it the same way,
and so do expressions and CAN3 playback.

A repeating parameter also turns in the keyform lookup. A value past its keys lies
between the last key and the first, one range further on, and the forms of those
two keys are blended linearly. With keys at 0 and 1 on a parameter from -2 to 2,
the value -1.5 gives half of each. The Cubism Editor manual says the rightmost
key is not needed for a repeat "from the beginning" and gives no formula for the
seam, and no Cubism Editor or Core output was compared, so every render of a
model with a repeating parameter carries the warning
`native repeat interpolation across the seam is approximate`. Physics inputs and
outputs still clamp.

Motion3 PartOpacity curves write Pose selection signals, including IDs absent
from authored parameters. Pose then updates Part opacity and links before
rendering; it replaces authored Part opacity rather than multiplying it.
Native Motion3 seeks reconstruct Pose from reset with 60 Hz replay, bounded to
one hour, while advance uses the requested frame delta. This deterministic seek
policy is a preview policy, not Cubism's state-preserving backward seek. Pose
survives source-form edits and undo through replay; open/new or attached-model
replacement clears it. Vendor comparison remains pending.

`live2d.physics.attach` accepts an explicit `.physics3.json` path for the active
or attached CMO3. The bounded Version 3 parser checks exact authored parameter
IDs, ranges, targets, group counts and strand indices before attaching anything.
Attachment during an active CAN3 or Motion3 preview replays its current clock
before publishing Physics; a failed replay preserves the previous runtime.
It accepts missing FPS (frame-delta stepping), zero FPS, or FPS from 1 through
240. It rejects unknown IDs, malformed weights, degenerate particle radii and
non-finite values. It supports up to 256 groups and 16384 entries per category.
Particle `Delay` has no upper bound, as in the Framework parser; official samples
ship values up to 1.5.

The Session keeps Physics3 particle state, input caches and output histories
separate from source/history/revision. `live2d.physics.step` evaluates a positive
frame delta; zero is observational. Negative or non-f32-representable deltas
are rejected without publishing runtime or timeline changes. Output overlays
affect native rendering. Each new frame starts from the
motion/slider/expression baseline, excluding the previous physics overlay;
groups still propagate outputs to subsequent groups within each substep.
Motion3 and CAN3 advance run physics before Pose. The Rust editor exposes
attachment, reset and a 1/60-second manual step in its Physics3 section.

`live2d.physics.options.set` replaces the complete runtime pair
`gravity: {x, y}, wind: {x, y}` after finite f32 validation. It preserves particle
history and does not modify the sidecar or document. Active options appear in
`nativePhysics.options`; Gravity X/Y and Wind X/Y controls expose them in the
GUI. Wind contributes to particle force. Gravity supplies the first-segment
angle reference; it is not the strand's acceleration vector.
`live2d.physics.stabilize` positions each strand along its current force, zeros
velocities, and fills output histories in group order without advancing time.
It uses the same external parameter baseline as stepping, then updates Pose.
Errors leave both runtimes unchanged. The GUI exposes a Stabilize button.

Motion3 absolute seeks reconstruct physics and Pose with the same bounded
60 Hz replay and expression composition. CAN3 absolute seeks use the same
reconstruction, converting the fractional clock frame with the scene's FPS;
the final frame's display interval still advances runtime time. Replay checks
and evaluation complete before publishing timeline or runtime state.
`live2d.physics.reset`
clears particles, remainder, caches and outputs and restores default force
options, a stronger reset than the pinned framework's particle-only reset.
Replay, motion reset and source edits rewind particle state while preserving
configured force options. Open/new or model replacement clears
the attachment; source edits/undo rewind it, and active Motion3 restores through
replay. Saves never write runtime physics into the CMO3.

Physics arithmetic follows the pinned
[Framework physics implementation](https://github.com/Live2D/CubismNativeFramework/blob/45de21229a9faab9675525bc59deddbe089b0e95/src/Physics/CubismPhysics.cpp).
This includes midpoint normalization, sequential X-then-Y rotation,
radius-based initialization, ordered group propagation and delayed output
interpolation. The pinned parser leaves X/Y output scales zero while assigning
JSON Scale to Angle outputs; this preview preserves that behavior. It uses the
framework constructor's gravity `(0,-1)` and wind `(0,0)`, which are separate
from the parsed Meta forces. Custom force options and stabilization follow the
same pinned source. Degenerate vector normalization produces an error with unchanged
state rather than non-finite particles. These are bounded preview semantics,
not a vendor-equivalence certification.

Advanced pixel arithmetic follows the published color and alpha formulas in
Cubism Native Framework commit `45de21229a9faab9675525bc59deddbe089b0e95`.
[Color blend specification](https://raw.githubusercontent.com/Live2D/CubismNativeFramework/45de21229a9faab9675525bc59deddbe089b0e95/src/Rendering/OpenGL/Shaders/Standard/FragShaderSrcColorBlend.frag)
and [alpha blend specification](https://raw.githubusercontent.com/Live2D/CubismNativeFramework/45de21229a9faab9675525bc59deddbe089b0e95/src/Rendering/OpenGL/Shaders/Standard/FragShaderSrcAlphaBlend.frag)
define straight-color blending followed by premultiplied overlap composition.
Literal pixel tests verify the arithmetic. These tests do not prove whole-model
Cubism equivalence, undocumented source-enum aliases or offscreen semantics.

## Reproduction

Use the existing verified copies of the supplied archives:

```bash
./tools/build-rust-core.sh
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust
cargo build --offline --manifest-path rust/Cargo.toml --target-dir out/rust --example caff-roundtrip
python3 tools/verify-caff-authoring.py build/verification/live2d-inputs-vjq5llby
```

The verifier compares Rust inspection through FFI, JSONL, and actual stdio MCP.
It runs the Rust codec on all eight source files, changes `ParamAngleX`'s default
in each CMO3 XML, and changes a comment in each CAN3 XML. An independent Python
decoder verifies the rewritten CRC, sizes, XML, assets, aliases, flags, and opaque
fields. Original files remain unchanged. The example writes only to a newly
created evidence directory; it is a verification helper, not a GUI editing path.

Accepted local evidence is retained at
`build/verification/caff-authoring-0s5s62yq/result.json`, with source/library hashes,
rewritten containers, original/edited XML, transport logs, and per-file inspection.
The full Rust suite passes 298 tests in `phase2-caff-rust-suite-2.log`.

The inspector now resolves `xs.id`/`xs.ref` and inherited `super` fields,
reads parameters in the model's `parameterSourceSet._sources` order, and reads
GUIDs from their `uuid` attribute. Shared definitions can reside outside the
model subtree. Duplicate IDs, dangling references, alias cycles, excessive
reference chains, and duplicate parameter GUIDs are rejected. Owner links may
form cycles; these are distinct from invalid reference aliases.
The updated eight-file corpus check passes at
`build/verification/caff-authoring-l13egfwb/result.json`; all 305 Rust tests pass.

Source-coordinate evaluation remains open. The samples contain `Canvas`,
`DeformerLocal`, and `Basic Coord` forms, nested warp/rotation deformers,
multi-axis keyform grids, and `ELLIPSE`/`SNS_CURVE` extended interpolation.
Coordinate labels alone do not establish canvas-space positions. Saved original
deformer shapes are restoration records and cannot replace animated evaluation.

## Native source reader

`live2d.authoring.read` takes an optional CMO3 `path` and returns native source records
without replacing or editing the active INX session. The stdio MCP tool is
`live2d_authoring_read`. Its `live2d.authoring.source.v1` result contains numeric
canvas dimensions, parameter GUIDs and ranges, ordered ArtMesh/deformer/part
sources, texture references, and owner-specific forms and keyform grids. Omitting
`path` reads the active native CMO3 session.

ArtMesh records retain positions, UVs outside the unit interval, triangles,
clipping and material settings. Warp records retain grid dimensions and Bezier
edit settings; rotation forms retain angle, origin, scale and reflection.
Part records retain child order and draw-order/offscreen settings. Forms retain
their GUID and coordinate type together, so distinct representations of one GUID
remain distinct. Axis references determine grid coordinate order; extended
interpolation strings and keys outside parameter ranges remain unchanged.
An explicit null image-to-logical transform stays null.

### Native hierarchy validation

All three native queries validate part parents and deformer targets before
returning results. Present parents must identify parts; present targets must
identify warp/rotation sources or the root-deformer sentinel. Hiyori's Part-only
all-zero target GUID remains unchanged as an empty reference. Missing optional
references remain missing. The root part cannot have a parent, and sources cannot
shadow the root-deformer sentinel. Both hierarchies must be acyclic, including
unreferenced sources, without requiring parent-first serialized order.

`build/verification/cmo-source-qxkq02sm/result.json` passes all four models and
23 failure controls. Eight repacked CMO3 hierarchy controls each reject on the
three queries through FFI and JSONL and on source read through stdio MCP (56
transport checks), preserving the edited INX session and original files.
The independent XML oracle uses topological elimination; Rust uses iterative
depth-first traversal. The full 349-test suite passes in
`phase2-cmo-hierarchy-rust-suite-2.log`. This validates reference integrity;
coordinate conversion, final pose evaluation and equivalent animation remain
unverified.

The reader validates indices, array counts, form ownership and grid references,
finite numbers, source/parameter/texture identity, and an aggregate five-million
scalar budget before growing numeric arrays. A separate 64 MiB copied-text budget
charges referenced strings before every copy, including repeated names and axis
metadata. The existing inspection query applies the same text limit. Image entries must exist in the
container; image pixels are not decoded by this query. CAN3 without a model is
rejected by this operation and remains supported by `live2d.authoring.inspect`.

Run `python3 tools/verify-cmo-source.py` against the verified input copies.
Its independent XML oracle compares every emitted source/form/grid/morph/texture
field with FFI, JSONL and stdio MCP, checks unchanged edited INX state and original
files, and retains failure controls. This verifies native source decoding,
not an evaluated canvas pose, model rendering, import into INX, or animation.
The reader also retains primary morph targets and blend-weight constraints with
parameter GUIDs and IDs, source-owned form references, key values, and weights.
Parameter IDs come from the model parameter source set; serialized GUID notes
are stale in 301 Zundamon target records and are not used as IDs.
Each target owner must resolve to its containing source. The supplied models have
534 targets and 233 constraints; all target edit base parameter maps are null.
Non-null edit base maps fail explicitly until their schema is verified.
Extended interpolation records retain source-owned generated forms, named grids,
nested targets and parameter-indexed settings. Optional pieces are decoded
independently. Ren has 20 generated normal forms; Zundamon has 1,240 generated
morph forms and 1,260 nested targets. Ten Ren generated forms have 53 vertices
against their source's 65; `vertexCountMatchesSource` records this count mismatch
without changing positions. Twenty Zundamon nested targets lack a form in their
owner's primary/generated lists and retain `formAvailable: false`; no global
GUID fallback is used. These flags do not prove cache freshness or evaluated
geometry.
Stale references the editor leaves behind are recorded, never repaired: an
extended-grid axis whose parameter was deleted has `parameterId: null` (the GUID
is kept), an extended-grid cell at an inserted point whose form was not stored is
listed in `missingFormGuids`, and a Glue pair naming a deleted vertex UID is
listed by pair index in `staleBindPairs`. A future Glue export must skip stale
pairs. Primary forms still require source-compatible counts, and all generated
arrays must contain finite coordinate pairs. Generated-cache regeneration and
extended morph evaluation remain pending. The primary morph query below evaluates
the supported primary targets and constraints. These records remain
intact in the original CAFF/XML and are required for equivalent animation.

Accepted primary-reader evidence is
`build/verification/cmo-source-r3sheq3h/result.json`: four models, 1,261 sources,
7,281 primary forms, all three five-axis 243-cell grids, and five failure controls.
Ren retains nine ATOP and seven OUT ArtMesh records and 331 form GUIDs reused
across owners. The 318-test suite passes in `phase2-native-cmo-rust-suite-3.log`;
the eight-file CAFF recheck passes in `caff-authoring-p3cvy5bp/result.json`.
Recorded source and executable hashes match the accepted files.

Morph-reader evidence is `build/verification/cmo-source-ibokgdil/result.json`:
all four models agree with the independent XML oracle through FFI, JSONL and
stdio MCP, retaining 534 targets and 233 constraints. Five morph-specific failure
controls supplement the five existing controls; the edited INX state and source
files stay unchanged. All 13 recorded source/binary hashes match current files.
The 320-test suite passes in `phase2-cmo-morph-rust-suite-3.log`.

Extended-reader evidence is `build/verification/cmo-source-acfg61c5/result.json`:
four-model oracle/FFI/JSONL/stdio MCP agreement, 44 extensions, 36 extended grids,
1,260 generated forms, 1,260 nested targets and 124 SNS_CURVE settings. Ten
vertex-count mismatches and twenty unavailable targets retain their flags.
All fifteen failure controls preserve the edited INX session and original files;
the 13 source/binary hashes match. The 324-test suite passes in
`phase2-cmo-extended-rust-suite-2.log`. These checks establish metadata decoding,
not cache regeneration or compatible model evaluation.

## Primary linear weights

`live2d.authoring.linear_weights` (stdio MCP `live2d_authoring_linear_weights`)
reads a CMO3 `path` without changing the active session, including pending edits.
Optional `parameterValues` maps model parameter IDs to finite numbers; omitted
IDs use model defaults. Optional `sourceGuid` selects one owner. The result
`live2d.authoring.primary-weights.v1` retains source order, resolved `axisValues`
in grid-axis order, and positive `weights` with original `cellIndex`, `formGuid`
and numeric `weight`. An absent primary grid has `weights: null`; a constant
zero-axis grid has one weight of 1.

The query computes tensor-product weights only for primary axes whose ordinary
and extended interpolation modes are both `LINEAR`. It rejects samples outside
stored key ranges, missing active corners, unsupported modes and unrepresentable
fractions/products. It neither clamps nor wraps values or renormalizes sparse
grids. Parsing and computation share the existing scalar/text budgets.
Generated caches, morph targets, blend constraints and coordinate transforms
are not evaluated. These weights alone do not produce a Cubism pose.

Run `python3 tools/verify-primary-weights.py` after building the Rust core.
The independent raw-XML oracle checks all four models at defaults and the three
five-axis sources at symmetric and asymmetric samples through FFI, JSONL and
actual stdio MCP, with unchanged edited INX state and original inputs.
The 333-test suite passes in `phase2-cmo-linear-rust-suite-2.log`.
Final evidence is `build/verification/primary-weights-hqztrbku/result.json`:
30 positive transport comparisons, seven failure controls, all 1,261 default
corners, 367 constant grids and six 32-corner queries. Raw vertex-0 sums agree
with independent XML calculations; they remain source-space values. All fourteen
source/binary hashes match. The source-reader regression also passes all fifteen
controls in `build/verification/cmo-source-kpn8iq_7/result.json`.

## Primary form interpolation

The separate morph geometry query below adds target deltas; the existing
`primary_forms` response and interpolation remain unchanged.

`live2d.authoring.primary_forms` (stdio MCP `live2d_authoring_primary_forms`)
accepts the same path, parameter overrides and optional owner filter as the
weight query. It returns `live2d.authoring.primary-forms.v1` source records with
`parentPartGuid`, `targetDeformerGuid`, `axisValues`, `weights` and
`evaluatedForms`. A missing primary grid has null weights and evaluated forms.
For each positive corner, forms resolve only within the containing source;
unreferenced primary forms and generated caches do not contribute.

Each evaluated representation retains `coordinateType` and interpolates
positions, opacity, multiply/screen colors, rotation angle/origin/scale and
draw order. Distinct coordinate representations remain separate. All active
corners must supply the same representation set, geometry type/array length,
draw-order presence and rotation-reflection flags. Unsupported differences
fail explicitly. Rotation angles remain authored numbers without shortest-arc
wrapping. Draw order remains a floating weighted value, without guessing the
runtime rounding or sorting policy. Animated flags are not evaluated here.

This is primary source-form arithmetic, not the final model pose. The owner’s
deformer reference is retained for the next coordinate-conversion stage.
Morphs, blend constraints, extended interpolation/cache generation, deformer
transforms and inherited render properties remain separate requirements.
The query preserves the active INX session, including pending deformation.
Run `python3 tools/verify-primary-forms.py` after building the Rust core; require
independent raw-XML agreement through FFI, JSONL and actual stdio MCP at all four
defaults and the six five-axis samples, plus unchanged active state/input hashes.

Accepted evidence is `build/verification/primary-forms-b75hlzsj/result.json`:
42 positive FFI/JSONL/stdio MCP comparisons and eighteen failure controls. The
four defaults cover all 1,261 owners. Six five-axis samples check all interpolated
positions and attributes; additional filtered probes cover Hiyori Pro ArtMesh3's
literal 637.5 draw order, Warp46 positions, Rotation23 angle/origin/scale, and
Ren PartAll opacity. The edited INX and original inputs stay unchanged; all
sixteen source/binary hashes match. The 340-test suite passes in
`phase2-cmo-primary-forms-rust-suite-1.log`. The weight and source-reader regressions
also pass in `primary-weights-o8dks9fr/result.json` and
`cmo-source-aa1vk722/result.json`, with fourteen/thirteen matching artifact hashes
and seven/fifteen failure controls respectively.

## Primary morph geometry

`live2d.authoring.morph_forms`, stdio MCP `live2d_authoring_morph_forms`, and
embedded MCP `Live2DCommand_AuthoringMorphForms` accept the primary query's
optional path, `sourceGuid`, and `parameterValues`. Omitting the path reads the
active native document. The result format is `live2d.authoring.morph-forms.v1`.
Each source keeps the primary response fields and adds `baseFormGuid` and
`morphWeights`, whose entries identify parameter ID, key value, form GUID, and
positive weight. Entries use parameter-ID order, then ascending key values.

This query adds target-minus-neutral geometry and opacity to the current primary LINEAR
interpolation. The neutral form must be a unique stored corner at the model's
normal parameter defaults. Morph parameters must have type `MORPH_TARGET`,
zero within their declared range, and no repeat. Their defaults can be nonzero.
Piecewise linear weights include an implicit zero
key. Values beyond the target keys saturate at the outer key, but values outside
the parameter's declared range fail. Independent parameter deltas add.

Finite unique target keys outside that range remain valid retained metadata;
they contribute only if their interpolation interval contains a legal sample.
Zundamon Rotation52 retains an inactive negative key outside its 0..1 range.

Positions, raw rotation angle/origin and opacity are supported. Opacity differences
add before the final result clamps to 0..1. Every target must match
the neutral representation set, geometry type, and position count. Changed
draw order, colors, rotation scale/reflection, duplicate or zero target
keys and ambiguous default corners fail, including
when the target is inactive. Sources without targets retain primary forms with
null `baseFormGuid` and empty `morphWeights`.

Blend-weight constraint records group by target parameter and controller within
their owner. Each supported curve must have unique finite knots spanning the
controller's complete declared range, weights in 0..1 and a non-repeating
controller distinct from its target. Between knots, limits interpolate linearly.
The smallest controller limit multiplies the original morph weight once.
Partial, duplicate or ambiguous graphs and out-of-range controller samples fail,
even for inactive targets. The Japanese
[weight-limit manual](https://docs.live2d.com/cubism-editor-manual/limit-settings-for-blend-shape-weights/)
explicitly defines this minimum rule; multiplying all controller limits is incorrect.

The [Live2D blend-shape manual](https://docs.live2d.com/cubism-editor-manual/blend-shape/)
describes additive differences from a normal form closest to the default.
The exact-default-corner requirement avoids guessing that selection when no
stored default exists. Extended caches, deformer transforms, other scalar morph
limits, rendered poses, and vendor equivalence remain separate requirements.
Run `python3 tools/verify-morph-forms.py` after building the Rust core. It checks
the supplied Ren and Zundamon targets against independently decoded XML through FFI, JSONL,
and stdio MCP, and checks that queries preserve the active session and originals.
The earlier unconstrained receipt `morph-forms-e6motefi/result.json` passes 329 cases,
987 complete-response comparisons and 30 refusal checks. The registered HTTP
route passes five ArtMesh/Warp/Rotation queries in `live2d-http-61712-0` alongside
four real-model vertex/opacity edit, history, save and reopen workflows.
The isolated constraint receipt `morph-constraints-q0xke5qb/result.json` passes
353 cases and 1,059 complete-response comparisons over FFI, JSONL and stdio MCP.
All 116 curves are probed at every knot and midpoint with other controllers at
limit 1, alongside three literal controls. It retains transport responses and
preserves native snapshots and the original. This avoids masking by default
controller values. Run `python3 tools/verify-morph-constraint-limits.py`.
The current constrained-morph corpus receipt, `morph-forms-k20j2yug/result.json`,
passes 1,629 cases, 4,887 complete-response comparisons and 36 refusal comparisons.
Its 20 source/binary hashes match current artifacts, and active INX/native
snapshots and all four original CMO3 inputs remain unchanged. This proves the
stated source-space arithmetic; it does not prove rendered Cubism equivalence.

## Opened in Cubism Editor (issue #148)

On 2026-10-18 two files rewritten by `soul2d-core` were opened in Cubism Editor
5.3.04 (trial licence) on macOS, from the official `hiyori_free` sample, which is
not in this repository:

- `hiyori_free_t08.cmo3` after `live2d.form.set_opacity` (0.5), `live2d.form.set_vertex`
  (vertex 0 moved by 0.05 in `DeformerLocal` space) and `live2d.rename` ("Soul2D renamed")
  on ArtMesh61, then `save`. The model opened and drew. The Inspector showed the new name,
  ID `ArtMesh61` and opacity 50 %. The Log palette held only INFO lines.
- `hiyori_free_t03.can3` after `live2d.animation.key.set` moved the `ParamAngleX` key at
  frame 6 of scene `hiyori_m01` from -8 to 12, then `save`. With the rewritten CMO3 beside it
  under the linked name, the CAN3 opened with its eight scenes, and at frame 6 the timeline
  showed Angle X 12.0.

This checks that Cubism Editor opens these rewrites and reads the edited values. It does
not cover other edit kinds, saving from Cubism Editor, or export to runtime files.

## Native session editing

`open` detects supplied-profile CMO3 CAFF containers and loads their source scene
into the shared Rust session. Snapshot `documentFormat` is `live2d.cmo3`; an INX
model's unknown `format` field does not change its storage format. Authoring
queries may omit `path` to read the active native document, including edits.
Explicit-path queries remain read-only inspections of another file.
Active linear-weight, primary-form and morph-form queries reuse the scene for
the current model state. Edits that change the model, Undo/Redo and Open/New discard that scene;
explicit-path queries do not replace it. Each evaluation retains the original
post-parse work limits. This is an internal optimization of the same response
contracts, not a new rendered-pose evaluator.

`live2d.form.set_vertex` changes one primary ArtMesh or Warp form vertex. It
requires `sourceGuid`, `formGuid`, a nonempty string `coordinateType`, zero-based
`vertexIndex`, a two-number `position`, and the current `expectedRevision`.
The owner/form/coordinate tuple disambiguates repeated GUIDs. Coordinates remain
in their recorded space and must be finite and within the native float range.
The command shares normal revision, modified state, undo and redo. No-op edits
retain the revision; stale requests and invalid edits leave the session intact.
Embedded MCP clients read `resource://soul2d/document/state` before editing.
This read-only resource returns the live Rust `documentFormat`, `revision`,
`modified`, `canUndo` and `canRedo`, without the full model payload. It applies
to both INX and native CMO3. Use its revision as `expectedRevision`; a concurrent
edit still causes rejection, so reread state before retrying. For a selected
native source, `Live2DCommand_AuthoringPrimaryForms` with `sourceGuid` returns a
bounded filtered result instead of reading the complete authoring scene.

The writer patches only changed scalar tokens in direct, unshared primary
`float-array` position storage. It preserves all other XML bytes, numeric
spellings, whitespace, image payloads and unknown container entries. It rejects
aliases or inbound shared references, changes to any other scene field, changed
vertex counts, owners with generated extended interpolation, base forms of an
owner with blend shapes, and non-plain text storage. A blend shape's own form
(one a morph target of the owner names, and no cell of its keyform grid) is
edited like any other form: the file stores a shape as a whole form and the
shape is its difference to the base form, so the edit changes that one shape.
A base form of such an owner stays refused because every shape of the owner
would change with it, and what Cubism Editor does to the shapes then was not
observed. These restrictions remain delivery
work; they do not establish full authoring compatibility. The rewritten CAFF is
reparsed and its complete source scene must equal the requested canonical model
before the edit reaches history. XML alias and position-user checks use bounded
indexes. Immutable template data never becomes a second editable document.

Normal `save` writes CMO3 atomically, with existing external-file and destination
conflict checks, and requires a `.cmo3` destination. Saving retains history;
reopening loads the saved source geometry into a fresh history. Native rendering
and INX conversion remain unsupported.

The Rust GUI includes a source/form/vertex coordinate editor. Its Apply action
and the embedded `Live2DCommand_FormSetVertex` MCP tool use the same Session
operation. Dirty drafts require Apply or Reload before saving or replacing the
document, including window close; revision conflicts retain the draft.
Automatic recovery uses discoverable `.cmo3` snapshots and `mark_saved:false`,
retaining the active document's revision, history and modified state.
Native canvas preview remains
pending. ACP agents reach these tools through the existing embedded MCP endpoint.
The opt-in `tools/verify-real-acp.py --native-cmo3 INPUT` (deleted with the D tree) checked real-agent editing
of one supplied Hiyori Free ArtMesh primary vertex through the production ACP
client. It uses scratch copies, a separate exact-edit permission guard and the
public document state revision. The parent requires edit, stale rejection,
Undo/Redo, public post-save state, reopen, complete model comparisons and exact
XML/opaque preservation outside the changed coordinates. Native rendering and
foreground GUI input are outside this check.
The actual workflow passes in
`build/verification/real-acp-native-79to5zif/acceptance.json`, including the
public post-save state check and owned-process cleanup. The embedded HTTP
workflow passes all four models with 33 public state reads in
`build/verification/live2d-http-54392-0/result.json`.

The current Rust evidence is
`build/verification/native-authoring-edit-zwyawzon/result.json`: 15 actual
ArtMesh/Warp FFI, JSONL and stdio MCP workflows, 74 rejection controls, and 18
matching source/binary hashes. The full suite passes 357 tests in
`phase2-cmo-edit-rust-suite-3.log`. Source reads, primary weights and primary
forms pass in `cmo-source-pi7fv6fw`, `primary-weights-yntetvpl` and
`primary-forms-zpg7g4s2`, with 16, 17 and 19 matching hashes respectively.
The embedded HTTP MCP workflow passes on all four real models in
`build/verification/live2d-http-4961-0/result.json`; `recovery-result.json` records
the actual native autosave/reparse check. The final D suite passes 304 automated
cases in `phase2-cmo-edit-d-suite-5.log`; the macOS app build passes in
`phase2-cmo-edit-d-app-build-3.log`. HTTP numeric comparisons allow only 2e-15
absolute/relative float round-trip differences from D JSON; the saved XML and
opaque entry preservation oracle remains exact. Draft-close regression passes
in `phase2-cmo-edit-d-gui-close-1.log`. These results leave all 103 foreground
computer-use cases unexecuted.

Cubism Editor re-opening of the rewritten files has not been tested. Runtime
MOC3 read/write, model rendering, animated equivalence, INX conversion, and GUI
authoring remain incomplete. CMO3 XML replacement alone does not prove them.

`open` refuses Live2D runtime files and names the kind it recognised from the
content. For `.motion3.json`, `.exp3.json`, `.physics3.json` and `.pose3.json`
the message names the operation that loads the file onto an open CMO3
(`motion3.import`, `live2d.expression.import`, `live2d.physics.attach`,
`live2d.pose.attach`). `.moc3`, `.model3.json`, `.cdi3.json`,
`.motionsync3.json` and `.userdata3.json` are reported as not read. The editor
shows the same message after "Open failed:".

Renaming uses `live2d.rename {guid, name, expectedRevision}` (`live2d_rename`,
`Live2DCommand_Rename`). It sets the display name of one ArtMesh, ArtPath, Part, deformer
or Glue (`localName`), one ArtPath brush or one parameter (`name`) and leaves the ID string
alone. The writer replaces one inline `<s>` element of the object. It rejects a
name stored through `xs.ref`, a name element another object can reference, and
an object without a name element.

The structure of the sources is edited with these tools, each one undo step
with the current `expectedRevision`:

- `live2d_set_id` changes the ID string of a source or a parameter. A
  parameter's keyform axes and morph weights follow it; motion, expression and
  physics files that name the old ID do not.
- `live2d_reparent` moves a source under another Part, at the end of its
  children. The root Part, a move under the source itself or its descendant,
  and a file whose GUID entries are shared definitions are refused.
- `live2d_reparent_deformer {sourceGuid, targetDeformerGuid, expectedRevision}`
  reattaches one static ArtMesh between existing static Rotation chains. It
  reexpresses the sole DeformerLocal form's positions, preserving canvas
  coordinates within `1e-6` per coordinate. Source-local round-trip precision
  loss above `1e-6` is refused before changing the model. Part membership, mesh opacity,
  topology, UVs, material and coordinate labels remain unchanged. Rootmost
  rotations require Canvas forms, nested rotations DeformerLocal, with neutral
  opacity and positive individual and complete-chain scales at least `1e-6`.
  Keyed, morph, extended/generated, Warp, reflected and Glue-bound geometry,
  shared writable storage and numerically unsafe results are refused. Root or
  detach destinations are unsupported. The same target is a no-op after
  revision and identity checks. The last successful model is readable through
  `live2d_authoring_read` and `model.sources` in `resource://vsoul/snapshot`. This bounded
  operation preserves Soul2D's native canvas calculation; vendor reparent
  equivalence has not been verified.
  In the GUI, choose an ArtMesh under **Live2D objects**, choose a
  **Rotation destination**, then press **Reparent to Rotation**. **Current target**
  shows the stored relationship separately from the pending choice. Listed
  Rotations are identities, not a guarantee of eligibility; unsupported edits
  show the backend refusal without changing the document. Selecting the current
  Rotation leaves revision and history unchanged. **Parent Part / Move** remains
  a separate operation. Matching destination labels include their GUIDs;
  destination tooltips also show GUIDs.
- `live2d_duplicate` copies an ArtMesh, ArtPath, deformer, Glue or empty Part
  under the same parent, or a parameter that nothing keys. The copy is the
  original's own XML with new GUIDs, and its `id` defaults to `<id>_copy`.
  It also copies a brush at the end of the brush list. The new brush inherits
  the current settings, including edits to an unsaved copy. Its texture settings
  get new identifiers while image bytes and model-image references stay shared.
  A texture setting embedded in another model object cannot be copied.
  This requires a donor brush and does not create a brush from nothing.
- `live2d_delete` and `live2d_parameter_delete` remove a source or a parameter
  that nothing else names, with the objects only it uses (its GUID object,
  extensions, keyform grid, an unused texture and a Cubism 2.1 conversion
  record that names it). A refusal says what still names the entry. A delete
  composes with copies and reparents made earlier in the same unsaved session,
  and a Part can be deleted after its last child in one session.
  `live2d_delete` also removes an unused brush. Any remaining ArtPath or XML
  reference prevents deletion, including unknown registry entries. The brush
  list remains present when its last brush is removed.

No object can be created from nothing: every new object is a copy of one the
file already holds.

The stored settings of a primary form and of a source are edited with
`live2d_form_set` and `live2d_source_set`. `live2d_form_set` covers draw order
of ArtMesh and existing private ArtPath primary forms, colours of ArtMesh and Part forms, angle, origin, scale
and reflection of rotation deformer forms, and the intensity of Glue forms.
An ArtPath draw-order request is an integer in `0..1000`. It requires an existing
plain `i/drawOrder` token through a private inline form inheritance chain.
Missing fields, aliases, IDs on mutable storage, duplicate or shadowed fields,
nested content, comments, CDATA and entities are refused atomically; no donor
field is inserted. Legitimate source IDs and form GUID, coordinate and `_source`
references remain unchanged. The original form's existing extended/morph edit
guard is checked again by the writer. Surrounding XML whitespace, including
CR/CRLF, and every other XML byte are preserved. Reader whitespace handling is
limited to primary ArtPath forms; empty optional order still reads as absent
and cannot be edited. Equal values are no-ops; a change is one Undo step and
supports save/reopen and staged point/source/copy edits. This adds no ArtPath
colour or opacity editing, GUI control, rendering or vendor acceptance.

`live2d_source_set` covers blend modes, `invertClippingMask`, a Part's
`useOffscreen`, and the clipping masks (`clipGuids`) of an ArtMesh, Part or
ArtPath. Its optional `closed` boolean also edits the stored closure of an ArtPath.
ArtPath inversion requires one existing private `b` named `invertClippingMask`
in the source's inline inheritance. The entire inheritance chain must be private
and unambiguous, even after the flag is found. Only a plain `true` or `false`
token changes; surrounding XML whitespace, including CRLF, is preserved. Missing,
shared, referenced, shadowed, encoded, CDATA or nested storage is refused without
changing the document or Undo/Redo. Source identity references remain supported.
This source flag adds no form-cache or morph restrictions. Combined source settings
use one Undo step; equal values are no-ops. This adds no native control, rendering
or vendor acceptance. Existing ArtPath name and clipping-mask APIs are unchanged.
A changed closure requires exactly one existing direct private `b` field named
`isClosed`, containing plain `true` or `false` text. Missing, duplicate,
inherited, aliased, nested, CDATA and escaped storage is refused before document,
revision or Undo/Redo state changes. Surrounding whitespace and all other XML
tokens remain unchanged. Equal values are no-ops; optional null and unknown-key
handling follows the existing source setter. This adds no editor control or
ArtPath rendering and does not establish vendor acceptance.
The optional `divisionCount` integer edits an ArtPath's existing direct private
`i` field named `divNum`, from 0 through 4294967295. It preserves surrounding
whitespace, including CR/CRLF; missing, duplicate, inherited, aliased, nested,
CDATA or escaped storage is refused. These are storage bounds, not vendor limits.
The optional `brushGuid` selects exactly one existing member of the model's
`brushes` list. Only the ArtPath's direct private `CArtPathBrushGuid` slot changes:
an inline UUID stays inline, a reference uses the destination GUID's existing ID,
and a reference becomes an inline UUID when that destination has no ID. Shared
GUID definitions and other references stay unchanged. Ambiguous, shared or
content-bearing slots are refused. Escaped attribute values are supported,
including numeric whitespace references. Combined settings use one Undo step;
Undo in the same Session restores original bytes. Reversing a selection after
reopening restores its meaning, without promising the original reference spelling.
This adds no brush creation, image-input retargeting, editor control or rendering.
Line generation and vendor acceptance remain separate work under #159.
Other scalar fields the file does not store are copied from the same field
elsewhere in the file, and is refused where the file stores it nowhere. A
missing mask list is copied, emptied, from another source of the same kind. In
the two samples whose fileFormatVersion starts with 40 no Part stores one, so a
Part mask stays refused there. The
editor's "Live2D source forms" panel edits the same settings for the selected
source and primary form: draw order and colours (ArtMesh, colours also Part),
rotation angle, origin, scale and reflection, Glue intensity, and the blend
mode, clipping inversion, offscreen drawing and clipping masks of the source.
"Apply form settings" and "Apply source settings" each send one request with the
`expectedRevision` the draft was loaded at, name only the settings that changed,
and show the refusal text when the file cannot store a setting.

For an existing ArtPath, this panel offers **Closed**, **Division count**
(`0..4294967295`), **Brush**, **Invert clipping mask**, and **Clipping masks**.
Brush choices come from the document's existing list and include their GUIDs.
An unlisted current brush stays unchanged until a listed replacement is chosen;
an empty list disables the chooser. Source settings work without a primary form.
A primary ArtPath form offers **Draw order** (`0..1000`) and **Opacity** with its actual coordinate
identity. **Apply opacity** uses the existing guarded `live2d.form.set_opacity` operation;
it changes form opacity independently of control-point opacity and requires no vertices.
Each Apply retains the existing revision guard, changed-fields-only
request, refusal message and separate Undo step. Missing private storage can still
refuse a projected setting. This adds no ArtPath vertex, colour, creation
or rendering controls; native and vendor acceptance remain separate evidence.

The metadata phase's focused panel checks pass 17 tests, including actual egui choices, numeric
drags, checkboxes and Apply buttons. The full suite passes 2,880 tests, with zero
failures and six ignored tests. These results do not establish native ArtPath
acceptance. Two isolated macOS attempts remained INCONCLUSIVE: the first reached
Source forms but lost observations before Apply; after the full suite, inventory
worked but capture of a fresh named-input app still timed out. Neither attempt
sent a native Apply. Native Undo/Redo, save/reopen and Quit remain unverified.
Both owned processes were terminated only after clean readback, with their ports
closed; this cleanup was not native Quit. Input hashes stayed unchanged.

Final evidence review found a later process using the first private input after
its recorded cleanup. A fresh read showed the initial model clean, and its exact
launch arguments identified it as another owned verification process. It was
terminated separately and its port was checked closed again. The original exit
receipt is retained; the later process's origin is not established.
`artpath-gui-late-relaunch-cleanup.json` records this reconciliation. Port closure
is an observation at each check, not a promise that a port remains unused.

Successful tests remove their scratch inputs. The native attempts used an early
retained test fixture and a separate synthetic copy with two display names added
to its XML through the existing packing helper. The derivation is recorded rather
than described as the successful test artifact or an original-container copy.
`build/verification/goal-20261018/handoff-20261022/artpath-gui-full-result.json`
records the full suite; `artpath-gui-native-result.json` and
`artpath-gui-native-final-result.json` retain the separate native limits.
Their `verify-artpath-gui-*-inconclusive.py` readers check preservation and cleanup
without promoting the native result to PASS. No timeout cause or speedup is proven.

On 2026-10-24, an owned macOS window exercised both Apply buttons through native
input, with AX observations, screenshots and HTTP MCP readback through `njc`.
The form test changed multiply-color R from 1 to 0.5; the source test changed one
ArtMesh's composition from ADD to NORMAL. Drafts left the entire Session unchanged
until Apply. Each Apply made one edit, native Undo/Redo restored the models, and
saved copies reopened equal. A missing draw-order field was refused without a
document change. An external vertex edit retained the draft, disabled Apply and
showed the revision warning; native Reload refreshed only the draft.

The same isolated window exercised Physics groups' input-preset Apply, Save,
Replace and Delete controls. Apply restored weight 25 to 50 in one Undo step;
typed physics and resource readback agreed, and save/reopen retained the result.
The raw physics XML changed only the equivalent weight token `50.0` to `50`.
Save, Replace and Delete affected only the isolated preset store, preserving the
entire document snapshot and its history. Replace changed only the selected
preset, and Delete restored the original preset resource. Original files and
prepared copies stayed unchanged; the owned process exited and its port closed.
The receipt is `build/verification/goal-20261018/handoff-20261022/native-controls-final-result.json`.
Screenshots were observed in chat, not saved as receipt files. This earlier
receipt covers six named controls; the vertices-preset controls were separate.

A fresh isolated macOS window on a 2026-10-24 build exercised five more existing
controls through CuaRepl: Angle (20 to 21), Draw order (900 to 901), Clipping
masks (one mask to none), Invert clipping mask (true to false), and Use offscreen
(false to true). Angle and Draw order explicitly selected the form at the
parameter defaults, rather than assuming the first form. Each native Apply made
one edit; changing the draft back disabled Apply and preserved the whole snapshot.
Native Undo/Redo, save and reopen matched the complete expected models. Undo
restored the previous saved file byte for byte; the independent reader allowed
only the named XML field to change and preserved container metadata and other
payloads. The mask-list test retained inversion, and the inversion test retained
the original mask list. MCP supplied reads, save and open, without edit requests.

Use offscreen passed on a small private synthetic fixture: an existing committed
fixture's XML plus one directly observed boolean field on an existing Part,
repacked with the existing test helper. This does not establish original-container
derivation or vendor compatibility. On the larger input, three native AX reads
timed out; no offscreen edit was attempted there, so that native case remains
INCONCLUSIVE. Original inputs and preparation copies stayed unchanged. The owned
process exited cleanly and its port closed. The retained-evidence check is
`build/verification/goal-20261018/handoff-20261022/verify-native-fields.py`, with
`native-fields-result.json` beside it. AX observations and screenshots remain in
the CuaRepl transcript. This verification changed no product code and ran no new
Cargo tests; pixel parity and broader compatibility remain unverified.

A later isolated run completed the larger real-input Use
offscreen case. CuaRepl selected an existing Part and changed its stored value
from true to false through native Apply. The whole-model comparison found only
that boolean change and one history step. Changing the draft back preserved the
entire snapshot. Native Undo/Redo, save and reopen passed; the Undo file equalled
the original bytes. The independent XML reader allowed only that field to
change, preserving container metadata and all other payloads. The ten MCP wire
pairs contain reads, save and open only. Original and preparation-copy hashes
stayed unchanged; the clean owned process exited and its listener closed.
`build/verification/goal-20261018/handoff-20261022/verify-large-offscreen-native.py`
rechecks retained evidence; `large-offscreen-native-result.json` records the
result. Intermittent native observation timeouts occurred even without Cargo.
This bounded completion supersedes the earlier unattempted large-input case;
it does not establish a performance improvement, vendor behavior or pixel parity.

A fresh isolated macOS window on the accepted `a1ce382d` build then exercised the
four vertices-preset controls through CuaRepl native input. Apply restored vertex
1's delay from 0.4 to 0.6, restoring the exact initial model and typed physics in
one Undo step. Native Undo/Redo restored each model; equal Apply preserved the
entire snapshot. The saved result equalled the original file byte for byte.
Save added one named vertices preset, Replace changed only its delay from 0.6
to 0.7, and Delete restored the seeded preset resource. Each store action kept
the whole document snapshot, revision and history unchanged; the other vertices
preset and the inputs preset were preserved. On-disk stores matched resource
reads. Both saved documents reopened equal; the final changed document differed
only in the one delay token, with protected container metadata and other payloads
unchanged. Original input and preparation-copy hashes stayed unchanged.
The owned process exited and its loopback port closed. Native observations and
screenshots are in the CuaRepl transcript; the repeatable retained-evidence check
and receipt are `build/verification/goal-20261018/handoff-20261022/verify-vertices-native-evidence.py`
and `vertices-native-result.json` in that directory. Other physics controls,
pixel parity, first-group/shared-storage editing and Cubism acceptance remain
separate. This verification changed no product code and did not rerun Cargo.

An ArtMesh's user data is the text Cubism keeps in its `userData` string and
writes beside a model as `.userdata3.json`. The scene shows it as
`sources[].userData`, absent when the text is empty, and only for an ArtMesh.
`live2d.user_data.set {guid, value, expectedRevision}` (`live2d_user_data_set`)
sets it, and an empty `value` clears it. The writer replaces the inline
`userData` element of the ArtMesh under the same rules as a rename. A text can
hold tabs and line feeds but no carriage return, since XML turns one into a line
feed, and at most 65536 bytes. `live2d.userdata3.export {path?}`
(`live2d_userdata3_export`) answers `Version` 3, `Meta` with `UserDataCount` and
`TotalUserDataSize`, and one `UserData` row `{Target: "ArtMesh", Id, Value}` per
ArtMesh with user data, in source order. `TotalUserDataSize` is the UTF-8 byte
count of the texts. The three vendor sample files hold ASCII only, so a byte
and a character count agree there and which one the vendor means for other
text is not known. All 10315 ArtMeshes of the 94 sample CMO3 files have an
empty `userData` element, and no sample folder holds a `.userdata3.json`, so
the export is not compared with a vendor file of the same model. The CMO3
export switch `exportUserDataJson` (true in 52 samples, false in 22) is not
read; the export answers whenever it is asked.

The display information is exported with `live2d.cdi3.export {path?}`
(`live2d_cdi3_export`). The result is the document Cubism writes beside a model
as `.cdi3.json`: `Version` 3, `Parameters` and `ParameterGroups` with `Id`,
`GroupId` and `Name`, `Parts` with `Id` and `Name`, and `CombinedParameters`.
Parameters and groups come in the order a walk of the parameter group tree
meets them, and a member of the root group has an empty `GroupId`. Parts come
level by level below the root Part, which is not listed. A parameter with
`combined` set pairs with the next member of its group; the key is left out
when no parameter is combined. With `path` the same JSON is also created
atomically, and an existing destination is refused. The export reads the
document as edited, changes no revision and is no undo step.

The export refuses what no sample shows instead of guessing: a combined
parameter whose next member is a group or the end of its group, a member whose
`parentGroupGuid` is not the group that lists it, a member named twice, a
parameter or group outside the group tree, and a Part outside the Part tree.
In the 94 sample files none of these occurs, and all 215 combined parameters
are followed by a parameter of the same group.

`tools/verify-cdi3-export.py <directories>` compares the export with a
derivation written apart from it and with the vendor files, after parsing. On
the supplied samples the export equals the derivation for 94 of 94 CMO3 files.
64 of them have exactly one `.cdi3.json` below their folder. Against those,
`Parameters` and `ParameterGroups` are equal in 64, and 52 files are equal as a
whole. In 5 the vendor file lists one Part ID the CMO3 does not hold and the
rest of `Parts` is equal, so those pairs are not one export.
`CombinedParameters` is equal in 37 and absent from both in 20. In 7 the vendor
file has no `CombinedParameters` although the CMO3 combines parameters; the
cause is not known, and the export writes the pairs. Keys are written in
alphabetical order, not in the order of the vendor file. No Cubism Viewer or
SDK has loaded an exported file, so vendor acceptance is NOT_RUN.

The first physics group can be created with `live2d.physics3.edit` in an existing
empty inline settings list. Omit `group`, `list`, `index` and `field`, and pass
`insert: 0`, the current `expectedRevision`, and
`value: {name, normalization: {position: {min, default, max}, angle: {min, default, max}}, inputs: [], outputs: [], vertices: []}`.
All five group fields are required. Populated arrays use the existing item shapes
and parameter IDs. The writer allocates the next unused `PhysicsSetting<n>` ID;
it preserves the surrounding XML and container entries. Missing, null, shared or
nonempty settings lists refuse this operation. Existing group duplication remains
available. Creation is one Undo step and uses the same save/reopen checks as edits.
Creation also gives the group a fresh GUID and selects it for the vendor editor.
Duplication preserves the current selection. Removing the selected group chooses
the first surviving group with a unique identity, or clears selection if none
remain. Missing or ambiguous survivor identities, opaque outside GUID links, and
unsupported selection storage refuse the removal atomically. Removing another
group leaves selection unchanged.
Saving changed physics source adds missing class imports to the XML prolog for
classes introduced by the edit. Existing declarations and their formatting
remain intact; a conflicting short class name refuses the edit atomically.
The Physics groups panel exposes **Create first group** with empty input, output
and vertex lists, position normalization `-10, 0, 10` and angle normalization
`-30, 0, 30`. This creates storage; simulation and Cubism acceptance require
separate verification.

Before editing shared storage, use `live2d.physics3.edit` with only
`expectedRevision` and `detach: true`. The Physics groups panel exposes the same
operation as **Make physics settings independent**. It copies the complete known
physics ownership tree into private inline storage in one Undo step. Original
shared definitions remain available to outside references. Parameter UUIDs,
unknown markup and numeric spelling are preserved. Repeating the operation on
private settings changes no state or history. Ordinary edits still refuse shared
storage and never detach it implicitly.
Existing parameter-GUID references remain references. The selected-group GUID
follows the copied group; ambiguous or missing selected-group identity refuses
the copy atomically.

Mutable ownership supports direct fields and one-hop references of the known physics classes.
Parameter-GUID references are borrowed and their chains remain unchanged.
Alias chains in mutable ownership, inherited fields,
conflicting alias metadata, ambiguous opaque references into copied identities,
ownership cycles and serial overflow refuse atomically. The copy is limited to
16 MiB and 500,000 emitted XML nodes, in addition to the existing group/item
limits. It does not add missing GUID fields or establish Y-input, X/Y-output,
simulation or native vendor acceptance.

The physics settings are exported with `live2d.physics3.export {path?}`
(`live2d_physics3_export`), in the layout of `.physics3.json`. Each physics
setting of the CMO3 becomes one entry of `PhysicsSettings` with its inputs,
outputs, pendulum vertices and the two normalization ranges, and one entry of
`Meta.PhysicsDictionary` with its name. Three rules come from the vendor files
and not from the CMO3 alone:

- Settings keep the order of the CMO3 and are numbered `PhysicsSetting1..N`.
  The IDs in the CMO3 are not written: they are not `1..N` in 29 of the 63
  sample pairs and repeat in 5, while every vendor file is numbered `1..N`.
- Numbers are rounded to three decimals, and a whole number has no fraction.
  All 15,562 vendor numbers equal the CMO3 value rounded so, and 72 of them
  differ from the unrounded text.
- `Meta.EffectiveForces` is gravity `(0, -1)` and wind `(0, 0)`. No field of
  the CMO3 was found for it, and all 63 vendor files hold these values.

`Meta.Fps` is written only when the CMO3 sets `settingFPS`. The input types
are `SRC_TO_X` (`X`) and `SRC_TO_G_ANGLE` (`Angle`), and an output must be an
angle, whose `Scale` is `angleScale`. The 94 samples hold no other type, so any
other is refused, as is an output whose vertex index is not between 1 and the
vertex count less one, a setting without an input or an output, and a CMO3
without a physics setting. A file created with a path ending in
`.physics3.json` can be given to `live2d.physics.attach`. That reader has
limits the export does not apply, such as weights from 0 to 100 and at most
256 settings and 16384 inputs, outputs or vertices; the export applies those
two counts as well. The reader accepted the export of all 83 sample files and
ran one step on each, measured once with a script that is not in the
repository.

`tools/verify-physics3-export.py <directories>` compares the export with a
derivation written apart from it and with the vendor files, after parsing. On
the supplied samples the export equals the derivation for the 83 CMO3 files
that hold a physics setting, and the other 11 are refused. 63 of the 83 have
exactly one `.physics3.json` below their folder. All 63 vendor files equal the
export apart from the setting names, and 42 are equal as a whole. Of the 500
setting names, 166 differ, in 21 files; in 20 of those every name differs. None
of the 166 vendor names is the name of another physics setting of the file, and
103 occur nowhere in the XML of the CMO3. The cause is not known. The export
writes the names of the CMO3. No Cubism Viewer,
Editor or SDK has loaded an exported file, so vendor acceptance is NOT_RUN.

`.moc3`, `.model3.json`, `.pose3.json` and `.exp3.json` are not written.

Primary ArtMesh and existing private ArtPath opacity editing uses `live2d.form.set_opacity` and
`live2d_form_set_opacity` in the Rust API and stdio MCP. The embedded MCP command
is `Live2DCommand_FormSetOpacity`. All require source/form/coordinate identity,
finite native-float opacity and the active `expectedRevision`. Values are not
clamped: supplied Hiyori forms include authored opacity `1.00005`.
Writes require one inline opacity scalar, including its inherited form storage;
shared aliases and unsupported derived forms are rejected. Combined vertex and
opacity edits preserve the rest of the source XML and opaque container entries.

ArtPath writes require one existing private literal `f/opacity` through private inline
form inheritance. The complete form spine is checked for aliases, IDs, references,
duplicate or shadowed fields and ambiguous inheritance; missing/default-only or
nonliteral storage is refused without insertion or donor copying. Only the numeric
token changes, preserving surrounding XML whitespace including CRLF. Form opacity
and control-point opacity are separate fields. Stored editing does not establish
ArtPath rendering or vendor acceptance.

The existing `live2d.form.set_opacities` / `live2d_form_set_opacities` batch operation
also accepts these ArtPath forms alongside ArtMesh forms. Overwrite uses the same
unclamped finite native-float policy. Add accepts a finite delta and clamps each
computed sum to 0..1; repeated identities apply sequentially. Any later setter or
storage refusal leaves the whole batch unchanged, including its redo branch.
A successful batch is one Undo step; an unchanged candidate remains a no-op.

The source-form GUI switches between vertex and opacity drafts. Apply and Reload
use the same Session and existing save, replacement and close draft guards.
Changing edit mode or selection requires resolving the current draft; external
revision conflicts retain it. ArtMesh can switch between vertex and opacity editing;
ArtPath directly shows opacity without vertex or mode controls. A new source selection
starts in its default mode. Opacity keeps finite authored values outside `0..1`;
native float range and private storage validation remain authoritative in the backend.
Invalid projected opacity displays an error while valid stored settings remain available.

Applying opacity preserves pending form and source settings; applying either settings
half preserves the opacity draft. Their revision guards advance together. Main Apply
runs after widgets finish reading the current snapshot, avoiding a transient false
conflict against that snapshot. Reload discards panel drafts and loads the current
document on the next frame. The focused GUI group passes 24 tests, including assertions
immediately after each Apply invocation and in subsequent frames, actual numeric input,
coordinate-specific identity, history, save/reopen, no-op, storage refusal and stale reload.
The all-target GUI build and six consistency checks pass. The full suite exits 0:
82 groups, 2,887 passed, zero failed and six ignored tests. Source hashes stayed
unchanged during the run; the native-verified editor binary hash matches the final build.
An isolated macOS run also verified native numeric input, Apply, Undo/Redo, retention
and reversion of the two metadata drafts, changed-back opacity no-op, and clean Quit
on a synthetic fixture. Persistence used the existing MCP save/open tools; the reopened
ArtPath value was checked again in the native panel. An independent reader checks
whole-model equality, the sole opacity XML token change, protected container entries,
byte-exact Undo output and unchanged input. Native file dialogs were not exercised.
See `artpath-opacity-gui-native-result.json` under the current handoff evidence directory.
This bounded result does not change the earlier metadata attempts' INCONCLUSIVE status
or establish real-input coverage, rendering or vendor parity.

The opacity phase passes 12 real workflows (four models through FFI, JSONL and
stdio MCP) and 30 rejection/no-change controls in
`build/verification/native-authoring-opacity-9d7guuz5/result.json`.
The current vertex receipt is `native-authoring-edit-df8o5ye9/result.json`
(15 workflows, 74 controls). Source, weights and forms receipts are
`cmo-source-a0gsclgq`, `primary-weights-79a6yckg` and `primary-forms-szj66c9x`.
Those receipts identify the source and binary hashes used for their historical runs.
The expanded embedded HTTP receipt `live2d-http-12046-0/result.json` passes
four vertex and four opacity cases, with 65 public state reads.
The Rust suite passes 366 tests in `phase2-cmo-opacity-rust-suite-1.log`;
the D suite passes 304 automated cases in `phase2-cmo-opacity-d-suite-2.log`.
Both pinned macOS builds pass. Existing INX host-render verification passes
35 cases with zero pixel differences in `rust-host-render-0yjezac7/result.json`;
it does not verify native Live2D rendering.

The preceding transport phase used an actual native ACP vertex workflow:
`real-acp-native-oyi1fd4i` passes
with owned-process cleanup. The final verifier also replays that receipt and
the earlier supplemental-read receipt, rejecting five adversarial controls.
Every observation and the terminal snapshot must preserve all five public state
fields against the latest mutation. Additional scoped reads are allowed while
mutation order, arguments and immediate readbacks remain mandatory.
The three original ZIP archives remain unchanged. These results supersede the
earlier phase receipts above; foreground input and vendor reopening remain open.

Actual ACP opacity editing now passes in `real-acp-native-hb6yz8j3/acceptance.json`:
one pinned Hiyori Free ArtMesh opacity edit, exact stale-payload rejection,
Undo/Redo, save and reopen, with full models, weighted primary opacity, exact
XML outside the changed scalar and opaque-entry preservation. Its task permits
only that opacity operation; the existing vertex task cannot authorize it.
Both task guards run in deterministic regression tests, including finite values
outside zero to one, invalid types/revisions and cross-operation refusal.
The vertex workflow also passes with the new guard in `real-acp-native-a1wfixhl`.
Both actual runs record owned-process cleanup. The final verifier replays both
receipts and rejects seven adversarial controls, including non-integer stale
revisions, wrong models/opacity, supplemental query history and terminal history.
The pinned regression build and all 304 automated cases pass in
`phase2-real-acp-opacity-d-build-2.log` and `phase2-real-acp-opacity-d-suite-1.log`.
This adds actual-agent opacity evidence for one target; it does not establish
foreground GUI acceptance or full native model compatibility.

Actual macOS GUI source-form opacity verification passes in
`build/verification/native-gui-6umstm4l/result.json`. A copied Hiyori Free model
and separate settings folder exercise one `DeformerLocal` ArtMesh form through
draft-save refusal, Apply, Undo/Redo, scratch Save and GUI reopen. Observed
revisions are 1, 2, 3, 4 and 5; reopened opacity is `0.375`, with history disabled.
The independent reader checks the complete saved scene and exact XML/opaque
preservation, and both source copies remain unchanged. The task-owned saved
window closes normally and the app inventory reports it stopped.
`tools/prepare-native-gui-verification.py` (deleted with the D tree) reproduced the isolated bundle setup;
its prepared bundle also passes real GUI startup and normal close in
`native-gui-58my94jl`. The existing editor and user settings are not used.
Screenshots were observed through the native UI tool in the verification
conversation; the receipt records those observations and independent file checks.
This is one GUI workflow, not execution of the full 103-case inventory.
The actual GUI still displays `Native preview pending`; native display and
animated compatibility remain incomplete.

The current shared-session GUI/ACP workflow is retained in
`build/verification/native-gui-cxfq4pu9/acceptance.json`. The GUI Agent negotiates
ACP with the live native MCP endpoint, edits one Hiyori Free Canvas ArtMesh
opacity to `0.375`, and saves the active document to a scratch CMO3. GUI Undo
and Redo advance revisions 3 and 4; GUI Open then restores revision 5 with
history reset. Independent checks compare the complete saved XML scene, allow
only the selected opacity token to change, and preserve all container metadata,
opaque entries and the original. The reopened native authoring scene matches
the independent XML oracle within the established D JSON tolerance of `2e-15`.
Independent Spec and Standards reviews accept this bounded workflow.
The edit turn was interrupted through the GUI Stop button after the agent
repeated read-result extraction errors; the save turn completed. The adapter
log truncates the large opacity mutation response, so acceptance relies on the
exact approved payload, observed shared revisions and independent artifact
checks rather than a complete response-wire comparison. The GUI Save button is
not covered by this run. The app exits normally. Native preview remains pending,
and the small Agent panel still clips permission and prompt controls.

## CMO3 to INX conversion

`live2d.convert.inx {path?, animation?, resourceGuid?}` writes the active CMO3 as an INX model in the lossy
mode `doc/format-conversion-map.md` describes: the default pose is the node tree
and every native parameter is a one-axis INX parameter whose keys hold baked
vertex offsets. Single-parameter keys reproduce the CMO3 exactly on the four
samples; combined poses add in INX and differ (worst 38 to 3,062 px on the
Core capture's pseudo-random combined poses). With `animation`, the scenes of
that CAN3 become INX animations of frame-exact Linear keyframes sampled by the
CAN3 player; a Part with opacity curves becomes a 0..1 parameter with opacity
bindings and one lane per scene. Keyed ArtMesh draw order and opacity become
zSort and opacity bindings. The answer lists what was baked and what was not
converted.
