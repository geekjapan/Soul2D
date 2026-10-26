#!/usr/bin/env python3
"""Run `soul2d-core --mcp` (or any command that speaks the same stdio MCP) and check the initialize reply.

Usage: tools/verify-mcp-initialize.py COMMAND [ARG...]
Example: tools/verify-mcp-initialize.py out/Soul2D.app/Contents/MacOS/soul2d-core --mcp

Sends one `initialize` request, closes stdin and requires a JSON-RPC result from `soul2d-core` with the
requested protocol version and a clean exit.
Used by the packaging checks (macOS bundle, Flatpak, MSI) to prove the shipped binary starts and answers.
"""
import json
import subprocess
import sys

if len(sys.argv) < 2:
    sys.exit(__doc__)

request = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "verify-mcp-initialize", "version": "0"},
    },
}
try:
    run = subprocess.run(sys.argv[1:], input=json.dumps(request) + "\n", capture_output=True, text=True, timeout=120)
except (OSError, subprocess.TimeoutExpired) as error:
    sys.exit(f"FAIL could not run {sys.argv[1:]}: {error}")

lines = [line for line in run.stdout.splitlines() if line.strip()]
try:
    reply = json.loads(lines[0])
    info = reply["result"]["serverInfo"]
except (IndexError, KeyError, ValueError):
    sys.exit(f"FAIL no initialize result (exit {run.returncode})\nstdout: {run.stdout!r}\nstderr: {run.stderr!r}")
protocol = reply["result"].get("protocolVersion")
if (reply.get("id") != 1 or info.get("name") != "soul2d-core" or run.returncode != 0
        or protocol != request["params"]["protocolVersion"]):
    sys.exit(f"FAIL unexpected reply or exit code {run.returncode}: {lines[0]}\nstderr: {run.stderr!r}")
print(f"PASS initialize answered by {info['name']} {info.get('version')}, protocol {protocol}")
