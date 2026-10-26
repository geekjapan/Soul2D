English | [日本語](../../ja/02-parameter-definition/03-basic-editing-flow.md)

# 03. Basic Setup Procedure

After you decide the value ranges and key positions, set the actual deformation at each key position. This chapter covers the basic operations common to the face, body, and expressions.

When you set up parameters, first decide which parameter to arm for editing and which key position to edit. Then deform the GridDeformer, MeshGroup, Part, and so on. Don't touch the deformation target first and select the parameter afterward. Decide the parameter and key position first, and then apply the deformation.

## Basic flow

For each parameter, proceed in this order:

1. Select the parameter to edit.
2. Arm that parameter: press its "Arm" button in the Parameters panel.
3. Select the key position to edit.
4. At that key position, deform the GridDeformer, MeshGroup, Part, and so on.
5. If needed, add corrections with other Deformers or Parts.
6. Move to the next key position and set the deformation in the same way.
7. After you set the keys on the axes, check the diagonal and combination keys.

"Armed for editing" means a state in which operating a key position of that parameter lets you edit the corresponding deformation setting. If the parameter is not armed, the deformation might not be registered to the keys of the intended parameter, even if you think you are moving the appearance.

The armed parameter is shown under "Armed Parameter", and its button then reads "Armed" (MCP: `editor_armed_set`). While a parameter is armed, the viewport mesh tools of the selected GridDeformer, MeshGroup, PathDeformer or Part edit the deformation at the key instead of the mesh itself (MCP: `deform_edit`), and "Edit Mesh" and the transform handles are not offered. To set a node's translation, rotation or scale at the key numerically, select that node's binding under "Armed Parameter" (MCP: `binding_set_trs`).

## Select the key position, then deform

You select a key position by the value of the parameter: set the value with the X and Y sliders or the XY pad, and the key position nearest to that value is the one you edit. The panel lists its bindings as "Bindings at key X, Y" (the indices of the axis points, counted from 0). For example, for `Face::Yaw-Pitch`, `(0,0)` is front, `(-1,0)` and `(1,0)` are left and right, and `(0,-1)` and `(0,1)` are down and up.

At each key position, create the shape you want to show at that value. While `(-1,0)` is selected, create a face turned left. While `(0,1)` is selected, create a face turned up. If you edit a Deformer without selecting a key position, it becomes unclear which value the deformation belongs to.

The center `(0,0)` is the reference. Register the center first, and then set the end points and intermediate points. If you create the end points first and then fix the center, the relationship with the deformations you already set tends to break.

## Choose the deformation target

Create large shapes with a GridDeformer or MeshGroup. Use them when you want to move several Parts together, such as the whole face, the whole body, or a group of hair.

Adjust Parts directly where a GridDeformer or MeshGroup alone doesn't give a good result. The eyes, mouth, nose, clothing edges, accessories, and so on might lose their shape if you only ride them on the overall deformation. In that case, correct them individually at the relevant key position.

The basic order of priority is as follows:

1. Create the large orientation and position with a GridDeformer or MeshGroup.
2. Correct the Parts whose shape broke, one by one.
3. Adjust the Parts that need front-back order or depth.
4. Check the keys on the axes, and correct the diagonal and combination keys last.

## Build large deformations with depth in mind

When you deform a GridDeformer or a MeshGroup whose vertices are arranged in a grid, it helps to think of the shape of the Parts it contains as a surface with depth. The front-facing artwork itself is 2D, but the groups that make up the face, body, hair, and clothing have parts that stand forward and parts that recede.

For orientation parameters such as `Face::Yaw-Pitch` and `Body::Yaw-Pitch`, treat this depth like depth-of-field information. Think of the GridDeformer or grid MeshGroup as a loosely curved surface that follows that depth information. If you move the vertices as if sliding them along that curve, you can create left-right and up-down rotation with relatively little adjustment.

For example, on a face, think of the nose, the mouth area, and part of the bangs as front, and the ears and the far side of the outline as back. When you turn the face sideways with Yaw, move the front parts a lot and the back parts a little. When you turn the face up or down with Pitch, also change the amount of deformation according to the difference in depth, so that the forehead, nose, chin, and neck don't look stuck on the same plane.

This approach is not meant to finish everything with a GridDeformer alone. First create the large orientation along the curve. Then correct individually the parts that depth alone can't explain, such as the eyes, mouth, clothing edges, and accessories.

## Don't finish at a single key position

A shape that looks natural at a single key position can still break in the interpolation with the neighboring key position. Don't check only the end points `-1`, `0`, and `1`. Also check the intermediate keys at `-0.5` and `0.5` as needed.

For face and body orientation in particular, create the end points and then look at the intermediate keys. If the outline, neck, clothing, hair, eyes, or mouth move suddenly in the middle, add corrections to the intermediate key.

Don't stop at the keys on the axes of a 2D parameter. At diagonal keys such as `(-1,-1)`, `(-1,1)`, `(1,-1)`, and `(1,1)`, the left-right and up-down deformations apply at the same time, so failures that you couldn't see on the axes can appear.

## Next

- To set up face orientation
  → [04. Face Orientation](./04-face.md)
