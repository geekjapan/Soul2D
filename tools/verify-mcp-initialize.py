#!/usr/bin/env python3
"""Run `soul2d-core --mcp` (or any command that speaks the same stdio MCP) and check the initialize reply.

Usage: tools/verify-mcp-initialize.py [--editor] COMMAND [ARG...]
Example: tools/verify-mcp-initialize.py out/Soul2D.app/Contents/MacOS/soul2d-core --mcp

Sends one `initialize` request, closes stdin and requires a JSON-RPC result from `soul2d-core` with the
requested protocol version and a clean exit.
Used by the packaging checks (macOS bundle, Flatpak, MSI) to prove the shipped binary starts and answers.
Editor mode also requires a pristine editor state and an acknowledged clean Quit.
"""
import json
import subprocess
import sys

arguments = sys.argv[1:]
editor = bool(arguments and arguments[0] == '--editor')
if editor:
    arguments = arguments[1:]
if not arguments:
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
requests = [request]
if editor:
    requests += [
        {"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "resource://soul2d/editor/state"}},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "editor_quit", "arguments": {}}},
    ]
try:
    run = subprocess.run(arguments, input=''.join(json.dumps(item) + '\n' for item in requests),
                         capture_output=True, text=True, timeout=120)
except (OSError, subprocess.TimeoutExpired) as error:
    sys.exit(f"FAIL could not run {arguments}: {error}")

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
if editor:
    try:
        replies = [json.loads(line) for line in lines]
        if len(replies) != 3 or any(item.get('jsonrpc') != '2.0' or item.get('id') != index
                                    or 'error' in item for index, item in enumerate(replies, 1)):
            raise ValueError('Expected exactly three matching JSON-RPC replies')
        state = json.loads(replies[1]['result']['contents'][0]['text'])
        quit_result = replies[2]['result']
        if (state['editor'] is not True or state['document']['path'] is not None
                or state['document']['modified'] is not False or quit_result.get('isError') is not False
                or quit_result['structuredContent']['quit']['state'] != 'accepted'):
            raise ValueError('Expected pristine editor and accepted Quit')
    except (IndexError, KeyError, TypeError, ValueError) as error:
        sys.exit(f"FAIL editor stdio acceptance: {error}\nstdout: {run.stdout!r}\nstderr: {run.stderr!r}")
    print('PASS editor state and clean Quit over stdio')
print(f"PASS initialize answered by {info['name']} {info.get('version')}, protocol {protocol}")
