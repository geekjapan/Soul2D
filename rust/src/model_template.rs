//! Model templates (IO08/IO09, issue #154): the deformer structure and parameters of a finished INX model, written
//! to a file and applied to another model's Parts. Applying maps template Parts to the model's Parts by name or by
//! position, fits the template to them with one uniform scale and a move, copies the structure nodes (Node,
//! GridDeformer, PathDeformer, Composite) with fresh UUIDs, moves each mapped Part under its template parent and
//! carries the parameters over: deformer keys scaled, Part deform keys re-sampled on the model's own mesh.

use crate::bone_deform::{invert, mul_mat, transform_point, world_matrix, Mat4};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub(crate) const FORMAT: &str = "soul2d.model-template.v1";
pub(crate) const MAX_TEMPLATE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEMPLATE_PARTS: usize = 4096;
const STRUCTURE: [&str; 4] = ["Node", "GridDeformer", "PathDeformer", "Composite"];

/// The template of `model`: its node tree without textures and its parameters.
pub(crate) fn export(model: &Value, name: &str, description: &str) -> Result<Value, String> {
    let mut nodes = model.get("nodes").cloned().ok_or("model is missing nodes")?;
    strip_textures(&mut nodes);
    Ok(json!({"format": FORMAT, "name": name, "description": description, "nodes": nodes,
        "param": model.get("param").cloned().unwrap_or(json!([]))}))
}

fn strip_textures(node: &mut Value) {
    if let Some(object) = node.as_object_mut() {
        object.remove("textures");
    }
    for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
        strip_textures(child);
    }
}

/// How `apply` maps and merges.
pub(crate) struct Options {
    /// Map by position (nearest fitted centre) instead of by name.
    pub by_position: bool,
    /// A template parameter whose name the model has adds a renamed copy instead of joining the model's one.
    pub add_parameters: bool,
    /// The model Parts to map; empty means every Part.
    pub parts: Vec<u32>,
    /// Overrides of the fitted scale and move (template world to model world).
    pub scale: Option<f64>,
    pub offset: Option<[f64; 2]>,
}

struct Mesh {
    id: u32,
    name: String,
    parent: Option<u32>,
    centre: [f64; 2],
}

fn meshes(model: &Value, node: &Value, parent: Option<u32>, wanted: &[u32], out: &mut Vec<Mesh>) -> Result<(), String> {
    let id = node["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("a node has no UUID")?;
    if node["type"] == "Part" && (wanted.is_empty() || wanted.contains(&id)) {
        let (low, high) = world_bounds(model, node, id)?;
        out.push(Mesh {
            id,
            name: node["name"].as_str().unwrap_or_default().to_owned(),
            parent,
            centre: [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0],
        });
    }
    for child in node["children"].as_array().into_iter().flatten() {
        meshes(model, child, Some(id), wanted, out)?;
    }
    Ok(())
}

fn world_bounds(model: &Value, node: &Value, id: u32) -> Result<([f64; 2], [f64; 2]), String> {
    let world = world_matrix(model, id)?;
    let (mut low, mut high) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for point in crate::bone_deform::target_vertices(node)? {
        let [x, y, _] = transform_point(world, [point[0], point[1], 0.0])?;
        low = [low[0].min(x), low[1].min(y)];
        high = [high[0].max(x), high[1].max(y)];
    }
    if !low[0].is_finite() {
        return Err(format!("Part {id} has no vertices"));
    }
    Ok((low, high))
}

fn span(meshes: &[&Mesh], model: &Value) -> Result<([f64; 2], [f64; 2]), String> {
    let (mut low, mut high) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for mesh in meshes {
        let node = crate::nodes::find_node(model, mesh.id).ok_or("a mapped Part was not found")?;
        let (l, h) = world_bounds(model, node, mesh.id)?;
        low = [low[0].min(l[0]), low[1].min(l[1])];
        high = [high[0].max(h[0]), high[1].max(h[1])];
    }
    Ok((low, high))
}

/// Applies `template` to `model`, as one edit. Answers the edited model and a report.
pub(crate) fn apply(model: &Value, template: &Value, options: &Options) -> Result<(Value, Value), String> {
    if template["format"] != FORMAT {
        return Err(format!("the file is not a {FORMAT} template"));
    }
    let source = json!({"nodes": template["nodes"].clone(), "param": template["param"].clone()});
    crate::validate_model(&source).map_err(|error| format!("the template is not valid: {error}"))?;
    let mut from = Vec::new();
    meshes(&source, &source["nodes"], None, &[], &mut from)?;
    let mut to = Vec::new();
    meshes(model, model.get("nodes").ok_or("model is missing nodes")?, None, &options.parts, &mut to)?;
    if let Some(missing) = options.parts.iter().find(|id| !to.iter().any(|mesh| mesh.id == **id)) {
        return Err(format!("node {missing} is not a Part"));
    }
    if from.is_empty() || to.is_empty() {
        return Err("the template and the model both need Parts".into());
    }
    // ponytail: each Part's world matrix walks the tree, so the cost grows as Parts × nodes; index the tree if
    // templates outgrow this cap.
    if from.len() > MAX_TEMPLATE_PARTS || to.len() > MAX_TEMPLATE_PARTS {
        return Err(format!("a template applies to at most {MAX_TEMPLATE_PARTS} Parts"));
    }

    // Fit the template's Parts onto the model's: one scale from the areas, then centre on centre.
    let ((from_low, from_high), (to_low, to_high)) = (span(&from.iter().collect::<Vec<_>>(), &source)?,
        span(&to.iter().collect::<Vec<_>>(), model)?);
    let area = |low: [f64; 2], high: [f64; 2]| ((high[0] - low[0]).max(1e-9) * (high[1] - low[1]).max(1e-9)).sqrt();
    let scale = options.scale.unwrap_or(area(to_low, to_high) / area(from_low, from_high));
    if !scale.is_finite() || scale <= 0.0 {
        return Err("scale must be a positive number".into());
    }
    let centre = |low: [f64; 2], high: [f64; 2]| [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0];
    let (from_centre, to_centre) = (centre(from_low, from_high), centre(to_low, to_high));
    let offset = options.offset.unwrap_or([to_centre[0] - scale * from_centre[0], to_centre[1] - scale * from_centre[1]]);
    let fit = |point: [f64; 2]| [scale * point[0] + offset[0], scale * point[1] + offset[1]];

    // Map: same name first (each model Part once), or the nearest fitted centre.
    let mut taken = HashSet::new();
    let mut mapping: Vec<(&Mesh, &Mesh)> = Vec::new();
    for template_part in &from {
        let candidates = to.iter().filter(|part| !taken.contains(&part.id));
        let found = if options.by_position {
            let at = fit(template_part.centre);
            candidates.min_by(|a, b| {
                let distance = |part: &&Mesh| (part.centre[0] - at[0]).hypot(part.centre[1] - at[1]);
                distance(a).total_cmp(&distance(b))
            })
        } else {
            candidates.into_iter().find(|part| part.name == template_part.name)
        };
        if let Some(part) = found {
            taken.insert(part.id);
            mapping.push((template_part, part));
        }
    }

    // Fresh UUIDs for every structure node and parameter of the template.
    let mut used = HashSet::new();
    crate::resource_ids(model, &mut used);
    let mut next = 1u32;
    let mut fresh = || -> Result<u32, String> {
        while used.contains(&next) {
            next = next.checked_add(1).ok_or("no unused UUID remains")?;
        }
        used.insert(next);
        Ok(next)
    };
    let mut renamed: BTreeMap<u32, u32> = BTreeMap::new();
    let root = source["nodes"].clone();
    let mut created = Vec::new();
    let mut structure = Vec::new();
    for child in root["children"].as_array().into_iter().flatten() {
        if let Some(copy) = copy_structure(child, true, scale, offset, &mut fresh, &mut renamed, &mut created)? {
            structure.push(copy);
        }
    }
    let mut candidate = model.clone();
    let root_children = &mut candidate["nodes"]["children"];
    if root_children.is_null() {
        *root_children = json!([]);
    }
    let children = root_children.as_array_mut().ok_or("model root children must be an array")?;
    children.extend(structure);

    // Each mapped Part goes under the copy of its template parent, keeping where it is drawn.
    for (template_part, part) in &mapping {
        if let Some(parent) = template_part.parent.and_then(|parent| renamed.get(&parent)) {
            candidate = crate::nodes::move_nodes(&candidate, &[part.id], *parent, None)?;
        }
    }

    // Parameters: join the model's parameter of the same name, or add one.
    let mut parameters = Vec::new();
    let mut skipped = 0;
    // ponytail: group members join the top level; copy the groups too if templates come to carry them.
    let flat: Vec<&Value> = source["param"].as_array().into_iter().flatten()
        .flat_map(|item| match item["children"].as_array() {
            Some(children) if item.get("groupUUID").is_some() => children.iter().collect(),
            _ => vec![item],
        }).collect();
    for parameter in flat {
        let name = parameter["name"].as_str().unwrap_or_default().to_owned();
        let mut bindings = Vec::new();
        for binding in parameter["bindings"].as_array().into_iter().flatten() {
            let node = binding["node"].as_u64().and_then(|id| u32::try_from(id).ok()).unwrap_or_default();
            if let Some(&copy) = renamed.get(&node) {
                let mut binding = binding.clone();
                binding["node"] = json!(copy);
                scale_binding(&mut binding, scale);
                bindings.push(binding);
            } else if let Some((template_part, part)) = mapping.iter().find(|(template_part, _)| template_part.id == node) {
                bindings.push(part_binding(&source, &candidate, binding, [template_part.id, part.id], scale, offset)?);
            } else {
                skipped += 1;
            }
        }
        if bindings.is_empty() {
            continue;
        }
        if candidate["param"].is_null() {
            candidate["param"] = json!([]);
        }
        let list = candidate["param"].as_array_mut().ok_or("model.param must be an array")?;
        // A parameter of the same name, at the top level or inside a group.
        let same = list.iter_mut()
            .flat_map(|item| match item.get("groupUUID").is_some() {
                true => item["children"].as_array_mut().map_or(Vec::new(), |children| children.iter_mut().collect()),
                false => vec![item],
            })
            .find(|own| own["name"] == name.as_str());
        let taken = same.is_some();
        match same.filter(|own| !options.add_parameters && own["axis_points"] == parameter["axis_points"]
            && own["is_vec2"] == parameter["is_vec2"]) {
            Some(own) => {
                if own["bindings"].is_null() {
                    own["bindings"] = json!([]);
                }
                let own_bindings = own["bindings"].as_array_mut().ok_or("parameter bindings must be an array")?;
                let replaced: HashSet<(String, String)> = bindings.iter()
                    .map(|binding| (binding["node"].to_string(), binding["param_name"].to_string())).collect();
                own_bindings.retain(|kept| !replaced.contains(&(kept["node"].to_string(), kept["param_name"].to_string())));
                own_bindings.extend(bindings);
                parameters.push(json!({"name": name, "id": own["uuid"], "joined": true}));
            }
            None => {
                let mut copy = parameter.clone();
                let id = fresh()?;
                copy["uuid"] = json!(id);
                copy["bindings"] = json!(bindings);
                if taken {
                    let names = parameter_names(list);
                    let renamed = (1..).map(|n| if n == 1 { format!("{name} (Template)") } else { format!("{name} (Template {n})") })
                        .find(|candidate| !names.contains(candidate)).unwrap_or_default();
                    copy["name"] = json!(renamed);
                }
                parameters.push(json!({"name": copy["name"], "id": id, "joined": false}));
                list.push(copy);
            }
        }
    }
    crate::validate_model(&candidate)?;
    let unmapped: Vec<&str> = from.iter().filter(|template_part| !mapping.iter().any(|(mapped, _)| mapped.id == template_part.id))
        .map(|template_part| template_part.name.as_str()).collect();
    let report = json!({
        "scale": scale, "offset": offset, "created": created,
        "mapped": mapping.iter().map(|(template_part, part)| json!({"template": template_part.name, "part": part.id}))
            .collect::<Vec<_>>(),
        "unmapped": unmapped, "parameters": parameters, "skippedBindings": skipped,
    });
    Ok((candidate, report))
}

fn parameter_names(list: &[Value]) -> HashSet<String> {
    list.iter().flat_map(|item| std::iter::once(item).chain(item["children"].as_array().into_iter().flatten()))
        .filter_map(|item| item["name"].as_str().map(str::to_owned)).collect()
}

/// A fitted copy of a structure node and the structure below it; Parts and other drawables are left out.
fn copy_structure(
    node: &Value,
    top: bool,
    scale: f64,
    offset: [f64; 2],
    fresh: &mut impl FnMut() -> Result<u32, String>,
    renamed: &mut BTreeMap<u32, u32>,
    created: &mut Vec<u32>,
) -> Result<Option<Value>, String> {
    if !STRUCTURE.contains(&node["type"].as_str().unwrap_or_default()) {
        return Ok(None);
    }
    let mut copy = node.clone();
    let id = fresh()?;
    if let Some(old) = node["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()) {
        renamed.insert(old, id);
    }
    created.push(id);
    copy["uuid"] = json!(id);
    // A uniform scale about the origin scales every translation; the top level also moves.
    if let Some(trans) = copy.get_mut("transform").and_then(|transform| transform.get_mut("trans"))
        .and_then(Value::as_array_mut) {
        for (axis, value) in trans.iter_mut().take(2).enumerate() {
            let moved = value.as_f64().unwrap_or_default() * scale + if top { offset[axis] } else { 0.0 };
            *value = json!(moved);
        }
    }
    for key in ["grid_axis_x", "grid_axis_y", "vertices"] {
        if let Some(values) = copy.get_mut(key).and_then(Value::as_array_mut) {
            values.iter_mut().for_each(|value| *value = json!(value.as_f64().unwrap_or_default() * scale));
        }
    }
    let mut children = Vec::new();
    for child in node["children"].as_array().into_iter().flatten() {
        if let Some(child) = copy_structure(child, false, scale, offset, fresh, renamed, created)? {
            children.push(child);
        }
    }
    copy["children"] = json!(children);
    Ok(Some(copy))
}

/// Deform offsets and moves scale with the template; rotation, scale and opacity keys do not.
fn scale_binding(binding: &mut Value, scale: f64) {
    let name = binding["param_name"].as_str().unwrap_or_default();
    if !matches!(name, "deform" | "transform.t.x" | "transform.t.y") {
        return;
    }
    fn scaled(value: &mut Value, scale: f64) {
        match value {
            Value::Number(number) => *value = json!(number.as_f64().unwrap_or_default() * scale),
            Value::Array(items) => items.iter_mut().for_each(|item| scaled(item, scale)),
            _ => {}
        }
    }
    scaled(&mut binding["values"], scale);
}

/// A template Part's binding carried to a model Part: deform keys re-sampled at the model Part's vertices through
/// the template mesh, in the model Part's own frame; other keys copied.
fn part_binding(
    source: &Value,
    model: &Value,
    binding: &Value,
    [from, to]: [u32; 2],
    scale: f64,
    offset: [f64; 2],
) -> Result<Value, String> {
    let mut carried = binding.clone();
    carried["node"] = json!(to);
    if binding["param_name"] != "deform" {
        scale_binding(&mut carried, scale);
        return Ok(carried);
    }
    let template_node = crate::nodes::find_node(source, from).ok_or("a template Part was not found")?;
    let node = crate::nodes::find_node(model, to).ok_or("a model Part was not found")?;
    let template_points = crate::bone_deform::target_vertices(template_node)?;
    let indices: Vec<u32> = template_node["mesh"]["indices"].as_array().into_iter().flatten()
        .filter_map(|index| index.as_u64().and_then(|index| u32::try_from(index).ok())).collect();
    let points = crate::bone_deform::target_vertices(node)?;
    // Model Part local -> world -> template world -> template Part local.
    let (template_world, world) = (world_matrix(source, from)?, world_matrix(model, to)?);
    // Model world -> template world undoes the fit.
    let unfit: Mat4 = [[1.0 / scale, 0.0, 0.0, -offset[0] / scale], [0.0, 1.0 / scale, 0.0, -offset[1] / scale],
        [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let to_template = mul_mat(invert(template_world)?, unfit);
    let local_points = points.iter().map(|point| {
        let [x, y, _] = transform_point(world, [point[0], point[1], 0.0])?;
        let [x, y, _] = transform_point(to_template, [x, y, 0.0])?;
        Ok([x, y])
    }).collect::<Result<Vec<_>, String>>()?;
    let back = mul_mat(invert(world)?, mul_mat([[scale, 0.0, 0.0, 0.0], [0.0, scale, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0]], template_world));
    let vector = |offset: [f64; 2]| -> Result<[f64; 2], String> {
        let [x, y, _] = transform_point(back, [offset[0], offset[1], 0.0])?;
        let [ox, oy, _] = transform_point(back, [0.0, 0.0, 0.0])?;
        Ok([x - ox, y - oy])
    };
    let mut values = Vec::new();
    for column in binding["values"].as_array().into_iter().flatten() {
        let mut keys = Vec::new();
        for key in column.as_array().into_iter().flatten() {
            let offsets: Vec<[f64; 2]> = serde_json::from_value(key.clone()).unwrap_or_default();
            let sampled = if offsets.len() == template_points.len() {
                crate::geometry::transfer_mesh_offsets(&local_points, &template_points, &indices, &offsets, false)?
            } else {
                vec![[0.0, 0.0]; points.len()]
            };
            keys.push(json!(sampled.into_iter().map(vector).collect::<Result<Vec<_>, _>>()?));
        }
        values.push(json!(keys));
    }
    carried["values"] = json!(values);
    Ok(carried)
}
