# Parameters and bindings

Status: run live against the headless host (`parameter_create`, `binding_set_trs`, `binding_set_deform`, `binding_set_interpolation`,
`binding_remove`, `binding_keypoint`, `value_set`, `parameter_get`, `parameter_set_axes`, `parameter_duplicate`, `parameter_group_create`,
`ModelCommand_SetRotationXBinding`, binding resource, render).

A parameter has one or two axes. A binding ties one property of one node (`transform.t.x`, `deform`, ...) to a parameter and stores a value
at each keypoint. Keypoints are the cross product of the axis points.

## Sequence

1. Explore: `parameters {}` (all records) or `parameter_get {id}`. Note `uuid`, `min`, `max`, `axis_points`, `is_vec2`.
   `axis_points` are normalised 0..1 positions along `min..max`; a key is an index pair into them.
2. Create if needed: `parameter_create {dimensions: 1|2, min:[x,y], max:[x,y]}` (always two numbers, also for a 1D parameter; for 1D give `y` as 0).
   Fresh parameters get `axis_points [[0,0.5,1],...]`; edit with `parameter_set_axes`.
3. Act: write a keypoint with `binding_set_trs`, `binding_set_deform` or `deform_edit` (mesh-deform.md). `key` is `[ix, iy]` indexes, for example
   `[2,0]` is the third point on X of a 1D parameter, `[2,1]` on a 2D one. Select bindings in the other `binding_*` tools by
   `bindings: [{node: <uuid>, param_name: <name>}]`.
4. Confirm: `parameter_get {id}` lists each binding with `values`, `isSet` and `interpolate_mode`;
   `resource://soul2d/bindings/get?parameter=<id>&target=<node>&name=<param_name>` reads one. Then `value_set {id, value:[x,y]}` (document units,
   not normalised) and `render` to see the pose.

## Tools: parameters

| Tool | Purpose and minimum arguments |
| --- | --- |
| `parameters` | List raw parameter records. |
| `parameter_get` | `{id}` one parameter or group. |
| `parameter_create` | `{dimensions, min?, max?, name?}`; `min` and `max` default to `[-1, 0]` and `[1, 1]` for one dimension, `[-1, -1]` and `[1, 1]` for two. A range whose `max - min` is not a finite number (such as -1e308 to 1e308) is refused here, in `parameter_set_axes` and in a loaded model. |
| `ParamCommand_Add1DParameter` / `ParamCommand_Add2DParameter` | `{min:number, max:number, name?}` scalars, not arrays. |
| `parameter_create_mouth` | Standard 2D mouth parameter with extra axis points. |
| `parameter_create_standard` | `{all?}` (refused, with nothing added, when a parameter group has one of the names) add the 1D parameters of the Cubism standard parameter list, named by their IDs (`ParamAngleX`, `ParamEyeLOpen`, ...), with the list's minimum, default and maximum. Without `all` the 27 rows the list always asks for; `all: true` adds the 13 it marks as needed only sometimes (arms, hands, `ParamBustX`, `ParamBaseX`, ...). A name that exists is left as it is. Answers `{created:[names], existing:[names]}`; one undo step, none when nothing was missing. No groups are made. |
| `parameter_convert_to_2d` | `{id}` keeps the UUID and X axis. |
| `parameter_rename` / `ParamPropCommand_SetParameterName` | `{id, name}` |
| `parameter_set_repeat` | `{id, repeat}` join the two ends of a 1D parameter into a loop (`repeat: false` removes the flag). A repeating parameter accepts any finite X value in `value_set`, `parameter_set_defaults` and the `values` of `evaluate`/`render`, and wraps it into `[min, max)`; a value at `max` is the same point as `min`. Unset scalar and deformation keys interpolate between authored keys across the seam. Changing the flag refills the bindings in the same undo step. Authored keys, including an authored maximum, keep their values. A 2D parameter and a group are refused. |
| `parameter_set_axes` | `{id, min, max, axis_points}` remaps keys. Verified: `axis_points:[[0,0.25,0.5,1],[0]]` accepted. |
| `parameter_set_defaults` | `{id, value?}` starting position (default: the current session value). |
| `parameter_set_all_defaults` | `{}` every parameter's starting position becomes its current session value in one undo; answers `{parameters, changed}` and commits nothing when `changed` is 0. Any argument is refused (`id` does not narrow it; use `parameter_set_defaults`). Read `resource://vsoul/parameters` for the result. |
| `parameter_copy` / `parameter_duplicate` / `parameter_duplicate_flip` | `{id}` the source parameter. Copy fills the session clipboard and leaves the document alone; `parameter_duplicate {id, name?}` names the copy `<name> (Copy)` by default; the flip form mirrors the bindings onto the flip-pair partners. |
| `parameter_paste` | `{id, flip?}` paste the copied bindings into parameter `id` in one undo; consumes the clipboard. |
| `parameter_flip` / `parameter_mirror` / `parameter_mirrored_autofill` | `{id, axis?}` / `{id, axis?}` / `{id, direction}`. Reverse an axis; negate keys across an axis; fill one half from the other (`direction` 1..4). |
| `parameter_synthesize_corners` | `{id}` a 2D parameter. Each key off the default's row and column becomes row edge + column edge - reference key; answers `{reference, written, skipped}` (bindings whose reference row or column has an unset key are skipped). |
| `parameter_split` | `{id, nodes}` moves the nodes' bindings to a new `(Split)` parameter. |
| `parameter_link` / `parameter_unlink` | `{id, target, axis?, target_axis?}` make one axis drive another. |
| `parameter_move` | `{id, group?, index?}` |
| `parameter_delete` | `{id}` also clears SimplePhysics assignments. |
| `parameter_group_create` / `parameter_group_delete` / `parameter_group_set_color` | Groups; `{name?, members?}`, `{id}`, `{id, color:[r,g,b]}`. A group is created at the top level. A file from another tool may hold a group inside a group: the tools here reach a parameter or a group at any depth, and deleting an inner group releases its parameters into the group around it. |
| `value_set` | `{id, value:[x,y]}` session-only value, not an undo step. |
| `values_reset` / `ViewportCommand_ResetParameters` | Every parameter back to its default. |
| `default_form_lock_set` | `{locked}` INX only. While locked, an edit that changes or unsets a keyform set at a parameter's default value is refused with a message naming the parameter, node and binding; new bindings, first-time default keys, binding or parameter removal, range or default edits, `deform` bindings of a node whose own data the edit changes (`mesh_edit`, `depth_set`, ...) and undo/redo pass; values compare as numbers, and a change of axis points only (`parameter_flip`, `parameter_set_axes`) compares the keyform at the old and new default key, refusing a flip that moves another keyform there or an axis edit that leaves the default between keypoints (range or default edits are not compared, so they can still change which keyform the default uses; the lock cannot change while a deform refresh is pending). Not saved, no undo step, and a new or opened document starts unlocked; read back as `defaultFormLocked` in `snapshot` and `resource://soul2d/document/state`. |

## Tools: bindings

| Tool | Purpose and minimum arguments |
| --- | --- |
| `binding_set_trs` | `{parameter, nodes, key, translation?:[x,y], scale?:[x,y], rotation_degrees?, rotation_x?, rotation_y?, translation_z?}`. Answers `{key, parameter, updated}`. Verified on 1D (`key [2,0]`) and 2D (`key [2,1]`) parameters. `translation` is two numbers here. |
| `ModelCommand_SetTRSBinding` / `ModelCommand_SetRotationXBinding` / `ModelCommand_SetRotationYBinding` | Same writes under the D names: `{parameter, nodes, key, translation?, scale?, rotationDegrees?, applyRotation?}` and `{parameter, nodes, key, rotationRadians}`. They take explicit `parameter`, `nodes`, `key`; `context.parameters` / `context.parameterValue` is refused ("request.parameter must be an unsigned 32-bit integer"). |
| `binding_set_deform` | `{parameter, nodes, key, values:[dx0,dy0,dx1,dy1,...]}` one offset pair per vertex, for nodes without a driven deform. |
| `binding_set_interpolation` | `{parameter, mode: Nearest\|Linear\|Cubic, bindings:[{node, param_name}]}` answers `{changed}`. |
| `binding_remove` | `{parameter, bindings:[{node, param_name}]}` answers `{removed}`. |
| `binding_keypoint` | `{parameter, key, action}`: `unset`, `reset`, `set_current`, `invert`, `mirror_horizontal`, `mirror_vertical`, `flip_deform`, `symmetrize_deform`, `set_from_mirror_*`. `bindings` optional. |
| `binding_weight_limits_set` | `{parameter, node, limits:[{parameter, points:[[value, weight],...]}]}`: blend-shape weight limits on one deform binding; the smallest weight wins; `[]` removes them. Read back in the parameter's binding (`weightLimits`). One undo step. |
| `binding_multi_edit` | `{nodes, channel: opacity\|zSort\|tint\|screenTint, value, method?: overwrite\|add\|multiply\|screen, parameters?}`: Multiple Keys Editing of every set key on that channel; colours take `[r,g,b]`; opacity and colours clamp to 0..1. Result `{changed}`. One undo step. |
| `binding_copy` / `binding_paste` / `binding_copy_to` / `binding_swap_with` | Keypoint clipboard and transfer between nodes: `{parameter, key, ...}`; the last two also need `bindings` and `target`. `binding_paste` also takes `ratio` (0..1, Blend Form) and `invert {axis:"vertical"|"horizontal", origin?}` (mirrors pasted deform shapes, negates `transform.r.z`, reflects `transform.t.x`/`t.y` about `origin`, default 0; copy a key and paste it back with `invert` to Reflect it, rotation deformer position included). |

## Gotchas

- Verified: `bindings` entries are `{node, param_name}`. `{target, name}` and `context.bindings` (shown in doc/mcp_api.md before this fix) are refused with
  "binding node must be a nonzero u32 UUID" / "missing required argument(s) for <tool>: bindings".
- Verified: setting key `[2,0]` also marks the neighbouring key set to zero for translation (`isSet [[false],[true],[true]]`); `deform` offsets read back at
  every key of the 1D axis but only the written key is `isSet`.
- Verified: `binding_set_deform` with 8 values on a 4-vertex Part wrote four offset pairs; the count must equal the vertex count.
- Verified: `deform` binding names are the literal `deform`; transform bindings are `transform.t.x`, `transform.t.y`, `transform.r.x`, ...
- Default form lock: the lock protects default keys that are set. A deform binding written by a Depth Bone pose at another keypoint leaves its default key unset and filled from that keypoint; a later depth edit sets it.
- The flip-pair list is extension data, not a parameter field: read and write it with `extdata_get` / `extdata_set` under key `com.inochi2d.creator.FlipConfig` (base64 of a JSON array of `{uuid1, uuid2}`; session.md). `parameter_duplicate_flip`, `parameter_mirrored_autofill`, `parameter_paste` and the `binding_keypoint` `set_from_mirror_*` actions read it. `extdata_set` does not check the payload: after a bad write all four fail with "FlipConfig is not JSON", "FlipConfig is not an array" or "FlipConfig entry N has no valid uuid1" (or uuid2: missing, not an integer, or above 4294967295) until the key is fixed or removed. A pair whose two UUIDs are not both existing nodes is ignored, so the half pairs the flip-pair window stores with the placeholder 4294967295 are kept but have no effect.
- Not run: `parameter_link`, `parameter_split`, `parameter_mirrored_autofill`, `parameter_convert_to_2d`, `binding_copy_to`, `binding_swap_with`.
