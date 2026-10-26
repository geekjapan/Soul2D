#!/usr/bin/env python3
"""Report which MCP tools in doc/commands.manifest.json are named by a feature map.

Usage: coverage.py [--strict]   (--strict exits 1 when any tool is in no map)

A tool counts as mapped when a feature file names it in backticks or after `tools call`. D-era
names and their snake_case twins (NodeCommand_AddNode, node_create) are separate tools here.
"""
import json
from pathlib import Path
import re
import sys

skill = Path(__file__).resolve().parents[1]
root = skill.parents[2]
tools = [c["id"] for c in json.loads((root / "doc/commands.manifest.json").read_text())["commands"]]
maps = {p.name: p.read_text() for p in sorted((skill / "features").glob("*.md")) if p.name != "README.md"}
text = "\n".join(maps.values())
# ponytail: a one-word id that is also a field name (`bounds`) counts when the field is quoted;
# match on "tools call" only if that ever hides a real gap.
named = set(re.findall(r"`([A-Za-z0-9_]+)`", text)) | set(re.findall(r"tools call ([A-Za-z0-9_]+)", text))

families = {}
for tool in tools:
    family = tool.split("_")[0].removesuffix("Command").lower()
    families.setdefault(family, []).append(tool)

mapped = [tool for tool in tools if tool in named]
print(f"{len(mapped)} of {len(tools)} tools are named in {len(maps)} feature files")
print("family          mapped/total  unmapped")
for family, members in sorted(families.items()):
    missing = [tool for tool in members if tool not in named]
    shown = ", ".join(missing[:4]) + (f", +{len(missing) - 4} more" if len(missing) > 4 else "")
    print(f"{family:<15} {len(members) - len(missing):>3}/{len(members):<8} {shown}")
if "--strict" in sys.argv[1:] and len(mapped) != len(tools):
    sys.exit(1)
