English | [日本語](../../ja/01-model-structuring/04-step-1-root-blocks.md)

# 04. Step 1: Create large blocks directly under Root

First, split the model into large groups.  
If you create the large frames here (torso, limbs, wings, tail, back ribbon, accessories, and so on), later operations such as "move the whole torso" or "change only the head angle" become easy.

## Approach

Organize large units such as the torso and limbs directly under Root. Select one Part of each group you want to bundle, and use Insert → Node (type a Suffix such as `::Root` first) to create groups such as `Body::Root`, `Arm R::Root`, `Arm L::Root`, `Leg R::Root`, and `Leg L::Root`. Insert wraps each selected node in its own new Node, so move the other Parts of the group under the new Node afterwards (drag them in the tree, or use "Move destination" and "Move" in the Nodes panel).

At this stage, the important thing is not to finalize every parent-child relationship. It is to create groups that are easy to reorganize later. You organize the head area as a child of the neck in the next step. You reattach the wings, tail, and similar parts in a later step so that they become children of the torso or of a base clothing Part.

A Node inherits the origin position of the Part that you selected when you inserted it. This makes it easier to use as a reference for rotation and movement later.

## Common mistakes

- Placing every Part directly under Root
- Not creating groups, and then selecting several Parts each time to move them
- Mixing the large frames of the head, arms, legs, clothing, and accessories

If you place everything directly under Root, the structure tends to break when you later want to move only the head, only the torso, or only the arms.

## Example operations

1. Select the `Body` Part, right-click it in the node tree, type `::Root` in the Suffix field of the Insert menu, and choose Insert → Node to create `Body::Root`. Then move the other torso Parts under it.
2. Create a Root node for each side of the arms and legs in the same way.
3. Also group things that you might move independently, such as wings, tail, back ribbon, and accessories, as large frames.
4. For the head area, a temporary group is fine at this stage. You set up the neck and face parent-child relationships in the next step.

## Next

- To set up the parent-child relationships of the neck, face, eyes, and mouth  
  → [05. Step 2: Set up the parent-child relationships of the neck, face, eyes, and mouth](./05-step-2-face-hierarchy.md)
