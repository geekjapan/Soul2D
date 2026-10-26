//! Application-shell operations of the D editor that live outside the model editing core:
//! `import.inp` / `merge.inp` (`incImportINP`, `ngMergeImportedPuppet` in `commands/puppet/file.d`),
//! `import.folder` (`incImportFolder`), `merge.images` (`incCreatePartsFromFiles`) and the `tool.*` puppet
//! tools of `commands/puppet/tool.d` and `utils/repair.d`.
//!
//! The pure functions work on the document JSON and texture slots; the `Session` methods only read the
//! request, call them and commit one undo step.

use crate::document::{Document, TextureBlob};
use crate::model_import::{self, ImageFile, Merged, Texture};
use crate::{prepare_model, read_bounded_file, required_string, resource_ids, valid_id, Session};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_INP_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FOLDER_IMAGES: usize = 1024;
const MAX_FOLDER_PIXELS: u64 = 256 * 1024 * 1024;
/// Longest `png_base64` text of `texture.set`: the MCP HTTP body limit. Larger images come in by `path`.
// Below the 8 MiB MCP HTTP body limit, so a value at the cap still fits its JSON-RPC envelope.
const MAX_TEXTURE_SET_BASE64: usize = 7 * 1024 * 1024;
const NO_TEXTURE: u64 = u32::MAX as u64;
/// D `incRebleedTextures` bleeds with the default of `incColorBleedPixels`.
const BLEED_ITERATIONS: i32 = 32;
pub(crate) const BLEED_BUDGET: u64 = 4_000_000_000;
/// The extension entry `ImportSessionDataCommand` copies between puppets.
pub const SESSION_DATA_KEY: &str = "com.inochi2d.inochi-session.bindings";

// ------------------------------------------------------------------------------------ id remapping

fn remap_field(value: &mut Value, key: &str, map: &HashMap<u32, u32>) {
    let Some(id) = value.get(key).and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) else {
        return;
    };
    if let Some(new) = map.get(&id) {
        value[key] = json!(new);
    }
}

/// Rewrites every node UUID and every reference to a node or parameter UUID the model keeps:
/// `masks[].source`, SimplePhysics `param`, welds and depth rig bindings of nodes; parameter and group
/// UUIDs, their binding `node`s, and animation lane `uuid`s.
fn remap_node(node: &mut Value, map: &HashMap<u32, u32>) {
    remap_field(node, "uuid", map);
    for mask in node.get_mut("masks").and_then(Value::as_array_mut).into_iter().flatten() {
        remap_field(mask, "source", map);
    }
    if node.get("type").and_then(Value::as_str) == Some("SimplePhysics") {
        remap_field(node, "param", map);
    }
    for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
        remap_node(child, map);
    }
}

fn remap_parameters(list: &mut [Value], map: &HashMap<u32, u32>) {
    for item in list {
        remap_field(item, "uuid", map);
        remap_field(item, "groupUUID", map);
        for binding in item.get_mut("bindings").and_then(Value::as_array_mut).into_iter().flatten() {
            remap_field(binding, "node", map);
        }
        if let Some(children) = item.get_mut("children").and_then(Value::as_array_mut) {
            remap_parameters(children, map);
        }
    }
}

fn remap_lanes(model: &mut Value, map: &HashMap<u32, u32>) {
    for animation in model.get_mut("animations").and_then(Value::as_object_mut).into_iter().flat_map(|a| a.values_mut()) {
        for lane in animation.get_mut("lanes").and_then(Value::as_array_mut).into_iter().flatten() {
            remap_field(lane, "uuid", map);
        }
        let lists = animation.as_object_mut().into_iter().flat_map(|object| object.iter_mut());
        let lists = lists.filter(|(key, _)| *key == "bookmarks" || *key == "controllers");
        for item in lists.flat_map(|(_, list)| list.as_array_mut().into_iter().flatten()) {
            for id in item.get_mut("parameters").and_then(Value::as_array_mut).into_iter().flatten() {
                if let Some(new) = id.as_u64().and_then(|old| u32::try_from(old).ok()).and_then(|old| map.get(&old)) {
                    *id = json!(new);
                }
            }
            // A controller is pinned to a Part, which the merge renumbers with the parameters.
            for end in ["start", "end"] {
                if let Some(anchor) = item.get_mut(end) {
                    remap_field(anchor, "node", map);
                }
            }
        }
        for form in animation.get_mut("forms").and_then(Value::as_array_mut).into_iter().flatten() {
            remap_field(form, "node", map);
        }
    }
}

fn remap_subtree(root: &mut Value, map: &HashMap<u32, u32>) -> Result<(), String> {
    remap_node(root, map);
    crate::bindings::remap_copied_bindings(root, map)?;
    crate::weld::remap_copied_links(root, map);
    Ok(())
}

fn lowest_unused(used: &mut HashSet<u32>) -> Result<u32, String> {
    let id = (1..u32::MAX).find(|id| !used.contains(id)).ok_or("no unused resource UUID remains")?;
    used.insert(id);
    Ok(id)
}

fn node_ids_post_order(node: &Value, out: &mut Vec<u32>) {
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        node_ids_post_order(child, out);
    }
    if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
        out.push(id);
    }
}

// ------------------------------------------------------------------------------------ repair tools

/// D `incRegenerateNodeIDs`: every node (children first, the root last) gets a new UUID that no node,
/// parameter or group uses; references to nodes are rewritten. Returns how many nodes changed.
pub fn regenerate_node_ids(model: &Value) -> Result<(Value, usize), String> {
    let mut model = model.clone();
    let mut used = HashSet::new();
    resource_ids(&model, &mut used);
    let mut order = Vec::new();
    node_ids_post_order(model.get("nodes").ok_or("model is missing nodes")?, &mut order);
    let mut map = HashMap::new();
    for id in &order {
        map.insert(*id, lowest_unused(&mut used)?);
    }
    remap_subtree(model.get_mut("nodes").ok_or("model is missing nodes")?, &map)?;
    if let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) {
        remap_parameters(parameters, &map);
    }
    Ok((model, order.len()))
}

/// D `GenerateFakeLayerNameCommand`: every Part's `psdLayerPath` becomes `/` + its name.
pub fn fake_layer_names(model: &Value) -> Value {
    fn walk(node: &mut Value) {
        if node.get("type").and_then(Value::as_str) == Some("Part") {
            let path = format!("/{}", node.get("name").and_then(Value::as_str).unwrap_or_default());
            node["psdLayerPath"] = json!(path);
        }
        for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
            walk(child);
        }
    }
    let mut model = model.clone();
    if let Some(nodes) = model.get_mut("nodes") {
        walk(nodes);
    }
    model
}

fn blob_format(blob: &TextureBlob) -> Result<image::ImageFormat, String> {
    match blob.tag {
        0 => Ok(image::ImageFormat::Png),
        1 => Ok(image::ImageFormat::Tga),
        tag => Err(format!("unknown texture format tag {tag}")),
    }
}

/// Decodes every slot, lets `change` rewrite the RGBA pixels and encodes them in the slot's own format.
/// Returns `None` when no pixel changed.
fn map_textures(
    textures: &[TextureBlob],
    mut change: impl FnMut(image::RgbaImage) -> Result<image::RgbaImage, String>,
) -> Result<Option<Vec<TextureBlob>>, String> {
    let mut out = Vec::with_capacity(textures.len());
    let mut changed = false;
    for (slot, blob) in textures.iter().enumerate() {
        let format = blob_format(blob)?;
        let before = image::load_from_memory_with_format(&blob.data, format)
            .map_err(|error| format!("texture {slot} could not be decoded: {error}"))?
            .to_rgba8();
        let after = change(before.clone())?;
        if after == before {
            out.push(blob.clone());
            continue;
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        after.write_to(&mut bytes, format).map_err(|error| format!("texture {slot} could not be encoded: {error}"))?;
        out.push(TextureBlob { tag: blob.tag, data: bytes.into_inner() });
        changed = true;
    }
    Ok(changed.then_some(out))
}

/// rgb = rgb * alpha / 255 (D `inTexPremultiply`).
fn premultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
}

/// D `incPremultTextures` (`inTexPremultiply`): rgb = rgb * alpha / 255 in every texture slot.
pub fn premultiply_textures(textures: &[TextureBlob]) -> Result<Option<Vec<TextureBlob>>, String> {
    map_textures(textures, |mut image| {
        premultiply_rgba(&mut image);
        Ok(image)
    })
}

/// D `incRebleedTextures` (`incColorBleedPixels`): colour spreads into fully transparent pixels.
pub fn rebleed_textures(textures: &[TextureBlob]) -> Result<Option<Vec<TextureBlob>>, String> {
    let mut budget = crate::cv::WorkBudget::new(BLEED_BUDGET);
    map_textures(textures, |image| {
        let (width, height) = image.dimensions();
        let pixels = crate::pixels::PixelImage::from_pixels(width, height, crate::pixels::PixelFormat::Rgba, image.into_raw())?;
        let bled = pixels.bleed(BLEED_ITERATIONS, &mut budget)?;
        image::RgbaImage::from_raw(width, height, bled.pixels().to_vec()).ok_or_else(|| "bleed changed the image size".to_owned())
    })
}

// ------------------------------------------------------------------------------------ INP merge

fn transform_of(model: &Value) -> Value {
    model["nodes"].get("transform").cloned().unwrap_or(Value::Null)
}

fn shift_slots(node: &mut Value, offset: u64) {
    for entry in node.get_mut("textures").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(slot) = entry.as_u64().filter(|slot| *slot != NO_TEXTURE) {
            *entry = json!(slot + offset);
        }
    }
    for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
        shift_slots(child, offset);
    }
}

fn unique_animation_name(taken: &serde_json::Map<String, Value>, base: &str) -> String {
    let base = if base.is_empty() { "Imported" } else { base };
    if !taken.contains_key(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} ({n})")).find(|name| !taken.contains_key(name)).unwrap_or_default()
}

/// D `ngMergeImportedPuppet`: the source root's children move under the target root; parameters and
/// animations are appended (animation names made unique). The source's UUIDs that the target already uses
/// get new ones, with every reference rewritten. Both roots must have the same transform (D reparents with
/// `keepTransform`, which only matters when they differ).
pub fn merge_inp(
    target: &Value,
    target_textures: &[Texture],
    source: &Value,
    source_textures: &[Texture],
    merge_parameters: bool,
    merge_animations: bool,
) -> Result<Merged, String> {
    model_import::check_slots(target, target_textures.len())?;
    model_import::check_slots(source, source_textures.len())?;
    if transform_of(target) != transform_of(source) {
        return Err("merge.inp needs both puppets' root transforms to be equal".into());
    }
    let mut source = source.clone();
    let source_root = source["nodes"]["uuid"].as_u64().and_then(valid_id).ok_or("source root has no uuid")?;
    let mut used = HashSet::new();
    resource_ids(target, &mut used);
    let mut source_ids = HashSet::new();
    resource_ids(&source, &mut source_ids);
    let mut sorted: Vec<u32> = source_ids.iter().copied().filter(|id| *id != source_root).collect();
    sorted.sort_unstable();
    let mut occupied: HashSet<u32> = used.union(&source_ids).copied().collect();
    let mut map = HashMap::new();
    // Unchanged ids stay in the map: weld links to ids missing from it are dropped.
    for id in sorted {
        let new = if used.contains(&id) { lowest_unused(&mut occupied)? } else { id };
        map.insert(id, new);
    }
    if !merge_parameters {
        let mut parameter_ids = HashSet::new();
        resource_ids(&json!({ "param": source.get("param").cloned().unwrap_or(json!([])) }), &mut parameter_ids);
        fn unassign(node: &mut Value, ids: &HashSet<u32>) {
            if node.get("type").and_then(Value::as_str) == Some("SimplePhysics")
                && node.get("param").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()).is_some_and(|id| ids.contains(&id))
            {
                node["param"] = json!(crate::physics::UNASSIGNED_PARAM);
            }
            for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
                unassign(child, ids);
            }
        }
        unassign(&mut source["nodes"], &parameter_ids);
    }
    remap_subtree(&mut source["nodes"], &map)?;
    let offset = target_textures.len() as u64;
    let mut moved = source["nodes"].get("children").and_then(Value::as_array).cloned().unwrap_or_default();
    for child in &mut moved {
        shift_slots(child, offset);
    }
    let created: Vec<u32> = moved.iter().filter_map(|node| node["uuid"].as_u64().and_then(valid_id)).collect();

    let mut model = target.clone();
    model["nodes"]
        .as_object_mut()
        .ok_or("model nodes must be an object")?
        .entry("children")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("root children must be an array")?
        .extend(moved);
    if merge_parameters {
        let mut parameters = source.get("param").and_then(Value::as_array).cloned().unwrap_or_default();
        remap_parameters(&mut parameters, &map);
        let list = model
            .as_object_mut()
            .ok_or("model must be an object")?
            .entry("param")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("model param must be an array")?;
        list.extend(parameters);
        // The source's eye blinking parameters join the target's list.
        let eyes: Vec<u32> = crate::animation::eye_parameters(&source).filter_map(|id| map.get(&id).copied()).collect();
        if !eyes.is_empty() {
            let model = model.as_object_mut().ok_or("model must be an object")?;
            let live2d = model.entry("live2d").or_insert_with(|| json!({}));
            let live2d = live2d.as_object_mut().ok_or("model live2d must be an object")?;
            let list = live2d.entry("eyeBlink").or_insert_with(|| json!([]));
            let list = list.as_array_mut().ok_or("model live2d.eyeBlink must be an array")?;
            // A source id the target uses already was given a fresh one; a stale target id the source reuses would be
            // listed twice, which `eye_parameters` reads once.
            list.extend(eyes.into_iter().map(|id| json!(id)));
        }
    }
    if merge_animations {
        remap_lanes(&mut source, &map);
        let incoming: BTreeMap<String, Value> = source
            .get("animations")
            .and_then(Value::as_object)
            .map(|animations| animations.iter().map(|(name, value)| (name.clone(), value.clone())).collect())
            .unwrap_or_default();
        let animations = model
            .as_object_mut()
            .ok_or("model must be an object")?
            .entry("animations")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("model animations must be an object")?;
        for (name, animation) in incoming {
            let name = unique_animation_name(animations, &name);
            animations.insert(name, animation);
        }
    }
    let mut slots = target_textures.to_vec();
    slots.extend_from_slice(source_textures);
    let textures = model_import::compact_slots(&mut model, slots)?;
    Ok(Merged { model, textures, created, replaced: Vec::new() })
}

// ------------------------------------------------------------------------------------ session ops

pub(crate) fn blobs(textures: &[Texture]) -> Vec<TextureBlob> {
    textures.iter().map(|texture| TextureBlob { tag: texture.tag, data: texture.data.clone() }).collect()
}

fn request_flag(request: &Value, key: &str, default: bool) -> Result<bool, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be a boolean")),
    }
}

fn existing_directory(request: &Value, op: &str) -> Result<PathBuf, String> {
    let raw = required_string(request, "directory")?;
    if raw.is_empty() {
        return Err(format!("{op} needs a directory"));
    }
    let path = std::fs::canonicalize(raw).map_err(|error| format!("{op} could not resolve directory: {error}"))?;
    if !path.is_dir() {
        return Err(format!("{op} needs a directory"));
    }
    Ok(path)
}

fn slot_png_name(slot: usize) -> String {
    format!("slot-{slot}.png")
}

fn decoded_blob(blob: &TextureBlob, slot: usize) -> Result<image::RgbaImage, String> {
    let format = blob_format(blob)?;
    image::load_from_memory_with_format(&blob.data, format)
        .map(|image| image.to_rgba8())
        .map_err(|_| format!("texture slot {slot} could not be decoded"))
}

fn encode_rgba(rgba: &image::RgbaImage, format: image::ImageFormat, slot: usize) -> Result<Vec<u8>, String> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut bytes, format).map_err(|error| format!("texture slot {slot} could not be encoded: {error}"))?;
    Ok(bytes.into_inner())
}

impl Session {
    pub(crate) fn require_inx(&self, op: &str) -> Result<(), String> {
        if self.document.is_inx() {
            Ok(())
        } else {
            Err(format!("{op} needs an INP/INX document"))
        }
    }

    /// Replaces the whole session with a document that has no project path (D `incNewProject` + load).
    fn adopt_unsaved(&mut self, document: Document) -> Value {
        let revision = self.revision.saturating_add(1);
        self.replace_document(document, revision, None, None);
        self.snapshot()
    }

    pub(crate) fn commit_merged(&mut self, merged: &Merged) -> Result<(), String> {
        let blobs = blobs(&merged.textures);
        if merged.model == self.document.model && blobs == self.document.textures() {
            return Ok(());
        }
        let sections = self.document.sections_with_textures(blobs)?;
        self.commit_with_sections(merged.model.clone(), sections)
    }

    /// `import.inp`: the INP/INX file becomes a new, unsaved project (D `incImportINP`).
    pub(crate) fn op_import_inp(&mut self, request: &Value) -> Result<Value, String> {
        let bytes = read_bounded_file(required_string(request, "path")?, MAX_INP_BYTES, "INP")?;
        let mut document = Document::parse(&bytes)?;
        if !document.is_inx() {
            return Err("import.inp needs an INP/INX file".into());
        }
        prepare_model(&mut document.model)?;
        Ok(self.adopt_unsaved(document))
    }

    /// `merge.inp`: children, parameters and animations of another INP/INX join the open project.
    pub(crate) fn op_merge_inp(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("merge.inp")?;
        let bytes = read_bounded_file(required_string(request, "path")?, MAX_INP_BYTES, "INP")?;
        let mut source = Document::parse(&bytes)?;
        if !source.is_inx() {
            return Err("merge.inp needs an INP/INX source".into());
        }
        prepare_model(&mut source.model)?;
        let source_textures: Vec<Texture> =
            source.textures().iter().map(|blob| Texture { tag: blob.tag, data: blob.data.clone() }).collect();
        let merged = merge_inp(
            &self.document.model,
            &self.model_textures(),
            &source.model,
            &source_textures,
            request_flag(request, "mergeParameters", true)?,
            request_flag(request, "mergeAnimations", true)?,
        )?;
        self.commit_merged(&merged)?;
        Ok(json!({ "created": merged.created, "textures": merged.textures.len() }))
    }

    /// `import.folder`: every PNG/TGA of the folder becomes a Part of a new, unsaved project.
    pub(crate) fn op_import_folder(&mut self, request: &Value) -> Result<Value, String> {
        let folder = Path::new(required_string(request, "path")?);
        let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
            .map_err(|error| format!("could not read folder: {error}"))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && model_import::image_extension(path).is_some())
            .collect();
        // D lists the directory in OS order; sorting by name keeps the Part order reproducible.
        files.sort();
        let (mut images, mut pixels): (Vec<ImageFile>, u64) = (Vec::new(), 0);
        for file in files {
            if images.len() >= MAX_FOLDER_IMAGES {
                return Err(format!("the folder holds more than {MAX_FOLDER_IMAGES} images"));
            }
            let image = model_import::read_image(&file)?;
            pixels += u64::from(image.width) * u64::from(image.height);
            if pixels > MAX_FOLDER_PIXELS {
                return Err("the folder's images exceed the memory budget".into());
            }
            images.push(image);
        }
        let imported = model_import::image_folder_model(&images)?;
        let mut document = Document::parse(&model_import::inx_bytes(&imported.model, &imported.textures)?)?;
        prepare_model(&mut document.model)?;
        let mut snapshot = self.adopt_unsaved(document);
        // `skipped` is kept (always empty now that JPEG decodes) so existing clients still find the key.
        snapshot["imported"] = json!({ "nodes": images.len(), "textures": images.len(), "skipped": Vec::<String>::new() });
        Ok(snapshot)
    }

    /// `merge.images`: one Part per image file, added under `parent` (default: the root) in one undo step.
    pub(crate) fn op_merge_images(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("merge.images")?;
        let paths: Vec<String> = match request.get("paths") {
            Some(Value::String(joined)) => joined.split('|').map(str::to_owned).collect(),
            Some(Value::Array(list)) => list
                .iter()
                .map(|path| path.as_str().map(str::to_owned).ok_or("paths must hold strings"))
                .collect::<Result<_, _>>()?,
            _ => return Err("paths must be an array of paths or a string joined by |".into()),
        };
        if paths.is_empty() || paths.iter().any(String::is_empty) {
            return Err("Paths not provided".into());
        }
        if paths.len() > MAX_FOLDER_IMAGES {
            return Err(format!("more than {MAX_FOLDER_IMAGES} files"));
        }
        // D checks every extension before it loads anything.
        for path in &paths {
            if model_import::image_extension(Path::new(path)).is_none() {
                let ext = Path::new(path).extension().map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()));
                return Err(format!("Invalid file type {}", ext.unwrap_or_default()));
            }
        }
        let images = paths.iter().map(|path| model_import::read_image(Path::new(path))).collect::<Result<Vec<_>, _>>()?;
        let root = self.document.model["nodes"]["uuid"].as_u64().and_then(valid_id).ok_or("root has no uuid")?;
        let parent = match request.get("parent") {
            None | Some(Value::Null) => root,
            Some(value) => value.as_u64().and_then(valid_id).ok_or("parent must be a node UUID")?,
        };
        let merged = model_import::add_image_parts(&self.document.model, &self.model_textures(), parent, &images)?;
        self.commit_merged(&merged)?;
        Ok(json!({ "created": merged.created, "parent": parent }))
    }

    /// `tool.regenerate_node_ids` and `tool.repair` (D `incAttemptRepairPuppet` is exactly the id
    /// regeneration followed by a node rescan, which the JSON model does not need).
    pub(crate) fn op_tool_regenerate_ids(&mut self) -> Result<Value, String> {
        let (candidate, count) = regenerate_node_ids(&self.document.model)?;
        let marks = crate::dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "regenerated": count }))
    }

    pub(crate) fn op_tool_fake_layer_names(&mut self) -> Result<Value, String> {
        let candidate = fake_layer_names(&self.document.model);
        let changed = candidate != self.document.model;
        self.commit_model(candidate)?;
        Ok(json!({ "changed": changed }))
    }

    pub(crate) fn op_tool_textures(&mut self, rebleed: bool) -> Result<Value, String> {
        self.require_inx(if rebleed { "tool.rebleed_textures" } else { "tool.premultiply_textures" })?;
        let slots = self.document.textures();
        let count = slots.len();
        let changed = if rebleed { rebleed_textures(slots)? } else { premultiply_textures(slots)? };
        let Some(changed) = changed else {
            return Ok(json!({ "textures": count, "changed": false }));
        };
        let sections = self.document.sections_with_textures(changed)?;
        self.commit_with_sections(self.document.model.clone(), sections)?;
        Ok(json!({ "textures": count, "changed": true }))
    }

    /// `texture.set`: replaces the pixels of one texture slot (for every Part that uses it) with an image from
    /// `path` (PNG, TGA, JPEG) or `png_base64`, as one undo step. Input is straight alpha unless `premultiplied`;
    /// stored textures are premultiplied. A different size needs `allowResize`.
    pub(crate) fn op_texture_set(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("texture.set")?;
        let slot = request
            .get("slot")
            .and_then(Value::as_u64)
            .and_then(|slot| usize::try_from(slot).ok())
            .ok_or("slot must be a nonnegative integer")?;
        let premultiplied = request_flag(request, "premultiplied", false)?;
        let allow_resize = request_flag(request, "allowResize", false)?;
        let text = |key: &str| match request.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(_) => Err(format!("{key} must be a string")),
        };
        let mut rgba = match (text("path")?, text("png_base64")?) {
            (Some(path), None) if !path.is_empty() => model_import::read_rgba(Path::new(path))?.1,
            (None, Some(encoded)) => {
                if encoded.len() > MAX_TEXTURE_SET_BASE64 {
                    return Err("png_base64 is too large; use path for big images".into());
                }
                let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                    .map_err(|error| format!("png_base64 is not valid base64: {error}"))?;
                model_import::decode_rgba("png_base64", Some(image::ImageFormat::Png), &bytes)?
            }
            _ => return Err("texture.set needs exactly one of path (non-empty) or png_base64".into()),
        };
        let old = self.document.textures();
        let old_blob = old
            .get(slot)
            .ok_or_else(|| format!("texture slot {slot} is out of range ({} textures)", old.len()))?;
        let format = blob_format(old_blob)?;
        let old_rgba = image::load_from_memory_with_format(&old_blob.data, format)
            .map_err(|_| format!("texture slot {slot} could not be decoded"))?
            .to_rgba8();
        let (width, height) = rgba.dimensions();
        if old_rgba.dimensions() != (width, height) && !allow_resize {
            let (old_width, old_height) = old_rgba.dimensions();
            return Err(format!(
                "texture slot {slot} is {old_width}x{old_height} but the image is {width}x{height}; \
                 pass allowResize to change the size"
            ));
        }
        if !premultiplied {
            premultiply_rgba(&mut rgba);
        }
        let result = |changed: bool| json!({ "slot": slot, "width": width, "height": height, "changed": changed });
        if rgba == old_rgba {
            return Ok(result(false));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        rgba.write_to(&mut bytes, format).map_err(|error| format!("texture could not be encoded: {error}"))?;
        let mut blobs = old.to_vec();
        blobs[slot] = TextureBlob { tag: old_blob.tag, data: bytes.into_inner() };
        let sections = self.document.sections_with_textures(blobs)?;
        self.commit_with_sections(self.document.model.clone(), sections)?;
        Ok(result(true))
    }

    /// `texture.export`: every slot as `slot-{n}.png` of the stored pixels. Those pixels are already
    /// premultiplied. The document does not change. An existing file is refused before any write unless
    /// `overwrite` is true.
    pub(crate) fn op_texture_export(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("texture.export")?;
        let overwrite = request_flag(request, "overwrite", false)?;
        let directory = existing_directory(request, "texture.export")?;
        let slots = self.document.textures();
        let mut files = Vec::with_capacity(slots.len());
        for (slot, blob) in slots.iter().enumerate() {
            let name = slot_png_name(slot);
            let path = directory.join(&name);
            if path.exists() {
                if !path.is_file() {
                    return Err(format!("{name} is not a file"));
                }
                if !overwrite {
                    return Err(format!("texture.export would replace {name}; pass overwrite to replace it"));
                }
            }
            let rgba = decoded_blob(blob, slot)?;
            files.push((path, encode_rgba(&rgba, image::ImageFormat::Png, slot)?, name));
        }
        let written: Vec<String> = files.iter().map(|(_, _, name)| name.clone()).collect();
        for (path, bytes, _) in &files {
            crate::write_atomic(path, bytes, overwrite)?;
        }
        let directory = directory.to_str().ok_or("directory is not valid UTF-8")?;
        Ok(json!({ "directory": directory, "count": written.len(), "written": written }))
    }

    /// `texture.replace`: read `slot-{n}.png` back into that slot. A missing file leaves the slot and is
    /// named in `missing`. Decoded pixels are stored as they are, in the slot's own format, with no second
    /// premultiply. `texture.export` wrote the stored premultiplied pixels, so a straight-alpha editor
    /// should use `texture.set` instead. Changed slots are one undo step. A size mismatch refuses the
    /// whole batch before that step unless `allowResize` is true.
    pub(crate) fn op_texture_replace(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("texture.replace")?;
        let allow_resize = request_flag(request, "allowResize", false)?;
        let directory = existing_directory(request, "texture.replace")?;
        let old = self.document.textures();
        let count = old.len();
        let mut incoming = Vec::new();
        let mut missing = Vec::new();
        for (slot, blob) in old.iter().enumerate() {
            let name = slot_png_name(slot);
            let path = directory.join(&name);
            if !path.exists() {
                missing.push(name);
                continue;
            }
            let (_file_name, rgba) = model_import::read_rgba(&path)?;
            let old_rgba = decoded_blob(blob, slot)?;
            if old_rgba.dimensions() != rgba.dimensions() && !allow_resize {
                let (old_width, old_height) = old_rgba.dimensions();
                let (width, height) = rgba.dimensions();
                return Err(format!(
                    "texture slot {slot} is {old_width}x{old_height} but {name} is {width}x{height}; \
                     pass allowResize to change the size"
                ));
            }
            incoming.push((slot, rgba, old_rgba));
        }
        let mut blobs = old.to_vec();
        let mut changed = Vec::new();
        for (slot, rgba, old_rgba) in incoming {
            if rgba == old_rgba {
                continue;
            }
            let format = blob_format(&blobs[slot])?;
            blobs[slot] = TextureBlob { tag: blobs[slot].tag, data: encode_rgba(&rgba, format, slot)? };
            changed.push(slot);
        }
        if !changed.is_empty() {
            let sections = self.document.sections_with_textures(blobs)?;
            self.commit_with_sections(self.document.model.clone(), sections)?;
        }
        Ok(json!({ "missing": missing, "changed": changed, "count": count }))
    }

    /// `tool.import_session_data`: copies Inochi Session's tracking bindings from another puppet.
    pub(crate) fn op_tool_import_session_data(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("tool.import_session_data")?;
        let bytes = read_bounded_file(required_string(request, "path")?, MAX_INP_BYTES, "INP")?;
        let source = Document::parse(&bytes)?;
        if !source.is_inx() {
            return Err("tool.import_session_data needs an INP/INX source".into());
        }
        let payload = source
            .extension_entries()?
            .into_iter()
            .find(|(name, _)| name == SESSION_DATA_KEY.as_bytes())
            .map(|(_, payload)| payload)
            .ok_or("There was no Inochi Session data to import!")?;
        let mut entries = self.document.extension_entries()?;
        match entries.iter().position(|(name, _)| name == SESSION_DATA_KEY.as_bytes()) {
            Some(at) => entries[at].1 = payload.clone(),
            None => entries.push((SESSION_DATA_KEY.as_bytes().to_vec(), payload.clone())),
        }
        let sections = self.document.sections_with_extensions(&entries)?;
        self.commit_with_sections(self.document.model.clone(), sections)?;
        Ok(json!({ "key": SESSION_DATA_KEY, "bytes": payload.len() }))
    }

    /// `view.screenshot`: the whole puppet as a PNG file (D `SaveScreenshotCommand` always writes `.png`).
    /// The longest side is `size` pixels (default 1024).
    pub(crate) fn op_view_screenshot(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        if path.is_empty() {
            return Err("Filename not set".into());
        }
        let size = match request.get("size") {
            None | Some(Value::Null) => 1024,
            Some(value) => value.as_u64().filter(|size| (16..=8192).contains(size)).ok_or("size must be 16 to 8192")?,
        };
        let bounds = self.op_bounds(&json!({}))?["bounds"].clone();
        let bounds: Vec<f64> = bounds.as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
        let [x0, y0, x1, y1] = bounds[..] else {
            return Err("Puppet bounds are empty".into());
        };
        let (w, h) = (x1 - x0, y1 - y0);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return Err("Puppet bounds are empty".into());
        }
        let scale = size as f64 / w.max(h);
        let (width, height) = (((w * scale).round() as u64).max(1), ((h * scale).round() as u64).max(1));
        let target = Path::new(path).with_extension("png");
        let target = target.to_str().ok_or("path is not valid UTF-8")?.to_owned();
        self.op_render(&json!({ "width": width, "height": height, "bounds": [x0, y0, x1, y1], "path": target }))?;
        Ok(json!({ "width": width, "height": height, "path": target }))
    }

    /// `sketch.snapshot` (Cubism "Save snapshot as guide image", UI07): the puppet as it shows now, at the active
    /// values and previews, becomes a Part at `opacity` (default 0.25) over the area it draws, drawn in front of
    /// every other node, as one undo step. About one pixel per world unit up to 4096 pixels a side; the Part is
    /// scaled so its pixels cover that area exactly.
    pub(crate) fn op_sketch_snapshot(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("sketch.snapshot")?;
        let opacity = match request.get("opacity") {
            None | Some(Value::Null) => 0.25,
            Some(value) => value.as_f64().filter(|opacity| (0.0..=1.0).contains(opacity)).ok_or("opacity must be 0 to 1")?,
        };
        let bounds = self.op_bounds(&json!({}))?["bounds"].clone();
        let bounds: Vec<f64> = bounds.as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
        let [x0, y0, x1, y1] = bounds[..] else {
            return Err("Puppet bounds are empty".into());
        };
        let (w, h) = (x1 - x0, y1 - y0);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return Err("Puppet bounds are empty".into());
        }
        let scale = (4096.0 / w.max(h)).min(1.0);
        let (width, height) = (((w * scale).round() as u64).max(1), ((h * scale).round() as u64).max(1));
        // The ambient tint is applied again when the Part is drawn, so keep the picture untinted.
        let ambient = std::mem::replace(&mut self.scene_ambient, [1.0; 3]);
        let rendered = self.op_render(&json!({ "width": width, "height": height, "bounds": [x0, y0, x1, y1] }));
        self.scene_ambient = ambient;
        let rendered = rendered?;
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, rendered["png"].as_str().unwrap_or_default())
            .map_err(|error| format!("the snapshot could not be read: {error}"))?;
        let rgba = model_import::decode_rgba("Snapshot", Some(image::ImageFormat::Png), &png)?;
        if rgba.pixels().all(|pixel| pixel[3] == 0) {
            return Err("The puppet draws nothing to keep".into());
        }
        // Crop to the drawn pixels so the bounds margin does not grow the puppet with every snapshot. Row 0 is `y0`: y grows downward.
        let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, _) in rgba.enumerate_pixels().filter(|(_, _, pixel)| pixel[3] != 0) {
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1));
        }
        let rgba = image::imageops::crop_imm(&rgba, left, top, right - left, bottom - top).to_image();
        let (unit_x, unit_y) = (w / width as f64, h / height as f64);
        let (width, height) = ((right - left) as u64, (bottom - top) as u64);
        let mut image = model_import::image_from_rgba("Snapshot".into(), "Snapshot".into(), rgba)?;
        // ponytail: the bounds are world space and the Part sits under the root, so a moved root offsets it;
        // map through the root's inverse transform if roots get moved.
        image.trans = [x0 + f64::from(left + right) / 2.0 * unit_x, y0 + f64::from(top + bottom) / 2.0 * unit_y, 0.0];
        let root = self.document.model["nodes"]["uuid"].as_u64().and_then(valid_id).ok_or("root has no uuid")?;
        // Drawn in front: the lowest summed zsort draws last.
        let root_z = self.document.model["nodes"]["zsort"].as_f64().unwrap_or(0.0);
        let front = lowest_zsort(&self.document.model["nodes"], 0.0) - root_z - 1.0;
        let mut merged = model_import::add_image_parts(&self.document.model, &self.model_textures(), root, &[image])?;
        let id = *merged.created.first().ok_or("the snapshot Part was not made")?;
        let part = crate::nodes::find_node_mut(&mut merged.model, id).ok_or("the snapshot Part was not made")?;
        part["opacity"] = json!(opacity);
        part["zsort"] = json!(front);
        // The import halves odd sizes down, which would shift the quad by half a pixel.
        let (half_x, half_y) = (width as f64 / 2.0, height as f64 / 2.0);
        part["mesh"]["verts"] = json!([-half_x, -half_y, -half_x, half_y, half_x, -half_y, half_x, half_y]);
        part["transform"]["scale"] = json!([unit_x, unit_y]);
        self.commit_merged(&merged)?;
        Ok(json!({ "created": merged.created, "parent": root, "width": width, "height": height }))
    }

    /// `values.reset`: every parameter returns to its default (D `ResetParametersCommand`).
    pub(crate) fn op_values_reset(&mut self) -> Result<Value, String> {
        self.runtime_values = crate::default_values(&self.document.model);
        self.physics_output = None;
        self.animation_preview = None;
        Ok(self.snapshot())
    }
}

/// The lowest summed zsort of `node` and the nodes under it, `parent` being the sum above it.
fn lowest_zsort(node: &Value, parent: f64) -> f64 {
    let own = parent + node["zsort"].as_f64().unwrap_or(0.0);
    node["children"].as_array().into_iter().flatten().fold(own, |lowest, child| lowest.min(lowest_zsort(child, own)))
}
