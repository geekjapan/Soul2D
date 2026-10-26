use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_AXIS_POINTS: usize = 256;

#[derive(Clone, Debug)]
struct NodeTransform {
    translation: [f64; 3],
    rotation: [f64; 3],
    scale: [f64; 2],
    scale_multiplier: [f64; 2],
}

#[derive(Clone, Copy)]
struct AxisCell {
    left: usize,
    offset: f64,
}

pub(crate) fn evaluate(model: &Value, values: &BTreeMap<u32, [f64; 2]>) -> Result<Value, String> {
    let linked = resolve_links(model, values)?;
    let values = &*linked;
    if let Some(automation) = model.get("automation").filter(|value| !value.is_null()) {
        let automation = automation
            .as_array()
            .ok_or_else(|| "model.automation must be an array".to_string())?;
        if !automation.is_empty() {
            return Err("model.automation is unsupported".to_string());
        }
    }
    let mut output = model.clone();
    let mut transforms = HashMap::new();
    let mut offsets: BTreeMap<(u32, String), f64> = BTreeMap::new();
    let root = output
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    collect_transforms(root, &mut transforms)?;

    let parameter_groups = output
        .get("param")
        .and_then(Value::as_array)
        .ok_or_else(|| "model.param must be an array".to_string())?;
    let mut parameters = Vec::new();
    for parameter in parameter_groups {
        collect_parameters(parameter, &mut parameters)?;
    }
    let mut known_parameters = HashSet::new();
    for parameter in &parameters {
        let id = required_u32(parameter, "uuid", "parameter")?;
        if !known_parameters.insert(id) {
            return Err(format!("duplicate parameter id {id}"));
        }
    }
    for id in values.keys() {
        if !known_parameters.contains(id) {
            return Err(format!("runtime value refers to unknown parameter {id}"));
        }
    }

    for parameter in &parameters {
        let id = required_u32(parameter, "uuid", "parameter")?;
        let runtime = values.get(&id).copied();
        if let Some(pair) = runtime {
            if !pair[0].is_finite() || !pair[1].is_finite() {
                return Err(format!("parameter {id} runtime value is not finite"));
            }
        }
        let defaults = vec2_field(parameter, "defaults", [0.0, 0.0])?;
        let parameter_value = runtime.unwrap_or(defaults);
        let is_vec2 = parameter
            .get("is_vec2")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let axes = parameter_axes(parameter, is_vec2)?;
        let normalized = normalized_parameter_value(parameter, parameter_value, is_vec2)?;
        let cell_x = find_axis_cell(&axes[0], normalized[0])?;
        let cell_y = if is_vec2 {
            Some(find_axis_cell(&axes[1], normalized[1])?)
        } else {
            None
        };

        let bindings = parameter
            .get("bindings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for binding in bindings {
            if link_axis(binding).is_some() {
                continue;
            }
            let node_id = required_u32(binding, "node", "binding")?;
            let transform = transforms
                .get_mut(&node_id)
                .ok_or_else(|| format!("parameter {id} binding targets missing node {node_id}"))?;
            let channel = binding
                .get("param_name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("parameter {id} binding param_name must be a string"))?;

            // Bone apply stores per-vertex offsets on "deform". They are not
            // added to TRS, and this pass does not push them onto the mesh.
            if channel == "deform" {
                continue;
            }
            // Value channels that scale or shift one node field after the transforms: physics settings, opacity
            // (neutral 1, so bindings multiply) and zSort (neutral 0, so bindings add), as `channel_default` states.
            let offset_channel = matches!(channel,
                "zSort" | "opacity" | "gravity" | "length" | "frequency" | "angleDamping" | "lengthDamping"
                    | "outputScale.x" | "outputScale.y")
                && crate::parameter_flip::node_has_param(
                    crate::nodes::find_node(model, node_id)
                        .and_then(|node| node.get("type")).and_then(Value::as_str).unwrap_or("Node"),
                    channel);
            // D Node.hasParam accepts these on every node. setValue adds translation
            // and rotation, including pitch, yaw, and Z. Scale stays multiplicative.
            if !matches!(
                channel,
                "transform.t.x"
                    | "transform.t.y"
                    | "transform.t.z"
                    | "transform.r.x"
                    | "transform.r.y"
                    | "transform.r.z"
                    | "transform.s.x"
                    | "transform.s.y"
            ) && !offset_channel {
                if binding_has_values(binding)? {
                    return Err(format!(
                        "parameter {id} binding channel {channel:?} is unsupported"
                    ));
                }
                continue;
            }

            let mode = binding
                .get("interpolate_mode")
                .and_then(Value::as_str)
                .unwrap_or("Linear");
            if !mode.eq_ignore_ascii_case("linear") {
                return Err(format!(
                    "parameter {id} binding channel {channel:?} uses unsupported interpolation {mode:?}"
                ));
            }
            let grid = binding_grid(binding, &axes, is_vec2, &format!("parameter {id} binding"))?;
            let amount = interpolate_grid(&grid, cell_x, cell_y);
            if !amount.is_finite() {
                return Err(format!(
                    "parameter {id} binding evaluated to a non-finite value"
                ));
            }
            if offset_channel {
                let additive = matches!(channel, "length" | "zSort");
                let offset = offsets.entry((node_id, channel.to_owned()))
                    .or_insert(if additive { 0.0 } else { 1.0 });
                if additive {
                    *offset += amount;
                } else {
                    *offset *= amount;
                }
                continue;
            }
            match channel {
                "transform.t.x" => transform.translation[0] += amount,
                "transform.t.y" => transform.translation[1] += amount,
                "transform.t.z" => transform.translation[2] += amount,
                "transform.r.x" => transform.rotation[0] += amount,
                "transform.r.y" => transform.rotation[1] += amount,
                "transform.r.z" => transform.rotation[2] += amount,
                "transform.s.x" => {
                    if !transform.scale_multiplier[0].is_finite()
                        || transform.scale_multiplier[0] == 0.0
                    {
                        transform.scale_multiplier[0] = 1.0;
                    }
                    transform.scale_multiplier[0] *= amount;
                }
                "transform.s.y" => {
                    if !transform.scale_multiplier[1].is_finite()
                        || transform.scale_multiplier[1] == 0.0
                    {
                        transform.scale_multiplier[1] = 1.0;
                    }
                    transform.scale_multiplier[1] *= amount;
                }
                _ => unreachable!(),
            }
        }
    }

    for transform in transforms.values_mut() {
        transform.scale[0] *= transform.scale_multiplier[0];
        transform.scale[1] *= transform.scale_multiplier[1];
    }
    if transforms.values().any(|t| {
        t.translation
            .iter()
            .chain(t.rotation.iter())
            .chain(t.scale.iter())
            .any(|v| !v.is_finite())
    }) {
        return Err("evaluated transform is not finite".to_string());
    }
    write_transforms(
        output
            .get_mut("nodes")
            .ok_or_else(|| "model.nodes is missing".to_string())?,
        &transforms,
    )?;
    for ((id, channel), offset) in offsets {
        let node = crate::nodes::find_node_mut(&mut output, id)
            .ok_or_else(|| format!("binding targets missing node {id}"))?;
        if channel.starts_with("outputScale.") {
            let mut scale = vec2_field(node, "output_scale", [1.0, 1.0])?;
            let axis = usize::from(channel == "outputScale.y");
            scale[axis] *= offset;
            if !scale[axis].is_finite() {
                return Err("evaluated physics output scale is not finite".into());
            }
            node["output_scale"] = json!(scale);
        } else {
            let (field, default) = match channel.as_str() {
                "opacity" => ("opacity", 1.0),
                "zSort" => ("zsort", 0.0),
                "gravity" => ("gravity", 1.0),
                "length" => ("length", 100.0),
                "frequency" => ("frequency", 1.0),
                "angleDamping" => ("angle_damping", 0.5),
                "lengthDamping" => ("length_damping", 0.5),
                _ => unreachable!(),
            };
            let base = match node.get(field) {
                Some(value) => finite_number(value, field)?,
                None => default,
            };
            let value = if matches!(field, "length" | "zsort") { base + offset } else { base * offset };
            if !value.is_finite() {
                return Err(format!("evaluated {field} is not finite"));
            }
            node[field] = json!(value);
        }
    }
    Ok(output)
}

/// Offsets for the headless raster. The bracket matches TRS evaluation.
/// `Nearest`, `Linear`, and `Cubic` follow `ParameterBindingImpl.interpolate`.
/// Empty cells contribute nothing. Cells whose lengths differ fail closed.
pub(crate) fn raster_deformations(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
) -> Result<BTreeMap<u32, Vec<[f64; 2]>>, String> {
    let linked = resolve_links(model, values)?;
    let values = &*linked;
    let parameter_groups = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or_else(|| "model.param must be an array".to_string())?;
    let mut parameters = Vec::new();
    for parameter in parameter_groups {
        collect_parameters(parameter, &mut parameters)?;
    }
    let mut combined = BTreeMap::new();
    for parameter in &parameters {
        let id = required_u32(parameter, "uuid", "parameter")?;
        let defaults = vec2_field(parameter, "defaults", [0.0, 0.0])?;
        let parameter_value = values.get(&id).copied().unwrap_or(defaults);
        if !parameter_value[0].is_finite() || !parameter_value[1].is_finite() {
            return Err(format!("parameter {id} runtime value is not finite"));
        }
        let is_vec2 = parameter
            .get("is_vec2")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let axes = parameter_axes(parameter, is_vec2)?;
        let normalized = normalized_parameter_value(parameter, parameter_value, is_vec2)?;
        let cell_x = find_axis_cell(&axes[0], normalized[0])?;
        let cell_y = if is_vec2 {
            Some(find_axis_cell(&axes[1], normalized[1])?)
        } else {
            None
        };
        let bindings = parameter
            .get("bindings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for binding in bindings {
            if binding.get("param_name").and_then(Value::as_str) != Some("deform") {
                continue;
            }
            let node_id = required_u32(binding, "node", &format!("parameter {id} binding"))?;
            let mut offsets = interpolate_deform(binding, &axes, is_vec2, cell_x, cell_y, id, parameter_repeats(parameter))?;
            if offsets.is_empty() {
                continue;
            }
            let limit = weight_limit(binding, values, &parameters)?;
            if limit < 1.0 {
                offsets.iter_mut().for_each(|offset| *offset = [offset[0] * limit, offset[1] * limit]);
            }
            match combined.get(&node_id) {
                None => {
                    combined.insert(node_id, offsets);
                }
                Some(existing) if existing.len() == offsets.len() => {
                    let summed = existing
                        .iter()
                        .zip(offsets)
                        .map(|(base, extra)| [base[0] + extra[0], base[1] + extra[1]])
                        .collect();
                    combined.insert(node_id, summed);
                }
                Some(_) => {
                    return Err(format!(
                        "parameter {id} deform offset length mismatch"
                    ));
                }
            }
        }
    }
    Ok(combined)
}

/// Blend-shape weight limits (issue #185): a deform binding may hold `weightLimits`, a list of
/// `{parameter, points: [[value, weight], ...]}` graphs over another parameter's first axis. The binding's offsets
/// are scaled by the smallest weight the graphs give at the current values, as Cubism applies the smallest of
/// several limits. Between points the weight is linear; outside them it is the nearest point's.
pub(crate) fn weight_limit(
    binding: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    parameters: &[&Value],
) -> Result<f64, String> {
    let Some(limits) = binding.get("weightLimits") else {
        return Ok(1.0);
    };
    let mut smallest: f64 = 1.0;
    for limit in limits.as_array().ok_or("weightLimits must be an array")? {
        let driver = required_u32(limit, "parameter", "weight limit")?;
        let points = parse_weight_points(limit.get("points"))?;
        let value = match values.get(&driver) {
            Some(value) => value[0],
            None => {
                let parameter = parameters
                    .iter()
                    .find(|parameter| parameter.get("uuid").and_then(Value::as_u64) == Some(u64::from(driver)))
                    .ok_or_else(|| format!("weight limit parameter {driver} was not found"))?;
                vec2_field(parameter, "defaults", [0.0, 0.0])?[0]
            }
        };
        let weight = match points.iter().position(|point| point[0] >= value) {
            None => points[points.len() - 1][1],
            Some(0) => points[0][1],
            Some(index) => {
                let ([x0, y0], [x1, y1]) = (points[index - 1], points[index]);
                y0 + (y1 - y0) * (value - x0) / (x1 - x0)
            }
        };
        smallest = smallest.min(weight);
    }
    Ok(smallest)
}

/// 2 to 5 `[value, weight]` points with strictly rising values and weights in 0..1, as the manual's custom graph.
pub(crate) fn parse_weight_points(points: Option<&Value>) -> Result<Vec<[f64; 2]>, String> {
    let points: Vec<[f64; 2]> = serde_json::from_value(points.cloned().unwrap_or(Value::Null))
        .map_err(|_| "weight limit points must be [value, weight] pairs".to_string())?;
    if !(2..=5).contains(&points.len()) {
        return Err("a weight limit needs 2 to 5 points".into());
    }
    if points.iter().any(|[value, weight]| !value.is_finite() || !(0.0..=1.0).contains(weight)) {
        return Err("weight limit values must be finite and weights in 0..1".into());
    }
    if points.windows(2).any(|pair| pair[1][0] <= pair[0][0]) {
        return Err("weight limit point values must rise".into());
    }
    Ok(points)
}

pub(crate) fn overlay_preview(
    deformations: &mut BTreeMap<u32, Vec<[f64; 2]>>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
) {
    for (id, offsets) in preview {
        deformations.insert(*id, offsets.clone());
    }
}

pub(crate) fn set_trs(model: &mut Value, request: &Value) -> Result<Value, String> {
    let parameter_id = required_u32(request, "parameter", "request")?;
    let target_ids = request
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "request.nodes must be an array".to_string())?
        .iter()
        .map(|id| {
            id.as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| "request.nodes must contain unsigned 32-bit ids".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if target_ids.is_empty() {
        return Err("request.nodes must not be empty".to_string());
    }
    let mut unique_ids = HashSet::new();
    if target_ids.iter().any(|id| !unique_ids.insert(*id)) {
        return Err("request.nodes contains a duplicate id".to_string());
    }
    let key = index_pair(request.get("key"), "request.key")?;
    let mut edits = Vec::new();
    if let Some(value) = request.get("translation") {
        let pair = array2(Some(value), "request.translation")?;
        edits.push(("transform.t.x", pair[0], 0.0));
        edits.push(("transform.t.y", pair[1], 0.0));
    }
    if let Some(value) = request.get("scale") {
        let pair = array2(Some(value), "request.scale")?;
        edits.push(("transform.s.x", pair[0], 1.0));
        edits.push(("transform.s.y", pair[1], 1.0));
    }
    if let Some(value) = request.get("translation_z") {
        let z = finite_number(value, "request.translation_z")?;
        edits.push(("transform.t.z", z, 0.0));
    }
    if let Some(value) = request.get("rotation_degrees") {
        let degrees = finite_number(value, "request.rotation_degrees")?;
        edits.push(("transform.r.z", degrees.to_radians(), 0.0));
    }
    if let Some(value) = request.get("rotation_x") {
        let radians = finite_number(value, "request.rotation_x")?;
        edits.push(("transform.r.x", radians, 0.0));
    }
    if let Some(value) = request.get("rotation_y") {
        let radians = finite_number(value, "request.rotation_y")?;
        edits.push(("transform.r.y", radians, 0.0));
    }
    if edits.is_empty() {
        return Err(
            "request must include translation, translation_z, scale, rotation_degrees, rotation_x, or rotation_y"
                .to_string(),
        );
    }

    let mut output = model.clone();
    let root = output
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    let node_ids = collect_node_ids(root)?;
    for id in &target_ids {
        if !node_ids.contains(id) {
            return Err(format!("request targets missing node {id}"));
        }
    }
    let params = output
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "model.param must be an array".to_string())?;
    let parameter = find_parameter_mut(params, parameter_id)
        .ok_or_else(|| format!("request refers to unknown parameter {parameter_id}"))?;
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let axes = parameter_axes(parameter, is_vec2)?;
    let coordinates = parameter_physical_axes(parameter, &axes, is_vec2)?;
    let repeat = parameter_repeats(parameter);
    let x = key[0];
    let y = key[1];
    if x >= axes[0].len() || y >= axes[1].len() || (!is_vec2 && y != 0) {
        return Err("request.key must identify an existing parameter keypoint index".to_string());
    }
    let bindings = parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "parameter.bindings must be an array".to_string())?;
    for node_id in &target_ids {
        for &(channel, value, default) in &edits {
            let matches = bindings
                .iter()
                .enumerate()
                .filter(|(_, binding)| {
                    binding.get("node").and_then(Value::as_u64) == Some(u64::from(*node_id))
                        && binding.get("param_name").and_then(Value::as_str) == Some(channel)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(format!("node {node_id} has duplicate {channel} bindings"));
            }
            let binding = if let Some(index) = matches.first() {
                &mut bindings[*index]
            } else {
                let mut created =
                    new_binding(*node_id, channel, default, &axes, &coordinates, is_vec2)?;
                bindings.push(created.take());
                bindings.last_mut().expect("binding was just appended")
            };
            if let Some(existing) = binding.get("interpolate_mode").and_then(Value::as_str) {
                if !existing.eq_ignore_ascii_case("linear") {
                    return Err(format!(
                        "node {node_id} {channel} binding uses unsupported interpolation {existing:?}"
                    ));
                }
            }
            let mut grid = binding_grid(binding, &axes, is_vec2, "binding")?;
            let mut flags = binding_flags(binding, &axes, is_vec2, "binding")?;
            let (gx, gy) = if is_vec2 { (x, y) } else { (x, 0) };
            grid[gx][gy] = value;
            flags[gx][gy] = true;
            reinterpolate(&mut grid, &flags, &axes[0], &axes[1], repeat)?;
            write_binding_grid(binding, &grid, &flags)?;
        }
    }

    *model = output;
    Ok(json!({
        "updated": target_ids,
        "parameter": parameter_id,
        "key": [x, y],
    }))
}

/// Writes `(node, channel, value)` cells into one keypoint of a parameter and creates missing bindings, the way
/// `set_trs` does per node. `key` must name an existing keypoint (checked even when `cells` is empty).
pub(crate) fn set_pose_cells(
    model: &mut Value,
    parameter_id: u32,
    key: [usize; 2],
    cells: &[(u32, &str, f64)],
) -> Result<(), String> {
    let params = model
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "model.param must be an array".to_string())?;
    let parameter = find_parameter_mut(params, parameter_id)
        .ok_or_else(|| format!("request refers to unknown parameter {parameter_id}"))?;
    let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    let axes = parameter_axes(parameter, is_vec2)?;
    if key[0] >= axes[0].len() || key[1] >= axes[1].len() || (!is_vec2 && key[1] != 0) {
        return Err("key must identify an existing parameter keypoint index".to_string());
    }
    for &(node, channel, value) in cells {
        upsert_linear_binding(parameter, node, channel, &[(key[0], key[1], value)], 0.0)?;
    }
    Ok(())
}

/// The parameter value of every keypoint along each axis (`min + (max - min) * axis point`), the values an animation
/// lane must hold to land exactly on a keypoint.
pub(crate) fn parameter_key_values(parameter: &Value) -> Result<[Vec<f64>; 2], String> {
    let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    let axes = parameter_axes(parameter, is_vec2)?;
    parameter_physical_axes(parameter, &axes, is_vec2)
}

/// `[min, max]` of the parameter along `axis`.
pub(crate) fn parameter_axis_range(parameter: &Value, axis: usize) -> Result<[f64; 2], String> {
    Ok([vec2_field(parameter, "min", [0.0, 0.0])?[axis], vec2_field(parameter, "max", [1.0, 1.0])?[axis]])
}

fn collect_transforms(
    root: &Value,
    output: &mut HashMap<u32, NodeTransform>,
) -> Result<(), String> {
    let id = required_u32(root, "uuid", "node")?;
    let transform = parse_transform(root.get("transform"), id)?;
    if output.insert(id, transform).is_some() {
        return Err(format!("duplicate node id {id}"));
    }
    if let Some(children) = root.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| format!("node {id}.children must be an array"))?;
        for child in children {
            collect_transforms(child, output)?;
        }
    }
    Ok(())
}

fn collect_node_ids(root: &Value) -> Result<HashSet<u32>, String> {
    fn visit(node: &Value, ids: &mut HashSet<u32>) -> Result<(), String> {
        let id = required_u32(node, "uuid", "node")?;
        if !ids.insert(id) {
            return Err(format!("duplicate node id {id}"));
        }
        if let Some(children) = node.get("children") {
            let children = children
                .as_array()
                .ok_or_else(|| format!("node {id}.children must be an array"))?;
            for child in children {
                visit(child, ids)?;
            }
        }
        Ok(())
    }
    let mut ids = HashSet::new();
    visit(root, &mut ids)?;
    Ok(ids)
}

/// The axis a parameter-link binding drives: 0 for X, 1 for Y. The engine writes "X" / "Y" and still reads
/// the older integer form, so both count.
pub(crate) fn link_axis(binding: &Value) -> Option<usize> {
    match binding.get("param_name")? {
        Value::String(name) => match name.as_str() {
            "X" => Some(0),
            "Y" => Some(1),
            _ => None,
        },
        Value::Number(number) => match number.as_u64() {
            Some(0) => Some(0),
            Some(1) => Some(1),
            _ => None,
        },
        _ => None,
    }
}

/// `ParameterParameterBinding.applyToTarget`: a driver parameter forces one axis of its target to the value
/// its link binding holds at the driver's value. Drivers run in document order until nothing changes, which
/// settles chains whatever their order.
fn resolve_links<'a>(
    model: &Value,
    values: &'a BTreeMap<u32, [f64; 2]>,
) -> Result<std::borrow::Cow<'a, BTreeMap<u32, [f64; 2]>>, String> {
    let Some(list) = model.get("param").and_then(Value::as_array) else {
        return Ok(std::borrow::Cow::Borrowed(values));
    };
    let mut parameters = Vec::new();
    for parameter in list {
        collect_parameters(parameter, &mut parameters)?;
    }
    let bindings_of = |parameter: &Value| -> Vec<Value> {
        parameter
            .get("bindings")
            .and_then(Value::as_array)
            .map(|bindings| bindings.iter().filter(|b| link_axis(b).is_some()).cloned().collect())
            .unwrap_or_default()
    };
    if !parameters.iter().any(|parameter| !bindings_of(parameter).is_empty()) {
        return Ok(std::borrow::Cow::Borrowed(values));
    }
    let mut defaults = HashMap::new();
    for parameter in &parameters {
        defaults.insert(required_u32(parameter, "uuid", "parameter")?, vec2_field(parameter, "defaults", [0.0, 0.0])?);
    }
    let mut out = values.clone();
    for _ in 0..=parameters.len() {
        let before = out.clone();
        for parameter in &parameters {
            let id = required_u32(parameter, "uuid", "parameter")?;
            let own = out.get(&id).copied().unwrap_or(defaults[&id]);
            for binding in bindings_of(parameter) {
                let target = required_u32(&binding, "node", "binding")?;
                let (Some(axis), Some(target_default)) = (link_axis(&binding), defaults.get(&target)) else {
                    continue;
                };
                let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
                let axes = crate::binding_grid::Axes::of(parameter)?;
                let normalized = normalized_parameter_value(parameter, own, is_vec2)?;
                let (left, offset) = axes.find_offset(normalized);
                let parsed = crate::binding_grid::Binding::parse(&binding, &axes, target_default[axis], None)?;
                let value = parsed.interpolate(&axes, left, offset)?[0];
                if !value.is_finite() {
                    return Err(format!("parameter {id} link evaluated to a non-finite value"));
                }
                out.entry(target).or_insert(*target_default)[axis] = value;
            }
        }
        if out == before {
            break;
        }
    }
    Ok(std::borrow::Cow::Owned(out))
}

/// Every stored deform form of each node: one offset list per set key of each of its `deform` bindings.
pub(crate) fn deform_key_forms(model: &Value) -> Result<BTreeMap<u32, Vec<Vec<[f64; 2]>>>, String> {
    let mut parameters = Vec::new();
    for parameter in model.get("param").and_then(Value::as_array).into_iter().flatten() {
        collect_parameters(parameter, &mut parameters)?;
    }
    let mut forms: BTreeMap<u32, Vec<Vec<[f64; 2]>>> = BTreeMap::new();
    for parameter in parameters {
        let id = required_u32(parameter, "uuid", "parameter")?;
        for binding in parameter.get("bindings").and_then(Value::as_array).into_iter().flatten() {
            if binding.get("param_name").and_then(Value::as_str) != Some("deform") {
                continue;
            }
            let node = required_u32(binding, "node", &format!("parameter {id} binding"))?;
            let columns = binding.get("values").and_then(Value::as_array).into_iter().flatten();
            for cell in columns.flat_map(|column| column.as_array().into_iter().flatten()) {
                if cell.as_array().is_some_and(|points| !points.is_empty()) {
                    forms.entry(node).or_default().push(parse_offset_cell(cell, id)?);
                }
            }
        }
    }
    Ok(forms)
}

fn collect_parameters<'a>(value: &'a Value, output: &mut Vec<&'a Value>) -> Result<(), String> {
    if value.get("groupUUID").is_some() {
        let children = value
            .get("children")
            .and_then(Value::as_array)
            .ok_or_else(|| "parameter group children must be an array".to_string())?;
        for child in children {
            collect_parameters(child, output)?;
        }
    } else {
        output.push(value);
    }
    Ok(())
}

fn find_parameter_mut(values: &mut [Value], id: u32) -> Option<&mut Value> {
    for value in values {
        if value.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
            return Some(value);
        }
        if value.get("groupUUID").is_some() {
            if let Some(children) = value.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = find_parameter_mut(children, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn write_transforms(
    root: &mut Value,
    transforms: &HashMap<u32, NodeTransform>,
) -> Result<(), String> {
    let id = required_u32(root, "uuid", "node")?;
    let state = transforms
        .get(&id)
        .ok_or_else(|| format!("node {id} transform state is missing"))?;
    let transform = root
        .get_mut("transform")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("node {id}.transform must be an object"))?;
    set_array(transform, "trans", &state.translation)?;
    set_array(transform, "rot", &state.rotation)?;
    set_array(transform, "scale", &state.scale)?;
    if let Some(children) = root.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            write_transforms(child, transforms)?;
        }
    }
    Ok(())
}

fn parse_transform(value: Option<&Value>, id: u32) -> Result<NodeTransform, String> {
    let defaults = NodeTransform {
        translation: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0],
        scale: [1.0, 1.0],
        scale_multiplier: [1.0, 1.0],
    };
    let Some(value) = value else {
        return Ok(defaults);
    };
    if !value.is_object() {
        return Err(format!("node {id}.transform must be an object"));
    }
    Ok(NodeTransform {
        translation: array_with_defaults(
            value.get("trans"),
            defaults.translation,
            &format!("node {id}.transform.trans"),
        )?,
        rotation: array_with_defaults(
            value.get("rot"),
            defaults.rotation,
            &format!("node {id}.transform.rot"),
        )?,
        scale: array_with_defaults(
            value.get("scale"),
            defaults.scale,
            &format!("node {id}.transform.scale"),
        )?,
        scale_multiplier: defaults.scale_multiplier,
    })
}

fn array_with_defaults<const N: usize>(
    value: Option<&Value>,
    mut default: [f64; N],
    path: &str,
) -> Result<[f64; N], String> {
    let Some(value) = value else {
        return Ok(default);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if array.len() > N {
        return Err(format!("{path} has too many components"));
    }
    for (index, component) in array.iter().enumerate() {
        default[index] = finite_number(component, path)?;
    }
    Ok(default)
}

fn set_array<const N: usize>(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    values: &[f64; N],
) -> Result<(), String> {
    object.insert(
        key.to_string(),
        Value::Array(
            values
                .iter()
                .map(|value| {
                    serde_json::Number::from_f64(*value)
                        .map(Value::Number)
                        .ok_or_else(|| format!("transform.{key} is not finite"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    );
    Ok(())
}

fn parameter_axes(parameter: &Value, is_vec2: bool) -> Result<[Vec<f64>; 2], String> {
    let raw = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or_else(|| "parameter.axis_points must contain two arrays".to_string())?;
    if raw.len() != 2 {
        return Err("parameter.axis_points must contain two arrays".to_string());
    }
    let x = axis_values(&raw[0], "parameter.axis_points[0]")?;
    let y = axis_values(&raw[1], "parameter.axis_points[1]")?;
    if x.len() > MAX_AXIS_POINTS
        || y.len() > MAX_AXIS_POINTS
        || x.len().saturating_mul(y.len()) > MAX_AXIS_POINTS * MAX_AXIS_POINTS
    {
        return Err("parameter axis grid exceeds the supported size".to_string());
    }
    if is_vec2 && y.len() < 2 {
        return Err("2D parameter needs at least two Y axis points".to_string());
    }
    Ok([x, y])
}

fn axis_values(value: &Value, path: &str) -> Result<Vec<f64>, String> {
    let axis = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if axis.len() < 2 && path.ends_with("[0]") || axis.is_empty() {
        return Err(format!("{path} must have axis points"));
    }
    let values = axis
        .iter()
        .map(|value| finite_number(value, path))
        .collect::<Result<Vec<_>, _>>()?;
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!("{path} must be strictly increasing"));
    }
    Ok(values)
}

fn parameter_physical_axes(
    parameter: &Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
) -> Result<[Vec<f64>; 2], String> {
    let min = vec2_field(parameter, "min", [0.0, 0.0])?;
    let max = vec2_field(parameter, "max", [1.0, 1.0])?;
    let mut output = [Vec::new(), Vec::new()];
    for axis in 0..2 {
        if !min[axis].is_finite()
            || !max[axis].is_finite()
            || max[axis] < min[axis]
            || (max[axis] == min[axis] && (is_vec2 || axis == 0))
        {
            return Err(format!("parameter range on axis {axis} is invalid"));
        }
        output[axis] = axes[axis]
            .iter()
            .map(|point| min[axis] + (max[axis] - min[axis]) * point)
            .collect();
    }
    Ok(output)
}

/// Whether the parameter is a repeating 1D one. A 2D parameter never repeats, whatever its record says.
pub(crate) fn parameter_repeats(parameter: &Value) -> bool {
    parameter.get("repeat").and_then(Value::as_bool).unwrap_or(false)
        && !parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false)
}

/// Where `value` lies on a repeating axis, from 0 up to but not including 1. The value is reduced by the span before it
/// is divided, as `value.set` reduces it: a preview can hand over a lane value far past the range, and dividing first
/// would round its place away or overflow on a narrow range.
pub(crate) fn repeat_position(value: f64, min: f64, max: f64) -> f64 {
    let span = max - min;
    ((value - min).rem_euclid(span) / span).rem_euclid(1.0)
}

fn normalized_parameter_value(
    parameter: &Value,
    value: [f64; 2],
    is_vec2: bool,
) -> Result<[f64; 2], String> {
    let min = vec2_field(parameter, "min", [0.0, 0.0])?;
    let max = vec2_field(parameter, "max", [1.0, 1.0])?;
    let normalize = |axis: usize| -> Result<f64, String> {
        if max[axis] <= min[axis] {
            return Err(format!("parameter range on axis {axis} is invalid"));
        }
        // A repeating parameter runs on a circle: its right end is the same point as its left end.
        Ok(if axis == 0 && parameter_repeats(parameter) {
            repeat_position(value[axis], min[axis], max[axis])
        } else {
            ((value[axis] - min[axis]) / (max[axis] - min[axis])).clamp(0.0, 1.0)
        })
    };
    Ok([normalize(0)?, if is_vec2 { normalize(1)? } else { 0.0 }])
}

fn find_axis_cell(axis: &[f64], position: f64) -> Result<AxisCell, String> {
    if axis.len() < 2 {
        return Err("linear interpolation needs at least two axis points".to_string());
    }
    let left = axis
        .windows(2)
        .position(|pair| pair[1] > position)
        .unwrap_or(axis.len() - 2);
    let offset = (position - axis[left]) / (axis[left + 1] - axis[left]);
    Ok(AxisCell { left, offset })
}

fn binding_grid(
    binding: &Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    path: &str,
) -> Result<Vec<Vec<f64>>, String> {
    let raw = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path}.values must be an array"))?;
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    if raw.len() != axes[0].len() {
        return Err(format!("{path}.values has a mismatched X axis"));
    }
    raw.iter()
        .map(|column| {
            let column = column
                .as_array()
                .ok_or_else(|| format!("{path}.values columns must be arrays"))?;
            if column.len() != y_count {
                return Err(format!("{path}.values has a mismatched Y axis"));
            }
            column
                .iter()
                .map(|value| finite_number(value, &format!("{path}.values")))
                .collect()
        })
        .collect()
}

fn binding_flags(
    binding: &Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    path: &str,
) -> Result<Vec<Vec<bool>>, String> {
    let raw = binding
        .get("isSet")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path}.isSet must be an array"))?;
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    if raw.len() != axes[0].len() {
        return Err(format!("{path}.isSet has a mismatched X axis"));
    }
    raw.iter()
        .map(|column| {
            let column = column
                .as_array()
                .ok_or_else(|| format!("{path}.isSet columns must be arrays"))?;
            if column.len() != y_count {
                return Err(format!("{path}.isSet has a mismatched Y axis"));
            }
            column
                .iter()
                .map(|value| match value {
                    Value::Bool(set) => Ok(*set),
                    Value::Null => Ok(false),
                    _ => Err(format!("{path}.isSet entries must be booleans or null")),
                })
                .collect()
        })
        .collect()
}

fn binding_has_values(binding: &Value) -> Result<bool, String> {
    let Some(flags) = binding.get("isSet").and_then(Value::as_array) else {
        return Ok(binding.get("values").is_some());
    };
    for column in flags {
        let Some(column) = column.as_array() else {
            return Err("binding.isSet columns must be arrays".to_string());
        };
        for flag in column {
            if flag.as_bool() == Some(true) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn interpolate_grid(grid: &[Vec<f64>], x: AxisCell, y: Option<AxisCell>) -> f64 {
    let Some(y) = y else {
        return lerp(grid[x.left][0], grid[x.left + 1][0], x.offset);
    };
    let lower = lerp(grid[x.left][y.left], grid[x.left][y.left + 1], y.offset);
    let upper = lerp(
        grid[x.left + 1][y.left],
        grid[x.left + 1][y.left + 1],
        y.offset,
    );
    lerp(lower, upper, x.offset)
}

fn write_binding_grid(
    binding: &mut Value,
    grid: &[Vec<f64>],
    flags: &[Vec<bool>],
) -> Result<(), String> {
    let values = grid
        .iter()
        .map(|column| {
            column
                .iter()
                .map(|value| number(*value, "binding value"))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        })
        .collect::<Result<Vec<_>, _>>()?;
    binding["values"] = Value::Array(values);
    binding["isSet"] = json!(flags);
    Ok(())
}

fn new_binding(
    node_id: u32,
    channel: &str,
    default: f64,
    axes: &[Vec<f64>; 2],
    physical_axes: &[Vec<f64>; 2],
    is_vec2: bool,
) -> Result<Value, String> {
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    let values = vec![vec![default; y_count]; axes[0].len()];
    let mut flags = vec![vec![false; y_count]; axes[0].len()];
    let nearest_zero = |axis: &[f64]| -> usize {
        axis.iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    let zx = nearest_zero(&physical_axes[0]);
    let zy = nearest_zero(&physical_axes[1]);
    if physical_axes[0][zx].abs() < 0.001 && physical_axes[1][zy].abs() < 0.001 {
        flags[zx][zy.min(y_count - 1)] = true;
    }
    Ok(json!({
        "node": node_id,
        "param_name": channel,
        "values": values,
        "isSet": flags,
        "interpolate_mode": "Linear"
    }))
}

/// Writes `cells` into the binding of `channel` on `node_id`, creating it when missing. A cell already set within
/// `tolerance` of the new value is left alone; returns whether anything changed.
pub(crate) fn upsert_linear_binding(
    parameter: &mut Value,
    node_id: u32,
    channel: &str,
    cells: &[(usize, usize, f64)],
    tolerance: f64,
) -> Result<bool, String> {
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let axes = parameter_axes(parameter, is_vec2)?;
    let coordinates = parameter_physical_axes(parameter, &axes, is_vec2)?;
    let repeat = parameter_repeats(parameter);
    let bindings = parameter
        .as_object_mut()
        .ok_or("parameter must be an object")?
        .entry("bindings")
        .or_insert_with(|| json!([]));
    let bindings = bindings
        .as_array_mut()
        .ok_or("parameter.bindings must be an array")?;
    let matches = bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            binding.get("node").and_then(Value::as_u64) == Some(u64::from(node_id))
                && binding.get("param_name").and_then(Value::as_str) == Some(channel)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(format!("node {node_id} has duplicate {channel} bindings"));
    }
    let created = matches.is_empty();
    let binding = if let Some(index) = matches.first() {
        &mut bindings[*index]
    } else {
        bindings.push(new_binding(node_id, channel, 0.0, &axes, &coordinates, is_vec2)?);
        bindings.last_mut().expect("binding was just appended")
    };
    if let Some(existing) = binding.get("interpolate_mode").and_then(Value::as_str) {
        if !existing.eq_ignore_ascii_case("linear") {
            return Err(format!(
                "node {node_id} {channel} binding uses unsupported interpolation {existing:?}"
            ));
        }
    }
    let mut grid = binding_grid(binding, &axes, is_vec2, "binding")?;
    let mut flags = binding_flags(binding, &axes, is_vec2, "binding")?;
    let mut changed = created;
    for &(x, y, value) in cells {
        let (gx, gy) = if is_vec2 { (x, y) } else { (x, 0) };
        if gx >= grid.len() || gy >= grid.get(gx).map(Vec::len).unwrap_or(0) {
            return Err(format!(
                "Standard depth binding keypoint is not present in parameter"
            ));
        }
        if flags[gx][gy] && (grid[gx][gy] - value).abs() <= tolerance {
            continue;
        }
        grid[gx][gy] = value;
        flags[gx][gy] = true;
        changed = true;
    }
    if changed {
        reinterpolate(&mut grid, &flags, &axes[0], &axes[1], repeat)?;
        write_binding_grid(binding, &grid, &flags)?;
    }
    Ok(changed)
}

pub(crate) fn refill_parameter_bindings(parameter: &mut Value) -> Result<(), String> {
    let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    let axes = parameter_axes(parameter, is_vec2)?;
    let repeat = parameter_repeats(parameter);
    let Some(bindings) = parameter.get_mut("bindings") else { return Ok(()) };
    let bindings = bindings.as_array_mut().ok_or("parameter.bindings must be an array")?;
    for binding in bindings {
        if binding.get("param_name").and_then(Value::as_str) == Some("deform") {
            let node = required_u32(binding, "node", "binding")?;
            crate::deformers::refill_deform_binding(binding, node, &axes[0], &axes[1], repeat)?;
        } else {
            let mut grid = binding_grid(binding, &axes, is_vec2, "binding")?;
            let flags = binding_flags(binding, &axes, is_vec2, "binding")?;
            reinterpolate(&mut grid, &flags, &axes[0], &axes[1], repeat)?;
            binding["values"] = json!(grid);
        }
    }
    Ok(())
}

pub(crate) fn reinterpolate(
    values: &mut [Vec<f64>],
    is_set: &[Vec<bool>],
    x_axis: &[f64],
    y_axis: &[f64],
    repeat: bool,
) -> Result<(), String> {
    // Copyright © 2022 Inochi2D Project; Copyright © 2024 nijigenerate Project.
    // Derived from ValueParameterBindingImpl.reInterpolate in nijilive/core/param/binding.d.
    // Distributed under the 2-Clause BSD License; see the repository LICENSE.
    let x_count = x_axis.len();
    let y_count = y_axis.len();
    if x_count == 0 || y_count == 0 || values.len() != x_count || is_set.len() != x_count {
        return Err("binding grid dimensions do not match parameter axes".to_string());
    }
    if values.iter().any(|column| column.len() != y_count) {
        return Err("binding value grid is not rectangular".to_string());
    }
    let mut valid = is_set.to_vec();
    let mut valid_count = valid.iter().flatten().filter(|&&set| set).count();
    let total_count = x_count * y_count;
    if valid.iter().any(|column| column.len() != y_count) {
        return Err("binding set grid is not rectangular".to_string());
    }
    if valid_count == 0 {
        return Ok(());
    }
    if repeat && y_count == 1 {
        let anchors: Vec<usize> = (0..x_count).filter(|&x| is_set[x][0]).collect();
        let first = anchors[0];
        let last = anchors[anchors.len() - 1];
        for x in 0..x_count {
            if is_set[x][0] {
                continue;
            }
            let left = anchors.iter().copied().rev().find(|&i| i < x);
            let right = anchors.iter().copied().find(|&i| i > x);
            let (a, b) = (left.unwrap_or(last), right.unwrap_or(first));
            let ax = x_axis[a] - if left.is_none() { 1.0 } else { 0.0 };
            let bx = x_axis[b] + if right.is_none() { 1.0 } else { 0.0 };
            values[x][0] = if a == b || x_axis[x] == ax {
                values[a][0]
            } else if x_axis[x] == bx {
                values[b][0]
            } else {
                let t = (x_axis[x] - ax) / (bx - ax);
                values[a][0] * (1.0 - t) + values[b][0] * t
            };
        }
        return Ok(());
    }
    let mut newly_set = vec![vec![false; y_count]; x_count];
    let mut distances = vec![vec![0.0; y_count]; x_count];
    let mut commit = Vec::<(usize, usize)>::new();
    let mut iterations = 0usize;
    while valid_count < total_count {
        iterations += 1;
        if iterations > total_count.saturating_mul(2).saturating_add(4) {
            return Err("binding interpolation did not converge".to_string());
        }
        for &(x, y) in &commit {
            if valid[x][y] {
                return Err("binding interpolation generated a duplicate point".to_string());
            }
            valid[x][y] = true;
            valid_count += 1;
        }
        commit.clear();
        if valid_count == total_count {
            break;
        }
        newly_set.iter_mut().for_each(|column| column.fill(false));
        interpolate_1d_2d(
            values,
            &valid,
            &mut newly_set,
            &mut commit,
            x_axis,
            y_axis,
            false,
        );
        interpolate_1d_2d(
            values,
            &valid,
            &mut newly_set,
            &mut commit,
            x_axis,
            y_axis,
            true,
        );
        if !commit.is_empty() {
            continue;
        }
        extrapolate_corners(values, &valid, &mut commit, x_count, y_count);
        if !commit.is_empty() {
            continue;
        }
        extend_and_intersect(
            values,
            &valid,
            &mut newly_set,
            &mut distances,
            &mut commit,
            x_axis,
            y_axis,
            false,
        );
        extend_and_intersect(
            values,
            &valid,
            &mut newly_set,
            &mut distances,
            &mut commit,
            x_axis,
            y_axis,
            true,
        );
        if commit.is_empty() {
            return Err("binding interpolation could not fill sparse keypoints".to_string());
        }
    }
    Ok(())
}

fn point(y_major: bool, major: usize, minor: usize) -> (usize, usize) {
    if y_major {
        (minor, major)
    } else {
        (major, minor)
    }
}

fn axis_point(y_major: bool, index: usize, x_axis: &[f64], y_axis: &[f64]) -> f64 {
    if y_major {
        x_axis[index]
    } else {
        y_axis[index]
    }
}

fn interpolate_1d_2d(
    values: &mut [Vec<f64>],
    valid: &[Vec<bool>],
    newly_set: &mut [Vec<bool>],
    commit: &mut Vec<(usize, usize)>,
    x_axis: &[f64],
    y_axis: &[f64],
    second_pass: bool,
) {
    let y_major = second_pass;
    let major_count = if y_major { y_axis.len() } else { x_axis.len() };
    let minor_count = if y_major { x_axis.len() } else { y_axis.len() };
    let mut detected_intersections = false;
    for major in 0..major_count {
        let mut left = (0..minor_count).find(|&minor| {
            let (x, y) = point(y_major, major, minor);
            valid[x][y]
        });
        let Some(mut left_index) = left.take() else {
            continue;
        };
        loop {
            while left_index + 1 < minor_count {
                let (x, y) = point(y_major, major, left_index + 1);
                if !valid[x][y] {
                    break;
                }
                left_index += 1;
            }
            if left_index >= minor_count - 1 {
                break;
            }
            let right_index = match (left_index + 1..minor_count).find(|&minor| {
                let (x, y) = point(y_major, major, minor);
                valid[x][y]
            }) {
                Some(right) => right,
                None => break,
            };
            let left_axis = axis_point(y_major, left_index, x_axis, y_axis);
            let right_axis = axis_point(y_major, right_index, x_axis, y_axis);
            for mid in left_index + 1..right_index {
                let (x, y) = point(y_major, major, mid);
                let mid_axis = axis_point(y_major, mid, x_axis, y_axis);
                let offset = (mid_axis - left_axis) / (right_axis - left_axis);
                let left_point = point(y_major, major, left_index);
                let right_point = point(y_major, major, right_index);
                let value = lerp(
                    values[left_point.0][left_point.1],
                    values[right_point.0][right_point.1],
                    offset,
                );
                if second_pass && newly_set[x][y] {
                    if !detected_intersections {
                        commit.clear();
                    }
                    let averaged = (value + values[x][y]) * 0.5;
                    set_new(values, newly_set, commit, x, y, averaged);
                    detected_intersections = true;
                }
                if !detected_intersections {
                    set_new(values, newly_set, commit, x, y, value);
                }
            }
            left_index = right_index;
        }
    }
}

fn extrapolate_corners(
    values: &mut [Vec<f64>],
    valid: &[Vec<bool>],
    commit: &mut Vec<(usize, usize)>,
    x_count: usize,
    y_count: usize,
) {
    if x_count <= 1 || y_count <= 1 {
        return;
    }
    for x in 0..x_count - 1 {
        for y in 0..y_count - 1 {
            let corner = if valid[x][y]
                && valid[x + 1][y]
                && valid[x][y + 1]
                && !valid[x + 1][y + 1]
            {
                Some((x + 1, y + 1, x, y))
            } else if valid[x][y] && valid[x + 1][y] && !valid[x][y + 1] && valid[x + 1][y + 1] {
                Some((x, y + 1, x + 1, y))
            } else if valid[x][y] && !valid[x + 1][y] && valid[x][y + 1] && valid[x + 1][y + 1] {
                Some((x + 1, y, x, y + 1))
            } else if !valid[x][y] && valid[x + 1][y] && valid[x][y + 1] && valid[x + 1][y + 1] {
                Some((x, y, x + 1, y + 1))
            } else {
                None
            };
            let Some((missing_x, missing_y, base_x, base_y)) = corner else {
                continue;
            };
            let base = values[base_x][base_y];
            let dx = if base_x == x {
                values[x + 1][base_y] - base
            } else {
                values[x][base_y] - base
            };
            let dy = if base_y == y {
                values[base_x][y + 1] - base
            } else {
                values[base_x][y] - base
            };
            values[missing_x][missing_y] = base + dx + dy;
            commit.push((missing_x, missing_y));
        }
    }
}

fn extend_and_intersect(
    values: &mut [Vec<f64>],
    valid: &[Vec<bool>],
    newly_set: &mut [Vec<bool>],
    distances: &mut [Vec<f64>],
    commit: &mut Vec<(usize, usize)>,
    x_axis: &[f64],
    y_axis: &[f64],
    second_pass: bool,
) {
    let y_major = second_pass;
    let major_count = if y_major { y_axis.len() } else { x_axis.len() };
    let minor_count = if y_major { x_axis.len() } else { y_axis.len() };
    let mut detected_intersections = false;
    for major in 0..major_count {
        let Some(first) = (0..minor_count).find(|&minor| {
            let (x, y) = point(y_major, major, minor);
            valid[x][y]
        }) else {
            continue;
        };
        let first_value = {
            let (x, y) = point(y_major, major, first);
            values[x][y]
        };
        let first_origin = axis_point(y_major, first, x_axis, y_axis);
        for minor in 0..first {
            set_or_average(
                values,
                newly_set,
                distances,
                commit,
                y_major,
                major,
                minor,
                first_value,
                first_origin,
                second_pass,
                &mut detected_intersections,
                x_axis,
                y_axis,
            );
        }
        let last = (0..minor_count)
            .rev()
            .find(|&minor| {
                let (x, y) = point(y_major, major, minor);
                valid[x][y]
            })
            .expect("a row with a first valid point has a last point");
        let last_value = {
            let (x, y) = point(y_major, major, last);
            values[x][y]
        };
        let last_origin = axis_point(y_major, last, x_axis, y_axis);
        for minor in last + 1..minor_count {
            set_or_average(
                values,
                newly_set,
                distances,
                commit,
                y_major,
                major,
                minor,
                last_value,
                last_origin,
                second_pass,
                &mut detected_intersections,
                x_axis,
                y_axis,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn set_or_average(
    values: &mut [Vec<f64>],
    newly_set: &mut [Vec<bool>],
    distances: &mut [Vec<f64>],
    commit: &mut Vec<(usize, usize)>,
    y_major: bool,
    major: usize,
    minor: usize,
    value: f64,
    origin: f64,
    second_pass: bool,
    detected_intersections: &mut bool,
    x_axis: &[f64],
    y_axis: &[f64],
) {
    let (x, y) = point(y_major, major, minor);
    let min_distance = (axis_point(y_major, minor, x_axis, y_axis) - origin).abs();
    if second_pass && newly_set[x][y] {
        if !*detected_intersections {
            commit.clear();
        }
        let major_distance = distances[x][y];
        let fraction =
            min_distance / (min_distance + major_distance * major_distance / min_distance);
        let blended = value * (1.0 - fraction) + values[x][y] * fraction;
        set_new(values, newly_set, commit, x, y, blended);
        distances[x][y] = 0.0;
        *detected_intersections = true;
    }
    if !*detected_intersections {
        set_new(values, newly_set, commit, x, y, value);
        distances[x][y] = min_distance;
    }
}

fn set_new(
    values: &mut [Vec<f64>],
    newly_set: &mut [Vec<bool>],
    commit: &mut Vec<(usize, usize)>,
    x: usize,
    y: usize,
    value: f64,
) {
    values[x][y] = value;
    newly_set[x][y] = true;
    commit.push((x, y));
}

fn required_u32(value: &Value, key: &str, path: &str) -> Result<u32, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{path}.{key} must be an unsigned 32-bit integer"))
}

fn vec2_field(value: &Value, key: &str, fallback: [f64; 2]) -> Result<[f64; 2], String> {
    let Some(value) = value.get(key) else {
        return Ok(fallback);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array"))?;
    if array.len() < 2 {
        return Err(format!("{key} must have two components"));
    }
    Ok([
        finite_number(&array[0], key)?,
        finite_number(&array[1], key)?,
    ])
}

fn array2(value: Option<&Value>, path: &str) -> Result<[f64; 2], String> {
    let array = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} must be a two-element array"))?;
    if array.len() != 2 {
        return Err(format!("{path} must be a two-element array"));
    }
    Ok([
        finite_number(&array[0], path)?,
        finite_number(&array[1], path)?,
    ])
}

fn index_pair(value: Option<&Value>, path: &str) -> Result<[usize; 2], String> {
    let array = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} must be a two-element array of axis indexes"))?;
    if array.len() != 2 {
        return Err(format!(
            "{path} must be a two-element array of axis indexes"
        ));
    }
    let parse = |component: &Value| {
        component
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| format!("{path} entries must be unsigned axis indexes"))
    };
    Ok([parse(&array[0])?, parse(&array[1])?])
}

fn finite_number(value: &Value, path: &str) -> Result<f64, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{path} must be a number"))?;
    if !number.is_finite() {
        return Err(format!("{path} must be finite"));
    }
    Ok(number)
}

fn number(value: f64, path: &str) -> Result<Value, String> {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| format!("{path} is not finite"))
}

fn interpolate_deform(
    binding: &Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    cell_x: AxisCell,
    cell_y: Option<AxisCell>,
    parameter_id: u32,
    repeat: bool,
) -> Result<Vec<[f64; 2]>, String> {
    let mode = binding
        .get("interpolate_mode")
        .and_then(Value::as_str)
        .unwrap_or("Linear");
    let columns = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("parameter {parameter_id} deform values must be an array"))?;
    let mut flags_loaded = false;
    let mut flags = None;
    let mut derived: HashMap<(usize, usize), Vec<[f64; 2]>> = HashMap::new();
    let mut sample = |x: usize, y: usize| -> Result<Vec<[f64; 2]>, String> {
        let column = columns.get(x).and_then(Value::as_array).ok_or_else(|| {
            format!("parameter {parameter_id} deform value is missing")
        })?;
        let y = if is_vec2 { y } else { 0 };
        let cell = column.get(y).ok_or_else(|| {
            format!("parameter {parameter_id} deform value is missing")
        })?;
        if cell.is_null() || cell.as_array().is_some_and(|points| points.is_empty()) {
            if !flags_loaded {
                flags = deform_flags(binding, axes, is_vec2, parameter_id)?;
                flags_loaded = true;
            }
            if let Some(flags) = flags.as_ref() {
                let set = flags
                    .get(x)
                    .and_then(|column| column.get(y))
                    .ok_or_else(|| {
                        format!("parameter {parameter_id} deform isSet entry is missing")
                    })?;
                if !*set {
                    if let Some(offsets) = derived.get(&(x, y)) {
                        return Ok(offsets.clone());
                    }
                    let offsets = reinterpolate_deform_cell(
                        columns,
                        flags,
                        axes,
                        is_vec2,
                        (x, y),
                        parameter_id,
                        repeat,
                    )?;
                    derived.insert((x, y), offsets.clone());
                    return Ok(offsets);
                }
            }
        }
        parse_offset_cell(cell, parameter_id)
    };
    let y_at = |cell: AxisCell, high: bool| cell.left + usize::from(high);
    // An exact key is `offset == 0` on its cell, or `offset == 1` at the last
    // axis point. Neighbors do not contribute, so a single-key apply can draw.
    let exact = |cell: AxisCell| -> Option<usize> {
        if cell.offset <= 0.0 {
            Some(cell.left)
        } else if cell.offset >= 1.0 {
            Some(cell.left + 1)
        } else {
            None
        }
    };
    match mode.to_ascii_lowercase().as_str() {
        "nearest" => {
            let x = cell_x.left + usize::from(cell_x.offset >= 0.5);
            let y = cell_y
                .map(|cell| y_at(cell, cell.offset >= 0.5))
                .unwrap_or(0);
            sample(x, y)
        }
        "linear" | "cubic"
            if exact(cell_x).is_some()
                && cell_y.map(|cell| exact(cell).is_some()).unwrap_or(true) =>
        {
            let x = exact(cell_x).unwrap_or(cell_x.left);
            let y = cell_y.and_then(exact).unwrap_or(0);
            sample(x, y)
        }
        "linear" => {
            if let Some(cell_y) = cell_y {
                let p00 = sample(cell_x.left, cell_y.left)?;
                let p01 = sample(cell_x.left, cell_y.left + 1)?;
                let p10 = sample(cell_x.left + 1, cell_y.left)?;
                let p11 = sample(cell_x.left + 1, cell_y.left + 1)?;
                let p0 = lerp_offsets(&p00, &p01, cell_y.offset)?;
                let p1 = lerp_offsets(&p10, &p11, cell_y.offset)?;
                lerp_offsets(&p0, &p1, cell_x.offset)
            } else {
                let p0 = sample(cell_x.left, 0)?;
                let p1 = sample(cell_x.left + 1, 0)?;
                lerp_offsets(&p0, &p1, cell_x.offset)
            }
        }
        "cubic" => {
            let x_len = columns.len();
            if is_vec2 {
                let y_len = columns
                    .first()
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                let mut rows = Vec::with_capacity(4);
                for y in 0..4 {
                    let y_index = clamped_index(cell_y.map(|cell| cell.left as isize).unwrap_or(0) + y as isize - 1, y_len);
                    let mut points = Vec::with_capacity(4);
                    for x in 0..4 {
                        let x_index = clamped_index(cell_x.left as isize + x as isize - 1, x_len);
                        points.push(sample(x_index, y_index)?);
                    }
                    rows.push(cubic_offsets(&points[0], &points[1], &points[2], &points[3], cell_x.offset)?);
                }
                cubic_offsets(&rows[0], &rows[1], &rows[2], &rows[3], cell_y.map(|cell| cell.offset).unwrap_or(0.0))
            } else {
                let x = cell_x.left as isize;
                let p0 = sample(clamped_index(x - 1, x_len), 0)?;
                let p1 = sample(clamped_index(x, x_len), 0)?;
                let p2 = sample(clamped_index(x + 1, x_len), 0)?;
                let p3 = sample(clamped_index(x + 2, x_len), 0)?;
                cubic_offsets(&p0, &p1, &p2, &p3, cell_x.offset)
            }
        }
        other => Err(format!(
            "parameter {parameter_id} deform interpolation {other:?} is unsupported"
        )),
    }
}

fn deform_flags(
    binding: &Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    parameter_id: u32,
) -> Result<Option<Vec<Vec<bool>>>, String> {
    let Some(raw) = binding.get("isSet") else {
        return Ok(None);
    };
    let raw = raw.as_array().ok_or_else(|| {
        format!("parameter {parameter_id} deform isSet must be an array")
    })?;
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    if raw.len() != axes[0].len() {
        return Err(format!(
            "parameter {parameter_id} deform isSet has a mismatched X axis"
        ));
    }
    raw.iter()
        .map(|column| {
            let column = column.as_array().ok_or_else(|| {
                format!("parameter {parameter_id} deform isSet columns must be arrays")
            })?;
            if column.len() != y_count {
                return Err(format!(
                    "parameter {parameter_id} deform isSet has a mismatched Y axis"
                ));
            }
            column
                .iter()
                .map(|flag| match flag {
                    Value::Bool(set) => Ok(*set),
                    Value::Null => Ok(false),
                    _ => Err(format!(
                        "parameter {parameter_id} deform isSet entries must be booleans or null"
                    )),
                })
                .collect()
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn reinterpolate_deform_cell(
    columns: &[Value],
    flags: &[Vec<bool>],
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    target: (usize, usize),
    parameter_id: u32,
    repeat: bool,
) -> Result<Vec<[f64; 2]>, String> {
    let x_count = axes[0].len();
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    if columns.len() != x_count {
        return Err(format!(
            "parameter {parameter_id} deform values have a mismatched X axis"
        ));
    }
    let columns = columns
        .iter()
        .map(|column| {
            let column = column.as_array().ok_or_else(|| {
                format!("parameter {parameter_id} deform value is missing")
            })?;
            if column.len() != y_count {
                return Err(format!(
                    "parameter {parameter_id} deform values have a mismatched Y axis"
                ));
            }
            Ok(column)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut sources = Vec::new();
    let mut empty_sources = Vec::new();
    let mut vertex_count = None;
    for x in 0..x_count {
        for y in 0..y_count {
            if !flags[x][y] {
                continue;
            }
            let cell = columns[x]
                .get(y)
                .ok_or_else(|| format!("parameter {parameter_id} deform value is missing"))?;
            let points = cell.as_array().ok_or_else(|| {
                format!("parameter {parameter_id} deform value must be an array")
            })?;
            for point in points {
                let pair = point.as_array().ok_or_else(|| {
                    format!("parameter {parameter_id} deform offset must be a pair")
                })?;
                if pair.len() != 2 {
                    return Err(format!(
                        "parameter {parameter_id} deform offset must be a pair"
                    ));
                }
                finite_number(&pair[0], "deform offset")?;
                finite_number(&pair[1], "deform offset")?;
            }
            if points.is_empty() {
                empty_sources.push((x, y));
            } else if let Some(count) = vertex_count {
                if count != points.len() {
                    return Err("deform offset length mismatch".into());
                }
            } else {
                vertex_count = Some(points.len());
            }
            sources.push((x, y, points));
        }
    }
    let Some(vertex_count) = vertex_count else {
        return Ok(Vec::new());
    };
    let unused_y_axis = [0.0];
    let y_axis = if is_vec2 {
        axes[1].as_slice()
    } else {
        unused_y_axis.as_slice()
    };
    if !empty_sources.is_empty() {
        let mut dependencies = vec![vec![0.0; y_count]; x_count];
        for &(x, y) in &empty_sources {
            dependencies[x][y] = f64::NAN;
        }
        reinterpolate(&mut dependencies, flags, &axes[0], y_axis, repeat)?;
        if !dependencies[target.0][target.1].is_finite() {
            return Err("deform offset length mismatch".into());
        }
    }

    let source_count = sources.iter().filter(|(_, _, points)| !points.is_empty()).count();
    if source_count < vertex_count.saturating_mul(2) {
        let mut values = vec![vec![0.0; y_count]; x_count];
        let mut weights = Vec::with_capacity(source_count);
        for &(x, y, points) in &sources {
            if points.is_empty() {
                continue;
            }
            for column in &mut values {
                column.fill(0.0);
            }
            values[x][y] = 1.0;
            reinterpolate(&mut values, flags, &axes[0], y_axis, repeat)?;
            let weight = values[target.0][target.1];
            if !weight.is_finite() {
                return Err(format!(
                    "parameter {parameter_id} deform interpolation is not finite"
                ));
            }
            weights.push(weight);
        }
        let mut offsets = Vec::with_capacity(vertex_count);
        for vertex in 0..vertex_count {
            let mut offset = [0.0; 2];
            let mut source_index = 0;
            for (_, _, points) in &sources {
                if points.is_empty() {
                    continue;
                }
                let weight = weights[source_index];
                source_index += 1;
                if weight == 0.0 {
                    continue;
                }
                let pair = points[vertex].as_array().expect("authored pair was validated");
                offset[0] += weight * finite_number(&pair[0], "deform offset")?;
                offset[1] += weight * finite_number(&pair[1], "deform offset")?;
            }
            offsets.push(offset);
        }
        return Ok(offsets);
    }

    let mut x_values = vec![vec![0.0; y_count]; x_count];
    let mut y_values = vec![vec![0.0; y_count]; x_count];
    let mut offsets = Vec::with_capacity(vertex_count);
    // shortcut: per-vertex fill for dense sources, broaden weight use if profiling shows this branch is costly.
    for vertex in 0..vertex_count {
        for column in &mut x_values {
            column.fill(0.0);
        }
        for column in &mut y_values {
            column.fill(0.0);
        }
        for &(x, y, points) in &sources {
            if points.is_empty() {
                continue;
            }
            let pair = points[vertex].as_array().expect("authored pair was validated");
            x_values[x][y] = finite_number(&pair[0], "deform offset")?;
            y_values[x][y] = finite_number(&pair[1], "deform offset")?;
        }
        reinterpolate(&mut x_values, flags, &axes[0], y_axis, repeat)?;
        reinterpolate(&mut y_values, flags, &axes[0], y_axis, repeat)?;
        offsets.push([x_values[target.0][target.1], y_values[target.0][target.1]]);
    }
    Ok(offsets)
}

fn parse_offset_cell(cell: &Value, parameter_id: u32) -> Result<Vec<[f64; 2]>, String> {
    let points = cell.as_array().ok_or_else(|| {
        format!("parameter {parameter_id} deform value must be an array")
    })?;
    points
        .iter()
        .map(|point| {
            let pair = point.as_array().ok_or_else(|| {
                format!("parameter {parameter_id} deform offset must be a pair")
            })?;
            if pair.len() != 2 {
                return Err(format!(
                    "parameter {parameter_id} deform offset must be a pair"
                ));
            }
            let x = finite_number(&pair[0], "deform offset")?;
            let y = finite_number(&pair[1], "deform offset")?;
            Ok([x, y])
        })
        .collect()
}

fn lerp_offsets(a: &[[f64; 2]], b: &[[f64; 2]], t: f64) -> Result<Vec<[f64; 2]>, String> {
    if a.len() != b.len() {
        return Err("deform offset length mismatch".into());
    }
    Ok(a.iter()
        .zip(b)
        .map(|(left, right)| {
            [
                left[0] * (1.0 - t) + right[0] * t,
                left[1] * (1.0 - t) + right[1] * t,
            ]
        })
        .collect())
}

fn cubic_offsets(
    p0: &[[f64; 2]],
    p1: &[[f64; 2]],
    p2: &[[f64; 2]],
    p3: &[[f64; 2]],
    t: f64,
) -> Result<Vec<[f64; 2]>, String> {
    let count = p1.len();
    if p0.len() != count || p2.len() != count || p3.len() != count {
        return Err("deform offset length mismatch".into());
    }
    Ok((0..count)
        .map(|index| {
            [
                cubic_component(p0[index][0], p1[index][0], p2[index][0], p3[index][0], t),
                cubic_component(p0[index][1], p1[index][1], p2[index][1], p3[index][1], t),
            ]
        })
        .collect())
}

pub(crate) fn cubic_component(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * (2.0 * p1
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

fn clamped_index(index: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    index.clamp(0, len as isize - 1) as usize
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a * (1.0 - t) + b * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_drives_the_deformation_and_transform_bindings_of_the_target() {
        // Look (20) spans -1..1 and drives Open (21), 0..2, from 0 at -1 to 2 at 1. Open moves node 3
        // by 0..10 and deforms node 4 by 0..8 in x.
        let model = json!({
            "nodes": {"uuid": 1, "type": "Node", "transform": {}, "children": [
                {"uuid": 3, "type": "Node", "transform": {}},
                {"uuid": 4, "type": "Part", "transform": {}},
            ]},
            "param": [
                {"uuid": 20, "name": "Look", "is_vec2": false, "min": [-1.0, 0.0], "max": [1.0, 1.0],
                 "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": [
                    {"node": 21, "param_name": "X", "interpolate_mode": "Linear",
                     "values": [[0.0], [2.0]], "isSet": [[true], [true]]}]},
                {"uuid": 21, "name": "Open", "is_vec2": false, "min": [0.0, 0.0], "max": [2.0, 1.0],
                 "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": [
                    {"node": 3, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                     "values": [[0.0], [10.0]], "isSet": [[true], [true]]},
                    {"node": 4, "param_name": "deform", "interpolate_mode": "Linear",
                     "values": [[[[0.0, 0.0]]], [[[8.0, 0.0]]]], "isSet": [[true], [true]]}]},
            ],
        });
        let at = |look: f64| BTreeMap::from([(20, [look, 0.0])]);
        let moved = evaluate(&model, &at(0.5)).unwrap();
        assert_eq!(moved["nodes"]["children"][0]["transform"]["trans"][0], json!(7.5));
        assert_eq!(raster_deformations(&model, &at(0.5)).unwrap()[&4], vec![[6.0, 0.0]]);
        assert_eq!(raster_deformations(&model, &at(-1.0)).unwrap()[&4], vec![[0.0, 0.0]]);
        // The driven parameter's own value is overridden on the linked axis only.
        let both = BTreeMap::from([(20, [1.0, 0.0]), (21, [0.0, 0.0])]);
        assert_eq!(evaluate(&model, &both).unwrap()["nodes"]["children"][0]["transform"]["trans"][0], json!(10.0));
        // Chains settle whatever the order: C is driven by B, which is driven by A, listed C, B, A.
        let chain = json!({
            "nodes": {"uuid": 1, "type": "Node", "transform": {}, "children": [{"uuid": 3, "type": "Node", "transform": {}}]},
            "param": [
                {"uuid": 32, "name": "C", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
                 "axis_points": [[0.0, 1.0], [0.0]], "bindings": [
                    {"node": 3, "param_name": "transform.t.x", "values": [[0.0], [100.0]], "isSet": [[true], [true]]}]},
                {"uuid": 31, "name": "B", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
                 "axis_points": [[0.0, 1.0], [0.0]], "bindings": [
                    {"node": 32, "param_name": "X", "values": [[1.0], [0.0]], "isSet": [[true], [true]]}]},
                {"uuid": 30, "name": "A", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
                 "axis_points": [[0.0, 1.0], [0.0]], "bindings": [
                    {"node": 31, "param_name": 0, "values": [[0.0], [1.0]], "isSet": [[true], [true]]}]},
            ],
        });
        // A = 1 forces B = 1, which forces C = 0 (B's link inverts), so node 3 stays put; A = 0 gives C = 1.
        let chained = |a: f64| evaluate(&chain, &BTreeMap::from([(30, [a, 0.0])])).unwrap();
        assert_eq!(chained(1.0)["nodes"]["children"][0]["transform"]["trans"][0], json!(0.0));
        assert_eq!(chained(0.0)["nodes"]["children"][0]["transform"]["trans"][0], json!(100.0));
    }

    #[test]
    fn physics_bindings_multiply_settings_and_add_length_without_baking() {
        let mut model = json!({
            "nodes": {"uuid":1,"type":"SimplePhysics","transform":{},"gravity":2.0,"length":50.0,
                "angle_damping":0.4,"output_scale":[2.0,3.0]},
            "param": []
        });
        for (uuid, channel, value) in [(2,"gravity",0.5),(3,"gravity",3.0),
                (4,"length",10.0),(5,"length",-2.0),(6,"angleDamping",0.5),
                (7,"outputScale.y",2.0)] {
            model["param"].as_array_mut().unwrap().push(json!({
                "uuid":uuid,"min":[0,0],"max":[1,1],"axis_points":[[0,1],[0]],
                "bindings":[{"node":1,"param_name":channel,"values":[[value],[value]]}]
            }));
        }
        let before = model.clone();
        let posed = evaluate(&model, &BTreeMap::new()).unwrap();
        assert_eq!(posed["nodes"]["gravity"], 3.0);
        assert_eq!(posed["nodes"]["length"], 58.0);
        assert_eq!(posed["nodes"]["angle_damping"], 0.2);
        assert_eq!(posed["nodes"]["output_scale"], json!([2.0,6.0]));
        assert_eq!(model, before);
        model["nodes"]["type"] = json!("Node");
        assert!(evaluate(&model, &BTreeMap::new()).unwrap_err().contains("unsupported"));
    }

    #[test]
    fn opacity_bindings_multiply_and_zsort_bindings_add_on_the_node() {
        let mut model = json!({
            "nodes": {"uuid":1,"type":"Node","transform":{},"children":[
                {"uuid":2,"type":"Part","transform":{},"opacity":0.5,"zsort":3.0}]},
            "param": []
        });
        let bindings = [(10, "opacity", 1.0, 0.5), (11, "opacity", 1.0, 0.5), (12, "zSort", 0.0, 4.0)];
        for (uuid, channel, low, high) in bindings {
            model["param"].as_array_mut().unwrap().push(json!({
                "uuid":uuid,"min":[0,0],"max":[1,1],"axis_points":[[0,1],[0]],
                "bindings":[{"node":2,"param_name":channel,"values":[[low],[high]]}]
            }));
        }
        let before = model.clone();
        let values = BTreeMap::from([(10, [1.0, 0.0]), (11, [0.5, 0.0]), (12, [0.5, 0.0])]);
        let posed = evaluate(&model, &values).unwrap();
        // 0.5 * 0.5 * 0.75 for the opacity, 3 + 2 for the draw order.
        assert!((posed["nodes"]["children"][0]["opacity"].as_f64().unwrap() - 0.1875).abs() < 1e-12);
        assert!((posed["nodes"]["children"][0]["zsort"].as_f64().unwrap() - 5.0).abs() < 1e-12);
        assert_eq!(model, before);
        // zSort fits every node; opacity fits only the node types that have it.
        model["param"][0]["bindings"][0]["node"] = json!(1);
        assert!(evaluate(&model, &values).unwrap_err().contains("unsupported"));
        let zsort_parameter = model["param"][2].clone();
        model["param"] = json!([zsort_parameter]);
        model["param"][0]["bindings"][0]["node"] = json!(1);
        let posed = evaluate(&model, &BTreeMap::from([(12, [1.0, 0.0])])).unwrap();
        assert_eq!(posed["nodes"]["zsort"], json!(4.0));
    }

    #[test]
    fn scale_bindings_reset_zero_offset_without_erasing_base_scale() {
        let parameter = |uuid, amount| {
            json!({
                "uuid": uuid,
                "is_vec2": false,
                "min": [0, 0],
                "max": [1, 1],
                "defaults": [0, 0],
                "axis_points": [[0, 1], [0]],
                "bindings": [{
                    "node": 1,
                    "param_name": "transform.s.x",
                    "values": [[amount], [amount]]
                }]
            })
        };
        let model = json!({
            "nodes": {"uuid": 1, "transform": {"scale": [2, 1]}},
            "param": [parameter(2, 0), parameter(3, 3)]
        });
        let output = evaluate(&model, &BTreeMap::new()).unwrap();
        assert_eq!(output["nodes"]["transform"]["scale"][0], 6.0);
    }

    #[test]
    fn rejects_unsupported_or_malformed_automation_without_mutating_model() {
        for automation in [json!([{"type": "physics"}]), json!({})] {
            let model = json!({
                "nodes": {"uuid": 1, "transform": {}},
                "param": [],
                "automation": automation
            });
            let before = model.clone();
            assert!(evaluate(&model, &BTreeMap::new()).is_err());
            assert_eq!(model, before);
        }
        let model = json!({"nodes": {"uuid": 1, "transform": {}}, "param": [], "automation": null});
        assert!(evaluate(&model, &BTreeMap::new()).is_ok());
    }

    #[test]
    fn repeat_fill_uses_authored_anchors_across_the_seam() {
        let axis = [0.0, 0.25, 0.75, 1.0];
        for (input, set, expected) in [
            ([0.0, 10.0, 20.0, 0.0], [false, true, true, false], [15.0, 10.0, 20.0, 15.0]),
            ([10.0, 0.0, 20.0, 0.0], [true, false, true, false], [10.0, 40.0 / 3.0, 20.0, 10.0]),
            ([0.0, 10.0, 0.0, 30.0], [false, true, false, true], [30.0, 10.0, 70.0 / 3.0, 30.0]),
            ([0.0, 10.0, 0.0, 0.0], [false, true, false, false], [10.0; 4]),
            ([10.0, 0.0, 0.0, 30.0], [true, false, false, true], [10.0, 15.0, 25.0, 30.0]),
        ] {
            let mut values: Vec<_> = input.iter().map(|v| vec![*v]).collect();
            let flags: Vec<_> = set.iter().map(|v| vec![*v]).collect();
            reinterpolate(&mut values, &flags, &axis, &[0.0], true).unwrap();
            for (cell, expected) in values.iter().zip(expected) {
                assert!((cell[0] - expected).abs() < 1e-12, "{values:?}");
            }
        }
        let mut values = vec![vec![10.0], vec![f64::NAN], vec![0.0]];
        reinterpolate(&mut values, &[vec![true], vec![true], vec![false]],
            &[0.0, 0.5, 1.0], &[0.0], true).unwrap();
        assert_eq!(values[2][0], 10.0, "zero-weight empty source must not poison the seam");
    }

    #[test]
    fn repeat_lazy_deform_matches_literal_seam_values_for_sparse_and_dense_weights() {
        for vertices in [1, 3] {
            let model = json!({
                "nodes": {"uuid": 1, "type": "Node", "children": []},
                "param": [{"uuid": 9, "repeat": true, "is_vec2": false,
                    "min": [0.0, 0.0], "max": [4.0, 1.0], "defaults": [0.0, 0.0],
                    "axis_points": [[0.0, 0.25, 0.75, 1.0], [0.0]],
                    "bindings": [{"node": 2, "param_name": "deform", "interpolate_mode": "Linear",
                        "values": [[[]], [vec![[10.0, 20.0]; vertices]], [vec![[20.0, 40.0]; vertices]], [[]]],
                        "isSet": [[false], [true], [true], [false]]}]}]
            });
            let before = model.clone();
            for (x, expected) in [(0.0, 15.0), (3.5, 17.5), (4.0, 15.0)] {
                let sampled = raster_deformations(&model, &BTreeMap::from([(9, [x, 0.0])])).unwrap();
                assert_eq!(sampled[&2], vec![[expected, expected * 2.0]; vertices]);
            }
            assert_eq!(model, before);
        }
    }

    #[test]
    fn fills_sparse_one_dimensional_keys_using_physical_axis_spacing() {
        let mut values = vec![vec![-2.0], vec![0.0], vec![4.0]];
        let flags = vec![vec![true], vec![false], vec![true]];
        reinterpolate(&mut values, &flags, &[0.0, 0.2, 1.0], &[0.0], false).unwrap();
        assert_eq!(values, vec![vec![-2.0], vec![-0.8], vec![4.0]]);
    }

    #[test]
    fn fills_sparse_two_dimensional_grid_like_binding_reinterpolate() {
        let mut values = vec![vec![0.0, 0.0, 20.0], vec![0.0; 3], vec![10.0, 0.0, 30.0]];
        let flags = vec![
            vec![true, false, true],
            vec![false; 3],
            vec![true, false, true],
        ];
        reinterpolate(&mut values, &flags, &[0.0, 0.2, 1.0], &[0.0, 0.4, 1.0], false).unwrap();
        assert_eq!(
            values,
            vec![
                vec![0.0, 8.0, 20.0],
                vec![2.0, 10.0, 22.0],
                vec![10.0, 18.0, 30.0]
            ]
        );
    }

    #[test]
    fn set_trs_edits_exact_physical_key_and_does_not_mutate_on_error() {
        let mut model = json!({
            "nodes": {"uuid": 4, "type": "Node", "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]}},
            "param": [{"uuid": 9, "is_vec2": false, "min": [0,0], "max": [1,1], "defaults": [0,0], "axis_points": [[0,0.5,1],[0]], "bindings": []}]
        });
        let before = model.clone();
        assert!(set_trs(
            &mut model,
            &json!({"parameter":9,"nodes":[4],"key":[9,0],"translation":[2,3]})
        )
        .is_err());
        assert_eq!(model, before);
        set_trs(
            &mut model,
            &json!({"parameter":9,"nodes":[4],"key":[1,0],"translation":[2,3]}),
        )
        .unwrap();
        let bindings = model["param"][0]["bindings"].as_array().unwrap();
        let tx = bindings
            .iter()
            .find(|binding| binding["param_name"] == "transform.t.x")
            .unwrap();
        assert_eq!(tx["values"][1][0], 2.0);
        assert_eq!(tx["isSet"][1][0], true);
    }

    #[test]
    fn set_trs_accepts_a_degenerate_unused_y_range_for_one_dimensional_parameters() {
        let mut model = json!({
            "nodes": {"uuid": 4, "type": "Node", "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]}},
            "param": [{"uuid": 9, "is_vec2": false, "min": [0,0], "max": [1,0], "defaults": [0,0], "axis_points": [[0,1],[0]], "bindings": []}]
        });
        set_trs(
            &mut model,
            &json!({"parameter":9,"nodes":[4],"key":[0,0],"translation":[2,3]}),
        )
        .unwrap();
    }

    fn pitch_model() -> Value {
        json!({
            "nodes": {
                "uuid": 1,
                "type": "Node",
                "transform": {"trans": [10, 20, 3], "rot": [0.25, -0.5, 1], "scale": [2, 4]},
                "children": [{
                    "uuid": 4,
                    "type": "Part",
                    "enabled": true,
                    "transform": {"trans": [1, 2, 3], "rot": [0.25, -0.5, 0.125], "scale": [2, 2]},
                    "depths": [0.5, -1],
                    "children": []
                }, {
                    "uuid": 5,
                    "type": "DepthBone",
                    "enabled": false,
                    "transform": {"trans": [0, 0, 3], "rot": [0.25, -0.5, 0], "scale": [1, 1]},
                    "children": []
                }]
            },
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [-1, 0],
                "max": [1, 1],
                "defaults": [0, 0],
                "axis_points": [[0, 0.5, 1], [0]],
                "bindings": []
            }]
        })
    }

    #[test]
    fn linear_pitch_yaw_and_translation_z_add_to_every_node_base_transform() {
        let mut model = pitch_model();
        for (key, tz, rx, ry) in [
            (0, -2.0, -0.25, 0.5),
            (1, 1.0, 0.125, 0.0),
            (2, 4.0, 0.5, -0.5),
        ] {
            set_trs(
                &mut model,
                &json!({
                    "parameter": 9, "nodes": [4, 5], "key": [key, 0],
                    "translation_z": tz, "rotation_x": rx, "rotation_y": ry
                }),
            )
            .unwrap();
        }
        let mut second = json!({
            "uuid": 10, "is_vec2": false, "min": [0, 0], "max": [1, 1], "defaults": [0, 0],
            "axis_points": [[0, 1], [0]],
            "bindings": [{
                "node": 4, "param_name": "transform.t.z",
                "values": [[0.5], [0.5]], "isSet": [[true], [true]], "interpolate_mode": "Linear"
            }]
        });
        model["param"].as_array_mut().unwrap().push(second.take());
        let mut values = BTreeMap::new();
        values.insert(9, [0.5, 0.0]);
        let output = evaluate(&model, &values).unwrap();
        let part = &output["nodes"]["children"][0]["transform"];
        assert!((part["trans"][2].as_f64().unwrap() - 6.0).abs() < 1e-12);
        assert!((part["rot"][0].as_f64().unwrap() - 0.5625).abs() < 1e-12);
        assert!((part["rot"][1].as_f64().unwrap() - (-0.75)).abs() < 1e-12);
        assert_eq!(part["trans"][0], 1.0);
        assert_eq!(part["rot"][2], 0.125);
        assert_eq!(output["nodes"]["children"][0]["depths"], json!([0.5, -1]));
        let bone = &output["nodes"]["children"][1]["transform"];
        assert!((bone["trans"][2].as_f64().unwrap() - 5.5).abs() < 1e-12);
        assert!((bone["rot"][0].as_f64().unwrap() - 0.5625).abs() < 1e-12);
        assert!((bone["rot"][1].as_f64().unwrap() - (-0.75)).abs() < 1e-12);
    }

    #[test]
    fn pitch_channels_reject_nonlinear_interpolation_and_keep_deform_off_trs() {
        let mut nonlinear = pitch_model();
        nonlinear["param"][0]["bindings"] = json!([{
            "node": 4, "param_name": "transform.r.x", "interpolate_mode": "Nearest",
            "values": [[0.2], [0.2], [0.2]], "isSet": [[true], [true], [true]]
        }]);
        let before = nonlinear.clone();
        assert!(evaluate(&nonlinear, &BTreeMap::new())
            .unwrap_err()
            .contains("unsupported interpolation"));
        assert_eq!(nonlinear, before);

        let mut deform = pitch_model();
        deform["param"][0]["bindings"] = json!([{
            "node": 4, "param_name": "deform",
            "values": [[[[0.0, 1.0]]]], "isSet": [[true]]
        }]);
        let posed = evaluate(&deform, &BTreeMap::new()).unwrap();
        let baseline = evaluate(&pitch_model(), &BTreeMap::new()).unwrap();
        assert_eq!(posed["nodes"], baseline["nodes"]);
    }

    #[test]
    fn set_trs_pitch_yaw_z_writes_radians_and_does_not_mutate_on_error() {
        let mut model = pitch_model();
        let before = model.clone();
        assert!(set_trs(
            &mut model,
            &json!({"parameter": 9, "nodes": [4], "key": [0, 0], "rotation_x": "nope"})
        )
        .is_err());
        assert_eq!(model, before);
        set_trs(
            &mut model,
            &json!({"parameter": 9, "nodes": [4], "key": [2, 0], "rotation_x": 0.0, "translation_z": 0.0}),
        )
        .unwrap();
        let bindings = model["param"][0]["bindings"].as_array().unwrap();
        let rx = bindings
            .iter()
            .find(|binding| binding["param_name"] == "transform.r.x")
            .unwrap();
        assert_eq!(rx["values"][2][0], 0.0);
        assert_eq!(rx["isSet"][2][0], true);
        let tz = bindings
            .iter()
            .find(|binding| binding["param_name"] == "transform.t.z")
            .unwrap();
        assert_eq!(tz["values"][2][0], 0.0);
    }

    #[test]
    fn deform_raster_interpolates_like_parameter_binding_apply() {
        let model = deform_model("Linear");
        let on_key = raster_deformations(&model, &BTreeMap::from([(9, [0.25, 0.0])])).unwrap();
        assert_eq!(on_key[&2][0], [0.0, 0.0]);
        let halfway = raster_deformations(&model, &BTreeMap::from([(9, [0.5, 0.0])])).unwrap();
        assert!((halfway[&2][0][0] - 2.0).abs() < 1e-9, "{:?}", halfway[&2][0]);

        let nearest = deform_model("Nearest");
        let snapped = raster_deformations(&nearest, &BTreeMap::from([(9, [0.5, 0.0])])).unwrap();
        assert_eq!(snapped[&2][0], [4.0, 0.0]);

        let cubic = deform_model("Cubic");
        let curved = raster_deformations(&cubic, &BTreeMap::from([(9, [0.5, 0.0])])).unwrap();
        assert!((curved[&2][0][0] - 2.25).abs() < 1e-9, "{:?}", curved[&2][0]);

        let mut sparse = deform_model("Linear");
        sparse["param"][0]["bindings"][0]["values"] = json!([
            [[]], [[[0.0, 1.0]]], [[]], [[]]
        ]);
        let armed = raster_deformations(&sparse, &BTreeMap::from([(9, [0.25, 0.0])])).unwrap();
        assert_eq!(armed[&2], vec![[0.0, 1.0]]);
        assert!(raster_deformations(&sparse, &BTreeMap::from([(9, [0.5, 0.0])]))
            .unwrap_err()
            .contains("length mismatch"));

        let mut mismatched = deform_model("Linear");
        mismatched["param"][0]["bindings"][0]["values"][2][0] = json!([[4.0, 0.0], [4.0, 0.0]]);
        let before = mismatched.clone();
        assert!(raster_deformations(&mismatched, &BTreeMap::from([(9, [0.5, 0.0])]))
            .unwrap_err()
            .contains("length mismatch"));
        assert_eq!(mismatched, before);

        let stepped = deform_model("Stepped");
        let before = stepped.clone();
        assert!(raster_deformations(&stepped, &BTreeMap::from([(9, [0.5, 0.0])]))
            .unwrap_err()
            .contains("unsupported"));
        assert_eq!(stepped, before);

        let mut offsets = halfway;
        overlay_preview(&mut offsets, &BTreeMap::from([(2, vec![[0.0, 0.0]])]));
        assert_eq!(offsets[&2][0], [0.0, 0.0]);
    }

    #[test]
    fn large_sparse_deform_raster_reconstructs_deferred_cells_read_only() {
        let axis_count = MAX_AXIS_POINTS;
        let axis = (0..axis_count)
            .map(|index| index as f64 / (axis_count - 1) as f64)
            .collect::<Vec<_>>();
        let offsets = vec![[1.25, -2.5]; 3_922];
        let mut binding = json!({
            "node": 2,
            "param_name": "deform",
            "interpolate_mode": "Linear",
            "values": vec![json!([[]]); axis_count],
            "isSet": vec![json!([false]); axis_count]
        });
        // 3,922 vertices across 255 unset cells exceeds the 1M cache budget.
        crate::deformers::update_deform_key(
            &mut binding,
            2,
            [0, 0],
            &offsets,
            &axis,
            &[0.0],
            false,
            crate::deformers::UnsetFillOverflow::Defer,
            false,
        )
        .unwrap();
        let model = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 0.0],
                "defaults": [0.0, 0.0],
                "axis_points": [axis, [0.0]],
                "bindings": [binding]
            }]
        });
        let before = model.clone();
        let exact = raster_deformations(&model, &BTreeMap::from([(9, [128.0 / 255.0, 0.0])]))
            .unwrap();
        assert_eq!(exact[&2].len(), offsets.len());
        assert_eq!(exact[&2][0], [1.25, -2.5]);
        assert_eq!(exact[&2][offsets.len() - 1], [1.25, -2.5]);

        let off_key = (127.5 / 255.0, 0.0);
        let halfway = raster_deformations(&model, &BTreeMap::from([(9, [off_key.0, off_key.1])]))
            .unwrap();
        assert_eq!(halfway[&2].len(), offsets.len());
        assert_eq!(halfway[&2][0], [1.25, -2.5]);
        assert_eq!(model, before);
    }

    #[test]
    fn lazy_two_dimensional_deform_fill_matches_irregular_eager_oracle() {
        let model = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": true,
                "min": [0.0, 0.0],
                "max": [1.0, 1.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 0.2, 1.0], [0.0, 0.4, 1.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "interpolate_mode": "Linear",
                    "isSet": [[true, false, true], [false, false, false], [true, false, true]],
                    "values": [
                        [[[0.0, 0.0], [2.0, 1.0]], [], [[20.0, 0.0], [-4.0, 8.0]]],
                        [[], [], []],
                        [[[0.0, 10.0], [6.0, -2.0]], [], [[30.0, 40.0], [9.0, 3.0]]]
                    ]
                }]
            }]
        });
        let before = model.clone();
        let eager = eager_deform_cache(&model, true);
        for value in [[0.2, 0.4], [0.6, 0.7]] {
            assert_eq!(
                raster_deformations(&model, &BTreeMap::from([(9, value)])).unwrap(),
                raster_deformations(&eager, &BTreeMap::from([(9, value)])).unwrap()
            );
        }
        assert_eq!(model, before);

        let corner = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": true,
                "min": [0.0, 0.0],
                "max": [1.0, 1.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 0.2, 1.0], [0.0, 0.4, 1.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "isSet": [[true, true, false], [true, false, false], [false, false, false]],
                    "values": [
                        [[[1.0, 0.0], [4.0, 5.0]], [[4.0, 0.0], [8.0, 2.0]], []],
                        [[[3.0, 0.0], [-2.0, 7.0]], [], []],
                        [[], [], []]
                    ]
                }]
            }]
        });
        let corner_eager = eager_deform_cache(&corner, true);
        let corner_value = [0.2, 0.4];
        let corner_actual = raster_deformations(&corner, &BTreeMap::from([(9, corner_value)])).unwrap();
        assert_eq!(
            corner_actual,
            raster_deformations(&corner_eager, &BTreeMap::from([(9, corner_value)])).unwrap()
        );
        assert_eq!(corner_actual[&2], vec![[6.0, 0.0], [2.0, 4.0]]);

        let diagonal = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": true,
                "min": [0.0, 0.0],
                "max": [100.0, 1.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0, 1.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "isSet": [[false, true], [true, false]],
                    "values": [
                        [[], [[10.0, 0.0]]],
                        [[[20.0, 0.0]], []]
                    ]
                }]
            }]
        });
        assert_eq!(
            raster_deformations(&diagonal, &BTreeMap::from([(9, [0.0, 0.0])])).unwrap()[&2][0],
            [15.0, 0.0]
        );
    }

    #[test]
    fn lazy_deform_fill_supports_nearest_cubic_null_flags_and_preserves_caches() {
        let mut nearest = deform_model("Nearest");
        nearest["param"][0]["bindings"][0]["isSet"][1][0] = json!(false);
        nearest["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        let nearest_eager = eager_deform_cache(&nearest, false);
        let value = [0.25, 0.0];
        assert_eq!(
            raster_deformations(&nearest, &BTreeMap::from([(9, value)])).unwrap(),
            raster_deformations(&nearest_eager, &BTreeMap::from([(9, value)])).unwrap()
        );

        let mut cubic = deform_model("Cubic");
        cubic["param"][0]["bindings"][0]["isSet"][1][0] = json!(false);
        cubic["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        let cubic_eager = eager_deform_cache(&cubic, false);
        let value = [0.5, 0.0];
        assert_eq!(
            raster_deformations(&cubic, &BTreeMap::from([(9, value)])).unwrap(),
            raster_deformations(&cubic_eager, &BTreeMap::from([(9, value)])).unwrap()
        );

        let mut populated = nearest.clone();
        populated["param"][0]["bindings"][0]["values"][1][0] = json!([[9.0, -3.0]]);
        assert_eq!(
            raster_deformations(&populated, &BTreeMap::from([(9, [0.25, 0.0])])).unwrap()[&2][0],
            [9.0, -3.0]
        );

        let mut null_false = nearest.clone();
        null_false["param"][0]["bindings"][0]["isSet"][1][0] = Value::Null;
        null_false["param"][0]["bindings"][0]["values"][1][0] = Value::Null;
        assert_eq!(
            raster_deformations(&null_false, &BTreeMap::from([(9, [0.25, 0.0])])).unwrap(),
            raster_deformations(&nearest_eager, &BTreeMap::from([(9, [0.25, 0.0])])).unwrap()
        );

        let mut no_authored_keys = nearest;
        for index in 0..4 {
            no_authored_keys["param"][0]["bindings"][0]["isSet"][index][0] = json!(false);
            no_authored_keys["param"][0]["bindings"][0]["values"][index][0] = json!([]);
        }
        assert!(raster_deformations(&no_authored_keys, &BTreeMap::from([(9, [0.5, 0.0])]))
            .unwrap()
            .is_empty());

        let scalar_with_unused_y_axis = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 0.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0, 1.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "interpolate_mode": "Nearest",
                    "isSet": [[true], [false]],
                    "values": [[[[1.0, 0.0]]], [[]]]
                }]
            }]
        });
        assert_eq!(
            raster_deformations(
                &scalar_with_unused_y_axis,
                &BTreeMap::from([(9, [1.0, 0.0])])
            )
            .unwrap()[&2][0],
            [1.0, 0.0]
        );
    }

    #[test]
    fn lazy_deform_fill_keeps_authored_empty_and_malformed_length_errors() {
        let mut authored_empty = deform_model("Nearest");
        authored_empty["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        let before = authored_empty.clone();
        assert!(raster_deformations(&authored_empty, &BTreeMap::from([(9, [0.25, 0.0])]))
            .unwrap()
            .is_empty());
        assert_eq!(authored_empty, before);

        let mut mismatched = deform_model("Nearest");
        mismatched["param"][0]["bindings"][0]["isSet"][1][0] = json!(false);
        mismatched["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        mismatched["param"][0]["bindings"][0]["values"][2][0] = json!([[4.0, 0.0], [5.0, 0.0]]);
        let before = mismatched.clone();
        assert!(raster_deformations(&mismatched, &BTreeMap::from([(9, [0.25, 0.0])]))
            .unwrap_err()
            .contains("length mismatch"));
        assert_eq!(mismatched, before);

        let mut malformed = deform_model("Nearest");
        malformed["param"][0]["bindings"][0]["isSet"][1][0] = json!(false);
        malformed["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        malformed["param"][0]["bindings"][0]["values"][0][0] = json!([["bad", 0.0]]);
        let before = malformed.clone();
        assert!(raster_deformations(&malformed, &BTreeMap::from([(9, [0.25, 0.0])]))
            .unwrap_err()
            .contains("must be a number"));
        assert_eq!(malformed, before);

        let mut empty_source = deform_model("Nearest");
        empty_source["param"][0]["bindings"][0]["isSet"][0][0] = json!(true);
        empty_source["param"][0]["bindings"][0]["values"][0][0] = json!([]);
        empty_source["param"][0]["bindings"][0]["isSet"][1][0] = json!(false);
        empty_source["param"][0]["bindings"][0]["values"][1][0] = json!([]);
        let before = empty_source.clone();
        assert!(raster_deformations(&empty_source, &BTreeMap::from([(9, [0.25, 0.0])]))
            .unwrap_err()
            .contains("length mismatch"));
        assert_eq!(empty_source, before);

        let weighted_candidate = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 0.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 0.5, 1.0], [0.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "interpolate_mode": "Nearest",
                    "isSet": [[true], [false], [true]],
                    "values": [[[]], [[]], [[[10.0, 1.0], [20.0, 2.0]]]]
                }]
            }]
        });
        let before = weighted_candidate.clone();
        assert!(raster_deformations(&weighted_candidate, &BTreeMap::from([(9, [0.5, 0.0])]))
            .unwrap_err()
            .contains("length mismatch"));
        assert_eq!(weighted_candidate, before);

        let empty_source_outside_extension = json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 0.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 0.25, 0.5, 0.75, 1.0], [0.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "interpolate_mode": "Nearest",
                    "isSet": [[true], [false], [true], [true], [false]],
                    "values": [[[]], [[]], [[[10.0, 0.0]]], [[[20.0, 0.0]]], [[]]]
                }]
            }]
        });
        let before = empty_source_outside_extension.clone();
        assert_eq!(
            raster_deformations(
                &empty_source_outside_extension,
                &BTreeMap::from([(9, [1.0, 0.0])])
            )
            .unwrap()[&2][0],
            [20.0, 0.0]
        );
        assert_eq!(empty_source_outside_extension, before);
    }

    fn eager_deform_cache(model: &Value, is_vec2: bool) -> Value {
        let parameter = &model["param"][0];
        let binding = &parameter["bindings"][0];
        let axes = parameter_axes(parameter, is_vec2).unwrap();
        let raw = binding["values"].as_array().unwrap();
        let mut offsets = raw
            .iter()
            .map(|column| {
                column
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|cell| parse_offset_cell(cell, 9).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let flags = binding_flags(binding, &axes, is_vec2, "binding").unwrap();
        let vertex_count = offsets
            .iter()
            .flatten()
            .find(|cell| !cell.is_empty())
            .map(Vec::len)
            .unwrap_or(0);
        for x in 0..offsets.len() {
            for y in 0..offsets[x].len() {
                if !flags[x][y] {
                    offsets[x][y] = vec![[0.0, 0.0]; vertex_count];
                }
            }
        }
        crate::deformers::fill_unset_offsets(&mut offsets, &flags, &axes[0], &axes[1], false).unwrap();
        let mut eager = model.clone();
        eager["param"][0]["bindings"][0]["values"] = json!(offsets);
        eager
    }

    fn deform_model(mode: &str) -> Value {
        json!({
            "nodes": {"uuid": 1, "type": "Node", "children": []},
            "automation": [],
            "param": [{
                "uuid": 9,
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 0.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 0.25, 0.75, 1.0], [0.0]],
                "bindings": [{
                    "node": 2,
                    "param_name": "deform",
                    "interpolate_mode": mode,
                    "isSet": [[true], [true], [true], [true]],
                    "values": [
                        [[[0.0, 0.0]]],
                        [[[0.0, 0.0]]],
                        [[[4.0, 0.0]]],
                        [[[0.0, 0.0]]]
                    ]
                }]
            }]
        })
    }
}
