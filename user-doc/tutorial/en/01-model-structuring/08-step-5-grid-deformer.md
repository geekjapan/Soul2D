English | [日本語](../../ja/01-model-structuring/08-step-5-grid-deformer.md)

# 08. Step 5: Add GridDeformer only where you need it

Use a GridDeformer to deform a wide area, such as the face or torso, all at once.  
It is convenient, but too many make the model hard to manage, so limit them to the places you need.

## Where to add them

As a rule, add a GridDeformer in these places:

- Face
- Back hair
- Side hair
- Torso
- Front hair
- Around the chest, if the chest is a separate part

For wide areas such as the face and torso, it is hard to work with only a mesh on each Part. If you first create a deformation group with a GridDeformer, it is easier to adjust expressions and body direction naturally later.

## How to insert

If a grouping `Node` is already at the same position and you want to make it a grid, convert that `Node` to a `GridDeformer` (Convert To...). If there is no grouping `Node`, select the Part you want to deform and insert a GridDeformer as its parent (Insert → GridDeformer).

AutoMesh makes the mesh of a Part only, so you do not apply it to the GridDeformer itself. A new GridDeformer starts as a single cell: shape its grid yourself, with the Grid Vertex Tool in Edit Mesh (drag its points, double-click to add or remove lines) or, over MCP, with `deformer_define_grid`. If you plan to move a wide area, prepare a GridDeformer outside the Part before you apply AutoMesh to the Part.

## Grid cell count and margin

As a guide, use about 8×8 cells for the face and back hair and about 10×14 for the torso. Adjust them to the character's aspect ratio and level of detail. The "Grid cuts" setting of the Grid Vertex Tool is the number of lines per axis of a newly drawn grid, so 8 cells need 9 cuts.

As a guide, make the grid about 10% larger than the Parts it covers. With some margin, the edges move more naturally when you deform. Vertices that leave the grid are clamped to its edge; over MCP, `deformer_validate` lists the children that leave a grid and `deformer_extend_grid` adds the missing lines.

## DynamicComposite for eyes and mouth, and grids

If you use a `DynamicComposite` to manage face variations, such as the eyes and mouth, a grid also makes the effect of deformation easier to see. Start with about 6×6 to 8×8, and increase or decrease it as needed.

## Example operations

1. Select the face Part.
2. Right-click it in the node tree, type `::G` in the Suffix field, and choose Insert → GridDeformer to get a parent named `Face::G`.
3. Check that the `Face` Part remains as a child of `Face::G`.
4. Insert a GridDeformer for the back hair, side hair, torso, front hair, and chest as needed.

## Next

- To set up the front hair, chest parts, and clothing  
  → [09. Step 6: Place the front hair Grid, chest parts, and clothing](./09-step-6-front-hair-chest-clothes.md)
