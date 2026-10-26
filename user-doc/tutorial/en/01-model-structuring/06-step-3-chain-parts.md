English | [日本語](../../ja/01-model-structuring/06-step-3-chain-parts.md)

# 06. Step 3: Arrange limbs and back hair from root to tip

Arrange long, connected parts, such as limbs and hair strands, in the tree in order from root to tip.  
If the side closer to the body is the parent and the tip side is the child, it is easier to set up rotation and sway.

## Ideal order

- For an arm, use the order `Body` → `UpperArm` → `LowerArm` → `Hand`.
- For a leg, use the order `Root` → `Leg R::Root` / `Leg L::Root` → `UpperLeg` → `LowerLeg` → `Foot`.
- For wings, tail, back ribbon, and similar parts, use the order torso or base Part → `Root` node → `Base` → ... → `Tip`.
- For back hair, arrange it like `Head::Root` or `Body` → `BackHairRoot` → `HairChunk` → `HairTip`.
- If you group the mouth Parts with a `DynamicComposite`, put the reference mouth node on the root side and hang the variations below it.

## Common mistakes

- Making the tip of a hand or hair the parent
- Placing wings, tail, back ribbon, and similar parts directly under Root
- Hanging everything directly from the base Part without creating a Node at the root

If the tip is the parent, it is hard to move the part naturally from its root. Sandwich a Node at the root and use it as the reference for rotation and movement.

## Example operations

1. Check the Parts of the right arm in order, starting from the one closest to the root.
2. Select the root Part, insert a `Node` with Insert → Node (use a Suffix such as `::Root`, or rename it with the Name field in the Inspector), and name it something like `Arm R::Root`.
3. Under `Arm R::Root`, arrange the Parts in the order `UpperArm` → `LowerArm` → `Hand`.
4. For the legs, create `Leg R::Root` / `Leg L::Root` directly under Root, and under each one arrange the Parts in the order `UpperLeg` → `LowerLeg` → `Foot`.
5. Arrange wings, tail, back ribbon, and similar parts from root to tip in the same way, using the torso or a skirt as the base.

## Next

- To insert helper nodes as parents  
  → [07. Step 4: Insert Node / Composite / DynamicComposite as parents](./07-step-4-insert-helper-nodes.md)
