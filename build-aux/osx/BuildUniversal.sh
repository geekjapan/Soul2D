#!/usr/bin/env bash
# Build the Rust editor and CLI tools for macOS, then assemble out/Soul2D.app.
#
# The result is universal (arm64 + x86_64, glued with lipo) when both Rust
# targets are installed, and host-architecture only otherwise. Run
# `rustup target add x86_64-apple-darwin` (or aarch64-apple-darwin) to enable
# the universal build. Dependencies must already be fetched:
#   cargo fetch --locked --manifest-path rust/Cargo.toml
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

bins=(soul2d-editor soul2d-core njc)
stage=out/macos-bin
arm=aarch64-apple-darwin
x86=x86_64-apple-darwin

installed=$(rustup target list --installed 2>/dev/null || true)
rm -rf "$stage"
mkdir -p "$stage"

if grep -qx "$arm" <<<"$installed" && grep -qx "$x86" <<<"$installed"; then
    echo "Building universal binaries (arm64 + x86_64)..."
    for target in "$arm" "$x86"; do
        ./tools/build-rust-core.sh --locked --release --features gui --target "$target"
    done
    for bin in "${bins[@]}"; do
        lipo -create -output "$stage/$bin" \
            "out/rust/$arm/release/$bin" "out/rust/$x86/release/$bin"
    done
else
    echo "NOTE: both $arm and $x86 Rust targets are required for a universal build;" >&2
    echo "NOTE: building for the host architecture ($(uname -m)) only." >&2
    ./tools/build-rust-core.sh --locked --release --features gui
    for bin in "${bins[@]}"; do
        cp "out/rust/release/$bin" "$stage/$bin"
    done
fi

lipo -info "$stage/soul2d-editor"
exec ./build-aux/osx/osxbundle.sh
