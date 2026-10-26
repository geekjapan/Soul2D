English | [日本語](../../ja/01-model-structuring/07-step-4-insert-helper-nodes.md)

# 07. Step 4: Insert Node / Composite / DynamicComposite as parents

From here, you add helper nodes.  
The basic method is not to move existing children to another tree. Instead, select the Part you want to move and insert the node you need as its parent.

## When to use insert and when to use convert

Use insert when you want to add one level of parent. For example, to add a Node for rotation at the root of an arm, select the root Part and insert a Node as its parent.

Use convert when you want to change the type of an existing Node. For example, convert a `Node` to a `DynamicComposite` to manage eye or mouth variations. Right after you convert a `Node` to a `DynamicComposite`, toggle "Resize automatically" (in the Inspector of the `DynamicComposite`; the property is `auto_resized`) twice to settle the display range.

## Guide to usage

- Use `Composite` or `DynamicComposite` to manage eye and mouth variations.
- For arm and leg segments, insert a `Node` as the parent of the Part that serves as the starting point.
- If you make the mouth a `DynamicComposite` as well as the eyes, it is easier to keep the expression-variation rules consistent.

## Example operations

1. Select one of the eye Parts.
2. Right-click it in the node tree and choose Insert → Composite or Insert → DynamicComposite. Insert wraps each selected node in its own new node, so move the other eye Part into the new node afterwards (drag it in the tree, or use "Move destination" and "Move" in the Nodes panel).
3. Give it a name that shows its purpose, such as `Eyes::Comp`, with the Name field in the Inspector.
4. Select the Part at the root of an arm, and use Insert → Node to create a helper node such as `Arm R::Joint`.

## Next

- To add GridDeformer where you need it  
  → [08. Step 5: Add GridDeformer only where you need it](./08-step-5-grid-deformer.md)
