#!/usr/bin/env bash
# Launch soul2d-editor on a headless X11 server and on a headless Wayland compositor, and require a window
# that draws a textured model: a screenshot of each display must contain the fixture's colour.
# Linux only. Needs xvfb, weston, ImageMagick and Mesa's software GL. CI runs it after the build.
#
# Usage: tools/verify-linux-launch.sh [EVIDENCE_DIR]   (default build/verification/linux-launch)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/build/verification/linux-launch}
core=$root/out/rust/debug/soul2d-core
editor=$root/out/rust/debug/soul2d-editor
mkdir -p "$out"
export LIBGL_ALWAYS_SOFTWARE=1

# A 256x256 magenta square imported as one Part. No other pixel the editor draws has this colour.
build_fixture() {
    python3 - "$out/square.png" "$out/fixture.inx" <<'PY' | "$core" > "$out/fixture.jsonl"
import json, struct, sys, zlib
size = 256
row = b"\x00" + b"\xff\x00\xff\xff" * size
chunk = lambda kind, data: struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(row * size)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
for request in ({"op": "new"}, {"op": "merge.images", "paths": [sys.argv[1]]}, {"op": "save", "path": sys.argv[2]}):
    print(json.dumps(request))
PY
    if grep -q '"succeeded":false' "$out/fixture.jsonl" || [ ! -s "$out/fixture.inx" ]; then
        echo "FAIL: could not build the fixture" >&2
        cat "$out/fixture.jsonl" >&2
        exit 1
    fi
}

# Number of fixture-coloured pixels in a screenshot.
painted() {
    convert "$1" -alpha off -fuzz 8% -fill black +opaque '#ff00ff' -fill white -opaque '#ff00ff' \
        -format '%[fx:int(mean*w*h+0.5)]' info:
}

# launch NAME SHOOT: start the editor in the current display environment, wait until SHOOT's screenshot
# shows the model, and require the process to be alive at that moment.
launch() {
    local name=$1 shoot=$2 pixels=0
    INOCHI_CONFIG_PATH="$out/config-$name" "$editor" --open "$out/fixture.inx" > "$out/$name.log" 2>&1 &
    local pid=$!
    for _ in $(seq 60); do
        sleep 1
        kill -0 "$pid" 2>/dev/null || break
        if "$shoot" "$out/$name.png" 2>> "$out/$name-shoot.log"; then
            pixels=$(painted "$out/$name.png")
            [ "$pixels" -ge 1000 ] && break
        fi
    done
    local alive=no
    kill -0 "$pid" 2>/dev/null && alive=yes
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    echo "$name: alive=$alive fixture-coloured pixels=$pixels"
    if [ "$alive" != yes ] || [ "$pixels" -lt 1000 ]; then
        echo "FAIL: $name launch did not show the model" >&2
        tail -n 40 "$out/$name.log" "$out/$name-shoot.log" >&2 || true
        return 1
    fi
}

shoot_x11() { import -window root "$1"; }
shoot_wayland() {
    # Not on PATH in every package; weston only lets it capture because of --debug.
    local shooter
    shooter=$(command -v weston-screenshooter || find /usr/libexec /usr/lib -name weston-screenshooter 2>/dev/null | head -n 1)
    [ -n "$shooter" ] || { echo "weston-screenshooter not found" >&2; return 1; }
    (cd "$out/wayland-shots" && rm -f wayland-screenshot-*.png && "$shooter") || return 1
    mv "$out"/wayland-shots/wayland-screenshot-*.png "$1"
}

case ${2:-all} in
x11-inner) launch x11 shoot_x11 ;;
all)
    rm -f "$out/fixture.inx"
    build_fixture
    xvfb-run -a -s "-screen 0 1280x800x24" "$0" "$out" x11-inner

    export XDG_RUNTIME_DIR="$out/xdg-runtime"
    mkdir -p "$XDG_RUNTIME_DIR" "$out/wayland-shots"
    chmod 700 "$XDG_RUNTIME_DIR"
    weston --backend=headless --renderer=pixman --width=1280 --height=800 --socket=soul2d-launch --debug \
        > "$out/weston.log" 2>&1 &
    weston_pid=$!
    trap 'kill "$weston_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 20); do [ -S "$XDG_RUNTIME_DIR/soul2d-launch" ] && break; sleep 0.5; done
    env -u DISPLAY WAYLAND_DISPLAY=soul2d-launch bash -c "$(declare -f painted launch shoot_wayland); out='$out' editor='$editor' launch wayland shoot_wayland"
    echo "PASS: the editor opened and drew the model on X11 and Wayland; evidence in $out"
    ;;
esac
