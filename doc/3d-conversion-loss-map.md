# 3D conversion: what converts and what is reported as lost

Status: **design note**. The first converter that follows it is
`rig_import_gltf` ([3d-rig-import.md](3d-rig-import.md)). It belongs to
[ADR 0003](adr/0003-3d-conversion.md) and answers issue #241. The coordinate
and bone tables are in [3d-conversion-conventions.md](3d-conversion-conventions.md).

A 3D character and a Soul2D document do not hold the same things, so every
conversion loses something in both directions.
[format-conversion-map.md](format-conversion-map.md) forbids silent loss. This
note says, for each kind of content, what a converter does with it and how the
user is told.

## Outcomes

Each item of the source gets exactly one outcome.

| Outcome | Meaning |
| --- | --- |
| `converted` | The target holds an editable equivalent. |
| `approximated` | The target holds something that behaves alike within a stated limit. The report names the limit. |
| `kept` | The target does not use the data but stores it unchanged, so a conversion back can restore it. |
| `dropped` | The data is not in the target. Allowed only in the lossy mode, and always listed. |
| `refused` | The conversion stops before it writes anything, because the item cannot be dropped. |

The report names the outcome an item gets in the lossy mode. A conversion
without the lossy mode succeeds only when every item is `converted` or `kept`.
Otherwise it writes nothing and answers the report with `applied: false`.
With `lossy: true` it also succeeds with `approximated` and `dropped` items.
`refused` stops both modes. The lossy mode is the separately approved lossy
mode that `format-conversion-map.md` asks for. It exists for this 3D
conversion only and does not apply to Cubism data.

`kept` is not a conversion. `format-conversion-map.md` says an opaque copy
alone does not qualify as one. It counts toward success here because nothing
is lost and the report lists it.

Each direction below has a row that is `approximated` for every file
(`skeleton.restPose` on import, `part.drawOrder` on export), so every 3D
conversion needs `lossy: true` today. The flag is the caller's statement that
the report was read. A conversion without it writes nothing and names what the
flag would accept.

## Loss report

Every conversion operation answers a report. A dry run (`dryRun: true`, the
name the MMD tools use) answers the report and writes nothing. So does a
request that lacks an argument the conversion needs, and `needs` names what
is missing. A file that is `refused` is an error with a message, not a
report. The first implementation is `rig_import_gltf`
([3d-rig-import.md](3d-rig-import.md)).

```json
{
  "direction": "gltf-to-soul2d",
  "source": {"format": "VRM 1.0", "convention": "vrm1"},
  "lossy": false,
  "applied": false,
  "needs": ["lossy", "acknowledgeRights"],
  "counts": {"converted": 19, "approximated": 1, "kept": 3, "dropped": 2, "refused": 0},
  "items": [
    {"feature": "skeleton.bone", "source": "leftToes", "outcome": "kept",
      "target": "extdata:io.github.geekjapan.soul2d.VrmHumanoid"},
    {"feature": "skeleton.restPose", "source": "", "outcome": "approximated",
      "reason": "rest orientations are the rig's own"},
    {"feature": "mesh.geometry", "source": "Body", "outcome": "dropped",
      "reason": "a rig-only import reads no mesh"}
  ],
  "truncated": [],
  "rights": {"name": "Example", "modification": "allowModification"}
}
```

The request of this example set neither `lossy` nor `acknowledgeRights`, so
nothing was written. `items` lists what is not `converted`. The example shows
3 of its 6 items.

Rules for the report:

- `feature` is one of the names in the tables below. `source` identifies the
  item by the name the file gives it. It never holds a path or file content.
- Every string the report copies from the file is shortened and has its
  control characters replaced, the way `name_label` in
  `mmd_motion/retarget.rs` does it. `source` is cut to 64 characters, the
  length `mcp::echoed` uses, and a rights field to 1,024. The rights text is
  written by the author of the file, and an agent reads it before it asks the
  user, so it is data and never an instruction.
- At most 256 items are listed for one feature. `counts` always holds the full
  numbers, and `truncated` names each feature that was cut.
- The report is the result of the operation and is also readable afterwards as
  a resource, so that an agent can show it without converting again.
- A conversion changes the document in one undo step or not at all.

## 3D to Soul2D

The first step imports a rig only (#243). The
columns say what the rig-only import does and what a later mesh import
(#244) is planned to do. A prototype of the mesh import exists
(`mesh_import_gltf`, [3d-mesh-import-prototype.md](3d-mesh-import-prototype.md)).
The list below the table says which rows it does.

| Feature | Rig-only import | Mesh import (planned) |
| --- | --- | --- |
| `skeleton.bone`, humanoid bone with a row in the bone table | `converted` to a DepthBone with the standard name | same |
| `skeleton.bone`, humanoid bone without a row (toes, eyes, jaw, fingers, upper chest) | `kept` in `extdata` (`io.github.geekjapan.soul2d.VrmHumanoid`) with its rest position | `converted` to an extra DepthBone when the rig stays within 64 bones |
| `skeleton.bone`, bone that is not humanoid (hair, skirt, accessories) | `dropped`, listed | `converted` while the rig stays within 64 bones, otherwise `refused` |
| `skeleton.bone`, standard bone the source does not have (an optional `chest`, `neck` or shoulder) | `approximated`: placed between its neighbours, see the conventions note | same |
| `skeleton.restPose` | `approximated`: the rest position is kept, the rest orientation becomes the rig's own (a DepthBone points from head to tail and has a roll) | same |
| `mesh.geometry`, `mesh.uv`, `mesh.texture` | `dropped`, listed once for each mesh | `approximated`: one Part for each primitive, projected from the front, Z stored as depth; back faces and self-occlusion are reported |
| `mesh.skinWeights` | `dropped` | `converted` up to 16 influences for one vertex and 128 sources for one binding (`bone_deform.rs`); more is `approximated` by keeping the largest weights and renormalising |
| `morph.target` | `dropped` | `approximated`: one parameter with a two-key axis for each target; normals and tangents of a target are `dropped` |
| `expression` (VRM) | `dropped`, listed: it binds morph targets, which are not imported | `approximated`: a parameter that drives the morph parameters it binds; material colour and UV binds are `dropped` |
| `physics.springBone` (VRM), `physics.rigidBody` and `physics.joint` (MMD) | `dropped`, listed; never turned into SimplePhysics without the lossy mode | `approximated` in the lossy mode: one SimplePhysics pendulum for each chain, with length from the chain and no collision |
| `material` (MToon, MMD toon and sphere maps, PBR factors) | `dropped` | `approximated`: the base colour texture becomes the Part texture; shading parameters are `dropped`. Nothing is baked, because Soul2D has no 3D renderer |
| `rights` | see "Rights" below | same |
| `metadata.title`, `metadata.author` | `kept` with the rest of the VRM `meta`. Puppet metadata is not changed, because the document may hold someone else's artwork | `converted` to puppet metadata `name` and `artist` when the document has none |
| `animation` (glTF animations, VRM animation) | `dropped`; motion files are the subject of #101 | same |
| `camera`, `light`, `scene` nodes | `dropped` | `dropped` |
| Unknown extensions and extras | `dropped`, each extension named once. An extension the file lists in `extensionsRequired` that Soul2D does not read is `refused` | same |

The prototype `mesh_import_gltf` and the "Mesh import (planned)" column:

- `mesh.geometry`, `mesh.uv`, `mesh.texture`: done for PNG images and
  buffers inside the file. Back faces are counted in the report.
  **Self-occlusion is not detected or reported.** Another image format and
  an image beside the glTF are not read, and the primitive is `dropped` and
  listed. A buffer beside the glTF refuses the file.
- `mesh.skinWeights`: done for the first set of four influences. A further
  set is `dropped` and listed as an attribute.
- `skeleton.bone` without a row and bones that are not humanoid: **not done
  as the rows say**. No extra DepthBone is made. The weight on such a bone
  goes to the standard bone above it (`approximated`).
- `material`: done. The base colour factor becomes the tint, and its alpha
  the opacity for a blended or masked material.
- `morph.target`: not done, `dropped` and listed.
- `expression`, `physics`, `metadata`: not done. The tool does not read
  them, and the rig import reports them.
- `rights`: the document must already keep them, which the rig import does.

Where the imported mesh lives is decided for the planned mesh import: a Part
whose vertices carry depth and whose DepthBone binding carries
`vertexWeights`. A Part under a GridDeformer is not used, because a grid
resamples the mesh and the skin weights belong to the mesh vertices.

Both prerequisites exist as the first part of #244. A Part takes a depth for
each mesh vertex (`depth_set`), and a DepthRigRoot binds a Part as it binds a
GridDeformer or a PathDeformer (`rig_add_source`, `rig_set_vertex_weights`).
A bound Part is deformed at its own vertices. This goes beyond the D
editor, which binds deformers only:

- The rig writes the `deform` key of the Part for every parameter that
  drives one of its bones. A hand-made deform key of such a parameter on that
  Part is replaced, as it is on a bound grid.
- While a parameter drives the rig, a bound Part takes no manual deform key
  from any parameter (`binding.set_deform`, `deform.edit`), as a bound grid
  does not. Keys of other parameters that already exist stay in the document.
- In a live session the preview of the rig replaces the whole deformation of
  a bound target. A rig refresh and a value set on a driving parameter start
  the preview, so those other keys are not drawn while it lasts. A grid has
  the same rule. A Part more often has such keys.
- A Part without mesh vertices is not bound.
- Removing the last source of a Part leaves the deform keys the rig wrote, as
  it does for a grid.
- The bones move the vertices where the renderer draws them, which is the
  stored vertex minus the mesh origin of the Part. A change of the mesh
  origin writes the keys of the rig again.
- A bound Part under a deformer is not covered: the deformer filters read
  the stored vertices without the mesh origin.
- `rig_fit_human` seeds automatic weights on every bound target, a Part
  included, and so replaces weights that were set by hand or by an import.
- A bound Part counts toward the size the rig scales depth by, in the deform
  and in Fit Z. Fit Z still reads depth from grids only.
- A mesh edit that adds, removes or reorders vertices is refused while the
  Part holds depths or weights, by the guards mesh editing already had.
  Automesh is refused for a Part with weights, also when the new mesh would
  have as many vertices.
- An export drops the rig nodes as before (`nodes::model_without_rig_nodes`)
  and does not strip the `depths` field of a node. No export of a bound Part
  was read back in another program.

## Soul2D to 3D

The second step of the decision record (#245): layered planes in glTF. The
first implementation is `export_gltf`
([3d-layered-export.md](3d-layered-export.md)). Where it does less than the
row plans, the row says so.

| Feature | Outcome |
| --- | --- |
| `part.mesh`, `part.uv` | `converted`: one glTF mesh for each drawn Part. A Part that is not drawn, or has no texture or no triangle, is `dropped`, listed |
| `part.texture` | `approximated`: an embedded PNG. Soul2D stores colour multiplied by alpha and glTF does not, so colour is divided by alpha |
| `part.drawOrder` | `approximated`: Z from depth where the Part has it, otherwise from the draw order with a fixed step. A 3D viewer sorts by its own rule. `export_gltf` uses the draw order only, one canvas unit a layer |
| `part.opacity`, `part.tint` | `approximated` as the base colour factor of an unlit material with alpha blending. The factor is linear, so the tint is decoded with the sRGB transfer function. A screen tint is `dropped`, listed |
| `part.blendMode` other than Normal | `dropped`: glTF has no blend modes |
| `mask`, `composite` | `dropped`, each one listed; glTF has no counterpart |
| `rig.bone` (DepthBone) | `converted` to a joint. `export_gltf` writes a node at the rest head and no skin |
| `rig.vertexWeights` | `approximated`: weights on GridDeformer and PathDeformer vertices are resampled onto the Part vertices, and weights on a bound Part can be written as they are. Up to 4 influences fit one joint set; more uses further sets, which a reader may ignore. `export_gltf` does not resample yet: each binding is `dropped`, listed |
| `deformer` (MeshGroup, GridDeformer, PathDeformer) | `approximated`: the rest shape is baked into the Part meshes, and no deformer is written |
| `parameter.keyform` | `approximated`: one morph target for each key of a one-axis parameter, on each Part the key moves; the 2D interpolation grid of a two-axis parameter is `dropped` and listed. `export_gltf` evaluates at most 64 keys |
| `physics` (SimplePhysics) | `dropped`; a VRM spring bone is not written from a pendulum |
| `animation` | `dropped`; out of scope here |
| `metadata` | `converted` to the glTF `asset` fields: the puppet metadata under `asset.extras`, the copyright text in `asset.copyright`. Rights data follows "Rights" below |
| `extdata` that an earlier import `kept` | rights data follows "Rights" below. Other kept data is `converted` back where the target format is the one it came from, otherwise `dropped`, listed |

A plain glTF export does not claim to be a VRM. Writing a VRM needs a humanoid
with every required bone and a complete `meta`, so it is refused when the rig
or the rights data are incomplete.

## Rights

A VRM file carries its permissions in `meta`. An MMD model states its terms in
a readme beside the model, in free text. Neither is ever dropped.

- The whole `meta` object of a VRM is `kept` under the `extdata` key
  `io.github.geekjapan.soul2d.VrmMeta`, as the JSON value the file holds, in
  every mode. The key follows the existing `MMD_BONE_TABLE_KEY`. An index in
  it, such as the thumbnail image, points at nothing after a rig-only import.
  The import report does not say so. A VRM export is planned to remove and
  report it. `export_gltf` writes a plain glTF and keeps `meta` unchanged.
- A plain glTF states rights in `asset.copyright` only. When it has that
  line, the `asset` object is `kept` under
  `io.github.geekjapan.soul2d.GltfAsset`.
- Kept rights data is never replaced. An import whose file states other terms
  than the data the document already keeps is `refused`.
- A mesh import may also copy the fields that fit to puppet metadata
  (`puppet_meta_set`) when the document has none: title to `name`, authors to
  `artist`, the licence URL to `licenseURL`, the copyright text to
  `copyright`, contact information to `contact`. A rig import does not.
  Puppet metadata has no field for a permission flag, so the flags live in
  `extdata` only.
- A conversion is `refused` when the file states that modification is
  prohibited. VRM 1.0 says so with `modification: "prohibited"`, which is also
  the default of its schema when the field is absent. A value that is neither
  of the two that allow modification is read as prohibited. VRM 0.x has no
  such flag; there the licence name decides, and a licence that forbids
  derivatives (a `ND` Creative Commons licence, or `Redistribution_Prohibited`)
  refuses.
- For every other value the conversion needs `acknowledgeRights: true`. Without
  it the operation answers the report with the rights fields listed and writes
  nothing. An agent must show them to the user; it must not set the flag on its
  own.
- An MMD model has no machine-readable terms. A PMX or PMD import therefore
  always needs `acknowledgeRights: true`, and the report says that the terms
  are in the model's readme and were not read. The comment fields of the model
  header are `kept` in `extdata` under `io.github.geekjapan.soul2d.MmdComment`.
- An export never drops kept rights data, and it needs `acknowledgeRights:
  true` like an import when the document holds any.
  - A VRM 1.0 export writes a kept VRM 1.0 `meta` back unchanged, except an
    index that points at nothing, which is removed and reported.
  - A plain glTF export writes every kept rights entry into `asset.extras`
    under its `extdata` key, and the copyright text into `asset.copyright`.
  - A VRM 1.0 export of a document whose kept `meta` is VRM 0.x is `refused`.
    The permission fields of the two versions do not map one to one, and
    Soul2D does not write VRM 0.x.
  - A VRM export of a document without kept rights data is `refused`. A plain
    glTF export of such a document writes the puppet metadata it has.

## Persistence

A rig that came from 3D exists in INX only. The INP export strips the
editor-only rig nodes (`inp_export.rs`), so an exported INP holds the Parts in
their rest shape without DepthBone and DepthRigRoot. The export does not
report the stripped rig nodes today (`prunedNodes` counts disabled nodes
only). Reporting them is not done: the rig import (#243) does not touch the
export. The export does not refuse, because that is what an INP is.
`extdata` is written to INX and INP alike, so the kept rights data stays with
the file.

## Limits a converter applies

- 64 bones for one rig, 128 sources for one binding and 16 influences for one
  vertex (`bone_deform.rs`).
- 65,535 vertices for one Part mesh (`mesh_edit.rs`) and 1,000,000 for one
  deform cell (`bone_deform.rs`). What happens to a primitive with more
  vertices than a Part holds, a split or a refusal, is decided in #244.
- 256 bytes for an `extdata` key, 16 MiB for one payload and 4096 entries
  (`lib.rs`). Data that must be `kept` and does not fit is `refused`, because
  it cannot be dropped.
- The parser limits of ADR 0003 for counts and external resources. The rig
  import prototype (#243) sets the remaining numbers.
