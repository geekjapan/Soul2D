English | [日本語](../../ja/01-model-structuring/02-decision-guide.md)

# 02. Decision guide

While you edit the tree, you often stop and wonder which operation to choose.  
When you are unsure, decide the following points first, then start working.

## 3.0 In this tutorial, "parent-child" means a link between Parts

This is the point people misunderstand most often.

- Phrases such as "the neck is a child of the torso", "the eyes and mouth are children of the face", and "the arms are children of the torso" refer to **parent-child relationships between Parts, which hold the visible image**.
- `Node` and `GridDeformer` play a supporting role. You can sandwich them above or below a Part, but **they do not replace the skeletal links such as "torso → neck → face" or "face → eyes/mouth".**

For example, if you insert `Body::G (GridDeformer)` into the torso, think of it in one of these two ways:

- **Keep `Body (Part)` as a child** of `Body::G`, and make the neck a child of `Body (Part)`.
- If you have a reason to make `Body::G` the parent, make it **clear that the torso reference is the Body Part**, so that you can treat the neck as a "child of the torso Part".

Wherever the following steps say "make X a child of Y", read it as: **build the parent-child relationship on Parts first, then sandwich a Node / GridDeformer in between if needed**.

## 3.0.1 Sandwich a Node only for parts that often move independently

Sandwiching a Node is not a universal rule.  
Use it mainly for **parts that often move independently** of the torso (arms and face, and legs for some characters).

These parts tend to break if they receive the parent's (torso's) deformation as is, and you often want a separate center of rotation for each. **Sandwich one Node** between the parent Part and the child Part to make them easier to handle.

The basic form has three levels:

- Parent (`Part`)
- `Node`
  Create it by selecting the child part and choosing Insert → Node (see 3.1)
- Child (`Part` or similar)

The key point is not to add a new Node at a higher level, but to **insert it as a parent of the child part**.  
This aligns the origin of the Node with the child part, which makes position shifts less likely and makes the deformation easier to adjust.

## 3.1 To add one level of parent, choose Insert

**When to use it**

- You want to create a center of rotation (pivot).
- You want to group existing Parts.
- You want to put a node "between" an existing parent and child.

**Why Insert**

Insert puts a node in as "the parent of the selected target", so **it is easy to align the inserted node's origin with the target part's position**.  
In contrast, Add tends to add a new node as a child of the parent, so the origin lands on the parent's coordinates or the default position. As a result,

- The part shifts a lot when you rotate it.
- The reference for deformation and masks does not match.
- You spend more effort on fixes later.

These cases are very common. Remember: **if your goal is to add hierarchy, choose Insert by default**.

**Where to find it**

- Right-click the node in the node tree and choose Insert, then the node class (`Node`, `Composite`, `DynamicComposite`, `GridDeformer`, and so on).
- Or use the Nodes panel: pick the class in "Insert class" and press "Insert".
- Type a "Suffix" first if you want a name: the new node is named after the selected node plus the suffix (the Part `Body` with suffix `::Root` gives `Body::Root`). Without a suffix it gets a default name. You can rename it later with the Name field in the Inspector.
- Insert wraps each selected node in its own new node. To put several Parts under one new node, insert it on one of them and then move the others into it (drag them in the tree, or use "Move destination" and "Move" in the Nodes panel).
- Over MCP, use `node_insert`.

## 3.2 If a hierarchy already exists but the type is wrong, use Convert To

**When to use it**

- The hierarchy already uses a `Node`, but you actually want a `DynamicComposite`.
- The hierarchy already uses a `Node`, but you actually want a `GridDeformer`.
- The grouping node has the wrong type, which makes variation management and masking hard.

**Why Convert To**

Inserting or adding a node does not solve the root problem here.  
What you want is not "an added level" but "a replaced node type". The shortest way is to **change the type itself with Convert To**. Right-click the node in the node tree and choose "Convert To...", or pick the class in "Convert to" in the Nodes panel and press "Convert". A `Node` can become a `MeshGroup`, a `DynamicComposite` or a `GridDeformer`. Over MCP, use `node_convert`.

**Note (when you want a GridDeformer)**

- A grouping `Node` is already at that position.  
  → **Convert To** that `Node` to a `GridDeformer`, replacing it in place.
- There is no `Node` to replace at that position, or you want to wrap the parts in one more grid.  
  → Select the **Part** you want to deform, and **insert a GridDeformer as its parent**.

## 3.3 Notes on converting Node → DynamicComposite with Convert To

Right after you convert a Node to a DynamicComposite, the display and the hit-area resize may not catch up.  
In that case, **toggle "Resize automatically" twice** (in the Inspector of the `DynamicComposite`; the property is `auto_resized`) to settle the resize.

## 3.4 Group the eyes and the mouth the same way

If you make only the eyes a DynamicComposite and leave the mouth as loose pieces, only the mouth variations and masks follow a different rule, and the structure tends to break.  
**If you group the mouth with a DynamicComposite as you do the eyes**,

- It is easy to add and switch expression variations.
- It is easy to organize the parent-child relationships of shadows and masks.
- It is easy to extend later.

You get these benefits.

## 3.5 Arrange connected parts such as arms, legs, and the mouth from root to tip

The parent's motion passes to the child in connected parts. If the root is the parent and the tip is the child, the follow-through looks natural. If you make the tip the parent,

- The root comes loose or gets pulled.
- It is hard to find the center of rotation.
- Physics and deformation settings become complicated.

These problems tend to occur. We recommend the "reference (root) → end (tip)" order not only for arms and legs but also for mouth opening and closing and the jaw area.

## 3.6 Grid / AutoMesh / batch operations are tools to reduce work

- As a starting point, use about **8×8** grid cells for a GridDeformer on the face or back hair and about **10×14** on the torso. Adjust them slightly to the aspect ratio. You shape the grid with the Grid Vertex Tool in Edit Mesh (drag its points, double-click to add or remove lines); its "Grid cuts" setting is the number of lines per axis of a newly drawn grid (8 cells need 9 cuts).
- AutoMesh (Tools panel → "Auto mesh") makes the mesh of a Part only. Use Optimum for a Part. A GridDeformer is not meshed by AutoMesh; you define its grid yourself.
- For wide areas such as the face and torso, prepare a GridDeformer outside the Part mesh. This makes large movements easier later.
- It helps to think of it this way: use AutoMesh (Optimum) where the mesh should follow the shape of the Part, and use a GridDeformer where you want to move the whole group with a grid.
- When you repeat the same operation many times, select several nodes and do it as a batch to reduce mistakes.
- After a batch operation, check that the settings went only to the Parts or GridDeformers you intended.

## Next

- To check the prerequisites before you start  
  → [03. Preparation](./03-preparation.md)
- To go to the actual steps  
  → [04. Step 1: Create large blocks directly under Root](./04-step-1-root-blocks.md)
