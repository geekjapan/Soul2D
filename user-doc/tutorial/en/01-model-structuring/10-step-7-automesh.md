English | [日本語](../../ja/01-model-structuring/10-step-7-automesh.md)

# 10. Step 7: Configure AutoMesh and use batch operations

After you finish the tree structure and the GridDeformer placement, create the meshes.  
Here, you handle the grids that move wide areas separately from the meshes that follow the shape of each Part.

## Basic rules

Apply AutoMesh with the `Optimum` algorithm to a `Part` by default. Select the Part, open the Tools panel, and in "Auto mesh" choose the Algorithm (Grid, Contour, Optimum, or Skeleton) and press "Generate mesh". Over MCP, use `automesh_generate`. AutoMesh refuses a Part that already has deform bindings, welds, or depth data, so mesh the Parts before you add parameters.  
AutoMesh works on Parts only. A `GridDeformer` has no AutoMesh; you define its grid with the Grid Vertex Tool in Edit Mesh (see step 5).

## Finishing flow

1. Check that a GridDeformer is in place for every range you want to deform together, such as the face, torso, and front hair.
2. Check that the group of Parts you want to deform is under the GridDeformer.
3. Define the grid of each GridDeformer with the Grid Vertex Tool in Edit Mesh.
4. Apply Optimum to each Part.

In this order, you can manage the grid that moves wide areas separately from the mesh that follows the shape of each part.

## Common mistakes

- Expecting AutoMesh to make the grid of a GridDeformer
- Using the Grid algorithm on a Part when you want the mesh to follow its shape (use Optimum)
- Forgetting to add a GridDeformer to a wide area such as the face or torso
- Processing a range you want to move as a grid and a range you want to follow the part shape with the same settings

## Batch operations on several parts

When you repeat the same node operation on similar parts, a batch operation with multiple selection is convenient.  
You can use it for inserting or adding nodes (each selected node gets its own new node) and for moving several child nodes to the same parent node. AutoMesh runs on one Part at a time: keep the same Algorithm and Preset, and press "Generate mesh" on each Part in turn.

After a batch operation, check in the tree that only the intended targets changed.

## Next

- To register flip pairs  
  → [11. Step 8: Register flip pairs](./11-step-8-symmetry-pairs.md)
