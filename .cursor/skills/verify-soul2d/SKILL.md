---
name: verify-soul2d
description: Verify Soul2D (the Rust 2D model editor) by driving a disposable headless soul2d-core MCP host with the shipped njc client and keeping the evidence. Use after any change to project files, nodes, parameters, image import, meshes, the depth rig, Live2D native documents, Motion3 or viewport rendering, and before claiming such a change works.
---

# Verify soul2d

Soul2D has one Rust Session behind every client. The surface this skill drives is
**MCP over local HTTP**: a headless `soul2d-core` host and the production `njc`
client. Semantic state comes from MCP tools and resources, rendered state from PNG
bytes; no Computer Use is needed
([P5 in doc/ARCHITECTURE.md](../../../doc/ARCHITECTURE.md#principles)).

Read [the feature map](features/README.md) first and drive every entry point your
change touches. The default proof below covers only part of the map.

Other surfaces share the same Session and tools: stdio MCP (`soul2d-core --mcp`),
JSONL (`printf '%s\n' '{"op":"new"}' | ./out/rust/debug/soul2d-core`) and the
`soul2d-editor` GUI. Drive the GUI only for GUI-only behaviour, as described in the
[appendix](#appendix-gui-only-verification-optional).

## Launch

Build once from the repository root. Prerequisites are in `BUILD.md`; report a
missing prerequisite instead of claiming verification.

```bash
./tools/build-rust-core.sh --locked
.cursor/skills/verify-soul2d/scripts/prove.py
```

The build writes `out/rust/debug/soul2d-core` and `out/rust/debug/njc`. Rebuild when
either is missing or older than `rust/src`. To prove binaries built elsewhere (for
example a `cargo --target-dir` of your own), set `SOUL2D_BIN_DIR` to the directory
that holds both.

`prove.py` starts `soul2d-core --mcp-port 18088` (one Session, bound to
`127.0.0.1`), waits until that child PID owns the listener and `njc tools list`
answers, runs the default proof and stops the child. It refuses an occupied port.
Pass another unused port to run beside a second instance:
`.cursor/skills/verify-soul2d/scripts/prove.py 18090`. The host needs no seed data.
Without `SOUL2D_MCP_BEARER` it generates a bearer token and writes it, owner-only, to
`mcp-<port>.token` in its config directory, where `njc` finds it. `prove.py` points
`HOME`, `XDG_CONFIG_HOME` and `APPDATA` at its scratch directory, so the token never
lands in the user's settings, and checks that an anonymous request gets `401`.

To drive by hand, give the host and `njc` one token through the environment (no
file is written then), start the host yourself and record its PID:

```bash
export SOUL2D_MCP_BEARER=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
./out/rust/debug/soul2d-core --mcp-port 18090 > "$RUN/app.log" 2>&1 &
echo $! > "$RUN/pid"
```

Ready means `app.log` contains
`headless MCP host listening on http://127.0.0.1:18090/mcp`. Add `--project PATH` to
open a scratch `.inx` at startup. `$RUN` is a new directory under
`build/verification/`. Stop the host with `kill "$(cat "$RUN/pid")"`.

## Doctor

Run this read-only check before driving a hand-started instance, and again whenever
a response looks wrong:

```bash
lsof -nP -iTCP:18090 -sTCP:LISTEN
./out/rust/debug/njc --port 18090 tools list
```

The listener PID must equal the PID you recorded, and the tool list must include
`NodeCommand_AddNode` and `ViewCommand_CaptureLiveScreenshot`. An answer from any
other process is a failure: do not drive it, and never attach to the user's editor
or host. `prove.py` performs this check itself. Read `app.log` when startup fails.

## Drive

Use `./out/rust/debug/njc --port PORT` with the commands in the selected feature
file.

- `tools list` returns the live `inputSchema` of every tool.
- `resources list`, `find '*'` and `read UUID` read state; `read` also takes a
  resource URI such as `resource://soul2d/document/state`.
- `tools call NAME --json 'JSON'` acts.

UUIDs come from the current run's responses, never from another run.
`context.nodes` and `context.parameters` take UUIDs; `context.parameterValue` takes
axis values, not keypoint indexes.

A tool result is the bare value (for example `{"created":[2],"parent":1}`) as JSON
text in `result.content[0].text`. Failure is `result.isError: true` or a JSON-RPC
`error`. `njc` exiting zero proves neither.

## Evidence

`prove.py` prints its run directory, `build/verification/YYYYMMDD-HHMMSS/`, and
keeps there: `actions.jsonl` (every command and exit code), each raw response and
stderr, `instance.json`, `app.log`, the saved `roundtrip.inx`, `viewport.png`,
`result.json` and `cleanup.json`.

The default proof creates and renames a node, checks that an untextured scene
renders no pixels, imports a generated PNG as a Part, generates its mesh, saves,
reopens, and requires the persisted node name, the persisted mesh vertex count and
a capture with painted pixels. It writes real files; there is no dry-run.

Standards for any proof you add:

- Use the production path (`njc` and the MCP tools), not internal setters or mocks.
- Record the action and the state after it. Read the state back from a resource or
  a query tool; a success response alone is not proof.
- Check side effects: the file on disk, the reopened document, the rendered PNG.
- Open the PNG and look at it before making a claim about appearance.
- Map entries you did not drive stay unverified. Say so.

## Cleanup

`prove.py` waits for its child to exit, removes its temporary scratch directory and
then asserts that `viewport.png` and `roundtrip.inx` still exist in the run
directory. For a hand-started host, kill only the PID you recorded and delete only
the scratch directory you created. Never kill by process name. Keep everything
under `build/verification/`; it is ignored by Git. Build output stays reusable.

## Helpers

Both helpers are executable, use only the Python standard library and change
nothing outside `build/verification/` and their own temporary directory.

| Helper | Invocation | Result |
| --- | --- | --- |
| `scripts/prove.py` | `[SOUL2D_BIN_DIR=DIR] .cursor/skills/verify-soul2d/scripts/prove.py [PORT]` | Runs the default proof; nonzero on a failed assertion, timeout or command error. Needs the built binaries and `lsof`. |
| `scripts/coverage.py` | `.cursor/skills/verify-soul2d/scripts/coverage.py [--strict]` | Lists, per tool family in `doc/commands.manifest.json`, how many tools a feature map names. `--strict` exits 1 while any tool is unmapped. |

Deeper scripted checks for Live2D native formats (`tools/verify-*.py`) are
catalogued in the companion skill [verify-vsoul](../verify-vsoul/SKILL.md).

## Appendix: GUI-only verification (optional)

Use this only when a change affects `soul2d-editor` layout, menus, shortcuts, panel
visibility or native file dialogs. Build with
`./tools/build-rust-core.sh --locked --features gui` and launch
`out/rust/debug/soul2d-editor [--open PATH]` with `INOCHI_CONFIG_PATH` set to a
scratch directory, so the run never touches the user's settings. The editor serves
HTTP MCP only when its settings enable it (`MCP.Enabled`, `MCP.Host`, `MCP.Port`;
off by default) and takes `--mcp` for stdio MCP.

Apply the installed cua-driver skill. Check `cua-driver status` and
`cua-driver permissions status --json`, then use
`cua-driver list_windows '{"pid":PID}'` and a fresh `get_window_state` with that PID
and the returned window ID. egui may expose no accessibility row for a control;
ground pixel actions on a fresh screenshot of that exact window, record the action
and take a new snapshot afterward. Use the labels in the feature map, not saved
coordinates. A permission or capture failure is unsupported GUI evidence, not a
pass; do not change system permissions as hidden setup. Do not stop the shared cua
daemon; end only the session this run created.

`File > Open...`, `File > Save As...` and the Import and Merge entries ask for a
path in an editor prompt; its `Browse...` button opens the operating system's file
dialog, which is a separate window outside egui. The MCP proof uses
`FileCommand_OpenFile` and `FileCommand_SaveFile` with absolute scratch paths
instead.

The live-window scripts are `tools/verify-*-gui.py`; they share the harness in
`tools/verify-animation-key-gui.py` (isolated config directory, one editor at a time, pid-matched window).
`tools/verify-panels-gui.py` clicks through the parameters, animation, Motion3, depth, agent and recovery
panels and reports PASS, FAIL, INCONCLUSIVE or SKIP per check (exit 0, 1, 2); canvas drags need
`--foreground-drags`, which lets the driver briefly front only the editor window. A locked screen
leaves every check INCONCLUSIVE.

### Editor path-prompt E2E (optional category)

Verify File > New, welcome-screen New, path-prompt submission and cancellation, and unsaved-change
decisions in a separate owned GUI run with recorded interaction. The default MCP proof supplies absolute
scratch paths and does not exercise those controls. See
[verify-vsoul — Optional categories](../verify-vsoul/SKILL.md#optional-categories-not-default-proof).
