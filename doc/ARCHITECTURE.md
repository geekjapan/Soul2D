# Soul2D architecture

Status: Migration-era contract, updated 2026-10 for the Rust clients (`soul2d-core`, `njc`, `soul2d-editor`).

> Written during the D-to-Rust migration. The D editor, its OpenGL host and the "Rust mode"
> projection described below were deleted on 2026-10-12; read every mention of D as history.
> The contract itself (principles P1 onward) still applies to the Rust clients.

This document freezes the contract for how agents, CLI tools, MCP, ACP, and the GUI interact with puppet document state during the Rust authoritative-core migration.

## Principles

**P1. No GUI-only mutation.** Persistent document changes must go through the shared command path. Editor panels, dialogs, and viewport drags (egui, `soul2d-editor`) are views and controllers; they must not be the only way to apply an edit that agents or headless hosts need.

**P2. Core owns state.** The Rust editing core (`Session` in `rust/src/lib.rs`) owns the canonical document, persistent history, and revision. The GUI is a client; it must not hold a second authoritative copy of model state. (During the migration, D objects were projections of that session; the D editor is gone.)

**P3. Commands are semantic.** Mutations are expressed as typed, named commands with explicit inputs and documented effects—not ad hoc GUI callbacks or opaque widget state. See [commands.md](commands.md) for the command surface and effect vocabulary.

**P4. Every mutation is transactional.** A failed or rejected command leaves document state, runtime values, history, and revision unchanged. Partial application of a multi-step edit is not exposed to clients.

**P5. Every result is observable without Computer Use.** Verification and agent workflows must be able to confirm outcomes through semantic reads (resources, snapshots, revision) and rendered output (PNG, MCP image content) without driving the OS GUI. Computer Use is optional for human GUI-only scenarios, not the default proof path.

## Target architecture

All clients sit above the same typed buses. Document state lives below them in Rust.

```text
                    +----------+  +----------+  +----------+  +----------+
                    |   MCP    |  | Agent   |  |   CLI    |  |   GUI    |
                    | client   |  | via MCP  |  | (soul2d- |  | (soul2d- |
                    | (njc)    |  |  / ACP   |  |  core)   |  |  editor) |
                    +----+-----+  +----+-----+  +----+-----+  +----+-----+
                         |             |             |             |
                         +-------------+-------------+-------------+
                                              |
                              +---------------v---------------+
                              |      CommandBus / QueryBus     |
                              |  (typed commands & queries)    |
                              +---------------+---------------+
                                              |
                              +---------------v---------------+
                              |     Rust authoritative core    |
                              |  document · history · revision |
                              +--------------------------------+
```

`CommandBus` and `QueryBus` name roles, not Rust types: no type of those names exists in `rust/src`. The entry points
are `Session::handle_request` (one JSON object `{"op": ...}` in, `{"succeeded", "status", "message", "result"}` out) and
`Session::handle_mcp` (JSON-RPC tools and resources). See [command_palette.md](command_palette.md) for the result shape.
The D reference oracle that used to sit below the core was removed with the D source on 2026-10-12.

- **CommandBus** carries semantic mutations (create, rename, transform, bind, undo, save, …).
- **QueryBus** carries reads (parameters, nodes, bindings, snapshots, render requests).
- **MCP** maps JSON-RPC tools and resources onto the buses.
- **ACP** connects the GUI client to an external agent for prompts, updates, and permission requests. The agent accesses the active Rust session through the existing MCP server; ACP does not create a second document or history.
- **CLI** (`soul2d-core`: JSONL on stdin, `--mcp` stdio MCP, `--mcp-port` HTTP MCP) and **GUI** (`soul2d-editor`, egui,
  which also takes `--mcp` and has an optional HTTP MCP listener in `mcp_listener.rs`) drive the same `Session` type as
  MCP. `njc` is the MCP client of the CLI.

## Agent and MCP workflow

Clients follow **Explore → Read → Act** as defined in [mcp_api.md](mcp_api.md):

1. Explore with `resources/list` or `resource://…/resources/find?selector=…`
2. Read with `resource://…/resources/{uuid}` or binding resources
3. Act with `tools/call` for mutations

Resources observe; tools mutate. Do not use GUI-only paths for persistence when a tool or CLI request exists.

## Present state (landed Rust subset)

The migration is incremental. Today, `rust/` already owns the following for the migrated subset (details and evidence in [rust-migration.md](rust-migration.md)):

- Parameter and node editing, TRS bindings, structural conversion, and clipboard operations
- Grid/Path/MeshGroup geometry, depth fields, depth-bone bindings, and deformation refresh
- Shared persistent history, runtime parameter values, and separate physics state
- Open and save of INP/INX with opaque tail and texture preservation
- Headless PNG rendering with masks, Composite layers, and evaluated scene geometry
- Read-only evaluated GPU packets (`render_packet`) and evaluated scene geometry for previews and capture
- Bounded CAFF container reading/writing, CMO3/CAN3 authoring inspection, native CMO3 session/history/save,
  and scoped primary source-form vertex edits; model evaluation and conversion remain incomplete
- CPU deformation, with a headless software kernel that is checked against it (Phase 4 P4-G1 in [adr/0001-rust-authoritative-core.md](adr/0001-rust-authoritative-core.md))
- MCP routing that rejects unsupported edits for the active document format instead of applying them

The editor exposes a smaller subset than the core and MCP adapters.
Advanced GUI authoring, remaining scene parity cases, and complete Live2D load/edit/write are
tracked as incomplete in the migration status. There is no legacy D editor any more.

Native CMO3 sessions store the `live2d.authoring.source.v1` scene in the same
canonical model, revision, and history as INX. A format-specific storage enum
retains an immutable CAFF/XML template and initial scene as its persistence
envelope. Saving applies allowlisted semantic differences to that template;
unmodeled differences fail. There is no second mutable XML document. Snapshot
`documentFormat` identifies the active storage format independently of unknown
model fields. INX-only operations fail on native CMO3 sessions.

## Decisions record

Normative architecture decisions are captured in [adr/0001-rust-authoritative-core.md](adr/0001-rust-authoritative-core.md).
