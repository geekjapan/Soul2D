# Live2D native documents

## Sub-features

- Open a `.cmo3` as the active document and inspect its container and source.
- Set a runtime parameter and render the preview.
- Edit one form vertex or opacity, save and reopen (mapped, driven by `tools/`).

## How to get to it (user POV)

Open the `.cmo3` with `File > Open...` or `soul2d-editor --open PATH`. The editor
shows `CMO3 source summary`, `Live2D source forms` and `Live2D native preview`
sections for it.

## Driving it with njc

You need a `.cmo3` the user supplied. `*.cmo3` is ignored by Git and the project
ships none. Copy it into this run's scratch directory and open the copy:

```bash
./out/rust/debug/njc --port 18090 tools call open --json '{"path":"/tmp/verify-soul2d-run/model.cmo3"}'
./out/rust/debug/njc --port 18090 read resource://soul2d/document/state
./out/rust/debug/njc --port 18090 tools call Live2DCommand_AuthoringInspect --json '{}'
./out/rust/debug/njc --port 18090 tools call Live2DCommand_ParameterSet --json '{"parameterId":"ParamAngleX","value":30}'
./out/rust/debug/njc --port 18090 tools call ViewCommand_CaptureLiveScreenshot --json '{}'
```

Proof: the state resource reports `documentFormat: "live2d.cmo3"`; after
`Live2DCommand_ParameterSet` its `values` hold the new value while `revision` and
`modified` are unchanged (a runtime value is not an edit); the capture returns an
image plus a text item whose `warnings` name what the preview approximates.

Source edits use `Live2DCommand_FormSetVertex` and `Live2DCommand_FormSetOpacity`
with `sourceGuid`, `formGuid`, `coordinateType` and the current `expectedRevision`.
Run `python3 tools/verify-native-authoring-edit.py` and
`python3 tools/verify-native-authoring-opacity.py` for the full edit, undo, save and
reopen check; [verify-vsoul](../../verify-vsoul/SKILL.md) lists the inputs they need.

## Gotchas

- Never open or save over the user's original; work on the scratch copy.
- INX tools fail here with `operation is not supported for the active Live2D document`.
- Only three edits exist on native data: one form vertex, one ArtMesh form opacity,
  and one existing CAN3 key value. Everything else is read-only.
- The render is a bounded preview: Glue is not applied and warps are piecewise
  bilinear. It is not evidence of Cubism equivalence.
- Take `expectedRevision` from the state resource immediately before each edit.
