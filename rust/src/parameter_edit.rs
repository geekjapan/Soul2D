use crate::evaluation;
use serde_json::{json, Value};
use std::collections::HashMap;

const MAX_AXIS_POINTS: usize = 256;
const MAX_MATCH_PAIRS: usize = 1_000_000;
const MAX_REMAP_VALUES: usize = 1_000_000;

pub(crate) fn delete_parameter(model: &mut Value, id: u32) -> Result<(), String> {
    if id == 0 {
        return Err("parameter UUID must be nonzero".into());
    }
    let parameters = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or("model.param must be an array")?;
    find_parameter(parameters, id)?
        .ok_or_else(|| format!("parameter {id} was not found"))?;

    let parameters = model
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .ok_or("model.param must be an array")?;
    remove_parameter(parameters, id);
    strip_links_to(parameters, id);
    if let Some(nodes) = model.get_mut("nodes") {
        clear_physics_parameter(nodes, id);
    }
    if let Some(animations) = model.get_mut("animations").and_then(Value::as_object_mut) {
        for animation in animations.values_mut() {
            if let Some(lanes) = animation.get_mut("lanes").and_then(Value::as_array_mut) {
                lanes.retain(|lane| lane.get("uuid").and_then(Value::as_u64) != Some(u64::from(id)));
            }
            let lists = animation.as_object_mut().into_iter().flat_map(|object| object.iter_mut());
            let lists = lists.filter(|(key, _)| *key == "bookmarks" || *key == "controllers");
            for label in lists.flat_map(|(_, list)| list.as_array_mut().into_iter().flatten()) {
                if let Some(ids) = label.get_mut("parameters").and_then(Value::as_array_mut) {
                    ids.retain(|listed| listed.as_u64() != Some(u64::from(id)));
                }
            }
        }
    }
    drop_eye(model, id);
    if let Some(motion_bindings) = model
        .get_mut("live2d")
        .and_then(|live2d| live2d.get_mut("motionBindings"))
        .and_then(Value::as_object_mut)
    {
        for bindings in motion_bindings.values_mut().filter_map(Value::as_array_mut) {
            bindings.retain(|binding| {
                binding.get("parameter").and_then(Value::as_u64) != Some(u64::from(id))
            });
        }
    }
    Ok(())
}

/// Takes parameter `id` off the eye blinking list, which leaves no key once it is empty.
pub(crate) fn drop_eye(model: &mut Value, id: u32) {
    let Some(live2d) = model.get_mut("live2d").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(eyes) = live2d.get_mut("eyeBlink").and_then(Value::as_array_mut) {
        eyes.retain(|eye| eye.as_u64() != Some(u64::from(id)));
        if eyes.is_empty() {
            live2d.remove("eyeBlink");
        }
    }
}

pub(crate) fn set_axes(
    model: &mut Value,
    id: u32,
    min: [f64; 2],
    max: [f64; 2],
    axis_x: &[f64],
    axis_y: &[f64],
) -> Result<(), String> {
    if id == 0 {
        return Err("parameter UUID must be nonzero".into());
    }
    let parameters = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or("model.param must be an array")?;
    let parameter = find_parameter(parameters, id)?
        .ok_or_else(|| format!("parameter {id} was not found"))?;
    let is_vec2 = match parameter.get("is_vec2") {
        Some(value) => value
            .as_bool()
            .ok_or("parameter is_vec2 must be a boolean")?,
        None => false,
    };
    validate_range(min, max)?;
    validate_axis(axis_x, "axisX", true)?;
    validate_axis(axis_y, "axisY", is_vec2)?;
    let old_axes = stored_axes(parameter, is_vec2)?;
    let repeat = evaluation::parameter_repeats(parameter);
    let new_axes = [axis_x.to_vec(), axis_y.to_vec()];

    let mut remapped_bindings = None;
    if old_axes != new_axes {
        let pairs = matching_pairs(&old_axes, &new_axes, is_vec2)?;
        if let Some(raw) = parameter.get("bindings") {
            let bindings = raw
                .as_array()
                .ok_or("parameter.bindings must be an array")?;
            let mut remaining_values = MAX_REMAP_VALUES;
            remapped_bindings = Some(
                bindings
                    .iter()
                    .map(|binding| remap_binding(binding, &old_axes, &new_axes, is_vec2, &pairs,
                        &mut remaining_values, repeat))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
    } else if let Some(raw) = parameter.get("bindings") {
        if !raw.is_array() {
            return Err("parameter.bindings must be an array".into());
        }
    }

    let parameter = find_parameter_mut(
        model
            .get_mut("param")
            .and_then(Value::as_array_mut)
            .ok_or("model.param must be an array")?,
        id,
    )
    .ok_or_else(|| format!("parameter {id} was not found"))?;
    let object = parameter
        .as_object_mut()
        .ok_or("parameter must be an object")?;
    object.insert("min".into(), json!(min));
    object.insert("max".into(), json!(max));
    if old_axes != new_axes {
        object.insert("axis_points".into(), json!(new_axes));
        if let Some(bindings) = remapped_bindings {
            object.insert("bindings".into(), json!(bindings));
        }
    }
    Ok(())
}

/// A deleted parameter can no longer be driven: its link bindings go, as D drops them when a puppet loads.
fn strip_links_to(parameters: &mut [Value], id: u32) {
    for parameter in parameters {
        if let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) {
            bindings.retain(|binding| {
                !(evaluation::link_axis(binding).is_some()
                    && binding.get("node").and_then(Value::as_u64) == Some(u64::from(id)))
            });
        }
        if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
            strip_links_to(children, id);
        }
    }
}

fn find_parameter(parameters: &[Value], id: u32) -> Result<Option<&Value>, String> {
    for parameter in parameters {
        if parameter.get("groupUUID").and_then(Value::as_u64) == Some(u64::from(id)) {
            return Err(format!("parameter {id} refers to a parameter group"));
        }
        if parameter.get("groupUUID").is_none()
            && parameter.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
        {
            return Ok(Some(parameter));
        }
        if let Some(children) = parameter.get("children").and_then(Value::as_array) {
            if let Some(found) = find_parameter(children, id)? {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn find_parameter_mut(parameters: &mut [Value], id: u32) -> Option<&mut Value> {
    for parameter in parameters {
        if parameter.get("groupUUID").is_none()
            && parameter.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
        {
            return Some(parameter);
        }
        if parameter.get("groupUUID").is_some() {
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = find_parameter_mut(children, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn remove_parameter(parameters: &mut Vec<Value>, id: u32) -> bool {
    let mut index = 0;
    while index < parameters.len() {
        if parameters[index].get("groupUUID").is_none()
            && parameters[index].get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
        {
            parameters.remove(index);
            return true;
        }
        if parameters[index].get("groupUUID").is_some() {
            if let Some(children) = parameters[index]
                .get_mut("children")
                .and_then(Value::as_array_mut)
            {
                if remove_parameter(children, id) {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn clear_physics_parameter(node: &mut Value, id: u32) {
    if node.get("type").and_then(Value::as_str) == Some("SimplePhysics")
        && node.get("param").and_then(Value::as_u64) == Some(u64::from(id))
    {
        node["param"] = json!(crate::physics::UNASSIGNED_PARAM);
    }
    if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            clear_physics_parameter(child, id);
        }
    }
}

fn validate_range(min: [f64; 2], max: [f64; 2]) -> Result<(), String> {
    for axis in 0..2 {
        if !min[axis].is_finite() || !max[axis].is_finite() || min[axis] > max[axis] {
            return Err(format!("parameter range on axis {axis} is invalid"));
        }
    }
    Ok(())
}

fn validate_axis(axis: &[f64], name: &str, active: bool) -> Result<(), String> {
    if axis.len() > MAX_AXIS_POINTS {
        return Err(format!("{name} exceeds the supported axis size"));
    }
    if !active {
        return if axis == [0.0] {
            Ok(())
        } else {
            Err("1D parameter Y axis must be [0]".into())
        };
    }
    if axis.len() < 2
        || axis[0] != 0.0
        || axis[axis.len() - 1] != 1.0
        || axis.iter().any(|value| !value.is_finite())
        || axis.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(format!("{name} must strictly increase from 0 to 1"));
    }
    Ok(())
}

fn stored_axes(parameter: &Value, is_vec2: bool) -> Result<[Vec<f64>; 2], String> {
    let raw = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .filter(|axes| axes.len() == 2)
        .ok_or("parameter.axis_points must contain two axis arrays")?;
    let read = |value: &Value, name: &str, active: bool| -> Result<Vec<f64>, String> {
        let values = value
            .as_array()
            .ok_or_else(|| format!("{name} must be an array"))?
            .iter()
            .map(|value| crate::finite_number(value, name))
            .collect::<Result<Vec<_>, _>>()?;
        validate_axis(&values, name, active)?;
        Ok(values)
    };
    Ok([
        read(&raw[0], "parameter.axis_points[0]", true)?,
        read(&raw[1], "parameter.axis_points[1]", is_vec2)?,
    ])
}

#[derive(Clone, Copy)]
struct CellPair {
    new: usize,
    old: usize,
    distance: f64,
}

fn matching_pairs(
    old: &[Vec<f64>; 2],
    new: &[Vec<f64>; 2],
    is_vec2: bool,
) -> Result<Vec<Option<usize>>, String> {
    let old_y = if is_vec2 { old[1].len() } else { 1 };
    let new_y = if is_vec2 { new[1].len() } else { 1 };
    let old_count = old[0].len() * old_y;
    let new_count = new[0].len() * new_y;
    let pair_count = old_count
        .checked_mul(new_count)
        .filter(|count| *count <= MAX_MATCH_PAIRS)
        .ok_or("parameter axes require too many cell matches")?;
    let mut pairs = Vec::with_capacity(pair_count);
    for new_x in 0..new[0].len() {
        for new_y_index in 0..new_y {
            let new_index = new_x * new_y + new_y_index;
            for old_x in 0..old[0].len() {
                for old_y_index in 0..old_y {
                    let old_index = old_x * old_y + old_y_index;
                    let distance = (new[0][new_x] - old[0][old_x]).abs()
                        + if is_vec2 {
                            (new[1][new_y_index] - old[1][old_y_index]).abs()
                        } else {
                            0.0
                        };
                    pairs.push(CellPair {
                        new: new_index,
                        old: old_index,
                        distance,
                    });
                }
            }
        }
    }
    pairs.sort_unstable_by(|a, b| {
        a.distance
            .total_cmp(&b.distance)
            .then_with(|| a.new.cmp(&b.new))
            .then_with(|| a.old.cmp(&b.old))
    });
    let mut result = vec![None; new_count];
    let mut used_old = vec![false; old_count];
    let mut matched = 0;
    for pair in pairs {
        if result[pair.new].is_none() && !used_old[pair.old] {
            result[pair.new] = Some(pair.old);
            used_old[pair.old] = true;
            matched += 1;
            if matched == old_count.min(new_count) {
                break;
            }
        }
    }
    Ok(result)
}

fn remap_binding(
    binding: &Value,
    old: &[Vec<f64>; 2],
    new: &[Vec<f64>; 2],
    is_vec2: bool,
    pairs: &[Option<usize>],
    remaining_values: &mut usize,
    repeat: bool,
) -> Result<Value, String> {
    let object = binding.as_object().ok_or("parameter binding must be an object")?;
    let Some(raw_channel) = object.get("param_name") else {
        return Ok(binding.clone());
    };
    let channel = raw_channel
        .as_str()
        .ok_or("binding.param_name must be a string")?;
    if channel == "deform" {
        remap_deform_binding(binding, old, new, is_vec2, pairs, remaining_values, repeat)
    } else {
        remap_value_binding(binding, old, new, is_vec2, pairs, remaining_values, repeat)
    }
}

fn binding_flags(binding: &Value, x_count: usize, y_count: usize) -> Result<Vec<Vec<bool>>, String> {
    let raw = binding
        .get("isSet")
        .and_then(Value::as_array)
        .ok_or("binding.isSet must be an array")?;
    if raw.len() != x_count {
        return Err("binding.isSet has a mismatched X axis".into());
    }
    raw.iter()
        .map(|column| {
            let column = column
                .as_array()
                .ok_or("binding.isSet columns must be arrays")?;
            if column.len() != y_count {
                return Err("binding.isSet has a mismatched Y axis".into());
            }
            column
                .iter()
                .map(|flag| match flag {
                    Value::Bool(value) => Ok(*value),
                    Value::Null => Ok(false),
                    _ => Err("binding.isSet entries must be booleans".into()),
                })
                .collect()
        })
        .collect()
}

fn remap_value_binding(
    binding: &Value,
    old: &[Vec<f64>; 2],
    new: &[Vec<f64>; 2],
    is_vec2: bool,
    pairs: &[Option<usize>],
    remaining_values: &mut usize,
    repeat: bool,
) -> Result<Value, String> {
    let old_y = if is_vec2 { old[1].len() } else { 1 };
    let new_y = if is_vec2 { new[1].len() } else { 1 };
    let raw = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or("binding.values must be an array")?;
    if raw.len() != old[0].len() {
        return Err("binding.values has a mismatched X axis".into());
    }
    let values = raw
        .iter()
        .map(|column| {
            let column = column
                .as_array()
                .ok_or("binding.values columns must be arrays")?;
            if column.len() != old_y {
                return Err("binding.values has a mismatched Y axis".into());
            }
            column
                .iter()
                .map(|value| crate::finite_number(value, "binding.values"))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let flags = binding_flags(binding, old[0].len(), old_y)?;
    consume_remap_budget(remaining_values, new[0].len(), new_y, 1)?;
    let mut next_values = vec![vec![0.0; new_y]; new[0].len()];
    let mut next_flags = vec![vec![false; new_y]; new[0].len()];
    for (new_index, old_index) in pairs.iter().enumerate() {
        let Some(old_index) = old_index else { continue };
        let (new_x, new_y_index) = (new_index / new_y, new_index % new_y);
        let (old_x, old_y_index) = (old_index / old_y, old_index % old_y);
        if flags[old_x][old_y_index] {
            next_values[new_x][new_y_index] = values[old_x][old_y_index];
            next_flags[new_x][new_y_index] = true;
        }
    }
    evaluation::reinterpolate(&mut next_values, &next_flags, &new[0], &new[1], repeat)?;
    let mut result = binding.clone();
    result["values"] = json!(next_values);
    result["isSet"] = json!(next_flags);
    Ok(result)
}

fn remap_deform_binding(
    binding: &Value,
    old: &[Vec<f64>; 2],
    new: &[Vec<f64>; 2],
    is_vec2: bool,
    pairs: &[Option<usize>],
    remaining_values: &mut usize,
    repeat: bool,
) -> Result<Value, String> {
    let old_y = if is_vec2 { old[1].len() } else { 1 };
    let new_y = if is_vec2 { new[1].len() } else { 1 };
    let raw = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or("binding.values must be an array")?;
    if raw.len() != old[0].len() {
        return Err("binding.values has a mismatched X axis".into());
    }
    let values = raw
        .iter()
        .map(|column| {
            let column = column
                .as_array()
                .ok_or("binding.values columns must be arrays")?;
            if column.len() != old_y {
                return Err("binding.values has a mismatched Y axis".into());
            }
            column
                .iter()
                .map(parse_offsets)
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let flags = binding_flags(binding, old[0].len(), old_y)?;
    let vertex_count = values
        .iter()
        .zip(&flags)
        .flat_map(|(columns, flags)| columns.iter().zip(flags))
        .find_map(|(offsets, set)| (*set && !offsets.is_empty()).then_some(offsets.len()))
        .or_else(|| {
            values
                .iter()
                .flatten()
                .find(|offsets| !offsets.is_empty())
                .map(Vec::len)
        })
        .unwrap_or(0);
    for (columns, set_column) in values.iter().zip(&flags) {
        for (offsets, set) in columns.iter().zip(set_column) {
            if *set && offsets.len() != vertex_count {
                return Err("deform binding values have inconsistent vertex counts".into());
            }
        }
    }
    consume_remap_budget(remaining_values, new[0].len(), new_y, vertex_count.saturating_mul(2).max(1))?;
    let mut next_values = vec![vec![vec![[0.0, 0.0]; vertex_count]; new_y]; new[0].len()];
    let mut next_flags = vec![vec![false; new_y]; new[0].len()];
    for (new_index, old_index) in pairs.iter().enumerate() {
        let Some(old_index) = old_index else { continue };
        let (new_x, new_y_index) = (new_index / new_y, new_index % new_y);
        let (old_x, old_y_index) = (old_index / old_y, old_index % old_y);
        if flags[old_x][old_y_index] {
            next_values[new_x][new_y_index] = values[old_x][old_y_index].clone();
            next_flags[new_x][new_y_index] = true;
        }
    }
    crate::deformers::fill_unset_offsets(
        &mut next_values,
        &next_flags,
        &new[0],
        &new[1],
        repeat,
    )?;
    let mut result = binding.clone();
    result["values"] = json!(next_values);
    result["isSet"] = json!(next_flags);
    Ok(result)
}

fn consume_remap_budget(remaining: &mut usize, x: usize, y: usize, components: usize) -> Result<(), String> {
    let count = x.checked_mul(y).and_then(|cells| cells.checked_mul(components))
        .ok_or("parameter remap exceeds the output size limit")?;
    *remaining = remaining.checked_sub(count).ok_or("parameter remap exceeds the output size limit")?;
    Ok(())
}

fn parse_offsets(value: &Value) -> Result<Vec<[f64; 2]>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(points) => points
            .iter()
            .map(|point| {
                let pair = point
                    .as_array()
                    .filter(|pair| pair.len() >= 2)
                    .ok_or("deform binding offsets must be coordinate pairs")?;
                Ok([
                    crate::finite_number(&pair[0], "deform binding offset")?,
                    crate::finite_number(&pair[1], "deform binding offset")?,
                ])
            })
            .collect(),
        _ => Err("deform binding values must be arrays".into()),
    }
}

// ---------------------------------------------------------------------------
// Parameter panel operations: duplicate, groups, ordering, flip/mirror, defaults, split, bindings.
// Each function edits a candidate model; the session commits it as one undo step.
// ---------------------------------------------------------------------------

fn is_group(value: &Value) -> bool {
    value.get("groupUUID").is_some()
}

fn uuid_of(value: &Value) -> Option<u64> {
    value.get("uuid").and_then(Value::as_u64)
}

/// Where a parameter or a group lives: the group that holds it, at any depth (`None` for the top level), and its
/// index there. A group id names one group: the validation refuses a resource UUID that is used twice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Slot {
    pub(crate) group: Option<u32>,
    index: usize,
}

fn top_list(model: &mut Value) -> Result<&mut Vec<Value>, String> {
    model
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "model.param must be an array".to_string())
}

fn group_id(item: &Value) -> Option<u32> {
    item.get("groupUUID").and_then(Value::as_u64).and_then(crate::valid_id)
}

/// The slot of the parameter (`group` false) or the group (`group` true) with this id, at any depth.
fn locate(list: &[Value], id: u32, group: bool, parent: Option<u32>) -> Option<Slot> {
    for (index, item) in list.iter().enumerate() {
        let found = match group {
            true => group_id(item) == Some(id),
            false => !is_group(item) && uuid_of(item) == Some(u64::from(id)),
        };
        if found {
            return Some(Slot { group: parent, index });
        }
        if let (Some(inner), Some(children)) = (group_id(item), item.get("children").and_then(Value::as_array)) {
            if let Some(slot) = locate(children, id, group, Some(inner)) {
                return Some(slot);
            }
        }
    }
    None
}

pub(crate) fn locate_plain(model: &mut Value, id: u32) -> Result<Slot, String> {
    let list = top_list(model)?;
    if locate(list, id, true, None).is_some() {
        return Err(format!("parameter {id} refers to a parameter group"));
    }
    locate(list, id, false, None).ok_or_else(|| format!("parameter {id} was not found"))
}

fn children_of(list: &mut [Value], group: u32) -> Option<&mut Vec<Value>> {
    for item in list {
        let Some(inner) = group_id(item) else { continue };
        let Some(children) = item.get_mut("children").and_then(Value::as_array_mut) else { continue };
        if inner == group {
            return Some(children);
        }
        if let Some(found) = children_of(children, group) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn container_of(model: &mut Value, slot: Slot) -> Result<&mut Vec<Value>, String> {
    let list = top_list(model)?;
    match slot.group {
        None => Ok(list),
        Some(group) => children_of(list, group).ok_or_else(|| format!("parameter group {group} has no children array")),
    }
}

pub(crate) fn slot_index(slot: Slot) -> usize {
    slot.index
}

fn set_parent_uuid(parameter: &mut Value, group: Option<u32>) {
    if let Some(object) = parameter.as_object_mut() {
        match group {
            Some(group) => {
                object.insert("parentUUID".into(), json!(group));
            }
            None => {
                object.remove("parentUUID");
            }
        }
    }
}

fn find_group(model: &mut Value, id: u32) -> Result<Slot, String> {
    locate(top_list(model)?, id, true, None).ok_or_else(|| format!("parameter group {id} was not found"))
}

pub(crate) fn free_name(model: &Value, wanted: &str, variant: impl Fn(usize) -> String) -> String {
    let mut name = wanted.to_owned();
    let mut n = 1;
    while crate::check_parameter_name(model, &name, None).is_err() {
        n += 1;
        name = variant(n);
    }
    name
}

/// Copy a parameter next to the original, in the same container. The default name follows
/// `ExParameter.dup` ("<name> (Copy)"), made unique with a counter when it is taken.
pub(crate) fn duplicate_parameter(
    model: &mut Value,
    id: u32,
    new_id: u32,
    name: Option<&str>,
) -> Result<Value, String> {
    let slot = locate_plain(model, id)?;
    let mut copy = container_of(model, slot)?[slot_index(slot)].clone();
    let name = match name {
        Some(name) => {
            crate::check_parameter_name(model, name, None)?;
            name.to_owned()
        }
        None => {
            let base = copy.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
            free_name(model, &format!("{base} (Copy)"), |n| format!("{base} (Copy {n})"))
        }
    };
    copy["uuid"] = json!(new_id);
    copy["name"] = json!(name);
    container_of(model, slot)?.push(copy.clone());
    Ok(copy)
}

/// Create a parameter group; `members` move into it in the given order (drop-on-parameter gesture).
pub(crate) fn create_group(
    model: &mut Value,
    id: u32,
    name: Option<&str>,
    members: &[u32],
) -> Result<Value, String> {
    let name = match name {
        Some(name) => {
            crate::check_parameter_name(model, name, None)?;
            name.to_owned()
        }
        None => free_name(model, "New Parameter Group", |n| format!("New Parameter Group {n}")),
    };
    let mut seen = Vec::new();
    for member in members {
        if seen.contains(member) {
            return Err(format!("parameter {member} is listed twice"));
        }
        seen.push(*member);
        locate_plain(model, *member)?;
    }
    top_list(model)?.push(json!({
        "groupUUID": id, "name": name, "color": [0.15, 0.15, 0.15], "children": [],
    }));
    for member in members {
        move_parameter(model, *member, Some(id), None)?;
    }
    let group = find_group(model, id)?;
    Ok(container_of(model, group)?[group.index].clone())
}

/// Delete a group; its children return to the list that held the group (the top level, or the group around it) at
/// the group's position.
pub(crate) fn delete_group(model: &mut Value, id: u32) -> Result<Vec<Value>, String> {
    let slot = find_group(model, id)?;
    let index = slot.index;
    let list = container_of(model, slot)?;
    let group = list.remove(index);
    let mut children = group
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for child in &mut children {
        set_parent_uuid(child, slot.group);
    }
    for (offset, child) in children.iter().cloned().enumerate() {
        list.insert(index + offset, child);
    }
    Ok(children)
}

pub(crate) fn set_group_color(model: &mut Value, id: u32, color: [f64; 3]) -> Result<(), String> {
    if color.iter().any(|channel| !channel.is_finite()) {
        return Err("color must contain finite numbers".into());
    }
    let slot = find_group(model, id)?;
    container_of(model, slot)?[slot.index]["color"] = json!(color);
    Ok(())
}

/// Move a parameter into a group (`Some`) or to the top level (`None`), at `index` or the end.
/// Moving inside one container reorders it.
pub(crate) fn move_parameter(
    model: &mut Value,
    id: u32,
    group: Option<u32>,
    index: Option<usize>,
) -> Result<(), String> {
    let slot = locate_plain(model, id)?;
    let mut parameter = container_of(model, slot)?.remove(slot_index(slot));
    set_parent_uuid(&mut parameter, group);
    let target = match group {
        Some(group) => {
            find_group(model, group)?;
            container_of(model, Slot { group: Some(group), index: 0 })?
        }
        None => top_list(model)?,
    };
    let at = index.unwrap_or(target.len());
    if at > target.len() {
        return Err(format!("index {at} is outside the target list of {} entries", target.len()));
    }
    target.insert(at, parameter);
    Ok(())
}

/// `ExParameter.reverseAxis`: mirror the axis point layout and flip the key grids that follow it.
pub(crate) fn flip_axis(model: &mut Value, id: u32, axis: usize) -> Result<(), String> {
    let slot = locate_plain(model, id)?;
    let parameter = &mut container_of(model, slot)?[slot_index(slot)];
    let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    if axis > 1 || (axis == 1 && !is_vec2) {
        return Err("axis must be 0, or 1 for a 2D parameter".into());
    }
    let axes = stored_axes(parameter, is_vec2)?;
    let mut flipped = axes[axis].clone();
    flipped.reverse();
    let flipped: Vec<f64> = flipped.iter().map(|point| 1.0 - point).collect();
    parameter["axis_points"][axis] = json!(flipped);
    let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for binding in bindings {
        for key in ["values", "isSet"] {
            let grid = binding
                .get_mut(key)
                .and_then(Value::as_array_mut)
                .ok_or_else(|| format!("binding.{key} must be an array"))?;
            if axis == 0 {
                grid.reverse();
            } else {
                for column in grid {
                    column
                        .as_array_mut()
                        .ok_or_else(|| format!("binding.{key} columns must be arrays"))?
                        .reverse();
                }
            }
        }
    }
    Ok(())
}

/// `Node.scaleValue(key, value, axis, -1)`: whether mirroring over `axis` (None: invert) negates `channel`.
/// A parameter link ("X" / "Y") scales as a plain number, so it always negates.
pub(crate) fn mirror_negates(channel: &str, axis: Option<usize>) -> bool {
    match (axis, channel) {
        (None, _) | (_, "X" | "Y") => true,
        (_, "transform.r.z") => true,
        (Some(0), "transform.r.y" | "transform.t.x") => true,
        (Some(1), "transform.r.x" | "transform.t.y") => true,
        _ => false,
    }
}

/// Neutral value of a binding channel (`getDefaultValue` of Node, Part and SimplePhysics).
pub(crate) fn channel_default(channel: &str) -> f64 {
    match channel {
        "transform.s.x" | "transform.s.y" | "opacity" | "tint.r" | "tint.g" | "tint.b" | "emissionStrength"
        | "gravity" | "frequency" | "angleDamping" | "lengthDamping" | "outputScale.x" | "outputScale.y" => 1.0,
        _ => 0.0,
    }
}

/// The neutral value of `binding`: a link resets to the default of the parameter axis it drives.
pub(crate) fn neutral_of(binding: &Value, channel: &str, defaults: &HashMap<u32, [f64; 2]>) -> f64 {
    match (evaluation::link_axis(binding), binding.get("node").and_then(Value::as_u64)) {
        (Some(axis), Some(node)) => {
            u32::try_from(node).ok().and_then(|node| defaults.get(&node)).map_or(0.0, |pair| pair[axis])
        }
        _ => channel_default(channel),
    }
}

/// Default (starting) values of every plain parameter.
pub(crate) fn parameter_defaults(model: &Value) -> HashMap<u32, [f64; 2]> {
    fn walk(list: &[Value], out: &mut HashMap<u32, [f64; 2]>) {
        for item in list {
            if item.get("groupUUID").is_some() {
                if let Some(children) = item.get("children").and_then(Value::as_array) {
                    walk(children, out);
                }
            } else if let Some(id) = uuid_of(item).and_then(|id| u32::try_from(id).ok()) {
                let pair = item.get("defaults").and_then(|d| crate::parse_pair(d, "defaults").ok());
                out.insert(id, pair.unwrap_or([0.0, 0.0]));
            }
        }
    }
    let mut out = HashMap::new();
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        walk(list, &mut out);
    }
    out
}

enum Cells<'a> {
    AllSet,
    At(&'a [(usize, usize)]),
}

enum CellEdit {
    Scale(Option<usize>),
    Unset,
    Reset,
    /// Mark the key as set at its current (interpolated) value.
    SetCurrent,
}

/// Apply one edit to cells of one binding and re-interpolate, like `scaleValueAt`/`unset`/`reset`.
fn edit_binding_cells(
    binding: &mut Value,
    axes: &[Vec<f64>; 2],
    is_vec2: bool,
    cells: &Cells,
    edit: &CellEdit,
    defaults: &HashMap<u32, [f64; 2]>,
    repeat: bool,
) -> Result<(), String> {
    let x_count = axes[0].len();
    let y_count = if is_vec2 { axes[1].len() } else { 1 };
    let channel = binding
        .get("param_name")
        .and_then(Value::as_str)
        .ok_or("only value and deform bindings can be edited")?
        .to_owned();
    let neutral = neutral_of(binding, &channel, defaults);
    let raw = binding
        .get("values")
        .and_then(Value::as_array)
        .filter(|columns| columns.len() == x_count)
        .ok_or("binding.values has a mismatched X axis")?
        .clone();
    let mut flags = binding_flags(binding, x_count, y_count)?;
    let targets: Vec<(usize, usize)> = match cells {
        Cells::AllSet => (0..x_count)
            .flat_map(|x| (0..y_count).map(move |y| (x, y)))
            .filter(|(x, y)| flags[*x][*y])
            .collect(),
        Cells::At(list) => list.to_vec(),
    };
    if targets.iter().any(|(x, y)| *x >= x_count || *y >= y_count) {
        return Err("key is outside the parameter grid".into());
    }
    if targets.is_empty() {
        return Ok(());
    }
    let sets_flag = !matches!(edit, CellEdit::Unset);
    if channel == "deform" {
        let mut grid = raw
            .iter()
            .map(|column| {
                column
                    .as_array()
                    .filter(|column| column.len() == y_count)
                    .ok_or("binding.values has a mismatched Y axis")?
                    .iter()
                    .map(parse_offsets)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, String>>()?;
        let vertex_count = grid
            .iter()
            .zip(&flags)
            .flat_map(|(column, set)| column.iter().zip(set))
            .find_map(|(offsets, set)| (*set && !offsets.is_empty()).then_some(offsets.len()))
            .or_else(|| grid.iter().flatten().find(|o| !o.is_empty()).map(Vec::len))
            .unwrap_or(0);
        // Only the cells this edit allocates count: a binding with every cell stored is edited in place.
        let allocated = grid.iter().flatten().filter(|offsets| offsets.len() != vertex_count).count();
        let mut remaining_values = MAX_REMAP_VALUES;
        consume_remap_budget(&mut remaining_values, allocated, 1, vertex_count.saturating_mul(2).max(1))?;
        for (column, set) in grid.iter_mut().zip(&flags) {
            for (offsets, set) in column.iter_mut().zip(set) {
                if offsets.len() != vertex_count {
                    if *set {
                        return Err("deform binding values have inconsistent vertex counts".into());
                    }
                    *offsets = vec![[0.0, 0.0]; vertex_count];
                }
            }
        }
        for (x, y) in &targets {
            match edit {
                CellEdit::Scale(axis) => {
                    for offset in &mut grid[*x][*y] {
                        if axis.is_none_or(|a| a == 0) {
                            offset[0] = -offset[0];
                        }
                        if axis.is_none_or(|a| a == 1) {
                            offset[1] = -offset[1];
                        }
                    }
                }
                CellEdit::Unset | CellEdit::Reset => grid[*x][*y] = vec![[0.0, 0.0]; vertex_count],
                CellEdit::SetCurrent => {}
            }
            flags[*x][*y] = sets_flag;
        }
        let any_set = flags.iter().flatten().any(|set| *set);
        if any_set {
            crate::deformers::fill_unset_offsets(&mut grid, &flags, &axes[0], &axes[1], repeat)?;
        } else {
            grid.iter_mut().flatten().for_each(|o| *o = vec![[0.0, 0.0]; vertex_count]);
        }
        binding["values"] = json!(grid);
    } else {
        let mut grid = raw
            .iter()
            .map(|column| {
                column
                    .as_array()
                    .filter(|column| column.len() == y_count)
                    .ok_or("binding.values has a mismatched Y axis")?
                    .iter()
                    .map(|value| crate::finite_number(value, "binding.values"))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (x, y) in &targets {
            match edit {
                CellEdit::Scale(axis) => {
                    if mirror_negates(&channel, *axis) {
                        grid[*x][*y] = -grid[*x][*y];
                    }
                }
                CellEdit::Unset | CellEdit::Reset => grid[*x][*y] = neutral,
                CellEdit::SetCurrent => {}
            }
            flags[*x][*y] = sets_flag;
        }
        if flags.iter().flatten().any(|set| *set) {
            evaluation::reinterpolate(&mut grid, &flags, &axes[0], &axes[1], repeat)?;
        } else {
            grid.iter_mut().flatten().for_each(|v| *v = neutral);
        }
        binding["values"] = json!(grid);
    }
    binding["isSet"] = json!(flags);
    Ok(())
}

fn parameter_for_edit<'a>(model: &'a mut Value, id: u32) -> Result<(&'a mut Value, bool, [Vec<f64>; 2]), String> {
    let slot = locate_plain(model, id)?;
    let parameter = &mut container_of(model, slot)?[slot_index(slot)];
    let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    let axes = stored_axes(parameter, is_vec2)?;
    Ok((parameter, is_vec2, axes))
}

/// `mirrorAll`: negate every set key of every binding across `axis` (0 horizontal, 1 vertical).
pub(crate) fn mirror_all(model: &mut Value, id: u32, axis: usize) -> Result<(), String> {
    let defaults = parameter_defaults(model);
    let (parameter, is_vec2, axes) = parameter_for_edit(model, id)?;
    let repeat = evaluation::parameter_repeats(parameter);
    if axis > 1 || (axis == 1 && !is_vec2) {
        return Err("axis must be 0, or 1 for a 2D parameter".into());
    }
    let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for binding in bindings {
        edit_binding_cells(binding, &axes, is_vec2, &Cells::AllSet, &CellEdit::Scale(Some(axis)), &defaults, repeat)?;
    }
    Ok(())
}

/// Identifies one binding of a parameter by node and channel.
pub(crate) type BindingKey = (u32, String);

pub(crate) fn binding_matches(binding: &Value, keys: &[BindingKey]) -> bool {
    keys.iter().any(|(node, channel)| {
        binding.get("node").and_then(Value::as_u64) == Some(u64::from(*node))
            && binding.get("param_name").and_then(Value::as_str) == Some(channel.as_str())
    })
}

pub(crate) fn check_binding_keys(bindings: &[Value], keys: &[BindingKey]) -> Result<(), String> {
    for key in keys {
        if !bindings.iter().any(|b| binding_matches(b, std::slice::from_ref(key))) {
            return Err(format!("parameter has no {} binding on node {}", key.1, key.0));
        }
    }
    Ok(())
}

/// Keypoint menu actions of the armed parameter: unset, reset, set_current, invert, mirror a single key.
pub(crate) fn edit_keypoint(
    model: &mut Value,
    id: u32,
    key: [usize; 2],
    action: &str,
    only: Option<&[BindingKey]>,
) -> Result<usize, String> {
    let defaults = parameter_defaults(model);
    let (parameter, is_vec2, axes) = parameter_for_edit(model, id)?;
    let repeat = evaluation::parameter_repeats(parameter);
    if key[0] >= axes[0].len() || key[1] >= if is_vec2 { axes[1].len() } else { 1 } {
        return Err("key must identify an existing parameter keypoint index".into());
    }
    let edit = match action {
        "unset" => CellEdit::Unset,
        "reset" => CellEdit::Reset,
        "set_current" => CellEdit::SetCurrent,
        "invert" => CellEdit::Scale(None),
        "mirror_horizontal" => CellEdit::Scale(Some(0)),
        "mirror_vertical" if is_vec2 => CellEdit::Scale(Some(1)),
        "mirror_vertical" => return Err("vertical mirroring needs a 2D parameter".into()),
        _ => return Err("action must be unset, reset, set_current, invert, mirror_horizontal or mirror_vertical".into()),
    };
    let bindings = parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .ok_or("parameter.bindings must be an array")?;
    if let Some(keys) = only {
        check_binding_keys(bindings, keys)?;
    }
    let cell = [(key[0], key[1])];
    let mut edited = 0;
    for binding in bindings.iter_mut().filter(|b| only.is_none_or(|keys| binding_matches(b, keys))) {
        edit_binding_cells(binding, &axes, is_vec2, &Cells::At(&cell), &edit, &defaults, repeat)?;
        edited += 1;
    }
    if edited == 0 {
        return Err("parameter has no bindings to edit".into());
    }
    Ok(edited)
}

pub(crate) fn remove_bindings(model: &mut Value, id: u32, keys: &[BindingKey]) -> Result<usize, String> {
    let (parameter, _, _) = parameter_for_edit(model, id)?;
    let bindings = parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .ok_or("parameter.bindings must be an array")?;
    if keys.is_empty() {
        return Err("bindings must not be empty".into());
    }
    check_binding_keys(bindings, keys)?;
    let before = bindings.len();
    bindings.retain(|binding| !binding_matches(binding, keys));
    Ok(before - bindings.len())
}

pub(crate) fn set_interpolation(
    model: &mut Value,
    id: u32,
    keys: &[BindingKey],
    mode: &str,
) -> Result<usize, String> {
    if !["Nearest", "Linear", "Cubic"].contains(&mode) {
        return Err("mode must be Nearest, Linear or Cubic".into());
    }
    let (parameter, _, _) = parameter_for_edit(model, id)?;
    let bindings = parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .ok_or("parameter.bindings must be an array")?;
    if keys.is_empty() {
        return Err("bindings must not be empty".into());
    }
    check_binding_keys(bindings, keys)?;
    let mut changed = 0;
    for binding in bindings.iter_mut().filter(|b| binding_matches(b, keys)) {
        binding["interpolate_mode"] = json!(mode);
        changed += 1;
    }
    Ok(changed)
}

/// `SetStartingKeyFrame`: the parameter's default becomes `value`.
pub(crate) fn set_defaults(model: &mut Value, id: u32, value: [f64; 2]) -> Result<(), String> {
    let list = top_list(model)?;
    // A field edit needs the record, not its slot, so a parameter in a group inside a group is reached too.
    find_parameter(list, id)?;
    let parameter = find_parameter_mut(list, id).ok_or_else(|| format!("parameter {id} was not found"))?;
    parameter["defaults"] = json!(value);
    Ok(())
}

/// `ngSplitParameterBindings`: bindings of `nodes` move to a new "<name> (Split)" parameter placed
/// after the original. Unlike the D code, the new parameter keeps the source range and defaults.
pub(crate) fn split_parameter(
    model: &mut Value,
    id: u32,
    new_id: u32,
    nodes: &[u32],
) -> Result<Value, String> {
    let slot = locate_plain(model, id)?;
    let source = container_of(model, slot)?[slot_index(slot)].clone();
    let bindings = source
        .get("bindings")
        .and_then(Value::as_array)
        .ok_or("parameter.bindings must be an array")?;
    let takes = |binding: &Value| {
        binding
            .get("node")
            .and_then(Value::as_u64)
            .is_some_and(|node| nodes.iter().any(|n| u64::from(*n) == node))
    };
    let (taken, kept): (Vec<Value>, Vec<Value>) = bindings.iter().cloned().partition(|b| takes(b));
    if taken.is_empty() {
        return Err("no binding of the selected nodes was found".into());
    }
    let base = source.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
    let name = free_name(model, &format!("{base} (Split)"), |n| format!("{base} (Split {n})"));
    let mut split = source.clone();
    split["uuid"] = json!(new_id);
    split["name"] = json!(name);
    split["bindings"] = json!(taken);
    let list = container_of(model, slot)?;
    list[slot_index(slot)]["bindings"] = json!(kept);
    list.insert(slot_index(slot) + 1, split.clone());
    Ok(split)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(is_vec2: bool, axes: Value, bindings: Value) -> Value {
        json!({
            "uuid": 7,
            "is_vec2": is_vec2,
            "min": [-1.0, -1.0],
            "max": [1.0, 1.0],
            "axis_points": axes,
            "bindings": bindings,
        })
    }

    #[test]
    fn expanding_1d_axes_remaps_set_cells_and_interpolates_gaps() {
        let mut model = json!({
            "param": [parameter(false, json!([[0.0, 0.5, 1.0], [0.0]]), json!([{
                "node": 3,
                "param_name": "transform.t.x",
                "values": [[10.0], [20.0], [30.0]],
                "isSet": [[true], [true], [true]],
            }]))]
        });
        set_axes(
            &mut model,
            7,
            [-1.0, 0.0],
            [1.0, 0.0],
            &[0.0, 0.25, 0.5, 0.75, 1.0],
            &[0.0],
        )
        .unwrap();
        let binding = &model["param"][0]["bindings"][0];
        assert_eq!(binding["values"], json!([[10.0], [15.0], [20.0], [25.0], [30.0]]));
        assert_eq!(binding["isSet"], json!([[true], [false], [true], [false], [true]]));
    }

    #[test]
    fn shrinking_1d_axes_keeps_nearest_endpoints() {
        let mut model = json!({
            "param": [parameter(false, json!([[0.0, 0.5, 1.0], [0.0]]), json!([{
                "node": 3,
                "param_name": "transform.t.x",
                "values": [[10.0], [20.0], [30.0]],
                "isSet": [[true], [true], [true]],
            }]))]
        });
        set_axes(&mut model, 7, [-1.0, 0.0], [1.0, 0.0], &[0.0, 1.0], &[0.0]).unwrap();
        let binding = &model["param"][0]["bindings"][0];
        assert_eq!(binding["values"], json!([[10.0], [30.0]]));
        assert_eq!(binding["isSet"], json!([[true], [true]]));
    }

    #[test]
    fn deform_bindings_interpolate_each_vertex_component() {
        let mut model = json!({
            "param": [parameter(false, json!([[0.0, 1.0], [0.0]]), json!([{
                "node": 3,
                "param_name": "deform",
                "values": [[[ [0.0, 2.0], [4.0, 6.0] ]], [[ [10.0, 12.0], [14.0, 16.0] ]]],
                "isSet": [[true], [true]],
            }]))]
        });
        set_axes(&mut model, 7, [-1.0, 0.0], [1.0, 0.0], &[0.0, 0.5, 1.0], &[0.0]).unwrap();
        let binding = &model["param"][0]["bindings"][0];
        assert_eq!(binding["values"][1][0], json!([[5.0, 7.0], [9.0, 11.0]]));
        assert_eq!(binding["isSet"], json!([[true], [false], [true]]));
    }

    #[test]
    fn range_only_change_preserves_binding_structure() {
        let binding = json!({
            "node": 3,
            "param_name": "transform.t.x",
            "values": [[10.0], [20.0]],
            "isSet": [[true], [false]],
            "future_metadata": {"keep": true},
        });
        let mut model = json!({
            "param": [parameter(false, json!([[0.0, 1.0], [0.0]]), json!([binding.clone()]))]
        });
        set_axes(&mut model, 7, [-2.0, 0.0], [2.0, 0.0], &[0.0, 1.0], &[0.0]).unwrap();
        assert_eq!(model["param"][0]["bindings"][0], binding);
        assert_eq!(model["param"][0]["axis_points"], json!([[0.0, 1.0], [0.0]]));
    }

    #[test]
    fn delete_nested_parameter_clears_physics_and_preserves_other_data() {
        let mut model = json!({
            "param": [{"groupUUID": 40, "children": [
                {"uuid": 7, "name": "remove"},
                {"uuid": 8, "name": "keep"},
            ]}],
            "nodes": {"uuid": 1, "type": "Root", "children": [{
                "uuid": 2, "type": "SimplePhysics", "param": 7,
                "children": [{"uuid": 3, "type": "SimplePhysics", "param": 8, "children": []}],
            }]},
            "animations": [{"parameter": 7, "metadata": "preserve"}],
            "incomingparambindings": [{"parameter": 7}],
            "unknown": {"keep": true},
        });
        delete_parameter(&mut model, 7).unwrap();
        assert_eq!(model["param"][0]["children"], json!([{"uuid": 8, "name": "keep"}]));
        assert_eq!(model["nodes"]["children"][0]["param"], json!(u32::MAX));
        assert_eq!(model["nodes"]["children"][0]["children"][0]["param"], json!(8));
        assert_eq!(model["animations"][0]["parameter"], json!(7));
        assert_eq!(model["incomingparambindings"][0]["parameter"], json!(7));
        assert_eq!(model["unknown"]["keep"], json!(true));
    }

    #[test]
    fn invalid_group_grid_and_axis_requests_fail() {
        let mut group = json!({"param": [{"groupUUID": 40, "children": []}]});
        assert!(delete_parameter(&mut group, 40).is_err());
        assert!(set_axes(&mut group, 40, [0.0, 0.0], [1.0, 1.0], &[0.0, 1.0], &[0.0]).is_err());

        let mut malformed = json!({
            "param": [parameter(false, json!([[0.0, 1.0], [0.0]]), json!([{
                "param_name": "transform.t.x",
                "values": [[1.0]],
                "isSet": [[true]],
            }]))]
        });
        assert!(set_axes(&mut malformed, 7, [-1.0, 0.0], [1.0, 0.0], &[0.0, 0.5, 1.0], &[0.0]).is_err());
        assert_eq!(malformed["param"][0]["axis_points"], json!([[0.0, 1.0], [0.0]]));

        let mut valid = json!({"param": [parameter(false, json!([[0.0, 1.0], [0.0]]), json!([]))]});
        assert!(set_axes(&mut valid, 7, [-1.0, 0.0], [1.0, 0.0], &[0.0, 0.5, 0.5, 1.0], &[0.0]).is_err());
        assert!(set_axes(&mut valid, 7, [-1.0, 0.0], [1.0, 0.0], &[0.0, 1.0], &[0.0, 1.0]).is_err());
    }

    #[test]
    fn matching_pair_limit_is_checked_before_candidate_allocation() {
        let axis = (0..MAX_AXIS_POINTS)
            .map(|index| index as f64 / (MAX_AXIS_POINTS - 1) as f64)
            .collect::<Vec<_>>();
        let axes = [axis.clone(), axis];
        assert!(matching_pairs(&axes, &axes, true).is_err());
    }

    #[test]
    fn collapsed_ranges_preserve_existing_bindings() {
        let mut model = json!({"param":[parameter(false, json!([[0.0,1.0],[0.0]]), json!([]))]});
        set_axes(&mut model, 7, [0.0,0.0], [0.0,0.0], &[0.0,1.0], &[0.0]).unwrap();
        assert_eq!(model["param"][0]["min"], model["param"][0]["max"]);
    }

    #[test]
    fn deformation_expansion_is_bounded_before_output_allocation() {
        let offsets = json!(vec![[0.0,0.0]; 10_000]);
        let mut model = json!({"param":[parameter(true, json!([[0.0,1.0],[0.0,1.0]]), json!([{
            "param_name":"deform", "values":[[offsets,offsets],[offsets,offsets]],
            "isSet":[[true,true],[true,true]]
        }]))]});
        let axis = (0..256).map(|index| index as f64 / 255.0).collect::<Vec<_>>();
        assert!(set_axes(&mut model, 7, [-1.0,-1.0], [1.0,1.0], &axis, &axis)
            .unwrap_err().contains("output size limit"));
        assert_eq!(model["param"][0]["axis_points"], json!([[0.0,1.0],[0.0,1.0]]));
        let mut aggregate = 10;
        consume_remap_budget(&mut aggregate, 2, 2, 2).unwrap();
        assert!(consume_remap_budget(&mut aggregate, 1, 2, 2).is_err());
        assert!(consume_remap_budget(&mut aggregate, usize::MAX, 2, 2).is_err());
    }
}
