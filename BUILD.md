# Build Soul2D from source

Run `python3 tools/check-project-artifacts.py` to check package, binary and
bundle-metadata names. After building the Rust targets, add `--built` to check
that `soul2d-core`, `njc` and `soul2d-editor` exist in `out/rust/debug/` or
`out/rust/release/`. If `out/Soul2D.app` exists (see below), `--built` also
checks its executable, `Info.plist` and icon.

These commands build development source. They do not create a supported
distribution. `rust-core.yml` builds all targets and runs Rust tests on Linux, Windows
and macOS. It also checks the editor launch on Linux. `packaging.yml` builds the Flatpak
and the MSI (see below). Windows and macOS GUI runtime support remains experimental.

`rust/Cargo.toml` holds the one product version. `python3 tools/verify-version.py`
fails when `Info.plist`, `Product.wxs`, `version.h` or `Cargo.lock` disagree with it;
change the version in `Cargo.toml` and in those files together.
`osxbundle.sh` also stamps the Cargo version into the bundle's `Info.plist`.

## Build the Rust core and CLI

Install a current stable Rust toolchain with Cargo using the
[official Rust installation instructions](https://www.rust-lang.org/tools/install).
Use the committed `rust/Cargo.lock`.

From the repository root:

```sh
cargo fetch --locked --manifest-path rust/Cargo.toml
./tools/build-rust-core.sh --locked
printf '%s\n' '{"op":"new"}' | ./out/rust/debug/soul2d-core
```

The last command creates an in-memory document and prints a JSON response.
The build script uses offline mode after dependencies have been fetched.
Outputs are in `out/rust/debug/`.

## Build the experimental Rust GUI

On Ubuntu, install the native build tools:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libxkbcommon-dev
```

On X11 the editor loads `libxkbcommon-x11.so` when it starts and exits without it; a minimal system needs
`libxkbcommon-x11-0` as well.

Build the GUI and launch it from a graphical desktop with OpenGL:

```sh
./tools/build-rust-core.sh --locked --features gui
./out/rust/debug/soul2d-editor
```

To compile every target, including the test targets, without running tests:

```sh
./tools/build-rust-core.sh --locked --all-targets --features gui
```

## Run CI checks locally

`rust-core.yml` runs these checks on pull requests and pushes to `main`, and through
`workflow_dispatch`. Its matrix covers Ubuntu 24.04 x64, Windows 2025 x64, and macOS 15
on Apple Silicon. Intel macOS and 32-bit Windows are outside the supported CI targets.
It uses stable Rust, Python 3.12, the committed lockfile
and cached Cargo dependencies. A failed platform does not cancel the other platforms.
After building all targets with the `gui` feature, run:

```sh
cargo test --locked --offline --manifest-path rust/Cargo.toml --target-dir out/rust --features gui --no-fail-fast
python3 tools/verify-rust-core.py
./tools/verify-command-manifest.sh
git diff --exit-code -- doc/commands.manifest.json
python3 tools/verify-mcp-initialize.py ./out/rust/debug/soul2d-core --mcp
python3 tools/verify-doc-twins.py
python3 tools/verify-version.py
python3 tools/check-project-artifacts.py
```

`tools/verify-rust-core.py` loads `out/rust/debug/libsoul2d_core.*` through
ctypes, so build first. `./tools/verify-command-manifest.sh` rewrites
`doc/commands.manifest.json` and fails if a second export differs.
The other `tools/verify-*.py` scripts drive the Rust binaries and fixtures.
On Linux, CI also runs `./tools/verify-linux-launch.sh` with Xvfb, Weston and Mesa
installed. It retains launch logs and screenshots for three days, including on failure.
The job has a 45-minute timeout, and a newer run on the same ref cancels the previous run.
On Windows, use `soul2d-core.exe` in the MCP initialization command. The FFI, CLI and MCP
checks run on every platform. Native editor window checks run only on Linux.
Three scripted HTTP MCP workflows also run on every platform. They check edits,
undo, saved state and reopening without starting an AI agent or using provider credentials.
Linux additionally runs the four real FFmpeg export tests that the default suite ignores,
including decoding, audio duration and audio placement. The two licensed-corpus tests
still require separately acquired inputs.
File-format checks that use external models require separately acquired
inputs and their licenses. Keep those inputs under the ignored `build/` directory.

## Translations

`./gentl.sh` compiles `tl/*.po` into `out/*.mo` with `msgfmt` from GNU gettext.
`python3 tools/genpot.py` regenerates `tl/template.pot` from the Rust sources
(see `TRANSLATING.md`); `--check` fails when the template is stale.

## Other platforms and packaging

Windows and macOS builds use the same Cargo commands. All packaging inputs live under
`build-aux/` and use the release binaries from `out/rust/` (`soul2d-editor` is installed
as `soul2d`). No installer is published or signed.

`python3 tools/verify-mcp-initialize.py <command> --mcp` sends one `initialize` request to a
`soul2d-core --mcp` process and checks the reply and the exit. The packaging checks use it to
prove that the shipped `soul2d-core` starts and answers.

### Third-party notices

Every artifact ships the set copied by `tools/copy-notices.sh`: `LICENSE`, the two Noto
font licences, the OpenDyslexic licence, `res/epaint_default_fonts-LICENSES.txt` (egui's default fonts),
`THIRD_PARTY.md`, `doc/licenses/rust-dependencies.tsv` and
`doc/licenses/rust-dependencies-licenses.txt`. The macOS bundle
(`Contents/SharedSupport`), the Linux and Windows zips, the MSI and the Flatpak
(`/app/share/licenses/`) all use it. After `Cargo.lock` changes, run
`python3 tools/gen-third-party-crates.py` (needs the crates in the Cargo cache) to refresh the
crate ledger.

### macOS app bundle and DMG

```sh
cargo fetch --locked --manifest-path rust/Cargo.toml
./build-aux/osx/BuildUniversal.sh
```

The script builds `soul2d-editor`, `soul2d-core` and `njc` with
`./tools/build-rust-core.sh --locked --release --features gui` and assembles
`out/Soul2D.app` (via `build-aux/osx/osxbundle.sh`). The editor becomes
`Contents/MacOS/soul2d`, the CLI tools sit beside it, `out/*.mo` go to
`Contents/Resources/i18n/` (needs `msgfmt`), and `soul2d.icns` is generated from
`build-aux/osx/soul2d.iconset` with `iconutil`. The app is not code signed.

The app is universal (arm64 + x86_64, joined with `lipo`) only when both
`aarch64-apple-darwin` and `x86_64-apple-darwin` are installed in `rustup`;
otherwise the script says so and builds the host architecture only.

`./build-aux/osx/gendmg.sh` turns `out/Soul2D.app` into `out/Install_Soul2D.dmg`
with `create-dmg` when it is installed (`brew install create-dmg`; styled window and EULA);
otherwise it builds a plain image with `hdiutil`. The EULA makes `hdiutil attach` ask for
agreement; pipe `yes` into it when scripting.

### Linux Flatpak

`build-aux/linux/flatpak/nightly/io.github.geekjapan.soul2d.yml` builds the GUI inside the
Freedesktop 25.08 SDK with the `org.freedesktop.Sdk.Extension.rust-stable` extension. The build
sandbox has no network, so vendor the crates first. The manifest copies the repository root, so the
`vendor/` directory (ignored by Git) travels with it, and `cargo-vendor-config.toml` is installed as
`.cargo/config.toml` in the build:

```sh
cargo vendor --locked --manifest-path rust/Cargo.toml build-aux/linux/flatpak/nightly/vendor > /dev/null
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --default-branch=nightly \
  --state-dir=out/flatpak-state --repo=out/flatpak-repo out/flatpak-build \
  build-aux/linux/flatpak/nightly/io.github.geekjapan.soul2d.yml
flatpak build-bundle out/flatpak-repo out/soul2d-nightly.flatpak io.github.geekjapan.soul2d nightly
```

Keep the state, build and repo directories under `out/`: the manifest skips `out`, and anything else
inside the repository root would be copied into the build.
The manifest grants only the Documents, Downloads and Pictures folders; other
locations need `flatpak override --user --filesystem=<dir> io.github.geekjapan.soul2d`.

### Windows installer

`build-aux/windows/wix/Product.wxs` is a WiX 3 source. It packages `out\rust\release\soul2d-editor.exe`
(installed as `soul2d.exe`), `soul2d-core.exe`, `njc.exe`, the notice set and `out\i18n\*.mo`.
`gentl.sh` writes `out\*.mo` (it needs `msgfmt`); copy them to `out\i18n\` first.
`build-aux/windows/soul2d.rc` carries the icon and version information: compile it with `rc /c65001`
from a Visual Studio developer prompt and pass the `.res` file to the linker through `RUSTFLAGS`
(`-C target-feature=+crt-static -C link-arg=<path>\out\soul2d.res`) when building the release binaries.

Install the WiX Toolset 3.14 (the GitHub `windows-latest` image has it; `WIX` points at its folder).
Paths in `Product.wxs` are relative to `build-aux\windows\wix`:

```powershell
cd build-aux\windows\wix
candle -nologo -arch x64 -ext WixUIExtension -out obj\ Product.wxs
light -nologo -sval -ext WixUIExtension -cultures:en-us -out ..\..\..\out\Soul2D.msi obj\Product.wixobj
```

`-sval` skips ICE validation, which needs the Windows Installer service.
All three executables retain the icon and version resources. The editor retains the console
subsystem so its existing stdin/stdout MCP entry remains available.

### Packaging checks

`.github/workflows/packaging.yml` runs on changes to what the packages are assembled from: `build-aux/`,
`tools/`, `rust/Cargo.*`, the translations (`tl/`, `gentl.sh`), `res/`, the notices (`LICENSE`,
`THIRD_PARTY.md`, `doc/licenses/`) and the workflow itself. It does not run on changes to `rust/src`;
start it by hand (`workflow_dispatch`) before a release. The `flatpak` job vendors the crates, builds and bundles the
Flatpak, installs the bundle and runs `soul2d-core --mcp` in the sandbox. The `wix` job builds the
release binaries and the MSI, extracts it with `msiexec /a`, checks that the executables and notices are beside
`soul2d.exe` and the catalogues in `i18n` under it, and runs the extracted `soul2d-core.exe --mcp`.
It then performs a real quiet installation on the disposable Windows runner with
`tools/verify-windows-install.ps1`. The verifier checks installed x64 executables and resources,
resolves and launches the advertised Start Menu shortcut, and opens an owned INX through
its file association. It checks the installed process, its window visible style, its owned MCP
listener and the loaded document identity before uninstalling. Window visible style is not
an assertion about rendered pixels or an unobscured desktop. A missing window or listener
fails the check. Installation logs and JSON observations are retained for three days; isolated
settings and tokens are excluded. Neither job uploads the package.

### Status

Run on the maintainer's Mac on 2026-10-13 (macOS arm64, Rust 1.95.0, `create-dmg` 1.3.0 from Homebrew):
`BuildUniversal.sh` built the universal bundle (`lipo` reports `x86_64 arm64` for all three
binaries), `gendmg.sh` built `out/Install_Soul2D.dmg` with `create-dmg`, `plutil -lint` passed, every
notice file was in `Contents/SharedSupport`, `hdiutil verify` passed, the image mounted read-only with
the app inside, and the bundled and the mounted `soul2d-core --mcp` answered `initialize`. The bundle is
not code signed or notarized, and the app was not opened in a window. An earlier run covered the plain
`hdiutil` fallback of `gendmg.sh` and, under Rosetta, the `x86_64` slices.

The Flatpak manifest and the WiX project are exercised by `.github/workflows/packaging.yml`, not by
hand; that workflow is the evidence for them. Both jobs first passed on 2026-10-12 (UTC): the Flatpak
bundle (20.5 MB) installed and `soul2d-core --mcp` answered inside the sandbox, and the MSI (27.8 MB)
extracted with the expected files and its `soul2d-core.exe --mcp` answered. Neither package was
installed on a user's machine, and the editor window was not opened from either.
