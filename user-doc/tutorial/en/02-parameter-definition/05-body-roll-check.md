English | [日本語](../../ja/02-parameter-definition/05-body-roll-check.md)

# 05-3. Body Roll and Checks

In this section, you set `Body::Roll` and organize the checklist for all the body parameters.

## Set `Body::Roll`

With `Body::Roll`, you tilt the whole body to the left and right. First register the front state at `0`, and then create the tilt at one side, `-1` or `1`. If the model can be symmetric, create the opposite side with "Set from mirror" (a single button for a 1D parameter).

Don't confuse it with the Yaw of `Body::Yaw-Pitch`. Yaw is an operation that turns the body's orientation left and right, and Roll is an operation that tilts the body to the left and right. If you create a sideways turn that you should build with Yaw by tilting it like Roll, you can't express the thickness of the body or the sides of the clothing.

After you set the end points, place intermediate keys at `-0.5` and `0.5`. In the intermediate keys, check the connection of the shoulders, chest, clothing, and neck area. Also move the face and body together, and check that the neck area and arm roots don't shift unnaturally.

## What to check

- Is the left-right direction of the body within a range that doesn't break the clothing or the arm roots?
- Does Yaw look like the body turning, and not like a tilt to the left or right?
- Does the sideways turn look like about 30 degrees from the front, and do the chest, abdomen, waist, and clothing sides keep the same trunk thickness?
- Did you squeeze the far-side shoulder, armpit, and clothing side too much, so that the body looks like a thin board?
- If the chest is a separate part, is it adjusted naturally to match the orientation of the whole body?
- Does the neck look like it comes in front of the chest?
- Are you treating the skirt and thick clothing as part of the body's depth?
- Do the upper body, lower body, and skirt look like separate rotations?
- Did you correct, as needed, the clothing sides, pockets, decorative cords, accessories, and so on that are visible when the body faces sideways?
- In the forward and backward lean, do the position and size of the head match the body's motion?
- Does the forward lean look like the upper body tilting forward and down around the waist, and not like a simple sinking?
- Does the backward lean look like the chest opening and the upper body straightening backward, and not like a simple lift?
- In the forward and backward lean when turned sideways, did you create the deformation as depth differences of each part, without relying on node rotation (the Rotation fields of the bindings under "Armed Parameter")?
- At the diagonal keys, do the 30-degree sideways orientation and the forward or backward lean hold at the same time?
- When you move the face and body together, do the neck and arm roots stay intact?

After you set them, check the display at each key position. At the diagonal keys in particular, even if the values are a numerical combination of left-right and up-down, the actual appearance can show up as sideways sliding, sinking, or crushing of the far side. If it doesn't look natural, review only that key position, and adjust until the relationship among the waist, chest, shoulders, neck, and arm roots improves.

## Next

- To set up expressions
  → [06. Expressions](./06-expression.md)
