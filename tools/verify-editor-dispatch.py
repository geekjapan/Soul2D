#!/usr/bin/env python3
"""Check that every command the editor dispatches is a registered command (#99 section 7).

Reads the editor sources (`rust/src/editor_*.rs`, `rust/src/app_shell.rs`) and doc/commands.manifest.json.
Fails (exit 1) when:
- a request literal `"op": "<name>"` in the editor names a session operation that no manifest tool reaches
  (the tool of an operation is its name with `.` replaced by `_`), unless the name is a nested step of a larger
  request (NESTED below: mesh-edit steps and split steps, which are arguments, not commands);
- a D command id (`XCommand.Y`) in `editor_windows.rs` or `editor_ui.rs` is neither a manifest tool
  (`XCommand_Y`) nor in HOST_ENTRIES, the table of entries that end in a host dialog, an editor window or a
  Session operation under another name;
- a HOST_ENTRIES target names a tool the manifest does not have.
An operation held in a `const` and used as `json!({"op": CONST})` is not seen by the literal scan; the two such
constants of `editor_ui.rs` (`KEY_ALL`, `UNKEY_ALL`) are covered through HOST_ENTRIES.
Plain stdlib; reads files only.

usage: tools/verify-editor-dispatch.py [--manifest PATH] [--src DIR]
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OP = re.compile(r'"op"\s*:\s*"([A-Za-z0-9_.]+)"')
D_ID = re.compile(r'"([A-Z][A-Za-z0-9]*Command\.[A-Za-z0-9]+)"')

# Request fields named "op" inside the `steps` / `edits` of one command: mesh-edit steps built by
# `editor_mesh_tools.rs` (`push_edit`, `Draft::Edit`) and split steps built by `editor_part_split.rs`.
NESTED = {
    "add", "remove", "toggle", "insert", "move", "cut", "flip", "triangulate",
    "grid_create", "grid_move", "grid_add_axis", "grid_remove_axis",
    "box", "alpha", "part",
}

# D ids the editor dispatches that are not manifest tools under their own name: the tool each one ends in,
# or None for an entry that opens an editor window and runs no Session operation (GUI-only, as the
# inventory in doc/gui-operation-inventory.md lists it).
HOST_ENTRIES = {
    "FileCommand.NewFile": "new",
    "FileCommand.ShowOpenFileDialog": "open",
    "FileCommand.ShowSaveFileDialog": "save",
    "FileCommand.ShowSaveFileAsDialog": "save",
    "AnimationCommand.KeyAll": "animation_key_add_all",
    "AnimationCommand.UnkeyAll": "animation_key_delete_all",
    "EditCommand.ShowSettingsWindow": "editor_settings_set",
    "EditCommand.ShowCommandBrowserWindow": "editor_command_browser_set",
    "PaletteCommand.ListCommand": "editor_command_browser_set",
}


def main(argv):
    manifest = ROOT / "doc/commands.manifest.json"
    src = ROOT / "rust/src"
    args = iter(argv)
    for arg in args:
        if arg == "--manifest":
            manifest = Path(next(args))
        elif arg == "--src":
            src = Path(next(args))
        else:
            sys.exit(f"unknown argument {arg}")
    tools = {command["id"] for command in json.loads(manifest.read_text())["commands"]}
    files = sorted(src.glob("editor_*.rs")) + [src / "app_shell.rs"]
    files = [path for path in files if path.exists()]
    if not files:
        sys.exit(f"no editor sources under {src}")
    problems = []
    ops = {}
    ids = {}
    for path in files:
        for number, line in enumerate(path.read_text().splitlines(), 1):
            for op in OP.findall(line):
                ops.setdefault(op, f"{path.name}:{number}")
            for d_id in D_ID.findall(line):
                ids.setdefault(d_id, f"{path.name}:{number}")
    for op, where in sorted(ops.items()):
        if op in NESTED:
            continue
        if op.replace(".", "_") not in tools:
            problems.append(f"{where}: operation {op} has no manifest tool {op.replace('.', '_')}")
    for d_id, where in sorted(ids.items()):
        if d_id.replace(".", "_") in tools:
            continue
        if d_id not in HOST_ENTRIES:
            problems.append(f"{where}: command id {d_id} is neither a manifest tool nor a HOST_ENTRIES row")
    for d_id, target in sorted(HOST_ENTRIES.items()):
        if target is not None and target not in tools:
            problems.append(f"HOST_ENTRIES: {d_id} names tool {target}, which the manifest does not have")
    if problems:
        print("\n".join(problems))
        sys.exit(1)
    gui_only = sorted(d_id for d_id, target in HOST_ENTRIES.items() if target is None)
    print(
        f"ok: {len(ops) - len(ops.keys() & NESTED)} editor operations and {len(ids)} D command ids all reach a "
        f"manifest tool; GUI-only entries: {', '.join(gui_only)}"
    )


if __name__ == "__main__":
    main(sys.argv[1:])
