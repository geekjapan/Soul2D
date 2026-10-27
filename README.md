# Soul2D

English | [日本語](README.ja.md)

Soul2D is an experimental 2D model editor with a Rust editing core.
The project was previously named VSoul25D. The executables are `soul2d-core`
(headless CLI and MCP host), `njc` (MCP client) and `soul2d-editor` (experimental
GUI). C ABI symbols, MCP resource identifiers, and the legacy settings directory
keep their earlier names for compatibility.
It develops the code and workflows inherited from
[nijigenerate](https://github.com/nijigenerate/nijigenerate) and
[Inochi2D](https://github.com/Inochi2D/inochi-creator).

**Development status: incomplete. Binary distribution is stopped.**
This repository is available for source review and development. There is no
supported release, installer, Nightly build, or Flatpak distribution.
The repository starts from a new Git history; earlier development history is not published.

## Current scope

The Rust Session owns document state, revisions, undo/redo, and persistence.
Clients are a JSONL CLI, stdio and HTTP MCP, the `njc` MCP client, and an
experimental Rust GUI with an ACP agent panel. The earlier D application has
been removed; [the migration map](doc/rust-module-migration-map.md) lists what
replaced each D module and the gaps that remain.

Current work includes INP/INX editing, scene previews, parameter and deformer
operations, and bounded inspection and editing of CMO3/CAN3 source files.
Support varies by format and operation. Reading or saving a source file does
not establish correct rendering or playback in another editor.

In the CAN3 Outline, expand an Image or Guide track and a readable scalar channel
to add or delete keys, or edit an anchor value, either handle, or the stored segment
type. Apply or discard each edit before saving. Missing or fixed channels are not created.

The following work remains incomplete:

- Packaging: `build-aux/osx/BuildUniversal.sh` builds an unsigned macOS app
  bundle from the Rust binaries (universal `x86_64 arm64` build run on an arm64 host; the `x86_64` slice was
  only smoke-started under Rosetta; the DMG was built and mounted). The Flatpak manifest and the WiX project
  are built only by `.github/workflows/packaging.yml`; neither package was installed on a user's machine, and
  nothing is signed. See [BUILD.md](BUILD.md).
- Live click-through verification of the Rust editor beyond one create/undo/save/reopen flow on macOS.
- Full Live2D Cubism authoring, rendering, animation, Physics, and Pose parity.
- Live2D runtime data export beyond the display information and the physics settings: `live2d.cdi3.export` writes
  `.cdi3.json` and `live2d.physics3.export` writes `.physics3.json`, while `.model3.json`, `.pose3.json`, `.exp3.json`
  and `.moc3` are not written.
- Native ArtPath evaluation and drawing, Glue creation and editing, and the
  deformer cases no sample could measure. Stored ArtPath control points can be edited; this does not
  establish native drawing support (see [doc/live2d-authoring.md](doc/live2d-authoring.md)).
- Production validation of Windows, macOS, and GPU backends such as Metal,
  CUDA, Vulkan, and ROCm.

Use copies of your models for experiments.

## Documentation

- [Tutorials](user-doc/tutorial/en/01-model-structuring.md): structure a model and
  [define its parameters](user-doc/tutorial/en/02-parameter-definition.md).
- [Build from source](BUILD.md) and [translate the editor](TRANSLATING.md).
- [Developer documentation index](doc/README.md): [architecture](doc/ARCHITECTURE.md),
  [MCP API](doc/mcp_api.md), and [format conversion map](doc/format-conversion-map.md).

English is the canonical language of the documentation. Japanese versions exist for user-facing pages and
link back to the English text.

## Build from source

See [BUILD.md](BUILD.md) for prerequisites and build commands.

For the Rust core and CLI, after installing Rust and Cargo:

```sh
cargo fetch --locked --manifest-path rust/Cargo.toml
./tools/build-rust-core.sh --locked
printf '%s\n' '{"op":"new"}' | ./out/rust/debug/soul2d-core
```

The CLI returns a JSON response. To use stdio MCP:

```sh
./out/rust/debug/soul2d-core --mcp
```

For the experimental Rust GUI:

```sh
./tools/build-rust-core.sh --locked --features gui
./out/rust/debug/soul2d-editor
```

A graphical desktop and OpenGL are required for the GUI.

## Continuous integration

CI runs on pull requests targeting `main`, pushes to `main`, and manual dispatches.

- [Rust development build](.github/workflows/rust-core.yml) compiles the core, CLI, GUI, and test targets on
  Linux and launches the editor on headless X11 and Wayland.
- [Packaging](.github/workflows/packaging.yml) builds the Flatpak bundle and the Windows MSI and smoke-tests
  them, when the files they are assembled from change.

CI does not execute the test suite, sign anything, or publish releases.
A passing build does not establish runtime behavior or format compatibility.

## Contribute and report issues

Report Soul2D problems in
[this repository's issue tracker](https://github.com/geekjapan/Soul2D/issues).
Include your commit hash, platform, build command, and reproduction steps.
Remove credentials, personal paths, and model data from logs and screenshots.
Do not attach third-party models without redistribution permission.

Keep changes focused, preserve unknown source data when editing formats, and
verify the behavior you change. Manual checks and optional automated tests are
described in [BUILD.md](BUILD.md).

## License and acknowledgements

Project code is licensed under [BSD-2-Clause](LICENSE).
The original Inochi2D and nijigenerate copyright notices and
[contributor credits](CONTRIBUTORS.md) are retained.

Project artwork is self-authored and generated by
`tools/generate-project-artwork.py` (see [res/ARTWORK.md](res/ARTWORK.md));
inherited artwork was replaced under the BSD-2-Clause project terms.
Bundled fonts, vendored code, and external dependencies have their own
notices and terms. See [THIRD_PARTY.md](THIRD_PARTY.md).
The BSD license does not grant rights to third-party models, branding, or
trademarks. Live2D and Cubism names identify formats and compatibility targets;
Soul2D is an independent project and is not affiliated with Live2D Inc.

Current third-party notices and publication limits are in [THIRD_PARTY.md](THIRD_PARTY.md).
