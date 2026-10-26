English | [日本語](../../ja/02-parameter-definition/04-face.md)

# 04. Face Orientation

In this section, you set `Face::Yaw-Pitch` to turn the face left, right, up, and down, and `Face::Roll` to tilt the head.  
Face orientation strongly affects the impression of the whole model. Don't try to move it a lot from the start. Decide on a range that looks natural for the structure of the original artwork.

## Angle guidelines

When you build from front-facing artwork, aim for about 30 degrees of left-right turning and about 15 to 20 degrees of up-down turning. Depending on the art style, you might reach about 45 degrees left and right, but it breaks easily if the depth of the nose, eyes, outline, and hair is insufficient.

To create larger turns, prepare separate assets: a nose for the side view, parts that hide the far-side eye, an outline close to a profile, and hair variations. If you don't have these, don't force a large angle. Keep to the range that you can interpolate naturally from the front artwork.

## Parameters to create

- `Face::Yaw-Pitch`
  `min=(-1,-1)`, `default=(0,0)`, `max=(1,1)`
- `Face::Roll`
  `min=-1`, `default=0`, `max=1`

`Face::Yaw-Pitch` is a two-dimensional parameter. The X direction handles left-right turning, and the Y direction handles up-down turning.  
`Face::Roll` is a one-dimensional parameter. It handles only the motion of tilting the head to the left and right, and you set it separately from left-right turning.

## Editing policy

Create the large deformation of the whole face with a face GridDeformer or MeshGroup, such as `Face::G`. Create the rough orientation with the GridDeformer or MeshGroup, and then correct individually the eyes, mouth, nose, outline, bangs, and so on that it can't fix alone.

To make the face look rotated, it is important to change the amount of deformation between the front and the back. For the nose, mouth area, and bangs on the front, adjust the position and shape so that they show depth. Make the eyes, eyebrows, mouth, and ears on the face follow so that they don't look like they slide off the face surface.

When you create the orientation of the whole face with a GridDeformer or grid MeshGroup, it is easier to adjust if you treat the face surface as a curve with depth. For details, see [03. Basic Setup Procedure - Build large deformations with depth in mind](./03-basic-editing-flow.md#build-large-deformations-with-depth-in-mind).

If you deform the eyes and mouth with a GridDeformer alone, their shape tends to distort. After you create the orientation of the whole face, check the shape of the eyes and mouth individually. If needed, correct them on the Part or DynamicComposite side.

## Set `Face::Yaw-Pitch`

First, register the front state at `(0,0)`. This is the reference for everything, so check that the eyes, mouth, nose, outline, and hair look natural as the front state.

Next, set one side, left or right. At `(-1,0)` or `(1,0)`, create the shape where the whole face turns sideways. For a model drawn left-right symmetrically, set one side carefully, and then create the opposite side efficiently by selecting the opposite end point and choosing "Set from mirror" → "Horizontally" in the keypoint actions under "Armed Parameter" (MCP: `binding_keypoint` with `set_from_mirror_horizontal`).

You can't create the up-down direction with a left-right flip, so set it individually. At `(0,-1)` and `(0,1)`, create the downward-facing and upward-facing faces. After you edit up and down, "Symmetrize Deform" (the same keypoint actions; `symmetrize_deform`) helps reduce left-right breakage.

After you set the end points, add the intermediate keys `(-0.5,0)`, `(0.5,0)`, `(0,-0.5)`, and `(0,0.5)`. In the intermediate keys, adjust so that the outline, eye position, mouth area, bangs, and neck don't move suddenly.

Finally, check the four corners `(-1,-1)`, `(-1,1)`, `(1,-1)`, and `(1,1)`. When you apply left-right and up-down at the same time, a deformation that looked natural on the axes can break. Treat the four corners not as places to simply add up the settings on the axes, but as places to correct the final appearance.

## Set `Face::Roll`

With `Face::Roll`, you tilt the face left and right around the base of the neck. First register the front state at `0`, and then create the tilt at one side, `-1` or `1`. If the model can be symmetric, create the opposite side with "Set from mirror" (a single button for a 1D parameter).

With only the end points, the outline and neck can look stiff in the middle of the tilt, so place intermediate keys at `-0.5` and `0.5` as well. In the intermediate keys, check that the neck, hair, ears, and face outline connect naturally.

## What to check

- Is the left-right direction of the face within the range you can create naturally from the original artwork?
- Are the nose, far-side eye, outline, and hair variations that you need for a large sideways turn missing?
- Do the eyes, eyebrows, mouth, and ears look like they slide off the face surface?
- After you set the up-down direction, is the left-right symmetry of the face still intact?
- Did you correct the breakage that appears at the four corner keys when you apply left-right and up-down at the same time?

## Next

- To set up body orientation  
  → [05. Body Orientation](./05-body.md)
