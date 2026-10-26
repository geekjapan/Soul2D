#!/usr/bin/env bash
# Build out/Install_Soul2D.dmg from out/Soul2D.app (see osxbundle.sh).
# Uses create-dmg when installed (styled window, EULA); otherwise falls back to a plain hdiutil image.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

DMGTITLE="Install Soul2D"
DMGFILENAME="Install_Soul2D.dmg"

if [ ! -d "out/Soul2D.app" ]; then
    echo "Could not find out/Soul2D.app; run build-aux/osx/BuildUniversal.sh first." >&2
    exit 1
fi
cd out/
rm -f "$DMGFILENAME"
echo "Building $DMGFILENAME..."

# Create the install volume directory
rm -rf InstallVolume
mkdir -p InstallVolume
cp ../LICENSE LICENSE
cp -R "Soul2D.app" "InstallVolume/Soul2D.app"

if command -v create-dmg >/dev/null 2>&1; then
    create-dmg \
        --volname "$DMGTITLE" \
        --volicon "soul2d.icns" \
        --background "../build-aux/osx/dmgbg.png" \
        --window-size 800 600 \
        --icon "Soul2D.app" 200 250 \
        --hide-extension "Soul2D.app" \
        --eula "LICENSE" \
        --app-drop-link 600 250 \
        "$DMGFILENAME" InstallVolume/
else
    echo "create-dmg not found; building a plain image with hdiutil."
    ln -s /Applications InstallVolume/Applications
    cp LICENSE InstallVolume/LICENSE
    hdiutil create -volname "$DMGTITLE" -srcfolder InstallVolume -format UDZO -ov "$DMGFILENAME"
fi

echo "Done! Cleaning up temporaries..."
rm -rf LICENSE InstallVolume

echo "DMG generated as $PWD/$DMGFILENAME"
