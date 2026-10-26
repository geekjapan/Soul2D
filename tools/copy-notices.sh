#!/bin/sh
# Copy the third-party notice set that every distributed artifact ships into the directory $1.
# Used by the macOS bundle script, the Linux and Windows zip steps and the Flatpak manifest;
# build-aux/windows/wix/Product.wxs lists the same files; keep the two in step.
set -eu
dest=${1:?usage: copy-notices.sh DEST_DIR}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$dest"
# Only the two Noto fonts and OpenDyslexic from res/ are compiled into the editor; egui's own default fonts are
# covered by epaint_default_fonts-LICENSES.txt. The rust-dependencies files come from gen-third-party-crates.py.
for f in LICENSE res/NotoSans-Regular-LICENSE res/NotoSansCJK-Regular-LICENSE \
    res/OpenDyslexic-LICENSE \
    res/epaint_default_fonts-LICENSES.txt THIRD_PARTY.md doc/licenses/rust-dependencies.tsv \
    doc/licenses/rust-dependencies-licenses.txt; do
    cp "$root/$f" "$dest/"
done
