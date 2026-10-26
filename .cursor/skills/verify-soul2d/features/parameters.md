# Parameters

## Sub-features

- Create a 1D, 2D or mouth parameter; read its axes; delete it.
- Undo and redo creation.

## How to get to it (user POV)

The Parameters panel's create menu offers 1D and 2D ranges and the mouth preset.
The panel shows one slider (1D) or one pad (2D) per parameter.

## Driving it with njc

```bash
./out/rust/debug/njc --port 18090 tools call ParamCommand_Add1DParameter --json '{"min":-1,"max":1,"name":"Probe"}'
./out/rust/debug/njc --port 18090 tools call parameter_get --json '{"id":UUID}'
./out/rust/debug/njc --port 18090 tools call parameters --json '{}'
./out/rust/debug/njc --port 18090 tools call parameter_delete --json '{"id":UUID}'
```

The create call answers the parameter record:
`{"uuid":N,"name":"Probe","is_vec2":false,"min":[-1.0,0.0],"max":[1.0,1.0],"axis_points":[[0.0,0.5,1.0],[0.0]],"bindings":[],...}`.
`parameter_get`, `parameters` and `read UUID` (at `item.data`) return the same
record, so range and axis shape are checkable without the GUI.

Proof: the record read back with the requested range, then an empty `parameters`
list after `parameter_delete`. For history, assert the UUID's absence and presence
in fresh `parameters` responses around `EditCommand_Undo` and `EditCommand_Redo`.

`ParamCommand_Add2DParameter` takes the same arguments and `parameter_create_mouth`
takes `{}`; neither has been driven by this skill yet.

## Gotchas

- `min` is stored per axis; a 1D parameter still reports two axes with the second fixed.
- Binding tools address a key by axis value through `context.parameterValue`, never by index.
- Nothing here is in the default proof; drive it yourself when parameters change.
