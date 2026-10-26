English | [日本語](../../ja/01-model-structuring/12-checklist-and-tree.md)

# 12. Checkpoints and reference tree

## Points to check at the end

Before you try moving the model, check the following:

- Are the large frames, such as `Body` (upper body) and `Leg R/L` (lower body), organized directly under Root?
- Do the arms, and the wings, tail, and back ribbon if you have them, hang from `Body` or a base Part?
- Is the face a child of the neck, and the neck a child of the torso?
- Are the parents of the face and neck Parts where they need to be?
- Are the right eye, left eye, and mouth each grouped with a `DynamicComposite`?
- Is any part of the eyes or mouth missing after you convert them to a `DynamicComposite`?
- Are the limbs and back hair arranged from root to tip?
- Are the Parts and Deformers that have a left or right counterpart registered as flip pairs?
- Are the shadow parts children of the main parts that cast the shadows?
- Are GridDeformers only in the places you need them, with Parts hanging correctly as their children?
- Is `Optimum` applied to each `Part`, and is the grid of each `GridDeformer` defined with the Grid Vertex Tool?

If you want to move the face, torso, hair, and similar areas together, check the outer GridDeformer as well as the Part meshes.  
For places you plan to deform a lot later, preparing the grid here makes adjustment easier.

If you have done everything up to here, the model structure is hard to break even when you add basic motion or fine adjustments.

## Reference tree example

Characters differ in practice, but this is one example that reflects everything so far.

- `Root`
  - `Body::G` (GridDeformer) → `Body` (Part)
    - `Neck` (Part) → `Face::Root` (Node)
      - `Face::G` (GridDeformer) → `Face` (Part)
        - `Eye R` / `Eye L` / `Mouth` (DynamicComposite)
        - `Ear`
        - `FrontHair::G` (GridDeformer) → `FrontHair` (Part)
      - `BackHair::G` (GridDeformer) → `BackHair` (Part)
      - `SideHair::G` (GridDeformer) → `SideHair` (Part)
    - `Arm R::Root` (Node) → `Arm R` (Part) → `Hand R` (Part + shadow)
    - `Arm L::Root` (Node) → `Arm L` (Part) → `Hand L` (Part + shadow)
    - `Skirt` (Part) → `Ribbon` / `Frill` (Part)
      - `Tail::Root` / `Wing::Root` / `BackRibbon::Root` (Node) → `Base` (Part) → ... → `Tip` (Part)
    - `Chest::G` (GridDeformer) → `Chest` (Part)
  - `Leg R::Root` / `Leg L::Root` (Node) → `UpperLeg` (Part) → `LowerLeg` (Part) → `Foot` (Part + shadow)
  - Accessories (add a Grid or physics as needed)

## What you have achieved

If you build the tree by following everything up to here, it is easy to see what moves when you move something.  
You are also less likely to pick the wrong AutoMesh range or algorithm, and you can extend the model without breaking its structure when you add parts later.

## Next

- To go on to defining and setting the basic parameters  
  → [Parameter definition tutorial](../02-parameter-definition.md)
