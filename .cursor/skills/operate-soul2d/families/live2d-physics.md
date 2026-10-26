# Live2D physics (Physics3)

Status: run live against the headless host by `tools/verify-acp-agent.py --families live2d-physics` (2026-10-21), as a scripted MCP client and by an agent over ACP, on a synthetic CMO3 runtime model and Physics3 file the script writes (`live2d_physics_attach`, `live2d_physics_step`, `live2d_physics_reset`, `live2d_physics_options_set`, `live2d_physics_stabilize`, `nativePhysics` read back from the snapshot). Native GUI and vendor acceptance are not covered.

Runtime Physics3 simulation for a native Live2D document. Runtime only: the document, history and `revision` are not touched, and linked files are never followed. INX pendulums are in physics.md.

## Sequence

1. Attach: `live2d_physics_attach {path}` an explicit Physics3 file to the open native model.
2. Act: `live2d_physics_options_set {gravity, wind}` (complete vectors, objects), `live2d_physics_step {dt}`, `live2d_physics_stabilize {}`, `live2d_physics_reset {}`.
3. Confirm: `resource://soul2d/document/state` `values`; `render`.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `live2d_physics_attach` | `{path}` |
| `live2d_physics_options_set` | `{gravity, wind}` both required; undeclared arguments are refused. |
| `live2d_physics_step` | `{dt}` seconds. |
| `live2d_physics_stabilize` | `{}` settle the runtime pose. |
| `live2d_physics_reset` | `{}` |

## Gotchas

- Verified: any physics call before an attach fails with "no native physics is attached".
- `motion3_reset` also clears the Motion3 preview's physics (manifest); see motion3.md.
