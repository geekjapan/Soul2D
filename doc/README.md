# Soul2D documentation

This directory holds the developer documentation: design contracts, API references, format notes, plans and
dated records. User guides are in [`user-doc/`](../user-doc/). Start with the [project README](../README.md).

English is the canonical language of the documentation. A few developer notes below are written in Japanese
and are marked (ja).

## Use Soul2D

- [Tutorials](../user-doc/tutorial/en/01-model-structuring.md): model structuring and parameter definition.
  Japanese: [チュートリアル](../user-doc/tutorial/ja/01-model-structuring.md).
- [Build from source](../BUILD.md): prerequisites, build commands, local checks and packaging.
- [Translate the editor](../TRANSLATING.md): gettext workflow for `tl/*.po`.

## Architecture and decisions

- [Architecture](ARCHITECTURE.md): the client and command-bus contract (principles P1 onward).
- Architecture decision records:
  - [ADR 0001](adr/0001-rust-authoritative-core.md): Rust authoritative core with typed command buses.
  - [ADR 0002](adr/0002-cubism-external-api-mapping.md): document a mapping for the Cubism external application
    API instead of implementing it.
  - [ADR 0003](adr/0003-3d-conversion.md): convert between 3D character formats and Soul2D through glTF 2.0.
  - [ADR 0004](adr/0004-no-moc3-writer.md): do not write `.moc3`.
- [Development goal](development-goal.md) and [acceptance scope](acceptance-scope.md).

## MCP, commands and the editor

- [MCP API](mcp_api.md): the public MCP surface (tools, resources, results).
- [Commands](commands.md): how [`commands.manifest.json`](commands.manifest.json) lists the MCP tools.
- [GUI operation inventory](gui-operation-inventory.md): GUI operations and the MCP tools that reach them.
  `tools/gen-gui-entries.py` reads this file. (Partly ja.)
- [Rust host rendering](rust-render-packet.md): the read-only `render.packet` request.
- [Command Browser and tool results](command_palette.md): how the editor and MCP report success, failure and
  affected resources.

## Formats and conversion

- [Semantic conversion obligations](format-conversion-map.md): what each conversion direction must keep.
- [Layered image import](layered-import.md): PSD and KRA import.
- [Live2D authoring containers](live2d-authoring.md): CMO3 and CAN3 source files.
- [The `.moc3` layout Soul2D reads](moc3-layout.md).
- Cubism research: [Cubism 5.3 Pro reference research](cubism-53-research.md),
  [operation ledger](cubism-feature-ledger.md), [external application API](cubism-external-api.md),
  [vendor comparison contract](vendor-comparison-contract.md).
- 3D conversion: [conventions](3d-conversion-conventions.md), [loss map](3d-conversion-loss-map.md),
  [rig import](3d-rig-import.md), [mesh import prototype](3d-mesh-import-prototype.md),
  [layered export](3d-layered-export.md).
- [MMD motion and pose readers](mmd-motion.md) (partly ja) and [motion sync from WAV speech](motion-sync.md).
- [Part split](part-split.md): split an illustration into parts.

## Depth and bones

- Depth Bone rig: [design](bone.md), [refresh after edits](plan.md),
  [target conversion recovery](depthbone-conversion-recovery.md).
- DepthBone GPU offsets: [design](depth-gpu.md) and [checklist](depth-gpu-checklist.md).
- PSD depth: [map import design](psd-depth/design.md) and [BoneSource rotation design](psd-depth-rotation.md).

## Rust core contracts

The D application inherited from nijigenerate was removed on 2026-10-12.

- [Rust migration removal map](rust-module-migration-map.md): what replaced each D module.
- [Rust editing core](rust-migration.md) and [depth and bone contracts](rust-migration-bone-depth.md).

## Verification

- [Real-file verification](real-file-verification.md) and [official sample corpus](official-sample-verification.md).

## Licenses

- [Third-party notices](../THIRD_PARTY.md).
- Rust dependency notices: [`licenses/`](licenses/).
