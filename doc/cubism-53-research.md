# Cubism 5.3 Pro reference research

Status: **PRIMARY SOURCES READ; NO VENDOR ACCEPTANCE RUN**. Retrieved on 2026-10-10.
The full target remains Cubism 5.3 Pro, all features. This note is an initial
source inventory, not proof that Soul2D implements those features or that the
inventory is exhaustive.

## Reference releases selected

The official 5.3 update history and release list both record Cubism Editor
5.3.04 as a stable release dated 2026-09-03. It is the newest stable 5.3 release
listed in the captured pages. Select **Cubism Editor 5.3.04 Pro** as the
reference release. Beta and 5.4 alpha announcements do not change this target.
The actual executable build metadata, SHA-256, licensed edition, and installed
platform remain unpinned because the reference environment is not set up. [S2][S3]

The SDK manual announces **Cubism 5 SDK for Native R5** on 2026-04-02. Select
that official SDK family for the macOS runtime-reference plan, not a beta SDK.
This does not establish a numeric Core version or header/library compatibility;
those must be recorded from the legitimate matching artifacts before execution.
Editor, SDK release, and Core version are separate identifiers. [S9][S10]

## Feature inventory seeded from official sources

The Pro/Free comparison is an edition comparison, not a complete versioned
Editor specification. The manual index includes related products and current
content, and displays a 5.4 alpha announcement. Reconcile each feature with the
selected 5.3.04 release before treating these sources as a frozen feature list.
The following groups are open work, not successful acceptance. [S1][S2][S5]

| Group | Source-backed inventory seed | Source |
| --- | --- | --- |
| Capacity and interoperability | Textures, ArtMeshes, parameters, blend-shape parameters, deformers, parts, art paths, warp subdivisions, Pro/Free data interoperability | S1 |
| Source import and textures | PSD import/reimport, source-image links, texture atlas editing, PNG image replacement | S5 |
| Hierarchy and rendering | Draw-order groups, clipping and inverted masks, color multiplication/screen, blend modes, offscreen parts | S1, S4, S5 |
| Geometry authoring | Automatic/manual mesh editing, mesh copy/paste, art-path creation/brushes, warp/rotation creation, parent-child operations, deformer validation, model resizing | S1, S5 |
| Keyforms and rigging | Form copy/paste/blend, align parameter structures, apply deformers to children, special/reflected pastes, generated corner/face/sway forms, glue, paths, skinning, multi-key edits, temporary deformation | S1, S5 |
| Parameters and morphs | Key addition/removal, defaults, repeat, extended interpolation, blend shapes and constraints, parameter recording, MotionSync, 3D rotation expressions | S1, S5 |
| GUI authoring | Menus, palettes, views, multi-view, Solo/focus, grid/guides, snapshots, onion skin, mesh presets, form locks, backups, preferences, shortcuts | S1, S5 |
| Bulk tooling | ID/name CSV export/import, find/replace, templates and parameter bookkeeping | S1, S2, S5 |
| Animation projects | Model/background/audio tracks, scenes, timeline/dope sheet/graph editor, bookmarks, markers, fade, lightweight playback, parameter controllers, loops | S5 |
| Animation deformation and timing | Form animation for all object types, exposure timing, image-sequence tracks and time remapping, physics/MotionSync baking | S1, S5 |
| Exports and assets | Image sequences, GIF, MP4/WebM/MOV, audio-codec options, linked-asset collection, embedded runtime-model tracks, embedded data and user events | S1, S4, S5 |
| Physics and external interfaces | Physics group configuration, presets/playlists, external-application API | S2, S5 |
| 5.3 additions and cumulative changes | New blend/offscreen operations, square/line/inflation brushes, warp resizing aids, reflected rotation positions, transparent WebM, parameter/mesh/physics changes through 5.3.04 | S2, S4 |
| Related-product classification | Viewer workflows, Photoshop helpers, AE plugin, validation tools referenced by the manual need an explicit product-boundary classification | S5 |

A grouped row is not one completed feature. Expand it into individual operations,
source references, version evidence, implementation owners, dedicated fixtures,
comparison procedures, and outcomes. Features absent from the four supplied
models still require probes. A manual page concerning another product or a
later release is a classification question, not permission to silently reduce
the full Pro target.

## Editor and runtime evidence are different

The SDK feature and compatibility pages, updated 2026-01-29, explicitly warn
that they include beta-version information. Their tables list parameter
controllers as unsupported and identify editing features such as loop-edit aids,
brushes, deformer restoration/resizing, and WebM output as Editor operations.
These older tables do not prove the complete support status of an installed
R5 official SDK. Use Editor reference workflows for Editor operations and check
the pinned official SDK separately. [S7][S8]

The official R5 compatibility page, updated 2026-04-02, documents changes in
parameter-update ownership/order and color APIs. It also notes that Unity
physics-related parameter results differ from the beta SDK. Do not mix beta,
Unity, and Native outputs as interchangeable references. [S9]

The beta-focused 5.3 compatibility documentation reports that older R4 SDKs
cannot read 5.3-targeted MOC files, that render-order APIs changed from
`csmGetDrawableRenderOrders` to `csmGetRenderOrders`, and that advanced drawing
requires framework changes, not only replacing Core or shaders. Treat these
as integration checks against the matching official R5 header/framework, not
as proof that the existing unverified capture preparation works. [S7][S8]

## Rendering reference requirements

The 5.3 feature introduction describes 15 color-blend modes and five alpha-blend
modes, plus offscreen rendering that gives parts opacity and clipping behavior.
The reference must cover their individual and combined behavior, not only the
four existing models' default pose. [S4]

The model-production notes distinguish legacy blend modes from advanced modes
and explain the rendering/memory cost of offscreen structures. Reference image
comparisons must pin the rendering backend, mask processing, render-target
sizes, hierarchy, draw order, alpha convention, and canvas scale. A raw Core
geometry capture is not a reference rasterizer. [S6][S8]

## Capture and reproducibility

The approved retrieval captured only public HTML from `www.live2d.com` and
`docs.live2d.com`. It downloaded no SDK, executable, header, library, model,
or license activation material. No vendor software was installed or executed.

The earlier captures recorded requested and final URLs, retrieval time, HTTP
status, and SHA-256 of each raw HTML page. They, and the fresh Cloud public-HTML
reread of 2026-10-10, are local evidence and are not part of this repository.
The [operation ledger](cubism-feature-ledger.md)
records the new expansion and the still-open mode-count/version reconciliation.
`tools/capture-live2d-docs.py` reproduces a new dated capture without overwriting
an existing one; public pages can change, so a new capture is new evidence.
`python3 tools/test-capture-live2d-docs.py` validates the isolated HTML capture,
refused URLs and redirects, and rejected binary bodies without vendor or network
execution; it is not a live vendor acceptance.

The initial source capture used an earlier script, before the reproduction
helper was hardened. New captures identify the producing tool hash and exact-URL HTML
policy in their receipts.

## Remaining phase 1 gates

The reference release choices do not complete phase 1. Still required are the
per-feature inventory reconciliation, module-level D-to-Rust migration map,
full-feature conversion mapping, and reviewed comparison procedures with literal
pre-run tolerances. No numerical tolerances are frozen by this research.

The licensed Editor and matching Native R5/Core environment are not set up.
Acquisition, installation, activation, license acceptance, and spending remain
separate approval gates. The overall goal and every phase remain incomplete.

## Primary sources

- [S1: Pro/Free feature comparison](https://www.live2d.com/cubism/comparison/)
- [S2: Editor 5.3 update history](https://docs.live2d.com/cubism-editor-manual/updates-history5-3/)
- [S3: Editor release history](https://docs.live2d.com/cubism-editor-manual/new-feature-introduction-title/)
- [S4: Editor 5.3 new features](https://docs.live2d.com/cubism-editor-manual/new-function5-3/)
- [S5: Editor manual index](https://docs.live2d.com/cubism-editor-manual/top/)
- [S6: Model-production notes for 5.3](https://docs.live2d.com/cubism-editor-manual/cubism5-3-notes/)
- [S7: SDK support for 5.3 new features, beta caveat](https://docs.live2d.com/cubism-sdk-manual/cubism-5-3-new-functions/)
- [S8: SDK 5.3 compatibility and limits, beta caveat](https://docs.live2d.com/cubism-sdk-manual/compatibility-with-cubism-5-3/)
- [S9: Official R5 SDK compatibility](https://docs.live2d.com/cubism-sdk-manual/compatibility-with-cubism-5-3-official/)
- [S10: SDK manual release announcements](https://docs.live2d.com/cubism-sdk-manual/top/)
