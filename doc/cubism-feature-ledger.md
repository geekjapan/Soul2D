# Cubism 5.3 Pro operation ledger

Status: **OPEN — enumeration and probe design, not a compatibility verdict**.
Target: Editor 5.3.04 Pro (2026-09-03); Native R5 is a separate runtime reference.
This ledger expands the [scope](acceptance-scope.md); it does not replace the
all-features target with the operations listed here. Manual suboptions, cumulative
release fixes, capacities, and related-product boundaries still need reconciliation.

## Source and coverage rules

Sources S1–S10 are linked in [official research](cubism-53-research.md).
The Cloud reread on 2026-10-10 retained ten public HTML pages and their hashes as
local evidence. That evidence is not part of this repository and is not vendor
execution. Additional public pages:

- S11: [Blend mode](https://docs.live2d.com/en/cubism-editor-manual/blend-mode/)
- S12: [Offscreen drawing](https://docs.live2d.com/en/cubism-editor-manual/offscreen-drawing/)

Both additional pages say updated 2026-01-19; their current 5.4-alpha banner is
not evidence of 5.4 inclusion in the target. S1/S5 are discovery sources, not
version guarantees. Each row requires a 5.3.04 check before its scope is frozen.
All rows below have outcome **NOT_RUN** for vendor acceptance, regardless of
partial implementation or historical unit tests. No fixture absence is an exclusion.

Status **ACCEPTED_MAPPING** marks a row where the vendor feature is not built
and a mapping to Soul2D tools is recorded instead ([ADR 0002](adr/0002-cubism-external-api-mapping.md),
accepted by the maintainer on 2026-10-12). No vendor acceptance is run for it, so
its outcome stays NOT_RUN, and the row is outside the completion goal
([acceptance scope](acceptance-scope.md)).

Each ID names a separate obligation and a dedicated probe `P-<ID>`. The probe is
not yet an artifact. For every row create, edit, Undo/Redo, save, reopen, and read
semantic state, plus rendered evidence where applicable. Run the four mandatory
models wherever they contain that feature; use the dedicated probe regardless
when the models do not cover a required case. Pin all inputs, references, edits,
outputs, and operation transcripts. An operation group must be split further if
its suboptions have different semantics. Workflow-only rows need actual UI input,
not a command response substituted for a palette or gesture.

Owner column names the **planned Rust subsystem**, not verified implementation:
IO = document/persistence/import/export; GEO = geometry/deformers; PAR = parameters/
keyforms; REN = rendering; ANI = animation; UI = desktop workflows; API = adapters.
Comparison classes are in [the candidate contract](vendor-comparison-contract.md):
S semantic/discrete, G geometry, V values/time, I images, W actual workflow.

## Operation obligations

| ID | Operation / dedicated probe content | Source | Owner | Compare |
| --- | --- | --- | --- | --- |
| IO01 | PSD import with layers and ordering | S5 | IO | S,I,W |
| IO02 | PSD reimport with renamed/added/removed layers | S5 | IO | S,I,W |
| IO03 | Original-image/model-image link relinking | S5 | IO | S,I,W |
| IO04 | Texture atlas create, layout edit, replacement | S5 | IO | S,G,I,W |
| IO05 | PNG export/replacement beta workflow, classify against 5.3.04 | S5 | IO | S,I,W |
| IO06 | ID/name CSV export | S1,S5 | IO | S,W |
| IO07 | ID/name CSV import with collision refusal | S1,S5 | IO | S,W |
| IO08 | Model template creation/export | S5 | IO | S,W |
| IO09 | Model template application | S5 | IO | S,G,W |
| IO10 | Automatic backup and recovery after interruption | S5 | IO | S,W |
| IO11 | Pro data open/save across edition constraints (no license bypass) | S1 | IO | S,W |
| GEO01 | Automatic ArtMesh generation | S5 | GEO | S,G,I,W |
| GEO02 | Manual mesh vertex/edge/triangle editing | S5 | GEO | S,G,I,W |
| GEO03 | Mesh copy/paste and reflected paste | S1,S5 | GEO | S,G,W |
| GEO04 | Mesh-generation preset save/reuse | S1 | GEO | S,G,W |
| GEO05 | Mesh reset and coincident vertex handling | S2 | GEO | S,G,W |
| GEO06 | Art-path automatic generation | S5 | GEO | S,G,I,W |
| GEO07 | Art-path manual creation | S5 | GEO | S,G,I,W |
| GEO08 | Art-path brush editing | S5 | GEO | S,G,I,W |
| GEO09 | Art-path stroke creation/editing | S5 | GEO | S,G,I,W |
| GEO10 | Warp deformer creation including continuous creation | S1,S5 | GEO | S,G,W |
| GEO11 | Rotation deformer creation including continuous creation | S1,S5 | GEO | S,G,W |
| GEO12 | Parent-child reparenting and nested spaces | S5 | GEO | S,G,I,W |
| GEO13 | Automatic deformer generation | S5 | GEO | S,G,W |
| GEO14 | Out-of-parent child validation | S1,S5 | GEO | S,W |
| GEO15 | Warp expansion preserving keyed child motion | S2,S4 | GEO | S,G,I,W |
| GEO16 | Deformer shape restoration | S5 | GEO | S,G,W |
| GEO17 | Apply deformer to children including blend-shape constraints | S2 | GEO | S,G,I,W |
| GEO18 | Glue creation/editing | S5 | GEO | S,G,I,W |
| GEO19 | Deformation path editing | S5 | GEO | S,G,I,W |
| GEO20 | Skinning | S5 | GEO | S,G,I,W |
| GEO21 | Temporary deformation tool | S5 | GEO | S,G,W |
| GEO22 | Square deformation/selection brush | S4 | GEO | S,G,W |
| GEO23 | Line deformation/selection brush | S4 | GEO | S,G,W |
| GEO24 | Inflation/contraction brush | S4 | GEO | S,G,W |
| GEO25 | Reflection of rotation-deformer position | S4 | GEO | S,G,W |
| PAR01 | Parameter key addition | S5 | PAR | S,V,G,W |
| PAR02 | Parameter key deletion | S5 | PAR | S,V,G,W |
| PAR03 | Parameter range/default adjustment | S5 | PAR | S,V,W |
| PAR04 | Standard parameter list application | S5 | PAR | S,W |
| PAR05 | Linear keyform interpolation with coupled axes | S1,S5 | PAR | V,G,I |
| PAR06 | Ellipse extended interpolation | S1,S5 | PAR | V,G,I,W |
| PAR07 | SNS extended interpolation and regenerated intermediates | S1,S5 | PAR | V,G,I,W |
| PAR08 | Repeat across parameter endpoints | S1,S5 | PAR | V,G,I,W |
| PAR09 | Record parameter manipulation into animation | S5 | PAR | S,V,W |
| PAR10 | Copy/paste parameter values | S5 | PAR | S,V,W |
| PAR11 | Form copy | S1,S5 | PAR | S,G,W |
| PAR12 | Form paste and target selection | S1,S5 | PAR | S,G,W |
| PAR13 | Form blend | S1 | PAR | S,G,I,W |
| PAR14 | Special form paste options (enumeration still required) | S5 | PAR | S,G,W |
| PAR15 | Automatic corner form generation | S5 | PAR | S,G,I,W |
| PAR16 | Automatic face movement generation | S5 | PAR | S,G,I,W |
| PAR17 | Automatic sway movement generation | S5 | PAR | S,G,I,W |
| PAR18 | Vertex edits in canvas coordinates | S5 | PAR | S,G,W |
| PAR19 | Multi-key editing | S5 | PAR | S,G,W |
| PAR20 | Motion reflection | S5 | PAR | S,G,I,W |
| PAR21 | Blend-shape keyforms and additive deltas | S5 | PAR | S,G,I,W |
| PAR22 | Blend-shape weight constraints | S5 | PAR | S,V,G,W |
| PAR23 | Default form lock | S1 | PAR | S,W |
| PAR24 | Multiply-color keyforms | S1,S5 | PAR | S,V,I,W |
| PAR25 | Screen-color keyforms | S1,S5 | PAR | S,V,I,W |
| PAR26 | 3D rotation expression application/settings | S5 | PAR | S,G,I,W |
| PAR27 | MotionSync configuration | S5 | PAR | S,V,W |
| PAR28 | Set all current parameter values as defaults | S2 | PAR | S,V,W |
| REN01 | Draw order and draw-order groups | S5 | REN | S,I,W |
| REN02 | Clipping masks and mask membership | S5 | REN | S,I,W |
| REN03 | Inverted masks | S5 | REN | S,I,W |
| REN04 | Offscreen enable implies draw-order grouping | S12 | REN | S,I,W |
| REN05 | Offscreen part opacity keyforms | S12 | REN | S,V,I,W |
| REN06 | Offscreen clipping and inverse mask | S12 | REN | S,I,W |
| REN07 | Offscreen multiply-color keyforms | S12 | REN | S,V,I,W |
| REN08 | Offscreen screen-color keyforms | S12 | REN | S,V,I,W |
| REN09 | Nested offscreen passes and statistics | S12 | REN | S,I,W |
| REN10 | Advanced-blend overlapping polygon indication | S11 | REN | S,I,W |
| UI01 | Workspace and palette layout persistence | S5 | UI | S,W |
| UI02 | Multiple views with independent active-view selection | S1,S2,S5 | UI | S,I,W |
| UI03 | Solo display in active/non-active views | S1,S2,S5 | UI | S,I,W |
| UI04 | Focus selected object | S1 | UI | S,I,W |
| UI05 | Guides | S5 | UI | S,I,W |
| UI06 | Grid | S5 | UI | S,I,W |
| UI07 | Snapshot capture/use as sketch | S1,S5 | UI | S,I,W |
| UI08 | Onion skin and vertex trajectories | S1,S5 | UI | S,I,W |
| UI09 | Random pose playback/settings | S2,S5 | UI | S,V,W |
| UI10 | Object find/replace | S2,S5 | UI | S,W |
| UI11 | Shortcut customization and platform keys | S2,S5 | UI | S,W |
| UI12 | Parts visibility/locking/nesting/selection | S1,S5 | UI | S,I,W |
| UI13 | Parameter reordering with destination commands | S2 | UI | S,W |
| UI14 | Menus and disabled-command behavior | S2,S5 | UI | S,W |
| ANI01 | Linked model loading/placement by explicit identity | S5 | ANI | S,G,I,W |
| ANI02 | Blink settings | S5 | ANI | S,V,I,W |
| ANI03 | Audio/BGM track placement and synchronization | S5 | ANI | S,V,W |
| ANI04 | Background and group tracks | S5 | ANI | S,I,W |
| ANI05 | Scene create/select/settings | S5 | ANI | S,V,W |
| ANI06 | Pose switching in motions | S5 | ANI | S,V,I,W |
| ANI07 | Dope-sheet key operations | S5 | ANI | S,V,W |
| ANI08 | Graph editor keys/handles/interpolation | S5 | ANI | S,V,W |
| ANI09 | Parameter bookmarks | S5 | ANI | S,W |
| ANI10 | Shy track filtering | S5 | ANI | S,W |
| ANI11 | Freeze track operation | S5 | ANI | S,W |
| ANI12 | Repeat parameter animation | S5 | ANI | S,V,I,W |
| ANI13 | Timeline markers | S5 | ANI | S,V,W |
| ANI14 | Track trajectories | S5 | ANI | G,I,W |
| ANI15 | Animation template application | S5 | ANI | S,V,W |
| ANI16 | Fade settings/evaluation | S5 | ANI | S,V,I,W |
| ANI17 | Lightweight playback | S5 | ANI | V,I,W |
| ANI18 | Parameter controller creation/target editing | S2,S5 | ANI | S,V,G,W |
| ANI19 | Form animation creation for every target object type | S1,S5 | ANI | S,G,I,W |
| ANI20 | Form animation exposure timing | S5 | ANI | S,V,I,W |
| ANI21 | Form animation adjustment mode | S5 | ANI | S,G,W |
| ANI22 | Seek/play/pause/loop/reset with scene FPS and offsets | S5 | ANI | V,I,W |
| ANI23 | Image-sequence track and time remapping | S1,S5 | ANI | S,V,I,W |
| ANI24 | Stepped exposure timing (not only three-frame holds) | S1,S5 | ANI | S,V,I,W |
| ANI25 | Physics animation baking | S5 | ANI | S,V,I,W |
| ANI26 | MotionSync animation baking | S5 | ANI | S,V,W |
| ANI27 | Runtime-model tracks | S1,S5 | ANI | S,V,I,W |
| ANI28 | Loop-edit assistance | S5 | ANI | S,V,W |
| EXP01 | Still-image sequence export | S1,S5 | IO | S,V,I,W |
| EXP02 | GIF export with FPS and loops | S1 | IO | S,V,I,W |
| EXP03 | MP4 export | S1 | IO | S,V,I,W |
| EXP04 | Transparent WebM export | S4 | IO | S,V,I,W |
| EXP05 | MOV export | S1 | IO | S,V,I,W |
| EXP06 | AAC audio export (codec/platform/legal gates separate) | S1 | IO | S,V,W |
| EXP07 | Collect linked assets without broken paths | S1,S5 | IO | S,W |
| EXP08 | Runtime data export and provenance | S5 | IO | S,V,I,W |
| EXP09 | User data and events | S5 | IO | S,V,W |
| PHY01 | Physics configuration/groups | S5 | PAR | S,V,I,W |
| PHY02 | Physics presets import/save | S2,S5 | PAR | S,V,W |
| PHY03 | Physics playlists | S5 | PAR | S,V,W |
| API01 | External-application API operations. **ACCEPTED_MAPPING**: 15 methods and 5 events enumerated for 5.3.04, no wire-compatible endpoint; mapping in [cubism-external-api.md](cubism-external-api.md) | S5 | API | S,W |
| REL01 | Cubism Viewer (for OW). **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |
| REL02 | Cubism Viewer for Unity. **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |
| REL03 | Material separation Photoshop plug-in. **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |
| REL04 | Scripts for Photoshop. **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |
| REL05 | Cubism AE plug-in. **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |
| REL06 | Verification tool (MOC3 consistency checker). **ACCEPTED_MAPPING**: separate product, no counterpart | S5 | none | n/a |

REL01-REL06 are classified in [the related products section](cubism-external-api.md#related-products).

## Blend obligations — do not collapse to a single mode

S4 describes “15 color blends”; S11 explicitly lists Normal, fifteen additional
color modes, and two legacy modes. Retain **every named entry** rather than
discarding one to force a headline count. Pin the actual 5.3.04 enum/UI identifiers
before the inventory is frozen. S11 also states that legacy Add/Multiply ignore
the alpha-blend setting; test that behavior, not an invented formula.

| IDs | Individual named operations, in ID order | Probe and comparison |
| --- | --- | --- |
| COL01–COL06 | Normal; Add; Add (Glow); Darken; Multiply; Color burn | One probe per ID; REN; S,V,I,W |
| COL07–COL12 | Linear burn; Lighten; Screen; Color dodge; Overlay; Soft light | One probe per ID; REN; S,V,I,W |
| COL13–COL18 | Hard light; Linear light; Hue; Color; Add (Before 5.3); Multiply (Before 5.3) | One probe per ID; REN; S,V,I,W |
| ALP01–ALP05 | Over; Atop; Out; Conjoint; Disjoint | One probe per ID; REN; S,V,I,W |

Require the COL01–COL16 × ALP01–ALP05 cross-product, on ArtMesh and offscreen
parts, with nested offscreen and clipping variants. For COL17/18 vary all five
alpha settings and verify that the rendered output is unchanged. Include partial
alpha, transparent colors, bright overflow inputs, different draw orders, and
canvas/background isolation. PSD import/export is a distinct probe: S11 says
unsupported color modes import as Normal and all alpha modes import as Over;
S12 says offscreen settings are not supported in PSD interchange. These vendor
limitations are not authorization for silent loss in CMO3/CAN3/INX conversion.

## Capacity and reconciliation backlog (still in scope)

S1 separately requires texture count, ArtMesh count, ordinary parameter count,
blend-shape parameter count, combined deformer count, parts, art paths, warp
subdivisions (100×100), and simultaneous keyform parameter capacity. Create an
individual capacity probe for each, including values above Free limits. “No
limit” is not an infinite-memory requirement: freeze concrete finite stress
sizes and supported hardware before running; allocation refusal must be safe.
Do not infer capacity acceptance from a small fixture.

Still enumerate menu actions, special-paste suboptions, preferences, key/curve
handle modes, platform-specific exports, release-history regressions through
5.3.04, and the existing GUI inventory. The external API is enumerated in API01.
Viewer, Photoshop helpers, AE plugin and verification tools are classified in
REL01-REL06 (**ACCEPTED_MAPPING**), no longer BOUNDARY_UNRESOLVED, and are not
silently excluded: each has a recorded position. This ledger does not close
phase 1. No all-features percentage may be computed from its current row count.

## Definition checks on 2026-10-27

The official [release history](https://docs.live2d.com/en/cubism-editor-manual/new-feature-introduction-title/)
lists 5.3.04 on 2026-09-03 and 5.3.05 beta1 on 2026-10-13. The
[5.4 alpha announcement](https://www.live2d.com/en/information/cubism-5_4-alpha/)
states that downloading and using the alpha ended on 2026-10-18 at 23:59 JST.
An installed alpha is therefore not a usable reference for a new run. The target
above remains unchanged. Public Issue [#1](https://github.com/geekjapan/Soul2D/issues/1)
tracks successor features; prerelease descriptions do not establish implemented
or accepted Soul2D behavior.

- GEO19, [Issue #13](https://github.com/geekjapan/Soul2D/issues/13). The official
  [Deform Paths manual](https://docs.live2d.com/en/cubism-editor-manual/deformpath/)
  defines control paths on ArtMeshes. Editing path points and deforming the mesh
  are different modes. The remaining definition work is the source representation
  and deformation calculation, with a legitimate minimal example. An INX
  PathDeformer or an ArtPath is not a substitute.
- PAR06/PAR07, [Issue #14](https://github.com/geekjapan/Soul2D/issues/14). The
  [interpolation-type manual](https://docs.live2d.com/en/cubism-editor-manual/extended-type/)
  describes controls and trajectories but supplies no complete construction
  algorithm. The inspected official sources do not satisfy the published-formula
  prerequisite. This is not proof that no specification exists elsewhere.
  Unsupported evaluation stays refused.
- PAR20, [Issue #15](https://github.com/geekjapan/Soul2D/issues/15). The official
  [Motion Mirroring manual](https://docs.live2d.com/en/cubism-editor-manual/motion-mirroring/)
  describes baking opposite-side keyforms around the parameter default, rejecting
  the default source form, selectable ArtMesh axes, and parent-first processing.
  It does not settle asymmetric/even key pairing or the exact nested-space
  transformation. A three-key, single-deformer observation is the next bounded
  definition check; these notes are not implementation or vendor acceptance.
