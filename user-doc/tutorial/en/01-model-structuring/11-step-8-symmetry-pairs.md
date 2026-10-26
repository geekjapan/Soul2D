English | [日本語](../../ja/01-model-structuring/11-step-8-symmetry-pairs.md)

# 11. Step 8: Register flip pairs

After the tree structure, GridDeformers, and AutoMesh settings are done, register flip pairs last.  
If you register them after the left-right correspondence of Parts and Deformers is settled, you reduce re-registration when you change the structure later.

## Why you register them

If you register flip pairs, you can choose "Set from mirror" → "Horizontally" among the keypoint actions of the Parameters panel while editing parameters. The editor then reads the deformation of the paired node and sets the mirrored value. The pairs are saved with the document as extension data (`extdata_get` / `extdata_set` over MCP).

Register here every element that comes in a left-right pair, such as the face's left and right directions, arms, legs, side hair, sleeves, and accessories.

## Examples of names that are easy to register

- `Eye::L` / `Eye::R`
- `Arm::L` / `Arm::R`
- `Leg::L` / `Leg::R`
- `HairSide_l` / `HairSide_r`
- `Sleeve_L` / `Sleeve_R`

If you use names whose pairing is clear, such as `::L` / `::R`, `_l` / `_r`, or `_L` / `_R`, it is easy to register them by pattern matching.

## Example operations

1. Check that the names of the Parts and Deformers that form left-right pairs correspond according to a rule.
2. In the editor, choose Tools → "Flip Pairing" to open the flip pairing window.
3. Enter the left pattern in "Part 1" and the right pattern in "Part 2" (for example `::L` and `::R`), and press "Pair". Every unpaired node whose name contains the Part 1 text is paired with the node that has the same name with that text replaced by the Part 2 text, when that node exists. Repeat for each naming rule you used (`_l` / `_r`, `_L` / `_R`).
4. Check that the main left-right pairs are listed. You can also add a row by hand with "Add as new row" and fix a row with the buttons beside it. Press "Save" to keep the list.
5. When you edit face or torso parameters, check that "Set from mirror" → "Horizontally" flips and copies to the intended counterpart.

## Next

- To see the final check and the reference tree  
  → [12. Checkpoints and reference tree](./12-checklist-and-tree.md)
