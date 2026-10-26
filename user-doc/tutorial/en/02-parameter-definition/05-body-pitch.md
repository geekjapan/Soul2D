English | [日本語](../../ja/02-parameter-definition/05-body-pitch.md)

# 05-2. Body Pitch

In this section, you set the forward and backward lean of `Body::Yaw-Pitch`.
For Pitch, you create the result of tilting the body forward and backward around the waist, assuming the depth of the target Part or the Parts under the GridDeformer.

## Forward and backward lean from the front

At the front position, that is, `Yaw=0`, you create the forward lean and the backward lean separately.

For the forward lean, you create the result of tilting the body forward around the waist. However, simply lowering the whole upper body doesn't look like a forward lean. Keep the waist and lower abdomen as the pivot, and deform the target parts or the parts under the GridDeformer, assuming their depth, so that the group of the chest, shoulders, and neck tilts forward and down.

The chest and the front of the clothing on the near side move slightly more, and the neck area and the far side of the shoulders move in the direction where the chest hides them. The chest pushes the neck, so it becomes harder to see. Keep the neck from coming in front of the chest, and let it settle behind the chest. Also adjust the position of the head. If you enlarge the face by about 3% to 7%, it is easier to create the impression that the head comes forward.

For the backward lean, you create the result of straightening the body backward around the waist. The effect is less visible than the forward lean, but adjust the GridDeformer so that the shoulder width becomes slightly narrower, the chest opens, and the upper body straightens backward. Opposite to the forward lean, the upper chest, neck, and shoulders rise slightly, and the waist side stays as the pivot.

If you treat the part that recedes toward the back and the part of the clothing and chest that stays in front as separate depths, it is less likely to turn into a simple scale. If you make the face about 3% to 5% smaller, it is easier to create the impression that the head recedes backward.

After you set the forward and backward lean, also check both shoulders and the arm roots at the same key position. Even if you create the shape of the body with only a GridDeformer, if the arm roots stay at their original positions, the arms no longer connect from the shoulders.

## Forward lean when turned sideways

At the left-right turned positions, that is, `Yaw=-1` and `Yaw=1`, check the forward and backward lean individually. Here, don't simply add the front forward and backward lean. Create it as a posture in which the sideways-facing body leans forward and backward.

For the forward lean when turned sideways, first copy the sideways deformation of `(Yaw=-1, Pitch=0)` or `(Yaw=1, Pitch=0)` ("Copy keypoint", then "Paste keypoint" at the target key). Use that shape as the reference, and correct it so that it looks tilted forward by about 15 degrees around the waist. Don't use rotation of the whole node here.

If you use rotation, the depth differences of the chest, abdomen, clothing, and skirt all rotate at the same angle, and you can't express the thickness and overlap of the body. With GridDeformer and individual Part deformations, adjust separately the parts that come forward, the parts that are hidden at the back, and the parts that stay on the waist side.

## Backward lean when turned sideways

For the backward lean when turned sideways, also copy the sideways deformation first. Use that shape as the reference, and correct it so that it looks arched back by about 10 degrees. Don't use rotation here either. Create it so that the chest opens slightly, and the shoulders and neck straighten backward along the depth of the body.

If you simply move the whole thing diagonally upward, the sideways posture breaks, or the upper body and lower body look like they move separately. Using the waist as the reference, make the chest, abdomen, and top edge of the skirt connect as the movement of the same body.

## Diagonal keys

Create the diagonal keys `(-1,-1)`, `(1,-1)`, `(-1,1)`, and `(1,1)` not as a simple sum of the left-right direction and the front-back direction, but as a posture in which a body turned 30 degrees sideways leans forward or backward.

For the diagonal keys of the forward lean, keep the sideways outline, and make the upper body look tilted forward and down around the waist as the pivot. For the diagonal keys of the backward lean, make the chest open, and the neck and shoulders straighten backward along the depth of the body.

If there is a skirt, jacket, hood, or decoration around the waist, these tend to break especially in the forward and backward lean when turned sideways. Treat whether the fabric bulges forward or wraps behind the waist as depth, and check which parts are visible and which are hidden in the forward and backward lean.

When you create the diagonal keys, don't squeeze the far side too much. If you apply the sideways turn and the forward and backward lean at the same time, the far-side shoulder and armpit tend to shrink excessively. Even if the far side looks small, keep enough width to leave the thickness of the body.

## Next

- To organize Roll and the checklist for the body
  → [05-3. Body Roll and Checks](./05-body-roll-check.md)
