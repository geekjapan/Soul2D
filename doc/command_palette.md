# Command Palette and Result Model

Status: Rewritten for the Rust port. The earlier D design (`CommandResult`, `ExCommandResult!(T)`, `CreateResult!`,
`DeleteResult!`, `LoadResult!`, `Command.run` returning a struct) was never ported. Those types do not exist in
`rust/src`. This page describes what exists instead.

## Design intent that still applies

- Every command reports success or failure explicitly.
- A command that creates or deletes resources names them in its result, so a client need not re-query state.
- The palette and MCP use one result path, so a caller does not depend on how a command was started.

## The Rust result path

One shape serves every tool. There is no per-command result type.

1. `Session::handle_request` (`rust/src/lib.rs`) takes a JSON object `{"op": ..., ...}` and always returns
   `{"succeeded": bool, "status": "ok" | "error", "message": ..., "result": ...}`. The failure builder is `error()`
   (`succeeded: false`, `status: "error"`, `message`, `result: null`). A success carries `status: "ok"` and the
   operation's own JSON in `result`. A panic inside an operation becomes an `error("internal panic while handling
   request")`.
2. `mcp::call_tool` (`rust/src/mcp.rs`) wraps a `tools/call` around that response:
   - An unknown tool, a missing required argument, or an undeclared argument on a closed schema
     (`check_declared_arguments`), a failed `tool_request`, or a refusal by the host `guard` returns
     `tool_error(message)`: `{"isError": true, "content": [{"type": "text", "text": message}]}`.
   - `succeeded: false` returns `tool_error` with the response `message`, or `tool failed` when it has none.
   - Success returns `tool_reply(result)`: `isError: false`, the result as JSON text in `content[0].text`, and the same
     value as `structuredContent` when it is a JSON object. A result with a `png` field becomes an `image/png` content
     item plus a text item with the remaining fields.
   - A protocol-level problem is a JSON-RPC `error` instead (`rpc_error`), for example `-32601` method not found,
     `-32602` for a missing tool name or a bad resource URI, and `-32603` for an internal failure.
3. `soul2d-core` serves the same `Session` as JSONL (`handle_request`, one request per line) and as MCP
   (`handle_mcp`) in `rust/src/main.rs`. The editor reaches it through its own MCP path (`editor_ui.rs`).

A caller must therefore check the JSON-RPC `error`, the MCP `isError` flag, and, over JSONL, `succeeded`.

### Affected resources

Resource reporting is per operation and not a typed `CreateResult` / `DeleteResult` / `LoadResult`. The operations
below return the affected ids in `result`:

| Operation (`lib.rs`) | `result` |
| --- | --- |
| `op_node_create`, `op_node_insert` | `{"created": [...]}` (entries carry `uuid`) |
| `op_node_delete` | `{"deleted": [ids]}` |
| `op_node_move` | `{"moved": ..., "parent": ..., "index": ...}` |

Other operations return their own shapes, for example `{"created": ..., "parent": ...}`, `{"converted": ...,
"created": ...}` or a full `snapshot()`. Not verified in the Rust port: that every creating or deleting tool uses these
keys.

### Failure leaves the document unchanged

The edit operations read here build a candidate model and apply it with `Session::commit_model`. They return `Err`
before the commit when the candidate is rejected (for example `validate_candidate`, the locked default forms, or a
revision conflict on `expectedRevision`), so a rejected edit does not change the document. Not verified in the Rust
port: that every tool follows this pattern.

### Messages

A failure `message` is a human string. There is no separate machine-readable error code in `handle_request`.

## The editor Command Browser

`rust/src/editor_windows.rs` implements the palette as the "Command Browser" window. The shortcut id is
`PaletteCommand.ListCommand` (Ctrl+Shift+P in `editor_ui.rs`). `Edit > Command Browser` opens the same window through
`editor_command_browser_set`.

- Rows are `Entry` values of two kinds (`EntryKind`): `Action`, a fixed editor command from the `COMMANDS` table, and
  `Tool(schema)`, an MCP tool read with `tools/list` (`parse_tools`). `filter_entries` filters by label, an English
  token and category, and sorts by label.
- Running an `Action` row sends `Request::Command`, a prompt or a window change (`run_command`). The palette shows no
  success or failure line for an `Action` row. Not verified in the Rust port: how each `Action` reports failure
  elsewhere in the editor.
- Running a `Tool` row checks the typed JSON against the tool schema first (`validate_arguments`: JSON object, required
  fields present and non-empty, simple types). A failed check shows an argument error and sends nothing. Otherwise the
  palette sends `tools/call` and shows the answer through `call_outcome` as an `Outcome { ok, text }`: a JSON-RPC
  `error.message` or `isError: true` is `Failed`, anything else is `Succeeded`, and the first `content` text is shown,
  clipped to 20,000 characters.
- The palette does not read `created` / `deleted` keys. It shows the answer text as is.

## Tool metadata and the command manifest

`doc/commands.manifest.json` lists each tool with an `effect` slug and `headless_ok`. These describe the kind of effect
and not the result of a call. See [commands.md](commands.md).

## Not carried over

- A typed payload per command (`ExCommandResult!(T)`).
- A migration of `Command.run` to return a struct. `rust/src` has no `Command` trait or enum to migrate (searched
  for `trait Command` and `enum Command`).
- A `message` field that is empty by default. The Rust `message` is set on failure only.
