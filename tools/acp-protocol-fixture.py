#!/usr/bin/env python3
"""Deterministic ACP peer used by the Rust/MCP behavioral regression."""

import argparse
import base64
import binascii
import json
import os
from pathlib import Path
import struct
import sys
import urllib.request
import zlib


def validate_png(data, expected_width=640, expected_height=480):
    signature = b"\x89PNG\r\n\x1a\n"
    if not data.startswith(signature):
        raise AssertionError("Rust Core screenshot has no PNG signature")

    offset = len(signature)
    chunk_count = 0
    image_header = None
    image_data = []
    saw_idat = False
    idat_closed = False
    saw_plte = False
    saw_iend = False
    while offset < len(data):
        if len(data) - offset < 12:
            raise AssertionError("PNG has a truncated chunk header")
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4:offset + 8]
        payload_start = offset + 8
        payload_end = payload_start + length
        chunk_end = payload_end + 4
        if chunk_end > len(data):
            raise AssertionError(f"PNG {kind!r} chunk extends past the image")
        payload = data[payload_start:payload_end]
        stored_crc = struct.unpack_from(">I", data, payload_end)[0]
        actual_crc = zlib.crc32(kind + payload) & 0xFFFFFFFF
        if actual_crc != stored_crc:
            raise AssertionError(f"PNG {kind!r} chunk CRC is invalid")
        if any(not (65 <= byte <= 90 or 97 <= byte <= 122) for byte in kind):
            raise AssertionError(f"PNG chunk has an invalid type: {kind!r}")
        if kind[2] & 0x20:
            raise AssertionError(f"PNG {kind!r} uses a lowercase reserved type byte")
        if chunk_count == 0 and kind != b"IHDR":
            raise AssertionError("PNG does not start with IHDR")

        if kind == b"IHDR":
            if chunk_count != 0 or image_header is not None or length != 13:
                raise AssertionError("PNG must contain one 13-byte IHDR first")
            image_header = struct.unpack(">IIBBBBB", payload)
            width, height, bit_depth, color_type, compression, filtering, interlace = image_header
            if (width, height) != (expected_width, expected_height):
                raise AssertionError(
                    f"PNG dimensions are {width}x{height}, expected {expected_width}x{expected_height}"
                )
            if (bit_depth, color_type, compression, filtering, interlace) != (8, 6, 0, 0, 0):
                raise AssertionError("PNG must use non-interlaced 8-bit RGBA data")
        elif kind == b"IDAT":
            if image_header is None or idat_closed or saw_iend:
                raise AssertionError("PNG IDAT chunks are missing IHDR or are not consecutive")
            saw_idat = True
            image_data.append(payload)
        elif kind == b"IEND":
            if length != 0 or not saw_idat:
                raise AssertionError("PNG IEND must be empty and follow image data")
            saw_iend = True
            offset = chunk_end
            break
        else:
            if (kind[0] & 0x20) == 0 and kind != b"PLTE":
                raise AssertionError(f"PNG contains unsupported critical chunk {kind!r}")
            if kind == b"PLTE":
                if saw_plte or saw_idat or length == 0 or length % 3 or length > 768:
                    raise AssertionError("PNG PLTE chunk is duplicated, misplaced, or malformed")
                saw_plte = True
            if saw_idat:
                idat_closed = True

        offset = chunk_end
        chunk_count += 1

    if not saw_iend or offset != len(data):
        raise AssertionError("PNG is missing IEND or has trailing bytes")

    width, height = image_header[:2]
    expected_raw_length = height * (1 + width * 4)
    inflater = zlib.decompressobj()
    raw = inflater.decompress(b"".join(image_data), expected_raw_length + 1)
    if len(raw) > expected_raw_length or inflater.unconsumed_tail:
        raise AssertionError("PNG image data expands beyond its declared dimensions")
    raw += inflater.flush()
    if (len(raw) != expected_raw_length or not inflater.eof or inflater.unused_data or
            inflater.unconsumed_tail):
        raise AssertionError("PNG image data is truncated, oversized, or has trailing compressed data")
    stride = width * 4
    if any(raw[row * (stride + 1)] > 4 for row in range(height)):
        raise AssertionError("PNG contains an unsupported scanline filter")
    return width, height


def write_inx_fixture(path):
    transform = {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]}
    model = {
        "meta": {"name": "ACP protocol regression", "version": "0.0.1"},
        "physics": {"pixelsPerMeter": 1000, "gravity": 9.8},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": True,
            "zsort": 0, "transform": transform, "lockToRoot": False,
            "pinToMesh": False,
            "children": [{
                "uuid": 2, "name": "Original", "type": "Node", "enabled": True,
                "zsort": 0, "transform": transform, "lockToRoot": False,
                "pinToMesh": False, "children": [], "future_node": {"keep": True},
            }],
        },
        "param": [], "automation": [], "animations": {}, "groups": [],
        "future_root": {
            "nested": [1, {"keep": "unknown"}],
            "large_integer": 18446744073709551617,
        },
    }
    raw = json.dumps(model, separators=(",", ":")).encode("utf-8")
    tail = (
        b"TEX_SECT" + struct.pack(">I", 0)
        + b"EXT_SECT" + struct.pack(">I", 1)
        + struct.pack(">I", 6) + b"future"
        + struct.pack(">I", 5) + b"\x00\xffABC"
        + b"FUTURE_TAIL"
    )
    Path(path).write_bytes(b"TRNSRTS\0" + struct.pack(">I", len(raw)) + raw + tail)


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def response(request, result):
    send({"jsonrpc": "2.0", "id": request["id"], "result": result})


def rpc_error(request, code, message):
    send({"jsonrpc": "2.0", "id": request["id"],
          "error": {"code": code, "message": message}})


def init_result(request_id, http):
    return {
        "jsonrpc": "2.0", "id": request_id,
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {"mcpCapabilities": {"http": http}},
        },
    }


class McpPeer:
    def __init__(self, endpoint):
        self.endpoint = endpoint
        self.next_id = 0

    def call(self, method, params=None):
        self.next_id += 1
        request_id = self.next_id
        payload = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            payload["params"] = params
        request = urllib.request.Request(
            self.endpoint,
            data=json.dumps(payload, separators=(",", ":")).encode("utf-8"),
            headers={"Content-Type": "application/json",
                     "Accept": "application/json, text/event-stream"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=15) as reply:
            body = reply.read()
        if not body:
            raise AssertionError(f"MCP {method} returned an empty response")
        result = json.loads(body)
        if result.get("id") != request_id:
            raise AssertionError(f"MCP {method} response id did not match: {result}")
        if "error" in result:
            raise AssertionError(f"MCP {method} failed: {result['error']}")
        return result.get("result", {})

    def notify(self, method, params=None):
        payload = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        request = urllib.request.Request(
            self.endpoint,
            data=json.dumps(payload, separators=(",", ":")).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=15) as reply:
            reply.read()

    def initialize(self):
        self.call("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "acp-protocol-fixture", "version": "1"},
        })
        self.notify("notifications/initialized")

    def resources_read(self, uri):
        result = self.call("resources/read", {"uri": uri})
        contents = result.get("contents", [])
        if not contents or not isinstance(contents[0].get("text"), str):
            raise AssertionError(f"MCP resource {uri} returned no JSON text: {result}")
        return json.loads(contents[0]["text"])

    def tool(self, name, arguments):
        return self.call("tools/call", {"name": name, "arguments": arguments})

    def tool_json(self, result):
        if isinstance(result.get("structuredContent"), dict):
            return result["structuredContent"]
        for item in result.get("content", []):
            if item.get("type") == "text":
                try:
                    decoded = json.loads(item["text"])
                except (KeyError, TypeError, json.JSONDecodeError):
                    continue
                if isinstance(decoded, dict):
                    return decoded
        return {}

    def require_tool_success(self, name, arguments):
        result = self.tool(name, arguments)
        body = self.tool_json(result)
        if result.get("isError") or body.get("succeeded") is False:
            raise AssertionError(f"MCP tool {name} failed: {result}")
        return result

    def require_node_name(self, node_id, expected):
        item = self.resources_read(f"resource://soul2d/resources/{node_id}").get("item")
        if not isinstance(item, dict) or item.get("name") != expected:
            raise AssertionError(f"node {node_id} name was {item!r}, expected {expected!r}")


class AcpFixture:
    def __init__(self, args):
        self.args = args
        self.initialize_count = 0
        self.session_count = 0
        self.http_supported = False
        self.session_id = ""
        self.permission_turn = 0

    def read(self):
        line = sys.stdin.readline()
        if not line:
            return None
        return json.loads(line)

    def check_new_session(self, request):
        params = request.get("params", {})
        cwd = params.get("cwd")
        expected = os.path.realpath(self.args.expect_cwd)
        if not isinstance(cwd, str) or not os.path.isabs(cwd) or cwd != os.path.normpath(cwd):
            raise AssertionError(f"session/new cwd is not normalized and absolute: {cwd!r}")
        if os.path.realpath(cwd) != expected:
            raise AssertionError(f"session/new cwd {cwd!r} did not match constructor cwd {expected!r}")
        servers = params.get("mcpServers")
        if self.http_supported:
            if not isinstance(servers, list) or len(servers) != 1:
                raise AssertionError(f"HTTP-capable session/new must advertise one server: {servers!r}")
            server = servers[0]
            if (server.get("type") != "http" or server.get("name") != "soul2d"
                    or server.get("url") != self.args.expect_url):
                raise AssertionError(
                    f"session/new did not advertise live MCP endpoint {self.args.expect_url!r}: {servers!r}"
                )
        elif servers != []:
            raise AssertionError(f"session/new advertised MCP without HTTP support: {servers!r}")

    def handle_initialize(self, request):
        self.initialize_count += 1
        request_id = request.get("id")
        if self.args.mode == "sequence":
            if self.initialize_count == 1:
                self.http_supported = True
                send({
                    "jsonrpc": "2.0", "method": "session/update",
                    "params": {"sessionId": "sequence-session",
                               "update": {"sessionUpdate": "agent_message_chunk",
                                          "content": {"type": "text", "text": "queued-before-init"}}},
                })
                send({"jsonrpc": "2.0", "id": request_id + 1000,
                      "result": {"protocolVersion": 1, "wrongRequest": True}})
                send(init_result(request_id, True))
                return
            if self.initialize_count == 2:
                self.http_supported = False
                send(init_result(request_id, False))
                return
            if self.initialize_count == 3:
                self.http_supported = True
                send(init_result(request_id, True))
                return
            if self.initialize_count == 4:
                self.http_supported = False
                send({"jsonrpc": "2.0", "id": request_id,
                      "result": {"protocolVersion": 99,
                                 "agentCapabilities": {"mcpCapabilities": {"http": True}}}})
                return
            if self.initialize_count == 5:
                self.http_supported = False
                send(init_result(request_id, False))
                return
            if self.initialize_count == 6:
                self.http_supported = True
                send(init_result(request_id, True))
                return
            if self.initialize_count == 7:
                self.http_supported = False
                send({
                    "jsonrpc": "2.0", "id": request_id,
                    "error": {"code": -32001, "message": "fixture initialize failure"},
                    "result": {"protocolVersion": 1,
                               "agentCapabilities": {"mcpCapabilities": {"http": True}}},
                })
                return
            if self.initialize_count == 8:
                self.http_supported = False
                send(init_result(request_id, False))
                return
            raise AssertionError("sequence fixture received an unexpected initialize")

        if self.args.mode in ("workflow", "cancel", "cancel-permissions", "permission-layout", "session-error"):
            self.http_supported = True
            send(init_result(request_id, True))
            return
        if self.args.mode == "eof":
            self.http_supported = False
            send(init_result(request_id, False))
            return
        raise AssertionError(f"unknown initialize mode: {self.args.mode}")

    def handle_new_session(self, request):
        self.check_new_session(request)
        self.session_count += 1
        if self.args.mode == "session-error":
            rpc_error(request, -32002, "fixture session/new rejected")
            return
        self.session_id = f"{self.args.mode}-session-{self.session_count}-" + chr(34) + chr(92)
        if self.args.mode == "workflow":
            source = Path(self.args.source)
            if source.exists():
                raise AssertionError(f"temporary source fixture already exists: {source}")
            write_inx_fixture(source)
        response(request, {"sessionId": self.session_id})

    def send_permission(self, request_id, title, tool_id=None):
        send({
            "jsonrpc": "2.0", "id": request_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": self.session_id,
                "reason": title,
                "toolCall": {"toolCallId": tool_id or f"fixture-{request_id}",
                             "title": title, "kind": "edit", "rawInput": {}},
                "options": [
                    {"optionId": "allow:once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "allow:session", "name": "Allow for this session", "kind": "allow_always"},
                    {"optionId": "allow:always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "reject:once", "name": "Reject once", "kind": "reject_once"},
                ],
            },
        })

    def permission(self, request_id, title, tool_id=None):
        self.send_permission(request_id, title, tool_id)
        result = self.read()
        if not result or result.get("id") != request_id or "result" not in result:
            raise AssertionError(f"permission response did not match request {request_id}: {result!r}")
        outcome = result["result"].get("outcome", {})
        return outcome.get("outcome"), outcome.get("optionId")

    def stage(self, name):
        send({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {
                "sessionId": self.session_id,
                "update": {"sessionUpdate": "agent_message_chunk",
                           "content": {"type": "text", "text": f"fixture-stage:{name}"}},
            },
        })

    def workflow(self, request):
        request_id = request["id"]
        send({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": self.session_id,
                       "update": {"sessionUpdate": "agent_message_chunk",
                                  "content": {"type": "text", "text": "fixture-stage:started"}}},
        })
        send({"jsonrpc": "2.0", "id": request_id + 1000,
              "result": {"stopReason": "wrong-request"}})

        mcp = McpPeer(self.args.expect_url)
        mcp.initialize()
        tool_names = {tool.get("name") for tool in mcp.call("tools/list").get("tools", [])}
        required = {
            "FileCommand_OpenFile", "FileCommand_SaveFile", "NodeCommand_SetNodeName",
            "EditCommand_Undo", "EditCommand_Redo", "ViewCommand_CaptureLiveScreenshot",
        }
        missing = required - tool_names
        if missing:
            raise AssertionError(f"embedded MCP server is missing Rust tools: {sorted(missing)}")
        mcp.call("resources/list")

        mcp.require_tool_success("FileCommand_OpenFile", {"file": self.args.source})
        explore = mcp.resources_read("resource://soul2d/resources/find?selector=*")
        if not isinstance(explore.get("items"), list):
            raise AssertionError(f"resources/find returned no model tree: {explore!r}")
        mcp.require_node_name(2, "Original")
        self.stage("ExploreRead")

        outcome, option = self.permission(request["id"], "Allow the fixture edit")
        if outcome != "selected" or option != "allow:once":
            raise AssertionError(f"fixture expected permission success, got {(outcome, option)!r}")
        edit = mcp.require_tool_success("NodeCommand_SetNodeName", {
            "context": {"nodes": [2]}, "newNames": ["Renamed by ACP fixture"],
        })
        del edit
        mcp.require_node_name(2, "Renamed by ACP fixture")
        self.stage("Edit")

        denied, _ = self.permission("permission:deny:once", "Deny the optional fixture edit")
        if denied != "cancelled":
            raise AssertionError(f"fixture expected deterministic permission denial, got {denied!r}")

        rejected = mcp.tool("NodeCommand_SetNodeName", {
            "context": {"nodes": [999999]}, "newNames": ["Must be rejected"],
        })
        rejected_body = mcp.tool_json(rejected)
        if not (rejected.get("isError") or rejected_body.get("succeeded") is False or "error" in rejected_body):
            raise AssertionError(f"invalid MCP edit was not rejected: {rejected!r}")
        mcp.require_node_name(2, "Renamed by ACP fixture")
        self.stage("Reject")

        outcome, option = self.permission("permission:reject:checkpoint", "Confirm the rejected edit")
        if outcome != "selected" or option != "allow:once":
            raise AssertionError(f"fixture expected permission success, got {(outcome, option)!r}")
        mcp.require_tool_success("EditCommand_Undo", {})
        mcp.require_node_name(2, "Original")
        outcome, option = self.permission("permission:undo:checkpoint", "Confirm undo history")
        if outcome != "selected" or option != "allow:once":
            raise AssertionError(f"fixture expected permission success, got {(outcome, option)!r}")
        mcp.require_tool_success("EditCommand_Redo", {})
        mcp.require_node_name(2, "Renamed by ACP fixture")
        outcome, option = self.permission("permission:redo:checkpoint", "Confirm redo history")
        if outcome != "selected" or option != "allow:once":
            raise AssertionError(f"fixture expected permission success, got {(outcome, option)!r}")
        self.stage("UndoRedo")

        mcp.require_tool_success("FileCommand_SaveFile", {"file": self.args.save_copy})
        mcp.require_tool_success("FileCommand_OpenFile", {"file": self.args.save_copy})
        mcp.require_node_name(2, "Renamed by ACP fixture")
        self.stage("SaveOpen")

        screenshot = mcp.tool("ViewCommand_CaptureLiveScreenshot", {})
        if screenshot.get("isError"):
            raise AssertionError(f"Rust Core PNG tool failed: {screenshot!r}")
        image = next((item for item in screenshot.get("content", [])
                      if item.get("type") == "image" and item.get("mimeType") == "image/png"), None)
        if image is None or not isinstance(image.get("data"), str):
            raise AssertionError(f"Rust Core MCP tool did not return a PNG image: {screenshot!r}")
        try:
            png_data = base64.b64decode(image["data"], validate=True)
        except (binascii.Error, ValueError) as error:
            raise AssertionError(f"Rust Core MCP screenshot has invalid base64: {error}") from error
        width, height = validate_png(png_data)
        Path(self.args.save_copy).with_name("viewport.png").write_bytes(png_data)
        metadata = screenshot.get("_meta")
        if isinstance(metadata, dict):
            if metadata.get("width", width) != width or metadata.get("height", height) != height:
                raise AssertionError(f"PNG dimensions disagree with MCP metadata: {metadata!r}")
        self.stage("PNG")
        response(request, {"stopReason": "end_turn"})

    def cancel_prompt(self, request):
        prompt_id = request["id"]
        send({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": self.session_id,
                       "update": {"sessionUpdate": "agent_message_chunk",
                                  "content": {"type": "text", "text": "fixture-stage:cancel-wait"}}},
        })
        while True:
            message = self.read()
            if message is None:
                return
            if message.get("method") == "session/cancel":
                if message.get("params", {}).get("sessionId") != self.session_id:
                    raise AssertionError(f"cancel used the wrong session: {message!r}")
                response(request, {"stopReason": "cancelled"})
                return

    def cancel_permissions(self, request):
        self.permission_turn += 1
        turn = self.permission_turn
        if turn > 3:
            raise AssertionError("unexpected extra permission turn")
        first = 40 + turn
        second = f'pending:{turn}:"\\権限'
        late = f'late:{turn}:"\\権限'
        self.send_permission(first, "First permission")
        if turn < 3:
            self.send_permission(second, "Second permission")
        expected = {first: "selected" if turn in (1, 3) else "cancelled"}
        if turn < 3:
            expected.update({second: "cancelled", late: "cancelled"})
        seen = {}
        cancel_seen = False
        while len(seen) < len(expected) or (turn < 3 and not cancel_seen):
            message = self.read()
            if message is None:
                raise AssertionError("client closed before permission cancellation finished")
            if message.get("method") == "session/cancel":
                if turn == 3 or message.get("params", {}).get("sessionId") != self.session_id:
                    raise AssertionError(f"unexpected cancel: {message!r}")
                if turn == 1 and first not in seen:
                    raise AssertionError("selection-before-cancel was not sent before cancellation")
                if not cancel_seen:
                    self.send_permission(late, "Permission arriving after cancel")
                cancel_seen = True
                continue
            request_id = message.get("id")
            if request_id not in expected or request_id in seen or "result" not in message:
                raise AssertionError(f"unexpected or duplicate permission response: {message!r}")
            outcome = message["result"].get("outcome", {})
            if outcome.get("outcome") != expected[request_id]:
                raise AssertionError(f"wrong permission outcome: {message!r}")
            if outcome.get("outcome") == "selected" and cancel_seen:
                raise AssertionError("a permission was selected after cancellation")
            if expected[request_id] == "selected" and outcome.get("optionId") != "allow:once":
                raise AssertionError(f"selected option ID changed: {message!r}")
            seen[request_id] = outcome["outcome"]
        if turn == 3:
            outcome, option_id = self.permission(first, "Reused permission ID in the same prompt",
                                                 f"fixture-reused-{first}")
            if outcome != "selected" or option_id != "allow:once":
                raise AssertionError("reused permission ID did not receive its own selected response")
        response(request, {"stopReason": "cancelled" if turn < 3 else "end_turn",
                           "fixturePermissionTurn": turn,
                           "fixturePermissionResponses": len(seen) + (turn == 3)})

    def permission_layout(self, request):
        self.permission_turn += 1
        if self.permission_turn == 2:
            self.cancel_prompt(request)
            return
        expected = {1: "reject:once", 3: "allow:once"}.get(self.permission_turn)
        if expected is None:
            raise AssertionError("unexpected extra layout turn")
        outcome, option_id = self.permission(f"layout:{self.permission_turn}", "Four-option permission")
        if outcome != "selected" or option_id != expected:
            raise AssertionError(f"wrong layout permission response: {outcome}, {option_id}")
        self.stage(expected)
        response(request, {"stopReason": "end_turn"})

    def run(self):
        while True:
            request = self.read()
            if request is None:
                return
            try:
                method = request.get("method")
                if self.args.mode == "cancel-permissions" and method is None:
                    raise AssertionError(f"unsolicited client response between turns: {request!r}")
                if method == "initialize":
                    self.handle_initialize(request)
                elif method == "session/new":
                    self.handle_new_session(request)
                elif method == "session/prompt":
                    if request.get("params", {}).get("sessionId") != self.session_id:
                        raise AssertionError(f"prompt used the wrong session: {request!r}")
                    if self.args.mode == "workflow":
                        self.workflow(request)
                    elif self.args.mode == "cancel":
                        self.cancel_prompt(request)
                    elif self.args.mode == "cancel-permissions":
                        self.cancel_permissions(request)
                    elif self.args.mode == "permission-layout":
                        self.permission_layout(request)
                    elif self.args.mode == "eof":
                        sys.stdout.flush()
                        sys.stdout.close()
                        return
                    else:
                        rpc_error(request, -32601, "prompt unsupported by this fixture mode")
                elif method == "session/cancel":
                    continue
                elif method == "ping" and self.args.mode == "cancel-permissions":
                    response(request, {})
                else:
                    rpc_error(request, -32601, f"unsupported fixture method: {method}")
            except Exception as error:
                sys.stderr.write(f"ACP fixture failure: {error}\n")
                if self.args.mode == "cancel-permissions":
                    return
                if "id" in request:
                    rpc_error(request, -32099, str(error))
                else:
                    return


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("sequence", "session-error", "workflow", "cancel",
                                          "cancel-permissions", "permission-layout", "eof"), required=True)
    parser.add_argument("--expect-cwd", required=True)
    parser.add_argument("--expect-url", required=True)
    parser.add_argument("--source")
    parser.add_argument("--save-copy")
    args = parser.parse_args()
    if args.mode == "workflow" and (not args.source or not args.save_copy):
        parser.error("workflow mode requires --source and --save-copy")
    AcpFixture(args).run()


if __name__ == "__main__":
    main()
