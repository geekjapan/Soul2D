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

run_ime_probe() {
    local backend=$1 status=0
    timeout --kill-after=5s 30s "${SOUL2D_IME_ADAPTER_PROBE:?compiled probe required}" "$backend" \
        > "$out/ime-$backend.log" 2>&1 || status=$?
    cat "$out/ime-$backend.log"
    echo "ime-$backend: exit=$status scope=ACTUAL_ADAPTER_SYNTHETIC_INGRESS_NOT_PHYSICAL_IME" \
        | tee "$out/ime-$backend.status"
    return "$status"
}

query_wayland_capabilities() {
    local status=0
    timeout --kill-after=5s 30s "${SOUL2D_WAYLAND_CAPABILITIES:?compiled query required}" \
        > "$out/wayland-ime-capabilities.log" 2>&1 || status=$?
    cat "$out/wayland-ime-capabilities.log"
    echo "wayland-ime-capabilities: exit=$status scope=OWNED_WESTON_REGISTRY_CAPABILITIES_ONLY" \
        | tee "$out/wayland-ime-capabilities.status"
    return "$status"
}

stop_native_group() {
    [ -n "${native_pid:-}" ] || return 0
    kill -TERM -- "-$native_pid" 2>/dev/null || true
    for _ in $(seq 300); do
        kill -0 -- "-$native_pid" 2>/dev/null || break
        sleep 0.05
    done
    kill -KILL -- "-$native_pid" 2>/dev/null || true
    wait "$native_pid" 2>/dev/null || true
    if kill -0 -- "-$native_pid" 2>/dev/null; then
        echo "FAIL: owned native IME process group survived cleanup" >&2
        return 1
    fi
    native_pid=
    echo "native-ime process group cleanup: PASS"
}

run_native_ime() {
    local runtime="$probe_dir/native-runtime" status=0
    mkdir -m 700 "$runtime"
    setsid python3 - "$runtime" "$out" "$probe_dir/native-ime-probe" <<'PY' &
import os, pathlib, signal, subprocess, sys, time
runtime, out, executable = map(pathlib.Path, sys.argv[1:])
out = out.resolve()
env = os.environ.copy()
for key in ("DISPLAY", "WAYLAND_SOCKET", "WAYLAND_DISPLAY", "SWAYSOCK", "DBUS_SESSION_BUS_ADDRESS"):
    env.pop(key, None)
env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS="headless", WLR_RENDERER="pixman")
config = runtime / "sway.conf"
config.write_text("xwayland disable\nseat seat0 fallback true\ninput * xkb_layout us\noutput * mode 640x480\n")
class Cancelled(Exception):
    pass
def cancel(number, _frame):
    raise Cancelled(number)
for number in (signal.SIGINT, signal.SIGTERM):
    signal.signal(number, cancel)
actors = []
status = 1
try:
    with (out / "native-ime-environment.log").open("w") as log:
        subprocess.run(["sway", "--version"], stdout=log, stderr=subprocess.STDOUT, check=True, timeout=5)
        subprocess.run(["dpkg-query", "-W", "sway"], stdout=log, stderr=subprocess.STDOUT, check=True, timeout=5)
    with (out / "native-ime-sway.log").open("w") as log:
        sway = subprocess.Popen(["sway", "--config", str(config), "--debug"], env=env,
                                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        actors.append(sway)
    deadline = time.monotonic() + 10
    while True:
        if sway.poll() is not None:
            raise RuntimeError(f"owned Sway exited during startup: {sway.returncode}")
        paths = [p for p in runtime.glob("wayland-*") if p.is_socket()]
        if len(paths) > 1:
            raise RuntimeError("ambiguous private Wayland sockets")
        if paths:
            break
        if time.monotonic() >= deadline:
            raise TimeoutError("owned Sway startup incomplete")
        time.sleep(0.05)
    env.update(WAYLAND_DISPLAY=paths[0].name, WAYLAND_DEBUG="client")
    with (out / "native-ime-probe.log").open("w") as log, (out / "native-ime-protocol.log").open("w") as trace:
        probe = subprocess.Popen([str(executable), str(runtime)], env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=trace)
        actors.append(probe)
        status = probe.wait(timeout=45)
        if sway.poll() is not None:
            raise RuntimeError("owned Sway exited before probe acceptance")
except (TimeoutError, subprocess.TimeoutExpired):
    status = 124
    print("native-ime: bounded experiment incomplete; no safe-rejection conclusion", flush=True)
except Cancelled as error:
    status = 128 + error.args[0]
finally:
    for number in (signal.SIGINT, signal.SIGTERM):
        signal.signal(number, signal.SIG_IGN)
    for actor in reversed(actors):
        if actor.poll() is None:
            actor.terminate()
    for actor in reversed(actors):
        try:
            actor.wait(timeout=3)
        except subprocess.TimeoutExpired:
            actor.kill()
            actor.wait(timeout=2)
    print("native-ime owned direct children reaped=true", flush=True)
sys.exit(status)
PY
    native_pid=$!
    wait "$native_pid" || status=$?
    stop_native_group || status=1
    cat "$out/native-ime-environment.log" "$out/native-ime-probe.log" 2>/dev/null || true
    if [ "$status" -ne 0 ]; then
        tail -n 60 "$out/native-ime-sway.log" "$out/native-ime-protocol.log" >&2 || true
    fi
    echo "native-ime: exit=$status scope=LOCKED_NATIVE_EFRAME_DIAGNOSTIC_NOT_PRODUCT_ACCEPTANCE" \
        | tee "$out/native-ime.status"
    return "$status"
}

shoot_wayland() {
    # Not on PATH in every package; weston only lets it capture because of --debug.
    local shooter
    shooter=$(command -v weston-screenshooter || find /usr/libexec /usr/lib -name weston-screenshooter 2>/dev/null | head -n 1)
    [ -n "$shooter" ] || { echo "weston-screenshooter not found" >&2; return 1; }
    (cd "$out/wayland-shots" && rm -f wayland-screenshot-*.png && "$shooter") || return 1
    mv "$out"/wayland-shots/wayland-screenshot-*.png "$1"
}

case ${2:-all} in
x11-inner) run_ime_probe x11; launch x11 shoot_x11 ;;
all)
    probe_dir=$(mktemp -d "${TMPDIR:-/tmp}/soul2d-ime-probe.XXXXXX")
    weston_pid=
    native_pid=
    cleanup() {
        stop_native_group || true
        if [ -n "$weston_pid" ]; then
            kill "$weston_pid" 2>/dev/null || true
            wait "$weston_pid" 2>/dev/null || true
        fi
        rm -rf -- "$probe_dir"
        [ ! -e "$probe_dir" ] && echo "ime-probe temporary executable cleanup: PASS"
    }
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    libraries=("$root"/out/rust/debug/deps/libegui_winit-*.rlib)
    if [ "${#libraries[@]}" -ne 1 ] || [ ! -f "${libraries[0]}" ]; then
        echo "FAIL: expected exactly one compiled egui-winit rlib" >&2
        exit 1
    fi
    export SOUL2D_IME_ADAPTER_PROBE="$probe_dir/ime-adapter-probe"
    rustc --edition=2021 --crate-name soul2d_ime_adapter_probe "$root/tools/ime-adapter-probe.rs" \
        --extern "egui_winit=${libraries[0]}" -L "dependency=$root/out/rust/debug/deps" \
        -o "$SOUL2D_IME_ADAPTER_PROBE"
    {
        for crate in wayland_client wayland_protocols wayland_protocols_misc winit; do
            count=0
            for library in "$root"/out/rust/debug/deps/lib"$crate"-*.rlib; do
                [ -f "$library" ] || continue
                count=$((count + 1))
                printf 'rlib.%s.file=%s\n' "$crate" "${library##*/}"
            done
            printf 'rlib.%s.count=%s\n' "$crate" "$count"
        done
        for tool in weston-keyboard weston-simple-im weston-editor wayland-info; do
            command_path=$(command -v "$tool" || true)
            printf 'executable.%s.path=%s\n' "$tool" "${command_path:-NOT_ON_PATH}"
            for prefix in /usr/libexec /usr/lib/weston; do
                if [ -x "$prefix/$tool" ]; then
                    printf 'executable.%s.known=%s\n' "$tool" "$prefix/$tool"
                fi
            done
        done
    } > "$out/wayland-ime-inventory.log"
    cat "$out/wayland-ime-inventory.log"
    wayland_libraries=("$root"/out/rust/debug/deps/libwayland_client-*.rlib)
    if [ "${#wayland_libraries[@]}" -ne 1 ] || [ ! -f "${wayland_libraries[0]}" ]; then
        echo "FAIL: expected exactly one compiled wayland-client rlib" >&2
        exit 1
    fi
    export SOUL2D_WAYLAND_CAPABILITIES="$probe_dir/wayland-ime-capabilities"
    rustc --edition=2021 --crate-name soul2d_wayland_ime_capabilities "$root/tools/wayland-ime-capabilities.rs" \
        --extern "wayland_client=${wayland_libraries[0]}" -L "dependency=$root/out/rust/debug/deps" \
        -o "$SOUL2D_WAYLAND_CAPABILITIES"
    native_args=()
    for crate in eframe wayland_client wayland_protocols_misc; do
        libraries=("$root"/out/rust/debug/deps/lib"$crate"-*.rlib)
        if [ "${#libraries[@]}" -ne 1 ] || [ ! -f "${libraries[0]}" ]; then
            echo "FAIL: expected exactly one compiled $crate rlib for native IME" >&2
            exit 1
        fi
        printf 'native-rlib.%s.file=%s\n' "$crate" "${libraries[0]##*/}"
        native_args+=(--extern "$crate=${libraries[0]}")
    done
    rustc --edition=2021 --crate-name soul2d_native_ime_probe "$root/tools/native-ime-probe.rs" \
        "${native_args[@]}" -L "dependency=$root/out/rust/debug/deps" -o "$probe_dir/native-ime-probe"
    rm -f "$out/fixture.inx"
    build_fixture
    xvfb-run -a -s "-screen 0 1280x800x24" "$0" "$out" x11-inner

    export XDG_RUNTIME_DIR="$out/xdg-runtime"
    mkdir -p "$XDG_RUNTIME_DIR" "$out/wayland-shots"
    chmod 700 "$XDG_RUNTIME_DIR"
    weston --backend=headless --renderer=pixman --width=1280 --height=800 --socket=soul2d-launch --debug \
        > "$out/weston.log" 2>&1 &
    weston_pid=$!
    for _ in $(seq 20); do [ -S "$XDG_RUNTIME_DIR/soul2d-launch" ] && break; sleep 0.5; done
    env -u DISPLAY -u WAYLAND_SOCKET WAYLAND_DISPLAY=soul2d-launch bash -c \
        "$(declare -f query_wayland_capabilities); out=\$1; query_wayland_capabilities" _ "$out"
    env -u DISPLAY WAYLAND_DISPLAY=soul2d-launch bash -c \
        "$(declare -f run_ime_probe); out=\$1; run_ime_probe wayland" _ "$out"
    env -u DISPLAY WAYLAND_DISPLAY=soul2d-launch bash -c "$(declare -f painted launch shoot_wayland); out='$out' editor='$editor' launch wayland shoot_wayland"
    run_native_ime
    echo "PASS: the editor opened and drew the model on X11 and Wayland; evidence in $out"
    ;;
esac
