//! Per-vertex depth arrays for GridDeformer and PathDeformer (slices D0 and D1), and for the mesh vertices of a
//! Part, which a rig may bind.
//!
//! `depths` must match the target vertex count. `SetDepths` and Path reads
//! normalize samples with D `ngFiniteDepthOrZero` on `float`. `ClearDepths`
//! removes the key, matching `replaceDepths(null)`. Depth operations remain
//! GridDeformer-only.
//!
//! Depth-operation editing remains GridDeformer-only in `depth_ops`. Rebuffer
//! helpers live in `depth_resample`. PSD import stays rejected. Rig nodes stay
//! opaque: save keeps them, and export drops `DepthBone` / `DepthRigRoot`.

use crate::nodes::{find_node, find_node_mut};
use crate::{valid_id, validate_model};
use serde_json::{json, Value};

/// D `ngFiniteDepthOrZero`: values that are not finite `float`s become 0.
pub(crate) fn finite_depth_or_zero(value: f64) -> f64 {
    let sample = value as f32;
    if sample.is_finite() {
        f64::from(sample)
    } else {
        0.0
    }
}

pub(crate) fn list_depths(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_depth_mapped(node)?;
    Ok(depths_payload(id, node)?)
}

pub(crate) fn set_depths(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let raw = request.get("depths").ok_or("depths not provided")?;
    let depths = parse_depths(raw)?;
    let mut candidate = model.clone();
    store_depths(&mut candidate, id, &depths)?;
    validate_model(&candidate)?;
    Ok(candidate)
}

/// Stores `depths`, one for each vertex of the target `id`, in `candidate` itself. An import that sets the depths
/// of many targets calls this for each and validates the model once.
pub(crate) fn store_depths(candidate: &mut Value, id: u32, depths: &[f64]) -> Result<(), String> {
    let node = find_node_mut(candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_depth_mapped(node)?;
    if depths.len() != vertex_count(node)? {
        return Err("depths length must match target vertices".into());
    }
    let depths: Vec<f64> = depths.iter().map(|depth| finite_depth_or_zero(*depth)).collect();
    node.as_object_mut()
        .ok_or("node must be an object")?
        .insert("depths".into(), json!(depths));
    Ok(())
}

pub(crate) fn clear_depths(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_depth_mapped(node)?;
    let mut candidate = model.clone();
    let node = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    if let Some(object) = node.as_object_mut() {
        object.remove("depths");
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

/// Save keeps rig nodes. Export drops rig nodes, keeps lifted descendants in
/// world space, and prunes parameter bindings that pointed at removed UUIDs.
pub(crate) fn model_for_export(model: &Value) -> Result<Value, String> {
    crate::nodes::model_without_rig_nodes(model)
}

fn depths_payload(id: u32, node: &Value) -> Result<Value, String> {
    let (depths, count) = match node.get("depths") {
        None | Some(Value::Null) => (Value::Null, -1),
        Some(Value::Array(values)) => {
            let count = i64::try_from(values.len()).unwrap_or(i64::MAX);
            let values = if node.get("type").and_then(Value::as_str) == Some("PathDeformer") {
                Value::Array(
                    parse_depths(&Value::Array(values.clone()))?
                        .into_iter()
                        .map(|value| json!(value))
                        .collect(),
                )
            } else {
                Value::Array(values.clone())
            };
            (values, count)
        }
        Some(_) => return Err("depths must be an array".into()),
    };
    Ok(json!({
        "target": id,
        "depths": depths,
        "count": count,
    }))
}

fn require_depth_mapped(node: &Value) -> Result<(), String> {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if !matches!(kind, "GridDeformer" | "PathDeformer" | "Part") {
        return Err("target must be a GridDeformer, PathDeformer or Part".into());
    }
    Ok(())
}

fn vertex_count(node: &Value) -> Result<usize, String> {
    match node.get("type").and_then(Value::as_str) {
        Some("PathDeformer") => crate::depth_resample::path_vertex_count(node.get("vertices")),
        // One depth for each mesh vertex of a Part.
        Some("Part") => Ok(crate::bone_deform::target_vertices(node)?.len()),
        _ => grid_vertex_count(node),
    }
}

fn grid_vertex_count(node: &Value) -> Result<usize, String> {
    let xs = node
        .get("grid_axis_x")
        .and_then(Value::as_array)
        .ok_or("GridDeformer vertices are not a rectangular grid")?;
    let ys = node
        .get("grid_axis_y")
        .and_then(Value::as_array)
        .ok_or("GridDeformer vertices are not a rectangular grid")?;
    if xs.len() < 2 || ys.len() < 2 {
        return Err("GridDeformer vertices are not a rectangular grid".into());
    }
    xs.len()
        .checked_mul(ys.len())
        .ok_or_else(|| "GridDeformer vertices are not a rectangular grid".to_string())
}

fn parse_depths(value: &Value) -> Result<Vec<f64>, String> {
    let values = value
        .as_array()
        .ok_or("depths must be an array of numbers")?;
    let mut depths = Vec::with_capacity(values.len());
    for entry in values {
        let Some(number) = entry.as_f64() else {
            return Err("depths must be an array of numbers".into());
        };
        depths.push(finite_depth_or_zero(number));
    }
    Ok(depths)
}

fn target_id(request: &Value) -> Result<u32, String> {
    if let Some(value) = request.get("target").or_else(|| request.get("id")) {
        return parse_id(value, "target");
    }
    if let Some(nodes) = request.get("nodes").or_else(|| request.get("ids")) {
        let nodes = nodes
            .as_array()
            .ok_or("nodes must be an array of node UUIDs")?;
        if nodes.len() != 1 {
            return Err("exactly one depth target is required".into());
        }
        return parse_id(&nodes[0], "target");
    }
    Err("target must be a node UUID".into())
}

fn parse_id(value: &Value, name: &str) -> Result<u32, String> {
    let raw = if let Some(number) = value.as_u64() {
        number
    } else if let Some(text) = value.as_str() {
        text.parse::<u64>()
            .map_err(|_| format!("{name} must be a node UUID"))?
    } else {
        return Err(format!("{name} must be a node UUID"));
    };
    valid_id(raw).ok_or_else(|| format!("{name} must be a node UUID"))
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

    #[test]
    fn non_finite_depths_match_d_float_zero() {
        assert_eq!(finite_depth_or_zero(0.25), 0.25);
        assert_eq!(finite_depth_or_zero(0.1), f64::from(0.1f32));
        assert_eq!(finite_depth_or_zero(f64::NAN), 0.0);
        assert_eq!(finite_depth_or_zero(f64::INFINITY), 0.0);
        assert_eq!(finite_depth_or_zero(f64::NEG_INFINITY), 0.0);
        assert_eq!(finite_depth_or_zero(1.0e39), 0.0);

        let path = json!({"type":"PathDeformer","depths":[0.1,1.0e39]});
        let listed = depths_payload(2, &path).unwrap();
        assert_eq!(listed["depths"], json!([f64::from(0.1f32), 0.0]));
    }

    #[test]
    fn path_depth_set_rejects_missing_vertex_storage() {
        let model = json!({
            "nodes": {
                "uuid": 1,
                "type": "Node",
                "children": [{"uuid": 2, "type": "PathDeformer"}],
            },
        });
        let error = set_depths(&model, &json!({"target":2,"depths":[]})).unwrap_err();
        assert!(
            error.contains("PathDeformer vertices must be an array"),
            "{error}"
        );
    }

    #[test]
    fn path_depth_commands_validate_undo_and_survive_reopen() {
        let mut session = crate::Session::new();
        let path = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = path["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[0.0,0.0,1.0,0.0]}),
        ));

        let set = ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":path_id,"depths":[0.1,1.0e39]}),
        ));
        let expected = json!([f64::from(0.1f32), 0.0]);
        assert_eq!(set["depths"], expected);
        assert_eq!(set["count"], 2);

        let before_rejection = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        for request in [
            json!({"op":"depth.set","target":path_id,"depths":[0.5]}),
            json!({"op":"depth.clear","target":1}),
            json!({"op":"depth.list","target":999}),
        ] {
            let response = call(&mut session, &request);
            assert_eq!(response["succeeded"], false, "{response}");
            assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before_rejection);
        }

        let cleared = ok(&call(
            &mut session,
            &json!({"op":"depth.clear","target":path_id}),
        ));
        assert_eq!(cleared["depths"], Value::Null);
        assert_eq!(cleared["count"], -1);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before_rejection
        );
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"depth.list","target":path_id})))["depths"],
            expected
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let unset = ok(&call(&mut session, &json!({"op":"depth.list","target":path_id})));
        assert_eq!(unset["depths"], Value::Null);
        assert_eq!(unset["count"], -1);
        ok(&call(&mut session, &json!({"op":"redo"})));

        let path = std::env::temp_dir().join(format!(
            "vsoul-path-depths-{}-{path_id}.inx",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        ok(&call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy()}),
        ));
        let mut reopened = crate::Session::new();
        ok(&call(
            &mut reopened,
            &json!({"op":"open","path":path.to_string_lossy()}),
        ));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            ok(&call(&mut reopened, &json!({"op":"depth.list","target":path_id})))["depths"],
            expected
        );
    }
}
