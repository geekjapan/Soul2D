English | [日本語](../../ja/02-parameter-definition/05-body.md)

# 05. Body Orientation

In this section, you set `Body::Yaw-Pitch` to turn the body left, right, up, and down, and `Body::Roll` to tilt the body.
The body supports many elements, such as the neck, arms, clothing, chest, waist, skirt, and accessories, so edit it with more attention to consistency with the surrounding parts than you do for the face.

## Angle guidelines

Use about 30 degrees as the standard for left-right turning of the body. Within this range, it is easier to adjust without badly breaking the clothing, arm roots, and neck area.
To turn the body further, you need to create separately the parts that were not visible from the front, such as the sides of the clothing, the chest, the armpits, and the accessories.

## Parameters to create

- `Body::Yaw-Pitch`
  `min=(-1,-1)`, `default=(0,0)`, `max=(1,1)`
- `Body::Roll`
  `min=-1`, `default=0`, `max=1`

`Body::Yaw-Pitch` is a two-dimensional parameter. The X direction handles left-right orientation, and the Y direction handles forward and backward lean.
`Body::Roll` is a one-dimensional parameter. It handles only the motion of tilting the whole body to the left and right.

## Editing policy

First create the shape of the whole body with a GridDeformer or MeshGroup, and then adjust the details. If you build the details before the orientation of the whole body is decided, you have to redo more adjustments when you later move the whole body.

When you turn the body left, right, up, and down with a GridDeformer or grid MeshGroup, it is easier to organize the amount of movement around the shoulders, chest, and waist if you treat the body as a curve with depth. For details, see [03. Basic Setup Procedure - Build large deformations with depth in mind](./03-basic-editing-flow.md#build-large-deformations-with-depth-in-mind).

For the body GridDeformer, look at the chest, neck, shoulders, clothing, arm roots, waist, skirt, and so on under it, and decide a simple depth model first. For many front-facing models, you can treat the center front of the chest and clothing as the front, the neck as behind the chest, the shoulders as slightly behind the chest, and the armpits and clothing sides as further behind. If there is a skirt, also consider in which direction the fabric bulges as depth.

The Yaw of `Body::Yaw-Pitch` here is not a motion that tilts the body left and right. It is a motion in which the body turns left and right, that is, the chest, abdomen, waist, and clothing sides change direction. Use `Body::Roll` for the motion that tilts the body left and right.

If you create the body by rotating only the chest, only the abdomen, or only the waist individually, it breaks. Decide the orientation of the whole trunk first, so that the upper body, lower body, and skirt don't look like separate rotations. Then correct the chest, clothing, skirt, and accessories on top of that.

## Details

- To set up left-right turning
  → [05-1. Body Yaw](./05-body-yaw.md)
- To set up forward and backward lean
  → [05-2. Body Pitch](./05-body-pitch.md)
- To organize Roll and the checklist
  → [05-3. Body Roll and Checks](./05-body-roll-check.md)

## Next

- To set up left-right turning of the body
  → [05-1. Body Yaw](./05-body-yaw.md)
