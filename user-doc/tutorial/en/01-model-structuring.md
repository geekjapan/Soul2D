English | [日本語](../ja/01-model-structuring.md)

# Model Structuring Tutorial (Your First Node / Part Design)

This tutorial explains how to split a character model into pieces and in what order to build the parent-child relationships.

## How to read this tutorial

- To get the big picture first  
  → [01. Overview and target tree](./01-model-structuring/01-overview.md)
- To find decision criteria when you are unsure which operation to use  
  → [02. Decision guide](./01-model-structuring/02-decision-guide.md)
- To check only the prerequisites before you start  
  → [03. Preparation](./01-model-structuring/03-preparation.md)
- To follow the steps in order  
  → [04. Step 1: Create large blocks directly under Root](./01-model-structuring/04-step-1-root-blocks.md)  
  → [05. Step 2: Set up the parent-child relationships of the neck, face, eyes, and mouth](./01-model-structuring/05-step-2-face-hierarchy.md)  
  → [06. Step 3: Arrange limbs and back hair from root to tip](./01-model-structuring/06-step-3-chain-parts.md)  
  → [07. Step 4: Insert Node / Composite / DynamicComposite as parents](./01-model-structuring/07-step-4-insert-helper-nodes.md)  
  → [08. Step 5: Add GridDeformer only where you need it](./01-model-structuring/08-step-5-grid-deformer.md)  
  → [09. Step 6: Place the front hair Grid, chest parts, and clothing](./01-model-structuring/09-step-6-front-hair-chest-clothes.md)  
  → [10. Step 7: Configure AutoMesh and use batch operations](./01-model-structuring/10-step-7-automesh.md)  
  → [11. Step 8: Register flip pairs](./01-model-structuring/11-step-8-symmetry-pairs.md)
- To see the final check and the finished result  
  → [12. Checkpoints and reference tree](./01-model-structuring/12-checklist-and-tree.md)

## Next chapter

- After the model structure is ready, create the basic parameters  
  → [Parameter definition tutorial](./02-parameter-definition.md)
