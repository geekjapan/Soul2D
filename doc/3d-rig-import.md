# Rig import from glTF 2.0 and VRM (issue #243)

Status: **prototype of the first step of [ADR 0003](adr/0003-3d-conversion.md)**,
which was accepted on 2026-10-18. It has run on files the tests build, and on no real model.

MCP tool `rig_import_gltf`, Session operation `rig.import_gltf`. It adds the
19 standard bones below an empty DepthRigRoot at the rest positions of the
humanoid in a `.gltf`, `.glb` or `.vrm` file. The artwork, the bindings and
the weights are separate steps, as they are after `rig_add_skeleton`.

## Arguments

| Argument | Meaning |
| --- | --- |
| `root` | A DepthRigRoot without bones. Required. |
| `path` | A `.gltf`, `.glb` or `.vrm` file. Required. |
| `unitsPerSourceUnit` | Canvas units for one metre of the file. Finite and positive. |
| `lossy` | `true` accepts that the import approximates and drops what the report lists. |
| `acknowledgeRights` | `true` accepts the terms the file states. Only the user may decide it. |
| `dryRun` | `true` answers the report and writes nothing. |

The answer is the loss report of
[3d-conversion-loss-map.md](3d-conversion-loss-map.md). `needs` names each of
`unitsPerSourceUnit`, `lossy` and `acknowledgeRights` that the request has to
give and did not. While it names one, `applied` is `false` and nothing is
written, so a first call with `root` and `path` alone shows what the file
holds and what it asks for. A dry run that gives every argument goes through
the checks the import makes before it commits. After a
write the answer also holds `unitsPerSourceUnit` and `bones` (the list of
`rig_list_bones`), and the resource `resource://soul2d/rig/import-report`
returns the same answer until the next import that writes, or until another
document is created or opened. The write is one undo step.

## What is read

Only the JSON of the file: the text of a `.gltf`, or the first chunk of a
binary file. The binary chunk, buffers, images and external URIs are never
opened, so a `.gltf` needs none of its side files. From the JSON the import
reads the node transforms, the VRM humanoid table and the lists the report
counts.

The kind of file follows its content
([3d-conversion-conventions.md](3d-conversion-conventions.md)): the `VRM`
extension is VRM 0.x, `VRMC_vrm` is VRM 1.0, both are refused, neither is
plain glTF.

## Bones

- A VRM names the node of each humanoid bone in its extension. A plain glTF
  has no such table, so its nodes are matched by Mixamo-style names after a
  namespace that ends in `:` is removed. A node that draws a mesh is not
  matched. In a file whose `asset.generator`
  starts with `Soul2D ` they are also matched by the names of the standard
  bones, as `export_gltf` writes them. In a file of another writer such a
  name is not read: its `.L` may be the left of the character. The table is
  `gltf_rig::HUMANOID`, and a test compares it with the known answers of the
  conventions note.
- The rest head of a bone is the position of its node, composed from the
  node tree (`matrix`, or translation, rotation and scale), with the signs of
  the convention and times `unitsPerSourceUnit`. The origin of the file is
  the origin of the DepthRigRoot. Move the root to place the rig.
- A bone ends where the next bone of its chain starts. The ends of a chain
  follow the standard proportion, as the conventions note says.
- VRM 1.0 and plain glTF may leave out the chest, the neck and the shoulders.
  VRM 0.x may leave out the shoulders. Such a bone is placed between its
  neighbours and reported as `approximated`. Any other missing bone refuses
  the file.
- A VRM humanoid bone without a table row (toes, eyes, jaw, fingers,
  `upperChest`) is `kept`: its name and scaled position go to the `extdata`
  entry `io.github.geekjapan.soul2d.VrmHumanoid`. The entry describes the
  last import, so a later import that keeps such bones replaces it and lists
  that as `dropped`. A skin joint that is no humanoid bone is `dropped` and
  listed.
- Head keeps `allowParentToTargets` off and the feet keep `lockToRoot` on, as
  in the skeleton `rig_add_skeleton` makes.

## Rights

- VRM 1.0: the import is refused unless `meta.modification` is
  `allowModification` or `allowModificationRedistribution`. An absent field
  means prohibited, as in the schema, and so does any other value.
- VRM 0.x: the import is refused for the licence names
  `Redistribution_Prohibited`, `CC_BY_ND` and `CC_BY_NC_ND`.
- Every other VRM needs `acknowledgeRights: true`, and so does a plain glTF
  with an `asset.copyright` line. The report shows the fields under `rights`:
  at most 64 fields, names cut to 64 characters, text cut to 1,024
  characters, a list cut to 16 texts, control characters replaced, a number
  only up to 32 characters, and no value of another kind than text, a number,
  a flag or a list of texts. When that view is not all the file states,
  `truncated` names `rights`. A line break in a text is enough for that.
- The `meta` object is kept unchanged under
  `io.github.geekjapan.soul2d.VrmMeta`, or the `asset` object under
  `io.github.geekjapan.soul2d.GltfAsset`. Data an earlier import kept is not
  replaced: a file that states other terms is refused.
- Puppet metadata is not changed.

## Limits and refusals

- 16 MiB of JSON, the size the other JSON readers of the session allow. A
  binary file may be larger, because nothing after its JSON chunk is read.
- 65,536 nodes. The node graph must be a forest: a child index that does not
  exist, a node with two parents and a cycle are refused.
- Every transform number must be finite, a rotation must have a length, and a
  composed transform that leaves the finite range is refused. So is a bone
  that a file or a scale puts outside it or leaves without length. A scaled
  position may use half the range, so that the offset between two bones is
  finite.
- 256 humanoid bones without a table row, each with a name of its own in the
  first 64 characters. 256 listed items for one feature; the counts are not
  cut.
- A file that lists a required extension is refused unless the name is `VRM`
  or starts with `VRMC_`, `KHR_materials_` or `KHR_texture_`. These families
  cannot move a node. The list is narrow on purpose and grows when a real
  file needs it.
- INX documents only, because the rights data lives in the extension section.

## Fixtures (issue #242)

`rust/tests/gltf_rig.rs` builds every file it reads from one table of rest
positions: VRM 1.0 and VRM 0.x as binary files and a plain glTF as text. The
root of the node tree has a matrix that turns, scales and moves, and the
hips node is turned again, so a reader has to compose transforms. No model file is in the repository, and none was read.

## Not done

- Fitting a skeleton that already exists, and with it the default scale from
  the Pelvis-to-Head span.
- Meshes, weights, morphs, expressions, spring bones and materials (#244).
- PMX and PMD.
- A bone table in the request, for a plain glTF whose nodes have other names.
- A report of the rig nodes an INP export strips
  ([3d-conversion-loss-map.md](3d-conversion-loss-map.md), "Persistence").
- A test on a document that is not INX. The refusal is one line and no test
  builds such a document for this tool.
- A check on a real VRM or Mixamo file. The table rows and the schema fields
  come from the specifications, and the Mixamo-style names from common
  practice.
- An editor command. Per #99 the tool, the resource and the Skill entry come
  first.
