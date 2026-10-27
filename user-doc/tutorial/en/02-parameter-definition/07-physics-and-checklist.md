English | [日本語](../../ja/02-parameter-definition/07-physics-and-checklist.md)

# 07. Physics and Checkpoints

Think of the sway of hair, clothing, ribbons, and so on separately from the expression parameters of the face and mouth.  
You don't need to build everything in detail from the start, but decide which method to use for each.

## Handle hair with PathDeformer

If you handle a hair strand with a `PathDeformer`, enable the physics of the `PathDeformer`: turn on "Auto-Physics" in its Inspector section (MCP: `Inspector_Apply_PathDeformPhysicsEnabled`).

**Suitable cases**

- The hair strand is long and thin, and it bends from the root to the tip.
- You want to control the flow of the deformation with a path.
- You want to create a swaying hair tip.

In this case, using the physics on the `PathDeformer` side is more natural than forcing a regular expression parameter such as `Hair::Physics`.

## Handle hair and clothing with SimplePhysics

If you handle hair or clothing with regular `SimplePhysics`, add a `SimplePhysics` node to the target Part (node tree: "Add" → `SimplePhysics`; MCP: `Node_Add_SimplePhysics`).  
Then, if needed, create a two-dimensional parameter and choose it in the "Parameter" field of the node's `SimplePhysics` Inspector section (other settings: `physics_set`).

**Name examples**

- `HairFront::Physics`
- `HairSide::Physics`
- `Ribbon::Physics`
- `Sleeve::Physics`

A name like `<part name>::Physics` makes it easy to see which physics parameter corresponds to which Part.

## Decision criteria

- Use `PathDeformer` for long hair strands that bend from the root to the tip.
- Use `SimplePhysics` for small swaying items, and for hair, clothing, and ribbons that you want to sway per Part.
- Use a regular parameter for things whose shape you want to change on purpose in step with an expression.
- Use physics for things that you want to move with a natural delay, as inertia or sway.

## Edit shared CMO3 physics settings

For an open CMO3, the Physics groups panel has **Make physics settings independent**.
Use it before editing settings stored as shared references. It copies the complete
physics settings in one Undo step and keeps the original definitions for other
references. Parameter identities and authored values stay unchanged. Repeating it
on independent settings changes nothing. Unsupported storage is refused without
changing the document. This action does not establish Cubism interoperability or
simulation behavior.

MCP clients use `live2d_physics3_edit` with the current `expectedRevision` and
`detach: true`. Supply no other edit arguments.

## Final points to check

After you create the basic parameters, check the following:

- Is `Face::Yaw-Pitch` from `(-1,-1)` to `(1,1)`, with a default of `(0,0)`?
- Is `Face::Roll` `-1..1`, with a default of `0`?
- Is `Body::Yaw-Pitch` from `(-1,-1)` to `(1,1)`, with a default of `(0,0)`?
- Is `Body::Roll` `-1..1`, with a default of `0`?
- Are `Eye::L::Blink` / `Eye::R::Blink` `0..1` for X and `-1..1` for Y?
- Are `Eye::L::X-Y` / `Eye::R::X-Y` from `(-1,-1)` to `(1,1)`?
- Are `Eyebrow::L` / `Eyebrow::R` from `(-1,-1)` to `(1,1)`?
- Is `Mouth::Open` `0..1` for X and `-1..1` for Y?
- Does nothing break when you move the face, body, eye, eyebrow, and mouth parameters at the same time?
- Are the physics parameters mixed up with the expression parameters of the face and mouth?

## Where you are now

If you have done everything up to here, the basic inputs for expressions and orientation are organized.  
If you later build detailed expression variations or physics for hair and clothing, the basic parameters and the physics parameters stay separate, so adjustment is easier.
