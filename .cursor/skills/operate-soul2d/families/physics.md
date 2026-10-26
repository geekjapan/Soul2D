# Physics

Status: run live against the headless host (`node_create SimplePhysics`, `physics_set`, `physics_step`, `physics_reset`, resource read). Of the `Inspector_Apply_SimplePhysics*` aliases, `Gravity` and `ModelType` were run with `{value, context:{nodes:[uuid]}}` (a bare `nodes` fails with "missing required argument(s) for <tool>: context"); the other eight share the branch and are inferred.

SimplePhysics nodes drive a parameter from a pendulum or spring pendulum. This is the INP/INX runtime; native Live2D Physics3 is in live2d-physics.md.

## Sequence

ACP preset save/delete calls need host approval for the exact store file and entry. A changed file or config path expires approval; listing and applying a preset do not write the store.

1. Explore: `nodes_list {}` for SimplePhysics nodes; `parameters {}` for the parameter to drive.
2. Create if needed: `node_create {className:"SimplePhysics", name, parents}` (nodes-inspector.md).
3. Read: `resource://soul2d/resources/<uuid>`: `param`, `model_type`, `map_mode`, `length`, `gravity`, `frequency`, `angle_damping`, `length_damping`, `output_scale`, `local_only`.
4. Act: `physics_set {node, param, <fields>}` is one undo step; it neither steps the simulation nor writes the driven parameter.
5. Confirm: read the node again; run the simulation with `physics_step {dt}` (runtime only, no history) and read the parameter's value from `snapshot.values`; `physics_reset {}` returns to the authored pose.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `physics_set` | `{node \| nodes, param?, length?, gravity?, frequency?, angle_damping?, length_damping?, model_type: Pendulum\|SpringPendulum, map_mode: AngleLength\|XY\|LengthAngle\|YX, output_scale?, output_scale_x?, output_scale_y?, local_only?, preset?}`; `preset` alone is enough besides the node. The schema has `additionalProperties: false`: an undeclared field is an error that names it. |
| `physics_preset_save` | `{name, node?, settings?, overwrite?}`, a node or a setting or both. Keeps the settings (not `param`) under `name` for the user of this machine; with `node` the settings that node holds, with the settings of the call above them. `nodes`, one axis of the output scale and `preset` are refused. A saved name needs `overwrite:true`. No undo step. |
| `physics_preset_delete` | `{name}`. A name that is not saved is refused. |
| `physics_preset_list` | `{}`. Read only. `{presets: {name: {settings}}}`, the same as `resource://soul2d/physics/presets`. |
| `physics_step` | `{dt}` seconds; answers a snapshot. |
| `physics_reset` / `ViewportCommand_ResetPhysics` | Reset runtime physics to the authored pose. |
| `Inspector_Apply_SimplePhysicsAngleDamping` / `Inspector_Apply_SimplePhysicsFrequency` / `Inspector_Apply_SimplePhysicsGravity` / `Inspector_Apply_SimplePhysicsLength` / `Inspector_Apply_SimplePhysicsLengthDamping` / `Inspector_Apply_SimplePhysicsLocalOnly` / `Inspector_Apply_SimplePhysicsMapMode` / `Inspector_Apply_SimplePhysicsModelType` / `Inspector_Apply_SimplePhysicsOutputScaleX` / `Inspector_Apply_SimplePhysicsOutputScaleY` | One property each: `{value, context:{nodes:[uuid]}}`. |
| `puppet_meta_set` | `{property, value}`. Physics globals `pixelsPerMeter` and `gravity` are set here; see session.md. |

## Gotchas

- Verified: `physics_set {node, param, length:80}` then `{model_type:"SpringPendulum"}` showed `length 80.0` and `model_type SpringPendulum` on the node resource.
- Run (headless `soul2d-core` over JSONL, HTTP and stdio MCP): `physics_set {nodes, preset: name}` writes the saved settings to every
  node in one undo step, and a setting of the same call wins, one axis of the output scale too. There are no built-in
  presets. Saved presets are a file of the user, not part of the document: they are there after `new` and `open`, and
  another machine does not have them. The inspector of the editor does not offer them yet.
- A node's `param` is a parameter UUID, not a name (seen on the resource). `parameter_delete` clears SimplePhysics assignments (manifest).
- Physics state is runtime only. From the source, not run: an edit that changes the model resets it.
- Not run: `physics_step` value effects on the driven parameter; check `snapshot.values` after a step with a non-zero input.
