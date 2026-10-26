# Layered export to glTF 2.0 (issue #245)

Status: **prototype of the second step of [ADR 0003](adr/0003-3d-conversion.md)**,
which was accepted on 2026-10-18. Its files were checked by the tests of this repository and
read back by `rig_import_gltf`. **No glTF validator was run on them**, and no
other program has opened one.

MCP tool `export_gltf`, Session operation `export.gltf`. It writes the open
INX document as layered planes: one mesh for each drawn Part, in draw order
along Z. The session is not changed.

## Arguments

| Argument | Meaning |
| --- | --- |
| `path` | The file to write. It ends in `.glb` (binary) or `.gltf` (one file with the buffer inside). Required. |
| `unitsPerMetre` | Canvas units for one metre of the file. Finite and positive. |
| `lossy` | `true` accepts that the export approximates and drops what the report lists. |
| `acknowledgeRights` | `true` accepts the terms kept with the document. Only the user may decide it. |
| `overwrite` | `true` replaces an existing file. |
| `dryRun` | `true` answers the report and writes nothing. |

The answer is the loss report of
[3d-conversion-loss-map.md](3d-conversion-loss-map.md) with `direction`
`soul2d-to-gltf` and `lossy` as the request gave it. `needs` names each of
`unitsPerMetre`, `lossy` and
`acknowledgeRights` that the request has to give and did not. While it names
one, `applied` is `false` and nothing is written. Without `unitsPerMetre`
the report is the one of a file at one canvas unit a metre. After a write the
answer also holds `path`, `bytes` and `unitsPerMetre`, and the resource
`resource://soul2d/export/gltf-report` returns the same answer until the next
export that writes, or until another document is created or opened.

## What is written

- **Parts.** Every Part the renderer draws at the default value of every
  parameter, with its vertices where the deformers above it put them. A Part
  that is disabled, has no texture or has no triangle is `dropped` and listed.
- **Axes.** x stays, y is turned (the canvas has y down, glTF has y up), and
  lengths are divided by `unitsPerMetre`
  ([3d-conversion-conventions.md](3d-conversion-conventions.md)). Every
  triangle is written counter-clockwise as seen from +Z.
- **Draw order.** The written Part drawn first is at z 0, and each later
  written Part is one canvas unit nearer to the viewer. Depth values are not
  used.
- **Textures.** One embedded PNG for each texture a written Part uses, shared
  by the Parts that use it. Soul2D stores colour multiplied by alpha and glTF
  does not, so colour is divided by alpha, which loses precision at low alpha
  (`approximated`). A fully transparent texel keeps the colour it is stored
  with.
- **Materials.** One for each Part: unlit (`KHR_materials_unlit`), alpha
  blended, double sided. The tint and the opacity are the base colour factor,
  each cut to 0..1. glTF multiplies the decoded (linear) texel where Soul2D
  multiplies the encoded one, so the tint is decoded with the sRGB transfer
  function. That matches for a white texel and for a tint of 0 or 1, and is
  near elsewhere (`approximated`). A screen tint, a blend mode other than Normal and a mask are
  `dropped` and listed.
- **Parameters.** A key of a one-axis parameter that is not its default is a
  morph target of every Part it moves, named `<parameter>=<value>` in
  `extras.targetNames` of the mesh. The parameter name is cut to 32 characters
  and the whole to 64. A reader blends
  targets linearly, which is
  not how Soul2D interpolates between keys (`approximated`). A two-axis
  parameter is `dropped` and listed. At most 64 keys are evaluated.
- **Bones.** Every DepthRigRoot with bones is a node, and each DepthBone a
  node below it at its rest head, named by its `boneId`, or by its name when
  it has none. Parts are written as the default parameter values pose them,
  and bones at rest whatever a parameter does to them. The tail, the roll, the
  constraints and the orientation of a bone are not written. Nothing is
  skinned to the bones: see "Not done".
- **Metadata.** The name, artist, rigger, contact, licence URL, copyright and
  reference of the puppet go to `asset.extras` under
  `io.github.geekjapan.soul2d.PuppetMeta`, and the copyright text to
  `asset.copyright`.
- **Rights.** Rights data an import kept (`VrmMeta`, `GltfAsset`) is written
  unchanged to `asset.extras` under its `extdata` key and shown under `rights`
  with the bounds of [3d-rig-import.md](3d-rig-import.md). The copyright line
  of a kept glTF asset is the copyright line of the file. Such a document
  needs `acknowledgeRights: true`. Any other `extdata` entry is `dropped` and
  listed. The file is a plain glTF and does not claim to be a VRM.

A file this tool wrote reads back through `rig_import_gltf`, which matches
the standard bone names in a plain glTF whose `asset.generator` starts with
`Soul2D ` and matches no node that draws a mesh, so a Part may carry the
name of a standard bone. A rig root, or a bone without a standard id, that carries
such a name is read as that bone, and the import refuses the file when the
name is then stated twice.

## Limits and refusals

- INX documents only, and not while a deform refresh is pending.
- 256 MiB for the buffer. A `.gltf` holds it as base64, a third larger.
- The scene limits of the renderer apply (nodes, depth, Composite nesting),
  because the export reads the Parts the way the renderer does. A texture is
  decoded with the size limits of the renderer.
- A Part with more than 65535 vertices is refused. Indices are 16 bits wide.
- Two `extdata` entries of rights data under one key are refused.
- A scale that puts a point outside the range of a 32-bit float is refused.
- A document that draws no textured Part is refused.
- An existing file is refused without `overwrite`, and so are the path of the
  open document and a path that ends in a separator. The file is written with the atomic write of the other
  exports.

## Not done

- **A glTF validator run.** Issue #245 asked that a validator accepts the
  file. On 2026-10-18 the maintainer decided that no validator is fetched and
  that the structure check below stands in for it. `rust/tests/gltf_export.rs`
  checks the structure a reader relies on: index ranges, buffer bounds and
  alignment, accessor bounds compared in f64, one parent for each node, the
  winding, the
  morph target counts, PNG images that decode, and no null or empty list.
- **Skin weights.** Weights live on GridDeformer and PathDeformer vertices,
  not on Parts. Resampling them onto Part vertices is not written, so each
  binding is `dropped` and listed, and no skin is written.
- Z from depth values, VRM output, animations, physics.
- An editor command. Per #99 the tool, the resource and the Skill entry come
  first.
