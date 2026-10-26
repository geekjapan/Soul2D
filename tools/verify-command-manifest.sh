#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
run() {
  cargo run --quiet --offline --manifest-path rust/Cargo.toml --target-dir out/rust --bin soul2d-core -- "$@"
}
run --export-command-manifest doc/commands.manifest.json
run --export-command-manifest doc/commands.manifest.json --check
python3 -m json.tool doc/commands.manifest.json >/dev/null
echo "commands.manifest.json: export stable and valid JSON"
python3 tools/verify-operating-skill.py
python3 tools/verify-editor-dispatch.py
python3 tools/gen-gui-entries.py --check
python3 tools/gen-edit-ops.py --check
