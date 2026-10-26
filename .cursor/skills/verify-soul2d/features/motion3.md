# Motion3

## Sub-features

- Import a `.motion3.json`, list and read its curves, edit a point, export.
- Map curve IDs to parameters, then pose, seek, advance and reset the preview.

## How to get to it (user POV)

The editor's `Live2D Motion3` section imports a file, lists curves and points with
Apply, Reload and Discard, edits the mapping and has seek and play controls. For a
CMO3 document the section is `Runtime motions (.motion3.json)`.

## Driving it with njc

Create the parameter first, then pass its UUID as `parameter`:

```bash
./out/rust/debug/njc --port 18090 tools call Motion3Command_Import --json '{"name":"nod","path":"/tmp/verify-soul2d-run/nod.motion3.json"}'
./out/rust/debug/njc --port 18090 tools call Motion3Command_Bind --json '{"name":"nod","bindings":[{"id":"ParamAngleX","parameter":UUID,"axis":0}]}'
./out/rust/debug/njc --port 18090 tools call Motion3Command_Pose --json '{"name":"nod","time":0.5}'
./out/rust/debug/njc --port 18090 tools call Motion3Command_Seek --json '{"name":"nod","time":0.5}'
./out/rust/debug/njc --port 18090 tools call Motion3Command_SetPoint --json '{"name":"nod","curve":0,"point":1,"changes":{"value":0.5}}'
./out/rust/debug/njc --port 18090 tools call Motion3Command_Export --json '{"name":"nod","path":"/tmp/verify-soul2d-run/out.motion3.json"}'
```

A one-curve file is enough and needs no fixture:

```json
{"Version":3,"Meta":{"Duration":1.0,"Fps":30.0,"Loop":true,"AreBeziersRestricted":true,"CurveCount":1,"TotalSegmentCount":1,"TotalPointCount":2,"UserDataCount":0,"TotalUserDataSize":0},"Curves":[{"Target":"Parameter","Id":"ParamAngleX","Segments":[0,0,0,1.0,1.0]}]}
```

Proof: `Motion3Command_List` answers
`{"motions":[{"name":"nod","curveCount":1,"duration":1.0}]}`; `Motion3Command_Pose`
at 0.5 answers `applied` with `raw: 0.5` and `unmapped: []`; after
`Motion3Command_Seek` the state resource shows the value under `values` and the
clip under `motion3`; the exported file exists.

`Motion3Command_Read`, `Sample`, `Bindings`, `Advance` (`dt` seconds, optional
`loop`) and `Reset` are mapped but not driven by this skill. The Rust tests cover
them: `cargo test --offline --manifest-path rust/Cargo.toml --target-dir out/rust --test core --test last_gaps`.

## Gotchas

- Import and Bind are undoable edits on an INX document; Pose, Seek, Advance and
  Reset change runtime values only.
- `Motion3Command_Bind` replaces the whole mapping; an empty array clears it.
  Destination ranges are never inferred.
- `Motion3Command_Export` refuses an existing path.
- On a CMO3 document imported clips live in the Session only and are lost on open or new.
- Importing Motion3 brings curves, not Cubism meshes.
