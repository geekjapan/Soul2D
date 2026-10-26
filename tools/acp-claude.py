#!/usr/bin/env python3
"""ACP agent that runs the installed `claude` CLI (Claude Code) headless, for the editor's agent panel or any ACP
client (`ACP.Command`: `python3 -I tools/acp-claude.py`).

Speaks ACP v1 (JSON-RPC, one object per line or `Content-Length` frames) on stdin/stdout. `initialize` advertises
HTTP MCP support, so the client offers its MCP server in `session/new`; the server is handed to `claude -p` as its
only MCP configuration (`--strict-mcp-config`). The built-in tools are disabled but for the two that list and read
MCP resources (`--tools ListMcpResourcesTool,ReadMcpResourceTool`; the editor's briefing points the agent at the
skill and state resources), and the user's own settings, hooks and skills are left out (`--setting-sources ""`), so
the agent can act on the document only through the offered MCP server, under the host's own approval rules. Each
`session/prompt` is one user turn of the same claude process (`--input-format stream-json`); the assistant's text
and tool calls stream back as `session/update` notifications and the turn ends with `stopReason`. `session/cancel`
ends the claude process and answers the running prompt with `cancelled`. Sessions are not saved to disk
(`--no-session-persistence`): the bridge offers no `session/load`.

Environment: `ACP_CLAUDE_BIN` (default `claude`), `ACP_CLAUDE_MODEL` (default `claude-haiku-5-5`),
`ACP_CLAUDE_MAX_TURNS` (default 40), `ACP_CLAUDE_MAX_BUDGET_USD` (default 2), `ACP_CLAUDE_ARGS` (extra arguments,
shell-split), `ACP_CLAUDE_LOG` (a file that receives every `session/update` and each turn's `stopReason` as JSON lines,
so a run driven through the editor's panel can be checked afterwards). Standard library only; run with `python3 -I`.
"""
import json
import os
import shlex
import signal
import subprocess
import sys
import threading

PROTOCOL_VERSION = 1
RESOURCE_TOOLS = ["ListMcpResourcesTool", "ReadMcpResourceTool"]  # Claude Code's built-in readers of MCP resources


def read_message(stream):
    """Next JSON-RPC message and whether it came framed, or None at end of input."""
    while True:
        line = stream.readline()
        if not line:
            return None
        text = line.decode("utf-8", "replace").strip()
        if not text:
            continue
        if text.lower().startswith("content-length"):
            length = int(text.split(":", 1)[1].strip())
            while True:  # the header block ends with an empty line
                header = stream.readline()
                if not header or not header.strip():
                    break
            body = stream.read(length)
            try:
                return json.loads(body.decode("utf-8", "replace")), True
            except ValueError:
                continue
        try:
            return json.loads(text), False
        except ValueError:
            continue


class Bridge:
    def __init__(self, out):
        self.out = out
        self.framed = False
        self.lock = threading.Lock()
        self.claude = None
        self.session_id = None
        self.prompt_id = None
        self.next_id = 1000
        path = os.environ.get("ACP_CLAUDE_LOG")
        self.log = open(path, "a", encoding="utf-8") if path else None

    def record(self, entry):
        if self.log is not None:
            with self.lock:
                self.log.write(json.dumps(entry, ensure_ascii=False) + "\n")
                self.log.flush()

    def write(self, message):
        payload = json.dumps(message, ensure_ascii=False)
        with self.lock:
            if self.framed:
                self.out.write(f"Content-Length: {len(payload.encode())}\r\n\r\n{payload}".encode())
            else:
                self.out.write((payload + "\n").encode())
            self.out.flush()

    def reply(self, id, result):
        self.write({"jsonrpc": "2.0", "id": id, "result": result})

    def fail(self, id, code, message):
        self.write({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})

    def update(self, update):
        update["sessionId"] = self.session_id
        self.write({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": self.session_id, "update": update}})
        self.record({"update": update})

    def handle(self, message):
        method = message.get("method")
        id = message.get("id")
        params = message.get("params") or {}
        if method is None:
            return  # a response to nothing we asked
        if method == "initialize":
            self.reply(id, {
                "protocolVersion": PROTOCOL_VERSION,
                "agentCapabilities": {"loadSession": False, "mcpCapabilities": {"http": True, "sse": False}},
                "agentInfo": {"name": "acp-claude", "version": "0.1.0"},
                "authMethods": [],
            })
        elif method == "ping":
            self.reply(id, {})
        elif method == "session/new":
            try:
                self.start_claude(params)
            except OSError as error:
                self.fail(id, -32000, f"cannot start claude: {error}")
                return
            self.reply(id, {"sessionId": self.session_id})
        elif method == "session/prompt":
            if self.claude is None or params.get("sessionId") != self.session_id:
                self.fail(id, -32602, "unknown session")
            elif self.prompt_id is not None:
                self.fail(id, -32000, "a prompt is already running")
            else:
                self.prompt_id = id
                text = "\n\n".join(block.get("text", "") for block in params.get("prompt", []) if block.get("type") == "text")
                turn = {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}
                try:
                    self.claude.stdin.write((json.dumps(turn) + "\n").encode())
                    self.claude.stdin.flush()
                except OSError as error:
                    with self.lock:  # the pump may have ended this turn already, when claude exited
                        pending, self.prompt_id = self.prompt_id, None
                    if pending is not None:
                        self.fail(pending, -32000, f"claude is gone: {error}")
        elif method == "session/cancel":
            self.end_claude("cancelled")
        elif id is not None:
            self.fail(id, -32601, f"method not supported: {method}")

    def start_claude(self, params):
        servers = {}
        for server in params.get("mcpServers") or []:
            if server.get("type") not in (None, "http"):
                continue
            headers = server.get("headers") or []
            if isinstance(headers, list):
                headers = {h.get("name"): h.get("value") for h in headers if isinstance(h, dict) and h.get("name")}
            servers[server.get("name") or "soul2d"] = {"type": "http", "url": server.get("url", ""), "headers": headers}
        allowed = " ".join([f"mcp__{name}__*" for name in servers] + RESOURCE_TOOLS)
        command = [os.environ.get("ACP_CLAUDE_BIN", "claude"), "-p", "--input-format", "stream-json", "--output-format", "stream-json",
                   "--verbose", "--tools", ",".join(RESOURCE_TOOLS), "--strict-mcp-config",
                   "--mcp-config", json.dumps({"mcpServers": servers}),
                   "--allowedTools", allowed, "--setting-sources", "", "--disable-slash-commands",
                   "--no-session-persistence",
                   "--model", os.environ.get("ACP_CLAUDE_MODEL", "claude-haiku-5-5"),
                   "--max-turns", os.environ.get("ACP_CLAUDE_MAX_TURNS", "40"),
                   "--max-budget-usd", os.environ.get("ACP_CLAUDE_MAX_BUDGET_USD", "2")]
        command += shlex.split(os.environ.get("ACP_CLAUDE_ARGS", ""))
        env = {k: v for k, v in os.environ.items() if k != "CLAUDECODE"}  # claude refuses to nest otherwise
        cwd = params.get("cwd") or None
        self.claude = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.session_id = f"claude-{self.claude.pid}"
        threading.Thread(target=self.pump, args=(self.claude,), daemon=True).start()
        threading.Thread(target=self.drain_stderr, args=(self.claude,), daemon=True).start()

    def drain_stderr(self, claude):
        for line in claude.stderr:
            sys.stderr.buffer.write(line)
            sys.stderr.flush()

    def pump(self, claude):
        """Turns claude's event stream into session updates and ends each prompt at its `result`."""
        for line in claude.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            kind = event.get("type")
            if kind == "assistant":
                for block in event.get("message", {}).get("content", []):
                    if block.get("type") == "text" and block.get("text"):
                        self.update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": block["text"]}})
                    elif block.get("type") == "tool_use":
                        self.update({"sessionUpdate": "tool_call", "toolCallId": block.get("id", ""), "title": block.get("name", "tool"),
                                     "kind": "other", "status": "pending", "rawInput": block.get("input", {})})
            elif kind == "user":
                for block in event.get("message", {}).get("content", []):
                    if block.get("type") == "tool_result":
                        content = block.get("content")
                        if isinstance(content, list):
                            content = "\n".join(c.get("text", "") for c in content if isinstance(c, dict))
                        self.update({"sessionUpdate": "tool_call_update", "toolCallId": block.get("tool_use_id", ""),
                                     "status": "failed" if block.get("is_error") else "completed",
                                     "content": [{"type": "content", "content": {"type": "text", "text": str(content or "")}}]})
            elif kind == "result":
                usage = event.get("usage") if isinstance(event.get("usage"), dict) else {}
                self.update({"sessionUpdate": "usage_update", "usage": usage, "totalCostUsd": event.get("total_cost_usd"),
                             "numTurns": event.get("num_turns"), "durationMs": event.get("duration_ms")})
                if event.get("is_error") and event.get("result"):
                    self.update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": str(event["result"])}})
                stop = event.get("stop_reason") or ("end_turn" if event.get("subtype") == "success" else "refusal")
                if event.get("subtype") == "error_max_turns":
                    stop = "max_turn_requests"
                self.finish_prompt({"end_turn": "end_turn", "max_tokens": "max_tokens"}.get(stop, stop))
        self.finish_prompt("cancelled")

    def finish_prompt(self, stop_reason):
        with self.lock:  # the pump and serve threads can both end a turn; only one of them answers it
            id, self.prompt_id = self.prompt_id, None
        if id is not None:
            self.record({"stopReason": stop_reason})  # before the reply, so the log is whole when the turn ends
            self.reply(id, {"stopReason": stop_reason})

    def end_claude(self, stop_reason):
        claude, self.claude = self.claude, None
        if claude is not None and claude.poll() is None:
            claude.send_signal(signal.SIGTERM)
        self.finish_prompt(stop_reason)

    def serve(self, stream):
        while True:
            item = read_message(stream)
            if item is None:
                break
            message, framed = item
            self.framed = framed
            self.handle(message)
        self.end_claude("cancelled")


if __name__ == "__main__":
    Bridge(sys.stdout.buffer).serve(sys.stdin.buffer)
