English | [日本語](../../ja/02-parameter-definition/02-values-and-keys.md)

# 02. Value Ranges and Key Positions

Before you create a parameter, decide its value range, default value, and key positions.  
The goal of this chapter is to fix what each value means before you move on to deformation setup.

## Basic policy

Create most parameters with a range of `-1` to `1` and a default of `0`.  
However, for axes that don't need negative values, such as opening and closing an eye or the mouth, use `0` to `1` for the open-close axis only.

- Things that move in both directions from the center, such as left-right, up-down, and tilt  
  → `min=-1`, `default=0`, `max=1`
- Things that have an "open / closed" amount, such as opening and closing an eye or the mouth  
  → For the open-close axis, `min=0`, `default=0`, `max=1`
- Things that have an expression direction, such as joy, neutral, and anger  
  → For the expression axis, `min=-1`, `default=0`, `max=1`

## Recommended value ranges

| Parameter | Dimensions | Meaning and values of X | Meaning and values of Y |
| --- | --- | --- | --- |
| `Face::Yaw-Pitch` | 2D | `-1=facing left`, `0=front`, `1=facing right` | `-1=facing down`, `0=front`, `1=facing up` |
| `Face::Roll` | 1D | `-1=tilt left`, `0=front`, `1=tilt right` | - |
| `Body::Yaw-Pitch` | 2D | `-1=facing left`, `0=front`, `1=facing right` | `-1=facing down`, `0=front`, `1=facing up` |
| `Body::Roll` | 1D | `-1=tilt left`, `0=front`, `1=tilt right` | - |
| `Eye::L::Blink` / `Eye::R::Blink` | 2D | `0=open`, `1=closed` | `-1=joy`, `0=neutral`, `1=deeply closed` |
| `Eye::L::X-Y` / `Eye::R::X-Y` | 2D | `-1=left`, `0=center`, `1=right` | `-1=down`, `0=center`, `1=up` |
| `Eyebrow::L` / `Eyebrow::R` | 2D | `-1=down`, `0=center`, `1=up` | `-1=anger`, `0=neutral`, `1=sadness` |
| `Mouth::Open` | 2D | `0=closed`, `1=open` | `-1=joy`, `0=neutral`, `1=anger` |

You can reverse the positive and negative directions of left-right and up-down to match an existing model or project convention.  
Within one model, however, keep them consistent. If you mix conventions, such as `-1=left` for the face but `-1=right` for the eyes, you can't check combinations later.

## Recommended divisions

After you decide the value ranges, decide where to place keys.  
If you use the same fine granularity on every axis, the workload grows too large, so add intermediate points only on the axes that affect motion quality.

| Target | Recommended key positions | Reason |
| --- | --- | --- |
| `Face::Yaw-Pitch` | `-1`, `-0.5`, `0`, `0.5`, `1` for both X and Y | Interpolation failures are obvious when the face turns, so you create the intermediate shapes. |
| `Face::Roll` | `-1`, `-0.5`, `0`, `0.5`, `1` | The outline and neck tend to break in the middle of a tilt. |
| `Body::Yaw-Pitch` | `-1`, `-0.5`, `0`, `0.5`, `1` for both X and Y | Stabilizes interpolation of the body, neck, and arm roots. |
| `Body::Roll` | `-1`, `-0.5`, `0`, `0.5`, `1` | Makes it easier to adjust the intermediate shapes of clothing and the shoulder area. |
| X of `Eye::{L,R}::Blink` | `0`, `0.25`, `0.5`, `0.75`, `1` | The shape changes greatly from open to closed. |
| Expression axis | `-1`, `0`, `1` or `0`, `0.5`, `1` | Too many steps on an expression direction are hard to manage. |

The expression axis is an axis that represents joy, neutral, anger, and so on, such as Y of `Blink`, Y of `Eyebrow`, and Y of `Mouth::Open`.  
Don't divide it as finely as head turning or blinking. Create only the representative expressions first.

## Key positions for 2D parameters

For a 2D parameter, create the center and the keys on the axes first.  
Treat diagonal keys as corrections for when you apply left-right and up-down at the same time.

- `(0, 0)`  
  The reference value: front, normal expression, initial state.
- `(-1, 0)` / `(1, 0)`  
  The ends of the X direction only. Check only the meaning of X, such as left-right, open-close, or vertical position.
- `(0, -1)` / `(0, 1)`  
  The ends of the Y direction only. Check only the meaning of Y, such as up-down or expression direction.
- `(-1, -1)` / `(-1, 1)` / `(1, -1)` / `(1, 1)`  
  Combination corrections. After you set the keys on the axes, correct only the places that break.

For `Face::Yaw-Pitch` and `Body::Yaw-Pitch`, create intermediate keys at `-0.5` and `0.5` in addition to the end points.  
First create `(0,0)`, `(-1,0)`, `(1,0)`, `(0,-1)`, and `(0,1)`. Then add `(-0.5,0)`, `(0.5,0)`, `(0,-0.5)`, and `(0,0.5)` to smooth the interpolation.

When X is `0..1`, as with `Eye::{L,R}::Blink` and `Mouth::Open`, you don't use the negative side of X.  
For `Blink`, create `(0,0)`, `(0.25,0)`, `(0.5,0)`, `(0.75,0)`, and `(1,0)`, and refine the shape from open to closed step by step.  
For expression directions, check the representative points such as `(0,-1)`, `(0,1)`, `(1,-1)`, and `(1,1)`, and add intermediate points only where you need them.

## Key positions for 1D parameters

For a 1D parameter, create the center and both ends first.

- `0`  
  The reference value: front, no tilt.
- `-1`  
  Tilt to the left side or counterclockwise.
- `1`  
  Tilt to the right side or clockwise.

`Face::Roll` and `Body::Roll` handle only tilt, not orientation.  
Interpolation tends to feel stiff with only the end points, so place intermediate keys at `-0.5` and `0.5` as well.  
If you want to add a deformation that turns the model left or right, return to `Face::Yaw-Pitch` or `Body::Yaw-Pitch`.

## Work order

For each parameter, proceed in this order:

1. Create the parameter.
2. Set `min/default/max`.
3. Register the center key.
4. Set the end keys in the X direction.
5. Set the end keys in the Y direction.
6. Correct the diagonal and combination keys.
7. Move it together with other parameters and check.

In the editor, step 1 is "+ Add parameter" in the Parameters panel, for example "Add 2D Parameter (-1..+1)" (MCP: `parameter_create`). For step 2, use the parameter's "Edit ▾" menu. "Edit..." sets the X and Y min and max and the axis points that serve as key positions ("Add X point" adds one in the widest gap; MCP: `parameter_set_axes`). "Set Starting Position" stores the current value as the default (MCP: `parameter_set_defaults`).

## Next

- To review the basic parameter editing procedure
  → [03. Basic Setup Procedure](./03-basic-editing-flow.md)
