# soul2d feature map

Each file answers, for one feature: what it is, how a user reaches it, how to drive
it with `njc`, and what observable state proves it works.

| Feature | Entry points | Observable proof | Default proof |
| --- | --- | --- | --- |
| [Project files](project-files.md) | File menu; `FileCommand_OpenFile`, `FileCommand_SaveFile` | Saved file reopened with the edited node | yes |
| [Nodes](nodes.md) | Nodes panel; `NodeCommand_*`, `nodes_list` | Created and renamed node read back; undo/redo | create and rename |
| [Image import and automatic mesh](import-and-mesh.md) | File > Merge / Import; `merge_images`, `automesh_generate` | Part with the generated vertex count after reopen | yes |
| [Viewport capture](viewport-capture.md) | View > Save Screenshot; `ViewCommand_CaptureLiveScreenshot` | PNG with painted pixels, empty scene unpainted | yes |
| [Parameters](parameters.md) | Parameters panel; `ParamCommand_*`, `parameter_get` | Record with the requested range; gone after delete | no |
| [Depth rig](depth-rig.md) | Inspector Depth Rig; `DepthBoneCommand_*` | 19 standard bones under the root | no |
| [Motion3](motion3.md) | Live2D Motion3 section; `Motion3Command_*` | Mapped pose value, runtime state, exported file | no |
| [Live2D native documents](live2d-native.md) | File > Open a `.cmo3`; `open`, `Live2DCommand_*` | `documentFormat`, runtime value, preview with warnings | no, needs a user file |
| [Animation baking](../../../../doc/motion-sync.md#verification) | Timeline **Bake physics** and **Speech lip sync**; `animation_physics_bake`, `animation_motionsync_start`, `animation_motionsync_status`, `animation_motionsync_cancel`, `animation_motionsync_bake` | Baked lanes read from the snapshot resource, sampled values, refusal atomicity, undo/redo and save/reopen | no, dedicated proof |
| [Loop editing support](#loop-editing-support) | Animation settings **Loop editing support** and **Apply animation settings**; `animation_set` with `loopEditing` | Periodic values, unchanged saved key positions, linked timeline copies, refusal atomicity, undo/redo, save/reopen, and PNG output | no, dedicated proof |
| [Physics groups](#physics-groups) | CMO3 outline **Physics groups** panel; `live2d_physics3_read`, `live2d_physics3_edit` | Typed view, scalar/list edits, refusal atomicity, undo/redo, save/reopen over live MCP | no, dedicated proof |
| [CAN3 key structure](#can3-key-structure) | `live2d_animation_key_add`, `live2d_animation_key_delete`; animation-source resource | Ordered paired keys/types, history, refusal atomicity, accumulated edits, save/reopen and exact unrelated XML/CAFF bytes | no, dedicated proof |
| [CAN3 PartOpacity keys](#can3-partopacity-keys) | Existing five key operations; animation-source resource | Modern and legacy-colon PartOpacity key edits, inherited caches, ownership refusals, history, native save/reopen and unrelated XML/CAFF bytes | no, dedicated proof |
| [CAN3 parameter and Part curves](#can3-parameter-and-part-curves) | `live2d_animation_curve_add`, `live2d_animation_curve_delete`; animation-source resource | Empty curve add/delete, readd ordering and identity, opaque entries, history, refusal atomicity, save/reopen and exact unrelated XML/CAFF bytes | no, dedicated proof |
| [CAN3 model tracks](#can3-model-tracks) | `live2d_animation_track_add`, `live2d_animation_track_delete`; animation-source resource | Existing-resource track add/delete, final curve/key serialization, GUID selection, history, refusal atomicity, save/reopen and unrelated XML/CAFF preservation | no, dedicated proof |
| [CAN3 scenes](#can3-scenes) | `live2d_animation_scene_delete`, existing scene add/set; animation-source resource and `live2d_animation_list` | GUID-based scene deletion, retained-scene edits, empty scenes, composed additions, history, atomic refusals and exact unrelated XML/CAFF preservation | no, dedicated proof |
| [Split plan context](#split-plan-context) | `split_open`, `split_part_set`, `split_plan`; `resource://soul2d/part-split/plan` | Literal plan and statistics, draft revisions, read-only errors, and replay through production njc | no, dedicated proof |

"Default proof" says what `scripts/prove.py` exercises. Every command shown in a
feature file was run against a headless host on 2026-10-12 unless the file says it
was not driven.

## Coverage

```bash
.cursor/skills/verify-soul2d/scripts/coverage.py
```

The 9 feature files name 53 of the 564 tools in `doc/commands.manifest.json`. Run the
coverage report for the remaining tool families; `--strict` reports unmapped tools as a failure. The
dedicated checks documented below are outside the default proof and are not counted by that file-based
report. GUI-only behaviour (menus, layout, shortcuts) needs the appendix in `SKILL.md`.

Source anchors: the tool registry in `rust/src/mcp.rs` and `rust/src/mcp_aliases.rs`,
the editor in `rust/src/editor_ui.rs` and `rust/src/editor_windows.rs`. Run
`/maintain-verification-skill` when these paths or the public tools change.

## Loop editing support

Set `TARGET` to the isolated Cargo target directory containing the current binaries.
Run the [dedicated MCP proof](../../../../tools/verify-animation-loop-edit.py).

```bash
python3 tools/verify-animation-loop-edit.py \
  --core "$TARGET/debug/soul2d-core" \
  --njc "$TARGET/debug/njc"
```

The script starts its own headless host and drives the production `njc` client.
The script checks all five interpolation modes against literal expected values.
The snapshot resource must show the enabled flag and the original saved keys.
Undo, redo, save, and reopen must preserve the intended flag and keys.
Invalid flag values and incompatible loop markers must leave the snapshot unchanged.
The PNGs must differ between the ordinary and periodic poses.
The clock check must advance through the last frame before the loop boundary.
The script records commands, raw responses, resources, PNGs, binary and source hashes,
and the owned host's exit under `build/verification/ani229/`.
Open the PNGs before reporting appearance.

Verify GUI behavior separately with the installed cua-driver skill.
Enable **Loop editing support** in the animation settings and click
**Apply animation settings**. Confirm the flag through the editor's MCP resource.
Select a neighboring-cycle key and edit its key value. Confirm that only the saved
source key changed and that every displayed copy reflects that value.
Undo once and confirm the original value. This input test is not covered by
headless MCP or egui tests.

The INX check does not prove native Cubism document support or vendor acceptance.
The playhead stays in the saved clip range. Out-of-range saved keys are refused,
not deleted or replaced with copies.

## Physics groups

Set `TARGET` to the isolated Cargo target directory containing the current binaries.
Run the [dedicated MCP proof](../../../../tools/verify-physics3-edit.py) against the
frozen synthetic fixture `rust/tests/fixtures/physics3-editor.cmo3` (one Sway group on
ParamTest, two vertices, one input, one output, FPS 60).

```bash
python3 tools/verify-physics3-edit.py \
  --core "$TARGET/debug/soul2d-core" \
  --njc "$TARGET/debug/njc" \
  --fixture rust/tests/fixtures/physics3-editor.cmo3
```

The script starts its own headless host and drives the production `njc` client.
It checks the typed `live2d_physics3_read`/`live2d_physics3_edit` round-trip:
full authored precision (0.800001) versus the rounded `.physics3.json` export,
refusal atomicity, insert/removal with output renumbering, undo/redo, and the
saved/reopened view. Proof scope ends there: the six `editor_physics_groups` egui
tests cover the panel code path but are not native GUI acceptance, and no vendor
CMO3 or native window run is part of it (native GUI/vendor parity: NOT_RUN).

The proof also removes the last group and creates a populated first group without
a donor, checks one-step Undo/Redo and saves/reopens the created group. Linux Rust
CI runs this synthetic proof with isolated settings and retains only its result
JSON. Missing, null and shared storage refusals are covered by Rust tests; this
does not establish vendor interoperability or acceptance of every physics control.

For panel inspection in a real editor window, expand the collapsed-by-default
headers first: **Physics groups**, then the **Sway (group 0)** group header and the
**input inputs (1)**, **output outputs (1)** and **vertex vertices (2)** list
headers before using inner controls.

## CAN3 parameter and Part curves

Set `TARGET` to the isolated Cargo target directory containing the current binaries.

```sh
python3 tools/verify-can3-curves.py \
    --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc"
python3 tools/verify-can3-part-curves.py \
    --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc"
```

The proof packs the synthetic `rust/tests/fixtures/can3-curve-editor.xml` into its own CAN3 and drives an owned HTTP MCP host through `njc`. It checks empty parameter-curve addition from a CMO3 source, addition of keys before save, undo/redo, delete/re-add ordering, opaque attribute entries, the same parameter across two tracks, refusal atomicity, add/delete cancellation byte equality and a save/reopen oracle that strips only the new definition and reference. Native GUI acceptance is SKIP, Cubism vendor acceptance is NOT_RUN, and CI is skipped by user instruction.

The Part proof uses `can3-part-opacity.xml` and generated CMO3 Parts. It checks cumulative additions and
key edits before saving, exact unrelated XML/CAFF preservation, history, selector and duplicate refusals,
byte-exact cancellation, and re-add with original text or typed Part identity and opaque metadata.
Rust tests additionally cover absent/null effect creation, mixed curve additions, new track/scene save and
deletion, legacy ownership refusal and nested outside references. These do not establish native GUI curve
CRUD or Cubism vendor acceptance.

## CAN3 model tracks

Set `TARGET` to the isolated Cargo target directory containing the current binaries.

```sh
python3 tools/verify-can3-tracks.py \
    --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc"
```

The proof uses synthetic CAN3 and CMO3 containers, an owned loopback HTTP host and production `njc`. It checks live schemas, generated track GUIDs, existing-resource binding, curves/keys/handles added before save, new-scene composition, undo/redo, save/reopen and atomic refusals. Independent XML span oracles allow only expected owner/reference additions or removals and padded count-value changes; unrelated XML and non-main CAFF data must remain unchanged. The proof retains commands, raw responses, transitive helper hashes and host cleanup under `build/verification/ani145-tracks/`.

Fixed-ID Rust tests additionally exercise scene/track/curve storage collisions, IDs reserved after deletion, special-character identities and owner-first definition order. Native GUI acceptance is SKIP, Cubism vendor acceptance is NOT_RUN, and CI is skipped. Track moves, scene deletion, resource creation and part-opacity curve creation are outside this track proof; scene deletion has its own proof below.

## CAN3 scenes

Set `TARGET` to the isolated Cargo target directory containing the current binaries.

```sh
python3 tools/verify-can3-scenes.py --self-test
python3 tools/verify-can3-scenes.py \
    --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc"
```

The first command tests the independent XML-span oracle, including corrupted counts, unrelated data, restored scalar edits and UTF-8 offsets; it is not a product test. The second drives production `njc` against an owned loopback HTTP host with synthetic CAN3/CMO3 data. It requires exact live/manifest schemas, deletion by scene GUID, current scene-list/resource reads, undo/redo, save/reopen, last-scene deletion, new scene/track/curve composition and add/populate/delete cancellation before save.

The proof deletes an earlier scene before editing retained settings, keys and new tracks, and deletes original scenes after pending child edits. The oracle permits only owned scene/track/parameter-curve definitions, the project membership reference, expected scalar edits and lexical count changes; unrelated XML and non-main CAFF data must remain unchanged. Retained opaque track members stay untouched. Existing scene addition and deletion of another safe scene must also preserve readable inline scenes and aliased GUID storage in survivors. Invalid selectors/revisions and unsafe ownership variants must first open successfully and then leave the snapshot, history and owned files unchanged on refusal.

Deletion is limited to provably owned model-track/parameter-curve storage. Unsupported non-model tracks, shared/aliased/inherited/nested ownership in a deleted scene and outside references are conservatively refused; those ownership checks must not constrain unrelated surviving scenes. Linked-model resources remain even with no scenes. Scene/track moves, resource creation and additional curve/track kinds are outside this proof. Records, transitive helper/source/binary hashes and host cleanup remain under `build/verification/ani145-scenes/`. Native GUI acceptance is SKIP, Cubism vendor acceptance is NOT_RUN, and CI remains disabled.

## Split plan context

The INP/INX Outline **Part split review** section exposes mask previews, statistics, reorder, remove, and explicit apply. Headless egui tests in `editor_part_split::tests` exercise those controls, stale pointer input across multipass frames, and display-only texture resizing. `part_split_review_is_reachable_and_resets_on_remote_replacement` drives the real app and its MCP replacement path with a repeated draft revision. These tests do not establish native-window acceptance.

`part_split_applied` tests and the production context proof check branch-local application records, rename, undo/redo, UUID reuse after deletion, history pruning/clear, and save/reopen. The snapshot, tool, and resource must agree on `applied`. Raw apply still appends in one history step.

Run `python3 tools/verify-part-split-context.py --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc"` with the current isolated binaries. The proof starts its own loopback host and uses production `njc` to list and read the resource, change a draft at a fixed document revision, verify read-only failures, and replay the nested plan. The generated 8x8 fixture checks API behavior, not character split quality. Native GUI acceptance is `SKIP`, vendor acceptance is `NOT_RUN`, and CI remains disabled.

The same proof checks `split_components` discovery, strict alpha threshold, deterministic seed/count/bounds,
65 isolated pixels over three pages, seed-to-mask readback, and unchanged snapshots on successful and refused
reads. `part_split_resources` tests also cover native/CAN3 refusal, 33/65/128 islands and applied lineage;
the private walker test covers a winding region and budget exhaustion.

The raster phase uses `split_mask_inspect`, applies its canonical step through `split_part_set`, reads literal soft
coverage from the mask resource and replays the plan in another owned host. Changed/missing raster files must
refuse without state mutation. `part_split_resources` also checks source-alpha preservation, all combine modes,
references, PNG-format/dimension/size refusals and missing fingerprints. A private loader test checks the shared
work budget before file reads. This validates local raster input only; provider execution and inpainting remain open.

`python3 tools/verify-part-split-quality.py --core "$TARGET/debug/soul2d-core" --njc "$TARGET/debug/njc" --psd PATH`
measures the pinned, locally acquired CC0 Alien T-Rex PSD. It independently decodes 18 artist-layer masks,
constructs the benchmark PNG, measures a component-only production MCP baseline, applies and renders the plan,
and retains PNGs, per-layer scores, source/binary hashes and host cleanup. The fixed predictions' mean best-layer
IoU is 0.0896064; matching is nonexclusive. Alpha reconstruction is exact and RGB is checked against the existing
8bit premultiplication/PNG quantization, not byte equality. `BASELINE_MEASURED` does not establish PSD displayed
appearance equivalence, human-character segmentation, semantic quality, provider or real AI-agent acceptance.

## CAN3 key structure

Set `TARGET` to the isolated Cargo target directory containing the current binaries.

```bash
python3 tools/verify-can3-key-structure.py \
  --core "$TARGET/debug/soul2d-core" \
  --njc "$TARGET/debug/njc"
```

The proof packs the synthetic `rust/tests/fixtures/can3-key-editor.xml` into its own CAN3 and drives an owned HTTP MCP host through `njc`. It records each action and reads `resource://soul2d/document/animation-source` and document state after edits. It checks default metadata, multiple inserts into one original gap, edits before save, original-frame readdition, empty curves, strict refusal atomicity, undo/redo, actual save/reopen and independent byte preservation.

No third-party model, native GUI or Cubism Editor is used. Vendor acceptance remains `NOT_RUN`. This covers parameter-key insertion/deletion only, not curve, track or scene CRUD.

## CAN3 PartOpacity keys

```bash
python3 tools/verify-can3-part-opacity.py \
  --core "$TARGET/debug/soul2d-core" \
  --njc "$TARGET/debug/njc"
```

The proof uses `rust/tests/fixtures/can3-part-opacity.xml`, a generated fixture with two modern PartOpacity
curves, a legacy curve and an unrelated parameter curve. It checks all five key operations, state/resource
agreement, one-step history, stale and ownership refusals, save/reopen and byte preservation outside the
target attribute. Inline inherited identity/cache fields match locally observed CAN3 structure. This is a
source-editing proof; native GUI and Cubism vendor acceptance are separate gates.

Run the legacy-colon proof with `python3 tools/verify-can3-legacy-keys.py --core "$TARGET/debug/soul2d-core"
--njc "$TARGET/debug/njc" --native-can3 "$NATIVE_CAN3"`. The native input is the pinned local Hiyori
`hiyori_free_t03.can3` (139,956 bytes, SHA-256 `3dc6b5d1fcccb653976c650a89d2a83c966e15a45ab40e9002f32955677e6c19`).
The script acquires no input and edits only owned outputs. It exercises all five key operations, per-operation
Undo/Redo and stale refusal, save/reopen and CAFF/XML preservation on generated and native data. The generated
curve deliberately has different map, attribute and source identities. Rust tests cover absent lists, optional
members, external/shared storage refusal, immutable legacy metadata/membership and composition with modern edits
and new tracks. Legacy curve CRUD and underscore-map channels remain outside this proof.

Run the underscore-map proof with `python3 tools/verify-can3-underscore-keys.py --core "$TARGET/debug/soul2d-core"
--njc "$TARGET/debug/njc" --rice-can3 "$RICE_CAN3" --ren-can3 "$REN_CAN3"`.
It requires the existing local Rice `rice_pro_motions_t02.can3` (95,670 bytes, SHA-256
`ad7ff25d60619b77944c0473403c2bb9a04d71be9adf84a95ed151b826042631`) and Ren `ren_t01.can3`
(181,386 bytes, SHA-256 `e9de09c664a44d39954838cd95b4e6453f32b5a5f2f03af1b7b897be30869e1b`).
It acquires no inputs. Generated and native copies exercise duplicate attribute-ID selection through exact
`partOpacityMapKey`, all five key operations, per-operation Undo/Redo and stale refusal, save/reopen, and exact
XML/CAFF preservation outside edited keys and their verified caches. Rice covers extra opaque option fields;
Ren covers absent list membership. Underscore curve CRUD, playback arbitration and vendor acceptance remain open.
