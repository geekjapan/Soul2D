# ADR 0001: Rust authoritative core with typed command buses

- **Status:** Accepted
- **Date:** 2026-10-06
- **Context:** Soul2D is moving document ownership into Rust while keeping MCP, ACP, CLI, and GUI as clients. Agents must mutate and verify puppets without ImGui-only code paths.

## Decision

Adopt a **Rust authoritative core** behind a **typed CommandBus and QueryBus**. MCP, ACP, CLI, and GUI are adapters and clients of those buses. The D implementation remains the **reference oracle** for migrated operations (semantic diff against Rust). Incremental scope and evidence live in [rust-migration.md](../rust-migration.md).

### Since then (2026-10)

- The D source, the D editor and the D reference oracle were deleted on 2026-10-12. The clients are now `soul2d-core`,
  `njc` and `soul2d-editor` (egui), all in `rust/`. Read "D", "ImGui" and "oracle" below as history; the oracle
  sections stay as the record of how the landed slices were accepted.
- "CommandBus" and "QueryBus" are roles, not Rust types. The entry points are `Session::handle_request` and
  `Session::handle_mcp` in `rust/src/lib.rs`. The result shape is in [command_palette.md](../command_palette.md).
- The command list is `doc/commands.manifest.json`, described in [commands.md](../commands.md). It marks every tool
  `mcpExposed: true`. The consequence "MCP tool lists remain filtered; GUI-only commands stay hidden" below: Not verified
  in the Rust port.

Related references:

- [mcp_api.md](../mcp_api.md) — Explore → Read → Act client workflow
- [commands.md](../commands.md) — command IDs, effects, and MCP exposure
- [ARCHITECTURE.md](../ARCHITECTURE.md) — principles and target diagram

## Principles

### P1. No GUI-only mutation

Persistent edits required by agents, CLI, or automation must be reachable through CommandBus commands (and their MCP/CLI projections). Documentation and skills must not instruct agents to drive ImGui widgets as the only persistence path.

### P2. Core owns state

The Rust session owns canonical document bytes, persistent history, and revision for migrated scope. The GUI must not own authoritative document state; it sends commands and renders projections. Unsupported operations fail closed rather than updating a shadow D copy.

### P3. Commands are semantic

Mutations use registered commands with stable IDs and documented effects ([commands.md](../commands.md)). MCP `tools/call` names map to those commands; context objects carry semantic selection (nodes, parameters, binding descriptors), not internal widget indices.

### P4. Every mutation is transactional

Failed validation, unsupported ops, or internal errors leave the session unchanged. Open/save use atomic replace semantics described in [rust-migration.md](../rust-migration.md).

### P5. Every result is observable without Computer Use

Default verification uses MCP resources, snapshots, revision/history flags, CLI JSON responses, and PNG output. **Computer Use is not the default agent observation path**; it may appear only for optional GUI-only checks (e.g. native file pickers).

## MCP and ACP as adapters

MCP exposes tools and resources over JSON-RPC; ACP integrates the agent panel in the editor host (`rust/src/acp.rs`, `rust/src/editor_agent.rs`). Both **adapt** external protocols to the shared CommandBus/QueryBus—they are not substitutes for typed commands. High-frequency manipulation (viewport drag, mesh brush) stays on the GUI bus inside the process; agents use semantic tools and reads instead of pixel-driving the desktop.

Follow **Explore → Read → Act** from [mcp_api.md](../mcp_api.md): list or find resources, read UUID state, then call tools to mutate.

## D reference oracle

For each migrated operation, acceptance compares Rust behavior to D on the same fixture commands. [rust-migration.md](../rust-migration.md) is the living record of the **landed Rust subset** (parameters, TRS, history, open/save, headless PNG) and how oracle checks are run. New slices extend that document; they do not bypass oracle diff.

## Migration phasing

| Phase | Scope |
|-------|--------|
| Landed | Parameters, TRS bindings, history, open/save, headless PNG ([rust-migration.md](../rust-migration.md)) |
| Landed | Node create, rename, local transform, shared undo/redo, save/reload ([rust-migration.md](../rust-migration.md)) |
| Landed | Node move/reparent, delete, broader structural create types (`Composite`, `MeshGroup`, `DynamicComposite`, `Camera`, `Mask`), shared undo/redo, save/reload ([rust-migration.md](../rust-migration.md)) |
| Landed | Node clipboard cut/copy/paste and structural ConvertTo for `Node`, `Composite`, `MeshGroup`, `DynamicComposite`, `Camera`, and `Mask`. P5-M1 converts a Node to a Part with empty mesh arrays and keeps a Part, Mask, or DynamicComposite mesh. Grid and Path converts stay closed ([rust-migration.md](../rust-migration.md)) |
| Landed | `PathDeformer` and `GridDeformer` create, insert, structural edits, persisted control points, and deform properties; MeshGroup `dynamic_deformation`, `translate_children`, and mesh payload; ConvertTo for those deformers. Deform-binding edits and child deformation evaluation stay fail-closed ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B0: linear `transform.r.x`, `transform.r.y`, and `transform.t.z` on every node. Part raster still rejects non-zero X/Y rotation ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B1: `DepthRigRoot` / `DepthBone` create, save, and render skip. Export still drops rig nodes ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B2: standard skeleton placement from parent bounds, matching `ngAddStandardDepthSkeleton` ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B3: depth rig binding table on `DepthRigRoot` ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B4: structural edits rewrite the binding table in the same undo ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B5: CPU preview and apply for the current key, including ancestor-grid compensation. Preview stays outside history. Apply writes the armed `"deform"` key. Mesh consumption and the dirty queue remain later ([rust-migration.md](../rust-migration.md)) |
| Landed | Depth D0/D1: opaque `depths` / rig JSON round-trip, and `ListDepths` / `SetDepths` / `ClearDepths` on `GridDeformer` ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Depth D2: depth-operation editing and `ApplyDepthOps` on `GridDeformer`. PSD import stays fail-closed ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Depth D3: `DefineGrid` / `DefineVertices` resample `GridDeformer` `depths` and `depth-op-base-depths` in the same undo ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Depth D4: CPU `ImportPSDDepths` for version-1 8-bit raw RGB PSD and PNG. A driven rig refreshes `"deform"` in P2-E4 ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Next | Remaining inspector and node commands outside the landed subset, including parameter deform bindings and evaluated child deformation |
| Landed | Bone slice B6: standard face/body parameters and the DepthBone bindings named in `doc/bone.md` ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B7: Fit Z writes DepthBone local translation Z from the current mapped depth, in one undo ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B8: one dirty-queue flush writes `"deform"` into the causing edit's undo. The key budget and delete/convert refresh remain later ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B9: all-keypoints refresh writes every cell, previews the armed key, and rejects a key-vertex product above 262144 before any binding write ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice B10: delete, convert, and paste rewrite bindings before deform generation in the same undo. A non-grid target is not packed. A failed generate leaves no history entry ([rust-migration.md](../rust-migration.md)) |
| Landed | Bone slice P2-E1: rest and constraint edits share one undo with the deform refresh. Lock flags affect pose. Hinge fields are stored and not solved ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-R1: ancestor-grid propagation through Camera, Mask, Composite, and DynamicComposite, matching nijilive `mustPropagate` ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Bone slice P2-E2: standard parameters include the D Neck, pelvis, shin, and roll rows. A later call adds only missing rows ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-R2: headless raster adds interpolated `"deform"` offsets to Part vertices. `previewDeformation` wins for that node and is not saved ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Bone slice P2-E3: DepthBone and DepthRigRoot accept local transform and refresh deform in that undo ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-R3: Part raster projects full local Euler rotation. Translation Z does not move the sprite. Part `lockToRoot` is P3-R1 ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-E4: PSD import on a driven rig writes depths and refreshes `"deform"` in the same undo. An over-budget flush rolls the import back ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Bone slice P2-E5: an undriven `"deform"` binding is resized or reordered with the vertex edit or convert. A driven target still uses the dirty queue ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Bone slice P2-E6: convert leaves the parameter `"deform"` row and does not restore the rig row. `rig.add_source` regenerates it ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-E7: deleting a `DepthRigRoot` removes its bones and prunes bindings to those UUIDs. External `"deform"` rows stay ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 2 P2-E8: an over-budget all-keypoints flush keeps whole cells on a session cursor. `deform.continue` and `save` append them to the same undo ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 3 P3-R1: Part raster and bounds compose `lockToRoot` with the puppet root local transform. `pinToMesh` still fails the draw ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 3 P3-H1: `rig.overlay` returns read-only bone points, links, and effective yaw pivots. It does not draw or write the document ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-W1: a binding may store `vertexWeights`. A missing key keeps the gaussian score. The INP/INX version is unchanged ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-H1: `rig.pick_bone` hits overlay points. `rig.drag_bone` writes an undriven transform, or a driven key offset against the unchanged base, then refreshes deform ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-K1: `rig.solve_ik` bends a chain with CCD around `hingeAxis` and stores the Euler through the drag pose write ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-M1: `rig.mirror` reflects a closed subtree through the rig-local YZ plane and permutes `vertexWeights` when every source is selected ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-F1: `rig.fit_human` places B2 rest from named Part and Grid bounds and stores the gaussian `vertexWeights` ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 4 P4-G1: a headless software kernel matches the CPU deform within `1e-4` and is discarded when it disagrees. The CPU offsets stay the document ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-R1: `SimplePhysics` and `Shapes` stop ancestor-grid propagation like `Node.mustPropagate` ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-B1: `node.move` reparents a `DepthBone` or `DepthRigRoot`, keeps Z and X/Y rotation, and keeps rig bindings. A rig node does not regenerate `"deform"` ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-C1: one deform cell above 262144 vertices is assembled in session chunks and written only when every vertex has an offset. A cell above 1,000,000 vertices fails before any write. No new file bytes ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-E1: `mesh.mirror` reflects Path and Grid vertices with the D mirror formula, including `mirrorV` using `mirrorOrigin.x`. A Part without a mesh stays fail-closed ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-N1: `render` and `bounds` pin a child to its Part parent's triangle. The draw does not write translation or rotation ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-W1: `part.set_weld` writes reciprocal Part links. The draw mixes them only when both exist and does not save the mix ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-M1: `node.convert` to `Part` stores `vertices`, `uvs`, and `indices`, including a DynamicComposite source mesh. Grid and Path stay closed ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Landed | Phase 5 P5-P1: `SimplePhysics` stores `serializeSelfImpl` settings. `physics.set` edits them in one undo and does not step the simulation ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |
| Later | The scheduled Phase 5 slices are landed. Device GPU backends, a learned fit, VRM/Live2D, and the physics integrator stay open ([rust-migration-bone-depth.md](../rust-migration-bone-depth.md)) |

**Bone slices B0–B10, Depth slices D0–D4, Phase 2 P2-R1 through P2-R3 and P2-E1 through P2-E8, Phase 3 P3-R1 and P3-H1, Phase 4 P4-W1, P4-H1, P4-K1, P4-M1, P4-F1, and P4-G1, and Phase 5 P5-R1, P5-B1, P5-C1, P5-E1, P5-N1, P5-W1, P5-M1, and P5-P1 are landed.** They must not expand the Node slice ADR scope. What stays fail-closed is listed in [rust-migration-bone-depth.md](../rust-migration-bone-depth.md).

## Intermediate representation (v0)

v0 does **not** introduce a full **VRM** or **Live2D** interchange IR. IR work stays limited to what nijilive lossless INP/INX and Inochi Creator compatibility require. General third-party avatar IR is out of scope until those formats are stable requirements.

## Consequences

- New features that change persisted model state should land in Rust (or extend the oracle contract) before agents depend on them.
- MCP tool lists remain filtered; GUI-only commands stay hidden from default agent smoke paths.
- Verification skills should cite P5 and prefer `njc`, `soul2d-core`, and PNG bytes over Computer Use.
