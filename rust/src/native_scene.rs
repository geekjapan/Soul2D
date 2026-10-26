use crate::authoring_scene::{self, ParsedScene};
use crate::caff::CaffDocument;
use crate::document::TextureBlob;
use image::{DynamicImage, ImageFormat, ImageReader};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;

const MAX_DEFORMER_DEPTH: usize = 64;
/// How far above its origin, in the units of its parent warp, a rotation deformer samples that warp to find the angle
/// the warp turns it by. Cubism Core 6.0.1 vertex positions match with 0.1 and not with 0.01 or 1.
const WARP_ROTATION_PROBE: f64 = 0.1;
/// Rotation deformer scales matched Cubism Core on nine models; no compared model reflected one.
pub(crate) const ROTATION_UNVERIFIED: &str = "native rotation reflection is not checked against Cubism Core";
const ART_PATH_NOT_DRAWN: &str = "native ArtPath is read but not drawn";
const GLUE_SKIPPED: &str = "native Glue without an intensity or with a target that is not drawn is not applied";
const GLUE_STALE_PAIR: &str = "native Glue pair that binds a missing vertex is not applied";
const ROOT_ID: u64 = 1;

pub(crate) struct NativeScene {
    parsed: ParsedScene,
    source: Value,
    textures: Vec<TextureBlob>,
    texture_slots: HashMap<String, usize>,
}

impl NativeScene {
    pub(crate) fn parse(container: &CaffDocument) -> Result<Self, String> {
        let parsed = authoring_scene::parse_scene(container)?;
        let source = parsed.source_value()?;
        let sources = source["sources"]
            .as_array()
            .ok_or("native model source list is missing")?;
        let source_by_guid = sources
            .iter()
            .map(|item| Ok((string_field(item, "guid", "source")?.to_owned(), item)))
            .collect::<Result<HashMap<_, _>, String>>()?;
        let mask_sources = sources
            .iter()
            .flat_map(|item| {
                item["data"]["clipGuids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
            })
            .collect::<HashSet<_>>();
        let mut used_textures = HashSet::new();
        for item in sources {
            if item["data"]["type"] == "ArtMesh"
                && (inherited_visibility(item, &source_by_guid)?
                    || mask_sources.contains(string_field(item, "guid", "source")?))
            {
                used_textures.insert(string_field(&item["data"], "textureGuid", "art mesh")?);
            }
        }
        let texture_records = source["textures"]
            .as_array()
            .ok_or("native model texture metadata is missing")?;
        let mut textures = Vec::with_capacity(texture_records.len());
        let mut texture_slots = HashMap::new();
        for texture in texture_records {
            let guid = string_field(texture, "guid", "texture")?.to_owned();
            let entry = string_field(texture, "imageEntry", "texture")?;
            let data = container
                .entry_data(entry)
                .ok_or_else(|| format!("native texture entry {entry:?} is missing"))?;
            let data = if used_textures.contains(guid.as_str()) {
                premultiply_png_texture(data)?
            } else {
                data
            };
            let slot = textures.len();
            textures.push(TextureBlob { tag: 0, data });
            texture_slots.insert(guid, slot);
        }
        Ok(Self {
            parsed,
            source,
            textures,
            texture_slots,
        })
    }

    /// A scene from a runtime model: `textures` are the PNG files the `.model3.json` lists, in its order, each with
    /// the GUID the scene's texture records carry.
    pub(crate) fn from_runtime(parsed: ParsedScene, textures: Vec<(String, Vec<u8>)>) -> Result<Self, String> {
        let source = parsed.source_value()?;
        let mut blobs = Vec::with_capacity(textures.len());
        let mut texture_slots = HashMap::new();
        for (guid, data) in textures {
            texture_slots.insert(guid, blobs.len());
            blobs.push(TextureBlob { tag: 0, data: premultiply_png_texture(data)? });
        }
        Ok(Self { parsed, source, textures: blobs, texture_slots })
    }

    pub(crate) fn source(&self) -> &Value {
        &self.source
    }

    pub(crate) fn textures(&self) -> &[TextureBlob] {
        &self.textures
    }

    pub(crate) fn parameters(&self) -> &[Value] {
        self.source["parameters"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    #[cfg(test)]
    pub(crate) fn render_model(
        &self,
        values: &BTreeMap<String, f64>,
    ) -> Result<(Value, Vec<String>), String> {
        self.render_model_with_parts(values, &BTreeMap::new())
    }

    pub(crate) fn render_model_with_parts(
        &self,
        values: &BTreeMap<String, f64>,
        part_opacities: &BTreeMap<String, f64>,
    ) -> Result<(Value, Vec<String>), String> {
        let parameter_values = values
            .iter()
            .map(|(id, value)| (id.clone(), json!(value)))
            .collect::<Map<_, _>>();
        let morph = self
            .parsed
            .morph_forms(&json!({"parameterValues": parameter_values, "clampToKeys": true}))?;
        let evaluated = morph["sources"]
            .as_array()
            .ok_or("native morph evaluation returned no source list")?;
        let forms = evaluated
            .iter()
            .map(|source| {
                let guid = string_field(source, "guid", "evaluated source")?;
                Ok((guid.to_owned(), source))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        let sources = self.source["sources"]
            .as_array()
            .ok_or("native model source list is missing")?;
        let source_by_guid = sources
            .iter()
            .map(|source| {
                let guid = string_field(source, "guid", "source")?;
                Ok((guid.to_owned(), source))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        let ids = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let guid = string_field(source, "guid", "source")?;
                let id = u32::try_from(index + 2)
                    .map_err(|_| "native source count exceeds the render ID limit")?;
                Ok((guid.to_owned(), id))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        let (palette_ranks, unresolved_palette_children) =
            native_palette_ranks(&self.source, sources)?;
        let node_palette_ranks = palette_ranks.as_ref().map(|ranks| {
            ranks
                .iter()
                .map(|(guid, rank)| (u64::from(ids[guid]), *rank))
                .collect::<HashMap<_, _>>()
        });
        let plain_group_orders = native_plain_group_orders(sources, &forms)?;
        let mut nested_plain_groups = false;
        for guid in plain_group_orders.keys() {
            nested_plain_groups |= !native_plain_group_ancestors(
                guid,
                &source_by_guid,
                &plain_group_orders,
            )?
            .is_empty();
        }
        let mut mask_sources = HashSet::new();
        for source in sources {
            if !matches!(source["data"]["type"].as_str(), Some("ArtMesh" | "Part")) {
                continue;
            }
            let clips = source["data"]["clipGuids"]
                .as_array()
                .ok_or("native clipping mask list is missing")?;
            for clip in clips.iter().filter_map(Value::as_str) {
                if !source_by_guid
                    .get(clip)
                    .is_some_and(|mask| mask["data"]["type"] == "ArtMesh")
                {
                    return Err(format!(
                        "native clipping mask {clip:?} is missing or is not an ArtMesh"
                    ));
                }
                mask_sources.insert(clip.to_owned());
            }
        }
        let mut warnings = Vec::new();
        for source in sources.iter().filter(|source| source["data"]["type"] == "ArtPath") {
            if inherited_visibility(source, &source_by_guid)? {
                warnings.push(ART_PATH_NOT_DRAWN.into());
                break;
            }
        }
        // Every drawn ArtMesh is mapped to the canvas before a node is built: a Glue moves vertices of two meshes.
        let mut frames = HashMap::new();
        let mut mesh_vertices = HashMap::new();
        for source in sources.iter().filter(|source| source["data"]["type"] == "ArtMesh") {
            let guid = string_field(source, "guid", "art mesh")?;
            if !inherited_visibility(source, &source_by_guid)? && !mask_sources.contains(guid) {
                continue;
            }
            let form = select_form(source, forms.get(guid).copied())?;
            if form["data"]["type"] != "ArtMesh" {
                return Err(format!(
                    "native art mesh {guid:?} has no evaluated geometry"
                ));
            }
            if form["vertexCountMatchesSource"] == false {
                return Err(format!(
                    "native art mesh {guid:?} has stale evaluated geometry"
                ));
            }
            let raw_positions = number_array(&form["data"]["positions"], "native art mesh positions")?;
            let target = optional_string(source, "targetDeformerGuid")?;
            let mut vertices = Vec::with_capacity(raw_positions.len());
            for pair in raw_positions.chunks_exact(2) {
                let point = self.to_canvas(
                    [pair[0], pair[1]],
                    target,
                    &source_by_guid,
                    &forms,
                    &mut HashSet::new(),
                    0,
                    &mut warnings,
                    &mut frames,
                )?;
                vertices.extend(point);
            }
            mesh_vertices.insert(guid.to_owned(), vertices);
        }
        apply_glue(sources, &source_by_guid, &forms, &mut mesh_vertices, &mut warnings)?;
        let mut culled = false;
        if palette_ranks.is_none() {
            warnings.push(
                "native Part palette order is unavailable; source-array order is used for equal drawOrder".into(),
            );
        }
        if unresolved_palette_children {
            warnings.push(
                "native palette references unsupported child sources; their effects are unhandled"
                    .into(),
            );
        }
        if nested_plain_groups {
            warnings.push(
                "native nested non-offscreen draw-order groups use inferred hierarchy ordering"
                    .into(),
            );
        }
        let mut children = Vec::new();
        let mut scoped_nodes = HashMap::<String, Vec<Value>>::new();
        let mut offscreens = Vec::new();
        for source in sources.iter().filter(|s| s["data"]["type"] == "Part") {
            if source["data"]["useOffscreen"] != true {
                continue;
            }
            let guid = string_field(source, "guid", "offscreen part")?;
            let form = select_optional_form(source, forms.get(guid).copied())?
                .ok_or("native offscreen part has no evaluated form")?;
            let (opacity, multiply, screen, clips, parent) = native_layer(
                source,
                form,
                &source_by_guid,
                &forms,
                part_opacities,
                &mut warnings,
            )?;
            let color = string_field(&source["data"], "colorComposition", "offscreen part")?;
            let alpha = string_field(&source["data"], "alphaComposition", "offscreen part")?;
            let blend = native_blend_mode(color, alpha)?;
            let node = json!({
                "uuid": ids[guid], "name": source["name"], "type": "Composite",
                "enabled": inherited_visibility(source, &source_by_guid)?,
                "zsort": -whole_native_draw_order(number_field(form, "drawOrder", 0.0)?),
                "blend_mode": blend, "nativeColorBlend": color, "nativeAlphaBlend": alpha,
                "opacity": opacity.clamp(0.0, 1.0), "tint": &multiply[..3],
                "screenTint": &screen[..3], "masks": native_masks(&clips, &ids)?,
                "transform": {"trans":[0,0,0],"rot":[0,0,0],"scale":[1,1]},
                "children": []
            });
            offscreens.push((guid.to_owned(), parent, node));
        }
        for source in sources {
            if source["data"]["type"] != "ArtMesh" {
                continue;
            }
            let guid = string_field(source, "guid", "art mesh")?;
            let visible = inherited_visibility(source, &source_by_guid)?;
            if !visible && !mask_sources.contains(guid) {
                continue;
            }
            culled |= source["data"]["culling"] == true;
            let form = select_form(source, forms.get(guid).copied())?;
            let source_data = &source["data"];
            if !matches!(
                source_data["textureState"].as_str(),
                Some("TEXTURE_ATLAS" | "MODEL_IMAGE")
            ) {
                return Err(format!(
                    "native art mesh {guid:?} uses unsupported texture state {}",
                    source_data["textureState"]
                ));
            }
            let vertices = mesh_vertices
                .remove(guid)
                .ok_or("native art mesh was not mapped to the canvas")?;
            let texture_guid = string_field(source_data, "textureGuid", "art mesh")?;
            let texture_slot = *self.texture_slots.get(texture_guid).ok_or_else(|| {
                format!("native art mesh references missing texture {texture_guid:?}")
            })?;
            let texture = &self.source["textures"][texture_slot];
            // UVs address the texture image as stored; the texture's image-to-logical affine only names the
            // content box inside the padded image (every ren_ja and hiyori_pro texture confirmed this).
            // Checked here; the stored array itself goes into the node below.
            number_array(&source_data["uvs"], "native art mesh UVs")?;
            let indices = source_data["indices"]
                .as_array()
                .ok_or("native art mesh indices are missing")?;
            let (opacity, multiply, screen, clips, parent) = native_layer(
                source,
                form,
                &source_by_guid,
                &forms,
                part_opacities,
                &mut warnings,
            )?;
            let alpha_composition = string_field(source_data, "alphaComposition", "art mesh")?;
            let color_composition = string_field(source_data, "colorComposition", "art mesh")?;
            let blend_mode = native_blend_mode(color_composition, alpha_composition)?;
            let mask_ids = native_masks(&clips, &ids)?;
            let native_order = whole_native_draw_order(form["drawOrder"].as_f64().unwrap_or(0.0));
            let id = ids[guid];
            let mut node = json!({
                "uuid": id, "name": source["name"], "type": "Part",
                "enabled": visible, "zsort": -native_order,
                "textures": [texture_slot, u32::MAX, u32::MAX],
                "blend_mode": blend_mode, "tint": [multiply[0],multiply[1],multiply[2]],
                "nativeColorBlend": color_composition, "nativeAlphaBlend": alpha_composition,
                "screenTint": [screen[0],screen[1],screen[2]],
                "opacity": opacity.clamp(0.0,1.0), "masks": mask_ids,
                "transform": {"trans":[0,0,0],"rot":[0,0,0],"scale":[1,1]},
            });
            // The arrays move in; `json!` would serialize them into copies, formatting every number again.
            let mut mesh = Map::new();
            mesh.insert("verts".into(), Value::Array(vertices.into_iter().map(Value::from).collect()));
            mesh.insert("uvs".into(), source_data["uvs"].clone());
            mesh.insert("indices".into(), Value::Array(indices.clone()));
            mesh.insert("origin".into(), json!([0, 0]));
            node["mesh"] = Value::Object(mesh);
            if texture["minFilter"].as_str().is_some_and(|filter| filter.contains("MIPMAP")) {
                node["textureMipmaps"] = json!(true);
            }
            if let Some(parent) = parent {
                scoped_nodes.entry(parent).or_default().push(node);
            } else {
                children.push(node);
            }
        }
        assemble_offscreens(
            &offscreens,
            &mut scoped_nodes,
            &mut children,
            node_palette_ranks.as_ref(),
        )?;
        if plain_group_orders.is_empty() {
            sort_render_nodes(&mut children, node_palette_ranks.as_ref())?;
        } else {
            let guid_by_id = ids
                .iter()
                .map(|(guid, id)| (u64::from(*id), guid.as_str()))
                .collect::<HashMap<_, _>>();
            sort_native_group_scopes(
                &mut children,
                &guid_by_id,
                &source_by_guid,
                &plain_group_orders,
                palette_ranks.as_ref(),
                &ids,
            )?;
        }
        let mut root = json!({
            "uuid": ROOT_ID, "type": "Node", "enabled": true, "zsort": 0,
            "transform": {"trans":[0,0,0],"rot":[0,0,0],"scale":[1,1]},
        });
        root["children"] = Value::Array(children);
        let mut model = Map::new();
        model.insert("nodes".into(), root);
        let model = Value::Object(model);
        // ponytail: back faces are drawn; cull by triangle winding in the rasterizers if a model shows them.
        if culled {
            warnings.push("native face culling is unsupported; back faces are drawn".into());
        }
        warnings.sort();
        warnings.dedup();
        Ok((model, warnings))
    }

    fn to_canvas(
        &self,
        point: [f64; 2],
        target: Option<&str>,
        sources: &HashMap<String, &Value>,
        forms: &HashMap<String, &Value>,
        visiting: &mut HashSet<String>,
        depth: usize,
        warnings: &mut Vec<String>,
        frames: &mut HashMap<String, Frame>,
    ) -> Result<[f64; 2], String> {
        let Some(guid) = target else { return Ok(point) };
        if guid == crate::authoring_hierarchy::ROOT_DEFORMER_GUID {
            return Ok(point);
        }
        if depth >= MAX_DEFORMER_DEPTH || !visiting.insert(guid.to_owned()) {
            return Err("native deformer hierarchy is cyclic or too deep".into());
        }
        let Some(source) = sources.get(guid) else {
            visiting.remove(guid);
            return Err("native target deformer does not resolve".into());
        };
        let parent = optional_string(source, "targetDeformerGuid")?;
        // A deformer's frame is read from its evaluated form once per pose and applied to every vertex below it.
        if !frames.contains_key(guid) {
            let frame = self.frame(source, parent, sources, forms, visiting, depth, warnings, frames)?;
            frames.insert(guid.to_owned(), frame);
        }
        let step = match &frames[guid] {
            Frame::Warp { .. } => None,
            Frame::Rotation { transform, warp } => {
                Some(Step::Rotation { transform: *transform, warp: *warp })
            }
            Frame::Part => Some(Step::Part),
        };
        let result = match step {
            None => {
                let Frame::Warp { grid, columns, rows, quad } = &frames[guid] else { unreachable!() };
                warp_point(point, grid, *columns, *rows, *quad)?
            }
            Some(Step::Rotation { transform, warp }) => {
                match warp {
                    // The warp turns the deformer by the angle it turns this one direction. It does not scale or
                    // shear it; the rotation deformers above the warp scale it, as they scale the warp's own grid.
                    // `down` is where the warp sends the local +y direction.
                    Some((center, down, unit)) => {
                        let local = transform.offset(point);
                        [center[0] + (down[1] * local[0] + down[0] * local[1]) * unit,
                         center[1] + (down[1] * local[1] - down[0] * local[0]) * unit]
                    }
                    None => self.to_canvas(
                        transform.map(point),
                        parent,
                        sources,
                        forms,
                        visiting,
                        depth + 1,
                        warnings,
                        frames,
                    )?,
                }
            }
            Some(Step::Part) => {
                self.to_canvas(point, parent, sources, forms, visiting, depth + 1, warnings, frames)?
            }
        };
        visiting.remove(guid);
        if result.iter().any(|v| !v.is_finite()) {
            return Err("native geometry transform produced non-finite coordinates".into());
        }
        Ok(result)
    }

    /// Reads one deformer's evaluated form into the frame `to_canvas` applies: a warp's grid mapped to the canvas,
    /// or a rotation deformer's angle, origin, scale and reflection, with its place and direction in a parent warp.
    fn frame(
        &self,
        source: &Value,
        parent: Option<&str>,
        sources: &HashMap<String, &Value>,
        forms: &HashMap<String, &Value>,
        visiting: &mut HashSet<String>,
        depth: usize,
        warnings: &mut Vec<String>,
        frames: &mut HashMap<String, Frame>,
    ) -> Result<Frame, String> {
        let guid = string_field(source, "guid", "deformer")?;
        let form = select_form(source, forms.get(guid).copied())?;
        match source["data"]["type"].as_str() {
            Some("Warp") => {
                let columns = source["data"]["columns"]
                    .as_u64()
                    .ok_or("native warp column count is missing")?
                    as usize;
                let rows = source["data"]["rows"]
                    .as_u64()
                    .ok_or("native warp row count is missing")? as usize;
                // Cubism maps the grid to the canvas and interpolates in the mapped grid. Mapping the interpolated
                // point instead gives another result wherever a parent is not affine across the cell.
                let positions = number_array(&form["data"]["positions"], "native warp positions")?;
                let expected = columns
                    .checked_add(1)
                    .and_then(|x| rows.checked_add(1).and_then(|y| x.checked_mul(y)))
                    .and_then(|n| n.checked_mul(2))
                    .ok_or("native warp grid size overflows")?;
                if positions.len() != expected || columns == 0 || rows == 0 {
                    return Err(
                        "native warp grid dimensions do not match evaluated positions".into(),
                    );
                }
                let mut grid = Vec::with_capacity(positions.len());
                for node in positions.chunks_exact(2) {
                    grid.extend(self.to_canvas(
                        [node[0], node[1]],
                        parent,
                        sources,
                        forms,
                        visiting,
                        depth + 1,
                        warnings,
                        frames,
                    )?);
                }
                let quad = source["data"]["quadTransform"].as_bool().unwrap_or(false);
                Ok(Frame::Warp { grid, columns, rows, quad })
            }
            Some("Rotation") => {
                let transform = RotationTransform::read(source, form)?;
                if transform.reflect_x || transform.reflect_y {
                    warnings.push(ROTATION_UNVERIFIED.into());
                }
                let parent_warp = sources
                    .get(parent.unwrap_or_default())
                    .is_some_and(|parent_source| parent_source["data"]["type"] == "Warp");
                let warp = if parent_warp {
                    let center = self.to_canvas(transform.origin, parent, sources, forms, visiting, depth + 1, warnings, frames)?;
                    let above = self.to_canvas(
                        [transform.origin[0], transform.origin[1] - WARP_ROTATION_PROBE],
                        parent,
                        sources,
                        forms,
                        visiting,
                        depth + 1,
                        warnings,
                        frames,
                    )?;
                    let unit = self.ancestor_rotation_scale(parent, sources, forms)?;
                    let down = normalize([center[0] - above[0], center[1] - above[1]])?;
                    Some((center, down, unit))
                } else {
                    None
                };
                Ok(Frame::Rotation { transform, warp })
            }
            Some("Part") => Ok(Frame::Part),
            other => Err(format!("unsupported native deformer type {other:?}")),
        }
    }
}

/// One deformer as `to_canvas` applies it to a vertex, read once per pose.
enum Frame {
    /// The warp's grid mapped to the canvas.
    Warp { grid: Vec<f64>, columns: usize, rows: usize, quad: bool },
    /// A rotation deformer; `warp` holds its canvas centre, the direction its parent warp sends local +y and the
    /// scale of the rotation deformers above that warp when the parent is a warp.
    Rotation { transform: RotationTransform, warp: Option<([f64; 2], [f64; 2], f64)> },
    Part,
}

/// What `to_canvas` copies out of a frame before it recurses.
#[derive(Clone, Copy)]
enum Step {
    Rotation { transform: RotationTransform, warp: Option<([f64; 2], [f64; 2], f64)> },
    Part,
}

#[derive(Clone, Copy)]
struct RotationTransform {
    angle: f64,
    origin: [f64; 2],
    scale: f64,
    reflect_x: bool,
    reflect_y: bool,
}

impl RotationTransform {
    fn read(source: &Value, form: &Value) -> Result<Self, String> {
        let data = &form["data"];
        let angle = number_field(data, "angle", 0.0)?.to_radians();
        let origin = number_vec(data, "origin", [0.0, 0.0])?;
        let scale = number_field(data, "scale", 1.0)?;
        let reflect_x = data["reflectX"].as_bool().unwrap_or(false);
        let reflect_y = data["reflectY"].as_bool().unwrap_or(false);
        let base_angle = number_field(&source["data"], "baseAngle", 0.0)?.to_radians();
        Ok(Self {
            angle: angle + base_angle, origin, scale, reflect_x, reflect_y,
        })
    }

    fn offset(self, point: [f64; 2]) -> [f64; 2] {
        rotation_offset(point, self.angle, self.scale, self.reflect_x, self.reflect_y)
    }

    fn map(self, point: [f64; 2]) -> [f64; 2] {
        let local = self.offset(point);
        [self.origin[0] + local[0], self.origin[1] + local[1]]
    }
}

fn static_primary_form(source: &Value) -> Result<&Value, String> {
    let forms = source["forms"].as_array().ok_or("deformer reparent requires primary forms")?;
    let [form] = forms.as_slice() else { return Err("deformer reparent requires one primary form".into()) };
    string_field(form, "guid", "primary form")?;
    if form["data"]["type"] != source["data"]["type"]
        || form["animated"] != false || form["localAnimated"] != false
        || form["vertexCountMatchesSource"] == false
    {
        return Err("deformer reparent requires an unanimated primary form of its source type".into());
    }
    if !source["extendedInterpolations"].as_array().is_some_and(Vec::is_empty)
        || source.get("evaluatedForms").is_some()
    {
        return Err("deformer reparent does not support extended or generated forms".into());
    }
    let morph = &source["morphTargets"];
    if !morph.is_null() && !(morph["targets"].as_array().is_some_and(Vec::is_empty)
        && morph["blendWeightConstraints"].as_array().is_some_and(Vec::is_empty))
    {
        return Err("deformer reparent does not support morph targets or constraints".into());
    }
    let grid = &source["grid"];
    if !grid.is_null() {
        let cells = grid["cells"].as_array().ok_or("deformer reparent requires a static form grid")?;
        if !grid["axes"].as_array().is_some_and(Vec::is_empty) || cells.len() != 1
            || cells[0]["formGuid"] != form["guid"]
            || !cells[0]["indices"].as_array().is_some_and(Vec::is_empty)
        {
            return Err("deformer reparent does not support keyed forms".into());
        }
    }
    Ok(form)
}

struct StaticRotationChain(Vec<RotationTransform>);

impl StaticRotationChain {
    fn read<'a>(sources: &HashMap<&'a str, &'a Value>, target: &str) -> Result<Self, String> {
        let mut target = *sources.get_key_value(target).ok_or("deformer reparent target does not resolve")?.0;
        let mut steps = Vec::new();
        let mut seen = HashSet::new();
        let mut scale = 1.0;
        while target != crate::authoring_hierarchy::ROOT_DEFORMER_GUID {
            if steps.len() >= MAX_DEFORMER_DEPTH || !seen.insert(target) {
                return Err("deformer reparent chain is cyclic or too deep".into());
            }
            let source = sources.get(target).ok_or("deformer reparent target does not resolve")?;
            if source["data"]["type"] != "Rotation" {
                return Err("deformer reparent supports only rotation chains".into());
            }
            let form = static_primary_form(source)?;
            let parent = string_field(source, "targetDeformerGuid", "rotation")?;
            let coordinate = if parent == crate::authoring_hierarchy::ROOT_DEFORMER_GUID { "Canvas" } else { "DeformerLocal" };
            if form["coordinateType"] != coordinate || form["opacity"].as_f64() != Some(1.0) {
                return Err("deformer reparent requires supported rotation coordinates and neutral opacity".into());
            }
            if ![&form["data"]["angle"], &form["data"]["scale"], &source["data"]["baseAngle"]]
                .iter().all(|value| value.as_f64().is_some_and(f64::is_finite))
                || !form["data"]["reflectX"].is_boolean() || !form["data"]["reflectY"].is_boolean()
                || form["data"]["origin"].as_array().map(Vec::len) != Some(2)
            {
                return Err("deformer reparent requires explicit finite rotation fields".into());
            }
            let transform = RotationTransform::read(source, form)?;
            // Positive scales are the bounded authoring subset, not a claim that negative scales are singular.
            if transform.reflect_x || transform.reflect_y || transform.scale < 1.0e-6 {
                return Err("deformer reparent requires unreflected positive rotation scales of at least 1e-6".into());
            }
            scale *= transform.scale;
            if !scale.is_finite() || !(1.0 / scale).is_finite() || !transform.angle.is_finite() {
                return Err("deformer reparent rotation chain scale is not invertible".into());
            }
            steps.push(transform);
            target = parent;
        }
        if steps.is_empty() || scale < 1.0e-6 {
            return Err("deformer reparent requires a rotation chain with scale at least 1e-6".into());
        }
        Ok(Self(steps))
    }

    fn to_canvas(&self, mut point: [f64; 2]) -> Result<[f64; 2], String> {
        for step in &self.0 {
            point = step.map(point);
            if point.iter().any(|n| !n.is_finite()) {
                return Err("deformer reparent produced non-finite canvas coordinates".into());
            }
        }
        Ok(point)
    }

    fn from_canvas(&self, mut point: [f64; 2]) -> Result<[f64; 2], String> {
        for step in self.0.iter().rev() {
            point = rotation_offset([point[0] - step.origin[0], point[1] - step.origin[1]],
                -step.angle, 1.0 / step.scale, false, false);
            if point.iter().any(|n| !n.is_finite()) {
                return Err("deformer reparent produced non-finite local coordinates".into());
            }
        }
        Ok(point)
    }
}

/// Reexpress one static ArtMesh using the same rotation components and forward mapping as the renderer.
pub(crate) fn reexpress_rotation_positions(model: &Value, source_guid: &str, target: &str) -> Result<Vec<f64>, String> {
    let list = model["sources"].as_array().ok_or("deformer reparent requires sources")?;
    let mut sources = HashMap::new();
    for source in list {
        let guid = string_field(source, "guid", "source")?;
        if sources.insert(guid, source).is_some() {
            return Err("deformer reparent source GUID is ambiguous".into());
        }
    }
    let mesh = sources.get(source_guid).ok_or("deformer reparent mesh was not found")?;
    if mesh["data"]["type"] != "ArtMesh" {
        return Err("deformer reparent requires an ArtMesh".into());
    }
    let form = static_primary_form(mesh)?;
    if form["coordinateType"] != "DeformerLocal" {
        return Err("deformer reparent requires a DeformerLocal mesh form".into());
    }
    if list.iter().any(|source| source["data"]["type"] == "Glue"
        && ["targetArtMeshAGuid", "targetArtMeshBGuid"].iter().any(|key| source["data"][*key].as_str() == Some(source_guid)))
    {
        return Err("deformer reparent does not support a mesh used by Glue".into());
    }
    let positions = number_array(&form["data"]["positions"], "deformer reparent positions")?;
    let original = number_array(&mesh["data"]["positions"], "deformer reparent source positions")?;
    if positions.len() != original.len() || positions.len() % 2 != 0 {
        return Err("deformer reparent form positions do not match its source".into());
    }
    let old = StaticRotationChain::read(&sources, string_field(mesh, "targetDeformerGuid", "ArtMesh")?)?;
    let new = StaticRotationChain::read(&sources, target)?;
    let mut result = Vec::with_capacity(positions.len());
    for pair in positions.chunks_exact(2) {
        let canvas = old.to_canvas([pair[0], pair[1]])?;
        let restored = old.from_canvas(canvas)?;
        if (0..2).any(|axis| (restored[axis] - pair[axis]).abs() > 1.0e-6) {
            return Err("deformer reparent loses more than 1e-6 source-local coordinate precision".into());
        }
        let point = new.from_canvas(canvas)?;
        if point.iter().any(|n| !(*n as f32).is_finite()) {
            return Err("deformer reparent positions exceed native float range".into());
        }
        let verified = new.to_canvas(point)?;
        if (0..2).any(|axis| (verified[axis] - canvas[axis]).abs() > 1.0e-6) {
            return Err("deformer reparent exceeds 1e-6 canvas coordinate error".into());
        }
        let restored = old.from_canvas(verified)?;
        if (0..2).any(|axis| (restored[axis] - pair[axis]).abs() > 1.0e-6) {
            return Err("deformer reparent loses more than 1e-6 source-local coordinate precision".into());
        }
        result.extend(point);
    }
    Ok(result)
}

impl NativeScene {
    /// The product of the scales of the rotation deformers at and above `target`. A rotation deformer under a warp
    /// measures its local pixels in the warp's parent space, so Cubism Core scales it by these and by no warp.
    fn ancestor_rotation_scale(
        &self,
        target: Option<&str>,
        sources: &HashMap<String, &Value>,
        forms: &HashMap<String, &Value>,
    ) -> Result<f64, String> {
        let mut scale = 1.0;
        let mut guid = target;
        for _ in 0..MAX_DEFORMER_DEPTH {
            let Some(current) = guid else { return Ok(scale) };
            if current == crate::authoring_hierarchy::ROOT_DEFORMER_GUID {
                return Ok(scale);
            }
            let Some(source) = sources.get(current) else {
                return Err("native target deformer does not resolve".into());
            };
            if source["data"]["type"] == "Rotation" {
                let form = select_form(source, forms.get(current).copied())?;
                scale *= number_field(&form["data"], "scale", 1.0)?;
            }
            guid = optional_string(source, "targetDeformerGuid")?;
        }
        Err("native deformer hierarchy is cyclic or too deep".into())
    }
}

fn select_form<'a>(source: &'a Value, evaluated: Option<&'a Value>) -> Result<&'a Value, String> {
    let forms = evaluated
        .and_then(|source| source["evaluatedForms"].as_array())
        .or_else(|| source["forms"].as_array())
        .ok_or("native source has no evaluated forms")?;
    if forms.len() == 1 {
        return Ok(&forms[0]);
    }
    let source_type = source["data"]["type"].as_str();
    let candidates = forms
        .iter()
        .filter(|form| {
            let expected = match source_type {
                Some("ArtMesh") => "ArtMesh",
                Some("Warp") => "Warp",
                Some("Rotation") => "Rotation",
                Some("Part") => "Part",
                _ => "",
            };
            form["data"]["type"] == expected && form["coordinateType"] == "DeformerLocal"
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }
    Err("native source has ambiguous coordinate representations".into())
}

fn inherited_visibility(source: &Value, sources: &HashMap<String, &Value>) -> Result<bool, String> {
    let mut visible = source["visible"].as_bool().unwrap_or(true);
    let mut parent = optional_string(source, "parentPartGuid")?;
    let mut seen = HashSet::new();
    while let Some(guid) = parent {
        if !seen.insert(guid) || seen.len() > MAX_DEFORMER_DEPTH {
            return Err("native part hierarchy is cyclic or too deep".into());
        }
        let part = sources
            .get(guid)
            .ok_or("native art mesh references a missing parent part")?;
        if part["data"]["type"] != "Part" {
            return Err("native art mesh parent is not a part".into());
        }
        visible &= part["visible"].as_bool().unwrap_or(true) && part["data"]["sketch"] != true;
        parent = optional_string(part, "parentPartGuid")?;
    }
    Ok(visible)
}

fn select_optional_form<'a>(
    source: &'a Value,
    evaluated: Option<&'a Value>,
) -> Result<Option<&'a Value>, String> {
    if source["forms"].as_array().is_some_and(Vec::is_empty)
        && evaluated.is_none_or(|item| item["evaluatedForms"].as_array().is_none_or(Vec::is_empty))
    {
        return Ok(None);
    }
    select_form(source, evaluated).map(Some)
}

/// Applies each Glue to the mapped meshes the way Cubism Core 6.0.1 output shows: vertex A moves `weightA * intensity`
/// of the way to vertex B while B moves `weightB * intensity` of the way to A, one pair after another.
///
/// The Glues run from the last source to the first. One compared model binds vertices to two Glues, and it matches
/// in this order and not in source order.
fn apply_glue(
    sources: &[Value],
    source_by_guid: &HashMap<String, &Value>,
    forms: &HashMap<String, &Value>,
    vertices: &mut HashMap<String, Vec<f64>>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    for source in sources.iter().rev().filter(|source| source["data"]["type"] == "Glue") {
        let guid = string_field(source, "guid", "glue")?;
        let data = &source["data"];
        let intensity = select_optional_form(source, forms.get(guid).copied())?
            .and_then(|form| form["data"]["intensity"].as_f64());
        // ponytail: a Glue to a mesh that is not drawn is left out; map hidden targets too if Cubism turns out to
        // apply it.
        let slots = |key: &str| {
            let mesh = data[key].as_str()?;
            let count = vertices.get(mesh)?.len() / 2;
            let uids = source_by_guid.get(mesh)?["data"]["vertexUids"].as_array()?;
            let slots = uids
                .iter()
                .take(count)
                .enumerate()
                .filter_map(|(index, uid)| Some((uid.as_i64()?, index * 2)))
                .collect::<HashMap<_, _>>();
            Some((mesh, slots))
        };
        let pairs = (|| {
            let ((a, slots_a), (b, slots_b)) = (slots("targetArtMeshAGuid")?, slots("targetArtMeshBGuid")?);
            let (uids, weights) = (data["bindVertexUids"].as_array()?, data["weights"].as_array()?);
            if a == b || uids.len() != weights.len() || uids.len() % 2 != 0 {
                return None;
            }
            let intensity = intensity?;
            // A pair whose vertex was deleted stays in the file (`staleBindPairs`); it binds nothing.
            let pairs = uids
                .chunks_exact(2)
                .zip(weights.chunks_exact(2))
                .map(|(uid, weight)| {
                    Some((
                        *slots_a.get(&uid[0].as_i64()?)?,
                        *slots_b.get(&uid[1].as_i64()?)?,
                        weight[0].as_f64()? * intensity,
                        weight[1].as_f64()? * intensity,
                    ))
                })
                .collect::<Vec<_>>();
            Some((a.to_owned(), b.to_owned(), pairs))
        })();
        let Some((a, b, pairs)) = pairs else {
            warnings.push(GLUE_SKIPPED.into());
            continue;
        };
        if pairs.iter().any(Option::is_none) {
            warnings.push(GLUE_STALE_PAIR.into());
        }
        for (slot_a, slot_b, weight_a, weight_b) in pairs.into_iter().flatten() {
            for axis in 0..2 {
                let (from, to) = (vertices[&a][slot_a + axis], vertices[&b][slot_b + axis]);
                if !((to - from) * weight_a).is_finite() || !((to - from) * weight_b).is_finite() {
                    return Err("native Glue produced non-finite coordinates".into());
                }
                vertices.get_mut(&a).ok_or("native Glue target is missing")?[slot_a + axis] =
                    from + (to - from) * weight_a;
                vertices.get_mut(&b).ok_or("native Glue target is missing")?[slot_b + axis] =
                    to - (to - from) * weight_b;
            }
        }
    }
    Ok(())
}

/// A quad cut along the diagonal from its second to its third corner, as Cubism interpolates one.
/// Corners in the order (0,0), (1,0), (0,1), (1,1).
fn triangle_pair(q: [[f64; 2]; 4], u: f64, v: f64) -> [f64; 2] {
    let at = |i: usize| {
        if u + v <= 1.0 {
            q[0][i] + (q[1][i] - q[0][i]) * u + (q[2][i] - q[0][i]) * v
        } else {
            q[3][i] + (q[2][i] - q[3][i]) * (1.0 - u) + (q[1][i] - q[3][i]) * (1.0 - v)
        }
    };
    [at(0), at(1)]
}

/// Maps a point of a warp deformer's 0..1 square to its parent.
///
/// Inside the grid a cell is two triangles, or bilinear for a quad-transform deformer. Outside it the four grid
/// corners give one affine map, used as it is beyond -2..3; the border in between is a ring of quads from the grid
/// edge to that affine map, each cut into two triangles. Checked against Cubism Core 6.0.1 vertex positions.
fn warp_point(
    point: [f64; 2],
    positions: &[f64],
    columns: usize,
    rows: usize,
    quad: bool,
) -> Result<[f64; 2], String> {
    let [x, y] = point;
    if !x.is_finite() || !y.is_finite() || x.abs() > 1.0e6 || y.abs() > 1.0e6 {
        return Err("native warp coordinate exceeds the extrapolation limit".into());
    }
    let node = |column: usize, row: usize| {
        let i = (row * (columns + 1) + column) * 2;
        [positions[i], positions[i + 1]]
    };
    let finite = |out: [f64; 2]| {
        if out.iter().all(|value| value.is_finite()) {
            Ok(out)
        } else {
            Err("native warp produced non-finite coordinates".to_owned())
        }
    };
    if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) {
        if quad {
            return bilinear_grid(point, positions, columns, rows);
        }
        let (gx, gy) = (x * columns as f64, y * rows as f64);
        let (column, row) = ((gx as usize).min(columns - 1), (gy as usize).min(rows - 1));
        let cell = [node(column, row), node(column + 1, row), node(column, row + 1), node(column + 1, row + 1)];
        return finite(triangle_pair(cell, gx - column as f64, gy - row as f64));
    }
    let (p00, p10, p01, p11) = (node(0, 0), node(columns, 0), node(0, rows), node(columns, rows));
    let affine = |ax: f64, ay: f64| {
        let at = |i: usize| {
            let dx = ((p10[i] - p00[i]) + (p11[i] - p01[i])) / 2.0;
            let dy = ((p01[i] - p00[i]) + (p11[i] - p10[i])) / 2.0;
            (p00[i] + p10[i] + p01[i] + p11[i]) / 4.0 - (dx + dy) / 2.0 + ax * dx + ay * dy
        };
        [at(0), at(1)]
    };
    if !(x > -2.0 && x < 3.0 && y > -2.0 && y < 3.0) {
        return finite(affine(x, y));
    }
    // The span of the border quad along one axis: before the grid, one grid cell, or after the grid.
    let span = |t: f64, cells: usize| {
        if t <= 0.0 {
            (-2.0, 0.0)
        } else if t >= 1.0 {
            (1.0, 3.0)
        } else {
            let cell = ((t * cells as f64) as usize).min(cells - 1);
            (cell as f64 / cells as f64, (cell + 1) as f64 / cells as f64)
        }
    };
    let ((x0, x1), (y0, y1)) = (span(x, columns), span(y, rows));
    let corner = |cx: f64, cy: f64| {
        if (0.0..=1.0).contains(&cx) && (0.0..=1.0).contains(&cy) {
            node((cx * columns as f64).round() as usize, (cy * rows as f64).round() as usize)
        } else {
            affine(cx, cy)
        }
    };
    let ring = [corner(x0, y0), corner(x1, y0), corner(x0, y1), corner(x1, y1)];
    finite(triangle_pair(ring, (x - x0) / (x1 - x0), (y - y0) / (y1 - y0)))
}

fn bilinear_grid(
    point: [f64; 2],
    positions: &[f64],
    columns: usize,
    rows: usize,
) -> Result<[f64; 2], String> {
    let gx = point[0] * columns as f64;
    let gy = point[1] * rows as f64;
    let col = (gx.floor() as isize).clamp(0, columns as isize - 1) as usize;
    let row = (gy.floor() as isize).clamp(0, rows as isize - 1) as usize;
    let u = gx - col as f64;
    let v = gy - row as f64;
    if !u.is_finite() || !v.is_finite() || u.abs() > 1.0e6 || v.abs() > 1.0e6 {
        return Err("native warp coordinate exceeds the extrapolation limit".into());
    }
    let at = |x: usize, y: usize| {
        let i = (y * (columns + 1) + x) * 2;
        [positions[i], positions[i + 1]]
    };
    let p00 = at(col, row);
    let p10 = at(col + 1, row);
    let p01 = at(col, row + 1);
    let p11 = at(col + 1, row + 1);
    let blend = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let out = [
        blend(blend(p00[0], p10[0], u), blend(p01[0], p11[0], u), v),
        blend(blend(p00[1], p10[1], u), blend(p01[1], p11[1], u), v),
    ];
    if out.iter().any(|value| !value.is_finite()) {
        return Err("native warp produced non-finite coordinates".into());
    }
    Ok(out)
}

fn normalize(value: [f64; 2]) -> Result<[f64; 2], String> {
    let length = value[0].hypot(value[1]);
    if !length.is_finite() || length <= f64::EPSILON {
        return Err("native deformer tangent is singular".into());
    }
    Ok([value[0] / length, value[1] / length])
}

fn rotation_offset(
    point: [f64; 2],
    angle: f64,
    scale: f64,
    reflect_x: bool,
    reflect_y: bool,
) -> [f64; 2] {
    let vector = [
        if reflect_x { -point[0] } else { point[0] },
        if reflect_y { -point[1] } else { point[1] },
    ];
    let (sin, cos) = angle.sin_cos();
    [
        (vector[0] * cos - vector[1] * sin) * scale,
        (vector[0] * sin + vector[1] * cos) * scale,
    ]
}

fn premultiply_png_texture(data: Vec<u8>) -> Result<Vec<u8>, String> {
    // PNG stores unassociated alpha; convert only the transient renderer texture.
    let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|error| format!("could not decode unpremultiplied native texture: {error}"))?
        .into_rgba8();
    for pixel in image.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|error| format!("could not encode native texture: {error}"))?;
    Ok(output.into_inner())
}

/// The engine mode of a native colour and alpha composition. `ADD` and `MULTIPLY` are the modes of before 5.3,
/// which do not read the alpha composition (the Blend mode page of the Cubism Editor manual,
/// <https://docs.live2d.com/en/cubism-editor-manual/blend-mode/>): `ADD` is a mode of `native_blend` that skips
/// it, and `MULTIPLY` is the Multiply of INX, which keeps the alpha of what lies below.
fn native_blend_mode(color: &str, alpha: &str) -> Result<&'static str, String> {
    match (color, alpha) {
        ("NORMAL", "OVER") => Ok("Normal"),
        ("MULTIPLY", _) => {
            // The alpha composition is not read, but it must be one there is.
            crate::native_blend::Mode::parse("NORMAL", alpha)?;
            Ok("Multiply")
        }
        ("SCREEN", "OVER") => Ok("Screen"),
        _ => {
            crate::native_blend::Mode::parse(color, alpha)?;
            Ok("Cubism53")
        }
    }
}

fn native_masks(
    clips: &[(String, bool)],
    ids: &HashMap<String, u32>,
) -> Result<Vec<Value>, String> {
    clips
        .iter()
        .map(|(guid, inverted)| {
            let id = ids
                .get(guid)
                .ok_or_else(|| format!("native clipping mask {guid:?} is missing"))?;
            Ok(json!({"source": id, "mode": if *inverted { "DodgeMask" } else { "Mask" }}))
        })
        .collect()
}

type NativeLayer = (f64, [f64; 4], [f64; 4], Vec<(String, bool)>, Option<String>);

fn native_layer(
    source: &Value,
    form: &Value,
    sources: &HashMap<String, &Value>,
    forms: &HashMap<String, &Value>,
    part_opacities: &BTreeMap<String, f64>,
    warnings: &mut Vec<String>,
) -> Result<NativeLayer, String> {
    let mut opacity = if source["data"]["type"] == "Part" {
        part_opacities
            .get(source["guid"].as_str().unwrap_or(""))
            .copied()
            .unwrap_or(number_field(form, "opacity", 1.0)?)
    } else {
        number_field(form, "opacity", 1.0)?
    };
    if source["data"]["type"] == "ArtMesh" {
        let mut target = optional_string(source, "targetDeformerGuid")?;
        let mut seen = HashSet::new();
        while let Some(guid) = target {
            if guid == crate::authoring_hierarchy::ROOT_DEFORMER_GUID {
                break;
            }
            if seen.len() >= MAX_DEFORMER_DEPTH || !seen.insert(guid.to_owned()) {
                return Err("native deformer hierarchy is cyclic or too deep".into());
            }
            let deformer = sources
                .get(guid)
                .ok_or("native target deformer does not resolve")?;
            let deformer_form = select_form(deformer, forms.get(guid).copied())?;
            match deformer["data"]["type"].as_str() {
                Some("Warp" | "Rotation") => {
                    opacity *= number_field(deformer_form, "opacity", 1.0)?;
                }
                other => return Err(format!("unsupported native deformer type {other:?}")),
            }
            target = optional_string(deformer, "targetDeformerGuid")?;
        }
        if !opacity.is_finite() {
            return Err("native layer opacity is not finite".into());
        }
    }
    let mut multiply = number_vec(form, "multiplyColor", [1.0; 4])?;
    let mut screen = number_vec(form, "screenColor", [0.0, 0.0, 0.0, 1.0])?;
    let mut clips = source["data"]["clipGuids"].as_array()
        .ok_or("native clipping mask list is missing")?.iter().filter_map(Value::as_str)
        .map(|guid| (guid.to_owned(), source["data"]["invertClippingMask"] == true)).collect::<Vec<_>>();
    let mut parent = optional_string(source, "parentPartGuid")?;
    let mut seen = HashSet::new();
    while let Some(guid) = parent {
        if !seen.insert(guid) || seen.len() > MAX_DEFORMER_DEPTH {
            return Err("native part hierarchy is cyclic or too deep".into());
        }
        let part = sources
            .get(guid)
            .ok_or("native source references a missing parent part")?;
        if part["data"]["type"] != "Part" {
            return Err("native source parent is not a part".into());
        }
        if part["data"]["useOffscreen"] == true {
            if !opacity.is_finite() {
                return Err("native layer opacity is not finite".into());
            }
            return Ok((opacity, multiply, screen, clips, Some(guid.to_owned())));
        }
        if let Some(value) = part_opacities.get(guid) { opacity *= value; }
        if let Some(part_form) = select_optional_form(part, forms.get(guid).copied())? {
            if !part_opacities.contains_key(guid) { opacity *= number_field(part_form, "opacity", 1.0)?; }
            let tint = number_vec(part_form, "multiplyColor", [1.0; 4])?;
            for channel in 0..4 {
                multiply[channel] *= tint[channel];
            }
            // Screening with the child's colour and then the Part's equals one screen with s + p - s * p.
            // ponytail: inferred, not measured against Cubism Core (#195).
            let part_screen = number_vec(part_form, "screenColor", [0.0, 0.0, 0.0, 1.0])?;
            for channel in 0..3 {
                screen[channel] += part_screen[channel] - screen[channel] * part_screen[channel];
            }
        }
        let data = &part["data"];
        if data["alphaComposition"] != "OVER" {
            return Err("native non-offscreen part uses unsupported alpha composition".into());
        }
        if data["colorComposition"] != "NORMAL" {
            warnings.push("native part color composition is not inherited by the preview".into());
        }
        clips.extend(data["clipGuids"].as_array().into_iter().flatten().filter_map(Value::as_str)
            .map(|guid| (guid.to_owned(), data["invertClippingMask"] == true)));
        parent = optional_string(part, "parentPartGuid")?;
    }
    if !opacity.is_finite() {
        return Err("native layer opacity is not finite".into());
    }
    Ok((opacity, multiply, screen, clips, None))
}

fn assemble_offscreens(
    offscreens: &[(String, Option<String>, Value)],
    scopes: &mut HashMap<String, Vec<Value>>,
    roots: &mut Vec<Value>,
    palette_ranks: Option<&HashMap<u64, usize>>,
) -> Result<(), String> {
    let guids = offscreens
        .iter()
        .map(|(guid, _, node)| (node["uuid"].as_u64().unwrap(), guid.as_str()))
        .collect::<HashMap<_, _>>();
    for (_, parent, node) in offscreens {
        if let Some(parent) = parent {
            scopes.entry(parent.clone()).or_default().push(node.clone());
        } else {
            roots.push(node.clone());
        }
    }
    fn expand(
        node: &mut Value,
        guids: &HashMap<u64, &str>,
        scopes: &mut HashMap<String, Vec<Value>>,
        palette_ranks: Option<&HashMap<u64, usize>>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_DEFORMER_DEPTH {
            return Err("native offscreen hierarchy is cyclic or too deep".into());
        }
        if let Some(guid) = guids.get(&node["uuid"].as_u64().unwrap_or(0)) {
            let mut children = scopes.remove(*guid).unwrap_or_default();
            sort_render_nodes(&mut children, palette_ranks)?;
            for child in &mut children {
                expand(child, guids, scopes, palette_ranks, depth + 1)?;
            }
            node["children"] = json!(children);
        }
        Ok(())
    }
    for root in roots {
        expand(root, &guids, scopes, palette_ranks, 0)?;
    }
    if !scopes.is_empty() {
        return Err("native offscreen hierarchy has unreachable scopes".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct NativeDrawOrderSegment {
    draw_order: f64,
    palette_rank: usize,
}

fn native_plain_group_orders(
    sources: &[Value],
    forms: &HashMap<String, &Value>,
) -> Result<HashMap<String, f64>, String> {
    sources
        .iter()
        .filter(|source| {
            source["data"]["type"] == "Part"
                && source["data"]["useOffscreen"] != true
                && source["data"]["drawOrderGroup"] == true
        })
        .map(|source| {
            let guid = string_field(source, "guid", "draw-order group")?;
            let evaluated = forms.get(guid).copied();
            let has_evaluated_form = match evaluated.and_then(|source| source.get("evaluatedForms")) {
                None | Some(Value::Null) => false,
                Some(Value::Array(forms)) => !forms.is_empty(),
                Some(_) => return Err("native draw-order group evaluatedForms is not an array".into()),
            };
            let has_source_forms = match source.get("forms") {
                None => false,
                Some(Value::Array(forms)) => !forms.is_empty(),
                Some(_) => return Err("native draw-order group forms is not an array".into()),
            };
            let form = if has_evaluated_form || has_source_forms {
                select_optional_form(source, evaluated)?
            } else {
                None
            };
            if form.is_some_and(|form| form["data"]["type"] != "Part") {
                return Err("native draw-order group has no evaluated Part form".into());
            }
            let draw_order = if let Some(value) = form.and_then(|form| form.get("drawOrder")) {
                if value.is_null() {
                    None
                } else {
                    Some(value.as_f64().filter(|order| order.is_finite()).ok_or_else(|| {
                        "native draw-order group form.drawOrder is not finite".to_owned()
                    })?)
                }
            } else {
                None
            };
            let draw_order = match draw_order {
                Some(draw_order) => draw_order,
                None => match source["data"].get("defaultDrawOrder") {
                    None => 500.0,
                    Some(value) => value
                        .as_f64()
                        .filter(|order| order.is_finite())
                        .ok_or_else(|| {
                            "native draw-order group data.defaultDrawOrder is not finite".to_owned()
                        })?,
                },
            };
            Ok((guid.to_owned(), draw_order))
        })
        .collect()
}

fn native_plain_group_ancestors(
    guid: &str,
    sources: &HashMap<String, &Value>,
    group_orders: &HashMap<String, f64>,
) -> Result<Vec<String>, String> {
    let source = sources
        .get(guid)
        .ok_or("native draw-order source is missing")?;
    let mut ancestors = Vec::new();
    let mut seen = HashSet::from([guid.to_owned()]);
    let mut parent = optional_string(source, "parentPartGuid")?;
    while let Some(parent_guid) = parent {
        if !seen.insert(parent_guid.to_owned()) || seen.len() > MAX_DEFORMER_DEPTH {
            return Err("native part hierarchy is cyclic or too deep".into());
        }
        let part = sources
            .get(parent_guid)
            .ok_or("native source references a missing parent part")?;
        if part["data"]["type"] != "Part" {
            return Err("native source parent is not a part".into());
        }
        if part["data"]["useOffscreen"] == true {
            break;
        }
        if group_orders.contains_key(parent_guid) {
            ancestors.push(parent_guid.to_owned());
        }
        parent = optional_string(part, "parentPartGuid")?;
    }
    ancestors.reverse();
    Ok(ancestors)
}

fn native_palette_rank(
    guid: &str,
    palette_ranks: Option<&HashMap<String, usize>>,
    ids: &HashMap<String, u32>,
) -> Result<usize, String> {
    if let Some(rank) = palette_ranks.and_then(|ranks| ranks.get(guid)) {
        return Ok(*rank);
    }
    ids.get(guid)
        .and_then(|id| usize::try_from(*id).ok())
        .ok_or_else(|| format!("native source {guid:?} has no draw-order tie rank"))
}

// Cubism Core reports the whole part of an interpolated draw order (hiyori_pro's 636.51 as 636), and that whole
// order with rank ties reproduces its render orders, so the interpolation noise of equal keyform orders must not
// break a tie either; the epsilon keeps a noisy 499.999999999 at 500.
fn whole_native_draw_order(order: f64) -> f64 {
    (order + 1e-6).floor()
}

fn native_draw_order_path(
    guid: &str,
    draw_order: f64,
    sources: &HashMap<String, &Value>,
    group_orders: &HashMap<String, f64>,
    palette_ranks: Option<&HashMap<String, usize>>,
    ids: &HashMap<String, u32>,
) -> Result<Vec<NativeDrawOrderSegment>, String> {
    let mut path = native_plain_group_ancestors(guid, sources, group_orders)?
        .into_iter()
        .map(|group_guid| {
            Ok(NativeDrawOrderSegment {
                draw_order: whole_native_draw_order(group_orders[&group_guid]),
                palette_rank: native_palette_rank(&group_guid, palette_ranks, ids)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    path.push(NativeDrawOrderSegment {
        draw_order: whole_native_draw_order(draw_order),
        palette_rank: native_palette_rank(guid, palette_ranks, ids)?,
    });
    Ok(path)
}

fn compare_native_draw_order_paths(
    left: &[NativeDrawOrderSegment],
    right: &[NativeDrawOrderSegment],
) -> std::cmp::Ordering {
    for (left, right) in left.iter().zip(right) {
        let order = left
            .draw_order
            .total_cmp(&right.draw_order)
            .then_with(|| right.palette_rank.cmp(&left.palette_rank));
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

fn sort_native_group_scopes(
    nodes: &mut Vec<Value>,
    guid_by_id: &HashMap<u64, &str>,
    sources: &HashMap<String, &Value>,
    group_orders: &HashMap<String, f64>,
    palette_ranks: Option<&HashMap<String, usize>>,
    ids: &HashMap<String, u32>,
) -> Result<(), String> {
    for node in nodes.iter_mut() {
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            sort_native_group_scopes(
                children,
                guid_by_id,
                sources,
                group_orders,
                palette_ranks,
                ids,
            )?;
        }
    }

    let mut ordered = nodes
        .drain(..)
        .map(|node| {
            let id = node["uuid"]
                .as_u64()
                .ok_or("native render node has no numeric UUID")?;
            let guid = guid_by_id
                .get(&id)
                .ok_or("native render node has no source GUID")?;
            let zsort = number_field(&node, "zsort", 0.0)?;
            let path = native_draw_order_path(
                guid,
                -zsort,
                sources,
                group_orders,
                palette_ranks,
                ids,
            )?;
            Ok((path, node))
        })
        .collect::<Result<Vec<_>, String>>()?;
    ordered.sort_by(|(left, _), (right, _)| compare_native_draw_order_paths(left, right));
    let count = ordered.len();
    *nodes = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (_, mut node))| {
            node["zsort"] = json!((count - index) as f64);
            node
        })
        .collect();
    Ok(())
}

fn native_palette_ranks(
    model: &Value,
    sources: &[Value],
) -> Result<(Option<HashMap<String, usize>>, bool), String> {
    let mut has_palette_order = false;
    for part in sources
        .iter()
        .filter(|source| source["data"]["type"] == "Part")
    {
        if let Some(children) = part["data"].get("childGuids") {
            let children = children
                .as_array()
                .ok_or("native Part child GUID list is not an array")?;
            has_palette_order |= !children.is_empty();
        }
    }
    if !has_palette_order {
        return Ok((None, false));
    }

    fn visit_part(
        guid: &str,
        sources: &HashMap<String, &Value>,
        ranks: &mut HashMap<String, usize>,
        memberships: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        has_unresolved_children: &mut bool,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_DEFORMER_DEPTH || !visiting.insert(guid.to_owned()) {
            return Err("native Part palette hierarchy is cyclic or too deep".into());
        }
        let part = sources
            .get(guid)
            .ok_or("native palette references a missing Part")?;
        if part["data"]["type"] != "Part" {
            return Err("native palette parent is not a Part".into());
        }
        let children = part["data"]["childGuids"]
            .as_array()
            .ok_or("native Part child GUID list is missing")?;
        for child in children {
            let child = child
                .as_str()
                .filter(|child| !child.is_empty())
                .ok_or("native Part child GUID is not a non-empty string")?;
            let Some(source) = sources.get(child) else {
                *has_unresolved_children = true;
                continue;
            };
            if visiting.contains(child) {
                return Err("native Part palette hierarchy is cyclic".into());
            }
            if !memberships.insert(child.to_owned()) {
                return Err("native Part palette contains duplicate child membership".into());
            }
            if optional_string(source, "parentPartGuid")? != Some(guid) {
                return Err(format!(
                    "native palette child {child:?} does not name its Part parent {guid:?}"
                ));
            }
            ranks.insert(child.to_owned(), ranks.len());
            if source["data"]["type"] == "Part" {
                visit_part(
                    child,
                    sources,
                    ranks,
                    memberships,
                    visiting,
                    has_unresolved_children,
                    depth + 1,
                )?;
            }
        }
        visiting.remove(guid);
        Ok(())
    }

    let source_by_guid = sources
        .iter()
        .map(|source| Ok((string_field(source, "guid", "source")?.to_owned(), source)))
        .collect::<Result<HashMap<_, _>, String>>()?;
    let root_guid = string_field(model, "rootPartGuid", "model")?;
    let root = source_by_guid
        .get(root_guid)
        .ok_or("native palette root Part is missing")?;
    if root["data"]["type"] != "Part" {
        return Err("native palette root source is not a Part".into());
    }

    let mut ranks = HashMap::from([(root_guid.to_owned(), 0)]);
    let mut memberships = HashSet::new();
    let mut visiting = HashSet::new();
    let mut has_unresolved_children = false;
    visit_part(
        root_guid,
        &source_by_guid,
        &mut ranks,
        &mut memberships,
        &mut visiting,
        &mut has_unresolved_children,
        0,
    )?;
    for source in sources {
        let guid = string_field(source, "guid", "source")?;
        if guid != root_guid && !ranks.contains_key(guid) {
            return Err(format!(
                "native palette does not rank known source {guid:?}"
            ));
        }
    }
    Ok((Some(ranks), has_unresolved_children))
}

fn sort_render_nodes(
    nodes: &mut [Value],
    palette_ranks: Option<&HashMap<u64, usize>>,
) -> Result<(), String> {
    if let Some(palette_ranks) = palette_ranks {
        for node in nodes.iter() {
            let id = node["uuid"]
                .as_u64()
                .ok_or("native render node has no numeric UUID")?;
            if !palette_ranks.contains_key(&id) {
                return Err(format!(
                    "native palette rank is missing for render node {id}"
                ));
            }
        }
        nodes.sort_by_key(|node| {
            let id = node["uuid"].as_u64().unwrap();
            std::cmp::Reverse(palette_ranks[&id])
        });
    } else {
        nodes.sort_by_key(|node| std::cmp::Reverse(node["uuid"].as_u64().unwrap_or(0)));
    }
    Ok(())
}

fn number_array(value: &Value, name: &str) -> Result<Vec<f64>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{name} contains a non-finite number"))
        })
        .collect()
}

fn number_vec<const N: usize>(
    value: &Value,
    name: &str,
    fallback: [f64; N],
) -> Result<[f64; N], String> {
    let Some(values) = value.get(name).and_then(Value::as_array) else {
        return Ok(fallback);
    };
    if values.len() != N {
        return Err(format!("{name} must have {N} entries"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{name} contains a non-finite number"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| format!("{name} has an invalid length"))
}

fn number_field(value: &Value, name: &str, fallback: f64) -> Result<f64, String> {
    match value.get(name).and_then(Value::as_f64) {
        Some(number) if number.is_finite() => Ok(number),
        Some(_) => Err(format!("native field {name} is not finite")),
        None => Ok(fallback),
    }
}

fn optional_string<'a>(value: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(format!("native field {name} is not a string")),
    }
}

fn string_field<'a>(value: &'a Value, name: &str, owner: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("native {owner} field {name} is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_scene() -> NativeScene {
        let parsed = authoring_scene::tests::native_preview_scene();
        let source = parsed.source_value().unwrap();
        let texture_guid = source["textures"][0]["guid"].as_str().unwrap().to_owned();
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([255, 255, 255, 255]),
        ))
        .write_to(&mut output, ImageFormat::Png)
        .unwrap();
        NativeScene {
            parsed,
            source,
            textures: vec![TextureBlob {
                tag: 0,
                data: output.into_inner(),
            }],
            texture_slots: HashMap::from([(texture_guid, 0)]),
        }
    }

    #[test]
    fn a_mipmap_min_filter_marks_the_part_for_mipmapped_sampling() {
        let flags = |filter: &str| {
            let mut scene = preview_scene();
            scene.source["textures"][0]["minFilter"] = json!(filter);
            let model = scene.render_model(&BTreeMap::new()).unwrap().0;
            let mut found = Vec::new();
            let mut stack = vec![&model["nodes"]];
            while let Some(node) = stack.pop() {
                if node["type"] == "Part" {
                    found.push(node.get("textureMipmaps").cloned());
                }
                stack.extend(node["children"].as_array().into_iter().flatten());
            }
            found
        };
        assert_eq!(flags("LINEAR"), [None]);
        assert_eq!(flags("LINEAR_MIPMAP_LINEAR"), [Some(json!(true))]);
    }

    #[test]
    fn glue_without_a_drawn_pair_warns_without_changing_rendered_geometry() {
        let mut scene = preview_scene();
        let baseline = scene.render_model(&BTreeMap::new()).unwrap();
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        let mesh_guid = scene.source["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["data"]["type"] == "ArtMesh")
            .unwrap()["guid"]
            .as_str()
            .unwrap()
            .to_owned();
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"glue-guid", "id":"Glue", "name":"Glue",
            "parentPartGuid":root_guid, "targetDeformerGuid":null,
            "visible":true,"locked":false,"forms":[{"data":{"type":"Glue","intensity":1.0}}],
            "data":{"type":"Glue","targetArtMeshAGuid":mesh_guid,
                "targetArtMeshBGuid":"other-mesh","bindVertexUids":[101,202],"weights":[0.5,0.5]}
        }));
        set_part_children(&mut scene, &root_guid, &[&mesh_guid, "glue-guid"]);

        let (rendered, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        assert_eq!(rendered, baseline.0);
        assert!(warnings.iter().any(|warning| warning == GLUE_SKIPPED));
        assert!(!warnings
            .iter()
            .any(|warning| warning.contains("unsupported child sources")));
    }

    #[test]
    fn a_visible_art_path_warns_and_leaves_the_drawn_meshes_as_they_are() {
        let mut scene = preview_scene();
        let baseline = scene.render_model(&BTreeMap::new()).unwrap();
        assert!(!baseline.1.iter().any(|warning| warning == ART_PATH_NOT_DRAWN));
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        let mesh_guid = scene.source["sources"].as_array().unwrap().iter()
            .find(|source| source["data"]["type"] == "ArtMesh").unwrap()["guid"].as_str().unwrap().to_owned();
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"path-guid", "id":"Path", "name":"Path", "parentPartGuid":root_guid, "targetDeformerGuid":null,
            "visible":true, "locked":false, "forms":[{"data":{"type":"ArtPath","points":[]}}],
            "data":{"type":"ArtPath","closed":false,"divisionCount":20,"clipGuids":[]}
        }));
        set_part_children(&mut scene, &root_guid, &["path-guid", &mesh_guid]);

        let (rendered, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        assert_eq!(rendered, baseline.0);
        assert_eq!(warnings.iter().filter(|warning| *warning == ART_PATH_NOT_DRAWN).count(), 1);
        assert!(!warnings.iter().any(|warning| warning.contains("unsupported child sources")));

        scene.source["sources"].as_array_mut().unwrap().last_mut().unwrap()["visible"] = json!(false);
        let (hidden, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        assert_eq!(hidden, baseline.0);
        assert!(!warnings.iter().any(|warning| warning == ART_PATH_NOT_DRAWN));
    }

    fn glued_scene(weights: Value, forms: Value) -> NativeScene {
        let mut scene = grouped_preview_scene();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        for source in sources.iter_mut() {
            if source["guid"] == "red-mesh" || source["guid"] == "blue-mesh" {
                source["data"]["vertexUids"] = json!([10, 11, 12]);
            }
            if source["guid"] == "blue-mesh" {
                source["forms"][0]["data"]["positions"] = json!([16.0, 12.0, 40.0, 24.0, 8.0, 40.0]);
            }
        }
        let root_guid = scene.source["rootPartGuid"].clone();
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"glue-guid", "id":"Glue", "name":"Glue",
            "parentPartGuid":root_guid, "targetDeformerGuid":null,
            "visible":true,"locked":false,"forms":forms,
            "data":{"type":"Glue","targetArtMeshAGuid":"red-mesh","targetArtMeshBGuid":"blue-mesh",
                "bindVertexUids":[10,10,11,11],"weights":weights}
        }));
        scene
    }

    fn rendered_vertices(scene: &NativeScene) -> (Vec<Value>, Vec<String>) {
        let (model, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        let mut nodes = Vec::new();
        let mut pending = vec![&model["nodes"]];
        while let Some(node) = pending.pop() {
            if node["type"] == "Part" {
                nodes.push((node["uuid"].as_u64().unwrap(), node["mesh"]["verts"].clone()));
            }
            pending.extend(node["children"].as_array().into_iter().flatten());
        }
        nodes.sort_by_key(|(id, _)| *id);
        (nodes.into_iter().map(|(_, vertices)| vertices).collect(), warnings)
    }

    #[test]
    fn glue_pulls_bound_vertices_together_by_weight_and_intensity() {
        let intensity = |value: f64| json!([{"data":{"type":"Glue","intensity":value}}]);
        let (unglued, warnings) = rendered_vertices(&glued_scene(json!([0.5, 0.5, 0.0, 1.0]), json!([])));
        assert_eq!(unglued, [json!([8.0, 8.0, 40.0, 8.0, 8.0, 40.0]), json!([16.0, 12.0, 40.0, 24.0, 8.0, 40.0])]);
        assert!(warnings.iter().any(|warning| warning == GLUE_SKIPPED));

        // Equal weights meet half way; weights 0 and 1 leave the first vertex and bring the second one to it.
        let (glued, warnings) = rendered_vertices(&glued_scene(json!([0.5, 0.5, 0.0, 1.0]), intensity(1.0)));
        assert_eq!(glued, [json!([12.0, 10.0, 40.0, 8.0, 8.0, 40.0]), json!([12.0, 10.0, 40.0, 8.0, 8.0, 40.0])]);
        assert!(!warnings.iter().any(|warning| warning == GLUE_SKIPPED));

        let (half, _) = rendered_vertices(&glued_scene(json!([0.5, 0.5, 0.0, 1.0]), intensity(0.5)));
        assert_eq!(half, [json!([10.0, 9.0, 40.0, 8.0, 8.0, 40.0]), json!([14.0, 11.0, 40.0, 16.0, 8.0, 40.0])]);

        // A pair that binds a vertex the mesh no longer has is left out; the other pair still applies.
        let mut stale = glued_scene(json!([0.5, 0.5, 0.0, 1.0]), intensity(1.0));
        stale.source["sources"].as_array_mut().unwrap().last_mut().unwrap()["data"]["bindVertexUids"] =
            json!([10, 10, 11, 99]);
        let (vertices, warnings) = rendered_vertices(&stale);
        assert_eq!(vertices, [json!([12.0, 10.0, 40.0, 8.0, 8.0, 40.0]), json!([12.0, 10.0, 40.0, 24.0, 8.0, 40.0])]);
        assert!(warnings.iter().any(|warning| warning == GLUE_STALE_PAIR));
    }

    #[test]
    fn glues_sharing_a_vertex_run_from_the_last_source_to_the_first() {
        let form = json!({"evaluatedForms":[{"data":{"type":"Glue","intensity":1.0}}]});
        let mesh = json!({"data":{"type":"ArtMesh","vertexUids":[0]}});
        let glue = |guid: &str, a: &str, b: &str, weights: [f64; 2]| json!({
            "guid":guid,"forms":[],
            "data":{"type":"Glue","targetArtMeshAGuid":a,"targetArtMeshBGuid":b,"bindVertexUids":[0,0],"weights":weights}
        });
        let sources = [glue("first", "a", "b", [0.5, 0.5]), glue("second", "a", "c", [0.0, 1.0])];
        let source_by_guid =
            HashMap::from([("a".to_owned(), &mesh), ("b".to_owned(), &mesh), ("c".to_owned(), &mesh)]);
        let forms = HashMap::from([("first".to_owned(), &form), ("second".to_owned(), &form)]);
        let mut vertices = HashMap::from([
            ("a".to_owned(), vec![0.0, 0.0]),
            ("b".to_owned(), vec![10.0, 0.0]),
            ("c".to_owned(), vec![0.0, 6.0]),
        ]);
        apply_glue(&sources, &source_by_guid, &forms, &mut vertices, &mut Vec::new()).unwrap();
        // The second Glue brings c to where a was before the first one moved a.
        assert_eq!(vertices["a"], [5.0, 0.0]);
        assert_eq!(vertices["b"], [5.0, 0.0]);
        assert_eq!(vertices["c"], [0.0, 0.0]);
    }

    #[test]
    fn face_culling_renders_with_a_warning() {
        let baseline = preview_scene().render_model(&BTreeMap::new()).unwrap();
        let mut scene = preview_scene();
        let mesh = scene.source["sources"].as_array_mut().unwrap().iter_mut()
            .find(|source| source["data"]["type"] == "ArtMesh").unwrap();
        mesh["data"]["culling"] = json!(true);
        let (rendered, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        assert_eq!(rendered, baseline.0);
        assert!(warnings.iter().any(|warning| warning == "native face culling is unsupported; back faces are drawn"));
        assert!(!baseline.1.iter().any(|warning| warning.contains("culling")));
        let mesh = scene.source["sources"].as_array_mut().unwrap().iter_mut()
            .find(|source| source["data"]["type"] == "ArtMesh").unwrap();
        mesh["visible"] = json!(false);
        let (_, hidden) = scene.render_model(&BTreeMap::new()).unwrap();
        assert!(!hidden.iter().any(|warning| warning.contains("culling")), "a mesh that is not drawn does not warn");
    }

    fn set_part_children(scene: &mut NativeScene, part_guid: &str, children: &[&str]) {
        let part = scene.source["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["guid"] == part_guid)
            .unwrap();
        part["data"]["childGuids"] = json!(children);
    }

    fn grouped_preview_scene() -> NativeScene {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        for source in scene.source["sources"].as_array_mut().unwrap() {
            if source["guid"] == "mesh-guid" {
                source["guid"] = json!("red-mesh");
            } else if source["guid"] == "back-mesh-guid" {
                source["guid"] = json!("blue-mesh");
            }
            if source["guid"] == "red-mesh" || source["guid"] == "blue-mesh" {
                let base_local_form = source["forms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|form| {
                        form["guid"] == "form-guid"
                            && form["coordinateType"] == "DeformerLocal"
                    })
                    .cloned()
                    .expect("overlap fixture has a base DeformerLocal form");
                source["forms"] = json!([base_local_form]);
            }
        }
        scene
    }

    fn set_source_parent(scene: &mut NativeScene, guid: &str, parent: Option<&str>) {
        let source = scene.source["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["guid"] == guid)
            .unwrap();
        source["parentPartGuid"] = parent.map_or(Value::Null, |parent| json!(parent));
    }

    fn set_source_draw_order(scene: &mut NativeScene, guid: &str, draw_order: impl Into<Value>) {
        let source = scene.source["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["guid"] == guid)
            .unwrap();
        let forms = source["forms"].as_array_mut().unwrap();
        let index = forms
            .iter_mut()
            .position(|form| form["coordinateType"] == "DeformerLocal")
            .unwrap_or(0);
        forms[index]["drawOrder"] = draw_order.into();
    }

    fn part_source(
        guid: &str,
        parent: Option<&str>,
        children: &[&str],
        draw_order: Option<i64>,
        draw_order_group: bool,
        use_offscreen: bool,
        opacity: f64,
    ) -> Value {
        let mut form = json!({"data":{"type":"Part"},"opacity":opacity});
        if let Some(draw_order) = draw_order {
            form["drawOrder"] = json!(draw_order);
        }
        json!({
            "guid":guid,"name":guid,"visible":true,"parentPartGuid":parent,
            "data":{"type":"Part","childGuids":children,"drawOrderGroup":draw_order_group,
                "defaultDrawOrder":500,"useOffscreen":use_offscreen,
                "colorComposition":"NORMAL","alphaComposition":"OVER","clipGuids":[]},
            "forms":[form]
        })
    }

    fn set_root_part_children(scene: &mut NativeScene, children: &[&str]) -> String {
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        set_part_children(scene, &root_guid, children);
        root_guid
    }

    fn source_id(scene: &NativeScene, guid: &str) -> u64 {
        scene.source["sources"]
            .as_array()
            .unwrap()
            .iter()
            .position(|source| source["guid"] == guid)
            .map(|index| u64::try_from(index + 2).unwrap())
            .unwrap()
    }

    /// The pixel where an opaque red mesh in `color` and `alpha` lies over a blue mesh of half opacity, and the
    /// engine mode the red mesh got.
    fn blended_over_half_blue(color: &str, alpha: &str) -> Result<(String, [u8; 4]), String> {
        let mut scene = grouped_preview_scene();
        set_source_draw_order(&mut scene, "blue-mesh", 400);
        set_source_draw_order(&mut scene, "red-mesh", 600);
        for source in scene.source["sources"].as_array_mut().unwrap() {
            if source["guid"] == "red-mesh" {
                source["data"]["colorComposition"] = json!(color);
                source["data"]["alphaComposition"] = json!(alpha);
                source["forms"][0]["opacity"] = json!(1.0);
            } else if source["guid"] == "blue-mesh" {
                source["forms"][0]["opacity"] = json!(0.5);
            }
        }
        let model = scene.render_model(&BTreeMap::new())?.0;
        let id = source_id(&scene, "red-mesh");
        let red = model["nodes"]["children"].as_array().unwrap().iter().find(|node| node["uuid"] == id);
        let mode = red.expect("the red mesh is a node")["blend_mode"].as_str().unwrap().to_owned();
        Ok((mode, rendered_pixel(&scene, &model)))
    }

    #[test]
    fn the_modes_of_before_5_3_draw_the_same_under_every_alpha_composition() {
        const ALPHAS: [&str; 5] = ["OVER", "ATOP", "OUT", "CONJOINT", "DISJOINT"];
        // Red (1, 0, 0) of alpha 1 on blue (0, 0, 1) of alpha 0.5. Add sums the premultiplied colours and Multiply
        // leaves black, and both keep the alpha of the blue: (1, 0, 0.5) and (0, 0, 0) of alpha 0.5.
        let cases = [("ADD", "Cubism53", [255, 0, 255, 128]), ("MULTIPLY", "Multiply", [0, 0, 0, 128])];
        for (color, mode, pixel) in cases {
            for alpha in ALPHAS {
                assert_eq!(blended_over_half_blue(color, alpha).unwrap(), (mode.to_owned(), pixel), "{color} {alpha}");
            }
        }
        // The Multiply of 5.3 reads it: Over shows the red where the blue does not cover, Atop does not.
        let pixels = ALPHAS.map(|alpha| blended_over_half_blue("MULTIPLY_R2", alpha).unwrap().1);
        assert_eq!(pixels[..2], [[128, 0, 0, 255], [0, 0, 0, 128]]);
        // A composition that is no token is still refused, for a mode of before 5.3 too.
        for (color, alpha) in [("MULTIPLY", "UNDER"), ("ADD", "UNDER"), ("MULTIPLY", "")] {
            let refusal = blended_over_half_blue(color, alpha).unwrap_err();
            assert_eq!(refusal, format!("unsupported native alpha composition {alpha:?}"), "{color}");
        }
    }

    fn rendered_pixel(scene: &NativeScene, model: &Value) -> [u8; 4] {
        let geometry = crate::scene_deform::evaluate(model, &BTreeMap::new()).unwrap();
        let png = crate::render::render_png_for_scene(
            model,
            &geometry,
            scene.textures(),
            32,
            32,
            [0.0, 0.0, 50.0, 50.0],
        )
        .unwrap();
        image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .into_rgba8()
            .get_pixel(10, 10)
            .0
    }

    #[test]
    fn plain_group_orders_its_subtree_and_preserves_masks_and_source_ids() {
        let mut scene = grouped_preview_scene();
        let root = set_root_part_children(
            &mut scene,
            &["unsupported-affecter", "red-mesh", "group"],
        );
        set_source_parent(&mut scene, "red-mesh", Some(&root));
        set_source_parent(&mut scene, "blue-mesh", Some("group"));
        set_source_draw_order(&mut scene, "red-mesh", 300);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        scene.source["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["guid"] == "red-mesh")
            .unwrap()["data"]["clipGuids"] = json!(["blue-mesh"]);
        scene.source["sources"]
            .as_array_mut()
            .unwrap()
            .push(part_source("group", Some(&root), &["blue-mesh"], Some(500), true, false, 1.0));
        let source_before = scene.source.clone();
        let (model, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        let children = model["nodes"]["children"].as_array().unwrap();
        let red_id = source_id(&scene, "red-mesh");
        let blue_id = source_id(&scene, "blue-mesh");
        assert_eq!(
            children
                .iter()
                .map(|node| node["uuid"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![red_id, blue_id]
        );
        assert_eq!(children[0]["masks"], json!([{"source":blue_id,"mode":"Mask"}]));
        assert_eq!(children[0]["zsort"], json!(2.0));
        assert_eq!(children[1]["zsort"], json!(1.0));
        assert!(children.iter().all(|node| node["type"] == "Part"));
        assert!(!warnings
            .iter()
            .any(|warning| warning.contains("not reproduced")));
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);
        assert_eq!(scene.source, source_before);
    }

    #[test]
    fn transparent_parts_add_no_segment_and_disabling_grouping_restores_global_order() {
        let mut scene = grouped_preview_scene();
        let root = set_root_part_children(
            &mut scene,
            &[
                "unsupported-affecter",
                "red-mesh",
                "transparent",
                "trigger-group",
            ],
        );
        set_source_parent(&mut scene, "red-mesh", Some(&root));
        set_source_parent(&mut scene, "blue-mesh", Some("transparent"));
        set_source_draw_order(&mut scene, "red-mesh", 300);
        set_source_draw_order(&mut scene, "blue-mesh", 900);
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source(
            "transparent",
            Some(&root),
            &["blue-mesh"],
            Some(100),
            false,
            false,
            1.0,
        ));
        sources.push(part_source(
            "trigger-group",
            Some(&root),
            &[],
            Some(700),
            true,
            false,
            1.0,
        ));
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);

        let mut scene = grouped_preview_scene();
        let root = set_root_part_children(
            &mut scene,
            &[
                "unsupported-affecter",
                "red-mesh",
                "disabled-group",
                "trigger-group",
            ],
        );
        set_source_parent(&mut scene, "red-mesh", Some(&root));
        set_source_parent(&mut scene, "blue-mesh", Some("disabled-group"));
        set_source_draw_order(&mut scene, "red-mesh", 300);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source(
            "disabled-group",
            Some(&root),
            &["blue-mesh"],
            Some(500),
            false,
            false,
            1.0,
        ));
        sources.push(part_source(
            "trigger-group",
            Some(&root),
            &[],
            Some(700),
            true,
            false,
            1.0,
        ));
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn nested_plain_groups_keep_local_order_and_palette_ties() {
        let mut scene = grouped_preview_scene();
        let root = set_root_part_children(&mut scene, &["unsupported-affecter", "outer"]);
        set_source_parent(&mut scene, "red-mesh", Some("inner"));
        set_source_parent(&mut scene, "blue-mesh", Some("inner"));
        set_source_draw_order(&mut scene, "red-mesh", 100);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source(
            "outer",
            Some(&root),
            &["inner"],
            Some(600),
            true,
            false,
            1.0,
        ));
        sources.push(part_source(
            "inner",
            Some("outer"),
            &["blue-mesh", "red-mesh"],
            Some(250),
            true,
            false,
            1.0,
        ));
        let (model, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        let children = model["nodes"]["children"].as_array().unwrap();
        assert_eq!(children[0]["uuid"], source_id(&scene, "red-mesh"));
        assert_eq!(children[1]["uuid"], source_id(&scene, "blue-mesh"));
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("nested non-offscreen draw-order groups")));

        set_source_draw_order(&mut scene, "red-mesh", 900);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn plain_group_keeps_translucent_overlap_in_the_existing_scope() {
        let mut scene = grouped_preview_scene();
        let root = set_root_part_children(&mut scene, &["unsupported-affecter", "group"]);
        set_source_parent(&mut scene, "red-mesh", Some("group"));
        set_source_parent(&mut scene, "blue-mesh", Some("group"));
        set_source_draw_order(&mut scene, "red-mesh", 500);
        set_source_draw_order(&mut scene, "blue-mesh", 500);
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source(
            "group",
            Some(&root),
            &["blue-mesh", "red-mesh"],
            Some(500),
            true,
            false,
            0.5,
        ));
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let children = model["nodes"]["children"].as_array().unwrap();
        assert!(children.iter().all(|node| node["type"] == "Part"));
        assert!(children.iter().all(|node| node["opacity"] == 0.375));
        assert_eq!(rendered_pixel(&scene, &model), [98, 0, 157, 155]);
    }

    #[test]
    fn an_offscreen_part_inside_an_offscreen_part_counts_as_two_levels() {
        let mut scene = grouped_preview_scene();
        let root = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source("outer", Some(&root), &["inner", "red-mesh"], Some(600), false, true, 1.0));
        sources.push(part_source("inner", Some("outer"), &["blue-mesh"], Some(200), false, true, 1.0));
        set_source_parent(&mut scene, "red-mesh", Some("outer"));
        set_source_parent(&mut scene, "blue-mesh", Some("inner"));
        set_part_children(&mut scene, &root, &["unsupported-affecter", "outer"]);
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let (_, _, offscreen_depth) = crate::render::packet_scene_limits(&model["nodes"]).unwrap();
        assert_eq!(offscreen_depth, 2);
    }

    #[test]
    fn plain_group_orders_offscreen_composite_separately_from_its_children() {
        let mut scene = grouped_preview_scene();
        let mut outside = scene.source["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["guid"] == "red-mesh")
            .unwrap()
            .clone();
        outside["guid"] = json!("outside-red");
        outside["name"] = json!("outside-red");
        outside["parentPartGuid"] = scene.source["rootPartGuid"].clone();
        for form in outside["forms"].as_array_mut().unwrap() {
            form["opacity"] = json!(0.0);
        }
        let root = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(outside);
        sources.push(part_source(
            "outer",
            Some(&root),
            &["offscreen"],
            Some(600),
            true,
            false,
            1.0,
        ));
        sources.push(part_source(
            "offscreen",
            Some("outer"),
            &["blue-mesh", "red-mesh"],
            Some(200),
            false,
            true,
            1.0,
        ));
        set_source_parent(&mut scene, "red-mesh", Some("offscreen"));
        set_source_parent(&mut scene, "blue-mesh", Some("offscreen"));
        set_source_draw_order(&mut scene, "red-mesh", 300);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        set_part_children(
            &mut scene,
            &root,
            &["unsupported-affecter", "outside-red", "outer"],
        );
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let root_children = model["nodes"]["children"].as_array().unwrap();
        assert_eq!(root_children[0]["uuid"], source_id(&scene, "outside-red"));
        assert_eq!(root_children[1]["type"], "Composite");
        assert_eq!(root_children[1]["zsort"], json!(1.0));
        let scope_children = root_children[1]["children"].as_array().unwrap();
        assert_eq!(scope_children[0]["uuid"], source_id(&scene, "blue-mesh"));
        assert_eq!(scope_children[1]["uuid"], source_id(&scene, "red-mesh"));
        assert_eq!(scope_children[0]["zsort"], json!(2.0));
        assert_eq!(scope_children[1]["zsort"], json!(1.0));
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn plain_group_hierarchy_stops_at_the_offscreen_boundary() {
        let mut scene = grouped_preview_scene();
        let mut outside = scene.source["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["guid"] == "red-mesh")
            .unwrap()
            .clone();
        outside["guid"] = json!("outside-red");
        outside["name"] = json!("outside-red");
        for form in outside["forms"].as_array_mut().unwrap() {
            form["opacity"] = json!(0.0);
        }
        let root = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        scene.source["sources"].as_array_mut().unwrap().push(outside);
        set_source_parent(&mut scene, "outside-red", Some(&root));
        set_source_parent(&mut scene, "red-mesh", Some("offscreen"));
        set_source_parent(&mut scene, "blue-mesh", Some("inner"));
        set_source_draw_order(&mut scene, "red-mesh", 300);
        set_source_draw_order(&mut scene, "blue-mesh", 100);
        let sources = scene.source["sources"].as_array_mut().unwrap();
        sources.push(part_source(
            "outer",
            Some(&root),
            &["offscreen"],
            Some(600),
            true,
            false,
            1.0,
        ));
        sources.push(part_source(
            "offscreen",
            Some("outer"),
            &["inner", "red-mesh"],
            Some(200),
            false,
            true,
            1.0,
        ));
        sources.push(part_source(
            "inner",
            Some("offscreen"),
            &["blue-mesh"],
            Some(200),
            true,
            false,
            1.0,
        ));
        set_part_children(
            &mut scene,
            &root,
            &["unsupported-affecter", "outside-red", "outer"],
        );
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let root_children = model["nodes"]["children"].as_array().unwrap();
        assert_eq!(root_children[0]["uuid"], source_id(&scene, "outside-red"));
        assert_eq!(root_children[1]["type"], "Composite");
        let scope_children = root_children[1]["children"].as_array().unwrap();
        assert_eq!(scope_children[0]["uuid"], source_id(&scene, "blue-mesh"));
        assert_eq!(scope_children[1]["uuid"], source_id(&scene, "red-mesh"));
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn group_order_uses_evaluated_form_or_validated_source_default() {
        let source = json!({
            "guid":"group","data":{"type":"Part","useOffscreen":false,
                "drawOrderGroup":true,"defaultDrawOrder":450},"forms":[]
        });
        let sources = [source.clone()];
        let evaluated = json!({"evaluatedForms":[{"data":{"type":"Part"},"drawOrder":700}]});
        let forms = HashMap::from([("group".to_owned(), &evaluated)]);
        assert_eq!(native_plain_group_orders(&sources, &forms).unwrap()["group"], 700.0);
        let reevaluated = json!({"evaluatedForms":[{"data":{"type":"Part"},"drawOrder":300}]});
        let forms = HashMap::from([("group".to_owned(), &reevaluated)]);
        assert_eq!(native_plain_group_orders(&sources, &forms).unwrap()["group"], 300.0);
        let empty = HashMap::new();
        assert_eq!(native_plain_group_orders(&sources, &empty).unwrap()["group"], 450.0);

        let defaulted = json!({
            "guid":"group","data":{"type":"Part","useOffscreen":false,
                "drawOrderGroup":true},"forms":[]
        });
        assert_eq!(native_plain_group_orders(&[defaulted], &empty).unwrap()["group"], 500.0);

        let invalid_form = json!({"evaluatedForms":[{
            "data":{"type":"Part"},"drawOrder":"not-a-number"
        }]});
        let invalid_forms = HashMap::from([("group".to_owned(), &invalid_form)]);
        assert!(native_plain_group_orders(&sources, &invalid_forms)
            .unwrap_err()
            .contains("form.drawOrder"));
        let invalid_default = json!({
            "guid":"group","data":{"type":"Part","useOffscreen":false,
                "drawOrderGroup":true,"defaultDrawOrder":"not-a-number"},"forms":[]
        });
        assert!(native_plain_group_orders(&[invalid_default], &empty)
            .unwrap_err()
            .contains("data.defaultDrawOrder"));

        let a = part_source("a", Some("b"), &[], Some(500), true, false, 1.0);
        let b = part_source("b", Some("a"), &[], Some(500), true, false, 1.0);
        let hierarchy = HashMap::from([("a".to_owned(), &a), ("b".to_owned(), &b)]);
        let orders = HashMap::from([("a".to_owned(), 500.0), ("b".to_owned(), 500.0)]);
        assert!(native_plain_group_ancestors("a", &hierarchy, &orders)
            .unwrap_err()
            .contains("cyclic"));

        let missing = part_source("a", Some("missing"), &[], Some(500), true, false, 1.0);
        let hierarchy = HashMap::from([("a".to_owned(), &missing)]);
        assert!(native_plain_group_ancestors("a", &hierarchy, &orders)
            .unwrap_err()
            .contains("missing parent"));
    }

    #[test]
    fn art_mesh_opacity_inherits_warp_and_rotation_forms_once() {
        let mesh = json!({
            "guid":"mesh", "parentPartGuid":"part", "targetDeformerGuid":"rotation",
            "forms":[{"data":{"type":"ArtMesh"},"opacity":0.1}],
            "data":{"type":"ArtMesh","clipGuids":[]}
        });
        let rotation = json!({
            "guid":"rotation", "targetDeformerGuid":"warp",
            "forms":[{"data":{"type":"Rotation"},"opacity":0.25}],
            "data":{"type":"Rotation"}
        });
        let warp = json!({
            "guid":"warp", "targetDeformerGuid":crate::authoring_hierarchy::ROOT_DEFORMER_GUID,
            "forms":[{"data":{"type":"Warp"},"opacity":0.5}],
            "data":{"type":"Warp"}
        });
        let part = json!({
            "guid":"part", "visible":true,
            "forms":[{"data":{"type":"Part"},"opacity":0.5}],
            "data":{"type":"Part","useOffscreen":false,"alphaComposition":"OVER",
                "colorComposition":"NORMAL","clipGuids":[]}
        });
        let evaluated_rotation = json!({"evaluatedForms":[{
            "data":{"type":"Rotation"},"opacity":0.5
        }]});
        let sources = HashMap::from([
            ("mesh".to_owned(), &mesh),
            ("rotation".to_owned(), &rotation),
            ("warp".to_owned(), &warp),
            ("part".to_owned(), &part),
        ]);
        let forms = HashMap::from([("rotation".to_owned(), &evaluated_rotation)]);
        let layer = native_layer(
            &mesh,
            &json!({"opacity":0.75}),
            &sources,
            &forms,
            &BTreeMap::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert!((layer.0 - 0.09375).abs() < 1e-12);
    }

    #[test]
    fn part_colours_are_inherited_multiply_by_product_and_screen_by_screen() {
        let mesh = json!({"guid":"mesh", "parentPartGuid":"part", "data":{"type":"ArtMesh","clipGuids":[]}});
        let part = json!({
            "guid":"part", "forms":[{"data":{"type":"Part"},"multiplyColor":[0.5,1.0,1.0,1.0],"screenColor":[0.5,0.0,0.25,1.0]}],
            "data":{"type":"Part","useOffscreen":false,"alphaComposition":"OVER","colorComposition":"NORMAL","clipGuids":[]}
        });
        let sources = HashMap::from([("mesh".to_owned(), &mesh), ("part".to_owned(), &part)]);
        let mut warnings = Vec::new();
        let form = json!({"multiplyColor":[0.5,0.5,1.0,1.0],"screenColor":[0.5,0.5,0.0,1.0]});
        let layer = native_layer(&mesh, &form, &sources, &HashMap::new(), &BTreeMap::new(), &mut warnings).unwrap();
        assert_eq!(layer.1, [0.25, 0.5, 1.0, 1.0]);
        assert_eq!(layer.2, [0.75, 0.5, 0.25, 1.0]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn art_mesh_deformer_opacity_rejects_missing_cyclic_and_unsupported_targets() {
        let mesh = json!({
            "guid":"mesh", "targetDeformerGuid":"missing",
            "data":{"type":"ArtMesh","clipGuids":[]}
        });
        let sources = HashMap::from([("mesh".to_owned(), &mesh)]);
        let error = native_layer(
            &mesh,
            &json!({"opacity":1.0}),
            &sources,
            &HashMap::new(),
            &BTreeMap::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("native target deformer does not resolve"));

        let mesh = json!({
            "guid":"mesh", "targetDeformerGuid":"first",
            "data":{"type":"ArtMesh","clipGuids":[]}
        });
        let first = json!({
            "guid":"first", "targetDeformerGuid":"second",
            "forms":[{"data":{"type":"Rotation"},"opacity":1.0}],
            "data":{"type":"Rotation"}
        });
        let second = json!({
            "guid":"second", "targetDeformerGuid":"first",
            "forms":[{"data":{"type":"Warp"},"opacity":1.0}],
            "data":{"type":"Warp"}
        });
        let sources = HashMap::from([
            ("mesh".to_owned(), &mesh),
            ("first".to_owned(), &first),
            ("second".to_owned(), &second),
        ]);
        let error = native_layer(
            &mesh,
            &json!({"opacity":1.0}),
            &sources,
            &HashMap::new(),
            &BTreeMap::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("native deformer hierarchy is cyclic or too deep"));

        let unknown = json!({
            "guid":"unknown", "forms":[{"data":{"type":"Mask"},"opacity":1.0}],
            "data":{"type":"Mask"}
        });
        let mesh = json!({
            "guid":"mesh", "targetDeformerGuid":"unknown",
            "data":{"type":"ArtMesh","clipGuids":[]}
        });
        let sources = HashMap::from([("mesh".to_owned(), &mesh), ("unknown".to_owned(), &unknown)]);
        let error = native_layer(
            &mesh,
            &json!({"opacity":1.0}),
            &sources,
            &HashMap::new(),
            &BTreeMap::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("unsupported native deformer type"));
    }

    #[test]
    fn offscreen_group_applies_once_after_independent_deformer_opacity() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        for source in sources
            .iter_mut()
            .filter(|source| source["data"]["type"] == "ArtMesh")
        {
            source["parentPartGuid"] = json!("group");
            if source["guid"] == "mesh-guid" {
                source["targetDeformerGuid"] = json!("rotation");
            } else {
                source["visible"] = json!(false);
            }
        }
        sources.push(json!({
            "guid":"rotation", "targetDeformerGuid":"warp",
            "forms":[{"data":{"type":"Rotation","angle":0.0,"origin":[0.0,0.0],
                "scale":1.0},"opacity":0.5}],
            "data":{"type":"Rotation","baseAngle":0.0}
        }));
        sources.push(json!({
            "guid":"warp", "targetDeformerGuid":crate::authoring_hierarchy::ROOT_DEFORMER_GUID,
            "forms":[{"data":{"type":"Warp","positions":[0.0,0.0,1.0,0.0,0.0,1.0,1.0,1.0]},
                "opacity":0.5}],
            "data":{"type":"Warp","columns":1,"rows":1}
        }));
        sources.push(json!({
            "guid":"group", "name":"group", "visible":true,
            "data":{"type":"Part","useOffscreen":true,"drawOrderGroup":true,
                "colorComposition":"NORMAL","alphaComposition":"OVER","clipGuids":[]},
            "forms":[{"data":{"type":"Part"},"opacity":0.5,"drawOrder":500}]
        }));
        let source_before = scene.source.clone();
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        assert_eq!(
            model["nodes"]["children"][0]["children"][0]["opacity"],
            0.1875
        );
        let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
        let png = crate::render::render_png_for_scene(
            &model,
            &geometry,
            scene.textures(),
            32,
            32,
            [0.0, 0.0, 50.0, 50.0],
        )
        .unwrap();
        let image = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(image.pixels().map(|pixel| pixel[3]).max(), Some(24));
        assert_eq!(scene.source, source_before);
    }

    #[test]
    fn zero_deformer_opacity_hides_visible_art() {
        let mut scene = preview_scene();
        let mesh_index = scene.source["sources"]
            .as_array()
            .unwrap()
            .iter()
            .position(|source| source["data"]["type"] == "ArtMesh")
            .unwrap();
        scene.source["sources"][mesh_index]["targetDeformerGuid"] = json!("zero-opacity");
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"zero-opacity", "targetDeformerGuid":crate::authoring_hierarchy::ROOT_DEFORMER_GUID,
            "forms":[{"data":{"type":"Rotation","angle":0.0,"origin":[0.0,0.0],
                "scale":1.0},"opacity":0.0}],
            "data":{"type":"Rotation","baseAngle":0.0}
        }));
        let source_before = scene.source.clone();
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        assert_eq!(model["nodes"]["children"][0]["opacity"], 0.0);
        let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
        let png = crate::render::render_png_for_scene(
            &model,
            &geometry,
            scene.textures(),
            32,
            32,
            [0.0, 0.0, 50.0, 50.0],
        )
        .unwrap();
        let image = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(image.pixels().map(|pixel| pixel[3]).max(), Some(0));
        assert_eq!(scene.source, source_before);
    }

    #[test]
    fn offscreen_opacity_is_applied_once_after_overlapping_children() {
        for (nested, expected_alpha) in [(false, 128), (true, 64)] {
            let mut scene = preview_scene();
            scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
            scene.source = scene.parsed.source_value().unwrap();
            let sources = scene.source["sources"].as_array_mut().unwrap();
            for source in sources.iter_mut().filter(|s| s["data"]["type"] == "ArtMesh") {
                source["parentPartGuid"] = json!("group");
            }
            let group = |guid: &str, parent: Option<&str>| json!({
                "guid": guid, "name": guid, "visible": true, "parentPartGuid": parent,
                "data": {"type":"Part", "useOffscreen":true, "drawOrderGroup":true,
                    "colorComposition":"NORMAL", "alphaComposition":"OVER", "clipGuids":[]},
                "forms":[{"data":{"type":"Part"},"opacity":0.5,"drawOrder":500}]
            });
            sources.push(group("group", nested.then_some("outer")));
            if nested {
                sources.push(group("outer", None));
            }
            let mut model = scene.render_model(&BTreeMap::new()).unwrap().0;
            let group_node = if nested {
                &mut model["nodes"]["children"][0]["children"][0]
            } else {
                &mut model["nodes"]["children"][0]
            };
            for mesh in group_node["children"].as_array_mut().unwrap() {
                assert_eq!(mesh["opacity"], 0.75);
                mesh["opacity"] = json!(1.0);
            }
            let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
            let png = crate::render::render_png_for_scene(&model, &geometry, scene.textures(),
                32, 32, [0.0, 0.0, 50.0, 50.0]).unwrap();
            let image = image::load_from_memory_with_format(&png, ImageFormat::Png).unwrap().into_rgba8();
            assert_eq!(image.pixels().map(|pixel| pixel[3]).max(), Some(expected_alpha));
            assert!(image.pixels().any(|pixel| pixel[0] > pixel[2]));
            assert!(!image.pixels().any(|pixel| pixel[2] > pixel[0]));
        }
    }

    #[test]
    fn offscreen_external_hidden_mask_and_inversion_clip_the_group() {
        for inverted in [false, true] {
            let mut scene = preview_scene();
            scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
            scene.source = scene.parsed.source_value().unwrap();
            let sources = scene.source["sources"].as_array_mut().unwrap();
            for mesh in sources.iter_mut().filter(|s| s["data"]["type"] == "ArtMesh") {
                if mesh["guid"] == "back-mesh-guid" {
                    mesh["visible"] = json!(false);
                    mesh["parentPartGuid"] = Value::Null;
                } else { mesh["parentPartGuid"] = json!("group"); }
            }
            sources.push(json!({"guid":"group", "visible":true,
                "data":{"type":"Part","useOffscreen":true,"colorComposition":"NORMAL",
                    "alphaComposition":"OVER","clipGuids":["back-mesh-guid"],"invertClippingMask":inverted},
                "forms":[{"data":{"type":"Part"},"opacity":0.5,"drawOrder":500}]}));
            let model = scene.render_model(&BTreeMap::new()).unwrap().0;
            let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
            let png = crate::render::render_png_for_scene(&model,&geometry,scene.textures(),32,32,[0.0,0.0,50.0,50.0]).unwrap();
            let image = image::load_from_memory_with_format(&png,ImageFormat::Png).unwrap().into_rgba8();
            assert_eq!(image.get_pixel(10, 10)[3], if inverted { 0 } else { 96 });
        }
    }

    #[test]
    fn nested_out_then_atop_composes_at_each_offscreen_boundary() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        for mesh in sources.iter_mut().filter(|s| s["data"]["type"] == "ArtMesh") {
            mesh["parentPartGuid"] = json!(if mesh["guid"] == "back-mesh-guid" {"outer"} else {"inner"});
        }
        for (guid,parent,alpha,order) in [("inner",Some("outer"),"OUT",600),("outer",None,"ATOP",500)] {
            sources.push(json!({"guid":guid,"visible":true,"parentPartGuid":parent,
                "data":{"type":"Part","useOffscreen":true,"colorComposition":"NORMAL",
                    "alphaComposition":alpha,"clipGuids":[]},
                "forms":[{"data":{"type":"Part"},"opacity":1.0,"drawOrder":order}]}));
        }
        let mut model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let outer = &model["nodes"]["children"][0];
        let mut background = outer["children"].as_array().unwrap().iter()
            .find(|node| node["type"] == "Part").unwrap().clone();
        background["uuid"] = json!(777);
        background["zsort"] = json!(0);
        background["opacity"] = json!(1.0);
        background["tint"] = json!([0.0, 1.0, 0.0]);
        model["nodes"]["children"].as_array_mut().unwrap().push(background);
        let geometry = crate::scene_deform::evaluate(&model,&BTreeMap::new()).unwrap();
        let png = crate::render::render_png_for_scene(&model,&geometry,scene.textures(),32,32,[0.0,0.0,50.0,50.0]).unwrap();
        let image = image::load_from_memory_with_format(&png,ImageFormat::Png).unwrap().into_rgba8();
        assert_eq!(image.get_pixel(10, 10).0, [0, 207, 48, 255]);
    }

    #[test]
    fn equal_draw_orders_keep_frontmost_authoring_sources_on_top() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let (model, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("palette order is unavailable")));
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn palette_order_overrides_source_array_order_for_equal_draw_orders() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        set_part_children(
            &mut scene,
            &root_guid,
            &["unsupported-affecter", "back-mesh-guid", "mesh-guid"],
        );
        let source_before = scene.source.clone();
        let (model, warnings) = scene.render_model(&BTreeMap::new()).unwrap();
        let children = model["nodes"]["children"].as_array().unwrap();
        assert_eq!(
            children
                .iter()
                .map(|node| node["uuid"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![
                source_id(&scene, "mesh-guid"),
                source_id(&scene, "back-mesh-guid")
            ]
        );
        assert!(!warnings
            .iter()
            .any(|warning| warning.contains("palette order is unavailable")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("effects are unhandled")));
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);
        assert_eq!(scene.source, source_before);
    }

    #[test]
    fn interpolated_draw_orders_are_whole_before_the_palette_tiebreak() {
        let mut scene = grouped_preview_scene();
        set_root_part_children(&mut scene, &["unsupported-affecter", "blue-mesh", "red-mesh"]);
        set_source_draw_order(&mut scene, "blue-mesh", 500);
        let mut pixel = |red: f64| {
            set_source_draw_order(&mut scene, "red-mesh", red);
            let model = scene.render_model(&BTreeMap::new()).unwrap().0;
            rendered_pixel(&scene, &model)
        };
        assert_eq!(pixel(501.0), [204, 0, 51, 239]);
        // Interpolation noise and a fraction below the next whole order both keep the palette tie, which puts the
        // blue mesh on top.
        for red in [500.000_000_000_000_06, 500.9, 500.0] {
            assert_eq!(pixel(red), [51, 0, 204, 239], "{red}");
        }
    }

    #[test]
    fn numeric_zsort_remains_ahead_of_the_palette_tiebreak() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        set_part_children(&mut scene, &root_guid, &["back-mesh-guid", "mesh-guid"]);
        let mut model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let red_id = source_id(&scene, "mesh-guid");
        let blue_id = source_id(&scene, "back-mesh-guid");
        let children = model["nodes"]["children"].as_array_mut().unwrap();
        assert_eq!(
            children.iter().find(|node| node["uuid"] == red_id).unwrap()["zsort"],
            children.iter().find(|node| node["uuid"] == blue_id).unwrap()["zsort"]
        );
        // Distinct zsort values model distinct native drawOrder values; palette order only breaks ties.
        children
            .iter_mut()
            .find(|node| node["uuid"] == red_id)
            .unwrap()["zsort"] = json!(-600.0);
        children
            .iter_mut()
            .find(|node| node["uuid"] == blue_id)
            .unwrap()["zsort"] = json!(-500.0);
        assert_eq!(rendered_pixel(&scene, &model), [204, 0, 51, 239]);
    }

    #[test]
    fn nested_offscreen_scope_uses_palette_order_for_equal_draw_orders() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        for source in scene.source["sources"].as_array_mut().unwrap() {
            if source["data"]["type"] == "ArtMesh" {
                source["parentPartGuid"] = json!("inner");
            }
        }
        let group = |guid: &str, parent: &str, children: &[&str]| {
            json!({
                "guid":guid,"name":guid,"visible":true,"parentPartGuid":parent,
                "data":{"type":"Part","childGuids":children,"useOffscreen":true,
                    "drawOrderGroup":true,"colorComposition":"NORMAL","alphaComposition":"OVER",
                    "clipGuids":[]},
                "forms":[{"data":{"type":"Part"},"opacity":1.0,"drawOrder":500}]
            })
        };
        scene.source["sources"].as_array_mut().unwrap().push(group(
            "inner",
            "outer",
            &["back-mesh-guid", "mesh-guid"],
        ));
        scene.source["sources"].as_array_mut().unwrap().push(group(
            "outer",
            &root_guid,
            &["inner"],
        ));
        set_part_children(&mut scene, &root_guid, &["outer"]);
        let (model, _) = scene.render_model(&BTreeMap::new()).unwrap();
        let outer = &model["nodes"]["children"][0];
        let inner = &outer["children"][0];
        assert_eq!(
            inner["children"].as_array().unwrap()[0]["uuid"],
            source_id(&scene, "mesh-guid")
        );
        assert_eq!(
            inner["children"].as_array().unwrap()[1]["uuid"],
            source_id(&scene, "back-mesh-guid")
        );
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);
    }

    #[test]
    fn offscreen_root_part_has_a_palette_rank() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let old_root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();
        {
            let old_root = scene.source["sources"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|source| source["guid"] == old_root_guid)
                .unwrap();
            old_root["parentPartGuid"] = json!("offscreen-root");
            old_root["data"]["childGuids"] = json!(["back-mesh-guid", "mesh-guid"]);
        }
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"offscreen-root","name":"offscreen-root","visible":true,
            "data":{"type":"Part","childGuids":[old_root_guid],"useOffscreen":true,
                "drawOrderGroup":true,"colorComposition":"NORMAL","alphaComposition":"OVER",
                "clipGuids":[]},
            "forms":[{"data":{"type":"Part"},"opacity":1.0,"drawOrder":500}]
        }));
        scene.source["rootPartGuid"] = json!("offscreen-root");
        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let root = &model["nodes"]["children"][0];
        assert_eq!(root["uuid"], source_id(&scene, "offscreen-root"));
        assert_eq!(
            root["children"].as_array().unwrap()[0]["uuid"],
            source_id(&scene, "mesh-guid")
        );
        assert_eq!(
            root["children"].as_array().unwrap()[1]["uuid"],
            source_id(&scene, "back-mesh-guid")
        );
        assert_eq!(rendered_pixel(&scene, &model), [51, 0, 204, 239]);
    }

    #[test]
    fn palette_corruption_fails_closed() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let root_guid = scene.source["rootPartGuid"].as_str().unwrap().to_owned();

        set_part_children(
            &mut scene,
            &root_guid,
            &["mesh-guid", "mesh-guid", "back-mesh-guid"],
        );
        assert!(scene
            .render_model(&BTreeMap::new())
            .unwrap_err()
            .contains("duplicate child membership"));

        set_part_children(
            &mut scene,
            &root_guid,
            &[&root_guid, "mesh-guid", "back-mesh-guid"],
        );
        assert!(scene
            .render_model(&BTreeMap::new())
            .unwrap_err()
            .contains("cyclic"));

        set_part_children(&mut scene, &root_guid, &["mesh-guid", "back-mesh-guid"]);
        {
            let mesh = scene.source["sources"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|source| source["guid"] == "mesh-guid")
                .unwrap();
            mesh["parentPartGuid"] = json!("missing-part");
        }
        assert!(scene
            .render_model(&BTreeMap::new())
            .unwrap_err()
            .contains("does not name its Part parent"));

        {
            let mesh = scene.source["sources"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|source| source["guid"] == "mesh-guid")
                .unwrap();
            mesh["parentPartGuid"] = json!(root_guid);
        }
        set_part_children(&mut scene, &root_guid, &["mesh-guid"]);
        assert!(scene
            .render_model(&BTreeMap::new())
            .unwrap_err()
            .contains("does not rank known source"));
    }

    #[test]
    fn model_image_and_atlas_meshes_take_their_uvs_as_image_coordinates() {
        for state in ["MODEL_IMAGE", "TEXTURE_ATLAS"] {
            let mut scene = preview_scene();
            scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
            scene.source = scene.parsed.source_value().unwrap();
            scene.source["textures"][0]["imageToLogical"] = json!([2.0, 0.0, 0.25, 0.0, 4.0, 0.5]);
            for source in scene.source["sources"].as_array_mut().unwrap() {
                if source["data"]["type"] == "ArtMesh" {
                    source["data"]["textureState"] = json!(state);
                    source["data"]["uvs"] = json!([0.25, 0.5, 2.25, 0.5, 0.25, 4.5]);
                }
            }
            let model = scene.render_model(&BTreeMap::new()).unwrap().0;
            let children = model["nodes"]["children"].as_array().unwrap();
            assert_eq!(children.len(), 2);
            for mesh in children {
                assert_eq!(mesh["mesh"]["uvs"], json!([0.25, 0.5, 2.25, 0.5, 0.25, 4.5]));
                assert_eq!(mesh["textures"][0], 0);
            }
        }
        let mut scene = preview_scene();
        scene.source["sources"][0]["data"]["textureState"] = json!("UNKNOWN");
        assert!(scene
            .render_model(&BTreeMap::new())
            .unwrap_err()
            .contains("unsupported texture state"));
    }

    #[test]
    fn hidden_native_mask_clips_without_drawing_its_part() {
        let mut scene = preview_scene();
        scene.parsed = authoring_scene::tests::overlapping_native_preview_scene();
        scene.source = scene.parsed.source_value().unwrap();
        let (target_index, mask_index) = {
            let sources = scene.source["sources"].as_array().unwrap();
            (
                sources
                    .iter()
                    .position(|source| source["guid"] == "mesh-guid")
                    .unwrap(),
                sources
                    .iter()
                    .position(|source| source["guid"] == "back-mesh-guid")
                    .unwrap(),
            )
        };
        scene.source["sources"][target_index]["data"]["clipGuids"] = json!(["back-mesh-guid"]);
        scene.source["sources"][mask_index]["visible"] = json!(false);
        let mask_texture_guid = "mask-texture-guid";
        let mask_texture = scene.source["textures"][0].clone();
        scene.source["textures"].as_array_mut().unwrap().push({
            let mut texture = mask_texture;
            texture["guid"] = json!(mask_texture_guid);
            texture
        });
        scene.source["sources"][mask_index]["data"]["textureGuid"] = json!(mask_texture_guid);
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(2, 2, |_, y| {
            if y == 0 {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        }))
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
        scene.textures.push(TextureBlob {
            tag: 0,
            data: encoded.into_inner(),
        });
        scene.texture_slots.insert(mask_texture_guid.into(), 1);
        scene.source["sources"][mask_index]["targetDeformerGuid"] = json!("zero-opacity-mask");
        scene.source["sources"].as_array_mut().unwrap().push(json!({
            "guid":"zero-opacity-mask",
            "targetDeformerGuid":crate::authoring_hierarchy::ROOT_DEFORMER_GUID,
            "forms":[{"data":{"type":"Rotation","angle":0.0,"origin":[0.0,0.0],
                "scale":1.0},"opacity":0.0}],
            "data":{"type":"Rotation","baseAngle":0.0}
        }));

        let model = scene.render_model(&BTreeMap::new()).unwrap().0;
        let children = model["nodes"]["children"].as_array().unwrap();
        let target = children
            .iter()
            .find(|node| {
                node["masks"]
                    .as_array()
                    .is_some_and(|masks| !masks.is_empty())
            })
            .unwrap();
        let mask_id = target["masks"][0]["source"].as_u64().unwrap();
        let mask = children
            .iter()
            .find(|node| node["uuid"].as_u64() == Some(mask_id))
            .unwrap();
        assert_eq!(mask["type"], "Part");
        assert_eq!(mask["enabled"], false);
        assert_eq!(mask["opacity"], 0.0);

        let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
        let masked_png = crate::render::render_png_for_scene(
            &model,
            &geometry,
            scene.textures(),
            32,
            32,
            [0.0, 0.0, 50.0, 50.0],
        )
        .unwrap();
        let masked = image::load_from_memory_with_format(&masked_png, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        let masked_red = masked
            .pixels()
            .filter(|pixel| pixel[0] > pixel[2] && pixel[3] > 0)
            .count();
        assert!(masked_red > 0);
        assert!(!masked.pixels().any(|pixel| pixel[2] > pixel[0]));

        let mut unmasked_model = model.clone();
        let unmasked_target = unmasked_model["nodes"]["children"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["uuid"] == target["uuid"])
            .unwrap();
        unmasked_target["masks"] = json!([]);
        let unmasked_geometry =
            crate::scene_deform::evaluate(&unmasked_model, &BTreeMap::new()).unwrap();
        let unmasked_png = crate::render::render_png_for_scene(
            &unmasked_model,
            &unmasked_geometry,
            scene.textures(),
            32,
            32,
            [0.0, 0.0, 50.0, 50.0],
        )
        .unwrap();
        let unmasked = image::load_from_memory_with_format(&unmasked_png, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        let unmasked_red = unmasked
            .pixels()
            .filter(|pixel| pixel[0] > pixel[2] && pixel[3] > 0)
            .count();
        assert!(masked_red < unmasked_red);
    }

    #[test]
    fn parameter_values_change_native_render_packet_geometry() {
        let scene = preview_scene();
        let source_before = scene.source.clone();
        let negative = BTreeMap::from([("ParamMorph".into(), -1.0), ("ParamMorph2".into(), 0.0)]);
        let positive = BTreeMap::from([("ParamMorph".into(), 1.0), ("ParamMorph2".into(), 0.0)]);
        let negative_model = scene.render_model(&negative).unwrap().0;
        let positive_model = scene.render_model(&positive).unwrap().0;
        let negative_geometry =
            crate::scene_deform::evaluate(&negative_model, &BTreeMap::new()).unwrap();
        let positive_geometry =
            crate::scene_deform::evaluate(&positive_model, &BTreeMap::new()).unwrap();
        let negative_packet = crate::render::render_packet_for_scene(
            &negative_model,
            &negative_geometry,
            scene.textures(),
        )
        .unwrap();
        let positive_packet = crate::render::render_packet_for_scene(
            &positive_model,
            &positive_geometry,
            scene.textures(),
        )
        .unwrap();
        assert_ne!(
            negative_packet["draws"][0]["vertices"],
            positive_packet["draws"][0]["vertices"]
        );
        assert_eq!(negative_packet["textures"][0]["width"], 2);
        assert_eq!(scene.source, source_before);
    }

    #[test]
    fn rotation_uses_local_vectors_and_maps_nonzero_origins() {
        let mapped = rotation_offset([3.0, 4.0], std::f64::consts::FRAC_PI_2, 2.0, false, false);
        assert!((mapped[0] + 8.0).abs() < 1e-12);
        assert!((mapped[1] - 6.0).abs() < 1e-12);

        let scene = preview_scene();
        let root = json!({
            "guid":"root-rotation",
            "targetDeformerGuid":crate::authoring_hierarchy::ROOT_DEFORMER_GUID,
            "data":{"type":"Rotation","baseAngle":0.0}
        });
        let child = json!({
            "guid":"child-rotation","targetDeformerGuid":"root-rotation",
            "data":{"type":"Rotation","baseAngle":0.0}
        });
        let root_form = json!({"evaluatedForms":[{"data":{
            "angle":0.0,"origin":[100.0,200.0],"scale":1.0
        }}]});
        let child_form = json!({"evaluatedForms":[{"data":{
            "angle":0.0,"origin":[5.0,7.0],"scale":1.0
        }}]});
        let sources = HashMap::from([
            ("root-rotation".to_owned(), &root),
            ("child-rotation".to_owned(), &child),
        ]);
        let forms = HashMap::from([
            ("root-rotation".to_owned(), &root_form),
            ("child-rotation".to_owned(), &child_form),
        ]);
        assert_eq!(
            scene
                .to_canvas(
                    [-62.916374, 653.87494],
                    Some("root-rotation"),
                    &sources,
                    &forms,
                    &mut HashSet::new(),
                    0,
                    &mut Vec::new(),
                    &mut HashMap::new(),
                )
                .unwrap(),
            [37.083626, 853.87494]
        );
        assert_eq!(
            scene
                .to_canvas(
                    [10.0, 20.0],
                    Some("child-rotation"),
                    &sources,
                    &forms,
                    &mut HashSet::new(),
                    0,
                    &mut Vec::new(),
                    &mut HashMap::new(),
                )
                .unwrap(),
            [115.0, 227.0]
        );
    }

    fn static_rotation_reparent_model() -> Value {
        let rotation = |guid: &str, parent: &str, angle: f64, base: f64, origin: [f64; 2], scale: f64| {
            let form_guid = format!("{guid}-form");
            json!({"guid":guid,"targetDeformerGuid":parent,"data":{"type":"Rotation","baseAngle":base},
                "extendedInterpolations":[],"morphTargets":null,
                "grid":{"axes":[],"cells":[{"formGuid":form_guid,"indices":[]}]},
                "forms":[{"guid":form_guid,"coordinateType":if parent == crate::authoring_hierarchy::ROOT_DEFORMER_GUID {"Canvas"} else {"DeformerLocal"},
                    "animated":false,"localAnimated":false,"opacity":1.0,
                    "data":{"type":"Rotation","angle":angle,"origin":origin,"scale":scale,"reflectX":false,"reflectY":false}}]})
        };
        let root = crate::authoring_hierarchy::ROOT_DEFORMER_GUID;
        json!({"sources":[
            {"guid":"mesh","targetDeformerGuid":"old","data":{"type":"ArtMesh","positions":[1.0,2.0,-3.0,4.0]},
             "extendedInterpolations":[],"morphTargets":null,
             "grid":{"axes":[],"cells":[{"formGuid":"mesh-form","indices":[]}]},
             "forms":[{"guid":"mesh-form","coordinateType":"DeformerLocal","animated":false,"localAnimated":false,"opacity":0.7,
                "data":{"type":"ArtMesh","positions":[1.0,2.0,-3.0,4.0]}}]},
            rotation("old",root,60.0,30.0,[10.0,20.0],2.0),
            rotation("new-root",root,-90.0,0.0,[100.0,-50.0],0.5),
            rotation("new-child","new-root",90.0,0.0,[5.0,-7.0],4.0)
        ]})
    }

    #[test]
    fn static_rotation_reparent_preserves_hand_derived_and_rendered_canvas_points() {
        let mut model = static_rotation_reparent_model();
        let original = model.clone();
        let expected = [[6.0,22.0],[2.0,14.0]];
        let scene = preview_scene();
        for (iteration, target) in ["new-child", "new-root", "new-child", "old"].into_iter().enumerate() {
            let positions = reexpress_rotation_positions(&model, "mesh", target).unwrap();
            if iteration == 0 {
                for (actual, expected) in positions.iter().zip([-45.25,37.25,-47.25,33.25]) {
                    assert!((actual - expected).abs() < 1.0e-12, "{positions:?}");
                }
            }
            model["sources"][0]["targetDeformerGuid"] = json!(target);
            model["sources"][0]["forms"][0]["data"]["positions"] = json!(positions);
            let sources = model["sources"].as_array().unwrap().iter()
                .map(|s| (s["guid"].as_str().unwrap().to_owned(), s)).collect::<HashMap<_, _>>();
            for (point, expected) in positions.chunks_exact(2).zip(expected) {
                let actual = scene.to_canvas([point[0],point[1]], Some(target), &sources, &HashMap::new(),
                    &mut HashSet::new(), 0, &mut Vec::new(), &mut HashMap::new()).unwrap();
                for axis in 0..2 { assert!((actual[axis] - expected[axis]).abs() < 1.0e-9, "{actual:?}"); }
            }
            assert_eq!(model["sources"][0]["forms"][0]["opacity"], json!(0.7));
        }
        let before = original.clone();
        reexpress_rotation_positions(&original, "mesh", "new-child").unwrap();
        assert_eq!(original, before);
    }

    #[test]
    fn static_rotation_reparent_refuses_unsupported_geometry_and_unstable_inverses() {
        let base = static_rotation_reparent_model();
        for (path, value) in [
            ("/sources/0/forms/0/coordinateType",json!("Canvas")),
            ("/sources/0/forms/0/animated",json!(true)),
            ("/sources/0/forms/0/localAnimated",json!(true)),
            ("/sources/0/forms/0/data/positions",json!([1.0,2.0])),
            ("/sources/0/grid/axes",json!([{"parameterGuid":"p"}])),
            ("/sources/0/grid/cells/0/formGuid",json!("missing")),
            ("/sources/0/morphTargets",json!({"targets":[{}],"blendWeightConstraints":[]})),
            ("/sources/0/extendedInterpolations",json!([{}])),
            ("/sources/1/grid/axes",json!([{"parameterGuid":"p"}])),
            ("/sources/1/forms/0/opacity",json!(0.5)),
            ("/sources/1/forms/0/coordinateType",json!("DeformerLocal")),
            ("/sources/1/forms/0/data/reflectX",json!(true)),
            ("/sources/1/forms/0/data/reflectX",json!("false")),
            ("/sources/1/forms/0/data/reflectY",json!(true)),
            ("/sources/1/forms/0/data/scale",json!(0.0)),
            ("/sources/1/forms/0/data/scale",json!(-1.0)),
            ("/sources/1/forms/0/data/scale",json!(1.0e-7)),
            ("/sources/1/forms/0/data/angle",json!("bad")),
            ("/sources/1/data/baseAngle",json!("bad")),
            ("/sources/1/forms/0/data/origin",json!([0.0,"bad"])),
            ("/sources/1/forms/0/data/origin",json!([1.0e20,1.0e20])),
            ("/sources/1/data/type",json!("Warp")),
            ("/sources/1/targetDeformerGuid",json!("old")),
            ("/sources/1/targetDeformerGuid",json!("missing")),
            ("/sources/3/forms/0/coordinateType",json!("Canvas")),
            ("/sources/2/forms/0/data/origin",json!([1.0e20,1.0e20])),
        ] {
            let mut model = base.clone();
            *model.pointer_mut(path).unwrap() = value;
            assert!(reexpress_rotation_positions(&model,"mesh","new-child").is_err(), "accepted {path}");
        }
        let mut model = base.clone();
        model["sources"][2]["forms"][0]["data"]["scale"] = json!(1.0e-6);
        model["sources"][3]["forms"][0]["data"]["scale"] = json!(1.0e-6);
        assert!(reexpress_rotation_positions(&model,"mesh","new-child").is_err());
        let mut model = base.clone();
        model["sources"].as_array_mut().unwrap().push(json!({"guid":"glue","data":{"type":"Glue","targetArtMeshAGuid":"mesh"}}));
        assert!(reexpress_rotation_positions(&model,"mesh","new-child").unwrap_err().contains("Glue"));
        let mut model = base.clone();
        let extra = model["sources"][0]["forms"][0].clone();
        model["sources"][0]["forms"].as_array_mut().unwrap().push(extra);
        assert!(reexpress_rotation_positions(&model,"mesh","new-child").is_err());
        assert!(reexpress_rotation_positions(&base,"mesh",crate::authoring_hierarchy::ROOT_DEFORMER_GUID).is_err());
    }

    #[test]
    fn static_rotation_reparent_refuses_destination_precision_that_collapses_local_geometry() {
        let mut model = static_rotation_reparent_model();
        let points = json!([0.0,0.0,0.25,0.0,0.0,0.25]);
        model["sources"][0]["data"]["positions"] = points.clone();
        model["sources"][0]["forms"][0]["data"]["positions"] = points;
        for index in [1,2] {
            model["sources"][index]["data"]["baseAngle"] = json!(0.0);
            model["sources"][index]["forms"][0]["data"]["angle"] = json!(0.0);
        }
        model["sources"][1]["forms"][0]["data"]["origin"] = json!([0.0,0.0]);
        model["sources"][1]["forms"][0]["data"]["scale"] = json!(1.0e-6);
        model["sources"][2]["forms"][0]["data"]["origin"] = json!([1.0e20,1.0e20]);
        model["sources"][2]["forms"][0]["data"]["scale"] = json!(1.0);
        let before = model.clone();
        assert!(reexpress_rotation_positions(&model,"mesh","new-root").unwrap_err().contains("source-local"));
        assert_eq!(model,before);
    }

    #[test]
    fn static_rotation_reparent_bounds_depth_scale_and_native_float_positions() {
        let mut model = static_rotation_reparent_model();
        model["sources"][2]["forms"][0]["data"]["scale"] = json!(2.0e-6);
        model["sources"][3]["forms"][0]["data"]["scale"] = json!(0.5);
        reexpress_rotation_positions(&model,"mesh","new-child").unwrap();
        // Keep the source axis-aligned so this reaches the destination's native-float range guard.
        model["sources"][1]["data"]["baseAngle"] = json!(0.0);
        model["sources"][1]["forms"][0]["data"]["angle"] = json!(0.0);
        model["sources"][0]["forms"][0]["data"]["positions"][0] = json!(1.0e33);
        assert!(reexpress_rotation_positions(&model,"mesh","new-child").unwrap_err().contains("native float range"));

        let mut model = static_rotation_reparent_model();
        let mut rotation = model["sources"][1].clone();
        rotation["data"]["baseAngle"] = json!(0.0);
        rotation["forms"][0]["data"]["angle"] = json!(0.0);
        rotation["forms"][0]["data"]["origin"] = json!([0.0,0.0]);
        rotation["forms"][0]["data"]["scale"] = json!(1.0);
        for depth in 0..=MAX_DEFORMER_DEPTH {
            let mut step = rotation.clone();
            step["guid"] = json!(format!("depth-{depth}"));
            step["targetDeformerGuid"] = if depth == 0 { json!(crate::authoring_hierarchy::ROOT_DEFORMER_GUID) }
                else { json!(format!("depth-{}",depth-1)) };
            step["forms"][0]["guid"] = json!(format!("depth-form-{depth}"));
            step["grid"]["cells"][0]["formGuid"] = step["forms"][0]["guid"].clone();
            step["forms"][0]["coordinateType"] = json!(if depth == 0 { "Canvas" } else { "DeformerLocal" });
            model["sources"].as_array_mut().unwrap().push(step);
            if depth == MAX_DEFORMER_DEPTH - 1 {
                reexpress_rotation_positions(&model,"mesh",&format!("depth-{depth}")).unwrap();
            }
        }
        assert!(reexpress_rotation_positions(&model,"mesh",&format!("depth-{MAX_DEFORMER_DEPTH}")).unwrap_err().contains("too deep"));
        let duplicate = model["sources"][1].clone();
        model["sources"].as_array_mut().unwrap().push(duplicate);
        assert!(reexpress_rotation_positions(&model,"mesh","new-child").unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn hidden_ancestry_skips_material_checks_and_part_names_do_not_hide_art() {
        let values = BTreeMap::new();
        let mut scene = preview_scene();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        let mesh = sources
            .iter()
            .find(|source| source["data"]["type"] == "ArtMesh")
            .unwrap();
        let mesh_guid = mesh["guid"].as_str().unwrap().to_owned();
        let part_guid = mesh["parentPartGuid"].as_str().unwrap().to_owned();
        let part = sources
            .iter_mut()
            .find(|source| source["guid"] == part_guid)
            .unwrap();
        part["visible"] = json!(false);
        part["data"]["sketch"] = json!(true);
        let mesh = sources
            .iter_mut()
            .find(|source| source["guid"] == mesh_guid)
            .unwrap();
        mesh["data"]["textureState"] = json!("MODEL_IMAGE");
        mesh["data"]["culling"] = json!(true);
        let model = scene.render_model(&values).unwrap().0;
        assert_eq!(model["nodes"]["children"], json!([]));

        let mut scene = preview_scene();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        let part_guid = sources
            .iter()
            .find(|source| source["data"]["type"] == "ArtMesh")
            .unwrap()["parentPartGuid"]
            .as_str()
            .unwrap()
            .to_owned();
        let part = sources
            .iter_mut()
            .find(|source| source["guid"] == part_guid)
            .unwrap();
        part["name"] = json!("PartBackground");
        let model = scene.render_model(&values).unwrap().0;
        assert_eq!(model["nodes"]["children"].as_array().unwrap().len(), 1);

        let mut scene = preview_scene();
        let sources = scene.source["sources"].as_array_mut().unwrap();
        let mesh = sources
            .iter_mut()
            .find(|source| source["data"]["type"] == "ArtMesh")
            .unwrap();
        mesh["visible"] = json!(false);
        mesh["data"]["clipGuids"] = json!(["missing-mask"]);
        assert!(scene.render_model(&values).is_err());
    }

    #[test]
    fn warp_cells_are_triangle_pairs_and_the_outside_goes_over_to_one_affine_map() {
        // One cell whose far corner is pulled out: the two triangles meet on the diagonal from (1,0) to (0,1).
        let bent = [0.0, 0.0, 10.0, 0.0, 0.0, 10.0, 20.0, 20.0];
        assert_eq!(warp_point([0.25, 0.25], &bent, 1, 1, false).unwrap(), [2.5, 2.5]);
        assert_eq!(warp_point([0.5, 0.5], &bent, 1, 1, false).unwrap(), [5.0, 5.0]);
        assert_eq!(warp_point([0.75, 0.75], &bent, 1, 1, false).unwrap(), [12.5, 12.5]);
        // A quad-transform deformer is bilinear in the cell instead.
        assert_eq!(warp_point([0.5, 0.5], &bent, 1, 1, true).unwrap(), [7.5, 7.5]);
        // Beyond -2..3 the four corners give one affine map: (0,0) goes to (-2.5,-2.5), +x adds (15,5), +y (5,15).
        assert_eq!(warp_point([4.0, 0.0], &bent, 1, 1, false).unwrap(), [57.5, 17.5]);
        assert_eq!(warp_point([-3.0, 4.0], &bent, 1, 1, false).unwrap(), [-27.5, 42.5]);
        assert_eq!(warp_point([3.0, 0.0], &bent, 1, 1, false).unwrap(), [42.5, 12.5]);
        // In between, a border quad runs from the grid edge at (10,0) out to that map at x = 3.
        assert_eq!(warp_point([1.0, 0.0], &bent, 1, 1, false).unwrap(), [10.0, 0.0]);
        assert_eq!(warp_point([2.0, 0.0], &bent, 1, 1, false).unwrap(), [26.25, 6.25]);
    }

    fn deformer(guid: &str, parent: &str, data: Value, form: Value) -> (Value, Value) {
        (
            json!({"guid":guid,"targetDeformerGuid":parent,"data":data}),
            json!({"evaluatedForms":[{"data":form}]}),
        )
    }

    fn map_through(deformers: &[(Value, Value)], point: [f64; 2], target: &str) -> [f64; 2] {
        let key = |pair: &(Value, Value)| pair.0["guid"].as_str().unwrap().to_owned();
        let sources = deformers.iter().map(|pair| (key(pair), &pair.0)).collect::<HashMap<_, _>>();
        let forms = deformers.iter().map(|pair| (key(pair), &pair.1)).collect::<HashMap<_, _>>();
        preview_scene()
            .to_canvas(point, Some(target), &sources, &forms, &mut HashSet::new(), 0, &mut Vec::new(), &mut HashMap::new())
            .unwrap()
    }

    #[test]
    fn a_child_warp_interpolates_in_its_grid_mapped_through_the_parent() {
        let root = crate::authoring_hierarchy::ROOT_DEFORMER_GUID;
        let warp = |columns: u64| json!({"type":"Warp","columns":columns,"rows":1});
        // The parent bends at its middle column; the child spans it with a single cell.
        let deformers = [
            deformer("parent", root, warp(2), json!({"positions":[0.0,0.0, 50.0,40.0, 100.0,0.0, 0.0,100.0, 50.0,140.0, 100.0,100.0]})),
            deformer("child", "parent", warp(1), json!({"positions":[0.0,0.0, 1.0,0.0, 0.0,1.0, 1.0,1.0]})),
        ];
        assert_eq!(map_through(&deformers, [0.5, 0.0], "parent"), [50.0, 40.0]);
        // The child's cell has straight edges between its mapped corners, so it does not follow the bend.
        assert_eq!(map_through(&deformers, [0.5, 0.0], "child"), [50.0, 0.0]);
        assert_eq!(map_through(&deformers, [0.25, 0.25], "child"), [25.0, 25.0]);
    }

    #[test]
    fn a_rotation_under_a_warp_turns_with_the_warp_and_takes_the_size_of_the_rotations_above_it() {
        let root = crate::authoring_hierarchy::ROOT_DEFORMER_GUID;
        let rotation = |angle: f64| json!({"angle":angle,"origin":[0.5,0.5],"scale":1.0});
        // The grid is the unit square turned a quarter and stretched: (x, y) goes to (100 - 300 y, 200 x).
        let turned = json!({"positions":[100.0,0.0, 100.0,200.0, -200.0,0.0, -200.0,200.0]});
        let deformers = [
            deformer("warp", root, json!({"type":"Warp","columns":1,"rows":1}), turned),
            deformer("still", "warp", json!({"type":"Rotation","baseAngle":0.0}), rotation(0.0)),
            deformer("turned", "warp", json!({"type":"Rotation","baseAngle":0.0}), rotation(90.0)),
        ];
        assert_eq!(map_through(&deformers, [0.0, 0.0], "still"), [-50.0, 100.0]);
        // Local +y follows the warp to canvas -x, and local +x to canvas +y, both at their own length.
        let close = |got: [f64; 2], want: [f64; 2]| {
            assert!((got[0] - want[0]).abs() < 1e-9 && (got[1] - want[1]).abs() < 1e-9, "{got:?} is not {want:?}");
        };
        close(map_through(&deformers, [0.0, 10.0], "still"), [-60.0, 100.0]);
        close(map_through(&deformers, [10.0, 0.0], "still"), [-50.0, 110.0]);
        close(map_through(&deformers, [10.0, 0.0], "turned"), [-60.0, 100.0]);

        // The lower row leans. The origin (55,55) sits just inside it and the sample 0.1 above lands in the upper
        // row at (50,45), so the deformer turns by the direction (5,10) and not by the lean at its origin.
        let leaning = json!({"positions":[0.0,0.0, 100.0,0.0, 0.0,50.0, 100.0,50.0, 50.0,100.0, 150.0,100.0]});
        let deformers = [
            deformer("warp", root, json!({"type":"Warp","columns":1,"rows":2}), leaning),
            deformer("rotation", "warp", json!({"type":"Rotation","baseAngle":0.0}),
                json!({"angle":0.0,"origin":[0.5,0.55],"scale":1.0})),
        ];
        close(map_through(&deformers, [0.0, 0.0], "rotation"), [55.0, 55.0]);
        close(map_through(&deformers, [0.0, 10.0], "rotation"), [59.47213595499958, 63.94427190999916]);

        // A rotation deformer above the warp halves everything under it, the inner deformer's local pixels included.
        let square = json!({"positions":[0.0,0.0, 100.0,0.0, 0.0,100.0, 100.0,100.0]});
        let deformers = [
            deformer("outer", root, json!({"type":"Rotation","baseAngle":0.0}),
                json!({"angle":0.0,"origin":[0.0,0.0],"scale":0.5})),
            deformer("warp", "outer", json!({"type":"Warp","columns":1,"rows":1}), square),
            deformer("inner", "warp", json!({"type":"Rotation","baseAngle":0.0}), rotation(0.0)),
        ];
        close(map_through(&deformers, [0.0, 0.0], "inner"), [25.0, 25.0]);
        close(map_through(&deformers, [0.0, 10.0], "inner"), [25.0, 30.0]);
        close(map_through(&deformers, [10.0, 0.0], "inner"), [30.0, 25.0]);
    }

    #[test]
    fn native_png_alpha_is_premultiplied_for_the_renderer() {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([200, 100, 50, 128]),
        ))
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
        let original = encoded.into_inner();
        let premultiplied = premultiply_png_texture(original).unwrap();
        let decoded = image::load_from_memory_with_format(&premultiplied, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [100, 50, 25, 128]);
        let mut transparent = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([128, 128, 128, 0]),
        ))
        .write_to(&mut transparent, ImageFormat::Png)
        .unwrap();
        let normalized = premultiply_png_texture(transparent.into_inner()).unwrap();
        let decoded = image::load_from_memory_with_format(&normalized, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }
}
