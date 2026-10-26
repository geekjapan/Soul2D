# Repository Guidelines

## Project Structure & Module Organization
- `rust/`: the product, one Cargo package `soul2d-core` (library `soul2d_core`, plus the `soul2d-core`, `njc` and `soul2d-editor` binaries). `Cargo.lock` is committed.
  - `src/lib.rs`: `Session`, the single owner of document state, revisions and undo/redo.
  - `src/mcp*.rs`, `acp.rs`, `njc_main.rs`: MCP tools and resources, HTTP/stdio hosts, ACP client and the `njc` MCP client.
  - `src/editor_*.rs`, `app_shell.rs`, `window_shell.rs`: the egui editor (`soul2d-editor`, behind the `gui` feature).
  - `src/render.rs`, `gpu_deform.rs`, `native_*.rs`, `authoring*.rs`: rendering, deformation and Live2D source handling.
  - `src/psd*.rs`, `kra.rs`, `model_import.rs`, `gltf_rig.rs`, `gltf_mesh.rs`, `gltf_export.rs`, `export.rs`, `inp_export.rs`, `atlas.rs`: file import and export.
  - `src/manifest.rs`: writes `doc/commands.manifest.json`.
  - `tests/`: integration tests; `tests/fixtures/` holds their data (for example `depthdraw/`).
  - `examples/`: helper binaries used by tests (`acp-echo`, `caff-roundtrip`).
- `res/`: assets (fonts compiled into the editor, UI images, artwork notes).
- `tl/`, `TRANSLATING.md`: gettext translation data and guidance; `gentl.sh` compiles `.mo` files into `out/`.
- `doc/`: design notes, the migration records and the generated `commands.manifest.json`; `doc/README.md` indexes them.
- `user-doc/`: user guides; `user-doc/tutorial/en/` is canonical and `user-doc/tutorial/ja/` mirrors it file for file.
- `tools/`: Python and shell helpers that build or exercise the Rust binaries (`verify-*.py`, `build-rust-core.sh`, `verify-command-manifest.sh`).
- `build-aux/`: packaging resources (macOS bundle, Flatpak, Windows) for the Rust binaries. The macOS bundle and DMG scripts were run locally on 2026-10-13; the Flatpak and WiX paths are exercised by `.github/workflows/packaging.yml`; see `BUILD.md`.
- `out/`: build artifacts (`out/rust/debug/` holds the binaries).
- No D source remains. `doc/rust-module-migration-map.md` records what replaced each D module.

## Build, Test, and Development Commands
- Build core, CLI and library: `./tools/build-rust-core.sh [--locked]`. Outputs are in `out/rust/debug/`.
- Build the editor too: `./tools/build-rust-core.sh --locked --features gui`, then run `./out/rust/debug/soul2d-editor`.
- Build every target, including tests: `./tools/build-rust-core.sh --locked --all-targets --features gui`.
- Test: `cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust [--features gui]`.
- Headless MCP over HTTP: `./out/rust/debug/soul2d-core --mcp-port 18088 [--project <path>]`; client: `./out/rust/debug/njc --port 18088 tools list`.
- Stdio MCP: `./out/rust/debug/soul2d-core --mcp`. JSONL: one request per line on stdin of `soul2d-core`.
- Command manifest: `./tools/verify-command-manifest.sh` regenerates `doc/commands.manifest.json`, checks that the export is stable, then runs `python3 tools/verify-operating-skill.py`, which fails when a tool is in no `.cursor/skills/operate-soul2d/families/*.md` map, or when a "Tool rows" number in that skill's `SKILL.md` is out of date (add new tools to a map and correct the number), and `python3 tools/verify-editor-dispatch.py`, which fails when the editor dispatches an operation or D command id that no manifest tool reaches, and `python3 tools/gen-gui-entries.py --check`, which fails when `rust/src/manifest_gui_entries.rs` (the `guiEntry` table, generated from the Soul2D rows of `doc/gui-operation-inventory.md`) is out of date, and `python3 tools/gen-edit-ops.py --check`, which does the same for `rust/src/manifest_edit_ops.rs` (the `document-edit` table, derived from the operations' call graph); rerun either script without `--check` to refresh its table.
- Translations: `./gentl.sh` writes `out/*.mo`.

## Cursor Cloud
- Use a current stable Rust toolchain with Cargo. Fetch dependencies once with `cargo fetch --locked --manifest-path rust/Cargo.toml`; the build script then runs offline.
- The GUI needs `build-essential`, `pkg-config` and `libxkbcommon-dev` on Ubuntu, and a display with OpenGL (`DISPLAY=:1` in the cloud image).
- Headless MCP: `./tools/build-rust-core.sh`, then `./out/rust/debug/soul2d-core --mcp-port 18088`. Client: `./out/rust/debug/njc --port 18088 tools list`.
- Translations: `./gentl.sh` writes `out/*.mo`.

## Coding Style & Naming Conventions
- Rust 2021. Indentation: 4 spaces; avoid trailing whitespace; keep lines near 120 characters.
- Modules and files: `snake_case`. Types and enums: `UpperCamelCase`. Functions and variables: `snake_case`. Constants: `SCREAMING_SNAKE_CASE`.
- Imports: group logically and prefer specific imports.
- Blocks: `{` goes on the same line as the declaration.
- New MCP tools are registered in `rust/src/mcp.rs`; run `./tools/verify-command-manifest.sh` so `doc/commands.manifest.json` lists them.
- Formatting: keep consistent with existing code; avoid unrelated formatting churn.
- Comments: must be written in English.
- Documentation: English is canonical. A user-facing page also gets a Japanese twin (`README.ja.md`, `user-doc/tutorial/ja/`) with the same structure; each twin starts with a link to the other, and a change to the English page updates the Japanese one in the same commit. List new `doc/` files in `doc/README.md`.

## Testing Guidelines
- Checks: verify the implementation by running the build and test commands above after any change to source code; update docs/strings when changing UI; avoid unrelated formatting churn.

## Commit & Pull Request Guidelines
- Git operations are authorized for work in this repository. Run the needed Git commands, including commits, stash, branch and remote changes, fetch, pull, push, merge, rebase, reset, revert and tags, without requesting additional permission.
- Commits: present tense and concise. Optional scope prefix (e.g., `fix: core: …`, `feat: ui: …`, `refactor: shortcuts: …`).
- Commit messages and PR text must be written in English unless the user explicitly requests another language.
- For forks/unofficial builds, update the links in `rust/src/editor_windows.rs` (bug reports, docs, website), `rust/src/crashdump.rs` and `rust/Cargo.toml` before distribution.
- Open or close pull requests when the requested workflow calls for it.

## Agent Operation & Safety Rules
- Default-allowed operations: Edit files in the local workspace to implement requested changes (use `apply_patch`); inspect code; and run local build/test commands (e.g., `./tools/build-rust-core.sh`, `cargo test`) when relevant to the requested task.
- Prohibited without explicit user approval: Network access not inherent to the task; environment/system configuration changes; and destructive operations.
- Ask before acting: When an operation could be destructive, high-impact, long-running, or ambiguous, ask for confirmation with a concise plan and the exact commands.
- Minimal scope: Prefer the smallest, surgical change that resolves the issue; avoid unrelated edits or refactors.
- Reproducibility: When possible, include exact commands to build, run, and test the changes. Avoid long/expensive commands unless requested.
- Local edits first: Implement and verify changes before committing them.

## Language Policy
- Always respond in the same language used by the user’s latest message, unless the user explicitly requests another language.
- In mixed-language contexts, follow the user’s preference per message; code, logs, and paths should remain as-is.

## User Interaction
- Be concise and actionable. Provide short, clear next steps.
- Before running grouped actions, briefly summarize what you will do next.
- If constraints prevent an action (permissions, missing tools), state the limitation and offer alternatives.
