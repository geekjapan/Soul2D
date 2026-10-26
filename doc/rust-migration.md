# Rust editing core

Soul2D is an incomplete Rust application published as source.
This document describes the current ownership and persistence contracts.
It does not certify complete editor behavior or Cubism compatibility.

## Ownership and failure contract

`Session` in `rust/src/lib.rs` owns the editable document, revision, and
Undo/Redo history. Runtime values, clipboards, and previews also belong to
the session, but are not all persistent document data.

The Rust editor, JSONL host, MCP hosts, and C ABI call this core.
The editor's `EditorController` owns its Session on the UI thread.
Its MCP adapter forwards requests to that same owner.
An external ACP agent operates through the editor's MCP connection.

Document edits prepare and validate a candidate before committing it.
Failed validation and unsupported edits must leave the document and its
history unchanged. Revision checks reject stale native authoring edits.
GUI drafts retain their selection revision and reject stale Apply operations.

Persistent edits share one history. Runtime parameter changes and deformation
previews do not create persistent history entries.
Opening a document prepares a replacement before changing the active session,
then resets document history and transient document state.

## Persistence and unknown data

INX saves retain unknown JSON fields semantically and preserve opaque
container data. Supported native CMO3 and CAN3 edits preserve data outside
their intended source changes, including opaque container entries.

Save checks whether the active file changed or disappeared since it was read.
A different existing destination is refused.
Serialization and pending deformation work are staged before publication.
Save writes and syncs a temporary file in the destination directory, then
renames it into place. Session save state changes after successful publication.

Preservation is specific to each operation and format.
Keeping unknown bytes does not establish support for their meaning.
Format conversion and export have separate loss and refusal rules.

## Implemented areas

The core includes INX node and parameter editing, mesh and deformer operations,
depth maps, depth rigs, animation data, history, rendering, and file operations.
The command manifest describes registered tools and their argument schemas.

Native Live2D support includes CMO3 and CAN3 readers, bounded authoring edits,
source evaluation, runtime previews, and animation operations.
CAN3 scene operations include `live2d.animation.scene.add`,
`live2d.animation.scene.set`, and `live2d.animation.scene.delete`.
These operations do not establish full project or vendor equivalence.

The optional `soul2d-editor` binary provides the Rust desktop UI.
Draft guards, file replacement guards, recovery, and playback state belong to
the same editor session rather than a second mutable document.

## Native preview controls

The Rust editor uses the core's native rendering, parameter, expression,
and timeline operations. Unsupported source features can reject evaluation.
Approximation warnings must remain visible.

Open a CMO3 file, then explicitly attach its matching CAN3 file.
Alternatively, open CAN3 first and attach its CMO3.
Stored source paths are metadata and are not followed automatically.
Projects with multiple linked models require an explicit resource selection.

The native controls select a scene, seek by frame, and play or pause with
optional looping. Runtime parameters and imported expressions affect the
preview. CMO3 also exposes imported Motion3 playback.

Attachments, expression layers, and preview values are session state.
Saving the authoring document does not bake preview values into its keyforms.
`Fit preview` explicitly updates camera framing.

See [Live2D authoring](live2d-authoring.md) for the supported operations and
their limitations.

## Deformation and rendering

[Depth and bone contracts](rust-migration-bone-depth.md) describe persisted
depths, rig bindings, generated offsets, and continuation behavior.

CPU deformation results remain authoritative.
The deformation probe can compare a software implementation or a registered
OpenGL callback against those results. A software comparison does not prove
device execution or GPU correctness.

Rendering support and refusals depend on the document and node features.
A successful render of one fixture does not establish every material,
mask, deformation, animation, or native authoring combination.

## Verification boundaries

Source code and test coverage describe implementation, not current execution
results. A fresh acceptance run must identify its revision, inputs, platform,
commands, and observed results.

Core tests, transport tests, native GUI workflows, actual ACP-client workflows,
save/reopen checks, and vendor comparisons establish different facts.
An omitted, unsupported, or unexecuted check is not a pass.

See [BUILD.md](../BUILD.md) for local build and test commands,
[development goal](development-goal.md) for the intended outcome, and
[acceptance scope](acceptance-scope.md) for compatibility boundaries.

This source publication includes no binary distribution or release-readiness
claim.
