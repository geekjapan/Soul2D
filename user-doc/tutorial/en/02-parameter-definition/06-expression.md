English | [日本語](../../ja/02-parameter-definition/06-expression.md)

# 06. Expressions

In this section, you set the basic expression parameters for the eyes, eyebrows, and mouth.  
Face and body orientation parameters create a spatial orientation, but expression parameters organize which shape to show. Don't divide them too finely. First, prioritize creating the representative shapes reliably.

## Parameters to create

- `Eye::L::Blink` / `Eye::R::Blink`
  `min=(0,-1)`, `default=(0,0)`, `max=(1,1)`
- `Eye::L::X-Y` / `Eye::R::X-Y`
  `min=(-1,-1)`, `default=(0,0)`, `max=(1,1)`
- `Eyebrow::L` / `Eyebrow::R`
  `min=(-1,-1)`, `default=(0,0)`, `max=(1,1)`
- `Mouth::Open`
  `min=(0,-1)`, `default=(0,0)`, `max=(1,1)`

Make the left and right eyes and eyebrows separate parameters. This lets you close only one eye or move only one eyebrow.  
Don't split the mouth into left and right. Handle opening and closing and expression direction together with `Mouth::Open`.

## Eye blink

`Eye::L::Blink` and `Eye::R::Blink` handle the open-close amount in the X direction. `0` is the open state, and `1` is the closed state. Use the Y direction for expression-oriented closing, to separate a smiling closed eye from a deeply closed shape.

First, register the open eye at `(0,0)`. Next, create the partly closed shapes at `(0.25,0)`, `(0.5,0)`, and `(0.75,0)`, and finally create the normal closed eye at `(1,0)`. A blink involves a large change of shape, so if you skip the intermediate keys, the eyelids and eyelashes tend to get crushed unnaturally during interpolation.

After the normal closed eye is done, create the smiling closed eye at `(1,-1)` and the deeply closed shape at `(1,1)`. Check the eyelid curve, the eyelashes, and the direction of the outer corner of the eye, so that the smiling closed eye and the deeply closed eye don't end up as the same shape.

## Eye position

`Eye::L::X-Y` and `Eye::R::X-Y` are parameters for adjusting the relative position of the eyes. Register `(0,0)` as the center position, and set the left-right and up-down positions at `(-1,0)`, `(1,0)`, `(0,-1)`, and `(0,1)`.

You might also use them not for moving the gaze but for correcting the position of the eyes to match the face orientation. When you combine them with `Face::Yaw-Pitch`, check that the eyes don't look like they slide off the face surface.

## Eyebrows

`Eyebrow::L` and `Eyebrow::R` handle the vertical position of the eyebrow in the X direction and the direction of the expression in the Y direction.  
First, register the neutral position at `(0,0)`, and adjust the eyebrow height at `(-1,0)` and `(1,0)`. Next, create the shape for the anger direction at `(0,-1)` and the shape for the sadness and troubled direction at `(0,1)`.

Left-right differences in the eyebrows strongly affect the expression, so you don't need to make the left and right exactly the same. However, set the left and right parameters independently, so that moving only one eyebrow doesn't affect the other.

## Mouth

`Mouth::Open` handles opening and closing of the mouth in the X direction and the expression direction in the Y direction. Register the neutral closed mouth at `(0,0)`, and create the neutral open mouth at `(1,0)`.

After that, create the closed mouth and open mouth of the joy direction at `(0,-1)` and `(1,-1)`, and create the closed mouth and open mouth of the anger direction at `(0,1)` and `(1,1)`. The amount of opening and the expression direction change at the same time for the mouth, so check that the expression looks natural not only with the closed mouth but also with the open mouth.

## What to check

- Can you move the left and right eyes and eyebrows independently?
- Do the eyelids and eyelashes look unnaturally crushed at the intermediate blink keys?
- Can you tell the smiling closed eye and the deeply closed eye apart visually?
- Are the vertical position of the eyebrows and the expression direction, such as anger and sadness, mixed up?
- Does the mouth hold up when you move opening and closing and expression direction at the same time?
- When you combine them with `Face::Yaw-Pitch`, do the eyes, eyebrows, and mouth look detached from the face surface?

## Next

- To go on to how to handle physics and the final checks  
  → [07. Physics and Checkpoints](./07-physics-and-checklist.md)
