//! Keyed exposure lengths. The key starts a new hold grid; parameter groups can override the clip's grid.

use serde_json::{json, Value};
use std::collections::BTreeSet;

const MAX_STEP: u64 = 1_000_000;

fn keys(curve: &Value, length: usize) -> Result<&[Value], String> {
    let keys = curve
        .get("keyframes")
        .and_then(Value::as_array)
        .ok_or("frame step must have a keyframes array")?;
    if keys.len() > length {
        return Err("too many frame step keys".into());
    }
    let mut previous = None;
    for key in keys {
        let at = key["frame"]
            .as_u64()
            .filter(|at| *at < length as u64)
            .ok_or("frame step key frame must be an integer within the clip")?;
        if previous.is_some_and(|previous| previous >= at) {
            return Err("frame step keys must have distinct increasing frames".into());
        }
        key["value"]
            .as_u64()
            .filter(|step| (1..=MAX_STEP).contains(step))
            .ok_or("frame step value must be an integer from 1 to 1000000")?;
        previous = Some(at);
    }
    Ok(keys)
}

fn group_ids(list: &Value, ids: &mut BTreeSet<u32>) {
    for item in list.as_array().into_iter().flatten() {
        if let Some(id) = item["groupUUID"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
        {
            ids.insert(id);
            group_ids(&item["children"], ids);
        }
    }
}

pub(crate) fn validate(model: &Value, animation: &Value, length: usize) -> Result<(), String> {
    if let Some(curve) = animation.get("frameStep") {
        keys(curve, length)?;
    }
    if let Some(groups) = animation.get("frameStepGroups") {
        let groups = groups
            .as_object()
            .ok_or("frameStepGroups must be an object")?;
        let mut ids = BTreeSet::new();
        group_ids(&model["param"], &mut ids);
        for (id, curve) in groups {
            let parsed = id
                .parse::<u32>()
                .ok()
                .filter(|parsed| parsed.to_string() == *id && ids.contains(parsed));
            if parsed.is_none() {
                return Err(format!("frame step parameter group {id:?} was not found"));
            }
            keys(curve, length)?;
        }
    }
    Ok(())
}

fn at(curve: Option<&Value>, length: usize, frame: f64) -> Result<f64, String> {
    if !frame.is_finite() || frame < 0.0 {
        return Err("frame must be finite and nonnegative".into());
    }
    let Some(curve) = curve else { return Ok(frame) };
    let key = keys(curve, length)?
        .iter()
        .rev()
        .find(|key| key["frame"].as_u64().unwrap() as f64 <= frame);
    let Some(key) = key else { return Ok(frame) };
    let start = key["frame"].as_u64().unwrap() as f64;
    let step = key["value"].as_u64().unwrap() as f64;
    Ok(start + ((frame - start) / step).floor() * step)
}

pub(crate) fn frame(animation: &Value, length: usize, frame: f64) -> Result<f64, String> {
    at(animation.get("frameStep"), length, frame)
}

fn inherited_curve<'a>(
    list: &Value,
    curves: &'a Value,
    id: u32,
    inherited: Option<&'a Value>,
) -> Option<Option<&'a Value>> {
    for item in list.as_array().into_iter().flatten() {
        if let Some(group) = item["groupUUID"].as_u64() {
            let curve = curves.get(group.to_string()).or(inherited);
            if let Some(found) = inherited_curve(&item["children"], curves, id, curve) {
                return Some(found);
            }
        } else if item["uuid"].as_u64() == Some(u64::from(id)) {
            return Some(inherited);
        }
    }
    None
}

pub(crate) fn parameter_frame(
    model: &Value,
    animation: &Value,
    id: u32,
    length: usize,
    frame: f64,
) -> Result<f64, String> {
    let curve = inherited_curve(
        &model["param"],
        &animation["frameStepGroups"],
        id,
        animation.get("frameStep"),
    )
    .unwrap_or_else(|| animation.get("frameStep"));
    at(curve, length, frame)
}

pub(crate) fn set(model: &Value, request: &Value) -> Result<Value, String> {
    let name = crate::required_string(request, "name")?;
    let animation = model["animations"]
        .get(name)
        .ok_or_else(|| format!("animation {name:?} was not found"))?;
    let length = crate::animation::validate_animation_header(animation)?;
    let mut list = match request.get("keyframes") {
        Some(Value::Null) => Vec::new(),
        Some(list) => list
            .as_array()
            .ok_or("keyframes must be an array or null")?
            .clone(),
        None => return Err("keyframes is required; use null to clear frame stepping".into()),
    };
    list.sort_by_key(|key| key["frame"].as_u64());
    let curve = json!({"keyframes": list});
    keys(&curve, length)?;
    let group = request
        .get("parameterGroup")
        .map(|id| {
            id.as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or("parameterGroup must be a u32")
        })
        .transpose()?;
    if let Some(id) = group {
        let mut ids = BTreeSet::new();
        group_ids(&model["param"], &mut ids);
        if !ids.contains(&id) {
            return Err(format!("parameter group {id} was not found"));
        }
    }
    let mut candidate = model.clone();
    let object = candidate["animations"][name]
        .as_object_mut()
        .ok_or("animation must be an object")?;
    let empty = curve["keyframes"].as_array().unwrap().is_empty();
    if let Some(group) = group {
        let groups = object
            .entry("frameStepGroups")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("frameStepGroups must be an object")?;
        if empty {
            groups.remove(&group.to_string());
        } else {
            groups.insert(group.to_string(), curve);
        }
        if groups.is_empty() {
            object.remove("frameStepGroups");
        }
    } else if empty {
        object.remove("frameStep");
    } else {
        object.insert("frameStep".into(), curve);
    }
    let changed = &candidate["animations"][name];
    validate(model, changed, length)?;
    if let Some(refusal) = crate::animation::frozen_change(model, &candidate) {
        return Err(refusal);
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Value {
        json!({"param":[{"groupUUID":30,"children":[{"uuid":1},
            {"groupUUID":40,"children":[{"uuid":2}]}]}, {"uuid":3}],
            "animations":{"A":{"length":20,"timestep":0.1,"lanes":[]}}})
    }

    #[test]
    fn keys_restart_the_hold_grid_and_nested_groups_override_instead_of_quantizing_twice() {
        let mut model = model();
        for (group, step) in [(None, 4), (Some(30), 3), (Some(40), 2)] {
            let mut request = json!({"name":"A","keyframes":[{"frame":0,"value":step}]});
            if let Some(group) = group {
                request["parameterGroup"] = json!(group);
            }
            model = set(&model, &request).unwrap();
        }
        let animation = &model["animations"]["A"];
        assert_eq!(parameter_frame(&model, animation, 1, 20, 5.9).unwrap(), 3.0);
        assert_eq!(parameter_frame(&model, animation, 2, 20, 5.9).unwrap(), 4.0);
        assert_eq!(parameter_frame(&model, animation, 3, 20, 5.9).unwrap(), 4.0);
        let animation =
            json!({"frameStep":{"keyframes":[{"frame":2,"value":4},{"frame":5,"value":3}]}});
        for (time, expected) in [
            (1.5, 1.5),
            (2.0, 2.0),
            (4.9, 2.0),
            (5.0, 5.0),
            (7.9, 5.0),
            (8.0, 8.0),
        ] {
            assert_eq!(frame(&animation, 20, time).unwrap(), expected);
        }
    }

    #[test]
    fn malformed_exposure_and_frozen_lane_changes_are_refused() {
        for value in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("2"),
            json!(1_000_001),
        ] {
            assert!(set(
                &model(),
                &json!({"name":"A","keyframes":[{"frame":0,"value":value}]})
            )
            .is_err());
        }
        assert!(set(
            &model(),
            &json!({"name":"A","keyframes":[{"frame":20,"value":2}]})
        )
        .is_err());
        assert!(set(
            &model(),
            &json!({"name":"A","parameterGroup":1,"keyframes":[]})
        )
        .is_err());
        let mut model = model();
        model["animations"]["A"]["freezeFrame"] = json!(5);
        model["animations"]["A"]["lanes"] = json!([{"uuid":1,"frozen":true}]);
        assert!(set(
            &model,
            &json!({"name":"A","keyframes":[{"frame":0,"value":4}]})
        )
        .is_err());
        assert!(set(
            &model,
            &json!({"name":"A","parameterGroup":40,"keyframes":[{"frame":0,"value":4}]})
        )
        .is_ok());
    }
}
