//! A clean-room reader for Live2D runtime models (`.moc3` and `.model3.json`), issue #228 and ADR 0004 point 5.
//! The layout was derived only from the structure of local sample files, cross-checked against their sibling JSON
//! files: a 64-byte header (`MOC3`, a version byte, a zero byte for little-endian), a table of u32 section offsets
//! at 0x40, a table of u32 counts at the first offset, 64-byte NUL-padded ID slots and per-object arrays. Only the
//! sections confirmed on every sample are read; anything else, an unseen version included, is refused.

use crate::authoring_keyforms::{KeyformAxis, KeyformCell, KeyformGrid};
use crate::authoring_scene::{
    BlendWeightConstraint, Form, FormData, MorphTarget, MorphTargets, Parameter, ParsedScene, Scene, Source, SourceData, Texture,
};
use crate::native_scene::NativeScene;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) const MAX_MOC3_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MODEL3_BYTES: u64 = 4 * 1024 * 1024;
/// Versions seen in the samples. The offset table only grows between them, so the slots read here mean the same;
/// version 2 files store exactly the slots version 3 files do.
const VERSIONS: [u8; 5] = [1, 2, 3, 5, 6];
const TABLE: usize = 0x40;
const ID_BYTES: usize = 64;

// Offset-table slots.
const COUNTS: usize = 0;
const CANVAS: usize = 1;
const PART_IDS: usize = 3;
const DEFORMER_IDS: usize = 11;
const MESH_IDS: usize = 33;
const MESH_PARENT_PART: usize = 39;
const MESH_TEXTURE: usize = 41;
const MESH_VERTICES: usize = 43;
const MESH_INDICES: usize = 46;
const PARAMETER_IDS: usize = 50;
const PARAMETER_MAX: usize = 51;
const PARAMETER_MIN: usize = 52;
const PARAMETER_DEFAULT: usize = 53;
// Keyform slots, each confirmed against the sibling CMO3 of the four local samples (`doc/moc3-layout.md`).
const PART_BINDING: usize = 4;
const PART_KEYFORM_BEGIN: usize = 5;
const PART_KEYFORM_COUNT: usize = 6;
const PART_PARENT: usize = 9;
const DEFORMER_PARENT_PART: usize = 15;
const DEFORMER_PARENT: usize = 16;
const DEFORMER_TYPE: usize = 17;
const DEFORMER_INDEX: usize = 18;
const WARP_BINDING: usize = 19;
const WARP_KEYFORM_BEGIN: usize = 20;
const WARP_KEYFORM_COUNT: usize = 21;
const WARP_VERTICES: usize = 22;
const WARP_ROWS: usize = 23;
const WARP_COLUMNS: usize = 24;
const ROTATION_BINDING: usize = 25;
const ROTATION_KEYFORM_BEGIN: usize = 26;
const ROTATION_KEYFORM_COUNT: usize = 27;
const MESH_BINDING: usize = 34;
const MESH_KEYFORM_BEGIN: usize = 35;
const MESH_KEYFORM_COUNT: usize = 36;
const MESH_PARENT_DEFORMER: usize = 40;
const MESH_FLAGS: usize = 42;
const MESH_VERTEX_BEGIN: usize = 44;
const MESH_INDEX_BEGIN: usize = 45;
const MESH_MASK_BEGIN: usize = 47;
const MESH_MASK_COUNT: usize = 48;
const PARAMETER_REPEAT: usize = 54;
const PARAMETER_KEYTABLE_BEGIN: usize = 56;
const PARAMETER_KEYTABLE_COUNT: usize = 57;
const PART_KEYFORM_DRAW_ORDER: usize = 58;
const WARP_KEYFORM_OPACITY: usize = 59;
const WARP_KEYFORM_POSITION: usize = 60;
const ROTATION_KEYFORM_OPACITY: usize = 61;
const ROTATION_KEYFORM_ANGLE: usize = 62;
const ROTATION_KEYFORM_ORIGIN_X: usize = 63;
const ROTATION_KEYFORM_ORIGIN_Y: usize = 64;
const ROTATION_KEYFORM_SCALE: usize = 65;
const ROTATION_KEYFORM_REFLECT_X: usize = 66;
const ROTATION_KEYFORM_REFLECT_Y: usize = 67;
const MESH_KEYFORM_OPACITY: usize = 68;
const MESH_KEYFORM_DRAW_ORDER: usize = 69;
const MESH_KEYFORM_POSITION: usize = 70;
const KEYFORM_FLOATS: usize = 71;
const PARAMETER_BINDING_KEYTABLE: usize = 72;
const KEYFORM_BINDING_BEGIN: usize = 73;
const KEYFORM_BINDING_COUNT: usize = 74;
const KEYTABLE_KEY_BEGIN: usize = 75;
const KEYTABLE_KEY_COUNT: usize = 76;
const KEYS: usize = 77;
const UVS: usize = 78;
const INDICES: usize = 79;
const MASKS: usize = 80;
// Glue: each record binds one vertex, the records of a Glue alternating between its drawable A and drawable B,
// with a weight per record and an intensity per Glue keyform; the 53 Glues of the four samples matched their CMO3.
const GLUE_IDS: usize = 90;
const GLUE_BINDING: usize = 91;
const GLUE_KEYFORM_BEGIN: usize = 92;
const GLUE_KEYFORM_COUNT: usize = 93;
const GLUE_MESH_A: usize = 94;
const GLUE_MESH_B: usize = 95;
const GLUE_RECORD_BEGIN: usize = 96;
const GLUE_RECORD_COUNT: usize = 97;
const GLUE_WEIGHTS: usize = 98;
const GLUE_VERTICES: usize = 99;
const GLUE_KEYFORM_INTENSITY: usize = 100;
// The draw sequence: the drawables in the order the CMO3 lists them, bottom first, which breaks draw-order ties.
// hiyori_free, hiyori_pro and zundamon_ja (versions 1, 3 and 5) reproduce it exactly. Draw-order-group Parts are
// units of their own, flagged 1 in the kinds table, in every version (Natori, version 1, lists two); counts entry
// `DRAW_UNITS` holds the unit total, which ren_ja's four display drawables at the end of the sequence need.
const DRAW_UNIT_KINDS: usize = 86;
const DRAW_SEQUENCE: usize = 87;
/// The warp deformers' quad-transform flag; Rice (version 3) stores it for 50 of its deformers.
const ROTATION_BASE_ANGLE: usize = 28;
const WARP_QUAD_TRANSFORM: usize = 101;
// Keyform colours, version 5 and later: six arrays over the grid keyforms of every object type, the multiply
// RGB then the screen RGB, with each drawable's run starting at `MESH_COLOR_BEGIN`; ren_ja's two tinted
// drawables confirmed the mapping, zundamon_ja's runs are all white and black.
const MESH_COLOR_BEGIN: usize = 107;
const KEYFORM_MULTIPLY_RED: usize = 108;
const KEYFORM_SCREEN_RED: usize = 111;
/// Version 6: a word per drawable holding the colour composition in its low byte and the alpha composition above.
const MESH_BLEND_WORDS: usize = 153;
// Offscreen Parts, version 6: the Parts the runtime composes through an offscreen surface, each with a flag
// byte, a blend word like a drawable's, a mask run in `MASKS` and a keyform opacity per Part keyform, laid out
// in list order; ren_ja confirmed every table against its CMO3.
const OFFSCREEN_PART_LIST: usize = 155;
const OFFSCREEN_FLAGS: usize = 156;
const OFFSCREEN_BLEND_WORDS: usize = 157;
const OFFSCREEN_MASK_BEGIN: usize = 158;
const OFFSCREEN_MASK_COUNT: usize = 159;
const OFFSCREEN_KEYFORM_OPACITY: usize = 161;
// Blend shapes, version 5 and later; ren_ja (version 6) and zundamon_ja (version 5) confirmed every table below
// against their CMO3 morph targets. A blend-shape parameter owns a run of entries, each a key list in `KEYS`
// (zundamon's ParamArmCross has two: `[0, 1]` and `[-1, 0, 1]`); a binding ties one entry to a run of keyforms,
// stored after the grid keyforms of the same object type, that hold the difference from the object's
// default-cell keyform. Constraints limit a binding's weight by another parameter.
const PARAMETER_IS_BLEND_SHAPE: usize = 114;
const PARAMETER_BLEND_ENTRY_BEGIN: usize = 115;
const PARAMETER_BLEND_ENTRY_COUNT: usize = 116;
const BLEND_SHAPE_KEY_BEGIN: usize = 117;
const BLEND_SHAPE_KEY_COUNT: usize = 118;
const BLEND_BINDING_PARAMETER: usize = 120;
const BLEND_BINDING_KEYFORM_BEGIN: usize = 121;
const BLEND_BINDING_KEYFORM_COUNT: usize = 122;
const BLEND_BINDING_CONSTRAINT_BEGIN: usize = 123;
const BLEND_BINDING_CONSTRAINT_COUNT: usize = 124;
const WARP_BLEND_SOURCE: usize = 125;
const WARP_BLEND_BINDING_BEGIN: usize = 126;
const WARP_BLEND_BINDING_COUNT: usize = 127;
const MESH_BLEND_SOURCE: usize = 128;
const MESH_BLEND_BINDING_BEGIN: usize = 129;
const MESH_BLEND_BINDING_COUNT: usize = 130;
const CONSTRAINT_GROUP_REFS: usize = 131;
const CONSTRAINT_GROUP_PARAMETER: usize = 132;
const CONSTRAINT_POINT_BEGIN: usize = 133;
const CONSTRAINT_POINT_COUNT: usize = 134;
const CONSTRAINT_POINT_VALUE: usize = 135;
const CONSTRAINT_POINT_WEIGHT: usize = 136;
// Part blend-shape sources (Mao's PartRabbit: a draw-order difference of -900 on ParamRabbitDraworder); zundamon_ja
// and ren_ja have none, so there these three slots share the offset of slot 146.
const PART_BLEND_SOURCE: usize = 143;
const PART_BLEND_BINDING_BEGIN: usize = 144;
const PART_BLEND_BINDING_COUNT: usize = 145;
// Slot 146 names the rotation deformer a blend-shape binding run belongs to: Mao's rows name Rotation3, Rotation50
// and a third deformer, and Cubism Core moves Rotation3's fifteen drawables about its origin.
const ROTATION_BLEND_SOURCE: usize = 146;
const ROTATION_BLEND_BINDING_BEGIN: usize = 147;
const ROTATION_BLEND_BINDING_COUNT: usize = 148;
/// The slots the base layout needs; a shorter offset table is refused. Later slots count as absent when the table
/// stops before them.
const SLOTS_REQUIRED: usize = 102;
const SLOTS_READ: usize = 162;

// Counts-table entries.
const PARTS: usize = 0;
const DEFORMERS: usize = 1;
const WARP_DEFORMERS: usize = 2;
const ROTATION_DEFORMERS: usize = 3;
const MESHES: usize = 4;
const PARAMETERS: usize = 5;
const PART_KEYFORMS: usize = 6;
const WARP_KEYFORMS: usize = 7;
const ROTATION_KEYFORMS: usize = 8;
const MESH_KEYFORMS: usize = 9;
const KEYFORM_FLOAT_COUNT: usize = 10;
const PARAMETER_BINDINGS: usize = 11;
const KEYFORM_BINDINGS: usize = 12;
const KEYTABLES: usize = 13;
const KEY_COUNT: usize = 14;
const DRAW_UNITS: usize = 19;
const VERTEX_FLOATS: usize = 15;
const INDEX_COUNT: usize = 16;
const GLUES: usize = 20;
const GLUE_RECORDS: usize = 21;
const MASK_COUNT: usize = 17;
/// Version 5 and 6: blend shape parameters and (source, parameter) bindings, and version 6 offscreen Parts.
const BLEND_SHAPE_PARAMETERS: usize = 25;
const BLEND_SHAPE_BINDINGS: usize = 26;
const WARP_BLEND_SOURCES: usize = 27;
const MESH_BLEND_SOURCES: usize = 28;
const CONSTRAINT_GROUP_REF_COUNT: usize = 29;
const CONSTRAINT_GROUPS: usize = 30;
const CONSTRAINT_POINTS: usize = 31;
const COLOR_KEYFORMS: usize = 23;
const PART_BLEND_SOURCES: usize = 32;
const ROTATION_BLEND_SOURCES: usize = 33;
const OFFSCREEN_PARTS: usize = 35;
const OFFSCREEN_KEYFORMS: usize = 36;
const MAX_TEXTURE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RUNTIME_TEXTURE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RUNTIME_TEXTURES: usize = 16_384;
const RUNTIME_ROOT_PART: &str = "soul2d-moc3-root";

struct Reader<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
    /// How many entries the counts table holds: version 1 and 3 files stop before the blend shape counts.
    count_entries: usize,
}

impl Reader<'_> {
    fn u32_at(&self, at: usize) -> Result<u32, String> {
        let slice = self.bytes.get(at..at + 4).ok_or("the .moc3 file is truncated")?;
        Ok(u32::from_le_bytes(slice.try_into().map_err(|_| "the .moc3 file is truncated")?))
    }

    fn count(&self, entry: usize) -> Result<usize, String> {
        if entry >= self.count_entries {
            return Err("the .moc3 counts table is shorter than the layout Soul2D reads".into());
        }
        let count = self.u32_at(self.offsets[COUNTS] + 4 * entry)? as usize;
        // Every counted object takes at least four bytes somewhere in the file.
        if count > self.bytes.len() / 4 {
            return Err("a .moc3 count is larger than the file".into());
        }
        Ok(count)
    }

    fn words(&self, slot: usize, count: usize) -> Result<Vec<[u8; 4]>, String> {
        let start = self.offsets[slot];
        if start == 0 && count > 0 {
            return Err("a .moc3 section the file needs has no offset".into());
        }
        let slice = self.bytes.get(start..start + 4 * count).ok_or("a .moc3 section runs past the end of the file")?;
        Ok(slice.chunks_exact(4).map(|word| [word[0], word[1], word[2], word[3]]).collect())
    }

    /// One flag byte per object.
    fn flag_bytes(&self, slot: usize, count: usize) -> Result<&[u8], String> {
        let start = self.offsets[slot];
        if start == 0 && count > 0 {
            return Err("a .moc3 section the file needs has no offset".into());
        }
        self.bytes.get(start..start + count).ok_or_else(|| "a .moc3 flag section runs past the end of the file".into())
    }

    fn floats(&self, slot: usize, count: usize) -> Result<Vec<f64>, String> {
        self.words(slot, count)?.into_iter().map(|word| {
            let value = f32::from_le_bytes(word);
            value.is_finite().then_some(f64::from(value)).ok_or_else(|| "a .moc3 number is not finite".to_owned())
        }).collect()
    }

    /// Parameter ranges and keys as the shortest decimal with the same single-precision value, the number the
    /// CMO3 held: Natori's eye range ends at 1.3, which widening its f32 would put at 1.2999999523.
    fn decimals(&self, slot: usize, count: usize) -> Result<Vec<f64>, String> {
        Ok(self.floats(slot, count)?.into_iter().map(|value| (value as f32).to_string().parse().unwrap_or(value)).collect())
    }

    fn ints(&self, slot: usize, count: usize) -> Result<Vec<i32>, String> {
        Ok(self.words(slot, count)?.into_iter().map(i32::from_le_bytes).collect())
    }

    fn u16s(&self, slot: usize, count: usize) -> Result<Vec<u16>, String> {
        let start = self.offsets[slot];
        if start == 0 && count > 0 {
            return Err("a .moc3 section the file needs has no offset".into());
        }
        let slice = self.bytes.get(start..start + 2 * count).ok_or("a .moc3 section runs past the end of the file")?;
        Ok(slice.chunks_exact(2).map(|word| u16::from_le_bytes([word[0], word[1]])).collect())
    }

    fn ids(&self, slot: usize, count: usize) -> Result<Vec<String>, String> {
        let start = self.offsets[slot];
        if start == 0 && count > 0 {
            return Err("a .moc3 section the file needs has no offset".into());
        }
        let slice = self.bytes.get(start..start + ID_BYTES * count).ok_or("a .moc3 ID table runs past the end of the file")?;
        slice.chunks_exact(ID_BYTES).map(|id| {
            let end = id.iter().position(|byte| *byte == 0).filter(|end| *end > 0)
                .ok_or("a .moc3 ID is empty or not NUL-terminated")?;
            if id[end..].iter().any(|byte| *byte != 0) {
                return Err("a .moc3 ID slot is not zero-padded".to_owned());
            }
            String::from_utf8(id[..end].to_vec()).map_err(|_| "a .moc3 ID is not UTF-8".to_owned())
        }).collect()
    }
}

struct Header {
    version: u8,
    parts: usize,
    deformers: usize,
    warps: usize,
    rotations: usize,
    meshes: usize,
    parameters: usize,
}

fn open(bytes: &[u8]) -> Result<(Reader<'_>, Header), String> {
    if bytes.get(..4) != Some(b"MOC3") {
        return Err("not a .moc3 file".into());
    }
    let version = bytes.get(4).copied().ok_or("the .moc3 file is truncated")?;
    if !VERSIONS.contains(&version) {
        return Err(format!(".moc3 version {version} is not one Soul2D reads (it reads {VERSIONS:?})"));
    }
    if bytes.get(5) != Some(&0) || bytes.get(6..TABLE).is_none_or(|header| header.iter().any(|byte| *byte != 0)) {
        return Err("the .moc3 header is not the little-endian layout Soul2D reads".into());
    }
    let first = u32::from_le_bytes(bytes.get(TABLE..TABLE + 4).ok_or("the .moc3 file is truncated")?
        .try_into().map_err(|_| "the .moc3 file is truncated")?) as usize;
    if first < TABLE + 4 * SLOTS_REQUIRED || first > bytes.len() || (first - TABLE) % 4 != 0 {
        return Err("the .moc3 offset table is not the layout Soul2D reads".into());
    }
    let mut reader = Reader { bytes, offsets: Vec::new(), count_entries: 0 };
    let slots = ((first - TABLE) / 4).min(SLOTS_READ);
    reader.offsets = (0..SLOTS_READ).map(|slot| if slot < slots { reader.u32_at(TABLE + 4 * slot).map(|offset| offset as usize) } else { Ok(0) })
        .collect::<Result<_, _>>()?;
    // A slot is zero when the file stores no section for it (no masks, or a slot a later version added).
    if reader.offsets.iter().any(|offset| *offset != 0 && (*offset < first || *offset > bytes.len())) {
        return Err("a .moc3 section offset is outside the file".into());
    }
    // The counts table runs to the next section; the table of slots read here always lists that section.
    let next = reader.offsets.iter().copied().filter(|offset| *offset > first).min().unwrap_or(bytes.len());
    reader.count_entries = (next - first) / 4;
    let [parts, deformers, warps, rotations, meshes, parameters] =
        [PARTS, DEFORMERS, WARP_DEFORMERS, ROTATION_DEFORMERS, MESHES, PARAMETERS].map(|entry| reader.count(entry));
    let (parts, deformers, warps, rotations, meshes, parameters) =
        (parts?, deformers?, warps?, rotations?, meshes?, parameters?);
    if warps + rotations != deformers {
        return Err("the .moc3 deformer counts do not add up".into());
    }
    Ok((reader, Header { version, parts, deformers, warps, rotations, meshes, parameters }))
}

/// The parts, deformer counts, parameters, drawables and canvas of a `.moc3` file.
pub(crate) fn read(bytes: &[u8]) -> Result<Value, String> {
    let (reader, header) = open(bytes)?;
    let Header { version, parts, deformers, warps, rotations, meshes, parameters } = header;
    let canvas = reader.floats(CANVAS, 5)?;
    let part_ids = reader.ids(PART_IDS, parts)?;
    let deformer_ids = reader.ids(DEFORMER_IDS, deformers)?;
    let parameter_ids = reader.ids(PARAMETER_IDS, parameters)?;
    let (max, min, default) = (reader.decimals(PARAMETER_MAX, parameters)?, reader.decimals(PARAMETER_MIN, parameters)?,
        reader.decimals(PARAMETER_DEFAULT, parameters)?);
    let mut parameter_list = Vec::with_capacity(parameters);
    for (index, id) in parameter_ids.iter().enumerate() {
        if !(min[index] <= default[index] && default[index] <= max[index]) {
            return Err(format!("parameter {id} has its default outside its range"));
        }
        parameter_list.push(json!({"id": id, "min": min[index], "max": max[index], "default": default[index]}));
    }
    let mesh_ids = reader.ids(MESH_IDS, meshes)?;
    let (parent, texture, vertices, indices) = (reader.ints(MESH_PARENT_PART, meshes)?, reader.ints(MESH_TEXTURE, meshes)?,
        reader.ints(MESH_VERTICES, meshes)?, reader.ints(MESH_INDICES, meshes)?);
    let mut drawables = Vec::with_capacity(meshes);
    for (index, id) in mesh_ids.iter().enumerate() {
        let parent_part = match usize::try_from(parent[index]) {
            Ok(at) => Some(part_ids.get(at).ok_or_else(|| format!("drawable {id} names a part that is not there"))?),
            Err(_) if parent[index] == -1 => None,
            Err(_) => return Err(format!("drawable {id} has a bad parent part")),
        };
        if texture[index] < 0 || vertices[index] < 0 || indices[index] < 0 || indices[index] % 3 != 0 {
            return Err(format!("drawable {id} is not the layout Soul2D reads"));
        }
        drawables.push(json!({"id": id, "parentPart": parent_part, "texture": texture[index],
            "vertices": vertices[index], "triangles": indices[index] / 3}));
    }
    Ok(json!({
        "version": version,
        // Width and height are told apart by which is larger on landscape samples only; read them as such.
        "canvas": {"pixelsPerUnit": canvas[0], "origin": [canvas[1], canvas[2]], "size": [canvas[3], canvas[4]]},
        "parts": part_ids,
        "deformers": {"ids": deformer_ids, "warp": warps, "rotation": rotations},
        "parameters": parameter_list,
        "drawables": drawables,
    }))
}

/// `live2d.runtime.inspect {path}`: a `.model3.json` (its file references, groups and hit areas, plus the `.moc3`
/// it names) or a `.moc3` alone.
/// Checks a Motion3 document against a runtime model read by `inspect`: every Parameter and PartOpacity curve
/// must name an ID the model has. Returns the number of curves checked.
pub(crate) fn check_motion(model: &Value, motion: &Value) -> Result<usize, String> {
    let ids = |list: &Value, key: Option<&str>| -> HashSet<String> {
        list.as_array().into_iter().flatten()
            .filter_map(|entry| key.map_or(entry.as_str(), |key| entry[key].as_str()).map(str::to_owned)).collect()
    };
    let (parameters, parts) = (ids(&model["parameters"], Some("id")), ids(&model["parts"], None));
    let mut missing = Vec::new();
    let mut checked = 0;
    for curve in motion["Curves"].as_array().into_iter().flatten() {
        let id = curve["Id"].as_str().unwrap_or_default();
        let known = match curve["Target"].as_str() {
            Some("Parameter") => &parameters,
            Some("PartOpacity") => &parts,
            _ => continue,
        };
        checked += 1;
        if !known.contains(id) {
            missing.push(format!("{} {id}", curve["Target"].as_str().unwrap_or_default()));
        }
    }
    if !missing.is_empty() {
        return Err(format!("the runtime model has no {}", missing.join(", ")));
    }
    Ok(checked)
}

pub(crate) fn inspect(request: &Value) -> Result<Value, String> {
    let path = request.get("path").and_then(Value::as_str).ok_or("path must be a string")?;
    if path.to_ascii_lowercase().ends_with(".moc3") {
        return read(&crate::read_bounded_file(path, MAX_MOC3_BYTES, ".moc3")?);
    }
    let text = crate::read_bounded_file(path, MAX_MODEL3_BYTES, ".model3.json")?;
    let model3: Value = serde_json::from_slice(&text).map_err(|error| format!("the .model3.json is not JSON: {error}"))?;
    let references = model3.get("FileReferences").filter(|value| value.is_object())
        .ok_or("the .model3.json has no FileReferences")?;
    let moc = references["Moc"].as_str().ok_or("the .model3.json names no Moc")?;
    // References are relative to the .model3.json; one that leaves its folder is refused.
    if Path::new(moc).is_absolute() || Path::new(moc).components().any(|part| part == std::path::Component::ParentDir) {
        return Err("the .model3.json Moc reference must stay inside its folder".into());
    }
    let folder = Path::new(path).parent().unwrap_or(Path::new("."));
    let moc_path = folder.join(moc);
    let mut model = read(&crate::read_bounded_file(&moc_path.to_string_lossy(), MAX_MOC3_BYTES, ".moc3")?)?;
    let textures = references["Textures"].as_array().cloned().unwrap_or_default();
    if let Some(over) = model["drawables"].as_array().into_iter().flatten()
        .find(|drawable| drawable["texture"].as_u64().is_some_and(|texture| texture as usize >= textures.len())) {
        return Err(format!("drawable {} uses a texture the .model3.json does not list", over["id"]));
    }
    model["model3"] = json!({
        "moc": moc,
        "textures": textures,
        "physics": references.get("Physics"),
        "pose": references.get("Pose"),
        "displayInfo": references.get("DisplayInfo"),
        "userData": references.get("UserData"),
        "expressions": references.get("Expressions"),
        "motions": references.get("Motions"),
        "groups": model3.get("Groups"),
        "hitAreas": model3.get("HitAreas"),
    });
    Ok(model)
}

/// GUIDs a CAN3 already uses for the model's parameters and Parts, by ID. A runtime model stores IDs only; its
/// scene takes these GUIDs so the animation's curves bind to it as they bind to the CMO3 it was exported from.
#[derive(Default)]
pub(crate) struct RuntimeGuids {
    parameters: HashMap<String, String>,
    parts: HashMap<String, String>,
}

impl RuntimeGuids {
    /// The parameter and Part GUIDs the curves of this resource's tracks carry.
    pub(crate) fn from_animation(animation: &Value, resource_guid: &str) -> Result<Self, String> {
        let mut guids = Self::default();
        let tracks = animation["scenes"].as_array().into_iter().flatten()
            .flat_map(|scene| scene["tracks"].as_array().into_iter().flatten())
            .filter(|track| track["resourceGuid"].as_str() == Some(resource_guid));
        for track in tracks {
            for (list, id, guid, map) in [
                ("parameterCurves", "parameterId", "parameterGuid", &mut guids.parameters),
                ("partOpacityCurves", "partId", "partGuid", &mut guids.parts),
            ] {
                for curve in track[list].as_array().into_iter().flatten() {
                    let (Some(id), Some(guid)) = (curve[id].as_str(), curve[guid].as_str()) else { continue };
                    if map.insert(id.to_owned(), guid.to_owned()).is_some_and(|known| known != guid) {
                        return Err(format!("the animation names {id} by two GUIDs"));
                    }
                }
            }
        }
        Ok(guids)
    }

    fn parameter(&self, id: &str) -> String {
        self.parameters.get(id).cloned().unwrap_or_else(|| format!("moc3-parameter-{id}"))
    }

    fn part(&self, id: &str) -> String {
        self.parts.get(id).cloned().unwrap_or_else(|| format!("moc3-part-{id}"))
    }
}

/// `at` as an index below `len`, or `None` for -1 when `optional`.
fn index(at: i32, len: usize, optional: bool, what: &str) -> Result<Option<usize>, String> {
    match usize::try_from(at) {
        Ok(index) if index < len => Ok(Some(index)),
        Err(_) if optional && at == -1 => Ok(None),
        _ => Err(format!("a .moc3 {what} index is outside its table")),
    }
}

/// `begin..begin + count` inside a table of `len` entries. An empty range may start at -1 (a parameter without
/// key tables stores that).
fn range(begin: i32, count: i32, len: usize, what: &str) -> Result<std::ops::Range<usize>, String> {
    if count == 0 && (begin == -1 || usize::try_from(begin).is_ok_and(|begin| begin <= len)) {
        return Ok(0..0);
    }
    let (begin, count) = (usize::try_from(begin), usize::try_from(count));
    match (begin, count) {
        (Ok(begin), Ok(count)) if begin.checked_add(count).is_some_and(|end| end <= len) => Ok(begin..begin + count),
        _ => Err(format!("a .moc3 {what} range is outside its table")),
    }
}

/// The colour and alpha compositions a version 6 blend word names: the colour in the low byte, the alpha above it.
fn composition(word: i32, what: &str) -> Result<(&'static str, &'static str), String> {
    let color = match word & 0xff {
        0 => "NORMAL",
        4 => "ADD_R2",
        6 => "MULTIPLY_R2",
        14 => "HARDLIGHT",
        17 => "HSL_COLOR",
        _ => return Err(format!("{what} uses a blend mode Soul2D does not read")),
    };
    let alpha = match word >> 8 {
        0 => "OVER",
        1 => "ATOP",
        2 => "OUT",
        3 => "CONJOINT",
        4 => "DISJOINT",
        _ => return Err(format!("{what} uses an alpha composition Soul2D does not read")),
    };
    Ok((color, alpha))
}

/// The offscreen Parts of a version 6 file.
struct Offscreen {
    /// The offscreen index of each Part, when it has one.
    part: Vec<Option<usize>>,
    flags: Vec<u8>,
    blend: Vec<i32>,
    mask_begin: Vec<i32>,
    mask_count: Vec<i32>,
    /// One opacity per keyform of every offscreen Part, Parts in list order; `opacity_begin` is each Part's start.
    opacity: Vec<f64>,
    opacity_begin: Vec<usize>,
}

/// What an offscreen Part composes with.
struct PartComposition {
    color: &'static str,
    alpha: &'static str,
    clip_guids: Vec<String>,
    invert: bool,
}

impl Offscreen {
    fn read(reader: &Reader<'_>, version: u8, parts: usize, part_keyform_count: &[i32]) -> Result<Option<Self>, String> {
        if version < 6 || reader.count_entries <= OFFSCREEN_KEYFORMS || reader.count(OFFSCREEN_PARTS)? == 0 {
            return Ok(None);
        }
        let count = reader.count(OFFSCREEN_PARTS)?;
        let mut part = vec![None; parts];
        let mut opacity_begin = Vec::with_capacity(count);
        let mut total = 0usize;
        for (at, listed) in reader.ints(OFFSCREEN_PART_LIST, count)?.iter().enumerate() {
            let listed = index(*listed, parts, false, "offscreen Part")?.unwrap_or_default();
            if part[listed].replace(at).is_some() {
                return Err("a .moc3 Part is listed twice as offscreen".into());
            }
            opacity_begin.push(total);
            total += usize::try_from(part_keyform_count[listed]).map_err(|_| "a .moc3 Part has a negative keyform count")?;
        }
        let keyforms = reader.count(OFFSCREEN_KEYFORMS)?;
        if total != keyforms {
            return Err("the .moc3 offscreen Part keyforms do not add up to their count".into());
        }
        Ok(Some(Offscreen {
            part, flags: reader.flag_bytes(OFFSCREEN_FLAGS, count)?.to_vec(), blend: reader.ints(OFFSCREEN_BLEND_WORDS, count)?,
            mask_begin: reader.ints(OFFSCREEN_MASK_BEGIN, count)?, mask_count: reader.ints(OFFSCREEN_MASK_COUNT, count)?,
            opacity: reader.floats(OFFSCREEN_KEYFORM_OPACITY, keyforms)?, opacity_begin,
        }))
    }

    /// The opacity of keyform `keyform` of Part `part`, when the Part is offscreen.
    fn opacity(&self, part: usize, keyform: usize) -> Option<f64> {
        self.part[part].and_then(|at| self.opacity.get(self.opacity_begin[at] + keyform).copied())
    }

    fn composition(&self, part: usize, id: &str, masks: &[i32], mesh_guid: impl Fn(usize) -> String) -> Result<Option<PartComposition>, String> {
        let Some(at) = self.part[part] else { return Ok(None) };
        let flag = self.flags[at];
        if flag & !0b1111 != 0 {
            return Err(format!("Part {id} carries a flag Soul2D does not read"));
        }
        let (color, alpha) = composition(self.blend[at], &format!("Part {id}"))?;
        let clip_guids = masks[range(self.mask_begin[at], self.mask_count[at], masks.len(), "Part mask")?].iter()
            .map(|mask| index(*mask, usize::MAX, false, "mask drawable").map(|mask| mesh_guid(mask.unwrap_or_default())))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(PartComposition { color, alpha, clip_guids, invert: flag & 0b1000 != 0 }))
    }
}

struct Keyforms<'a> {
    reader: &'a Reader<'a>,
    floats: Vec<f64>,
    /// Canvas: pixels per unit and the origin, which root-level objects' coordinates are normalized by.
    ppu: f64,
    origin: [f64; 2],
    parameters: Vec<Parameter>,
    keytable_parameter: Vec<usize>,
    keytable_keys: Vec<Vec<f64>>,
    binding_keytables: Vec<Vec<usize>>,
    forms: usize,
    /// Blend-shape bindings whose entry no parameter's run covers.
    unlisted_bindings: usize,
}

/// The blend-shape tables of a version 5 or later file.
struct BlendShapes {
    /// The parameter behind each blend-shape entry, `None` when no parameter's run covers it.
    parameter: Vec<Option<usize>>,
    /// The keys of each blend-shape entry, from `KEYS`.
    keys: Vec<Vec<f64>>,
    binding_parameter: Vec<i32>,
    binding_keyform_begin: Vec<i32>,
    binding_keyform_count: Vec<i32>,
    binding_constraint_begin: Vec<i32>,
    binding_constraint_count: Vec<i32>,
    group_refs: Vec<i32>,
    group_parameter: Vec<i32>,
    point_begin: Vec<i32>,
    point_count: Vec<i32>,
    point_value: Vec<f64>,
    point_weight: Vec<f64>,
    /// The binding runs of each warp deformer, ArtMesh, rotation deformer and Part that carries blend shapes; the
    /// reader accepts several rows for one object, all of which apply.
    warps: Vec<Vec<std::ops::Range<usize>>>,
    meshes: Vec<Vec<std::ops::Range<usize>>>,
    rotations: Vec<Vec<std::ops::Range<usize>>>,
    parts: Vec<Vec<std::ops::Range<usize>>>,
}

impl BlendShapes {
    /// Reads the tables when the file is version 5 or later and lists blend-shape parameters; marks those
    /// parameters `MORPH_TARGET`.
    fn read(reader: &Reader<'_>, version: u8, parameters: &mut [Parameter], keys: &[f64], warps: usize, meshes: usize, rotations: usize,
        parts: usize)
        -> Result<Option<Self>, String> {
        if version < 5 || reader.count_entries <= ROTATION_BLEND_SOURCES || reader.count(BLEND_SHAPE_PARAMETERS)? == 0 {
            return Ok(None);
        }
        let entries = reader.count(BLEND_SHAPE_PARAMETERS)?;
        let is_blend = reader.ints(PARAMETER_IS_BLEND_SHAPE, parameters.len())?;
        let (entry_begin, entry_count) = (reader.ints(PARAMETER_BLEND_ENTRY_BEGIN, parameters.len())?,
            reader.ints(PARAMETER_BLEND_ENTRY_COUNT, parameters.len())?);
        let mut parameter = vec![None; entries];
        for (at, flag) in is_blend.iter().enumerate() {
            if *flag == 0 {
                continue;
            }
            parameters[at].parameter_type = "MORPH_TARGET".into();
            for entry in range(entry_begin[at], entry_count[at], entries, "blend-shape entry")? {
                if parameter[entry].replace(at).is_some() {
                    return Err("two .moc3 parameters share one blend-shape entry".into());
                }
            }
        }
        let (key_begin, key_count) = (reader.ints(BLEND_SHAPE_KEY_BEGIN, entries)?, reader.ints(BLEND_SHAPE_KEY_COUNT, entries)?);
        let mut blend_keys = Vec::with_capacity(entries);
        for n in 0..entries {
            let values = keys[range(key_begin[n], key_count[n], keys.len(), "blend-shape key")?].to_vec();
            if values.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("a .moc3 blend-shape key list is not ascending".into());
            }
            blend_keys.push(values);
        }
        let bindings = reader.count(BLEND_SHAPE_BINDINGS)?;
        let binding = |slot| reader.ints(slot, bindings);
        let groups = reader.count(CONSTRAINT_GROUPS)?;
        let points = reader.count(CONSTRAINT_POINTS)?;
        let runs = |source: usize, begin: usize, count: usize, entries: usize, objects: usize, what: &str| -> Result<Vec<Vec<std::ops::Range<usize>>>, String> {
            let entries = reader.count(entries)?;
            let (object, run_begin, run_count) = (reader.ints(source, entries)?, reader.ints(begin, entries)?, reader.ints(count, entries)?);
            let mut out = vec![Vec::new(); objects];
            for at in 0..entries {
                let object = index(object[at], objects, false, what)?.unwrap_or_default();
                out[object].push(range(run_begin[at], run_count[at], bindings, "blend-shape binding")?);
            }
            Ok(out)
        };
        Ok(Some(BlendShapes {
            parameter, keys: blend_keys,
            binding_parameter: binding(BLEND_BINDING_PARAMETER)?, binding_keyform_begin: binding(BLEND_BINDING_KEYFORM_BEGIN)?,
            binding_keyform_count: binding(BLEND_BINDING_KEYFORM_COUNT)?, binding_constraint_begin: binding(BLEND_BINDING_CONSTRAINT_BEGIN)?,
            binding_constraint_count: binding(BLEND_BINDING_CONSTRAINT_COUNT)?,
            group_refs: reader.ints(CONSTRAINT_GROUP_REFS, reader.count(CONSTRAINT_GROUP_REF_COUNT)?)?,
            group_parameter: reader.ints(CONSTRAINT_GROUP_PARAMETER, groups)?, point_begin: reader.ints(CONSTRAINT_POINT_BEGIN, groups)?,
            point_count: reader.ints(CONSTRAINT_POINT_COUNT, groups)?, point_value: reader.floats(CONSTRAINT_POINT_VALUE, points)?,
            point_weight: reader.floats(CONSTRAINT_POINT_WEIGHT, points)?,
            warps: runs(WARP_BLEND_SOURCE, WARP_BLEND_BINDING_BEGIN, WARP_BLEND_BINDING_COUNT, WARP_BLEND_SOURCES, warps, "blend-shape warp deformer")?,
            meshes: runs(MESH_BLEND_SOURCE, MESH_BLEND_BINDING_BEGIN, MESH_BLEND_BINDING_COUNT, MESH_BLEND_SOURCES, meshes, "blend-shape drawable")?,
            rotations: runs(ROTATION_BLEND_SOURCE, ROTATION_BLEND_BINDING_BEGIN, ROTATION_BLEND_BINDING_COUNT, ROTATION_BLEND_SOURCES, rotations,
                "blend-shape rotation deformer")?,
            parts: runs(PART_BLEND_SOURCE, PART_BLEND_BINDING_BEGIN, PART_BLEND_BINDING_COUNT, PART_BLEND_SOURCES, parts, "blend-shape Part")?,
        }))
    }
}

impl Keyforms<'_> {
    /// Root-level objects store `(value - origin) / pixelsPerUnit`; objects inside a deformer store that deformer's
    /// local coordinates as they are.
    fn point(&self, root: bool, x: f64, y: f64) -> [f64; 2] {
        if root { [x * self.ppu + self.origin[0], y * self.ppu + self.origin[1]] } else { [x, y] }
    }

    fn positions(&self, start: i32, pairs: usize, root: bool) -> Result<Vec<f64>, String> {
        let span = range(start, i32::try_from(2 * pairs).map_err(|_| "a .moc3 keyform is too large")?,
            self.floats.len(), "keyform position")?;
        Ok(self.floats[span].chunks_exact(2).flat_map(|pair| self.point(root, pair[0], pair[1])).collect())
    }

    /// Blend-shape keyforms store differences: root-level ones scaled by the pixels per unit, with no origin.
    fn deltas(&self, start: i32, pairs: usize, root: bool) -> Result<Vec<f64>, String> {
        let span = range(start, i32::try_from(2 * pairs).map_err(|_| "a .moc3 keyform is too large")?,
            self.floats.len(), "keyform position")?;
        let scale = if root { self.ppu } else { 1.0 };
        Ok(self.floats[span].iter().map(|value| value * scale).collect())
    }

    /// The cell of one binding's grid at the parameter defaults, which blend shapes are differences from; `None`
    /// when a default is not a key.
    fn default_cell(&self, binding: i32) -> Result<Option<usize>, String> {
        let binding = index(binding, self.binding_keytables.len(), false, "keyform binding")?.unwrap_or_default();
        let (mut cell, mut stride) = (0usize, 1usize);
        for keytable in &self.binding_keytables[binding] {
            let default = self.parameters[self.keytable_parameter[*keytable]].default;
            let keys = &self.keytable_keys[*keytable];
            let Some(at) = keys.iter().position(|key| *key == default) else { return Ok(None) };
            cell += at * stride;
            stride *= keys.len();
        }
        Ok(Some(cell))
    }

    /// The morph targets of one object from its blend-shape binding run: a Form per nonzero key, the default-cell
    /// keyform plus the stored difference, which `target` computes from the base Form and the keyform index.
    #[allow(clippy::too_many_arguments)]
    fn blend_targets(
        &mut self, blend: &BlendShapes, bindings: &[std::ops::Range<usize>], grid_binding: i32, root: bool, keyforms: usize, id: &str,
        forms: &mut Vec<Form>, warnings: &mut Vec<String>,
        target: impl Fn(&Self, &Form, usize) -> Result<(f64, Option<i32>, FormData), String>,
    ) -> Result<Option<MorphTargets>, String> {
        let Some(cell) = self.default_cell(grid_binding)? else {
            warnings.push(format!("{id} has blend shapes but no keyform at its parameter defaults; they are left out"));
            return Ok(None);
        };
        let base = forms.get(cell).ok_or("a .moc3 default keyform cell is outside its keyforms")?;
        let (multiply_color, screen_color) = (base.multiply_color, base.screen_color);
        let mut pending = Vec::new();
        let mut blend_weight_constraints = Vec::new();
        for binding in bindings.iter().cloned().flatten() {
            let entry = index(blend.binding_parameter[binding], blend.parameter.len(), false, "blend-shape binding entry")?.unwrap_or_default();
            let Some(parameter) = blend.parameter[entry] else {
                self.unlisted_bindings += 1;
                continue;
            };
            let keys = &blend.keys[entry];
            let span = range(blend.binding_keyform_begin[binding], blend.binding_keyform_count[binding], keyforms, "blend-shape keyform")?;
            if span.len() != keys.len() {
                return Err(format!("{id} stores another number of blend-shape keyforms than keys"));
            }
            for (key, keyform) in keys.iter().zip(span) {
                if *key != 0.0 {
                    pending.push((parameter, *key, target(self, base, keyform)?));
                }
            }
            let morph = &self.parameters[parameter];
            for group in range(blend.binding_constraint_begin[binding], blend.binding_constraint_count[binding], blend.group_refs.len(),
                "blend-shape constraint")? {
                let group = index(blend.group_refs[group], blend.group_parameter.len(), false, "blend-shape constraint group")?.unwrap_or_default();
                let controller = index(blend.group_parameter[group], self.parameters.len(), false, "blend-shape constraint parameter")?.unwrap_or_default();
                let controller = &self.parameters[controller];
                for point in range(blend.point_begin[group], blend.point_count[group], blend.point_value.len(), "blend-shape constraint point")? {
                    blend_weight_constraints.push(BlendWeightConstraint {
                        morph_target_parameter_guid: morph.guid.clone(), morph_target_parameter_id: morph.id.clone(),
                        constraint_parameter_guid: controller.guid.clone(), constraint_parameter_id: controller.id.clone(),
                        constraint_parameter_value: blend.point_value[point], blend_weight: blend.point_weight[point],
                    });
                }
            }
        }
        if pending.is_empty() {
            return Ok(None);
        }
        let mut targets = Vec::with_capacity(pending.len());
        for (parameter, key_value, (opacity, draw_order, data)) in pending {
            let mut form = self.form(root, opacity, draw_order, data);
            form.multiply_color = multiply_color;
            form.screen_color = screen_color;
            let parameter = &self.parameters[parameter];
            targets.push(MorphTarget {
                parameter_guid: parameter.guid.clone(), parameter_id: parameter.id.clone(), key_value, form_guid: form.guid.clone(),
                edit_base_parameter_map: (),
            });
            forms.push(form);
        }
        Ok(Some(MorphTargets { targets, blend_weight_constraints }))
    }

    /// The grid of one keyform binding over `forms` form GUIDs: the first axis varies fastest, as the CMO3 cells
    /// of every sampled source confirmed.
    fn grid(&self, binding: i32, forms: &[String]) -> Result<KeyformGrid, String> {
        let binding = index(binding, self.binding_keytables.len(), false, "keyform binding")?.unwrap_or_default();
        let mut axes = Vec::new();
        let mut expected = 1usize;
        for (position, keytable) in self.binding_keytables[binding].iter().enumerate() {
            let parameter = &self.parameters[self.keytable_parameter[*keytable]];
            let keys = self.keytable_keys[*keytable].clone();
            expected = expected.checked_mul(keys.len()).ok_or("a .moc3 keyform grid is too large")?;
            axes.push(KeyformAxis {
                binding_ref: format!("moc3-binding-{binding}-{position}"),
                parameter_guid: parameter.guid.clone(),
                parameter_id: Some(parameter.id.clone()),
                keys,
                interpolation: "LINEAR".into(),
                extended_interpolation: "LINEAR".into(),
                insert_point_count: 0,
                extended_interpolation_scale: 1.0,
            });
        }
        if forms.len() != expected {
            return Err("a .moc3 object stores another number of keyforms than its keys multiply to".into());
        }
        let cells = forms.iter().enumerate().map(|(cell, guid)| {
            let mut rest = cell;
            let indices = axes.iter().map(|axis| {
                let at = rest % axis.keys.len();
                rest /= axis.keys.len();
                at
            }).collect();
            KeyformCell { indices, form_guid: guid.clone() }
        }).collect();
        Ok(KeyformGrid { axes, cells })
    }

    fn form(&mut self, root: bool, opacity: f64, draw_order: Option<i32>, data: FormData) -> Form {
        self.forms += 1;
        Form {
            guid: format!("moc3-form-{}", self.forms),
            coordinate_type: Some(if root { "Canvas" } else { "DeformerLocal" }.into()),
            animated: false,
            local_animated: false,
            opacity,
            draw_order,
            multiply_color: [1.0, 1.0, 1.0, 1.0],
            screen_color: [0.0, 0.0, 0.0, 1.0],
            vertex_count_matches_source: None,
            data,
        }
    }
}

/// The width and height of a PNG from its header, so the texture record can state them.
fn png_size(data: &[u8]) -> Result<[u32; 2], String> {
    if data.get(..8) != Some(b"\x89PNG\r\n\x1a\n") || data.get(12..16) != Some(b"IHDR") {
        return Err("a runtime texture is not a PNG".into());
    }
    let dimension = |at: usize| data.get(at..at + 4).map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    match (dimension(16), dimension(20)) {
        (Some(width), Some(height)) if width > 0 && height > 0 => Ok([width, height]),
        _ => Err("a runtime texture PNG has no size".into()),
    }
}

/// The authoring scene of a `.moc3`: its parameters, Parts, deformers and ArtMeshes with every keyform, laid out
/// as the CMO3 source projection so the native evaluator and preview take it unchanged. `textures` are the PNG
/// files in the order the `.model3.json` lists them. What the file does not store, or the samples did not
/// confirm, is set to the authoring defaults: keyform opacity 1 for a Part that is not offscreen, white and
/// black keyform colours before version 5, no culling, and the usual blend `OVER`. Parts the animation names but
/// the file lacks become empty Parts.
pub(crate) fn runtime_scene(
    bytes: &[u8], name: &str, textures: &[(String, Vec<u8>)], guids: &RuntimeGuids,
) -> Result<(Scene, Vec<String>), String> {
    let (reader, header) = open(bytes)?;
    let Header { version, parts, deformers, warps, rotations, meshes, parameters } = header;
    let mut warnings = Vec::new();
    let canvas = reader.floats(CANVAS, 5)?;
    let (ppu, origin, size) = (canvas[0], [canvas[1], canvas[2]], [canvas[3], canvas[4]]);
    if ppu <= 0.0 || size.iter().any(|side| *side < 1.0 || *side > f64::from(u32::MAX) || side.fract() != 0.0) {
        return Err("the .moc3 canvas is not a positive integral size with a positive scale".into());
    }
    // Parameters and their key tables.
    let parameter_ids = reader.ids(PARAMETER_IDS, parameters)?;
    let (max, min, default, repeat) = (reader.decimals(PARAMETER_MAX, parameters)?, reader.decimals(PARAMETER_MIN, parameters)?,
        reader.decimals(PARAMETER_DEFAULT, parameters)?, reader.ints(PARAMETER_REPEAT, parameters)?);
    let mut seen = HashSet::new();
    let mut parameter_list = Vec::with_capacity(parameters);
    for (at, id) in parameter_ids.iter().enumerate() {
        if !(min[at] <= default[at] && default[at] <= max[at]) {
            return Err(format!("parameter {id} has its default outside its range"));
        }
        if !seen.insert(id.as_str()) {
            return Err(format!("parameter {id} is listed twice"));
        }
        parameter_list.push(Parameter {
            guid: guids.parameter(id), id: id.clone(), name: id.clone(), minimum: min[at], maximum: max[at],
            default: default[at], repeat: repeat[at] != 0, parameter_type: "NORMAL".into(),
        });
    }
    let keytables = reader.count(KEYTABLES)?;
    let (table_begin, table_count) = (reader.ints(PARAMETER_KEYTABLE_BEGIN, parameters)?, reader.ints(PARAMETER_KEYTABLE_COUNT, parameters)?);
    let mut keytable_parameter = vec![usize::MAX; keytables];
    for at in 0..parameters {
        for table in range(table_begin[at], table_count[at], keytables, "parameter key table")? {
            if keytable_parameter[table] != usize::MAX {
                return Err("a .moc3 key table belongs to two parameters".into());
            }
            keytable_parameter[table] = at;
        }
    }
    if keytable_parameter.contains(&usize::MAX) {
        return Err("a .moc3 key table belongs to no parameter".into());
    }
    let keys = reader.decimals(KEYS, reader.count(KEY_COUNT)?)?;
    let (key_begin, key_count) = (reader.ints(KEYTABLE_KEY_BEGIN, keytables)?, reader.ints(KEYTABLE_KEY_COUNT, keytables)?);
    let mut keytable_keys = Vec::with_capacity(keytables);
    for table in 0..keytables {
        let span = range(key_begin[table], key_count[table], keys.len(), "key")?;
        let values = keys[span].to_vec();
        if values.is_empty() || values.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("a .moc3 key table is empty or not ascending".into());
        }
        keytable_keys.push(values);
    }
    let blend = BlendShapes::read(&reader, version, &mut parameter_list, &keys, warps, meshes, rotations, parts)?;
    let bindings = reader.count(KEYFORM_BINDINGS)?;
    let parameter_bindings = reader.ints(PARAMETER_BINDING_KEYTABLE, reader.count(PARAMETER_BINDINGS)?)?;
    let (binding_begin, binding_count) = (reader.ints(KEYFORM_BINDING_BEGIN, bindings)?, reader.ints(KEYFORM_BINDING_COUNT, bindings)?);
    let mut binding_keytables = Vec::with_capacity(bindings);
    for binding in 0..bindings {
        let span = range(binding_begin[binding], binding_count[binding], parameter_bindings.len(), "parameter binding")?;
        let tables = parameter_bindings[span].iter()
            .map(|table| index(*table, keytables, false, "key table").map(Option::unwrap_or_default))
            .collect::<Result<Vec<_>, _>>()?;
        binding_keytables.push(tables);
    }
    let mut keyforms = Keyforms {
        reader: &reader, floats: reader.floats(KEYFORM_FLOATS, reader.count(KEYFORM_FLOAT_COUNT)?)?, ppu, origin,
        parameters: parameter_list, keytable_parameter, keytable_keys, binding_keytables, forms: 0, unlisted_bindings: 0,
    };
    let blend_run = |table: fn(&BlendShapes) -> &[Vec<std::ops::Range<usize>>], at: usize| {
        blend.as_ref().and_then(|blend| Some(&table(blend)[at]).filter(|runs| !runs.is_empty()).map(|runs| (blend, runs.as_slice())))
    };
    // The grid covers the keyforms read so far; blend-shape target Forms are appended after it.
    let grid_of = |keyforms: &Keyforms, binding: i32, forms: &[Form]| {
        let guids: Vec<String> = forms.iter().map(|form| form.guid.clone()).collect();
        keyforms.grid(binding, &guids)
    };
    let whole_draw_order = |order: f64| -> Result<i32, String> {
        if !(0.0..=1000.0).contains(&order) || order.fract() != 0.0 {
            return Err("a .moc3 draw order is not a whole number from 0 to 1000".into());
        }
        Ok(order as i32)
    };
    // Parts. The CMO3 root Part is not exported; a synthetic one holds the Parts the file lists at the top.
    let part_ids = reader.ids(PART_IDS, parts)?;
    let part_guids: Vec<String> = part_ids.iter().map(|id| guids.part(id)).collect();
    let (part_parent, part_binding, part_kf_begin, part_kf_count) = (reader.ints(PART_PARENT, parts)?,
        reader.ints(PART_BINDING, parts)?, reader.ints(PART_KEYFORM_BEGIN, parts)?, reader.ints(PART_KEYFORM_COUNT, parts)?);
    let part_keyforms = reader.count(PART_KEYFORMS)?;
    let part_draw_order = reader.floats(PART_KEYFORM_DRAW_ORDER, part_keyforms)?;
    let offscreen = Offscreen::read(&reader, version, parts, &part_kf_count)?;
    let masks = reader.ints(MASKS, reader.count(MASK_COUNT)?)?;
    let mesh_ids = reader.ids(MESH_IDS, meshes)?;
    let mesh_guid = |at: usize| format!("moc3-mesh-{}", mesh_ids[at]);
    let mut children: Vec<Vec<String>> = vec![Vec::new(); parts + 1];
    let part_guid = |at: Option<usize>| at.map_or(RUNTIME_ROOT_PART.to_owned(), |at| part_guids[at].clone());
    let mut sources = Vec::new();
    let mut part_sources = Vec::with_capacity(parts);
    for at in 0..parts {
        let parent = index(part_parent[at], parts, true, "part parent")?;
        children[parent.map_or(parts, |parent| parent)].push(part_guids[at].clone());
        let mut forms = Vec::new();
        for (slot, keyform) in range(part_kf_begin[at], part_kf_count[at], part_keyforms, "part keyform")?.enumerate() {
            let opacity = offscreen.as_ref().and_then(|offscreen| offscreen.opacity(at, slot)).unwrap_or(1.0);
            forms.push(keyforms.form(false, opacity, Some(whole_draw_order(part_draw_order[keyform])?), FormData::Part));
        }
        let grid = grid_of(&keyforms, part_binding[at], &forms)?;
        // A Part's blend shape changes its draw order (and, for an offscreen Part, its opacity, which no sample
        // stores); the group keeps its drawables together as Core does.
        let morph = match blend_run(|blend| &blend.parts, at) {
            Some((blend, run)) => keyforms.blend_targets(blend, run, part_binding[at], false, part_keyforms, &part_ids[at], &mut forms,
                &mut warnings, |_, base, keyform| {
                    let order = whole_draw_order(f64::from(base.draw_order.unwrap_or(500)) + part_draw_order[keyform])?;
                    Ok((base.opacity, Some(order), FormData::Part))
                })?,
            None => None,
        };
        let composition = match &offscreen {
            Some(offscreen) => offscreen.composition(at, &part_ids[at], &masks, mesh_guid)?,
            None => None,
        };
        part_sources.push((at, parent, forms, grid, morph, composition));
    }
    // Deformers: warp and rotation deformers interleave in the ID table; each carries its own type's index.
    let deformer_ids = reader.ids(DEFORMER_IDS, deformers)?;
    let (deformer_part, deformer_parent, deformer_type, deformer_index) = (reader.ints(DEFORMER_PARENT_PART, deformers)?,
        reader.ints(DEFORMER_PARENT, deformers)?, reader.ints(DEFORMER_TYPE, deformers)?, reader.ints(DEFORMER_INDEX, deformers)?);
    let mut warp_of = vec![usize::MAX; warps];
    let mut rotation_of = vec![usize::MAX; rotations];
    for at in 0..deformers {
        let slot = match deformer_type[at] {
            0 => &mut warp_of[index(deformer_index[at], warps, false, "warp deformer")?.unwrap_or_default()],
            1 => &mut rotation_of[index(deformer_index[at], rotations, false, "rotation deformer")?.unwrap_or_default()],
            _ => return Err("a .moc3 deformer has a type Soul2D does not read".into()),
        };
        if *slot != usize::MAX {
            return Err("two .moc3 deformers share one typed index".into());
        }
        *slot = at;
    }
    let deformer_parents: Vec<Option<usize>> = (0..deformers)
        .map(|at| index(deformer_parent[at], deformers, true, "deformer parent")).collect::<Result<_, _>>()?;
    // Whether a rotation deformer lies under another: then its scale is stored as it is, else divided by the
    // pixels per unit. Every sampled rotation deformer followed this rule.
    let under_rotation = |mut at: usize| -> Result<bool, String> {
        for _ in 0..=deformers {
            match deformer_parents[at] {
                Some(parent) if deformer_type[parent] == 1 => return Ok(true),
                Some(parent) => at = parent,
                None => return Ok(false),
            }
        }
        Err("the .moc3 deformer hierarchy is cyclic".into())
    };
    let deformer_guid = |at: usize| format!("moc3-deformer-{}", deformer_ids[at]);
    let (warp_binding, warp_kf_begin, warp_kf_count, warp_vertices, warp_rows, warp_columns) = (reader.ints(WARP_BINDING, warps)?,
        reader.ints(WARP_KEYFORM_BEGIN, warps)?, reader.ints(WARP_KEYFORM_COUNT, warps)?, reader.ints(WARP_VERTICES, warps)?,
        reader.ints(WARP_ROWS, warps)?, reader.ints(WARP_COLUMNS, warps)?);
    let warp_keyforms = reader.count(WARP_KEYFORMS)?;
    let (warp_opacity, warp_position) = (reader.floats(WARP_KEYFORM_OPACITY, warp_keyforms)?, reader.ints(WARP_KEYFORM_POSITION, warp_keyforms)?);
    let quad_transform = if reader.offsets[WARP_QUAD_TRANSFORM] != 0 {
        reader.ints(WARP_QUAD_TRANSFORM, warps)?
    } else {
        vec![0; warps]
    };
    let (rotation_binding, rotation_kf_begin, rotation_kf_count) = (reader.ints(ROTATION_BINDING, rotations)?,
        reader.ints(ROTATION_KEYFORM_BEGIN, rotations)?, reader.ints(ROTATION_KEYFORM_COUNT, rotations)?);
    let rotation_keyforms = reader.count(ROTATION_KEYFORMS)?;
    let rotation_base = if reader.offsets[ROTATION_BASE_ANGLE] != 0 { reader.floats(ROTATION_BASE_ANGLE, rotations)? } else { vec![0.0; rotations] };
    let rotation_field = |slot| reader.floats(slot, rotation_keyforms);
    let (rotation_opacity, rotation_angle, rotation_x, rotation_y, rotation_scale, reflect_x, reflect_y) = (
        rotation_field(ROTATION_KEYFORM_OPACITY)?, rotation_field(ROTATION_KEYFORM_ANGLE)?, rotation_field(ROTATION_KEYFORM_ORIGIN_X)?,
        rotation_field(ROTATION_KEYFORM_ORIGIN_Y)?, rotation_field(ROTATION_KEYFORM_SCALE)?, rotation_field(ROTATION_KEYFORM_REFLECT_X)?,
        rotation_field(ROTATION_KEYFORM_REFLECT_Y)?);
    let mut deformer_sources = Vec::with_capacity(deformers);
    for at in 0..deformers {
        let parent_part = index(deformer_part[at], parts, true, "deformer part")?;
        children[parent_part.map_or(parts, |part| part)].push(deformer_guid(at));
        let root = deformer_parents[at].is_none();
        let (forms, grid, morph, data) = if deformer_type[at] == 0 {
            let warp = deformer_index[at] as usize;
            let (rows, columns) = (warp_rows[warp], warp_columns[warp]);
            let vertices = usize::try_from(warp_vertices[warp]).ok();
            if rows < 1 || columns < 1 || vertices != Some((rows as usize + 1) * (columns as usize + 1)) {
                return Err("a .moc3 warp deformer grid does not match its vertex count".into());
            }
            let mut forms = Vec::new();
            for keyform in range(warp_kf_begin[warp], warp_kf_count[warp], warp_keyforms, "warp keyform")? {
                let positions = keyforms.positions(warp_position[keyform], vertices.unwrap_or_default(), root)?;
                forms.push(keyforms.form(root, warp_opacity[keyform], None, FormData::Warp { positions }));
            }
            let grid = grid_of(&keyforms, warp_binding[warp], &forms)?;
            let morph = match blend_run(|blend| &blend.warps, warp) {
                Some((blend, run)) => keyforms.blend_targets(blend, run, warp_binding[warp], root, warp_keyforms, &deformer_ids[at], &mut forms,
                    &mut warnings, |keyforms, base, keyform| {
                        let FormData::Warp { positions: base_positions } = &base.data else { return Err("a .moc3 warp base keyform is not a warp".into()) };
                        let deltas = keyforms.deltas(warp_position[keyform], vertices.unwrap_or_default(), root)?;
                        let positions = base_positions.iter().zip(deltas).map(|(base, delta)| base + delta).collect();
                        Ok((base.opacity + warp_opacity[keyform], None, FormData::Warp { positions }))
                    })?,
                None => None,
            };
            (forms, grid, morph, SourceData::Warp {
                columns: columns as u32, rows: rows as u32, quad_transform: quad_transform[warp] != 0, bezier_settings: Vec::new(),
            })
        } else {
            let rotation = deformer_index[at] as usize;
            let scale_by = if under_rotation(at)? { 1.0 } else { ppu };
            let mut forms = Vec::new();
            for keyform in range(rotation_kf_begin[rotation], rotation_kf_count[rotation], rotation_keyforms, "rotation keyform")? {
                // The file stores the scale as f32; its product with the pixels per unit is rounded back to a
                // value the authoring file would hold.
                let scale = (rotation_scale[keyform] * scale_by * 1e6).round() / 1e6;
                let data = FormData::Rotation {
                    angle: rotation_angle[keyform],
                    origin: keyforms.point(root, rotation_x[keyform], rotation_y[keyform]),
                    scale,
                    reflect_x: reflect_x[keyform] != 0.0,
                    reflect_y: reflect_y[keyform] != 0.0,
                };
                forms.push(keyforms.form(root, rotation_opacity[keyform], None, data));
            }
            let grid = grid_of(&keyforms, rotation_binding[rotation], &forms)?;
            let morph = match blend_run(|blend| &blend.rotations, rotation) {
                Some((blend, run)) => keyforms.blend_targets(blend, run, rotation_binding[rotation], root, rotation_keyforms, &deformer_ids[at],
                    &mut forms, &mut warnings, |keyforms, base, keyform| {
                        let FormData::Rotation { angle, origin, scale, reflect_x, reflect_y } = &base.data else {
                            return Err("a .moc3 rotation base keyform is not a rotation".into());
                        };
                        let shift = if root { keyforms.ppu } else { 1.0 };
                        // Differences take the base keyform's unit; Mao's Rotation3 (under rotation deformers) adds
                        // -0.29 and +0.48 to its scale 0.575 and up to 7,876 to its origin. No root-level sample
                        // stores a nonzero difference.
                        let data = FormData::Rotation {
                            angle: angle + rotation_angle[keyform],
                            origin: [origin[0] + rotation_x[keyform] * shift, origin[1] + rotation_y[keyform] * shift],
                            scale: ((scale + rotation_scale[keyform] * scale_by) * 1e6).round() / 1e6,
                            reflect_x: *reflect_x, reflect_y: *reflect_y,
                        };
                        Ok((base.opacity + rotation_opacity[keyform], None, data))
                    })?,
                None => None,
            };
            (forms, grid, morph, SourceData::Rotation { base_angle: rotation_base[rotation] })
        };
        deformer_sources.push((at, parent_part, forms, grid, morph, data));
    }
    // ArtMeshes.
    let mesh_field = |slot| reader.ints(slot, meshes);
    let (mesh_binding, mesh_kf_begin, mesh_kf_count, mesh_part, mesh_deformer, mesh_texture, mesh_vertices, mesh_vertex_begin,
        mesh_index_begin, mesh_index_count, mesh_mask_begin, mesh_mask_count) = (mesh_field(MESH_BINDING)?, mesh_field(MESH_KEYFORM_BEGIN)?,
        mesh_field(MESH_KEYFORM_COUNT)?, mesh_field(MESH_PARENT_PART)?, mesh_field(MESH_PARENT_DEFORMER)?, mesh_field(MESH_TEXTURE)?,
        mesh_field(MESH_VERTICES)?, mesh_field(MESH_VERTEX_BEGIN)?, mesh_field(MESH_INDEX_BEGIN)?, mesh_field(MESH_INDICES)?,
        mesh_field(MESH_MASK_BEGIN)?, mesh_field(MESH_MASK_COUNT)?);
    let flags = reader.flag_bytes(MESH_FLAGS, meshes)?;
    let blend_words = if version >= 6 && reader.offsets[MESH_BLEND_WORDS] != 0 { Some(reader.ints(MESH_BLEND_WORDS, meshes)?) } else { None };
    let mesh_keyforms = reader.count(MESH_KEYFORMS)?;
    let (mesh_opacity, mesh_draw_order, mesh_position) = (reader.floats(MESH_KEYFORM_OPACITY, mesh_keyforms)?,
        reader.floats(MESH_KEYFORM_DRAW_ORDER, mesh_keyforms)?, reader.ints(MESH_KEYFORM_POSITION, mesh_keyforms)?);
    let uvs = reader.floats(UVS, reader.count(VERTEX_FLOATS)?)?;
    let colors = if version >= 5 && reader.count_entries > COLOR_KEYFORMS && reader.offsets[MESH_COLOR_BEGIN] != 0 {
        let entries = reader.count(COLOR_KEYFORMS)?;
        // Only the drawables' grid entries are range-checked below: the table also covers deformer and blend-shape
        // keyforms, where Mao stores differences below zero that nothing here reads.
        let channels: Vec<Vec<f64>> = [KEYFORM_MULTIPLY_RED, KEYFORM_SCREEN_RED].iter()
            .flat_map(|first| (0..3).map(move |at| first + at)).map(|slot| reader.floats(slot, entries)).collect::<Result<_, _>>()?;
        Some((reader.ints(MESH_COLOR_BEGIN, meshes)?, channels))
    } else {
        None
    };
    let indices = reader.u16s(INDICES, reader.count(INDEX_COUNT)?)?;
    let mut mesh_sources = Vec::with_capacity(meshes);
    for at in 0..meshes {
        let parent_part = index(mesh_part[at], parts, true, "drawable part")?;
        children[parent_part.map_or(parts, |part| part)].push(mesh_guid(at));
        let deformer = index(mesh_deformer[at], deformers, true, "drawable deformer")?;
        let root = deformer.is_none();
        let vertices = usize::try_from(mesh_vertices[at]).map_err(|_| "a .moc3 drawable has a negative vertex count")?;
        let uv_span = range(mesh_vertex_begin[at], mesh_vertices[at].saturating_mul(2), uvs.len(), "drawable UV")?;
        let index_span = range(mesh_index_begin[at], mesh_index_count[at], indices.len(), "drawable index")?;
        if index_span.len() % 3 != 0 {
            return Err("a .moc3 drawable has indices that are not triangles".into());
        }
        let triangle_indices = indices[index_span].iter().map(|vertex| {
            (usize::from(*vertex) < vertices).then_some(u32::from(*vertex)).ok_or("a .moc3 drawable indexes a vertex it lacks")
        }).collect::<Result<Vec<_>, _>>()?;
        let mask_span = range(mesh_mask_begin[at], mesh_mask_count[at], masks.len(), "drawable mask")?;
        let clip_guids = masks[mask_span].iter()
            .map(|mask| index(*mask, meshes, false, "mask drawable").map(|mask| mesh_guid(mask.unwrap_or_default())))
            .collect::<Result<Vec<_>, _>>()?;
        // Bit 8 is an inverted mask and bit 2 the multiplicative blend of before 5.3 in the samples; bit 4 is set on
        // every drawable. Bit 1 is taken for that version's additive blend; no sample sets it. Version 6 names the
        // blend in a word per drawable instead: the colour composition in the low byte, the alpha above it.
        let flag = flags[at];
        if flag & !0b1111 != 0 {
            return Err(format!("drawable {} carries a flag Soul2D does not read", mesh_ids[at]));
        }
        let (color_composition, alpha_composition) = match blend_words.as_ref().map(|words| words[at]) {
            None => (match flag & 0b11 {
                0 => "NORMAL",
                1 => "ADD",
                2 => "MULTIPLY",
                _ => return Err(format!("drawable {} is both additive and multiplicative", mesh_ids[at])),
            }, "OVER"),
            Some(word) => composition(word, &format!("drawable {}", mesh_ids[at]))?,
        };
        let texture = index(mesh_texture[at], textures.len().max(1), false, "drawable texture")?.unwrap_or_default();
        let mut forms = Vec::new();
        for keyform in range(mesh_kf_begin[at], mesh_kf_count[at], mesh_keyforms, "drawable keyform")? {
            let positions = keyforms.positions(mesh_position[keyform], vertices, root)?;
            forms.push(keyforms.form(root, mesh_opacity[keyform], Some(whole_draw_order(mesh_draw_order[keyform])?), FormData::ArtMesh { positions }));
        }
        if let Some((begin, channels)) = &colors {
            let span = range(begin[at], mesh_kf_count[at], channels[0].len(), "drawable colour keyform")?;
            for (form, entry) in forms.iter_mut().zip(span) {
                if channels.iter().any(|channel| !(0.0..=1.0).contains(&channel[entry])) {
                    return Err("a .moc3 keyform colour is outside 0 to 1".into());
                }
                form.multiply_color = [channels[0][entry], channels[1][entry], channels[2][entry], 1.0];
                form.screen_color = [channels[3][entry], channels[4][entry], channels[5][entry], 1.0];
            }
        }
        let grid = grid_of(&keyforms, mesh_binding[at], &forms)?;
        let morph = match blend_run(|blend| &blend.meshes, at) {
            Some((blend, run)) => keyforms.blend_targets(blend, run, mesh_binding[at], root, mesh_keyforms, &mesh_ids[at], &mut forms, &mut warnings,
                |keyforms, base, keyform| {
                    let FormData::ArtMesh { positions: base_positions } = &base.data else { return Err("a .moc3 drawable base keyform is not a drawable".into()) };
                    let deltas = keyforms.deltas(mesh_position[keyform], vertices, root)?;
                    let positions = base_positions.iter().zip(deltas).map(|(base, delta)| base + delta).collect();
                    let order = whole_draw_order(f64::from(base.draw_order.unwrap_or(500)) + mesh_draw_order[keyform])?;
                    Ok((base.opacity + mesh_opacity[keyform], Some(order), FormData::ArtMesh { positions }))
                })?,
            None => None,
        };
        let positions = match forms.first().map(|form| &form.data) {
            Some(FormData::ArtMesh { positions }) => positions.clone(),
            _ => return Err(format!("drawable {} has no keyform", mesh_ids[at])),
        };
        let data = SourceData::ArtMesh {
            positions, uvs: uvs[uv_span].to_vec(), indices: triangle_indices, vertex_uids: Some((1..=vertices as u32).collect()),
            texture_guid: format!("moc3-texture-{texture}"), texture_state: "TEXTURE_ATLAS".into(), culling: false,
            color_composition: color_composition.into(), alpha_composition: alpha_composition.into(), clip_guids,
            invert_clipping_mask: flag & 0b1000 != 0,
        };
        mesh_sources.push((at, parent_part, deformer, forms, grid, morph, data));
    }
    // Glue. Every sampled Glue has one keyform and no parameters; it joins its drawable A's Part, as the CMO3
    // Glues do. Vertex UIDs of a runtime drawable are its vertex indices plus one. The 53 Glues of the four
    // samples matched their CMO3 records, and the runtime models then match Cubism Core 6.0.1 within 0.012 px.
    let glues = reader.count(GLUES)?;
    let glue_ids = reader.ids(GLUE_IDS, glues)?;
    let glue_guid = |at: usize| format!("moc3-glue-{}", glue_ids[at]);
    let glue_field = |slot| reader.ints(slot, glues);
    let (glue_binding, glue_kf_begin, glue_kf_count, glue_a, glue_b, glue_rec_begin, glue_rec_count) = (glue_field(GLUE_BINDING)?,
        glue_field(GLUE_KEYFORM_BEGIN)?, glue_field(GLUE_KEYFORM_COUNT)?, glue_field(GLUE_MESH_A)?, glue_field(GLUE_MESH_B)?,
        glue_field(GLUE_RECORD_BEGIN)?, glue_field(GLUE_RECORD_COUNT)?);
    let records = reader.count(GLUE_RECORDS)?;
    let (glue_weights, glue_vertices) = (reader.floats(GLUE_WEIGHTS, records)?, reader.u16s(GLUE_VERTICES, records)?);
    let glue_keyforms = (0..glues).map(|at| usize::try_from(glue_kf_begin[at].saturating_add(glue_kf_count[at])).unwrap_or(0)).max().unwrap_or(0);
    let glue_intensity = reader.floats(GLUE_KEYFORM_INTENSITY, glue_keyforms)?;
    let mut glue_sources = Vec::with_capacity(glues);
    for at in 0..glues {
        let a = index(glue_a[at], meshes, false, "Glue drawable")?.unwrap_or_default();
        let b = index(glue_b[at], meshes, false, "Glue drawable")?.unwrap_or_default();
        if a == b {
            return Err(format!("Glue {} binds a drawable to itself", glue_ids[at]));
        }
        let span = range(glue_rec_begin[at], glue_rec_count[at], records, "Glue record")?;
        if span.len() % 2 != 0 {
            return Err(format!("Glue {} has an odd number of records", glue_ids[at]));
        }
        let mut bind_vertex_uids = Vec::with_capacity(span.len());
        for (slot, record) in span.clone().enumerate() {
            let mesh = if slot % 2 == 0 { a } else { b };
            if i64::from(glue_vertices[record]) >= i64::from(mesh_vertices[mesh]) {
                return Err(format!("Glue {} binds a vertex drawable {} lacks", glue_ids[at], mesh_ids[mesh]));
            }
            bind_vertex_uids.push(u64::from(glue_vertices[record]) + 1);
        }
        let weights = glue_weights[span].to_vec();
        if weights.iter().any(|weight| !(0.0..=1.0).contains(weight)) {
            return Err(format!("Glue {} has a weight outside 0 to 1", glue_ids[at]));
        }
        let mut forms = Vec::new();
        for keyform in range(glue_kf_begin[at], glue_kf_count[at], glue_keyforms, "Glue keyform")? {
            forms.push(keyforms.form(false, 1.0, None, FormData::Glue { intensity: glue_intensity[keyform] }));
        }
        let grid = grid_of(&keyforms, glue_binding[at], &forms)?;
        let parent_part = mesh_sources[a].1;
        children[parent_part.map_or(parts, |part| part)].push(glue_guid(at));
        glue_sources.push((at, parent_part, forms, grid, a, b, bind_vertex_uids, weights));
    }
    // Siblings take the CMO3 order, which lists the topmost child first: the reverse of each drawable's or
    // group Part's place in the draw sequence, and for any other Part or a deformer of the first place among
    // the drawables under it. Without the sequence, or outside it, they stay in index order.
    let mut group_parts = vec![false; parts];
    if reader.offsets[DRAW_SEQUENCE] != 0 {
        let mut rank: HashMap<String, usize> = HashMap::new();
        let mut lower = |guid: String, at: usize| {
            let entry = rank.entry(guid).or_insert(at);
            *entry = (*entry).min(at);
        };
        let units = reader.count(DRAW_UNITS)?;
        let kinds = if reader.offsets[DRAW_UNIT_KINDS] != 0 { reader.ints(DRAW_UNIT_KINDS, units)? } else { vec![0; units] };
        let mut mesh_rank = vec![usize::MAX; meshes];
        for (place, (kind, unit)) in kinds.iter().zip(reader.ints(DRAW_SEQUENCE, units)?).enumerate() {
            match kind {
                0 => {
                    let mesh = index(unit, meshes, false, "draw sequence drawable")?.unwrap_or_default();
                    if mesh_rank[mesh] != usize::MAX {
                        return Err("the .moc3 draw sequence lists a drawable twice".into());
                    }
                    mesh_rank[mesh] = place;
                }
                1 => {
                    let part = index(unit, parts, false, "draw sequence Part")?.unwrap_or_default();
                    group_parts[part] = true;
                    lower(part_guids[part].clone(), place);
                }
                _ => return Err("the .moc3 draw sequence holds a unit Soul2D does not read".into()),
            }
        }
        for (at, parent_part, deformer, ..) in &mesh_sources {
            // A drawable outside the sequence goes above the listed ones, a later index above an earlier one; no
            // sample leaves one out once the sequence is read to its unit count.
            let place = if mesh_rank[*at] == usize::MAX { units + *at } else { mesh_rank[*at] };
            lower(mesh_guid(*at), place);
            let mut part = *parent_part;
            for _ in 0..=parts {
                let Some(current) = part else { break };
                lower(part_guids[current].clone(), place);
                part = part_sources[current].1;
            }
            if part.is_some() {
                return Err("the .moc3 Part hierarchy is cyclic".into());
            }
            let mut deformer = *deformer;
            for _ in 0..=deformers {
                let Some(current) = deformer else { break };
                lower(deformer_guid(current), place);
                deformer = deformer_parents[current];
            }
        }
        for list in &mut children {
            list.sort_by_key(|guid| std::cmp::Reverse(rank.get(guid).copied().unwrap_or(0)));
        }
    }
    // The source list: the root Part, the Parts, the deformers, then the drawables.
    let part_data = |children: Vec<String>, draw_order: i32, composition: Option<PartComposition>, group: bool| {
        let offscreen = composition.is_some();
        let composition = composition.unwrap_or(PartComposition { color: "NORMAL", alpha: "OVER", clip_guids: Vec::new(), invert: false });
        SourceData::Part {
            child_guids: children, draw_order_group: group || offscreen, default_draw_order: draw_order, sketch: false, use_offscreen: offscreen,
            clip_guids: composition.clip_guids, invert_clipping_mask: composition.invert, color_composition: composition.color.into(),
            alpha_composition: composition.alpha.into(),
        }
    };
    let source = |guid: String, id: String, parent_part: Option<String>, target: Option<String>, forms: Vec<Form>, grid: KeyformGrid,
        morph_targets: Option<MorphTargets>, data: SourceData| Source {
        guid, name: id.clone(), id, parent_part_guid: parent_part, target_deformer_guid: target, visible: true, locked: false,
        user_data: String::new(), forms, grid: Some(grid), morph_targets, extended_interpolations: Vec::new(), data,
    };
    let root_form = keyforms.form(false, 1.0, Some(500), FormData::Part);
    let root_grid = KeyformGrid { axes: Vec::new(), cells: vec![KeyformCell { indices: Vec::new(), form_guid: root_form.guid.clone() }] };
    sources.push(source(RUNTIME_ROOT_PART.into(), "__RootPart__".into(), None, None, vec![root_form], root_grid, None,
        part_data(children[parts].clone(), 500, None, false)));
    for (at, parent, forms, grid, morph, composition) in part_sources {
        let draw_order = forms.first().and_then(|form| form.draw_order).unwrap_or(500);
        sources.push(source(part_guids[at].clone(), part_ids[at].clone(), Some(part_guid(parent)), None, forms, grid, morph,
            part_data(children[at].clone(), draw_order, composition, group_parts[at])));
    }
    // Parts the animation animates but the export dropped (sketch and background Parts in the samples) stay as
    // empty Parts under the root, so the animation binds to the runtime model as it binds to the CMO3.
    let mut dropped: Vec<(&String, &String)> = guids.parts.iter().filter(|(id, _)| !part_ids.contains(id)).collect();
    dropped.sort();
    for (id, guid) in dropped {
        let form = keyforms.form(false, 1.0, Some(500), FormData::Part);
        let grid = KeyformGrid { axes: Vec::new(), cells: vec![KeyformCell { indices: Vec::new(), form_guid: form.guid.clone() }] };
        sources.push(source(guid.clone(), id.clone(), Some(RUNTIME_ROOT_PART.into()), None, vec![form], grid, None, part_data(Vec::new(), 500, None, false)));
        if let Some(SourceData::Part { child_guids, .. }) = sources.first_mut().map(|root| &mut root.data) {
            child_guids.push(guid.clone());
        }
    }
    for (at, parent_part, forms, grid, morph, data) in deformer_sources {
        let target = deformer_parents[at].map(deformer_guid);
        sources.push(source(deformer_guid(at), deformer_ids[at].clone(), Some(part_guid(parent_part)), target, forms, grid, morph, data));
    }
    for (at, parent_part, deformer, forms, grid, morph, data) in mesh_sources {
        sources.push(source(mesh_guid(at), mesh_ids[at].clone(), Some(part_guid(parent_part)), deformer.map(deformer_guid), forms, grid, morph, data));
    }
    // Cubism Core applies the Glues in file order (zundamon_ja binds one vertex to two of them), and the native
    // evaluator applies Glue sources from the last to the first, so they are listed in reverse.
    for (at, parent_part, forms, grid, a, b, bind_vertex_uids, weights) in glue_sources.into_iter().rev() {
        let data = SourceData::Glue {
            target_art_mesh_a_guid: mesh_guid(a), target_art_mesh_b_guid: mesh_guid(b), bind_vertex_uids, weights, stale_bind_pairs: Vec::new(),
        };
        sources.push(source(glue_guid(at), glue_ids[at].clone(), Some(part_guid(parent_part)), None, forms, grid, None, data));
    }
    if keyforms.unlisted_bindings > 0 {
        warnings.push(format!("{} blend-shape binding(s) name an entry no parameter carries; they are left out", keyforms.unlisted_bindings));
    }
    let texture_records = textures.iter().enumerate().map(|(at, (name, data))| Ok(Texture {
        guid: format!("moc3-texture-{at}"), name: name.clone(), image_entry: name.clone(), image_size: png_size(data)?,
        image_to_logical: None, wrap_mode: "CLAMP_TO_BORDER".into(), min_filter: "LINEAR_MIPMAP_LINEAR".into(),
        mag_filter: "LINEAR".into(), premultiplied: true,
    })).collect::<Result<Vec<_>, String>>()?;
    let _ = keyforms.reader;
    Ok((Scene {
        format: "live2d.authoring.source.v1", file_format_version: None, name: name.to_owned(),
        canvas: [size[0] as u32, size[1] as u32], root_part_guid: RUNTIME_ROOT_PART.into(), parameters: keyforms.parameters,
        sources, textures: texture_records, brushes: Vec::new(), physics_source: None,
    }, warnings))
}

/// A reference inside a `.model3.json`, kept inside the file's folder.
fn sibling(folder: &Path, reference: &str, what: &str) -> Result<std::path::PathBuf, String> {
    let path = Path::new(reference);
    if path.is_absolute() || path.components().any(|part| part == std::path::Component::ParentDir) {
        return Err(format!("the .model3.json {what} reference must stay inside its folder"));
    }
    Ok(folder.join(path))
}

fn read_runtime_textures(folder: &Path, references: &[Value], budget: u64) -> Result<Vec<(String, Vec<u8>)>, String> {
    if references.len() > MAX_RUNTIME_TEXTURES {
        return Err("too many runtime textures".into());
    }
    let mut remaining = budget;
    let mut textures = Vec::new();
    for texture in references {
        let name = texture.as_str().ok_or("a .model3.json texture reference is not a string")?;
        let file = sibling(folder, name, "texture")?;
        let data = crate::read_bounded_file(&file.to_string_lossy(), MAX_TEXTURE_BYTES.min(remaining), "texture")?;
        remaining -= data.len() as u64;
        textures.push((name.to_owned(), data));
    }
    Ok(textures)
}

/// The native scene of a `.model3.json`: its `.moc3` with every keyform and its textures. A `.moc3` alone names
/// no textures and is refused.
pub(crate) fn attach(path: &str, guids: &RuntimeGuids) -> Result<(NativeScene, Vec<String>), String> {
    if path.to_ascii_lowercase().ends_with(".moc3") {
        return Err("attach the .model3.json: a .moc3 alone names no textures".into());
    }
    let text = crate::read_bounded_file(path, MAX_MODEL3_BYTES, ".model3.json")?;
    let model3: Value = serde_json::from_slice(&text).map_err(|error| format!("the .model3.json is not JSON: {error}"))?;
    let references = model3.get("FileReferences").filter(|value| value.is_object())
        .ok_or("the .model3.json has no FileReferences")?;
    let folder = Path::new(path).parent().unwrap_or(Path::new("."));
    let moc = sibling(folder, references["Moc"].as_str().ok_or("the .model3.json names no Moc")?, "Moc")?;
    let bytes = crate::read_bounded_file(&moc.to_string_lossy(), MAX_MOC3_BYTES, ".moc3")?;
    let textures = read_runtime_textures(folder, references["Textures"].as_array().map(Vec::as_slice).unwrap_or_default(),
        MAX_RUNTIME_TEXTURE_BYTES)?;
    if textures.is_empty() {
        return Err("the .model3.json names no textures".into());
    }
    let name = moc.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let (scene, warnings) = runtime_scene(&bytes, &name, &textures, guids)?;
    let blobs = textures.into_iter().enumerate().map(|(at, (_, data))| (format!("moc3-texture-{at}"), data)).collect();
    Ok((NativeScene::from_runtime(ParsedScene::from_scene(scene), blobs)?, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.moc3` of one parameter (keys -1, 0, 1), one Part and one triangle drawable with three keyforms that slide
    /// along x, laid out as the samples are: a 64-byte header, 480 offsets, then 64-byte aligned sections. The
    /// options add a second drawable without keyform parameters, a draw sequence, version 5 keyform colours and
    /// a blend-shape parameter on the first drawable, and a version 6 offscreen Part.
    struct Fixture {
        version: u8,
        flag: u8,
        blend_word: Option<i32>,
        keys: Vec<f32>,
        second_mesh: bool,
        sequence: Option<Vec<i32>>,
        kinds: Option<Vec<i32>>,
        colors: bool,
        blend_shape: bool,
        offscreen: bool,
        glue: bool,
    }

    impl Fixture {
        fn bytes(&self) -> Vec<u8> {
            let ints = |values: &[i32]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
            let floats = |values: &[f32]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
            let id = |name: &str| { let mut bytes = name.as_bytes().to_vec(); bytes.resize(ID_BYTES, 0); bytes };
            let keyforms = self.keys.len() as i32;
            let meshes = if self.second_mesh { 2 } else { 1 };
            let mut positions = Vec::new();
            for (at, _) in self.keys.iter().enumerate() {
                let shift = 0.1 * at as f32;
                positions.extend([-0.5 + shift, -0.5, 0.5 + shift, -0.5, shift, 0.5]);
            }
            // The second drawable has one keyform; the blend shape adds two difference keyforms, one per key.
            let mut mesh_keyforms = vec![keyforms, 1][..meshes].to_vec();
            let mut opacities = vec![1.0f32; mesh_keyforms.iter().sum::<i32>() as usize];
            let mut draw_orders = vec![500.0f32; opacities.len()];
            if self.second_mesh {
                positions.extend([-0.5, -0.5, 0.5, -0.5, 0.0, 0.5]);
            }
            let target_begin = opacities.len() as i32;
            if self.blend_shape {
                positions.extend([0.0; 6]);
                positions.extend([0.0, 0.0, 0.0, 0.0, 0.0, 0.2]);
                opacities.extend([0.0, 0.0]);
                draw_orders.extend([0.0, 0.0]);
                mesh_keyforms.push(2);
            }
            let total_keyforms = opacities.len() as i32;
            let parameters = if self.blend_shape { 2 } else { 1 };
            let mut keys = self.keys.clone();
            if self.blend_shape {
                keys.extend([0.0, 1.0]);
            }
            let mut counts = vec![0i32; if self.version >= 5 { 37 } else { 24 }];
            counts[PARTS] = 1; counts[MESHES] = meshes as i32; counts[PARAMETERS] = parameters; counts[PART_KEYFORMS] = 1;
            counts[MESH_KEYFORMS] = total_keyforms; counts[KEYFORM_FLOAT_COUNT] = positions.len() as i32;
            counts[PARAMETER_BINDINGS] = 1; counts[KEYFORM_BINDINGS] = 2; counts[KEYTABLES] = 1;
            counts[KEY_COUNT] = keys.len() as i32; counts[VERTEX_FLOATS] = 6 * meshes as i32; counts[INDEX_COUNT] = 3;
            let per_mesh = |a: i32, b: i32| ints(&[a, b][..meshes]);
            let mut sections: Vec<(usize, Vec<u8>)> = vec![
                (CANVAS, floats(&[100.0, 50.0, 50.0, 100.0, 100.0])),
                (PART_IDS, id("PartA")), (PART_BINDING, ints(&[0])), (PART_KEYFORM_BEGIN, ints(&[0])),
                (PART_KEYFORM_COUNT, ints(&[1])), (PART_PARENT, ints(&[-1])), (PART_KEYFORM_DRAW_ORDER, floats(&[500.0])),
                (MESH_IDS, [id("MeshA"), id("MeshB")][..meshes].concat()), (MESH_BINDING, per_mesh(1, 0)),
                (MESH_KEYFORM_BEGIN, per_mesh(0, keyforms)), (MESH_KEYFORM_COUNT, per_mesh(keyforms, 1)),
                (MESH_PARENT_PART, per_mesh(0, 0)), (MESH_PARENT_DEFORMER, per_mesh(-1, -1)),
                (MESH_TEXTURE, per_mesh(0, 0)), (MESH_FLAGS, vec![self.flag, 4][..meshes].to_vec()), (MESH_VERTICES, per_mesh(3, 3)),
                (MESH_VERTEX_BEGIN, per_mesh(0, 6)), (MESH_INDEX_BEGIN, per_mesh(0, 0)), (MESH_INDICES, per_mesh(3, 3)),
                (MESH_MASK_BEGIN, per_mesh(0, 0)), (MESH_MASK_COUNT, per_mesh(0, 0)),
                (MESH_KEYFORM_OPACITY, floats(&opacities)),
                (MESH_KEYFORM_DRAW_ORDER, floats(&draw_orders)),
                (MESH_KEYFORM_POSITION, ints(&(0..total_keyforms).map(|k| 6 * k).collect::<Vec<_>>())),
                (KEYFORM_FLOATS, floats(&positions)),
                (PARAMETER_IDS, [id("ParamA"), id("ParamB")][..parameters as usize].concat()),
                (PARAMETER_MAX, floats(&[1.0, 1.0][..parameters as usize])), (PARAMETER_MIN, floats(&[-1.0, 0.0][..parameters as usize])),
                (PARAMETER_DEFAULT, floats(&[0.0, 0.0][..parameters as usize])), (PARAMETER_REPEAT, ints(&[0, 0][..parameters as usize])),
                (PARAMETER_KEYTABLE_BEGIN, ints(&[0, -1][..parameters as usize])), (PARAMETER_KEYTABLE_COUNT, ints(&[1, 0][..parameters as usize])),
                (PARAMETER_BINDING_KEYTABLE, ints(&[0])), (KEYFORM_BINDING_BEGIN, ints(&[0, 0])),
                (KEYFORM_BINDING_COUNT, ints(&[0, 1])), (KEYTABLE_KEY_BEGIN, ints(&[0])), (KEYTABLE_KEY_COUNT, ints(&[keyforms])),
                (KEYS, floats(&keys)), (UVS, floats(&[0.0, 0.0, 1.0, 0.0, 0.5, 1.0, 0.0, 0.0, 1.0, 0.0, 0.5, 1.0][..6 * meshes])),
                (INDICES, [0u16, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect()),
            ];
            if let Some(word) = self.blend_word {
                sections.push((MESH_BLEND_WORDS, ints(&vec![word; meshes])));
            }
            if let Some(sequence) = &self.sequence {
                counts[DRAW_UNITS] = sequence.len() as i32;
                sections.push((DRAW_SEQUENCE, ints(sequence)));
            }
            if let Some(kinds) = &self.kinds {
                sections.push((DRAW_UNIT_KINDS, ints(kinds)));
            }
            if self.colors {
                counts[COLOR_KEYFORMS] = total_keyforms;
                let channel = |value: f32| floats(&vec![value; total_keyforms as usize]);
                sections.push((MESH_COLOR_BEGIN, per_mesh(0, keyforms)));
                sections.extend([(KEYFORM_MULTIPLY_RED, channel(0.5)), (KEYFORM_MULTIPLY_RED + 1, channel(1.0)), (KEYFORM_MULTIPLY_RED + 2, channel(1.0)),
                    (KEYFORM_SCREEN_RED, channel(0.0)), (KEYFORM_SCREEN_RED + 1, channel(0.25)), (KEYFORM_SCREEN_RED + 2, channel(0.0))]);
            }
            if self.blend_shape {
                // ParamB owns one entry with keys 0 and 1; one binding ties it to MeshA's two difference keyforms
                // and to a constraint group on ParamA with two points.
                counts[BLEND_SHAPE_PARAMETERS] = 1; counts[BLEND_SHAPE_BINDINGS] = 1; counts[MESH_BLEND_SOURCES] = 1;
                counts[CONSTRAINT_GROUP_REF_COUNT] = 1; counts[CONSTRAINT_GROUPS] = 1; counts[CONSTRAINT_POINTS] = 2;
                sections.extend([
                    (PARAMETER_IS_BLEND_SHAPE, ints(&[0, 1])), (PARAMETER_BLEND_ENTRY_BEGIN, ints(&[-1, 0])), (PARAMETER_BLEND_ENTRY_COUNT, ints(&[0, 1])),
                    (BLEND_SHAPE_KEY_BEGIN, ints(&[keyforms])), (BLEND_SHAPE_KEY_COUNT, ints(&[2])),
                    (BLEND_BINDING_PARAMETER, ints(&[0])), (BLEND_BINDING_KEYFORM_BEGIN, ints(&[target_begin])), (BLEND_BINDING_KEYFORM_COUNT, ints(&[2])),
                    (BLEND_BINDING_CONSTRAINT_BEGIN, ints(&[0])), (BLEND_BINDING_CONSTRAINT_COUNT, ints(&[1])),
                    (MESH_BLEND_SOURCE, ints(&[0])), (MESH_BLEND_BINDING_BEGIN, ints(&[0])), (MESH_BLEND_BINDING_COUNT, ints(&[1])),
                    (CONSTRAINT_GROUP_REFS, ints(&[0])), (CONSTRAINT_GROUP_PARAMETER, ints(&[0])),
                    (CONSTRAINT_POINT_BEGIN, ints(&[0])), (CONSTRAINT_POINT_COUNT, ints(&[2])),
                    (CONSTRAINT_POINT_VALUE, floats(&[-1.0, 1.0])), (CONSTRAINT_POINT_WEIGHT, floats(&[0.5, 1.0])),
                ]);
            }
            if self.glue {
                // GlueAB binds MeshA's first vertex to MeshB's second, weights 0.5 and 1, intensity 0.75; GlueAB2
                // binds MeshA's second vertex to MeshB's third with weights 1 and 0.
                counts[GLUES] = 2; counts[GLUE_RECORDS] = 4;
                sections.extend([(GLUE_IDS, [id("GlueAB"), id("GlueAB2")].concat()), (GLUE_BINDING, ints(&[0, 0])),
                    (GLUE_KEYFORM_BEGIN, ints(&[0, 1])), (GLUE_KEYFORM_COUNT, ints(&[1, 1])),
                    (GLUE_MESH_A, ints(&[0, 0])), (GLUE_MESH_B, ints(&[1, 1])), (GLUE_RECORD_BEGIN, ints(&[0, 2])), (GLUE_RECORD_COUNT, ints(&[2, 2])),
                    (GLUE_WEIGHTS, floats(&[0.5, 1.0, 1.0, 0.0])), (GLUE_VERTICES, [0u16, 1, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect()),
                    (GLUE_KEYFORM_INTENSITY, floats(&[0.75, 1.0]))]);
            }
            if self.offscreen {
                counts[OFFSCREEN_PARTS] = 1; counts[OFFSCREEN_KEYFORMS] = 1;
                sections.extend([(OFFSCREEN_PART_LIST, ints(&[0])), (OFFSCREEN_FLAGS, vec![4 | 8]), (OFFSCREEN_BLEND_WORDS, ints(&[262])),
                    (OFFSCREEN_MASK_BEGIN, ints(&[0])), (OFFSCREEN_MASK_COUNT, ints(&[0])), (OFFSCREEN_KEYFORM_OPACITY, floats(&[0.5]))]);
            }
            sections.insert(0, (COUNTS, ints(&counts)));
            let mut bytes = vec![0u8; TABLE + 4 * 480];
            bytes[..4].copy_from_slice(b"MOC3");
            bytes[4] = self.version;
            let mut offsets = vec![0u32; 480];
            for (slot, data) in sections {
                while bytes.len() % 64 != 0 { bytes.push(0); }
                offsets[slot] = bytes.len() as u32;
                bytes.extend(data);
            }
            while bytes.len() % 64 != 0 { bytes.push(0); }
            for (slot, offset) in offsets.iter().enumerate() {
                bytes[TABLE + 4 * slot..TABLE + 4 * slot + 4].copy_from_slice(&offset.to_le_bytes());
            }
            bytes
        }
    }

    fn fixture() -> Fixture {
        Fixture { version: 3, flag: 4, blend_word: None, keys: vec![-1.0, 0.0, 1.0], second_mesh: false, sequence: None, kinds: None, colors: false,
            blend_shape: false, offscreen: false, glue: false }
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])))
            .write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn mesh(scene: &Scene) -> &Source {
        scene.sources.iter().find(|source| source.id == "MeshA").unwrap()
    }

    fn error<T>(result: Result<T, String>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[test]
    fn a_runtime_scene_carries_the_keyforms_in_canvas_pixels_with_the_parameter_keys() {
        let (scene, warnings) = runtime_scene(&fixture().bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(scene.canvas, [100, 100]);
        assert_eq!(scene.parameters.len(), 1);
        assert_eq!((scene.parameters[0].id.as_str(), scene.parameters[0].guid.as_str()), ("ParamA", "moc3-parameter-ParamA"));
        assert_eq!(scene.sources.iter().map(|source| source.id.as_str()).collect::<Vec<_>>(), ["__RootPart__", "PartA", "MeshA"]);
        let mesh = mesh(&scene);
        assert_eq!(mesh.parent_part_guid.as_deref(), Some("moc3-part-PartA"));
        let grid = mesh.grid.as_ref().unwrap();
        assert_eq!(grid.axes.len(), 1);
        assert_eq!(grid.axes[0].keys, [-1.0, 0.0, 1.0]);
        assert_eq!(grid.axes[0].parameter_id.as_deref(), Some("ParamA"));
        assert_eq!(grid.cells.iter().map(|cell| cell.indices.clone()).collect::<Vec<_>>(), [vec![0], vec![1], vec![2]]);
        // The base keyform is the second: normalized (-0.4, -0.5) is pixel (10, 0) with 100 pixels per unit from (50, 50).
        let FormData::ArtMesh { positions } = &mesh.forms[1].data else { panic!("not an art mesh form") };
        let rounded: Vec<f64> = positions.iter().map(|value| (value * 10.0).round() / 10.0).collect();
        assert_eq!(rounded, [10.0, 0.0, 110.0, 0.0, 60.0, 100.0]);
        assert_eq!(mesh.forms[1].coordinate_type.as_deref(), Some("Canvas"));
        let SourceData::ArtMesh { texture_guid, indices, color_composition, alpha_composition, .. } = &mesh.data else { panic!() };
        assert_eq!((texture_guid.as_str(), indices.as_slice()), ("moc3-texture-0", &[0, 1, 2][..]));
        assert_eq!((color_composition.as_str(), alpha_composition.as_str()), ("NORMAL", "OVER"));
        assert_eq!(scene.textures[0].image_size, [2, 2]);
        let root = &scene.sources[0];
        let SourceData::Part { child_guids, .. } = &root.data else { panic!() };
        assert_eq!(child_guids, &["moc3-part-PartA"]);
        let native = NativeScene::from_runtime(ParsedScene::from_scene(scene), vec![("moc3-texture-0".into(), png())]).unwrap();
        assert_eq!(native.parameters().len(), 1);
    }

    #[test]
    fn the_animation_guids_name_the_runtime_parameters_and_the_parts_it_animates() {
        let animation = json!({"scenes": [{"tracks": [
            {"resourceGuid": "r", "parameterCurves": [{"parameterId": "ParamA", "parameterGuid": "p-guid"}],
             "partOpacityCurves": [{"partId": "PartA", "partGuid": "a-guid"}, {"partId": "PartSketch", "partGuid": "s-guid"}]},
            {"resourceGuid": "other", "parameterCurves": [{"parameterId": "ParamA", "parameterGuid": "ignored"}]}
        ]}]});
        let guids = RuntimeGuids::from_animation(&animation, "r").unwrap();
        let (scene, _) = runtime_scene(&fixture().bytes(), "m", &[("t.png".into(), png())], &guids).unwrap();
        assert_eq!(scene.parameters[0].guid, "p-guid");
        let ids: Vec<(&str, &str)> = scene.sources.iter().map(|source| (source.id.as_str(), source.guid.as_str())).collect();
        assert!(ids.contains(&("PartA", "a-guid")));
        // The Part the export dropped stays as an empty Part under the root, so the curve still binds.
        assert!(ids.contains(&("PartSketch", "s-guid")));
        let SourceData::Part { child_guids, .. } = &scene.sources[0].data else { panic!() };
        assert_eq!(child_guids, &["a-guid", "s-guid"]);
        let conflicting = json!({"scenes": [{"tracks": [{"resourceGuid": "r", "parameterCurves": [
            {"parameterId": "ParamA", "parameterGuid": "one"}, {"parameterId": "ParamA", "parameterGuid": "two"}]}]}]});
        assert!(error(RuntimeGuids::from_animation(&conflicting, "r")).contains("two GUIDs"));
    }

    #[test]
    fn blend_flags_and_version_6_blend_words_name_the_compositions() {
        let legacy = Fixture { flag: 4 | 2, ..fixture() };
        let (scene, _) = runtime_scene(&legacy.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        let SourceData::ArtMesh { color_composition, .. } = &mesh(&scene).data else { panic!() };
        assert_eq!(color_composition, "MULTIPLY");
        let v6 = Fixture { version: 6, blend_word: Some(262), ..fixture() };
        let (scene, _) = runtime_scene(&v6.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        let SourceData::ArtMesh { color_composition, alpha_composition, .. } = &mesh(&scene).data else { panic!() };
        assert_eq!((color_composition.as_str(), alpha_composition.as_str()), ("MULTIPLY_R2", "ATOP"));
        let unknown = Fixture { version: 6, blend_word: Some(99), ..fixture() };
        assert!(error(runtime_scene(&unknown.bytes(), "m", &[], &RuntimeGuids::default())).contains("blend mode"));
        let inverted = Fixture { flag: 4 | 8, ..fixture() };
        let (scene, _) = runtime_scene(&inverted.bytes(), "m", &[], &RuntimeGuids::default()).unwrap();
        let SourceData::ArtMesh { invert_clipping_mask, .. } = &mesh(&scene).data else { panic!() };
        assert!(invert_clipping_mask);
    }

    #[test]
    fn malformed_runtime_files_are_refused() {
        let bytes = fixture().bytes();
        assert!(runtime_scene(&bytes[..bytes.len() - 64], "m", &[], &RuntimeGuids::default()).is_err());
        let descending = Fixture { keys: vec![1.0, 0.0, -1.0], ..fixture() };
        assert!(error(runtime_scene(&descending.bytes(), "m", &[], &RuntimeGuids::default())).contains("ascending"));
        let mut no_texture = fixture().bytes();
        // A drawable naming a texture the .model3.json does not list.
        let mesh_texture = u32::from_le_bytes(no_texture[TABLE + 4 * MESH_TEXTURE..TABLE + 4 * MESH_TEXTURE + 4].try_into().unwrap()) as usize;
        no_texture[mesh_texture..mesh_texture + 4].copy_from_slice(&1i32.to_le_bytes());
        assert!(error(runtime_scene(&no_texture, "m", &[("t.png".into(), png())], &RuntimeGuids::default())).contains("texture"));
        assert!(error(attach("model.moc3", &RuntimeGuids::default())).contains("names no textures"));
        assert!(png_size(b"not a png").is_err());
    }

    #[test]
    fn model3_references_stay_inside_their_folder_and_name_textures() {
        let folder = Path::new("/models/a");
        assert_eq!(sibling(folder, "runtime/a.moc3", "Moc").unwrap(), folder.join("runtime/a.moc3"));
        assert!(error(sibling(folder, "../b/a.moc3", "Moc")).contains("inside its folder"));
        assert!(error(sibling(folder, "/etc/passwd", "texture")).contains("inside its folder"));
        let dir = std::env::temp_dir().join(format!("soul2d-moc3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.moc3"), fixture().bytes()).unwrap();
        std::fs::write(dir.join("a.model3.json"), r#"{"FileReferences": {"Moc": "a.moc3", "Textures": []}}"#).unwrap();
        let result = attach(&dir.join("a.model3.json").to_string_lossy(), &RuntimeGuids::default());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(error(result).contains("names no textures"));
    }

    #[test]
    fn runtime_texture_budget_counts_repeated_slots_and_keeps_their_order() {
        let dir = std::env::temp_dir().join(format!("soul2d-moc3-budget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = png();
        std::fs::write(dir.join("a.png"), &data).unwrap();
        std::fs::write(dir.join("b.png"), &data).unwrap();
        let references = [json!("a.png"), json!("b.png"), json!("a.png")];
        let budget = data.len() as u64 * 2;
        let loaded = read_runtime_textures(&dir, &references[..2], budget).unwrap();
        assert_eq!(loaded.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["a.png", "b.png"]);
        assert_eq!(loaded[0].1, data);
        let repeated = [json!("a.png"), json!("a.png")];
        assert_eq!(read_runtime_textures(&dir, &repeated, budget).unwrap().len(), 2);
        assert!(error(read_runtime_textures(&dir, &references, budget)).contains("size limit"));
        assert!(error(read_runtime_textures(&dir, &repeated, data.len() as u64 - 1)).contains("size limit"));
        let excessive = vec![json!("missing.png"); MAX_RUNTIME_TEXTURES + 1];
        assert!(error(read_runtime_textures(&dir, &excessive, MAX_RUNTIME_TEXTURE_BYTES)).contains("too many runtime textures"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn children_of(scene: &Scene, id: &str) -> Vec<String> {
        let part = scene.sources.iter().find(|source| source.id == id).unwrap();
        let SourceData::Part { child_guids, .. } = &part.data else { panic!("not a Part") };
        child_guids.clone()
    }

    #[test]
    fn the_draw_sequence_orders_siblings_topmost_first() {
        let two = Fixture { second_mesh: true, ..fixture() };
        let (scene, _) = runtime_scene(&two.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_eq!(children_of(&scene, "PartA"), ["moc3-mesh-MeshA", "moc3-mesh-MeshB"]);
        // MeshB drawn first lies at the bottom, so the CMO3 lists it last.
        let bottom_b = Fixture { second_mesh: true, sequence: Some(vec![1, 0]), ..fixture() };
        let (scene, _) = runtime_scene(&bottom_b.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_eq!(children_of(&scene, "PartA"), ["moc3-mesh-MeshA", "moc3-mesh-MeshB"]);
        let bottom_a = Fixture { second_mesh: true, sequence: Some(vec![0, 1]), ..fixture() };
        let (scene, _) = runtime_scene(&bottom_a.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_eq!(children_of(&scene, "PartA"), ["moc3-mesh-MeshB", "moc3-mesh-MeshA"]);
        let twice = Fixture { second_mesh: true, sequence: Some(vec![0, 0]), ..fixture() };
        assert!(error(runtime_scene(&twice.bytes(), "m", &[], &RuntimeGuids::default())).contains("twice"));
        // Version 6 lists the offscreen Part as a unit of its own; a drawable the sequence leaves out goes above
        // the listed ones, and a later index above an earlier one.
        let unlisted = Fixture { version: 6, blend_word: Some(0), offscreen: true, second_mesh: true, sequence: Some(vec![0, 1, 1]),
            kinds: Some(vec![1, 0, 0]), ..fixture() };
        assert!(error(runtime_scene(&unlisted.bytes(), "m", &[], &RuntimeGuids::default())).contains("twice"));
        let unlisted = Fixture { sequence: Some(vec![0, 1, 0]), kinds: Some(vec![1, 0, 1]), ..unlisted };
        let (scene, _) = runtime_scene(&unlisted.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_eq!(children_of(&scene, "PartA"), ["moc3-mesh-MeshA", "moc3-mesh-MeshB"]);
        let unlisted = Fixture { sequence: Some(vec![0, 0, 0]), kinds: Some(vec![1, 0, 1]), ..unlisted };
        let (scene, _) = runtime_scene(&unlisted.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_eq!(children_of(&scene, "PartA"), ["moc3-mesh-MeshB", "moc3-mesh-MeshA"]);
    }

    #[test]
    fn version_5_keyform_colours_and_blend_shapes_become_tints_and_morph_targets() {
        let v5 = Fixture { version: 5, second_mesh: true, colors: true, blend_shape: true, ..fixture() };
        let (scene, warnings) = runtime_scene(&v5.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mesh = mesh(&scene);
        assert_eq!(mesh.forms[0].multiply_color, [0.5, 1.0, 1.0, 1.0]);
        assert_eq!(mesh.forms[0].screen_color, [0.0, 0.25, 0.0, 1.0]);
        assert_eq!(scene.parameters[1].parameter_type, "MORPH_TARGET");
        // The grid keeps the three keyforms; the key-1 difference is one more Form, the key-0 one is skipped.
        assert_eq!((mesh.grid.as_ref().unwrap().cells.len(), mesh.forms.len()), (3, 4));
        let morph = mesh.morph_targets.as_ref().unwrap();
        assert_eq!(morph.targets.len(), 1);
        assert_eq!((morph.targets[0].parameter_id.as_str(), morph.targets[0].key_value, morph.targets[0].form_guid.as_str()),
            ("ParamB", 1.0, mesh.forms[3].guid.as_str()));
        // Base keyform (key 0) plus 0.2 × 100 pixels per unit on the third vertex's y; the tint carries over.
        let FormData::ArtMesh { positions } = &mesh.forms[3].data else { panic!() };
        let rounded: Vec<f64> = positions.iter().map(|value| (value * 10.0).round() / 10.0).collect();
        assert_eq!(rounded, [10.0, 0.0, 110.0, 0.0, 60.0, 120.0]);
        assert_eq!((mesh.forms[3].opacity, mesh.forms[3].draw_order, mesh.forms[3].multiply_color[0]), (1.0, Some(500), 0.5));
        let constraints: Vec<(&str, &str, f64, f64)> = morph.blend_weight_constraints.iter().map(|c| (
            c.morph_target_parameter_id.as_str(), c.constraint_parameter_id.as_str(), c.constraint_parameter_value, c.blend_weight)).collect();
        assert_eq!(constraints, [("ParamB", "ParamA", -1.0, 0.5), ("ParamB", "ParamA", 1.0, 1.0)]);
        let second = scene.sources.iter().find(|source| source.id == "MeshB").unwrap();
        assert!(second.morph_targets.is_none());
        assert_eq!(second.forms[0].multiply_color, [0.5, 1.0, 1.0, 1.0]);
        assert!(NativeScene::from_runtime(ParsedScene::from_scene(scene), vec![("moc3-texture-0".into(), png())]).is_ok());
    }

    #[test]
    fn glue_records_bind_the_vertices_of_two_drawables_by_index() {
        let glued = Fixture { second_mesh: true, glue: true, ..fixture() };
        let (scene, warnings) = runtime_scene(&glued.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let SourceData::ArtMesh { vertex_uids, .. } = &mesh(&scene).data else { panic!() };
        assert_eq!(vertex_uids.as_deref(), Some(&[1, 2, 3][..]));
        let glue = scene.sources.iter().find(|source| source.id == "GlueAB").unwrap();
        assert_eq!((glue.guid.as_str(), glue.parent_part_guid.as_deref()), ("moc3-glue-GlueAB", Some("moc3-part-PartA")));
        assert!(children_of(&scene, "PartA").contains(&glue.guid));
        let SourceData::Glue { target_art_mesh_a_guid, target_art_mesh_b_guid, bind_vertex_uids, weights, stale_bind_pairs } = &glue.data else { panic!() };
        assert_eq!((target_art_mesh_a_guid.as_str(), target_art_mesh_b_guid.as_str()), ("moc3-mesh-MeshA", "moc3-mesh-MeshB"));
        assert_eq!((bind_vertex_uids.as_slice(), weights.as_slice(), stale_bind_pairs.len()), (&[1, 2][..], &[0.5, 1.0][..], 0));
        let FormData::Glue { intensity } = &glue.forms[0].data else { panic!() };
        assert_eq!((*intensity, glue.grid.as_ref().unwrap().cells.len()), (0.75, 1));
        // Listed in reverse, so the evaluator's last-to-first pass applies them in file order as Core does.
        let glue_ids: Vec<&str> = scene.sources.iter().filter(|source| matches!(source.data, SourceData::Glue { .. })).map(|source| source.id.as_str()).collect();
        assert_eq!(glue_ids, ["GlueAB2", "GlueAB"]);
        // The native preview applies it: MeshA's first vertex leaves its keyform position.
        let verts = |scene: Scene| {
            let native = NativeScene::from_runtime(ParsedScene::from_scene(scene), vec![("moc3-texture-0".into(), png())]).unwrap();
            let (model, warnings) = native.render_model(&std::collections::BTreeMap::new()).unwrap();
            assert!(warnings.iter().all(|warning| !warning.contains("Glue")), "{warnings:?}");
            fn first(node: &Value) -> Option<Value> {
                if node["name"] == "MeshA" { return Some(node["mesh"]["verts"].clone()); }
                node["children"].as_array().into_iter().flatten().find_map(first)
            }
            first(&model["nodes"]).unwrap()
        };
        let plain = Fixture { second_mesh: true, ..fixture() };
        let (unglued, _) = runtime_scene(&plain.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        assert_ne!(verts(scene), verts(unglued));
        let odd = Fixture { second_mesh: true, glue: true, ..fixture() };
        let mut bytes = odd.bytes();
        let count = u32::from_le_bytes(bytes[TABLE + 4 * GLUE_RECORD_COUNT..TABLE + 4 * GLUE_RECORD_COUNT + 4].try_into().unwrap()) as usize;
        bytes[count..count + 4].copy_from_slice(&1i32.to_le_bytes());
        assert!(error(runtime_scene(&bytes, "m", &[], &RuntimeGuids::default())).contains("odd"));
    }

    #[test]
    fn version_6_offscreen_parts_compose_with_their_blend_word_flags_and_opacity() {
        let v6 = Fixture { version: 6, blend_word: Some(0), offscreen: true, ..fixture() };
        let (scene, _) = runtime_scene(&v6.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        let part = scene.sources.iter().find(|source| source.id == "PartA").unwrap();
        assert_eq!(part.forms[0].opacity, 0.5);
        let SourceData::Part { use_offscreen, draw_order_group, color_composition, alpha_composition, invert_clipping_mask, .. } = &part.data else { panic!() };
        assert!(*use_offscreen && *draw_order_group && *invert_clipping_mask);
        assert_eq!((color_composition.as_str(), alpha_composition.as_str()), ("MULTIPLY_R2", "ATOP"));
        let plain = Fixture { version: 6, blend_word: Some(0), ..fixture() };
        let (scene, _) = runtime_scene(&plain.bytes(), "m", &[("t.png".into(), png())], &RuntimeGuids::default()).unwrap();
        let part = scene.sources.iter().find(|source| source.id == "PartA").unwrap();
        let SourceData::Part { use_offscreen, draw_order_group, .. } = &part.data else { panic!() };
        assert!(!*use_offscreen && !*draw_order_group && part.forms[0].opacity == 1.0);
    }
}
