# Live2D animation (CAN3)

Status: dedicated production HTTP MCP proofs cover parameter-key insertion/deletion (`tools/verify-can3-key-structure.py`), parameter curves (`tools/verify-can3-curves.py`) and model tracks (`tools/verify-can3-tracks.py`) with synthetic fixtures. `tools/verify-acp-agent.py --families live2d-animation` (2026-10-21) also ran `live2d_animation_key_add` with `expectedRevision` and `undo` on a synthetic CAN3, as a scripted MCP client and by an agent over ACP, then rendered the saved animation attached to the synthetic CMO3 at the new key. Other entries retain their narrower tests. These proofs do not establish native GUI or vendor acceptance.

Read CAN3 scenes, attach them to an explicit CMO3 for bounded parameter and GUID-bound PartOpacity playback, add or delete parameter, modern PartOpacity, legacy-colon PartOpacity or underscore-map PartOpacity keys, parameter curves and model tracks, edit anchors, handles or segment types, and rename, retime or add a scene. INX animations are in animation.md; Motion3 clips are in motion3.md.

The CAN3 Outline provides a new-scene form and per-scene deletion through the same Session operations as
`live2d_animation_scene_add` and `live2d_animation_scene_delete`. The form starts with Scene, 30 FPS and 60 frames,
and remains available with no scenes. Edited form values must be added or discarded before saving, replacing or
closing the document. An invalid addition preserves the form; a dirty anchor draft blocks both structural actions.
Unsaved-change, restore and scene-deletion dialogs disable the form until answered. Stdin EOF leaves the window
open while a panel draft or dialog waits for the user.

GUI deletion names the exact scene and GUID and deletes all stored contents. Its displayed model-track and
curve-entry counts cover the known projected families. Confirmation
uses the revision captured when it opened. A concurrent edit disables confirmation until the user cancels and
reviews again. Cancel preserves the creation form; closing Outline does not hide the confirmation window.
The new-track form uses the selected scene and an existing linked-model resource. A sole resource is selected
initially; multiple resources require an explicit choice. This choice does not change the preview's runtime model
binding. An edited track form keeps its original scene and revision; a concurrent edit or different scene selection
disables Add until the user discards and reviews it. Add starts at frame zero with the scene duration and no curves.
No resource or model file is created. Track drafts use the same save, replacement, close and EOF guards as scenes.

Model-track deletion displays the exact scene/track GUIDs, track name, resource GUID and the captured counts for all four
curve arrays. It requires a fresh revision-bound confirmation; cancel changes no document data. The confirmation
window remains independent of Outline. Undo/redo and native save/reopen use the existing Session operations.
The track confirmation deletes all stored contents; its displayed curve counts describe the known projected
parameter/Part families. Owned inherited/grouped model tracks can also contain visual, lip-sync and eye-blink
effects. Deletion inventories their float, integer and XY attributes, including fixed-value sequences and
unmapped members, before changing the document. The exact parent-group child entry and track identity are
removed; multiple unsaved deletions update that group's count together. Shared model, parent, parameter,
Part and parameter-group identities remain. Recognized unowned parameter declaration stubs remain as opaque
metadata. Unknown effect/attribute/sequence forms, malformed or aliased memberships, nested shared definitions
and outside references to removed storage are refused atomically. The local pinned Ren track proof checks
history and exact XML/CAFF preservation after save/reopen.

Image tracks appear separately in each scene's `imageTracks` with `guid`, `name`, `resourceGuid`, `start`,
`internalOffset`, `duration` and `visualCurves`. Their Image rows use the same deletion operation and revision-bound confirmation,
show the retained image-resource GUID, and omit model-curve counts. Owned VisualDefault attributes and the exact
source/group members are removed; linked image resources remain. Blank Image creation, metadata editing and
playback are unsupported. Missing `imageTracks` never means delete-all. Referenced map keys and outside references
remain refused. Read the animation-source resource after deletion, then undo/redo or save/reopen as needed.
`tools/verify-can3-image-tracks.py --core <core> --njc <njc>` checks an authored Image fixture; optional
`--native-can3 <path>` checks the pinned Hiyori project. It compares the exact XML deletion spans, CAFF
metadata/non-main entries, undo restoration bytes, and saved/reopened model. It is not a vendor GUI check.

Image and Guide `visualCurves` list supported `scalex`, `scaley`, `rotate`, `shear`, `opacity` and `artPathWidth`
channels with `id`, `points` and `curveTypes`, including empty sequences. Unsupported or malformed channels
are omitted; budget exhaustion fails the read. Use `live2d_animation_key_set` with an existing anchor frame
and the exact raw ID to edit its value. Guide requires the complete supported ownership and empty-map layout,
with consistent inline key/cache storage. `live2d_animation_key_add` and `live2d_animation_key_delete` also
accept Image/Guide channels. Added keys use the requested type (default `LINEAR`) and controls at the anchor.
Deleting the final key retains the immutable template's extrema and base value. Image channels reject
`partOpacityMapKey`; Guide channels reject it too. Image handle and existing interpolation-type edits, and integer-channel edits, remain unsupported.
Guide scalar handles/types use the existing tools without a Part selector and require complete supported
ownership. Only requested control frame/value leaves or stored type change, with cache invalidation.
Integer positions, optimization/corner flags, anchors/extrema and other channels stay unchanged.
Recreated and new keys retain canonical protected fields after later handle/type edits. This is stored-field
authoring; the four vendor inputs have one scalar key per channel and do not prove multi-key interpolation
or Guide playback equivalence.
Read back, undo/redo and save/reopen.

Image and Guide `pointChannels` list readable `xy` and `anchor` channels with `id` and
`points: [{frame, x, y, corner}]`. `live2d_animation_point_set` edits both coordinates
of an existing point. Supply scene/track GUID, raw channel ID, signed `anchorFrame`,
`x`, `y` and current `expectedRevision`. Coordinates round to finite f32; equivalent
values keep their original tokens and add no history. Frames, corners, base points,
other channels and unrelated bytes stay unchanged. Shared or inconsistent storage
is refused. `live2d_animation_point_delete` removes one existing point using those
selectors and revision, without x/y. The channel remains readable when empty.
Deletion preserves the base point, retained point order and unrelated bytes, and
requires the verified inline XY sequence layout with explicit matching count/type.
Unknown structural fields or shared storage are refused. Read back, undo/redo and
save/reopen. `live2d_animation_point_add` requires an unused signed-i32
`anchorFrame`, both finite-f32 coordinates and explicit boolean `corner`, along
with scene/track/channel selectors and revision. It supports empty channels and
inserts numerically only into increasing stored/requested frames with the verified
private record shape. Unknown fields, metadata or shared storage are refused.
Same-count delete/add changes use frame identity; an original frame can be
canonically recreated with a different corner under the strict structure guards.
Recreating the original frame with its original corner is a coordinate edit or no-op;
the original record, including unknown metadata, is preserved.
Read back, undo/redo and save/reopen. Playback equivalence remains unproven.
An empty stored attribute map is supported for point channels only when the other
three ownership references are exact. Populated Image maps must contain the matching entry. Guide point edits require the complete supported
Guide ownership layout, including both effects and distinct image-number attributes.

Read `imageResources` for linked image GUIDs and optional stored names/paths. Paths are literal and never opened;
these records remain read-only. `live2d_animation_track_duplicate` copies an existing Image, supported Guide or supported Sound track in the same scene
and parent group, preserving its timing, keys and caches while allocating private identities. Guide copies preserve
both distinct image-number attributes and the linked directory resource without opening the directory.
Sound copies require one SoundDefault effect with fixed volume and clone four private definitions, preserving
speed, default volume and volume caches without opening audio paths.
Copies of unsaved copies work, and deleting a donor does not remove its copies. Other effects, shared mutable
storage and unknown references are refused. Read back the new `trackGuid`, then undo/redo or save/reopen.

Scene deletion supports Model, Image, Sound and Guide tracks accepted by their existing ownership checks.
It validates each direct source-list member and complete group child membership, reusing track
ownership checks. It removes the owned scene, root group and track contents together, including an empty group
left after its last track was deleted and saved. Shared group/resource identities and other scenes remain.
Unknown children, outside references and aliased or nested shared storage are refused atomically. If the stored
`currentScene` points to the deleted scene, it selects the next surviving original scene, then the previous one,
or the first newly added scene using its allocated storage identity. An already surviving selection stays unchanged.
A file with `currentScene` and no surviving or new scene is refused: no native empty-selection encoding is verified.
The resource manager's `_sceneSource` follows the same replacement rule on deletion and moves to the last added
scene on `live2d_animation_scene_add`, as every supplied Cubism file keeps it on its last scene.
Files without that field retain the generated-fixture last-scene deletion behavior. The pinned Ren scene proof
checks the 149-definition closure, selected-scene reference, history and exact XML/CAFF save/reopen; it does not
establish Cubism or native-window acceptance.

The new-curve form targets the selected scene and track. Parameter and modern PartOpacity creation require a
CMO3 explicitly attached to that track's resource GUID; the attached path is passed to the existing curve-add
operation. Parameter choices come from the attachment. PartOpacity choices are loaded only when the user clicks
Refresh Part choices, through `live2d_authoring_read` on that attached path; explicit-path reads work while CAN3
is active without replacing its document. Read the returned Parts (`sources` whose `data.type` is `Part`) for the
equivalent agent operation. New PartOpacity curves require a nonempty Part ID without `:`. Stored `sourceFile`
paths are never opened by this form. Part choices are cleared on attachment changes, including reattachment at
the same path through the GUI or MCP, and document replacement. Reattach after external
parameter changes; refresh Part choices after external Part changes. The add operation rereads and validates the
chosen ID. Edited curve kind/target values join the existing draft guards, freeze their scene/track/revision and
attachment, and must be added or discarded. Failure preserves the form; a stale selection or attachment disables Add.

Every existing curve family has a deletion control, independent of CMO3 attachment. The confirmation captures
the scene/track GUIDs, curve ID, family, exact underscore map key when present, key count and revision. Underscore
headers include their map keys so duplicate IDs remain distinguishable. The shared delete operation still refuses
ambiguous or unsupported source ownership. Cancel preserves drafts; a concurrent change disables confirmation.
All scene, track and curve deletions share one pending confirmation state and the same close/replacement guards.
These interaction checks use Rust egui tests; native-window and Cubism acceptance remain separate.

The curve row's New key action opens one revision-bound form with frame, value and type (default `LINEAR`).
All four curve families support this operation without CMO3 attachment. Frames are signed 32-bit integers and
values must be finite; neither scene duration nor the authored channel range clamps them. Duplicate frames and
unsupported source ownership remain shared Session refusals, preserving the form. Add or discard this form
before saving, replacing or closing the document. A concurrent document edit requires discard and review again.
Each key row also has direct Delete key with shared Undo; deleting the last key leaves the curve empty.
Underscore operations pass the stored map key exactly. An existing dirty anchor draft blocks both mutations;
other open creation drafts remain intact and become stale when the document revision changes.

## Sequence

1. Explore: `live2d_animation_read {path}` reads CAN3 scenes, Model/Image/Sound/GuideImage tracks and supported keys without changing the session; with a CAN3 open, `resource://soul2d/document/animation-source` shows the stored keys.
   `live2d_animation_list {}` lists the active scenes and the model attachment: `{animationPath, modelName, modelPath, resourceGuid, scenes}`, all null or empty before attaching.
2. Attach: `live2d_animation_attach {path, resourceGuid?}` (CAN3 onto the open CMO3) or `live2d_model_attach {path, resourceGuid?}` (CMO3 onto the open CAN3). Linked paths are never followed; parameters use exact GUID and `live2dParam_` ID, and PartOpacity uses exact Part GUIDs. `live2d_model_attach` also takes a runtime `.model3.json`: its `.moc3` keyforms and the textures it names (inside its folder) become the preview model, with the parameter and Part GUIDs the CAN3's curves carry, and Parts the CAN3 animates but the export dropped as empty Parts. A bare `.moc3` is refused. Glue, blend shapes and keyform colours (versions 5 and 6) and offscreen Parts (version 6) are read as Glue, morph targets, tints and offscreen Parts. `warnings` lists blend shapes left out for lack of a default keyform or parameter.
   `live2d_animation_bindings {modelPath, resourceGuid, path?}` reports parameter and Part matching without changing the session. For underscore channels read `underscorePartBindings` and `unboundUnderscorePartCurves`, including `mapKey`, raw `curveId` and `sourcePart`. Matched rows also include canonical `partId`. Confirm sampled `nativePlayback.active.poseSignals` and `partOpacities` in the snapshot; Part IDs are not ordinary parameter values.
3. Preview: `live2d_animation_seek {sceneGuid, frame}`, `live2d_animation_advance {dt, loop}`, `live2d_animation_reset {}`. Previews are session values, not history.
4. Act: `live2d_animation_key_set {sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, value, expectedRevision}` edits one existing anchor in one undo step; it needs `revision` read just before.
   Stale revision answers "CAN3 key edit revision conflict" (source text, not run).
   `live2d_animation_handle_set {sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, side, frame?, value?, expectedRevision}` moves the `previous` or `next` control handle of one anchor; `live2d_animation_curve_type_set {sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, type, expectedRevision}` sets the type (`SMOOTH`, `BEZIER`, `LINEAR`, `STEP`) of the segment that starts at that anchor. Each is one undo step; a stale revision answers "CAN3 key edit revision conflict".
   `live2d_animation_scene_set {sceneGuid, name?, fps?, durationFrames?, loop?, tag?, expectedRevision}` renames or retimes one scene, sets its stored loop flag (only where the snapshot shows `scene.loop`), or sets its stored tag text (only where the snapshot shows `scene.tag`; an empty `tag` clears it); `live2d_animation_scene_add {name, fps, durationFrames, expectedRevision}` adds an empty scene at the end and answers its new `sceneGuid`; where the document's last scene shows `scene.tag`, the new scene shows a copy of it, because save copies a stored scene, tag included. Each is one undo step; a stale revision answers "CAN3 scene edit revision conflict".
   `live2d_animation_key_add {sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, value, expectedRevision, type?}` inserts one parameter, modern PartOpacity, legacy-colon PartOpacity or underscore-map PartOpacity key. The new frame is a signed 32-bit integer; the value is finite; `type` defaults to `LINEAR` and also accepts `SMOOTH`, `BEZIER` or `STEP`. Read identities and revision from the current source and state resources.
   `live2d_animation_key_delete {sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, expectedRevision}` deletes the existing key at that signed frame. Each accepted call is one undo step. Read `resource://soul2d/document/animation-source` after the call, then save and reopen to confirm persistence.
   Add a parameter or modern PartOpacity curve with live2d_animation_curve_add; delete a parameter or any PartOpacity family with live2d_animation_curve_delete. Both require the current expectedRevision. Addition takes modelPath and exactly one of parameterId or partId.
   For addition, provide sceneGuid, trackGuid, modelPath and exactly one of parameterId or partId. modelPath is the source CMO3 used to read the target GUID and parameter range. New Part curves use typed CPartId identity and range [0,1]. The new curve starts without keys and stores the parameter's default as `baseValue` (shown as `curve.baseValue` only where a curve stores one; a re-added original keeps the value it stored, and an edit of it is refused). Read its curveId from resource://soul2d/document/animation-source before adding keys.
   If an owned existing Parts effect has no member list, modern addition creates a list for the new modern members only, preserving absent legacy/underscore membership, existing map entries and effect registration. The insertion owner must be the effect itself or inline unshared ICMvEffect with the matching track backlink.
   An empty unmapped attribute with matching ID, GUID, source identity and range is reused instead of duplicated. Its existing member stays in place; only missing map/member references are added. With an absent/null effect, a detached attribute may have only its sequence backlink, and the new effect lists reused and fresh attributes in curve order. Conflicts, multiple candidates, stored keys or outside owners are refused. Later unsaved key edits use the same storage and preserve its opaque fields.
   For deletion, provide sceneGuid, trackGuid, curveId and optional partOpacityMapKey. A supplied selector chooses only the exact underscore map key; duplicate curve IDs require it and invalid selectors never fall back. Deletion removes the owned map entry, optional mapped member and attribute, preserving siblings, the effect and its registry. Shared, aliased, nested shared or externally referenced removed storage is refused. Re-adding an original curve requires matching source identity, and resets its keys while retaining supported original metadata and opaque fields.
   `live2d_animation_resource_add {modelPath, expectedRevision}` links a readable CMO3 as a new resource and answers its `resourceGuid`; save copies the file's own resource entry, so a CAN3 without one refuses it. `live2d_animation_track_add {sceneGuid, resourceGuid, name, expectedRevision}` adds one empty model track bound to an existing linked-model resource; the answer carries its new `trackGuid`, and the track starts at frame 0 with offset 0 and the scene's duration. `live2d_animation_track_delete {sceneGuid, trackGuid, expectedRevision}` deletes an owned model track and its owned parameter and modern, legacy-colon and underscore-map PartOpacity definitions. Exact raw map identities are used; modern members are required and mapped members may be absent or one bare reference. Unowned map/list entries, shared mutable or nested shared storage, and outside references are refused. Inline unshared Part inheritance is supported. Each is one undo step; a stale revision answers "CAN3 track edit revision conflict". An added track or a newly added scene can take curve and key edits before the first save.
   Read the snapshot and animation-source resource after each operation. Refused operations must leave revision, history, model and source files unchanged. Save and reopen to verify persistence.
   Track moves, scene reordering, image resource creation and legacy/underscore PartOpacity curve creation and reordering are outside its scope. Native GUI curve creation/deletion is unverified and Cubism vendor acceptance is NOT_RUN.
5. Confirm: read `animation-source` again; `history_summary`; `save` writes the CAN3 (session.md).

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `live2d_animation_read` / `Live2DCommand_AnimationRead` | `{path}` read only. |
| `live2d_animation_list` | `{}` read only. |
| `live2d_animation_bindings` / `Live2DCommand_AnimationBindings` | `{modelPath, resourceGuid, path?}` read only; undeclared arguments are refused. |
| `live2d_animation_attach` | `{path, resourceGuid?}` |
| `live2d_model_attach` | `{path, resourceGuid?}`; `path` is a CMO3 or a runtime `.model3.json` |
| `live2d_animation_seek` | `{sceneGuid, frame}` unsupported curve tokens or conflicting tracks are errors. |
| `live2d_animation_advance` | `{dt, loop}` looping uses the scene frame duration. |
| `live2d_animation_reset` | `{}` clears only the session preview values. |
| `live2d_animation_key_set` / `Live2DCommand_AnimationKeySet` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, value, expectedRevision}` |
| `live2d_animation_point_set` | `{sceneGuid, trackGuid, curveId, anchorFrame, x, y, expectedRevision}` edits existing Image or supported Guide `xy` or `anchor` points. |
| `live2d_animation_point_delete` | `{sceneGuid, trackGuid, curveId, anchorFrame, expectedRevision}` deletes one existing Image or supported Guide `xy` or `anchor` point; preserves an empty channel. |
| `live2d_animation_point_add` | `{sceneGuid, trackGuid, curveId, anchorFrame, x, y, corner, expectedRevision}` adds an explicit point to a supported Image/Guide channel with increasing frames. |
| `live2d_animation_key_add` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, value, expectedRevision, type?}` inserts a parameter, modern PartOpacity, legacy-colon PartOpacity or underscore-map PartOpacity key; omitted `type` is `LINEAR`. |
| `live2d_animation_key_delete` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, expectedRevision}` deletes an existing parameter, modern PartOpacity, legacy-colon PartOpacity or underscore-map PartOpacity key. |
| `live2d_animation_curve_add` | `{sceneGuid, trackGuid, modelPath, parameterId? or partId?, expectedRevision}` adds an empty parameter or modern PartOpacity curve. Select exactly one ID. A parameter curve stores the parameter's default as `baseValue`. |
| `live2d_animation_curve_delete` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, expectedRevision}` deletes a parameter or any PartOpacity curve. Exact underscore map keys disambiguate duplicate attribute IDs; a supplied selector never falls back. The owned map entry, optional mapped member and attribute are removed together. |
| `live2d_animation_resource_add` | `{modelPath, expectedRevision}` links a CMO3 as a new resource and answers its `resourceGuid`; with `track_add` this places a model on the canvas. |
| `live2d_animation_image_resource_add` | `{imagePath, expectedRevision}` validates a bounded static PNG/JPEG and links its canonical path and basename as a new Image resource using a stored owned donor. Returns `resourceGuid`; one undo step. Does not embed the image or create a track. Unsupported donor references, custom names and ambiguous memberships are refused. |
| `live2d_animation_image_resource_relink` | `{resourceGuid, imagePath, expectedRevision}` validates a static PNG/JPEG and changes only an existing Image resource's stored path. Every track sharing the resource uses the link. Keeps name, keys and transforms; one undo step, matching path is a no-op. Requires a plain unshared stored path. Does not resize or recenter. |
| `live2d_animation_track_add` | `{sceneGuid, resourceGuid, name, expectedRevision}` adds an empty model track. |
| `live2d_animation_track_duplicate` | `{sceneGuid, trackGuid, name, expectedRevision}` copies an owned Image, supported Guide or supported Sound track, preserving its stored keys, caches and linked resource. Returns the new `trackGuid`. |
| `live2d_animation_track_delete` | `{sceneGuid, trackGuid, expectedRevision}` deletes an owned Model, Image, supported Sound or supported GuideImage track, retaining linked resources and shared identities. Read IDs from `tracks`, `imageTracks`, `soundTracks` or `guideTracks`. Local FormEdit data supports distinct shared keyforms and frames with matching time-remapping links and caches; empty inline keyforms with exclusively owned dummy remapping attributes and sparse maps are also supported. Foreign parameter-group frame-step attributes are preserved; same-owner attributes use ordinary ownership checks. Global maps, nonempty inline keyforms and outside references into deleted storage are refused. Sound requires one SoundDefault effect and one fixed volume attribute. GuideImage requires VisualDefault and GuideImageDefault with eleven owned attributes, including separate list and named image-number attributes. Guide directories are never opened. Sound/Guide creation and playback, Sound key/volume edits remain unsupported. |
| `live2d_animation_handle_set` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, side, expectedRevision}` plus at least one of `frame`, `value`; Model or supported Guide scalar only. Guide rejects the Part selector. |
| `live2d_animation_curve_type_set` | `{sceneGuid, trackGuid, curveId, partOpacityMapKey?, anchorFrame, type, expectedRevision}`; Model or supported Guide scalar only. Guide rejects the Part selector. |
| `live2d_animation_scene_set` | `{sceneGuid, expectedRevision}` plus at least one of `name`, `fps`, `durationFrames`, `loop`, `tag` |
| `live2d_animation_scene_delete` | `{sceneGuid, expectedRevision}` deletes an owned scene with supported Model, Image, Sound and Guide tracks, including parameter curves, all three PartOpacity families and supported shared-keyform FormEdit data. Unsupported raw members and shared, aliased, nested shared or externally referenced target storage are refused; inline unshared Part inheritance is supported. Unrelated surviving scenes are preserved. The last scene may be deleted when no stored `currentScene` needs a surviving target; otherwise deletion is refused. Linked model, image, sound and guide-directory resources remain. Guide tracks use the same strict layout checks as track deletion. |
| `live2d_animation_scene_add` | `{name, fps, durationFrames, expectedRevision}`; where the last scene shows `scene.tag`, the new scene shows a copy of it. |

## Gotchas

- Verified: `live2d_animation_list {}` on an INX document is not an error; it returns `scenes:[]` and null attachments.
- Playback is bounded native parameter and PartOpacity playback; it does not claim Cubism equivalence. Runtime `values` are empty for a bare CAN3.
- An existing anchor-value edit changes the stored value and its extrema caches only. Controls, frame caches and unknown data stay as they were; shared scalar storage is rejected.
- A handle edit rewrites only the handle's `posF` and `doubleValue` on save. Its integer `pos`, `isPosOptimized` and every other stored field stay as they are. Cubism Editor may place a handle whose `positionOptimized` is true again itself (guess, not run). A `SMOOTH` or `BEZIER` segment whose frames are in order (anchor, its `next`, the following `previous`, the following anchor) must stay in order, or the edit is refused; this also refuses setting such a type on a segment whose handles are out of order. A segment a file brought in out of order can still be edited. The last key starts no segment, but its stored type can be set. Soul2D plays `SMOOTH` and `BEZIER` the same way. The five key operations accept a sole matching `curveId` across `parameterCurves`, modern `partOpacityCurves`, `legacyPartOpacityCurves` and `underscorePartOpacityCurves`. For underscore channels read the stored `mapKey` and pass it as optional `partOpacityMapKey`; it is required when the attribute ID is duplicated. The exact key and attribute ID must both match; a supplied empty, malformed or wrong selector never falls back. Parameter, modern and legacy selectors omit it. Legacy and underscore curve creation/reordering are unsupported; individual deletion uses the exact selector and ownership checks described above. Underscore playback uses exact Part GUID binding to the explicitly attached CMO3 and scene/track/mapKey identity; duplicate attribute IDs remain distinct. Empty PartOpacity sequences produce no write; multiple populated writers for one Part are refused. Read underscorePartBindings and unboundUnderscorePartCurves before playback; unmatched GUIDs are warned about and left unapplied. The five key operations keep PartOpacity membership, order, identity, source metadata and range unchanged. Writes require an owned map/sequence reference set and the modern owning member. A legacy or underscore member may be absent or be one bare reference; its member list is never synthesized. Raw legacy and underscore map, attribute and source identities stay independent. Underscore source identity may coexist with opaque option fields, which remain unchanged. Mutable storage must be inline and unshared; inline inheritance is supported. Additional owners, aliases or shared sequences are refused.
- A scene edit changes only the scene's name, frame rate, length, stored loop flag and stored tag elements on save (an empty tag is written as a self-closing element), and a setting stored shared (`xs.ref`) is refused. Tracks are not moved or cut to a new length.
- A scene starts with an empty track list. Tracks, curves and keys added before its first save are serialized from the final model together. Scene element names are copied from the first scene, with bounded synthetic fallbacks when none exists. Cubism Editor 5.3.04 opened a scene copied from a stored one, with an added track, curve and keys, on 2026-10-20; it has not opened this minimal scene. Scene add/delete GUI controls have bounded Rust interaction checks; track add/delete controls have the same bounded checks; curve creation/deletion controls now have bounded Rust checks; key insertion/deletion controls have bounded Rust checks for all four families.
- Key insertion/deletion requires inline point/type lists, strictly increasing anchors, supported curve types and valid native double extrema caches. Shared or referenced slots and edits that would make an affected Bezier segment invalid are refused. The affected current segments must already be valid. A prior handle repair does not require saving or reopening. No neighboring handle is silently moved.
- A new key has `corner: true` and both controls at the anchor, with matching integer frames, the same value and `positionOptimized: false`. Readding an original frame resets those known fields without dropping its unprojected XML payload.
- An empty candidate keeps the original save template's `curMin` and `curMax` tokens. It does not remember a previous unsaved candidate's derived extrema. Inserting into an empty curve sets both extrema to the new value. This is a local source contract, not a vendor acceptance claim.
- The dedicated key-structure proof covers state/resource equality, refused-call atomicity, one-step history, accumulated edits, saved/reopened values and unchanged unrelated XML/CAFF bytes. Parameter curves and model tracks have separate structural proofs. Separate scene, track, curve and key GUI controls use the same Session operations; track moves, scene reordering, resource creation and legacy PartOpacity curve creation remain unsupported.
