# Semantic conversion obligations

Status: **OPEN MAPPING; ONE LOSSY DIRECTION IMPLEMENTED**. The required directions
remain CMO3 + explicitly linked CAN3 → INX, INX → CMO3 + CAN3, and each format's
same-format edit/save/reopen. CMO3 → INX exists as the lossy mode described in
[CMO3 → INX, lossy mode](#cmo3--inx-lossy-mode); nothing else below is implemented. CAN3 is a project with scenes/tracks and external
resources, not an animation field to append blindly to CMO3.

For every feature in [the ledger](cubism-feature-ledger.md), record an explicit
mapping in both directions and an actual roundtrip probe. This table establishes
obligations and known representation gaps, not a promise of lossless support.
“Refuse” is the safe interim behavior, **not acceptance of the conversion goal**.
No opaque sidecar alone qualifies as semantic conversion: the target must remain
editable and produce the expected behavior after edits and reopening.

| Semantic feature | CMO3/CAN3 → INX obligation | INX → CMO3/CAN3 obligation | Present gate |
| --- | --- | --- | --- |
| Identity | Explicit GUID/attribute-suffix mapping to INX resource IDs | Stable inverse correspondence; no name-based binding | Binding metadata query exists; persistent conversion identity map unresolved |
| Hierarchy | Parts/deformers/meshes and nesting mapped without changing spaces | Reconstruct parent semantics, not merely matching labels | Native scene evaluation/rendering incomplete |
| Geometry | Topology, vertices, origins, transforms and UVs | Preserve drawable shape and editable topology | Coordinate correspondence and deformation mapping unverified |
| Texture assets | Preserve image content, dimensions, atlas/UV associations | Vendor-supported textures/atlas with correct UVs | Extraction is not atlas-authoring parity |
| Draw order | Preserve fractional/evaluated order and group semantics | Reconstruct order and grouped passes | No integer truncation or global-sort approximation |
| Masks | Normal/inverse masks and membership | Reconstruct masks at the correct pass boundary | Native/INX mask equivalence unverified |
| Offscreen | Nested part passes and keyed part colors/opacity | Reconstruct grouped offscreen parts | INX composite resemblance is not a proven mapping |
| Blend | Every COL/ALP obligation including legacy rules | Exact target mode and alpha behavior | Mode names/formulas must not be guessed |
| Parameters | Ranges, defaults, repeat, GUIDs, labels | Scalar/vector mapping with explicit identity | INX vector parameter may need multiple vendor scalar IDs |
| Keyforms | Coupled forms, interpolation, extended caches | Editable equivalent grids/curves and regenerated vendor caches | Linear bounded queries do not establish SNS/ellipse parity |
| Blend shapes | Additive deltas, normal baselines and constraints | Editable morph forms/constraints | No baking away editable morph semantics |
| Deformers | Warp/rotation hierarchy and nonlinear evaluation | Equivalent editable deformer graph | Grid/Path/Bone INX nodes have no approved universal mapping |
| Art paths/glue/skinning | Preserve authoring controls plus evaluated motion | Reconstruct corresponding controls and attachments | Dedicated mapping/probes missing |
| Clip time | Scene FPS, source frames, offsets, clip bounds | Explicit seconds/frame rational conversion | INX timestep is not automatically CAN3 FPS/offset semantics |
| Curves | Anchors, handles, interpolation modes and negative frames | Reconstruct curves without resampling away editable handles | Raw CAN3 key edits are not a curve converter |
| Multi-track project | Scenes, linked models, background/audio/group tracks | Reconstruct project and explicit asset links | INX animation map alone lacks a proven full-project representation |
| Form animation | Object-local animated geometry and exposure timing | Reconstruct editable FA tracks for each target object | Dedicated mapping missing |
| Controllers/fades/events | Controller targets, fade composition, events/bookmarks | Reconstruct project behavior and runtime export semantics | No silent dropping of editor-only structures |
| Physics/pose/MotionSync | Definitions, ordering, baking provenance | Vendor-equivalent configuration/evaluation | INX physics or Motion3 presence is not full vendor parity |
| Depth/bones/2.5D | Retain any representable incoming equivalent explicitly | Preserve INX-native depth/bone/constraint behavior | No approved lossless Cubism mapping; explicit refusal pending design |
| Unknown source fields | Retain opaque data and identify missing semantic support | Preserve extensions without presenting ignored data as implemented | Same-format byte preservation is necessary but insufficient |
| Editor state | Preserve required authoring workflow data where representable | Reconstruct editable workflow state, not screenshots | Separate UI workflow and persistence probes required |

## Per-direction acceptance sequence

For each original and dedicated probe: hash original → open/read → convert to a
new path → reopen converted output → compare semantic IDs and sampled behavior →
perform specified target-native edits → Undo/Redo → save → reopen → compare again
→ reverse conversion → open/save/export in the licensed Editor where applicable
→ legitimate Core/Editor render comparison → verify original hashes unchanged.
Keep intermediate files, loss/refusal diagnostics, resource reads and provenance.

Same-format edits must retain unedited unknown XML, container sections, images,
opaque INX tails and unrelated key/control metadata. Do not use full-file equality
to reject documented vendor normalization, but do retain those raw diffs and
compare exact canonical semantics under a reviewed allowlist. Soul2D's own
untouched-byte claims remain byte-for-byte requirements.

On unsupported semantics return a structured feature/owner/direction diagnostic
before publishing an output or changing the active document/history. Never label
partial conversion “success” without a separately approved lossy mode; no such
lossy mode is authorized by the full compatibility goal. Transactional rollback,
save conflict refusal, and original preservation are acceptance obligations.

## CMO3 → INX, lossy mode

Approved by the owner on 2026-10-20 (the standing instruction to follow the
recommendation that named this mapping), for this direction only. The owner was
told warp deformers would become MeshGroups; the implementation bakes every
deformer instead, because baking reproduces each single-parameter key exactly
with the evaluator the preview already has, and a MeshGroup mapping of Cubism
warp coordinates was unproven. `live2d.convert.inx {path?}` (`rust/src/native_convert.rs`):

- Nodes: the default-pose render model of the native scene, so Parts carry the
  canvas-space mesh, texture slot, draw order, blend mode, tints, opacity and
  masks the preview draws; offscreen Parts are Composites. Textures are the CMO3
  textures, premultiplied as every Soul2D INX stores them.
- Parameters: one one-axis INX parameter per native parameter, same range,
  default and repeat flag; axis points are 0, 1, the default and every key of
  every source. Each axis point holds a `deform` binding with the vertex offsets
  of the pose at that key, for every ArtMesh the key moves. A draw order or
  opacity that the key changes becomes a `zSort` or `opacity` binding of the
  same parameter: the evaluator adds zSort offsets to the node's draw order and
  multiplies opacity bindings into its opacity. A node keyed by one parameter
  gets tree opacity 1 and the absolute opacities as binding values; a node
  keyed by several gets the opacity ratios to its default, so those combined
  poses multiply.
- Baked, so no longer editable as such: warp and rotation deformers, Glue,
  blend shapes. Combined poses add their offsets in INX, where Cubism nests
  them.
- Not converted, and counted in the answer: physics, blend-shape constraints,
  CAN3 animations, and keyed opacities of a node that is drawn at opacity 0 at
  the default pose and keyed by several parameters (`opacityKeyChanges`).
- Linked CAN3 (`animation` names the file): every scene that drives the model
  becomes an INX animation named after the scene, `length` the scene's frames
  and `timestep` 1/fps, with one Linear lane per keyed parameter whose
  keyframes are the CAN3 player's values at every frame (Bezier handles baked,
  flat stretches collapsed to their end frames). A Part with opacity curves
  becomes a 0..1 parameter named after the Part, with one Forced Linear lane per
  scene and `opacity` bindings on every drawn node the Part fades. Soul2D's CAN3
  player replaces the Part's own opacity with the lane value, so the parameter
  default is the Part's opacity at the default pose and a node under the Part
  draws at value times its opacity with the Part at 1. Losses: a node whose
  Part opacity is also keyed by a parameter differs while the lane plays;
  several Part lanes multiply; a node drawn at opacity 0 at the default pose
  (its own form is 0, or another Part above it is at 0) is not faded by a Part
  lane; a Part that fades no drawn node stays counted in
  `notConverted.partOpacityLanes`, and so does a Part when a node it fades
  already has an opacity binding and is drawn at opacity 0 at the default pose.
  Between keys the INX draw order interpolates linearly where the CMO3 rounds
  to whole draw orders, so ties between meshes can resolve differently
  mid-key.
- The answer is the loss report; a conversion is never reported without it.

Measured on 2026-10-20 on the four local samples (`scene.geometry` of the INX
against the CMO3): the default pose and every single-parameter key are exact
(0.0 px over 86, 209, 195 and 215 poses). The twelve pseudo-random combined
poses of the Core capture differ by (worst / median) 37.8 / 13.0 px on
hiyori_free, 736 / 434 px on hiyori_pro, 3,062 / 2,617 px on ren_ja and
64.7 / 37.9 px on zundamon_ja: the cost of additive composition. The converted
INX takes a node edit, undo, save and reopen (`rust/tests/core.rs`,
`a_cmo3_converts_to_an_inx_whose_single_parameter_poses_match`). With the
samples' CAN3 files, every scene converted (8, 10, 8 and 23 scenes; 124 to 399
lanes; 8,570 to 20,850 keyframes) and the INX animation at every curve anchor
inside its scene matched the anchor value within 1.7e-6 (990, 1,582, 1,431 and
4,148 anchors; ren_ja has 354 anchors outside their track's span, which the
player never reaches). Measured again on 2026-10-21 with the opacity and zSort
bindings: the geometry, the combined poses and the CAN3 anchors give the same
numbers (CAN3 worst 1.7e-6). The conversions hold 4, 7, 19 and 146 opacity
bindings and 0, 4, 0 and 0 zSort bindings, and zundamon_ja counts 2 opacity key
changes as not converted. `render.packet` of the INX against the CMO3 at the
default pose and every single-parameter key shows the same opacities and the
same draw order on hiyori_free (1 pose changes an opacity), hiyori_pro (3) and
zundamon_ja (31), except one zundamon_ja drawable at 2 poses: the counted loss,
drawn at opacity 0 at the default pose and keyed by two parameters. ren_ja,
whose GPU packet refuses Cubism 5.3 blending, was compared through
`scene.geometry`, which lists each Part's evaluated opacity in painter order:
its 195 poses (10 change an opacity) show the same opacities and the same
painter order, and the other three samples give the same counts there, the
zundamon_ja loss included. The hiyori Part opacity curves, counted before, now
become 1 and 2 parameters with 8 and 20 lanes. No Cubism Editor or
nijigenerate opened the output; that acceptance stays NOT_RUN.

### Structural mapping, assessed on 2026-10-20 and not pursued

The recommendation the owner approved named warp deformers as MeshGroups. That
mapping, with rotation deformers as transform-bound Nodes, was measured against
the samples and the two evaluators before being built, and it cannot reproduce
Cubism's composition:

- A rotation deformer under a warp turns with the warp and keeps its size (the
  rule `a_rotation_under_a_warp_turns_with_the_warp_and_keeps_its_size` tests),
  while a MeshGroup moves every vertex of the Parts below it, whatever Node
  sits in between. 68 of 84, 123 of 140, 158 of 198 and 173 of 198 ArtMeshes of
  the samples have a rotation under a warp in their deformer chain.
- Every warp of ren_ja and zundamon_ja is a quad-transform (bilinear) warp; a
  MeshGroup is piecewise linear over triangles.
- ren_ja places 111 of its 198 ArtMeshes in offscreen or draw-order-group Parts
  whose deformers lie outside that Part, and INX has one tree for both.
- Sources keyed on two parameters could take a linked two-axis INX parameter
  (56, 58, 155 and 130 of them), but the combined-pose error comes from the
  deformer chain, whose parameters are many, so that alone does not lower it.

Baking therefore stays the mapping, with the combined-pose cost measured above.
An exact conversion of combined poses needs an INX node that evaluates Cubism
warps and rotation deformers, which nijigenerate would not read; that is a new
design decision, not a step of this issue.
