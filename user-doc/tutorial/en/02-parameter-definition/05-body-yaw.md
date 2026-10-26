English | [日本語](../../ja/02-parameter-definition/05-body-yaw.md)

# 05-1. Body Yaw

In this section, you set the left-right direction of `Body::Yaw-Pitch`, that is, `Yaw=-1..1`.
Yaw is not an operation that tilts the body left and right. It is an operation that turns the body's orientation left and right.

## Create the reference

First, register the front state at `(0,0)`. Next, at one side, `(-1,0)` or `(1,0)`, create the shape where the body faces sideways.

For a nearly symmetric model, set one side, and then create the opposite side with "Set from mirror" → "Horizontally" under "Armed Parameter", as you do for the face. After the mirror copy, check the clothing overlap, the accessories, and the overlap with the hair and arms, and correct only the places that need it.

## How to create the sideways turn

As a guide, the sideways turn is about 30 degrees from the front. Look at the center line of the chest, the clothing overlap, the shoulder width, and the arm roots, and check that the result doesn't look like a simple parallel shift of the front view. To make the center of the body look slightly rotated, move the chest and the center of the clothing on the near side by a larger amount, and don't squeeze the armpits and shoulders on the far side too much. If you make the far side too thin, the body looks like a thin board.

For the left-right direction of Yaw, don't try to show a sideways turn by tilting the body. Don't change the height of the shoulders or waist into a large diagonal. Instead, turn the chest, abdomen, waist, and clothing sides while they keep the same trunk thickness. For example, the front center of the chest moves toward the near side, and the far-side armpit and clothing side are slightly hidden. But if the waist and skirt also look like they turn in a different direction, it looks unnatural.

## Clothing and skirt thickness

If there is a skirt or thick clothing, also treat its bulge as part of the trunk thickness. When the body turns sideways, the fabric on the near side looks slightly wider, and the fabric on the far side is hidden or compressed.

However, if only the skirt looks like it rotates at a different angle from the body, the upper body and lower body look separated. Align the overall flow first, so that the waist, abdomen, and top edge of the skirt belong to the same trunk. Then correct the fabric's fullness.

Adjust individually, after you create the overall deformation, the parts that become visible only when the body faces sideways. The vertical lines on both sides of the clothing, pockets, jackets, hoods, decorative cords, and accessories can be such targets. For an accessory, it can be easier to adjust the position and angle of the Root node than to deform the shape itself.

## Intermediate keys

After you create the end points, add `(-0.5,0)` and `(0.5,0)`. In the intermediate keys, adjust so that the shoulders, chest, clothing sides, and arm roots don't appear to switch suddenly.

The legs strongly affect the sense of contact with the ground, so don't pull them too much into the body's orientation. Adjust the neck, arm roots, collar, and chest area while checking the range that follows the body.

## Next

- To set up forward and backward lean of the body
  → [05-2. Body Pitch](./05-body-pitch.md)
