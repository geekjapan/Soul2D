---
name: verify-vsoul
description: Verify Soul2D changes through MCP and CLI without Computer Use as the default path. Prefer njc or soul2d-core, resource snapshots, and PNG evidence per doc/ARCHITECTURE.md P5.
---

# Verify Soul2D

Default verification follows **[P5. Every result is observable without Computer Use](../../../doc/ARCHITECTURE.md#principles)** in `doc/ARCHITECTURE.md`: confirm outcomes with semantic reads and rendered PNG bytes, not OS GUI driving.

## Default proof order

For CAFF authoring container changes, build the Rust core and its
`caff-roundtrip` example, then run
`python3 tools/verify-caff-authoring.py build/verification/live2d-inputs-vjq5llby`.
Require all eight supplied CMO3/CAN3 sources, matching FFI/JSONL/MCP inspection,
independent XML/CRC/payload preservation checks, and unchanged source hashes.
This does not prove Cubism Editor reopening or complete model animation.

For native CMO3 source-reader changes, build the Rust core and run
`python3 tools/verify-cmo-source.py`. Require exact independent XML agreement for
all source/form/grid/morph/extension/texture fields through FFI, JSONL and actual stdio MCP,
unchanged edited INX state and original hashes, and twenty-three expected failure controls.
The eight hierarchy controls must reject part/deformer reference errors and cycles
through all three native queries on FFI/JSONL and source read on stdio MCP.
Check the three five-axis 243-cell grids and Ren's non-default ArtMesh alpha
compositions. Confirm the result's source/binary hashes match the tested files.
Check ten generated forms with mismatched vertex counts and twenty nested targets
without owner-local form payloads; preserve their raw data and availability flags.
This covers primary/generated source and morph metadata; cache regeneration,
morph evaluation, canvas-space evaluation, model rendering, editable INX import
and native GUI remain separate.

For primary linear-weight changes, build the Rust core and run
`python3 tools/verify-primary-weights.py` plus the source-reader check above.
Require raw-XML agreement at all four defaults and symmetric/asymmetric samples
of the three five-axis grids through FFI, JSONL and actual stdio MCP, unchanged
edited INX/input hashes, expected failure controls and matching artifact hashes.
This is in-range primary LINEAR tensor math; morph/extended-grid evaluation,
deformer transforms and final Cubism poses remain separate checks.

For primary form interpolation changes, also run
`python3 tools/verify-primary-forms.py`. Require complete owner-local
geometry/opacity/color/rotation agreement at defaults and nondefault samples,
including Pro's fractional draw order, edited INX/input preservation and matching
artifact hashes. Coordinate representations must remain separate, with parent
and target-deformer GUIDs retained. Source-form arithmetic does not establish
runtime sorting, inherited properties or canvas-space geometry.

For rendering changes, build the Rust core and run
`python3 tools/verify-composite-pixels.py`. It renders bounded Composite fixtures
through the Rust library and checks the pixels; the retained result lists each case.
This is a CPU-render check. It does not prove native GUI input or GPU output.

Run these steps in order. Do not install or require `cua-driver` on this path.

1. **Build** — `./tools/build-rust-core.sh` (see `BUILD.md`); it produces `soul2d-core` and `njc` in `out/rust/debug/`.
2. **Explore** — `njc tools list` against `soul2d-core --mcp-port PORT`, or the `soul2d-core --mcp` tool listing; `resources list` / `find '*'`.
3. **Read** — `resources read` / binding reads for UUIDs from the current run only.
4. **Act** — `tools call` for mutations (node create, rename, save, open, undo when in scope).
5. **Appearance** — PNG evidence:
   - HTTP host: `ViewCommand_CaptureLiveScreenshot` via `njc` (decode `image/png` from tool result).
   - Stdio or CLI: `soul2d-core` `render` tool or documented CLI render; same PNG signature and dimension checks.

Assert persisted names through resource readback after save/reopen, not success messages alone. Save PNGs under a unique run directory (for example `build/verification/YYYYMMDD-HHMMSS/viewport.png`) and inspect dimensions (`width > 0`, `height > 0`) before appearance claims.

## Headless MCP smoke (soul2d-core + njc)

For MCP and tool-surface changes, use the bundled helper:

```bash
.cursor/skills/verify-soul2d/scripts/prove.py
```

It launches an isolated `soul2d-core --mcp-port` host, runs **node add → rename → save → reopen → resource read → live viewport capture**, and writes `viewport.png`, `roundtrip.inx`, and JSON transcripts. Details and feature-specific extensions live in [verify-soul2d](../verify-soul2d/SKILL.md).

## Rust core smoke (soul2d-core)

When the change touches `rust/` or Rust-mode routing:

```bash
./tools/build-rust-core.sh
python3 tools/verify-rust-core.py
```

For editor changes, run the GUI-feature tests:

```bash
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust --features gui
```

Use stdio MCP on `out/rust/debug/soul2d-core --mcp` when the task is agent-shaped; prefer `render` PNG and `resource://vsoul/snapshot` reads per `doc/rust-migration.md`.

## Live2D Motion3 curves

For the raw Motion3 importer, evaluator, point editor or export changes:

```bash
./tools/build-rust-core.sh
cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust
python3 tools/verify-motion3.py build/verification/live2d-inputs-vjq5llby
python3 tools/verify-motion3-pose.py build/verification/live2d-inputs-vjq5llby
```

Use verified copies of the three supplied archives at the final path, with all
44 Motion3 files. The verifier runs Rust FFI and actual stdio MCP independently,
checks eight numeric samples against an independent curve oracle, and exercises
editing, undo/redo, INX save/reopen, Motion3 export/reimport and refusal to replace
existing files. It retains source and executable hashes, wire messages, edited
files and an INX fixture PNG in a fresh `build/verification/motion3-*` directory.
Require `result.json` with both paths passing. A failed run is not acceptance.

The pose verifier maps five independently calculated samples from all four
source models onto an existing textured INX fixture. It checks values, transform
and deformation readback, literal bounds, PNG pixels, physics input, persisted
mappings and parameter deletion/Undo through FFI and actual stdio MCP. Its
explicit destination range is authored in the synthetic fixture; this does not
infer Cubism parameter metadata. Require its separate `motion3-pose-*/result.json`.

Raw sampling is curve evaluation in seconds. Mapped seek/advance apply explicit
Parameter mappings to the INX runtime preview, optionally looping. Neither path
implements fades or runtime motion blending. The fixture PNG checks INX rendering
after curve import; it is not a rendered Live2D model. Native input, MOC loading,
CMO3 authoring and full animated model compatibility remain separate checks.

## Native CMO3 session editing

After native document, history, writer, or vertex-command changes, build the Rust
core and run `python3 tools/verify-native-authoring-edit.py`. Require the actual
four models' ArtMesh and a Warp form to pass open, active read, edit, no-op,
revision rejection, undo/redo, save and reopen through FFI, JSONL and stdio MCP.
Require exact raw XML preservation outside changed scalar tokens, unchanged
opaque CAFF entries and original hashes, and unchanged state on rejected edits.
Check derived-cache/morph refusals, float overflow and external-file conflicts.
The result's source/binary hashes must match the tested files.
For embedded MCP edits, first read `resource://soul2d/document/state`.
Require the five live state fields to match the active Session before and after
edit, rejection, Undo/Redo, save and reopen. Use this public resource's revision
in the edit request; do not inject a revision from the parent test process.

For embedded MCP edits, drive `soul2d-core --mcp-port` with `njc` and the model copies.
A passing headless helper or
HTTP workflow does not prove foreground GUI input, Cubism Editor reopening,
canvas pose equivalence or complete authoring compatibility.

## Native ArtMesh opacity

For native ArtMesh opacity, run `python3 tools/verify-native-authoring-opacity.py`
after rebuilding Rust. Require all four supplied models through FFI, JSONL and
stdio MCP, including no-op/stale/history/save/reopen and combined vertex-opacity
save. Check raw XML outside edited scalar tokens and opaque entries exactly.
Retain authored values above one; reject nonfinite/native-float overflow and
changed writes to missing/shared scalar storage without changing the Session.
Same-value requests preserve the existing bytes, revision and history as no-ops.
Foreground GUI and native rendering are separate evidence.

## ACP client

ACP initialization, session setup and the editor's agent routing are covered by
`rust/tests/acp.rs` and the `acp-echo` example (`cargo test`). These check protocol
behavior against a deterministic peer. The D regression targets and the real-agent
scripts (`verify-real-acp.py`) were removed with the D tree, so no real ACP agent has
been run against the Rust client; actual AI reasoning and native GUI input need
separate evidence.

## Real CLI agent verification

With an installed and authenticated Codex CLI, run this optional check:

```bash
./tools/build-rust-core.sh --bin soul2d-core
python3 tools/verify-rust-ai.py --self-check
python3 tools/verify-rust-ai.py
```

The check uses a fresh read-only CLI session and per-invocation MCP approval
settings for nine tools. A server-side guard restricts file operations to the
generated scratch fixture and named outputs; it does not alter global settings.
The agent reads this skill, explores the actual Rust MCP server, edits a textured
Part, checks Undo/Redo, saves and reopens INX, and renders a PNG. The parent checks
persisted data, unknown JSON and opaque bytes, intermediate snapshots, literal
bounds and two expected pixels. Rejected edits must leave the snapshot unchanged.
Keep the transcript, runtime model metadata, PNG, result and cleanup receipts in
the printed run directory. This verifies real CLI reasoning through stdio MCP;
ACP agents, native GUI input, full-image equivalence and Live2D remain separate.

## Doctor

Before driving an interactive host on a chosen loopback port (default `18088` in the proof helper):

```bash
lsof -nP -iTCP:18088 -sTCP:LISTEN
./out/rust/debug/njc --port 18088 tools list
```

**The listener PID must equal the child PID recorded for this run.** Tools must include `NodeCommand_AddNode` and `ViewCommand_CaptureLiveScreenshot`. A response from any other process is a failure. Do not attach to the user's editor.

## Evidence to retain

- Command transcript (`actions.jsonl` or equivalent), raw JSON responses, stderr, instance metadata (`pid`, port, config path).
- Saved project bytes used in the round-trip.
- At least one **nonempty PNG** with valid signature from capture or Rust render.
- `result.json` or lane report stating `PASS` and which surface was exercised.

## Primary morph geometry acceptance

For primary morph geometry changes, run `python3 tools/verify-morph-forms.py`
after building the Rust core. Require complete raw-XML response agreement over
FFI, JSONL and stdio MCP for all supplied Ren and Zundamon targets, including legal half/full
activation, simultaneous targets and non-default normal parameters. Require
neutral inactive-scalar, non-stored-default and normal-grid-overlap refusal
controls, malformed-constraint refusals, unchanged active INX/native snapshots and original source hashes.
Require every Zundamon constraint knot and midpoint, nonzero model defaults,
opacity deltas and the independent ArtMesh196 literal: original weight 1 with
three controller limits of 0.5 produces weight and opacity 0.5, not 0.125.
Also run `python3 tools/verify-morph-constraint-limits.py`: isolate each curve
by fixing every other controller at limit 1. Require all 116 curves, 353 cases
and 1,059 complete-response comparisons, retained transport responses and
unchanged native snapshots/original source. Default values can mask a curve's
effect, so the full corpus alone does not establish its isolated arithmetic.
The retained Rotation52 negative key is inactive throughout its legal 0..1 domain;
out-of-range samples must still fail.
Source-space geometry does not
prove deformer evaluation, final rendering or Cubism equivalence.

For active-scene cache changes, run `python3 tools/verify-authoring-cache.py`.
Require all four supplied models, repeated cached queries, independent XML
expectations for vertex/opacity edits and Undo/Redo, exact no-op/stale model and
revision preservation, independent explicit-path queries, Open/New reset,
unchanged query snapshots and original source hashes. Passing saved-output
agreement alone does not prove the intended edit/history transition.

## Cleanup

Terminate **only the recorded child PID** for this run; wait for exit. Remove only this run's scratch config and temp projects. **Never kill by process name** (no `pkill soul2d`, no broad port kills). Keep evidence under `build/verification/`. Reusable build artifacts stay in `out/`.

## Optional categories (not default proof)

These may use Computer Use or manual GUI. They are **not** required to mark a code change verified under P5.

### GUI-only interaction (optional appendix)

Menu layout, shortcuts, panel visibility, egui accessibility, and pixel-accurate clicks. See [verify-soul2d — GUI-only appendix](../verify-soul2d/SKILL.md#appendix-gui-only-verification-optional).

For an isolated native CMO3 GUI run, copy the model into a run-owned scratch directory and
launch `out/rust/debug/soul2d-editor --open <copy>` with `INOCHI_CONFIG_PATH` set to a scratch
config directory, rather than the installed editor's shared bundle identifier. Observe the initial source/form
and revision, switch to opacity, enter a draft and verify Save refuses it without
discarding the draft. Apply, Undo and Redo with fresh observations. Replace the
document path with `<run>/saved.cmo3`; wait until the complete path is visible
before Save. Reopen and verify the value and disabled history buttons. Compare
the complete saved model with the independent opacity oracle and check unchanged
source/XML/opaque bytes. Close only this run's saved window and verify exit.
egui controls may have no AX rows: ground pixel actions on the exact window's
fresh screenshot. Text entry may arrive after the first observation; never save
an incomplete path. This one source-form workflow does not cover all GUI cases
or native model preview/rendering.

For the coordinate workflow, compare the selected vertex's two submitted float32
values against the independent source. A coordinate unchanged by input must retain
its float32 bits; the decimal representation can differ. All XML outside those two
tokens must remain byte-exact. Native screenshots may be JPEG bytes; persist them
with the matching extension instead of labelling them PNG.

### OS file-picker E2E (optional)

`File > New`, native open/save dialogs, and MCP-hidden file entry points. Drive through GUI or documented manual dialog scripts in a **separate** run; do not fold file-picker steps into the default MCP smoke. CLI file commands (`FileCommand_OpenFile`, `FileCommand_SaveFile` with absolute scratch paths) satisfy default proof for persistence.
