# 3D conversion: coordinate systems, scale and humanoid bone tables

Status: **design note**. The converters that follow it are
`rig_import_gltf` ([3d-rig-import.md](3d-rig-import.md)) and `export_gltf`
([3d-layered-export.md](3d-layered-export.md)). It belongs to
[ADR 0003](adr/0003-3d-conversion.md) and answers issue #240. What converts and
what is lost is in [3d-conversion-loss-map.md](3d-conversion-loss-map.md).

The known answers are in `rust/tests/fixtures/conversion3d/conventions.json`,
and `rust/src/conversion3d_tests.rs` checks them. A converter must give those
answers. Where this note and the fixture disagree, the fixture is right and
this note has a defect.

## The rig frame

The frame of a DepthRigRoot is the one [mmd-motion.md](mmd-motion.md) already
uses: x points to the right of the screen, y points down, z points toward the
viewer. It is left-handed. Lengths are canvas units.

## Source conventions

| Name | Handedness | Up | The model faces | Unit | Signs to the rig (x, y, z) |
| --- | --- | --- | --- | --- | --- |
| `gltf2` | right | +Y | +Z | metre | (1, -1, 1) |
| `vrm1` | right | +Y | +Z | metre | (1, -1, 1) |
| `vrm0` | right | +Y | -Z | metre | (-1, -1, -1) |
| `mmd` | left | +Y | -Z | MMD unit | (1, -1, -1) |

Where each row comes from:

- `gltf2`: the glTF 2.0 specification states a right-handed system with +Y up,
  the front of an asset facing +Z, metres and radians
  ([Specification.adoc](https://raw.githubusercontent.com/KhronosGroup/glTF/main/specification/2.0/Specification.adoc), read on 2026-10-14).
- `vrm1`: VRM 1.0 follows glTF. The
  [VRMC_vrm-1.0 README](https://raw.githubusercontent.com/vrm-c/vrm-specification/master/specification/VRMC_vrm-1.0/README.md)
  does not state the facing direction in the part that was read. The VRM
  Consortium's [list of changes from 0.x](https://vrm.dev/en/vrm1/changed/)
  lists the change from -Z forward to +Z forward.
- `vrm0`: the
  [VRM 0.0 README](https://raw.githubusercontent.com/vrm-c/vrm-specification/master/specification/0.0/README.md)
  states a right-handed system with +Y up, metres, and a model that faces -Z.
- `mmd`: no official specification of the MMD model formats was found. The row
  is the convention that `rig_apply_mmd_pose` already uses
  ([mmd-motion.md](mmd-motion.md), "Coordinates"), and the test checks that the
  row equals `mmd_motion::mmd_position_to_rig`.

A file says which convention it uses by its content, not by its name. A glTF
file with the `VRM` extension is `vrm0`, one with `VRMC_vrm` is `vrm1`, one
with both is refused, and one with neither is `gltf2`.

## Points and rotations

Let s be the sign triple of the convention and d the product of its three
signs.

- A point (x, y, z) becomes (s.x x, s.y y, s.z z) times the scale below.
- A rotation R becomes S R S with S the diagonal matrix of s. For a unit
  quaternion (x, y, z, w) that is (d s.x x, d s.y y, d s.z z, w).
- When d is -1 the handedness changes, and a cross product computed from
  coordinates changes its sign against the geometry. `gltf2`, `vrm1` and
  `vrm0` have d = -1. `mmd` has d = +1.
- glTF defines the front face of a triangle as counter-clockwise under a
  transform with a positive determinant and clockwise under a negative one.
  A reader therefore decides front and back faces in the source frame before
  it converts. A writer emits every triangle counter-clockwise as seen from
  +Z. A Soul2D mesh has no front face of its own.

The fixture holds points, quaternions and one triangle for each row. The test
checks that rotating a point and then converting gives the same result as
converting both and then rotating, and that the cross product of the converted
triangle has the sign the rule gives.

A rest orientation is not copied. A DepthBone has its own rest frame that
points from head to tail, so an imported bone keeps its rest position and gets
the rig's orientation. The tail of an imported bone is the head of the next
bone of the standard chain. A bone that ends a chain continues the bone
before it with the proportion of the standard skeleton (a Head is 10/6 of the
Neck, a Hand 0.377 of the Forearm), and a Foot points toward the viewer with
0.4 of the Shin. A child bone of the source, such as the toes, is not used
for it. Poses are converted into that frame the way
`rig_apply_mmd_pose` does it ([mmd-motion.md](mmd-motion.md), "Hierarchy
compensation").

## Scale

The source unit is not a canvas unit, and no file states the ratio. The scale
is one number, canvas units for one source unit. The plan is that an
import fits the standard skeleton of the document when it has one, which
moves its bones to the scaled rest positions of the source, and creates one
otherwise. `rig_import_gltf` only creates: it refuses a DepthRigRoot that has
bones, so rule 2 does not apply to it yet.

1. `unitsPerSourceUnit` in the request (finite and positive) decides.
2. Without it, when the document has a skeleton with a Pelvis and a Head, the
   scale is the rig's Pelvis-to-Head rest distance divided by the source's
   hips-to-head distance. This has the shape of the `translationScale` default
   in [mmd-motion.md](mmd-motion.md), with a measured span in place of the
   constant there.
3. Without either, nothing is written and the answer names
   `unitsPerSourceUnit` under `needs`. An import that creates the skeleton
   therefore needs it. It does not guess from the canvas size or from puppet
   metadata. `pixelsPerMeter` is a physics property and is
   not used as a scale.

An export takes the same number as `unitsPerMetre` and divides by it. The answer names the scale it used. The fixture holds one worked example. MMD units have no fixed length. The
8 cm figure in `mmd-motion.md` is an assumption there and stays one here.

## Left and right

A humanoid format names sides from the character's point of view. The standard
skeleton names them from the picture's point of view. A character that faces
the viewer has its left hand on the right of the picture, so every source
"left" bone maps to a `.R` bone and every "right" bone to a `.L` bone. The
existing MMD table follows the same rule (左腕 drives `UpperArm.R`). The test
checks the rule on both tables of the fixture and on every 左 and 右 row of
`mmd_motion::BONE_TABLE`. `mmd-motion.md` calls the rule a design question, so
ADR 0003 settled it as decision point 8 (accepted 2026-10-18).

## Bone tables

The canonical humanoid for the first step is the 19-bone standard skeleton
(`skeleton::STANDARD_BONE_NAMES`). ADR 0003 records that choice.

| Standard bone | VRM 0.x and 1.0 | Mixamo-style FBX | MMD |
| --- | --- | --- | --- |
| `Pelvis` | `hips` | `Hips` | see `mmd-motion.md` |
| `Spine` | `spine` | `Spine` | |
| `Chest` | `chest` | `Spine2` | |
| `Neck` | `neck` | `Neck` | |
| `Head` | `head` | `Head` | |
| `Clavicle.R` | `leftShoulder` | `LeftShoulder` | |
| `UpperArm.R` | `leftUpperArm` | `LeftArm` | |
| `Forearm.R` | `leftLowerArm` | `LeftForeArm` | |
| `Hand.R` | `leftHand` | `LeftHand` | |
| `Thigh.R` | `leftUpperLeg` | `LeftUpLeg` | |
| `Shin.R` | `leftLowerLeg` | `LeftLeg` | |
| `Foot.R` | `leftFoot` | `LeftFoot` | |

The six `.L` arm and leg rows and `Clavicle.L` are the same rows with `right`
and `Right`. The fixture lists all 19 rows for both tables.

- **VRM.** The humanoid object of the extension names the node of each bone,
  so the node's own name is not used. The 19 names above exist in 0.x and in
  1.0. VRM 1.0 requires 15 humanoid bones and does not require `chest`, `neck`
  or the shoulders. VRM 0.x requires 17 and does not require the shoulders.
  Both define 55 names.
- **Mixamo-style FBX.** An FBX file has no humanoid table, so bones are matched
  by name after a namespace prefix that ends in `:` is removed. This naming is
  common practice, not a specification, and no FBX file was read for this
  note. The plan is that a request may give its own table in the form of
  `boneTable` in [mmd-motion.md](mmd-motion.md). `rig_import_gltf` does not
  read one yet.
- **MMD.** The table is `mmd_motion::BONE_TABLE`, documented under "Name
  table" in [mmd-motion.md](mmd-motion.md). It is not repeated here.

### Bones the table does not cover

- A source bone without a row (toes, eyes, jaw, fingers, `upperChest`, twist
  and helper bones) does not become a standard bone. Its children attach to
  the nearest ancestor that has a row. The loss report lists it
  ([3d-conversion-loss-map.md](3d-conversion-loss-map.md)).
- A standard bone the source does not have (`Chest`, `Neck` or a `Clavicle`
  in a file that leaves the optional bone out) is placed between its two
  neighbours and reported as `approximated`: the Chest between the Spine and
  the Neck (or the Head), the Neck between the Chest and the Head, a Clavicle
  between the Neck and its UpperArm. `skeleton::fit_human` refuses a rig
  without one of the 19 names, so the bone cannot be left out.
- A file that lacks a bone its own format requires is refused.
