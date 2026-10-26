# ADR 0003: Convert between 3D character formats and Soul2D through glTF 2.0, a rig first

- **Status:** Accepted on 2026-10-18 by the maintainer, as proposed on 2026-10-14 (issue #239). Decision points 1-3 and 5-9 are accepted as written; for point 9 the projected mesh is the mesh import. For point 4, no `bpy` helper script ships, so no licence is chosen for one, and the Blender steps are documented for the user. The rig import of the first step, the layered export of the second and the mesh import of decision point 9 exist as prototypes ([3d-rig-import.md](../3d-rig-import.md), [3d-layered-export.md](../3d-layered-export.md), [3d-mesh-import-prototype.md](../3d-mesh-import-prototype.md))
- **Date:** 2026-10-14
- **Context:** Issue #102 asked how to convert between 3D character formats (VRM 0.x and 1.0, MMD `.pmx` and `.pmd`, FBX, glTF 2.0, Blender `.blend`) and Soul2D documents. A Soul2D document is a stack of textured 2D meshes with deformers and parameters, plus a 3D skeleton of DepthBone nodes that skins vertices by depth and then drops Z ([bone.md](../bone.md)). A 3D model is a skinned 3D mesh with materials, morph targets and physics, seen from any angle. The conversion loses content in both directions, and [format-conversion-map.md](../format-conversion-map.md) forbids silent loss. #102 was split, and this record is #239. The tables it relies on are [3d-conversion-conventions.md](../3d-conversion-conventions.md) (#240) and [3d-conversion-loss-map.md](../3d-conversion-loss-map.md) (#241)

## Decision

1. **Order.** The first step imports a humanoid rig from glTF 2.0 and VRM onto DepthBones (#243). The second exports a document as layered planes in glTF (#245). A mesh import is prototyped on one synthetic model (#244) before the project chooses between a projected mesh and rendered layers.
2. **One interchange format.** Soul2D reads and writes glTF 2.0 (`.gltf` and `.glb`) with the VRM extensions natively. Every other format reaches Soul2D as glTF, or through its own native reader where one is listed below.
3. **No new dependency for glTF.** The reader and the writer use `serde_json`, `base64` and `image`, which the crate already pins.
4. **Blender is an external process**, never a library, and it is the only route for FBX and `.blend`.
5. **Loss is reported, never silent.** Every conversion answers the loss report of the loss map, and a lossy result needs `lossy: true`. Every 3D conversion needs it today, because each direction has a row that is approximated for every file.
6. **The 19-bone standard skeleton stays the canonical humanoid** for these steps.

## What "converted" means

"Converted" means the target holds an editable equivalent. The loss map defines the other outcomes (`approximated`, `kept`, `dropped`, `refused`) and gives the row for every feature. In short:

| Content | 3D to Soul2D, first step | 3D to Soul2D, mesh import | Soul2D to glTF |
| --- | --- | --- | --- |
| Rig | humanoid bones with a table row become DepthBones | further bones up to 64 | DepthBones become joints |
| Mesh | not imported, listed | projected from the front with depth (prototype, decision point 9) | one mesh for each Part |
| Textures | not imported, listed | base colour only | embedded PNG |
| Morphs | not imported, listed | one parameter for each target | one target for each key of a one-axis parameter |
| Physics | kept in `extdata` or listed | a pendulum for each chain, lossy mode only | not written |
| Metadata and rights | never dropped, see "Rights" in the loss map | same | never dropped, see "Rights" in the loss map |

## Formats and routes

| Format | Read | Write | Route |
| --- | --- | --- | --- |
| glTF 2.0 `.gltf`, `.glb` | first step | second step | native |
| VRM 1.0 (`VRMC_vrm`) | first step | after the rights fields are complete | native, as glTF with extensions |
| VRM 0.x (`VRM`) | first step | not planned | native, as glTF with an extension |
| MMD `.pmx`, `.pmd` | later | not planned | native reader, written the way `psd.rs`, `kra.rs` and `caff.rs` are |
| FBX | later | later | Blender exchanges glTF with Soul2D |
| Blender `.blend` | later | later | Blender exchanges glTF with Soul2D |

- The glTF 2.0 specification is public. Its coordinate and unit rules are in the conventions note.
- The VRM extensions are specified in the `vrm-c/vrm-specification` repository. Version 1.0 uses `VRMC_vrm`, `VRMC_springBone`, `VRMC_materials_mtoon` and `VRMC_node_constraint`. Version 0.x uses one extension named `VRM`.
- **No official specification of PMX or PMD was found.** A native reader would rest on community notes and on the files themselves. That is a weaker base than the other formats have, so the MMD reader comes after the glTF steps, starts as a rig-only reader, and refuses what it does not recognise. `rust/src/mmd_motion/` already reads `.vmd` and `.vpd` on the same footing.
- A VRM file is written only when the humanoid has every bone the format requires and the rights fields are complete. A plain glTF export does not claim to be a VRM.

## Dependencies and licences

Every crate is pinned with `=` and builds run offline, so each new crate is a decision of its own. This record adds none.

- **`gltf` crate.** Not used. The parts Soul2D needs are a JSON document, base64 data URIs, a binary chunk and images. The pinned crates cover the first three and PNG. The `image` build has no JPEG feature, so JPEG goes through the in-tree decoder `jpeg.rs`. Any other image format is listed as not read. A typed crate would also not carry the VRM extensions.
- **`ufbx`.** Its licence file offers MIT or the Unlicense ([LICENSE](https://raw.githubusercontent.com/ufbx/ufbx/master/LICENSE), read on 2026-10-14). It is C, and this crate has no C build step. Not used. Revisit only if FBX must work without Blender.
- **Assimp** and similar importers. Not used. One large native library conflicts with the small pinned set.
- **Autodesk FBX SDK.** Out. It is proprietary, and its redistribution terms were not confirmed.
- **Blender.** Its source is under GPL version 2 or later and its binaries under GPL version 3 or later ([blender.org licence page](https://www.blender.org/about/license/), read on 2026-10-14). Soul2D does not link it, bundle it or download it. The user installs it.
- **Blender add-ons.** MMD Tools states GPL version 3 ([README](https://raw.githubusercontent.com/MMD-Blender/blender_mmd_tools/main/README.md), read on 2026-10-14). The licence of the VRM add-on was not confirmed for this record. Add-ons run only inside the user's Blender process. No add-on source enters this BSD-2-Clause tree.
- **A helper script that imports `bpy`.** The Blender licence page says that scripts using its Python API must be published under a GPL-compliant licence. The proposal is one helper script kept as a separate file, with its own licence header, never compiled or embedded into a Soul2D binary, and listed in `THIRD_PARTY.md`. Which licence the header names is the maintainer's decision. This record is not legal advice.

## Input safety

A 3D file is untrusted input, like every other imported file.

- **Bounds.** A reader checks every count and length against the file size before it allocates. The result stays within the limits the document already has, which are 64 bones for one rig, 16 influences for one vertex and 128 sources for one binding (`bone_deform.rs`), and 65,535 vertices for one Part mesh (`mesh_edit.rs`). The loss map says what happens to content beyond them. Limits for the JSON size, the node count, the buffer total and the image dimensions are set by the first prototype (#243) and recorded in its note. A file over a parser limit is refused, not truncated. A parent graph of nodes, joints or bones is checked before it is walked: no cycle, no node with two parents, and a bounded depth.
- **Numbers.** Every float that reaches the document is finite. Accessor ranges, buffer view ranges and indices are checked before use.
- **External resources.** A glTF URI is either a data URI or a relative path. The specification allows a client to support more, and Soul2D does not. A relative path is resolved against the directory of the glTF file and refused when it has a scheme, an authority, an absolute path, a `..` segment, a backslash or a drive letter, or when it resolves outside that directory, also through a symbolic link. The checks run on the percent-decoded path. Nothing is fetched from a network. A data URI counts toward the buffer total.
- **Required extensions.** A file that lists an extension in `extensionsRequired` that Soul2D does not read is refused.
- **Text.** PMD names are decoded with the existing Shift-JIS decoder (`rust/src/mmd_motion/sjis.rs`), PMX text by the encoding its header names. A byte sequence that does not decode becomes U+FFFD and the raw bytes are kept beside the text for reports, as the motion reader does it. Names echoed in an answer are shortened and have control characters replaced.
- **Blender.** It is started with an argument vector, never through a shell, the way `export.rs` starts ffmpeg. The arguments are `--background`, `--disable-autoexec`, `--python` with the helper script, then `--` and the request. Automatic script execution is never enabled, so a `.blend` file cannot run its own scripts. The Blender path is a host setting, as the encoder path of the video export is (`export_ops.rs`). No request, document or model file can name a program. The whole run has a deadline, and the process is killed and reaped when it passes. `export.rs` does the same for the encoder after the last frame. Soul2D reads back only the glTF file it asked for, through the same bounded reader as any other glTF. Blender itself parses the FBX or `.blend` file without a sandbox and with the user's own add-ons. Soul2D cannot bound that, and the user documentation of this route must say so.
- **Rights.** Permission data is input that must survive. The loss map says when a conversion is refused and when it needs `acknowledgeRights: true`.
- **One undo step.** A conversion changes the document in one undo step or not at all, and writes its output with the existing atomic write.

## Skeleton

The 19-bone standard skeleton stays canonical. The rig tools, the standard parameters and the MMD pose and motion tools address its names, and the VRM humanoid has a bone for each of them. Humanoid bones without a row are kept in `extdata` with their rest positions and are not lost. Growing the skeleton toward the 55 VRM names is a separate change that this record does not make. The trigger for it is a mesh import that needs finger or toe weights.

## Surfaces

Per #99, each step gets an MCP tool, a resource and a Skill entry before its GUI. The loss report is the result of the tool and is readable afterwards as a resource. Tool names are chosen in the prototypes.

## Decision points (resolved 2026-10-18, see Status)

1. Accept the order (rig import, then layered export, then the mesh prototype), or name another first direction.
2. Accept glTF 2.0 with the VRM extensions as the only natively written format, with no `gltf` crate.
3. Accept that PMX and PMD readers rest on community notes, or drop native MMD model reading and send MMD models through Blender.
4. Decide the licence of the `bpy` helper script, or decide that Soul2D ships no such script and documents the Blender steps for the user.
5. Accept the lossy mode for 3D conversion only. `format-conversion-map.md` says no lossy mode is authorised for the Cubism goal, and this record does not change that.
6. Accept the rights rule of the loss map, in particular that a file whose terms prohibit modification is refused and that every other file needs an explicit acknowledgement.
7. Keep the 19-bone skeleton canonical for now.
8. Accept the side rule for every format: a source "left" bone maps to a `.R` bone, because the character's left is the right of the picture. `mmd-motion.md` leaves that question open for the MMD table.
9. Choose between a projected mesh and rendered layers for the mesh import. This point is new on 2026-10-14 and is a proposal, not a decision.
   - **What was built.** A projected mesh, as `mesh_import_gltf` (#244). [3d-mesh-import-prototype.md](../3d-mesh-import-prototype.md) has the measured result on one synthetic model and a table of the two ways side by side.
   - **What it showed.** At rest and under a pose in the picture plane the picture has the pixel counts a view from the front must have. Depth and skin weights reach the DepthBones in the units of the file. After half a turn about Y the picture is wrong: it is the mirror of the rest picture, where a 3D view shows the back.
   - **Proposal.** Take a projected mesh as the mesh import. It needs no 3D renderer and no dependency, which rendered layers do, and it keeps the texture and the skin weights of the file as data. Rendered layers would add the shading of the file and nothing for a turn, because a picture has no back either. They stay possible later through the Blender route.
   - **Open before acceptance.** No file of another exporter was read, so the count of Parts and the fit of real skins to the 19 bones are unknown.

## Not checked

- No model file was read for this record. Fixtures are the subject of #242 and are generated by a script.
- The licence of the VRM specification text and of the VRM add-on for Blender.
- The redistribution terms of the Autodesk FBX SDK.
- MMD model format details. They come from community notes, not from a specification.
- `.blend` and FBX behaviour in Blender. No Blender process was run.
