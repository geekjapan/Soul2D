# Project files

## Sub-features

- Start a new project, open an `.inx` or `.inp`, save, save under a new path.
- Saved state must survive a reopen.

## How to get to it (user POV)

`File > New...`, `File > Open...`, `File > Save` and `File > Save As...`. Open and
Save As ask for a path in an editor prompt; its `Browse...` button opens the operating
system's file dialog, a separate window outside egui.
`File > Recent` lists earlier projects and autosaves.

## Driving it with njc

Use absolute paths inside a scratch directory this run created:

```bash
./out/rust/debug/njc --port 18090 tools call FileCommand_SaveFile --json '{"file":"/tmp/verify-soul2d-run/roundtrip.inx"}'
./out/rust/debug/njc --port 18090 tools call FileCommand_OpenFile --json '{"file":"/tmp/verify-soul2d-run/roundtrip.inx"}'
./out/rust/debug/njc --port 18090 read resource://soul2d/document/state
```

Proof: the file exists with bytes, and after `FileCommand_OpenFile` a node edited
before the save reads back with its edited name (`prove.py` does this and keeps a
copy of the file as evidence). `resource://soul2d/document/state` reports
`documentFormat`, `revision`, `modified`, `canUndo` and `canRedo`.

`soul2d-core --mcp-port PORT --project PATH` opens a file at startup. The `new`,
`open` and `save` tools (argument `path`) reach the same operations; this skill has
driven only the `FileCommand_*` names.

## Gotchas

- Opening replaces the Session and resets history. Drive only a disposable instance.
- A success response is not proof; check the file and read the state back.
- Copy the written file into the evidence directory before deleting scratch.
- `*.inx` and `*.inp` are ignored by Git; evidence under `build/verification/` is too.
