#!/usr/bin/env python3
"""Check that the operating skill maps cover the MCP command manifest.

Fails (exit 1) when a tool in doc/commands.manifest.json has no table row in any family map, or when a family
map lists a tool that the manifest does not have. A map "lists" a tool in the first cell of a table row, written
as backticked names, for example:  | `node_rename` / `NodeCommand_SetNodeName` | rename a node |
A name mentioned only in prose does not count.
Also fails when the backticked text of a row's second cell does not mention every property that the manifest
lists in the tool's `inputSchema.required`, so a caller who forms a call from the row is not missing an argument. A D alias whose
schema requires fewer keys than the host needs (the `context` ones) still needs a row that shows `context`.
A row that lists several tools is checked as a whole: a key shown for one of them satisfies the others, so give
tools with different shapes their own rows.
Also fails when the "Tool rows" number of a map in the table of SKILL.md (beside the families directory) is not the
number of tools the map lists, when a map has no row or more than one, or when a row links a map that is not there.
A merge of two branches that each add a tool leaves that number one short.
Plain stdlib; reads files only.

usage: tools/verify-operating-skill.py [--manifest PATH] [--families DIR]
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TICKS = re.compile(r"`([^`\s]+)`")
ROW = re.compile(r"^\|\s*((?:`[^`\s]+`(?:\s*/\s*)?)+)\s*\|")
CELL = re.compile(r"(?<!\\)\|")  # a pipe not escaped as \| splits cells
MAP_LINK = re.compile(r"\(families/([\w-]+\.md)\)")


def main(argv):
    manifest = ROOT / "doc/commands.manifest.json"
    families = ROOT / ".cursor/skills/operate-soul2d/families"
    args = iter(argv)
    for arg in args:
        if arg == "--manifest":
            manifest = Path(next(args))
        elif arg == "--families":
            families = Path(next(args))
        else:
            sys.exit(f"unknown argument {arg}")
    commands = {command["id"]: command for command in json.loads(manifest.read_text())["commands"]}
    tools = set(commands)
    maps = sorted(families.glob("*.md"))
    if not maps:
        sys.exit(f"no family maps in {families}")
    unknown = []
    omitted = []
    rows = {}
    listed_all = set()
    for path in maps:
        listed = set()
        for number, line in enumerate(path.read_text().splitlines(), 1):
            row = ROW.match(line)
            if row:
                shapes = " ".join(re.findall(r"`([^`]*)`", CELL.split(line)[2]))
                for name in TICKS.findall(row.group(1)):
                    listed.add(name)
                    if name not in tools:
                        unknown.append(f"{path.name}:{number}: {name}")
                        continue
                    for key in commands[name].get("inputSchema", {}).get("required", []):
                        if not re.search(rf"(?<![\w]){re.escape(key)}(?![\w])", shapes):
                            omitted.append(f"{path.name}:{number}: {name} requires {key}")
        if not listed:
            unknown.append(f"{path.name}: lists no tool rows")
        rows[path.name] = len(listed)
        listed_all |= listed
    stated = set()
    miscounted = []
    for line in (families.parent / "SKILL.md").read_text().splitlines():
        cells = CELL.split(line)
        link = MAP_LINK.search(cells[1]) if len(cells) > 3 else None
        if not link:
            continue
        name, said = link.group(1), cells[3].strip()
        if name in stated:
            miscounted.append(f"{name}: SKILL.md has more than one row for this map")
        stated.add(name)
        if name not in rows:
            miscounted.append(f"{name}: SKILL.md has a row for a map that is not there")
        elif said != str(rows[name]):
            miscounted.append(f"{name}: SKILL.md says {said or 'nothing'}, the map lists {rows[name]}")
    miscounted += [f"{name}: SKILL.md has no row for this map" for name in rows if name not in stated]
    missing = sorted(tools - listed_all)
    if missing:
        print(f"{len(missing)} manifest tool(s) with no table row in any family map:")
        print("\n".join(f"  {name}" for name in missing))
    if unknown:
        print(f"{len(unknown)} map entr(y/ies) not in the manifest:")
        print("\n".join(f"  {entry}" for entry in unknown))
    if omitted:
        print(f"{len(omitted)} required argument(s) not mentioned in the row of the tool:")
        print("\n".join(f"  {entry}" for entry in omitted))
    if miscounted:
        print(f"{len(miscounted)} row(s) of the SKILL.md table that do not match the maps:")
        print("\n".join(f"  {entry}" for entry in miscounted))
    if missing or unknown or omitted or miscounted:
        return 1
    for name, count in rows.items():
        print(f"{count:4d}  {name}  (tools listed in table rows)")
    print(f"ok: {len(tools)} manifest tools, {len(maps)} family maps, every tool has a table row, required arguments shown, "
          "SKILL.md counts match")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
