English | [日本語](../../ja/01-model-structuring/05-step-2-face-hierarchy.md)

# 05. Step 2: Set up the parent-child relationships of the neck, face, eyes, and mouth

Next, set up the structure from the neck up.  
If you build this correctly, the whole face follows naturally when you rotate the neck.

## Ideal relationships

Arrange the neck, face, eyes, and mouth to match their visible parent-child relationships.

- Make the neck (the `Neck` Part) a child of the torso (the `Body` Part).
- Make the face (the `Face` Part) a child of the neck (the `Neck` Part).
- Make the right eye, left eye, mouth, and ear Parts children of the face (the `Face` Part).
- Group the eyes and mouth with a `DynamicComposite` if needed.
- Make a shadow Part a child of the main Part that receives the shadow.

The important point here is that the parent is a `Part`. Being under a `GridDeformer` or a `Node` may not be enough as a relationship between Parts.

## Common mistakes

- Hanging the face Part directly from the torso
- Assuming that putting the Part under a `GridDeformer` or a `Node` also made the face and neck parent-child relationships correct
- Placing the shadows of the eyes and mouth somewhere other than under their main Parts

If the parent-child relationships are off, the eyes, mouth, and shadows may be left behind when you move the neck or face.

## Example operations

1. Place the `Neck` Part under the torso Part, or under a suitable parent on the torso side.
2. Place the `Face` Part as a child of `Neck`.
3. Group the Parts inside the face (eyes, mouth, ears, and so on) as children of `Face`.
4. If you have several variations of the eyes or mouth, group them with a `DynamicComposite`.
5. Move each shadow Part so that it becomes a child of its main Part.

## Next

- To arrange limbs and hair from root to tip  
  → [06. Step 3: Arrange limbs and back hair from root to tip](./06-step-3-chain-parts.md)
