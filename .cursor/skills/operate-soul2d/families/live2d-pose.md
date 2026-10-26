# Live2D pose (Pose3)

Status: run live against the headless host by `tools/verify-acp-agent.py --families live2d-pose` (2026-10-21), as a scripted MCP client and by an agent over ACP, on a synthetic CMO3 runtime model and Pose3 file the script writes (`live2d_pose_attach`, `live2d_pose_step` with `signals`, `live2d_pose_reset`, `nativePose.partOpacities` read back from the snapshot). Native GUI and vendor acceptance are not covered.

Runtime Pose3 groups and selection signals for a native Live2D document. Runtime only, like live2d-physics.md; `motion3_seek` replays an attached Pose at 60 Hz for deterministic seeks (motion3.md).

## Sequence

1. Attach: `live2d_pose_attach {path}` an explicit Pose3 file.
2. Act: `live2d_pose_step {dt, signals?}` advance by seconds, optionally overriding known selection signals; `live2d_pose_reset {}` resets groups and signals.
3. Confirm: `resource://soul2d/document/state` `values`; `render`.
4. Author: `motion3_pose_switch {name, part, time}` keys a switch to `part` into an imported Motion3 (motion3.md).

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `live2d_pose_attach` | `{path}` |
| `live2d_pose_step` | `{dt, signals?}` |
| `live2d_pose_reset` | `{}` |

## Gotchas

- Verified: a pose step before an attach fails with "no native pose is attached".
- Linked files are not followed; attach each file explicitly.
