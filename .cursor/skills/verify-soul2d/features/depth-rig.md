# Depth rig

## Sub-features

- Create a DepthRigRoot and add the standard 19-bone skeleton.
- List bones with their rest pose.
- Bind targets, set depths and apply bone deformation (mapped, not yet driven).

## How to get to it (user POV)

Add a `DepthRigRoot` from the Nodes panel. With it selected, the Inspector's
`Depth Rig` section has `Add Standard Skeleton`, `Add Standard Parameters` and
`Fit Z to Depth`.

## Driving it with njc

```bash
./out/rust/debug/njc --port 18090 tools call DepthBoneCommand_CreateDepthRigRoot --json '{"name":"Rig"}'
./out/rust/debug/njc --port 18090 tools call DepthBoneCommand_AddStandardDepthSkeleton --json '{"root":ROOT}'
./out/rust/debug/njc --port 18090 tools call DepthBoneCommand_ListDepthBones --json '{"root":ROOT}'
```

The create call answers `{"created":[{"type":"DepthRigRoot","uuid":ROOT,...}]}`.
The skeleton and list calls answer `{"items":[...]}` with one entry per bone:
`boneId`, `name`, `parent`, `uuid`, `restHead`, `restTail`, `restRoll`.

Proof: 19 items whose `boneId`s are Pelvis, Spine, Chest, Neck, Head, and Clavicle,
UpperArm, Forearm, Hand, Thigh, Shin, Foot with `.L` and `.R`; Pelvis's `parent` is
`ROOT`. `nodes_list` shows the same nodes with type `DepthBone`.

Not driven yet: `DepthBoneCommand_AddDepthBoneSource`, `DepthMapCommand_SetDepths`,
`DepthBoneCommand_PreviewDepthBoneDeform`, `DepthBoneCommand_ApplyDepthBoneDeform`,
`rig_fit_human` and `rig_solve_ik`. Read their schemas from `tools list`; the Rust
tests in `rust/tests/depth_session.rs` show working sequences.

## Gotchas

- A second `DepthBoneCommand_AddStandardDepthSkeleton` on a root that already had the
  19 bones failed with `cannot match D parent bounds for the standard skeleton`; the
  existing bones stayed. Create a new root for a second skeleton.
- Save keeps rig nodes. INP export drops `DepthRigRoot` and `DepthBone`.
- Applying a deform writes 2D offsets into the armed parameter's current key, so a
  parameter and a target deformer must exist first.
- Rig tools are INX-only and fail on a native CMO3 document.
