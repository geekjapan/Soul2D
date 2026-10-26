use serde_json::Value;
use std::sync::Arc;

pub(crate) type NativePayloads = std::collections::BTreeMap<String, Arc<[u8]>>;

const MAGIC: &[u8; 8] = b"TRNSRTS\0";
const RUNTIME_JSON_LIMIT: usize = 16 << 20;

/// Names a Live2D runtime file by its content, with the operation that loads it when one exists.
fn live2d_runtime_kind(bytes: &[u8]) -> Option<(&'static str, Option<&'static str>)> {
    if bytes.starts_with(b"MOC3") {
        return Some((".moc3", None));
    }
    // The diagnosis is not worth parsing a large unrelated file; official runtime JSON stays far below this.
    if bytes.len() > RUNTIME_JSON_LIMIT {
        return None;
    }
    let json: Value = serde_json::from_slice(bytes).ok()?;
    let has = |key: &str| json.get(key).is_some();
    Some(match json["Type"].as_str() {
        Some("Live2D Expression") => (".exp3.json", Some("live2d.expression.import")),
        Some("Live2D Pose") => (".pose3.json", Some("live2d.pose.attach")),
        _ if json["Meta"]["SettingCount"].is_u64() && has("Settings") => (".motionsync3.json", None),
        _ if json["Version"] != 3 => return None,
        _ if has("FileReferences") => (".model3.json", None),
        _ if has("PhysicsSettings") => (".physics3.json", Some("live2d.physics.attach")),
        _ if has("Curves") => (".motion3.json", Some("motion3.import")),
        _ if has("UserData") => (".userdata3.json", None),
        _ if has("ParameterGroups") || (has("Parameters") && has("Parts")) => (".cdi3.json", None),
        _ => return None,
    })
}
const TEXTURES: &[u8; 8] = b"TEX_SECT";
const EXTENSIONS: &[u8; 8] = b"EXT_SECT";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextureBlob {
    pub tag: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Document {
    pub model: Value,
    storage: Storage,
}

/// What an INX file carries after the JSON: the raw tail (texture section plus extension sections)
/// and its decoded textures. Shared by `Arc` so each history state keeps the sections it was made with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InxSections {
    pub tail: Vec<u8>,
    pub textures: Vec<TextureBlob>,
}

/// One named payload of the extension section: raw name bytes (UTF-8 in files D writes) and payload.
pub(crate) type ExtensionEntry = (Vec<u8>, Vec<u8>);

#[derive(Clone, Debug)]
enum Storage {
    Inx {
        sections: Arc<InxSections>,
    },
    Caff {
        template: Arc<crate::caff::CaffDocument>,
        initial: Arc<Value>,
        /// Sources this session copied; a model holds the ones not undone.
        copies: Arc<Vec<crate::authoring_copy::Copy>>,
        payloads: Arc<NativePayloads>,
    },
    Animation {
        template: Arc<crate::caff::CaffDocument>,
        initial: Arc<Value>,
        copies: Arc<Vec<crate::animation_source::AnimationTrackCopy>>,
    },
}

impl Document {
    pub fn new(model: Value) -> Self {
        let mut tail = TEXTURES.to_vec();
        tail.extend_from_slice(&0u32.to_be_bytes());
        Self {
            model,
            storage: Storage::Inx {
                sections: Arc::new(InxSections {
                    tail,
                    textures: Vec::new(),
                }),
            },
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.starts_with(b"CAFF") {
            let template = crate::caff::CaffDocument::parse(bytes)?;
            if crate::animation_source::is_project(&template)? {
                let model = crate::animation_source::read(&template)?;
                return Ok(Self {
                    storage: Storage::Animation {
                        template: Arc::new(template),
                        initial: Arc::new(model.clone()),
                        copies: Arc::default(),
                    },
                    model,
                });
            }
            let model = crate::authoring_scene::read(&template)?;
            return Ok(Self {
                storage: Storage::Caff {
                    template: Arc::new(template),
                    initial: Arc::new(model.clone()),
                    copies: Arc::default(),
                    payloads: Arc::default(),
                },
                model,
            });
        }
        if bytes.len() < 12 || &bytes[..8] != MAGIC {
            return Err(match live2d_runtime_kind(bytes) {
                Some((kind, Some(operation))) => {
                    format!("this is a Live2D {kind} file, not a document; load it with {operation}")
                }
                Some((kind, None)) => format!("this is a Live2D {kind} file; Soul2D does not read it"),
                None => "invalid INP/INX magic or truncated header".into(),
            });
        }
        let json_len = read_u32(bytes, 8)? as usize;
        let json_start = 12usize;
        let tail_start = json_start
            .checked_add(json_len)
            .ok_or("JSON length overflow")?;
        if tail_start > bytes.len() {
            return Err("truncated INP/INX JSON payload".into());
        }
        let model = serde_json::from_slice(&bytes[json_start..tail_start])
            .map_err(|error| format!("invalid puppet JSON: {error}"))?;
        let tail = bytes[tail_start..].to_vec();
        let textures = parse_tail(&tail)?;
        Ok(Self {
            model,
            storage: Storage::Inx {
                sections: Arc::new(InxSections { tail, textures }),
            },
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if let Storage::Animation { template, initial, copies } = &self.storage {
            return crate::animation_source::rewrite_with_copies(template, initial, &self.model, copies);
        }
        if let Storage::Caff { template, initial, copies, payloads } = &self.storage {
            let bytes = crate::authoring_edit::rewrite_with_copies(template, initial, &self.model, copies)?;
            if payloads.is_empty() { return Ok(bytes); }
            let container = crate::caff::CaffDocument::parse(&bytes)?;
            let replacements: Vec<_> = payloads.iter().map(|(name, bytes)| (name.as_str(), bytes.as_ref())).collect();
            let bytes = container.replace_entries(&replacements)?;
            let reopened = crate::caff::CaffDocument::parse(&bytes)?;
            if crate::authoring_scene::read(&reopened)? != self.model
                || payloads.iter().any(|(name, bytes)| reopened.entry_data(name).as_deref() != Some(bytes.as_ref())) {
                return Err("native image replacement did not preserve the requested document".into());
            }
            return Ok(bytes);
        }
        let json = serde_json::to_vec(&self.model).map_err(|error| error.to_string())?;
        let json_len = u32::try_from(json.len()).map_err(|_| "puppet JSON is too large")?;
        let capacity = 12usize
            .checked_add(json.len())
            .and_then(|size| size.checked_add(self.tail().len()))
            .ok_or("INP/INX size overflow")?;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&json_len.to_be_bytes());
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(self.tail());
        Ok(bytes)
    }

    /// The sources this session copied in a CMO3 document.
    pub fn copies(&self) -> &[crate::authoring_copy::Copy] {
        match &self.storage {
            Storage::Caff { copies, .. } => copies,
            _ => &[],
        }
    }

    pub fn add_copy(&mut self, copy: crate::authoring_copy::Copy) {
        if let Storage::Caff { copies, .. } = &mut self.storage {
            Arc::make_mut(copies).push(copy);
        }
    }

    pub fn is_native(&self) -> bool {
        matches!(self.storage, Storage::Caff { .. })
    }

    pub(crate) fn native_container(&self) -> Option<&crate::caff::CaffDocument> {
        match &self.storage {
            Storage::Caff { template, .. } => Some(template),
            _ => None,
        }
    }

    pub(crate) fn native_payloads(&self) -> Option<Arc<NativePayloads>> {
        match &self.storage {
            Storage::Caff { payloads, .. } => Some(Arc::clone(payloads)),
            _ => None,
        }
    }

    pub(crate) fn set_native_payloads(&mut self, value: Arc<NativePayloads>) {
        if let Storage::Caff { payloads, .. } = &mut self.storage { *payloads = value; }
    }

    pub(crate) fn native_entry_data(&self, name: &str) -> Option<Vec<u8>> {
        let Storage::Caff { template, payloads, .. } = &self.storage else { return None };
        payloads.get(name).map(|bytes| bytes.to_vec()).or_else(|| template.entry_data(name))
    }

    pub(crate) fn replacing_native_entry(&self, name: &str, bytes: Vec<u8>) -> Result<Arc<NativePayloads>, String> {
        let Storage::Caff { template, payloads, .. } = &self.storage else {
            return Err("image replacement requires an active CMO3 document".into());
        };
        let original = template.entry_data(name).ok_or("the image entry is missing")?;
        let mut next = (**payloads).clone();
        if original == bytes { next.remove(name); } else { next.insert(name.to_owned(), bytes.into()); }
        Ok(Arc::new(next))
    }

    pub(crate) fn native_baseline_payloads(&self, baseline: &Self) -> Option<Arc<NativePayloads>> {
        let (base, other) = (self.native_container()?, baseline.native_container()?);
        if base.entries().len() != other.entries().len() { return None; }
        let mut payloads = NativePayloads::new();
        for (entry, saved) in base.entries().iter().zip(other.entries()) {
            if (&entry.name, &entry.alias, entry.unknown, entry.flags, entry.reserved)
                != (&saved.name, &saved.alias, saved.unknown, saved.flags, saved.reserved) { return None; }
            if entry.name == "main.xml" { continue; }
            let bytes = baseline.native_entry_data(&entry.name)?;
            if base.entry_data(&entry.name)? != bytes { payloads.insert(entry.name.clone(), bytes.into()); }
        }
        Some(Arc::new(payloads))
    }

    pub fn is_animation(&self) -> bool {
        matches!(self.storage, Storage::Animation { .. })
    }

    pub(crate) fn animation_initial(&self) -> Option<&Value> {
        match &self.storage {
            Storage::Animation { initial, .. } => Some(initial),
            _ => None,
        }
    }

    pub(crate) fn animation_copies(&self) -> &[crate::animation_source::AnimationTrackCopy] {
        match &self.storage {
            Storage::Animation { copies, .. } => copies,
            _ => &[],
        }
    }

    pub(crate) fn add_animation_copy(&mut self, copy: crate::animation_source::AnimationTrackCopy) {
        if let Storage::Animation { copies, .. } = &mut self.storage {
            Arc::make_mut(copies).push(copy);
        }
    }

    pub fn is_inx(&self) -> bool {
        matches!(self.storage, Storage::Inx { .. })
    }

    pub fn format(&self) -> &'static str {
        match self.storage {
            Storage::Inx { .. } => "inx",
            Storage::Caff { .. } => "live2d.cmo3",
            Storage::Animation { .. } => "live2d.can3",
        }
    }

    pub fn tail(&self) -> &[u8] {
        match &self.storage {
            Storage::Inx { sections } => &sections.tail,
            Storage::Caff { .. } | Storage::Animation { .. } => &[],
        }
    }

    pub fn inx_sections(&self) -> Option<Arc<InxSections>> {
        match &self.storage {
            Storage::Inx { sections } => Some(Arc::clone(sections)),
            Storage::Caff { .. } | Storage::Animation { .. } => None,
        }
    }

    /// Restores sections captured by [`Document::inx_sections`]; other formats have none and ignore it.
    pub fn set_inx_sections(&mut self, sections: Arc<InxSections>) {
        if let Storage::Inx { sections: current } = &mut self.storage {
            *current = sections;
        }
    }

    /// The sections with `textures` in place of the texture section. Extension sections that follow it
    /// in the tail are kept byte for byte.
    pub fn sections_with_textures(&self, textures: Vec<TextureBlob>) -> Result<Arc<InxSections>, String> {
        let Storage::Inx { sections } = &self.storage else {
            return Err("only INP/INX documents carry textures".into());
        };
        let (_, end) = parse_textures(&sections.tail)?;
        let mut tail = TEXTURES.to_vec();
        tail.extend_from_slice(&u32::try_from(textures.len()).map_err(|_| "too many textures")?.to_be_bytes());
        for texture in &textures {
            let length = u32::try_from(texture.data.len()).map_err(|_| "texture is too large")?;
            tail.extend_from_slice(&length.to_be_bytes());
            tail.push(texture.tag);
            tail.extend_from_slice(&texture.data);
        }
        tail.extend_from_slice(&sections.tail[end..]);
        Ok(Arc::new(InxSections { tail, textures }))
    }

    /// The raw bytes after the texture section (the `EXT_SECT` block, or empty when there is none).
    pub fn extension_bytes(&self) -> Result<&[u8], String> {
        let tail = self.tail();
        if tail.is_empty() {
            return Ok(tail);
        }
        let (_, end) = parse_textures(tail)?;
        Ok(&tail[end..])
    }

    /// The named payload entries of the extension section, in file order. Anything after the texture
    /// section that is not a complete `EXT_SECT` block is refused rather than guessed at.
    pub fn extension_entries(&self) -> Result<Vec<ExtensionEntry>, String> {
        parse_extensions(self.extension_bytes()?)
    }

    /// The sections with the extension block rebuilt from `entries`; the textures stay byte for byte.
    /// No entries means no block at all, as D writes it.
    pub fn sections_with_extensions(&self, entries: &[ExtensionEntry]) -> Result<Arc<InxSections>, String> {
        let Storage::Inx { sections } = &self.storage else {
            return Err("only INP/INX documents carry extension data".into());
        };
        let (_, end) = parse_textures(&sections.tail)?;
        let mut tail = sections.tail[..end].to_vec();
        if !entries.is_empty() {
            tail.extend_from_slice(EXTENSIONS);
            let count = u32::try_from(entries.len()).map_err(|_| "too many extension entries")?;
            tail.extend_from_slice(&count.to_be_bytes());
            for (name, payload) in entries {
                for part in [name, payload] {
                    let length = u32::try_from(part.len()).map_err(|_| "extension entry is too large")?;
                    tail.extend_from_slice(&length.to_be_bytes());
                    tail.extend_from_slice(part);
                }
            }
        }
        Ok(Arc::new(InxSections { tail, textures: sections.textures.clone() }))
    }

    pub fn textures(&self) -> &[TextureBlob] {
        match &self.storage {
            Storage::Inx { sections } => &sections.textures,
            Storage::Caff { .. } | Storage::Animation { .. } => &[],
        }
    }

    pub fn with_model(&self, model: Value) -> Self {
        Self {
            model,
            storage: self.storage.clone(),
        }
    }

    pub fn validate_candidate(&self, model: &Value) -> Result<(), String> {
        if !self.is_inx() {
            self.with_model(model.clone()).encode().map(|_| ())
        } else {
            crate::validate_model(model)
        }
    }
}

fn parse_tail(tail: &[u8]) -> Result<Vec<TextureBlob>, String> {
    let (textures, mut offset) = parse_textures(tail)?;
    if tail.get(offset..offset.saturating_add(8)) == Some(EXTENSIONS.as_slice()) {
        offset += 8;
        let sections = take_u32(tail, &mut offset)?;
        for _ in 0..sections {
            let name_len = take_u32(tail, &mut offset)? as usize;
            skip(tail, &mut offset, name_len, "truncated extension name")?;
            let payload_len = take_u32(tail, &mut offset)? as usize;
            skip(
                tail,
                &mut offset,
                payload_len,
                "truncated extension payload",
            )?;
        }
    }
    Ok(textures)
}

fn parse_extensions(bytes: &[u8]) -> Result<Vec<ExtensionEntry>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.get(..8) != Some(EXTENSIONS.as_slice()) {
        return Err("unrecognised data after the texture section".into());
    }
    let mut offset = 8;
    let count = take_u32(bytes, &mut offset)? as usize;
    let mut entries = Vec::with_capacity(count.min(bytes.len() / 8));
    for _ in 0..count {
        let mut parts = [Vec::new(), Vec::new()];
        for (part, message) in parts.iter_mut().zip(["truncated extension name", "truncated extension payload"]) {
            let length = take_u32(bytes, &mut offset)? as usize;
            let start = offset;
            skip(bytes, &mut offset, length, message)?;
            *part = bytes[start..offset].to_vec();
        }
        let [name, payload] = parts;
        entries.push((name, payload));
    }
    if offset != bytes.len() {
        return Err("extension section has trailing bytes".into());
    }
    Ok(entries)
}

/// The textures of the TEX_SECT at the start of `tail` and the offset just past that section.
fn parse_textures(tail: &[u8]) -> Result<(Vec<TextureBlob>, usize), String> {
    if tail.len() < 12 || &tail[..8] != TEXTURES {
        return Err("missing or truncated TEX_SECT".into());
    }
    let mut offset = 8;
    let count = take_u32(tail, &mut offset)? as usize;
    let mut textures = Vec::with_capacity(count.min(tail.len() / 5));
    for _ in 0..count {
        let length = take_u32(tail, &mut offset)? as usize;
        let tag = *tail.get(offset).ok_or("truncated texture type")?;
        offset += 1;
        let end = offset
            .checked_add(length)
            .ok_or("texture length overflow")?;
        let data = tail
            .get(offset..end)
            .ok_or("truncated texture payload")?
            .to_vec();
        offset = end;
        textures.push(TextureBlob { tag, data });
    }
    Ok((textures, offset))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let raw = bytes
        .get(offset..offset.checked_add(4).ok_or("offset overflow")?)
        .ok_or("truncated 32-bit length")?;
    Ok(u32::from_be_bytes(
        raw.try_into().map_err(|_| "truncated 32-bit length")?,
    ))
}

fn take_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, String> {
    let value = read_u32(bytes, *offset)?;
    *offset = offset.checked_add(4).ok_or("offset overflow")?;
    Ok(value)
}

fn skip(bytes: &[u8], offset: &mut usize, length: usize, message: &str) -> Result<(), String> {
    let end = offset
        .checked_add(length)
        .ok_or("section length overflow")?;
    if end > bytes.len() {
        return Err(message.into());
    }
    *offset = end;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Document;

    #[test]
    fn native_payload_staging_and_failed_save_preserve_the_previous_state() {
        use serde_json::json;
        let directory = std::env::temp_dir().join(format!("soul2d-image-failure-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&directory).unwrap();
        let png = |color| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(color)))
                .write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            bytes.into_inner()
        };
        let original = png([255, 255, 255, 255]);
        let changed = png([20, 40, 60, 255]);
        let path = directory.join("replacement.png");
        std::fs::write(&path, &changed).unwrap();
        let xml = crate::authoring_edit::tests::artmesh_xml()
            .replace("width='2' height='3'", "width='1' height='1'")
            .replace("<CPartSource xs.n='rootPart' xs.ref='part'/>",
                "<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
                 <carray_list xs.n='brushes' count='1'><CArtPathBrush><CArtPathBrushGuid xs.n='guid' uuid='brush'/>\
                 <CArtPathBrush_TextureInput xs.n='textureInput'><GTexture2D xs.n='texture' xs.ref='texture'/>\
                 </CArtPathBrush_TextureInput></CArtPathBrush></carray_list></CArtPathBrushSetting>");
        let bytes = crate::authoring_edit::tests::test_container(&xml).replace_entries(&[("mesh.png", &original)]).unwrap();
        let mut session = crate::Session::from_document(Document::parse(&bytes).unwrap(), 1, None, None);
        session.document.model["unsupportedTestField"] = json!(true);
        let before = session.snapshot();
        let replace = json!({"op":"live2d.brush.replace_image","guid":"brush","path":path,"expectedRevision":1});
        assert_eq!(session.handle_request(&replace)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.document.native_entry_data("mesh.png"), Some(original));
        session.document.model.as_object_mut().unwrap().remove("unsupportedTestField");
        assert_eq!(session.handle_request(&replace)["succeeded"], true);
        let saved_payloads = session.saved_payloads.clone();
        let before = session.snapshot();
        session.fail_save_after_staging = true;
        let destination = directory.join("saved.cmo3");
        assert_eq!(session.handle_request(&json!({"op":"save","path":destination}))["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.saved_payloads, saved_payloads);
        assert!(!destination.exists());
        assert_eq!(session.document.native_entry_data("mesh.png"), Some(changed));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn open_names_live2d_runtime_files() {
        let message = |bytes: &[u8]| Document::parse(bytes).err().unwrap();
        for (bytes, expected) in [
            (&b"MOC3\x00\x00\x00\x00\x05\x00\x00\x00"[..], "this is a Live2D .moc3 file; Soul2D does not read it"),
            (br#"{"Version":3,"FileReferences":{"Moc":"a.moc3"}}"#,
                "this is a Live2D .model3.json file; Soul2D does not read it"),
            (br#"{"Version":3,"Parameters":[],"ParameterGroups":[],"Parts":[]}"#,
                "this is a Live2D .cdi3.json file; Soul2D does not read it"),
            (br#"{"Version":3,"Meta":{"UserDataCount":0},"UserData":[]}"#,
                "this is a Live2D .userdata3.json file; Soul2D does not read it"),
            (br#"{"Version":3,"Meta":{},"Curves":[],"UserData":[]}"#,
                "this is a Live2D .motion3.json file, not a document; load it with motion3.import"),
            (br#"{"Type":"Live2D Expression","Parameters":[]}"#,
                "this is a Live2D .exp3.json file, not a document; load it with live2d.expression.import"),
            (br#"{"Type":"Live2D Pose","Groups":[]}"#,
                "this is a Live2D .pose3.json file, not a document; load it with live2d.pose.attach"),
            (br#"{"Version":3,"Meta":{},"PhysicsSettings":[]}"#,
                "this is a Live2D .physics3.json file, not a document; load it with live2d.physics.attach"),
            (br#"{"Version":1,"Meta":{"SettingCount":1},"Settings":[]}"#,
                "this is a Live2D .motionsync3.json file; Soul2D does not read it"),
            (br#"{"unrelated":true}"#, "invalid INP/INX magic or truncated header"),
            (br#"{"Settings":{},"Meta":{},"Curves":[]}"#, "invalid INP/INX magic or truncated header"),
            (b"plain text, long enough", "invalid INP/INX magic or truncated header"),
        ] {
            assert_eq!(message(bytes), expected);
        }
    }
}
