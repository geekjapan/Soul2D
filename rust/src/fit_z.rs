//! Fit DepthBone translation Z to the current mapped depth.
//!
//! The numbers follow `ngDepthRigNodeCurrentScaledDepth` and
//! `ngFitDepthRigNodeTranslationZToCurrentDepth`. A root fit walks descendant
//! bones parent-before-child and writes every change in one model snapshot.
//! `fitDepthBoneBindingZToCurrentDepth` is not called, and no deform offsets
//! are generated.

use crate::bone_deform::{self, Mat4};
use crate::evaluation;
use crate::nodes;
use crate::valid_id;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const ALIGNED: f64 = 0.00001;

struct SourceSetting {
    depth_scale: f64,
    depth_offset: f64,
    rotation: f64,
}

pub(crate) fn fit_root(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
) -> Result<(Value, Value), String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    let bones = bone_deform::bone_order(model, root_id)?;
    apply(model, values, root_id, &bones, true)
}

pub(crate) fn fit_bone(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    bone_id: u32,
) -> Result<(Value, Value), String> {
    let bone =
        nodes::find_node(model, bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?;
    if bone.get("type").and_then(Value::as_str) != Some("DepthBone") {
        return Err("Node is not a DepthBone".into());
    }
    let Some(root_id) = find_rig_root(model, bone_id)? else {
        return Err("No usable mapped depth sample was found for the DepthBone".into());
    };
    apply(model, values, root_id, &[bone_id], false)
}

fn apply(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    bones: &[u32],
    root_command: bool,
) -> Result<(Value, Value), String> {
    let mut candidate = model.clone();
    let mut fitted = false;
    let mut changed = Vec::new();
    for bone_id in bones {
        match fit_one(&mut candidate, values, root_id, *bone_id)? {
            FitOne::Unusable => {}
            FitOne::Aligned => fitted = true,
            FitOne::Changed(z) => {
                fitted = true;
                changed.push(json!({ "node": bone_id, "translationZ": z }));
            }
        }
    }
    if !fitted {
        return Err(if root_command {
            "No usable mapped depth samples were found for the DepthRigRoot".into()
        } else {
            "No usable mapped depth sample was found for the DepthBone".into()
        });
    }
    Ok((candidate, json!({ "changed": changed })))
}

enum FitOne {
    Unusable,
    Aligned,
    Changed(f64),
}

fn fit_one(
    model: &mut Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    bone_id: u32,
) -> Result<FitOne, String> {
    let posed = posed_model(model, values)?;
    let world = match bone_deform::world_matrix(&posed, bone_id) {
        Ok(matrix) => matrix,
        Err(_) => return Ok(FitOne::Unusable),
    };
    let world_point = [world[0][3], world[1][3]];
    let Some(root_depth) = sampled_depth(&posed, root_id, Some(bone_id), world_point)?
        .or(sampled_depth(&posed, root_id, None, world_point)?)
    else {
        return Ok(FitOne::Unusable);
    };
    if !root_depth.is_finite() || !world[2][3].is_finite() {
        return Ok(FitOne::Unusable);
    }
    let bone =
        nodes::find_node(model, bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?;
    let current_local = bone_deform::transform_vec(bone, "trans", [0.0, 0.0, 0.0])[2];
    let proposed = current_local + (root_depth - world[2][3]);
    if !proposed.is_finite() {
        return Ok(FitOne::Unusable);
    }
    if (current_local - proposed).abs() <= ALIGNED {
        return Ok(FitOne::Aligned);
    }
    set_local_z(model, bone_id, proposed)?;
    Ok(FitOne::Changed(proposed))
}

fn sampled_depth(
    posed: &Value,
    root_id: u32,
    bone_id: Option<u32>,
    world_point: [f64; 2],
) -> Result<Option<f64>, String> {
    let root =
        nodes::find_node(posed, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Ok(None);
    };
    let world_scale = depth_world_scale(posed, root_id)?;
    let mut total = 0.0;
    let mut count = 0.0;
    for binding in bindings {
        let Some(setting) = setting_for(binding, bone_id)? else {
            continue;
        };
        let Some(target_id) = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(target) = nodes::find_node(posed, target_id) else {
            continue;
        };
        if target.get("type").and_then(Value::as_str) != Some("GridDeformer") {
            continue;
        }
        let Some(depths) = target.get("depths").and_then(Value::as_array) else {
            continue;
        };
        if depths.is_empty() {
            continue;
        }
        let Ok(matrix) = bone_deform::world_matrix(posed, target_id) else {
            continue;
        };
        let Some(best) =
            nearest_target_depth(target, depths, matrix, &setting, world_scale, world_point)?
        else {
            continue;
        };
        total += best;
        count += 1.0;
    }
    if count == 0.0 {
        return Ok(None);
    }
    let depth = total / count;
    if depth.is_finite() {
        Ok(Some(depth))
    } else {
        Ok(None)
    }
}

fn nearest_target_depth(
    target: &Value,
    depths: &[Value],
    matrix: Mat4,
    setting: &SourceSetting,
    world_scale: f64,
    world_point: [f64; 2],
) -> Result<Option<f64>, String> {
    // D averages depths only when `vertices.length == 0`. A GridDeformer whose
    // axes cannot be decoded is not that case: a decoded grid always has
    // vertices. Skip it so Fit Z does not invent a sample at the origin.
    let vertices = match bone_deform::target_vertices(target) {
        Ok(vertices) => vertices,
        Err(_) => return Ok(None),
    };
    if !vertices.is_empty() {
        let mut found = false;
        let mut best_distance = f64::MAX;
        let mut best_depth = 0.0;
        for (index, vertex) in vertices.iter().enumerate() {
            if index >= depths.len() {
                break;
            }
            let Some(raw) = depths[index].as_f64() else {
                continue;
            };
            let local = source_local(vertex[0], vertex[1], raw, setting, world_scale);
            if !local.iter().all(|value| value.is_finite()) {
                continue;
            }
            let sample = bone_deform::transform_point(matrix, local).unwrap_or(local);
            let dx = sample[0] - world_point[0];
            let dy = sample[1] - world_point[1];
            let distance = dx * dx + dy * dy;
            if !found || distance < best_distance {
                found = true;
                best_distance = distance;
                best_depth = sample[2];
            }
        }
        return Ok(found.then_some(best_depth));
    }
    let mut total = 0.0;
    let mut count = 0.0;
    for value in depths {
        let Some(raw) = value.as_f64().filter(|raw| raw.is_finite()) else {
            continue;
        };
        total += raw;
        count += 1.0;
    }
    if count == 0.0 {
        return Ok(None);
    }
    let local = source_local(0.0, 0.0, total / count, setting, world_scale);
    let sample = bone_deform::transform_point(matrix, local).unwrap_or(local);
    let depth = if sample[2].is_finite() {
        sample[2]
    } else {
        local[2]
    };
    Ok(depth.is_finite().then_some(depth))
}

fn source_local(x: f64, y: f64, raw: f64, setting: &SourceSetting, world_scale: f64) -> [f64; 3] {
    let distance = (raw * setting.depth_scale + setting.depth_offset) * world_scale;
    [x + distance * tangent_slope(setting.rotation), y, distance]
}

fn tangent_slope(rotation: f64) -> f64 {
    let (sin, mut cos) = rotation.sin_cos();
    if cos.abs() < 1e-4 {
        cos = if cos < 0.0 { -1e-4 } else { 1e-4 };
    }
    sin / cos
}

fn setting_for(binding: &Value, bone_id: Option<u32>) -> Result<Option<SourceSetting>, String> {
    let Some(bone_id) = bone_id else {
        return Ok(Some(SourceSetting {
            depth_scale: 1.0,
            depth_offset: 0.0,
            rotation: 0.0,
        }));
    };
    let sources = binding.get("sourceBoneUuids").and_then(Value::as_array);
    let listed = sources.is_some_and(|ids| {
        ids.iter()
            .any(|id| id.as_u64().and_then(valid_id) == Some(bone_id))
    });
    if !listed {
        return Ok(None);
    }
    let stored = binding
        .get("sourceSettings")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("bone").and_then(Value::as_u64).and_then(valid_id) == Some(bone_id)
            })
        });
    Ok(Some(read_setting(stored)))
}

fn read_setting(value: Option<&Value>) -> SourceSetting {
    let mut setting = SourceSetting {
        depth_scale: 1.0,
        depth_offset: 0.0,
        rotation: 0.0,
    };
    let Some(value) = value else {
        return setting;
    };
    if let Some(scale) = value.get("depthScale").and_then(Value::as_f64) {
        setting.depth_scale = scale;
    }
    if let Some(offset) = value.get("depthOffset").and_then(Value::as_f64) {
        setting.depth_offset = offset;
    }
    if let Some(rotation) = value.get("rotation").and_then(Value::as_f64) {
        setting.rotation = normalize_rotation(rotation);
    }
    setting
}

fn normalize_rotation(rotation: f64) -> f64 {
    if !rotation.is_finite() {
        return 0.0;
    }
    let pi = std::f64::consts::PI;
    let two_pi = pi * 2.0;
    let mut normalized = (rotation + pi) % two_pi;
    if normalized < 0.0 {
        normalized += two_pi;
    }
    normalized - pi
}

fn depth_world_scale(posed: &Value, root_id: u32) -> Result<f64, String> {
    let root =
        nodes::find_node(posed, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Ok(1.0);
    };
    let mut min = [0.0; 2];
    let mut max = [0.0; 2];
    let mut has = false;
    for binding in bindings {
        let Some(id) = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(target) = nodes::find_node(posed, id) else {
            continue;
        };
        if !bone_deform::is_binding_target(target) {
            continue;
        }
        let Ok(vertices) = bone_deform::target_vertices(target) else {
            continue;
        };
        let Ok(matrix) = target_to_root(posed, root_id, id) else {
            continue;
        };
        for vertex in vertices {
            let Ok(point) = bone_deform::transform_point(matrix, [vertex[0], vertex[1], 0.0])
            else {
                continue;
            };
            if !point[0].is_finite() || !point[1].is_finite() {
                continue;
            }
            if !has {
                min = [point[0], point[1]];
                max = min;
                has = true;
            } else {
                min[0] = min[0].min(point[0]);
                min[1] = min[1].min(point[1]);
                max[0] = max[0].max(point[0]);
                max[1] = max[1].max(point[1]);
            }
        }
    }
    if !has {
        return Ok(1.0);
    }
    let size = (max[0] - min[0]).max(max[1] - min[1]);
    let scaled = (size as f32) * (0.42f32 / 2.9f32);
    if !scaled.is_finite() || scaled <= 0.0 {
        return Ok(1.0);
    }
    Ok(f64::from(scaled.max(1.0)))
}

fn target_to_root(posed: &Value, root_id: u32, target_id: u32) -> Result<Mat4, String> {
    let root = bone_deform::world_matrix(posed, root_id)?;
    let target = bone_deform::world_matrix(posed, target_id)?;
    Ok(bone_deform::mul_mat(bone_deform::invert(root)?, target))
}

fn posed_model(model: &Value, values: &BTreeMap<u32, [f64; 2]>) -> Result<Value, String> {
    let has_parameters = model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if !has_parameters && values.is_empty() {
        return Ok(model.clone());
    }
    evaluation::evaluate(model, values)
}

fn find_rig_root(model: &Value, bone_id: u32) -> Result<Option<u32>, String> {
    let mut current = Some(bone_id);
    while let Some(id) = current {
        let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
        if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
            return Ok(Some(id));
        }
        current = bone_deform::parent_of(model, id)?;
    }
    Ok(None)
}

fn set_local_z(model: &mut Value, id: u32, z: f64) -> Result<(), String> {
    let node = nodes::find_node_mut(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let translation = node
        .get_mut("transform")
        .and_then(Value::as_object_mut)
        .ok_or("node transform must be an object")?
        .get_mut("trans")
        .and_then(Value::as_array_mut)
        .ok_or("node transform.trans must be an array")?;
    if translation.len() < 3 {
        return Err("node transform.trans must contain three components".into());
    }
    translation[2] = json!(z);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(session: &mut crate::Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    fn local_z(session: &mut crate::Session, id: u64) -> f64 {
        let node = ok(&call(session, &json!({"op":"node.get","id":id})));
        node["transform"]["trans"][2].as_f64().unwrap()
    }

    fn display_scale(size: f64) -> f64 {
        let scaled = (size as f32) * (0.42f32 / 2.9f32);
        f64::from(scaled.max(1.0))
    }

    #[test]
    fn a_bound_part_counts_toward_the_depth_scale_of_a_fit() {
        let mut built = crate::Session::new();
        let rig = ok(&call(&mut built, &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(&mut built, &json!({
            "op":"node.create","className":"DepthBone","name":"FitBone","parents":[rig_id],
            "restHead":[-10.0,-10.0,0.0],"restTail":[-10.0,90.0,0.0]
        })));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(&mut built, &json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(&mut built, &json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[-10.0,10.0],"axisY":[-10.0,10.0]
        })));
        ok(&call(&mut built, &json!({"op":"depth.set","target":grid_id,"depths":[0.5,0.5,0.5,0.5]})));
        ok(&call(&mut built, &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id})));
        // A Part 200 wide, bound to the same rig. The fit reads depth from the grid and scales it by the size of
        // every bound target, as the deform does.
        let mut model = built.snapshot()["model"].clone();
        model["nodes"]["children"].as_array_mut().unwrap().push(json!({
            "uuid": 900, "name": "Wide", "type": "Part", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": [],
            "mesh": {"verts": [-100.0, -100.0, 100.0, -100.0, -100.0, 100.0, 100.0, 100.0],
                "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0], "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
            "textures": [u32::MAX, u32::MAX, u32::MAX], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
            "screenTint": [0.0, 0.0, 0.0], "opacity": 1.0, "emissionStrength": 1.0, "mask_threshold": 0.5,
        }));
        let mut session = crate::Session::new();
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        ok(&call(&mut session, &json!({"op":"rig.add_source","root":rig_id,"target":900,"bone":bone_id})));
        ok(&call(&mut session, &json!({"op":"rig.fit_root_z","root":rig_id})));
        let expected = 0.5 * display_scale(200.0);
        assert!((local_z(&mut session, bone_id) - expected).abs() < 1e-4, "{}", local_z(&mut session, bone_id));
    }

    #[test]
    fn root_fit_updates_every_fittable_bone_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"FitBone","parents":[rig_id],
                "restHead":[-10.0,-10.0,0.0],"restTail":[-10.0,90.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let sibling = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Sibling","parents":[rig_id],
                "restHead":[10.0,-10.0,0.0],"restTail":[10.0,90.0,0.0]
            }),
        ));
        let sibling_id = sibling["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[grid_id],"axisX":[-10.0,10.0],"axisY":[-10.0,10.0]
            }),
        ));
        let missing = call(&mut session, &json!({"op":"rig.fit_root_z","root":rig_id}));
        assert_eq!(missing["succeeded"], false);
        assert_eq!(
            missing["message"],
            "No usable mapped depth samples were found for the DepthRigRoot"
        );
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[0.5,0.5,0.5,0.5]}),
        ));
        let unbound = call(&mut session, &json!({"op":"rig.fit_root_z","root":rig_id}));
        assert_eq!(unbound["succeeded"], false);
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let held = call(&mut session, &json!({"op":"snapshot"}));
        let wrong = call(&mut session, &json!({"op":"rig.fit_root_z","root":bone_id}));
        assert_eq!(wrong["message"], "Node is not a DepthRigRoot");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            held["result"]
        );
        ok(&call(
            &mut session,
            &json!({"op":"rig.fit_root_z","root":rig_id}),
        ));
        let expected = 0.5 * display_scale(20.0);
        assert!(
            (local_z(&mut session, bone_id) - expected).abs() < 1e-4,
            "{}",
            local_z(&mut session, bone_id)
        );
        assert!((local_z(&mut session, sibling_id) - expected).abs() < 1e-4);
        let fitted = call(&mut session, &json!({"op":"snapshot"}));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})))["depths"],
            json!([0.5, 0.5, 0.5, 0.5])
        );
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})))["restHead"],
            json!([-10.0, -10.0, 0.0])
        );
        assert!(fitted["result"]["model"]["param"]
            .as_array()
            .unwrap()
            .is_empty());
        let again = call(&mut session, &json!({"op":"rig.fit_root_z","root":rig_id}));
        assert_eq!(again["succeeded"], true, "{again}");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            fitted["result"]
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert!(local_z(&mut session, bone_id).abs() < 1e-9);
        assert!(local_z(&mut session, sibling_id).abs() < 1e-9);
        ok(&call(&mut session, &json!({"op":"redo"})));
        assert!((local_z(&mut session, bone_id) - expected).abs() < 1e-4);
        assert!((local_z(&mut session, sibling_id) - expected).abs() < 1e-4);
        ok(&call(&mut session, &json!({"op":"undo"})));
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":bone_id,
                "settings":{"rotation":1.0471976,"depthScale":2.0,"depthOffset":0.5}
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.fit_bone_z","bone":bone_id}),
        ));
        let rotated = 1.5 * display_scale(20.0);
        assert!(
            (local_z(&mut session, bone_id) - rotated).abs() < 1e-4,
            "{}",
            local_z(&mut session, bone_id)
        );
        assert!(local_z(&mut session, sibling_id).abs() < 1e-9);
        let not_bone = call(&mut session, &json!({"op":"rig.fit_bone_z","bone":rig_id}));
        assert_eq!(not_bone["message"], "Node is not a DepthBone");
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})))["depths"],
            json!([0.5, 0.5, 0.5, 0.5])
        );
    }

    #[test]
    fn parent_is_fitted_before_a_coincident_child() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Parent","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let parent_id = parent["created"][0]["uuid"].as_u64().unwrap();
        let child = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Child","parents":[parent_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let child_id = child["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[1.0,1.0,1.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":parent_id}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":grid_id,"translation":[0.0,0.0,3.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.fit_root_z","root":rig_id}),
        ));
        let expected = 3.0 + display_scale(1.0);
        assert!((local_z(&mut session, parent_id) - expected).abs() < 1e-4);
        assert!(local_z(&mut session, child_id).abs() < 1e-9);
        let parameter = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Z","dimensions":1,"min":[-1.0,0.0],"max":[1.0,0.0]
            }),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[parent_id],"key":[2,0],"translation_z":0.25
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"value.set","id":parameter_id,"value":[1.0,0.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.fit_bone_z","bone":parent_id}),
        ));
        assert!((local_z(&mut session, parent_id) - (expected - 0.25)).abs() < 1e-4);
        let stored = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let binding = stored["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "transform.t.z")
            .unwrap();
        assert!((binding["values"][2][0].as_f64().unwrap() - 0.25).abs() < 1e-9);
        assert!(stored["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|binding| binding["param_name"] == "transform.t.z"));
    }

    fn posed_rig(grids: Vec<Value>) -> Value {
        let bindings: Vec<Value> = grids
            .iter()
            .enumerate()
            .map(|(index, _)| json!({"target": 4 + index as u64, "sourceBoneUuids": [3]}))
            .collect();
        let mut children = vec![json!({
            "uuid": 2, "type": "DepthRigRoot", "name": "Rig",
            "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
            "bindings": bindings,
            "children": [{
                "uuid": 3, "type": "DepthBone", "name": "Bone",
                "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                "children": [],
            }],
        })];
        children.extend(grids);
        json!({
            "nodes": {
                "uuid": 1, "type": "Node", "name": "Root",
                "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                "children": children,
            },
            "param": [],
        })
    }

    #[test]
    fn undecodable_grid_axes_do_not_use_the_vertexless_average() {
        let bad = json!({
            "uuid": 4, "type": "GridDeformer", "name": "Bad",
            "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
            "grid_axis_x": [0.0, "nope"], "grid_axis_y": [0.0, 1.0],
            "depths": [9.0, 9.0, 9.0, 9.0], "children": [],
        });
        let only_bad = posed_rig(vec![bad.clone()]);
        let failed = fit_root(&only_bad, &BTreeMap::new(), 2).unwrap_err();
        assert_eq!(
            failed,
            "No usable mapped depth samples were found for the DepthRigRoot"
        );
        let valid = json!({
            "uuid": 4, "type": "GridDeformer", "name": "Valid",
            "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
            "grid_axis_x": [0.0, 1.0], "grid_axis_y": [0.0, 1.0],
            "depths": [1.0, 1.0, 1.0, 1.0], "children": [],
        });
        let mut ignored = bad;
        ignored["uuid"] = json!(5);
        let (candidate, _) =
            fit_root(&posed_rig(vec![valid, ignored]), &BTreeMap::new(), 2).unwrap();
        let z = candidate["nodes"]["children"][0]["children"][0]["transform"]["trans"][2]
            .as_f64()
            .unwrap();
        assert!((z - 1.0).abs() < 1e-6, "{z}");
    }
}
