English | [日本語](../../ja/01-model-structuring/01-overview.md)

# 01. Overview and target tree

## What you build in this tutorial

This tutorial is a procedure for splitting a character model into pieces and building parent-child relationships in a sensible order.  
If you build the model this way:

- It is hard to break when you add parts later.
- AutoMesh is easy to apply.
- It is hard to break when you rotate the neck or hair.

In short, you get a model structure that is easy to work with.

## 1. Understand the model structure at a glance

First, learn the terms that appear most often and what each one does.  
You can pick up the details as you work.

- `Root`  
  The topmost node of the whole model.  
  The head, torso, limbs, and other blocks hang below it.

- `Part`  
  A node that holds an actual image (a part image).  
  As a rule of thumb, only a Part has a visible shape.

- `Node`  
  An empty container that you use to adjust pivots (centers of rotation) and offsets.  
  It has no appearance. It groups its child Parts.

- `Composite` / `DynamicComposite`  
  A grouping node that you use when you want to control several Parts as one, such as eye and mouth variations.

- `GridDeformer`  
  A node that deforms a wide area, such as the face or torso, with a grid.  
  The grid itself has no image. It deforms its child Parts together.

In the following steps,

- A Part holds the shape.
- Everything else (Node, Composite, GridDeformer) plays a supporting role.

You build the tree on these premises.

## 2. The tree shape this tutorial aims for

Aim for the following shape first. You can add or remove pieces to suit your character.

- Directly under Root, place the large frames, such as the torso (upper body) and the legs (lower body).
- Hang large independent pieces, such as the neck to face, arms, and clothing attachments (wings, tail, back ribbon, and so on), from **the `Body` (Part) or a base Part**.
- Place the legs directly under Root.
  If the legs move with the torso, the motion tends to look as if the character is not touching the ground, so separate them from the upper body.
- Make the neck a child of the torso and the face a child of the neck.
  Make the eyes, mouth, and ears children of the face, and make each shadow a child of its main part.
- Arrange limbs and back hair from root to tip.
  Make the side closer to the body the parent and the tip side the child.
- Add one GridDeformer to each wide area (face, back hair, torso, front hair, chest).
  If there are several front hair pieces, cover them together with a single GridDeformer.

## 3. Name symmetric pieces so you can use them later

Give the Parts and Deformers that have a left or right counterpart names that show the pairing, starting early in the build.  
If you register flip pairs later, you can choose "Set from mirror" → "Horizontally" among the keypoint actions of the Parameters panel while editing parameters. The editor then reads the deformation of the paired node and sets the mirrored value.

Use names that show the left and right rule, for example:

- `Eye::L` / `Eye::R`
- `Arm::L` / `Arm::R`
- `HairSide_l` / `HairSide_r`
- `Sleeve_L` / `Sleeve_R`

If you use names whose pairing is clear, such as `::L` / `::R`, `_l` / `_r`, or `_L` / `_R`, you can register flip pairs by pattern matching at the end.  
You do the actual registration at the end of the procedure, after the tree structure and mesh settings are done.

## Next

- To check the decision criteria  
  → [02. Decision guide](./02-decision-guide.md)
- To start working right away  
  → [03. Preparation](./03-preparation.md)
