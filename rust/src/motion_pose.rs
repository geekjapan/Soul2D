use crate::motion3;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) struct Pose {
    pub(crate) values: BTreeMap<u32, [f64; 2]>,
    pub(crate) applied: Value,
    pub(crate) unmapped: Value,
    pub(crate) clip_end: f64,
    pub(crate) time: f64,
}

pub(crate) fn bind(
    model: &Value,
    motion: &Value,
    name: &str,
    binding_rows: &Value,
    preserve_unknown: bool,
) -> Result<Value, String> {
    let registered = named_motion(model, name)?;
    if registered != motion {
        return Err(format!(
            "Motion3 {name:?} does not match the registered motion"
        ));
    }
    motion3::validate(motion)?;
    validate_binding_rows(model, motion, binding_rows)?;
    let stored_rows = if preserve_unknown {
        let previous = bindings(model, name)?;
        let rows = binding_rows.as_array().ok_or("bindings must be an array")?;
        let mut merged = Vec::with_capacity(rows.len());
        for row in rows {
            let prior = previous.as_array().and_then(|rows| rows.iter().find(|old| old["id"] == row["id"]));
            let mut updated = prior.unwrap_or(row).clone();
            let object = updated.as_object_mut().ok_or("binding must be an object")?;
            for key in ["id", "parameter", "axis"] {
                object.insert(key.to_owned(), row[key].clone());
            }
            merged.push(updated);
        }
        Value::Array(merged)
    } else {
        binding_rows.clone()
    };

    let mut candidate = model.clone();
    let live2d = candidate
        .as_object_mut()
        .ok_or("model must be an object")?
        .entry("live2d")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("model.live2d must be an object")?;
    let motion_bindings = live2d
        .entry("motionBindings")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("model.live2d.motionBindings must be an object")?;
    motion_bindings.insert(name.to_owned(), stored_rows);

    // Validate the complete stored result so a successful edit is immediately readable.
    bindings(&candidate, name)?;
    Ok(candidate)
}

pub(crate) fn bindings(model: &Value, name: &str) -> Result<Value, String> {
    let motion = named_motion(model, name)?;
    motion3::validate(motion)?;
    let raw = model
        .get("live2d")
        .and_then(|live2d| live2d.get("motionBindings"));
    let Some(raw) = raw else {
        return Ok(json!([]));
    };
    let raw = raw
        .as_object()
        .ok_or("model.live2d.motionBindings must be an object")?;
    let Some(bindings) = raw.get(name) else {
        return Ok(json!([]));
    };
    validate_binding_rows(model, motion, bindings)?;
    Ok(bindings.clone())
}

pub(crate) fn pose(
    model: &Value,
    motion: &Value,
    name: &str,
    time: f64,
    base: &BTreeMap<u32, [f64; 2]>,
    fade: Option<motion3::FadeClock>,
) -> Result<Pose, String> {
    if !time.is_finite() || time < 0.0 {
        return Err("motion pose time must be finite and nonnegative".into());
    }
    let registered = named_motion(model, name)?;
    if registered != motion {
        return Err(format!(
            "Motion3 {name:?} does not match the registered motion"
        ));
    }
    let raw_bindings = bindings(model, name)?;
    let rows = raw_bindings
        .as_array()
        .ok_or("motion bindings must be an array")?;
    if rows.is_empty() {
        return Err(format!("Motion3 {name:?} has no parameter bindings"));
    }
    let binding_map = validate_binding_rows(model, motion, &raw_bindings)?;
    let clip_end = motion3::end_time(motion)?;
    let time = time.min(clip_end);
    let sampled = motion3::sample(motion, time)?;
    let weights = fade.map(|clock| motion3::weights(motion, time, clip_end, clock)).transpose()?;
    let sampled_curves = sampled
        .get("curves")
        .and_then(Value::as_array)
        .ok_or("motion sample curves must be an array")?;

    let mut values = base.clone();
    let mut applied = Vec::new();
    let mut unmapped = Vec::new();
    for (index, curve) in sampled_curves.iter().enumerate() {
        let target = curve
            .get("target")
            .and_then(Value::as_str)
            .ok_or("motion sample target must be a string")?;
        let id = curve
            .get("id")
            .and_then(Value::as_str)
            .ok_or("motion sample id must be a string")?;
        let raw_value = curve
            .get("value")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or("motion sample value must be finite")?;

        if target == "Parameter" {
            if let Some((parameter_id, axis)) = binding_map.get(id).copied() {
                let parameter = find_leaf_parameter(model, parameter_id)?;
                let pair = values.get_mut(&parameter_id).ok_or_else(|| {
                    format!("runtime value for parameter {parameter_id} is missing")
                })?;
                // The fade blends from the value the parameter holds without the motion.
                pair[axis] = match &weights {
                    Some(weights) => pair[axis] + (raw_value - pair[axis]) * weights[index],
                    None => raw_value,
                };
                validate_value(parameter, parameter_id, *pair)?;
                applied.push((
                    target.to_owned(),
                    id.to_owned(),
                    raw_value,
                    parameter_id,
                    axis,
                    weights.as_ref().map(|weights| weights[index]),
                ));
                continue;
            }
        }
        unmapped.push(json!({"target": target, "id": id, "value": raw_value}));
    }
    let applied = applied
        .into_iter()
        .map(|(target, id, raw, parameter, axis, weight)| {
            let value = values
                .get(&parameter)
                .ok_or_else(|| format!("runtime value for parameter {parameter} is missing"))?;
            let mut row = json!({
                "target": target,
                "id": id,
                "raw": raw,
                "value": value,
                "parameter": parameter,
                "axis": axis,
            });
            if let Some(weight) = weight {
                row["weight"] = json!(weight);
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(Pose {
        values,
        applied: Value::Array(applied),
        unmapped: Value::Array(unmapped),
        clip_end,
        time,
    })
}

fn named_motion<'a>(model: &'a Value, name: &str) -> Result<&'a Value, String> {
    model
        .get("live2d")
        .ok_or("model.live2d is missing")?
        .as_object()
        .ok_or("model.live2d must be an object")?
        .get("motions")
        .ok_or("model.live2d.motions is missing")?
        .as_object()
        .ok_or("model.live2d.motions must be an object")?
        .get(name)
        .ok_or_else(|| format!("Motion3 {name:?} was not found"))
}

fn validate_binding_rows(
    model: &Value,
    motion: &Value,
    bindings: &Value,
) -> Result<HashMap<String, (u32, usize)>, String> {
    let rows = bindings
        .as_array()
        .ok_or("motion bindings must be an array")?;
    let curves = motion
        .get("Curves")
        .and_then(Value::as_array)
        .ok_or("motion3.Curves must be an array")?;
    let mut result = HashMap::with_capacity(rows.len());
    let mut mapped_axes = HashSet::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let row = row
            .as_object()
            .ok_or_else(|| format!("motion bindings[{index}] must be an object"))?;
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| format!("motion bindings[{index}].id must be a nonempty string"))?;
        let parameter_id = row
            .get("parameter")
            .and_then(Value::as_u64)
            .and_then(crate::valid_id)
            .ok_or_else(|| format!("motion bindings[{index}].parameter must be a positive UUID"))?;
        let axis = row
            .get("axis")
            .and_then(Value::as_u64)
            .filter(|axis| *axis <= 1)
            .map(|axis| axis as usize)
            .ok_or_else(|| format!("motion bindings[{index}].axis must be 0 or 1"))?;

        let matching_curve = curves.iter().any(|curve| {
            curve.get("Target").and_then(Value::as_str) == Some("Parameter")
                && curve.get("Id").and_then(Value::as_str) == Some(id)
        });
        if !matching_curve {
            return Err(format!(
                "motion binding {:?} does not name a Parameter curve",
                id
            ));
        }
        let parameter = find_leaf_parameter(model, parameter_id)?;
        if axis == 1 && !parameter_is_vec2(parameter)? {
            return Err(format!(
                "motion binding {:?} uses axis 1 on 1D parameter {parameter_id}",
                id
            ));
        }
        if result.insert(id.to_owned(), (parameter_id, axis)).is_some() {
            return Err(format!("motion binding id {id:?} is duplicated"));
        }
        if !mapped_axes.insert((parameter_id, axis)) {
            return Err(format!(
                "parameter {parameter_id} axis {axis} is mapped more than once"
            ));
        }
    }
    Ok(result)
}

fn find_leaf_parameter(model: &Value, id: u32) -> Result<&Value, String> {
    let parameters = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or("model.param must be an array")?;
    fn find(parameters: &[Value], id: u32) -> Result<Option<&Value>, String> {
        for parameter in parameters {
            if parameter.get("groupUUID").and_then(Value::as_u64) == Some(u64::from(id)) {
                return Err(format!("parameter {id} refers to a parameter group"));
            }
            if parameter.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
                if parameter.get("groupUUID").is_some() {
                    return Err(format!("parameter {id} refers to a parameter group"));
                }
                return Ok(Some(parameter));
            }
            if let Some(children) = parameter.get("children").and_then(Value::as_array) {
                if let Some(found) = find(children, id)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    find(parameters, id)?.ok_or_else(|| format!("parameter {id} was not found"))
}

fn parameter_is_vec2(parameter: &Value) -> Result<bool, String> {
    match parameter.get("is_vec2") {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or("parameter is_vec2 must be a boolean".into()),
    }
}

fn validate_value(parameter: &Value, id: u32, value: [f64; 2]) -> Result<(), String> {
    let min = parameter
        .get("min")
        .map(|raw| crate::parse_pair(raw, "parameter min"))
        .transpose()?
        .unwrap_or([0.0, 0.0]);
    let max = parameter
        .get("max")
        .map(|raw| crate::parse_pair(raw, "parameter max"))
        .transpose()?
        .unwrap_or([1.0, 1.0]);
    for axis in 0..2 {
        if !value[axis].is_finite() {
            return Err(format!("value for parameter {id} must be finite"));
        }
        if min[axis] > max[axis] {
            return Err(format!(
                "parameter {id} has an invalid range on axis {axis}"
            ));
        }
        // A curve may run a repeating parameter past its range to count turns: evaluation wraps the value.
        let turns = axis == 0 && crate::evaluation::parameter_repeats(parameter);
        if !turns && (value[axis] < min[axis] || value[axis] > max[axis]) {
            return Err(format!("value for parameter {id} is outside its range"));
        }
    }
    Ok(())
}
