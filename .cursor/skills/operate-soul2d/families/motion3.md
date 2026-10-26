# Motion3

Status: run live against the headless host on an INX document with a hand-written one-curve `.motion3.json` (`motion3_import`, `motion3_list`, `motion3_read`, `motion3_sample`, `motion3_bind`, `motion3_bindings`, `motion3_pose`, `motion3_seek` with and without `fades`, `motion3_advance`, `motion3_fade_set` with its undo and refusal, `motion3_point_set`, `motion3_export`, `motion3_reset`, advance-before-seek error). The CMO3 paths are written from the manifest only.

Live2D Motion3 clips. In an INX document the clip is stored in the document and bound to INX parameters; in a native CMO3 it lives in a session-only library
(lost on `open` and `new`, export it). Each tool has a snake_case name and, except `motion3_fade_set` and `motion3_pose_switch`, a D-style `Motion3Command_*` twin. Native Live2D playback of other kinds is in live2d-animation.md,
live2d-expression.md, live2d-physics.md and live2d-pose.md.

## Sequence

1. Explore: `motion3_list {}` answers `{motions:[{name, curveCount, duration}]}`.
2. Import: `motion3_import {name, path}` (history entry in INX, session-only in CMO3).
3. Read: `motion3_read {name}` the stored JSON (`Meta`, `Curves[].Segments`); `motion3_bindings {name}` the Parameter ID to UUID/axis map.
4. Act, INX: `motion3_bind {name, bindings:[{id, parameter, axis}]}` persists the mapping (one undo; empty list clears it). Edit a key with `motion3_point_set {name, curve, point, time?, value?}`.
   Act, CMO3 with an attached Pose3 (live2d-pose.md): `motion3_pose_switch {name, part, time}` keys a pose switch. At `time` (seconds) the PartOpacity curve of `part` is keyed 1 and the curves of the other parts in its pose group 0; it answers `{name, parts}`.
5. Preview: `motion3_sample {name, time}` raw curve values; `motion3_pose {name, time}` mapped values without applying; `motion3_seek {name, time}` applies them to the runtime; `motion3_advance {dt, loop?}` moves the preview; `motion3_reset {}` clears it.
6. Confirm: `motion3_pose` answers `{applied, unmapped, values, clipEnd, time}`; `motion3_seek` answers a full snapshot whose `values` hold the applied pose; `render` shows it; `motion3_export {name, path}` writes the edited JSON and fails if the destination exists.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `motion3_list` / `Motion3Command_List` | `{}` read only. |
| `motion3_import` / `Motion3Command_Import` | `{name, path}` |
| `motion3_read` / `Motion3Command_Read` | `{name}` read only. |
| `motion3_bind` / `Motion3Command_Bind` | `{name, bindings, preserve_unknown?}` INX only. |
| `motion3_bindings` / `Motion3Command_Bindings` | `{name}` read only. |
| `motion3_sample` / `Motion3Command_Sample` | `{name, time}` raw curves, no loop or fades. |
| `motion3_pose` / `Motion3Command_Pose` | `{name, time}` read only; unmapped curves are listed. A value outside the range of its parameter fails, except the X value of a repeating 1D parameter (`parameter_set_repeat`): that may be any finite value, `values` keep it and the pose wraps it. |
| `motion3_seek` / `Motion3Command_Seek` | `{name, time, fades?}` runtime values only, no history. `fades: true` plays with fades from this seek: each Parameter curve is weighted (`applied[].weight`; CMO3 PartOpacity is not) by its fade-in over the time played and its fade-out towards the clip end; `snapshot.motion3.fade` shows the clock. |
| `motion3_advance` / `Motion3Command_Advance` | `{dt, loop?}` needs a prior seek; keeps its fades (also across CMO3 edits, undo and physics or pose attach), and a looping advance never fades out. |
| `motion3_fade_set` | `{name, curve?, fadeIn?, fadeOut?}` seconds on the motion (`Meta`) or one curve (overrides the motion's); `null` removes a value. One undo on INX; session only on CMO3. Zero is an instant change; a curve without a fade (or a negative one) uses the motion's, and a motion without one has none. A CMO3 preview shows the edit at the next seek or advance. |
| `motion3_reset` / `Motion3Command_Reset` | `{}` rejects an active INX animation. |
| `motion3_point_set` / `Motion3Command_SetPoint` | `{name, curve, point, time?, value?}` / D form `{name, curve, point, changes:{time?, value?}}`; zero-based indexes, one undo. |
| `motion3_pose_switch` | `{name, part, time}` CMO3 with an attached Pose3; session only, like the import, and a playing preview shows it at the next seek or advance. A key within half a frame of an anchor sets it; a key touching a segment that is not stepped is refused and changes nothing. A missing curve holds its key from 0, so key the starting part at 0 first. |
| `motion3_export` / `Motion3Command_Export` | `{name, path, runtimeModel?}` atomic create at an unused path; no history. With `runtimeModel` (a `.model3.json` or `.moc3`, read as `live2d_runtime_inspect` reads it) the export is refused with "the runtime model has no Parameter X, ..." unless every Parameter and PartOpacity curve names an ID of that model; the reply then adds `checkedCurves`. |

## Gotchas

- Verified: the file must be a full Motion3 document (`Version`, `Meta` counts, `Curves[].Segments`); `Segments [0,0,0,1,1]` is a start point at t=0, v=0 and a linear segment to t=1, v=1, sampled 0.5 at t=0.5.
- Verified: after `motion3_bind`, `motion3_pose` answered `applied:[{id, parameter, axis, raw, value:[0.5,0]}]` and `unmapped:[]`, and `motion3_seek` made `snapshot.values` for the parameter `[0.5, 0]`.
- Verified: `motion3_pose` and `motion3_seek` before a bind fail with `Motion3 "<name>" has no parameter bindings`; a curve value outside the parameter's range fails with "value for parameter 3 is outside its range" (a clip whose point was edited to 3 for a parameter ranging -1..1 did this).
- Verified: clips and their bindings live in the document; `motion3_list` is empty after `new`, and a clip imported again after `new` has no bindings until you `motion3_bind` it.
- Verified: `motion3_point_set {curve:0, point:1, value:2}` turned the sample at t=1 into 2.0 and the stored segment into `[0,0,0,1,2.0]`; `motion3_export` wrote the file.
- Verified: `motion3_advance` before any seek fails with "seek a motion before advancing it".
- Verified: `motion3_import` answers only `{name}`; read it back with `motion3_list`.
- Verified: `motion3_seek` left `revision` unchanged (108); `motion3_point_set` and `motion3_import` each raised it by one in INX.
