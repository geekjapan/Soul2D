# Nodes

## Sub-features

- Add a node under the root or a parent, rename it, delete it.
- Undo and redo each of those edits.

## How to get to it (user POV)

The Nodes panel's context menu adds or inserts a node type. Selecting a node shows
its name in the Inspector. `Edit > Undo` and `Edit > Redo` walk the history.

## Driving it with njc

```bash
./out/rust/debug/njc --port 18090 tools call NodeCommand_AddNode --json '{"className":"Node","_suffix":""}'
./out/rust/debug/njc --port 18090 tools call NodeCommand_SetNodeName --json '{"newNames":["Verification Node"],"context":{"nodes":[UUID]}}'
./out/rust/debug/njc --port 18090 read UUID
./out/rust/debug/njc --port 18090 tools call nodes_list --json '{}'
```

`NodeCommand_AddNode` answers `{"created":[{"uuid":N,"name":"Unnamed Node","type":"Node",...}]}`.
Take `created[0].uuid`. After the rename, `read UUID` must show the new name in
`contents[0].text` at `item.name`. `nodes_list` returns every node's `uuid`,
`name`, `type` and `parent`.

History: read the node, call `EditCommand_Undo` with `{}`, read the old name,
call `EditCommand_Redo` with `{}`, read the new name. Both return the snapshot with
`canUndo` and `canRedo`.

Delete with `NodeCommand_DeleteNode` and `{"context":{"nodes":[UUID]}}`, then check
that `nodes_list` no longer has the UUID.

Proof: the saved file reopened with the renamed node (`prove.py` does this with
`Verification Node`). Delete and undo/redo are not in the default proof.

## Gotchas

- `_suffix` is required even when it is the empty string.
- `newNames` needs exactly one name per target node; an empty name fails.
- A plain Node has no texture, so an empty capture does not disprove creation.
- Node tools fail on a native CMO3 document with
  `operation is not supported for the active Live2D document`.
- A resource read proves document state, not GUI selection or panel layout.
