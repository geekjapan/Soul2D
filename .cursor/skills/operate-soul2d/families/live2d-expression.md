# Live2D expression and runtime parameters

Status: run live against the headless host by `tools/verify-acp-agent.py --families live2d-expression` (2026-10-21), as a scripted MCP client and by an agent over ACP, on a synthetic CMO3 runtime model and Expression JSON file the script writes (`live2d_parameter_set`, `live2d_expression_import`, `live2d_expression_list`, `live2d_expression_apply`, `live2d_expression_reset`, `values` and `nativePlayback` read back from the snapshot). Native GUI and vendor acceptance are not covered.

Session-only runtime state of a native Live2D document: one parameter value, and imported Expression JSON files. Nothing here edits the file or adds history.
For INX parameter values use `value_set` (parameters-bindings.md).

## Sequence

1. Explore: `live2d_expression_list {}` answers the imported expressions and the active one.
2. Act: `live2d_parameter_set {parameterId, value}` sets one runtime parameter inside its authored range (a repeating parameter wraps a value outside it) (native Parameter ID string such as the ones in `resource://soul2d/document/state` `values`);
   `live2d_expression_import {name, path}` loads an Expression JSON; `live2d_expression_apply {name, fades?}` applies it once after the active motion or timeline preview; `live2d_expression_reset {}` clears it.
3. Confirm: `resource://soul2d/document/state` (`values` keyed by native Parameter ID); `render` to see the effect (approximate for native scenes; the result lists warnings).

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `live2d_parameter_set` / `Live2DCommand_ParameterSet` | `{parameterId, value}` changes later authoring queries without changing saved data, `revision` or undo. |
| `live2d_expression_list` | `{}` read only. |
| `live2d_expression_import` | `{name, path}` bounded JSON file. |
| `live2d_expression_apply` | `{name, fades?}` `fades: true` fades the expression in over its `FadeInTime` as `motion3_advance`, `live2d_animation_advance` or `live2d_physics_step` advance time (`nativePlayback.expressionWeight` in the state). Without it the expression applies at full weight. |
| `live2d_expression_reset` | `{}` |

## Gotchas

- Verified: `live2d_parameter_set` on an INX document fails; it needs an active Live2D document.
- `live2d_parameter_set` does not move `revision` (manifest), so do not use `revision` to detect that someone else changed a value.
- Expressions are session-only; `open` and `new` drop them.
- Not run: `Live2DCommand_ParameterSet`; `live2d_expression_apply` with `fades: true`.
- An absent or negative `FadeInTime` is no fade. `FadeOutTime` is not played: reset and switching expressions cut at once, and applying the active expression again with `fades: true` starts its fade-in from zero.
- A host that calls both `motion3_advance` and `live2d_physics_step` each frame advances the fade twice.
