# The `.moc3` layout Soul2D reads

Soul2D reads Cubism runtime models (`.moc3`) with a clean-room reader (`rust/src/moc3.rs`, ADR 0004 point 5). Every
fact below was derived from the four sample models in the private verification set (versions 1, 3, 5 and 6) by
cross-checking each section against the CMO3 the model was exported from. Version 2 was added later from five
runtime models of one official sample: they store exactly the slots version 3 files store (the same 102 sections, slot
101 included, and the same zero slots), and Cubism Core's geometry for them is the comparison. Nothing was taken from
vendor SDK source or documentation. A section that the samples did not let the reader confirm is listed as unread, and
`live2d.model.attach` reports it in `warnings`.

Numbers are little-endian. "i32" and "f32" are 4-byte values; an "id" is 64 bytes, NUL padded.

## File

| Offset | Content |
| --- | --- |
| 0 | `MOC3`, then the version byte (1, 2, 3, 5 or 6), then zeros to 0x40 |
| 0x40 | u32 offsets, one per slot (480 in every sample); the first offset is the counts table; a slot is 0 when the file has no such section |
| sections | 64-byte aligned, in slot order; a trailing empty section may sit at the file end |

## Counts table (slot 0, u32 each)

| Entry | Count of | Entry | Count of |
| --- | --- | --- | --- |
| 0 | Parts | 11 | parameter bindings |
| 1 | deformers | 12 | keyform bindings |
| 2 | warp deformers | 13 | key tables |
| 3 | rotation deformers | 14 | keys |
| 4 | drawables | 15 | UV floats |
| 5 | parameters | 16 | triangle indices |
| 6 | Part keyforms | 17 | mask references |
| | | 19 | draw-sequence units: drawables plus draw-order-group Parts (Natori 178 for 176 drawables, Ren 226 for 198) |
| 7 | warp keyforms | 20 | Glues |
| 8 | rotation keyforms | 21 | Glue records (one bound vertex each) |
| 9 | drawable keyforms | 23 | version 5 and 6: colour keyforms (warp, rotation, drawable and offscreen keyforms together; only the drawables' grid entries are read, Mao's blend-shape entries hold differences below zero) |
| 10 | keyform floats | 25, 26 | version 5 and 6: blend-shape entries; blend-shape bindings |
| | | 27, 28, 32, 33 | version 5 and 6: warp, drawable, Part and rotation blend-shape sources |
| | | 29, 30, 31 | version 5 and 6: constraint group references; constraint groups; constraint points |
| | | 35, 36 | version 6: offscreen Parts; their keyforms |

Versions 1 to 3 stop the table before entry 25; the reader bounds the table by its section size.

Parameter ranges, defaults and keys are read as the shortest decimal with the same single-precision value (Natori's eye
range ends at 1.3, not 1.2999999523), the number the CMO3 held.

## Slots

| Slot | Per | Content |
| --- | --- | --- |
| 1 | file | f32 ×5: pixels per unit, origin x, origin y, width, height |
| 3 | Part | id |
| 4, 5, 6 | Part | keyform binding; keyform begin and count |
| 7, 8 | Part | 1 in every sample |
| 9 | Part | parent Part, -1 under the root |
| 11 | deformer | id |
| 13, 14 | deformer | 1 in every sample |
| 15 | deformer | parent Part |
| 16 | deformer | parent deformer, -1 at the root |
| 17 | deformer | type: 0 warp, 1 rotation |
| 18 | deformer | index within its type |
| 19, 20, 21 | warp | keyform binding; keyform begin and count |
| 22, 23, 24 | warp | vertices = (rows + 1)(columns + 1); rows; columns |
| 25, 26, 27 | rotation | keyform binding; keyform begin and count |
| 28 | rotation | f32 base angle (degrees) |
| 33 | drawable | id |
| 34, 35, 36 | drawable | keyform binding; keyform begin and count |
| 37, 38 | drawable | 1 in every sample |
| 39, 40 | drawable | parent Part; parent deformer, -1 at the root |
| 41 | drawable | texture index into the `.model3.json` texture list |
| 42 | drawable | flag byte: 4 always; 8 inverted mask; before version 6, 1 additive and 2 multiplicative blend |
| 43, 44 | drawable | vertex count; UV float begin (slot 78) |
| 45, 46 | drawable | index begin (slot 79) and count |
| 47, 48 | drawable | mask begin (slot 80) and count |
| 50 | parameter | id |
| 51, 52, 53, 54 | parameter | f32 maximum, minimum, default; repeat flag |
| 56, 57 | parameter | key table begin and count (-1, 0 for a parameter without key tables) |
| 58 | Part keyform | f32 draw order |
| 59, 60 | warp keyform | f32 opacity; float begin of the grid positions in slot 71 |
| 61, 62 | rotation keyform | f32 opacity; angle (degrees) |
| 63, 64, 65 | rotation keyform | origin x, y; scale |
| 66, 67 | rotation keyform | reflect x, y (nonzero = reflected) |
| 68, 69, 70 | drawable keyform | f32 opacity; draw order; float begin of the positions in slot 71 |
| 71 | file | f32 keyform floats |
| 72 | parameter binding | key table index |
| 73, 74 | keyform binding | parameter binding begin and count |
| 75, 76 | key table | key begin and count (slot 77) |
| 77 | file | f32 keys, ascending within a table |
| 78 | file | f32 UVs, one pair per vertex, drawables concatenated |
| 79 | file | u16 triangle indices |
| 80 | file | i32 mask drawable indices |
| 86, 87 | draw unit | draw sequence over counts entry 19 units: kind 0 drawable, 1 draw-order-group Part, in every version (Natori, version 1, has two Part units); unit index, bottom first (slot 88 numbers the Part units, unread) |
| 89 | Glue | 8 bytes, zero in every sample (unread) |
| 90 | Glue | id |
| 91, 92, 93 | Glue | keyform binding; keyform begin and count |
| 94, 95 | Glue | drawable A; drawable B |
| 96, 97 | Glue | record begin and count |
| 98 | Glue record | f32 weight |
| 99 | Glue record | u16 vertex index, even records in drawable A, odd records in drawable B |
| 100 | Glue keyform | f32 intensity |
| 101 | warp | quad transform flag; Rice (version 3) sets it on 50 deformers, so it is read whenever the slot has an offset |
| 107 | drawable | version 5 and 6: colour keyform begin of the drawable's grid keyforms |
| 108, 109, 110 | colour keyform | version 5 and 6: f32 multiply red, green, blue |
| 111, 112, 113 | colour keyform | version 5 and 6: f32 screen red, green, blue |
| 114 | parameter | version 5 and 6: 1 for a blend-shape parameter |
| 115, 116 | parameter | version 5 and 6: blend-shape entry begin and count (-1, 0 without) |
| 117, 118 | blend-shape entry | key begin and count (slot 77) |
| 119 | blend-shape entry | index of key 0 within the entry (unread) |
| 120 | blend-shape binding | entry |
| 121, 122 | blend-shape binding | keyform begin and count, after the grid keyforms of the source's type |
| 123, 124 | blend-shape binding | constraint group reference begin and count (slot 131) |
| 125, 126, 127 | warp blend source | warp index; binding begin and count |
| 128, 129, 130 | drawable blend source | drawable index; binding begin and count |
| 131 | constraint group reference | constraint group |
| 132 | constraint group | constraining parameter |
| 133, 134 | constraint group | point begin and count |
| 135, 136 | constraint point | f32 parameter value; blend weight |
| 143, 144, 145 | Part blend source | Part index; binding begin and count (Mao's PartRabbit: a draw-order difference of -900; zundamon_ja and ren_ja have no rows, so these slots share the offset of slot 146 there) |
| 146, 147, 148 | rotation blend source | rotation index; binding begin and count (slot 146, not 143: Cubism Core moves Mao's Rotation3 about its own origin) |
| 153 | drawable | version 6: blend word, colour composition in the low byte (0 NORMAL, 4 ADD_R2, 6 MULTIPLY_R2, 14 HARDLIGHT, 17 HSL_COLOR), alpha composition above (0 OVER, 1 ATOP, 2 OUT) |
| 152 | Part | version 6: offscreen index, -1 without |
| 155 | offscreen Part | version 6: Part index |
| 156 | offscreen Part | version 6: flag byte as a drawable's (8 inverted mask) |
| 157 | offscreen Part | version 6: blend word as slot 153, alpha compositions 3 CONJOINT and 4 DISJOINT included |
| 158, 159 | offscreen Part | version 6: mask begin (slot 80) and count |
| 160, 162, 163 | offscreen Part | version 6: unread |
| 161 | offscreen Part keyform | version 6: f32 opacity, the Parts' keyforms in list order |

## Keyforms

A keyform binding lists the parameters a source depends on; the keys of each come from the parameter's key table.
A source stores one keyform per combination, the first parameter varying fastest.

Coordinates: a source whose target deformer is the root stores normalized canvas coordinates, pixel = value ×
pixels per unit + origin. A source inside a deformer stores that deformer's local coordinates as they are. A
rotation deformer stores its scale divided by the pixels per unit unless a rotation deformer lies above it. Part
keyform opacity is stored only for offscreen Parts (slot 161).

## Blend shapes (versions 5 and 6)

A blend-shape parameter owns a run of entries (slots 115, 116), each a key list in slot 77; zundamon_ja's
ParamArmCross owns two (`[0, 1]` and `[-1, 0, 1]`). A binding (slots 120 to 124) ties one entry to a run of keyforms
stored after the grid keyforms of the same object type, one per key, holding the difference from the source's
default-cell keyform: positions and rotation origins as root-level normalized differences (× pixels per unit, no
origin), opacity, draw order, angle and scale as differences. The Cubism Editor's insert points become extra keys. A
binding's constraint groups (slot 131 into slots 132 to 136) limit its weight by another parameter's value. Soul2D
projects each nonzero key to a morph target Form and each point to a blend weight constraint; the CMO3 of both
samples matched exactly (118 of 118 constraints on zundamon_ja).

## Draw sequence

Slot 87 lists the drawables bottom first, in the CMO3 traversal order (Soul2D's evaluation order), which is the
reverse of each Part's `childGuids`. Soul2D sorts siblings by their first place in the sequence, topmost first;
hiyori_free, hiyori_pro and zundamon_ja reproduce their CMO3 exactly. Version 6 adds the draw-order-group Parts as
units of kind 1 (slot 86); ren_ja leaves four drawables under offscreen Parts out of the sequence, which stay above
the listed ones in index order.

## Glue

A Glue names two drawables and a run of records; the records alternate between a vertex of drawable A and a
vertex of drawable B, each with its weight, and the Glue's keyforms carry its intensity (one keyform without
parameters in every sample). That is the CMO3 `bindVertexUids` and `weights` order exactly; the 53 Glues of the
four samples matched. Runtime drawables get vertex UIDs equal to their vertex indices plus one so the Glue binds.
Core applies the Glues in file order (zundamon_ja binds one vertex to two); the native evaluator applies Glue
sources from the last to the first, so the runtime scene lists them in reverse. With that, all four runtime models
match a local Cubism Core 6.0.1 capture within 0.012 px (`tools/compare-cubism-core.py --runtime`), and five of
the eight runtime-only Cubism SDK sample models within 0.02 px; `doc/live2d-authoring.md` lists the residuals of
the other three.

## Offscreen Parts (version 6)

Slot 155 lists the Parts the CMO3 marks `useOffscreen`; each composes with a blend word and flag byte like a
drawable's, a mask run in slot 80 (the mask list starts with the Part masks) and an opacity per Part keyform, Parts
in list order. ren_ja's 24 offscreen Parts, their masks, blends and opacities matched its CMO3.

## Attachment refusal checks

Runtime attachment accepts at most 16,384 texture slots, 128 MiB of compressed data per slot, and 512 MiB of
compressed data across slots. Before texture decoding, it checks the PNG header against the existing image
limits of 8192 pixels per side and 16,777,216 pixels per image. A separate 512 MiB allowance bounds the sum of
`width × height × 4` across every listed slot. Repeated paths count separately because each slot is decoded
separately. Both aggregate allowances accept their exact boundary.

The RGBA allowance bounds declared decoded workload. It does not bound peak process memory, retained compressed
data, the previous attachment, decoder and encoder temporaries, or later rendering allocations. PNG headers that
pass preflight still require successful decoding.

The synthetic Session test `refused_runtime_attachments_preserve_the_previous_model_and_session`
attaches an owned runtime model and changes its preview parameter. It then refuses missing Moc or texture files,
escaping references, an empty texture list, invalid PNG data, excessive texture slots, oversized dimensions, and
excess aggregate RGBA workload. Each refusal preserves
the complete snapshot, evaluated geometry, CPU-rendered PNG and input files. Reattaching the valid model succeeds
and resets its preview values without editing the CAN3 document. These checks do not establish native-window
appearance or vendor rendering parity.

`runtime_texture_rgba_budget_and_dimensions_preflight_each_slot` checks independent compressed and RGBA
allowances with small valid PNGs. Header-only fixtures exercise dimension boundaries and the aggregate RGBA
refusal before decoding, without allocating large images. They do not prove successful decoding at maximum size.
