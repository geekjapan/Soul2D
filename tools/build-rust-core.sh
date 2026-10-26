#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec cargo build --offline --manifest-path "$root/rust/Cargo.toml" --target-dir "$root/out/rust" "$@"
