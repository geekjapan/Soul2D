#!/usr/bin/env bash
# Assemble out/Soul2D.app from the binaries staged in out/macos-bin by
# BuildUniversal.sh. The editor is installed as Contents/MacOS/soul2d, the name
# that Info.plist declares as CFBundleExecutable.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

stage=out/macos-bin
app=out/Soul2D.app
for bin in soul2d-editor soul2d-core njc; do
    if [ ! -x "$stage/$bin" ]; then
        echo "Missing $stage/$bin; run build-aux/osx/BuildUniversal.sh first." >&2
        exit 1
    fi
done

echo "Creating directory structure..."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/i18n" "$app/Contents/SharedSupport"

echo "Copying executables..."
install -m 755 "$stage/soul2d-editor" "$app/Contents/MacOS/soul2d"
install -m 755 "$stage/soul2d-core" "$app/Contents/MacOS/soul2d-core"
install -m 755 "$stage/njc" "$app/Contents/MacOS/njc"
cp build-aux/osx/Info.plist "$app/Contents/Info.plist"
# rust/Cargo.toml is the single version source (tools/verify-version.py keeps the tracked copy in step).
version=$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' rust/Cargo.toml)
for key in CFBundleShortVersionString CFBundleVersion; do
    /usr/libexec/PlistBuddy -c "Set :$key $version" "$app/Contents/Info.plist"
done

# The editor scans Contents/Resources/i18n for *.mo (see locale_dirs in rust/src/editor_ui.rs).
if command -v msgfmt >/dev/null 2>&1; then
    bash ./gentl.sh
    cp out/*.mo "$app/Contents/Resources/i18n/"
else
    echo "WARNING: msgfmt (GNU gettext) not found; the bundle has no translations." >&2
fi

# Licence, font and crate notices (see tools/copy-notices.sh for the set).
./tools/copy-notices.sh "$app/Contents/SharedSupport"

echo "Generating icon..."
iconutil -c icns -o out/soul2d.icns build-aux/osx/soul2d.iconset
cp out/soul2d.icns "$app/Contents/Resources/soul2d.icns"

echo "Done: $app"
