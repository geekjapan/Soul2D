English | [日本語](../../ja/01-model-structuring/09-step-6-front-hair-chest-clothes.md)

# 09. Step 6: Place the front hair Grid, chest parts, and clothing

Here, you set up how to place parts that tend to fall outside the basic structure, such as the front hair, chest, and clothing.  
These parts are easier to handle if they follow the face or torso and can still be deformed individually when needed.

## Grid for the front hair only

If you want to deform only the front hair by a large amount, prepare a GridDeformer for the front hair, separate from the face.  
Place `FrontHair::G` as a child of the face, and hang the front hair Parts below it.

If you separate the face deformation from the front hair deformation, the front hair follows the face direction, and you can still add extra sway or deformation to the hair alone.

## When the chest is a separate part

If the chest is a separate part from the torso, add a GridDeformer to the torso and place the chest Part as its child.  
This makes the chest follow the direction and sway of the torso more easily.

Before you move the chest independently, place it first so that it follows the deformation of the whole torso.

## Placing clothing

Place clothing parts that attach to the body, such as a skirt, as children of the torso.  
If the clothing moves with the body and you can still rotate or deform the clothing alone when needed, it is easier to adjust later.

## Next

- To configure AutoMesh  
  → [10. Step 7: Configure AutoMesh and use batch operations](./10-step-7-automesh.md)
