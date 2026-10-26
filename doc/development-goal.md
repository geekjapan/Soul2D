# Development goal

Soul2D aims to provide a Rust application that loads, displays, edits,
animates, and saves models through a desktop UI and automation interfaces.

The project is incomplete. This repository publishes source.
It does not provide a binary distribution or certify release readiness.

## Shared document ownership

The Rust Session owns the document, revision, and Undo/Redo history.
The GUI, CLI, MCP, and ACP integration must operate on that shared state.
They must not maintain competing editable documents or histories.

A completed workflow must show agreement after edits, Undo/Redo, save,
and reopen. Startup, parsing, or an isolated transport check is insufficient.

## Intended outcomes

| Area | Required outcome |
| --- | --- |
| Core | Rust owns persistent state, transactions, evaluation, rendering, and file operations. |
| Display | Supported hierarchy, geometry, textures, materials, masks, and deformation produce correct output. |
| Editing | Geometry, parameters, animation, and controls share history and reject conflicting edits. |
| Persistence | Save/reopen preserves supported edits and unrelated unknown data. |
| Animation | Seeking, playback, loops, and edited curves produce the specified results. |
| Conversion | Declared format mappings preserve semantics or explicitly refuse unsupported conversion. |
| Desktop UI | Real input workflows cover editing, playback, persistence, recovery, and lifecycle behavior. |
| Automation | MCP, an actual ACP client, and an operating skill can complete shared-session workflows. |
| Compatibility | Scoped native formats and features pass comparisons against pinned legitimate references. |

These outcomes are acceptance requirements, not completed-phase declarations.
Existing implementations and focused tests establish only their observed scope.

## Live2D compatibility

The intended compatibility target is Cubism 5.3 Pro, subject to the documented
[acceptance scope](acceptance-scope.md) and exclusions.

CMO3 and CAN3 authoring, runtime assets, and INX conversion require distinct
checks. Reading a runtime model does not establish native authoring support.
Preserving opaque bytes does not establish semantic conversion.

Features absent from available fixtures require dedicated probes.
Private model archives and historical verification outputs are not assumed
available in this source repository.

## Completion evidence

Acceptance requires identified source revisions, reproducible checks,
explicit input identities, and observed results.
Unsupported and unexecuted checks remain gaps.

Vendor comparisons also require pinned reference artifacts, environment,
settings, and comparison criteria.
The proposed [comparison contract](vendor-comparison-contract.md) is not
a completed compatibility verdict.

Current contracts are summarized in [Rust core](rust-migration.md),
[depth and bones](rust-migration-bone-depth.md), and
[Live2D authoring](live2d-authoring.md).
