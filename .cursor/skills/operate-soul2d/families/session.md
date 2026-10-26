# Session, files and document maintenance

Status: run live against the headless host (`new`, `open`, `save`, `FileCommand_SaveFile`, `snapshot`, `puppet_meta_set`, `extdata_set` / `extdata_get`, `tool_regenerate_node_ids`, `CommandScopeCommand_GetCurrentCommandScope`). `tool_repair`, `tool_generate_fake_layer_names` and `FileCommand_OpenFile` are written from the manifest only.

The session owns one document at a time (INP/INX, or a native Live2D CMO3/CAN3 where the tool says so). `new`, `open` and the `import_*` tools replace it; everything else edits it.

## Sequence

1. Explore: `snapshot {}` is the whole state (model, `values`, `history`, `revision`, `modified`, clipboards, `depthDraw`, `partSplit`, `views`). The cheaper read is `resource://soul2d/document/state`
   (`revision`, `canUndo`, `canRedo`, `documentFormat`, `modified`, `values`).
2. Act: `new {}` blank INX; `open {path}` INP/INX or supported CMO3; edit with the other families; `save {path}` writes atomically.
3. Confirm: `save` answers `{path}`; `open` the file in a scratch host and compare, or `export_inp` for a runtime file (atlas-export.md).

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `new` | `{model?}` empty model or a supplied one; a supplied model without `nodes` fails with "model is missing nodes". |
| `open` / `FileCommand_OpenFile` | `{path}` / `{file}` INP, INX, CMO3; history becomes one empty state. |
| `save` / `FileCommand_SaveFile` | `{path, mark_saved?}` / `{file}`. The D forms use `file`, not `path`. |
| `snapshot` | `{}` read only. |
| `puppet_meta_set` | `{property, value}` one undo step: `name`, `artist`, `rigger`, `contact`, `licenseURL`, `copyright`, `reference`, `preservePixels`, `pixelsPerMeter`, `gravity`. |
| `extdata_set` / `extdata_get` | `{key, base64}` (`null` removes) / `{key}` named payload of the INX extension section; `set` answers `{bytes, changed, present, key}`. The flip-pair list is key `com.inochi2d.creator.FlipConfig`, base64 of a JSON array of `{uuid1, uuid2}` node UUIDs (Tools > Flip Pairing reads and stores it through these two tools). |
| `tool_regenerate_node_ids` / `ToolCommand_RegenerateNodeIDs` | `{}` new UUID for every node, references rewritten; answers `{regenerated}`. Invalidates every UUID you hold; re-list. |
| `tool_repair` / `ToolCommand_AttemptRepairPuppet` | `{}` full repair; also regenerates node UUIDs. |
| `tool_generate_fake_layer_names` / `ToolCommand_GenerateFakeLayerName` | `{}` set every Part's layer path to `/name`. |
| `CommandScopeCommand_GetCurrentCommandScope` | `{}` available commands; the Rust host always reports the normal scope. |

## Gotchas

- Verified: `save` and `open` round trip: the opened document had the saved Part and no history.
- Verified: `snapshot.revision` never decreases inside one host process; `new` and `open` add one.
- Verified: `save` to `scratch.inx` and `FileCommand_SaveFile {file}` both wrote the path given. `export_inp` forces `.inp` (atlas-export.md); use `save` for the editable `.inx`.
- Tool results are not always the changed thing: some answer `{created}` or `{updated}`, some a full snapshot. Never assume a UUID, read it from the reply or `nodes_list`.
- Resources: `resource://vsoul/snapshot`, `resource://vsoul/parameters`, `resource://vsoul/nodes` are legacy-named listings of the same data.
