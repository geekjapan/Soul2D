use crate::nodes::{self, find_node};
use serde_json::{json, Value};

pub fn set_weld(
    model: &Value,
    part_id: u32,
    target_id: u32,
    indices: &[i64],
    weight: f64,
) -> Result<Value, String> {
    if !weight.is_finite() {
        return Err("weld weight must be finite".into());
    }
    if !(0.0..=1.0).contains(&weight) {
        return Err("weld weight must be between 0 and 1".into());
    }
    if part_id == target_id {
        return Err("a part cannot be welded to itself".into());
    }
    let part = part_node(model, part_id)?;
    let target = part_node(model, target_id)?;
    let part_count = crate::render::part_vertex_count(part, part_id)?;
    let target_count = crate::render::part_vertex_count(target, target_id)?;
    if indices.len() != part_count {
        return Err("weld indices length must match the part vertex count".into());
    }
    let mut seen = vec![false; target_count];
    for index in indices {
        if *index == -1 {
            continue;
        }
        if *index < 0 || *index as usize >= target_count {
            return Err(format!("weld index {index} is past the target vertices"));
        }
        let slot = *index as usize;
        if seen[slot] {
            return Err(format!("weld target index {index} is named twice"));
        }
        seen[slot] = true;
    }
    let counter = counter_indices(indices, target_count);
    let mut candidate = model.clone();
    write_link(&mut candidate, part_id, target_id, indices, weight)?;
    write_link(&mut candidate, target_id, part_id, &counter, 1.0 - weight)?;
    reject_shared_vertex(&candidate, part_id)?;
    reject_shared_vertex(&candidate, target_id)?;
    crate::validate_model(&candidate)?;
    Ok(candidate)
}

/// The weld indices D's inspector registered when a Part was dropped onto another: each part vertex takes the
/// nearest target vertex within `radius` canvas units at rest, or -1. A target vertex claimed by several part
/// vertices goes to the nearest of them, since a weld names each target vertex once.
pub fn nearest_indices(model: &Value, part_id: u32, target_id: u32, radius: f64) -> Result<Vec<i64>, String> {
    let world = |id: u32| -> Result<Vec<[f64; 2]>, String> {
        let node = part_node(model, id)?;
        let matrix = crate::bone_deform::world_matrix(model, id)?;
        crate::bone_deform::target_vertices(node)?.into_iter().map(|[x, y]| {
            crate::bone_deform::transform_point(matrix, [x, y, 0.0]).map(|[x, y, _]| [x, y])
        }).collect()
    };
    let (part, target) = (world(part_id)?, world(target_id)?);
    let mut nearest: Vec<Option<(usize, f64)>> = part.iter().map(|p| {
        target.iter().enumerate().map(|(index, t)| (index, (t[0] - p[0]).hypot(t[1] - p[1])))
            .filter(|(_, distance)| *distance < radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }).collect();
    let mut claimed: Vec<Option<(usize, f64)>> = vec![None; target.len()];
    for (vertex, found) in nearest.iter().enumerate() {
        if let Some((index, distance)) = found {
            if claimed[*index].is_none_or(|(_, best)| *distance < best) {
                claimed[*index] = Some((vertex, *distance));
            }
        }
    }
    for (vertex, found) in nearest.iter_mut().enumerate() {
        if found.is_some_and(|(index, _)| claimed[index].map(|(owner, _)| owner) != Some(vertex)) {
            *found = None;
        }
    }
    Ok(nearest.into_iter().map(|found| found.map_or(-1, |(index, _)| index as i64)).collect())
}

pub fn prune_deleted_links(model: &mut Value, removed: &std::collections::HashSet<u32>) {
    if let Some(root) = model.get_mut("nodes") {
        prune_node_links(root, removed);
    }
}

pub fn remap_copied_links(node: &mut Value, uuid_map: &std::collections::HashMap<u32, u32>) {
    let child_count = node
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.len())
        .unwrap_or(0);
    for index in 0..child_count {
        let Some(child) = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
        else {
            continue;
        };
        remap_copied_links(child, uuid_map);
    }
    let Some(object) = node.as_object_mut() else {
        return;
    };
    let Some(links) = object.get("weldedLinks").and_then(Value::as_array).cloned() else {
        return;
    };
    let mut kept = Vec::new();
    for link in links {
        let Some(target) = link
            .get("targetUUID")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
        else {
            continue;
        };
        let Some(mapped) = uuid_map.get(&target) else {
            continue;
        };
        let mut updated = link;
        if let Some(fields) = updated.as_object_mut() {
            fields.insert("targetUUID".into(), json!(mapped));
        }
        kept.push(updated);
    }
    if kept.is_empty() {
        object.remove("weldedLinks");
    } else {
        object.insert("weldedLinks".into(), Value::Array(kept));
    }
}

pub fn clear_weld(model: &Value, part_id: u32, target_id: u32) -> Result<Value, String> {
    let part = part_node(model, part_id)?;
    let _target = part_node(model, target_id)?;
    if !has_link(part, target_id) {
        return Err("welding link was not found".into());
    }
    let mut candidate = model.clone();
    remove_link(&mut candidate, part_id, target_id)?;
    remove_link(&mut candidate, target_id, part_id)?;
    crate::validate_model(&candidate)?;
    Ok(candidate)
}

fn part_node(model: &Value, id: u32) -> Result<&Value, String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if kind != "Part" {
        return Err(format!("node {id} must be a Part"));
    }
    Ok(node)
}

fn counter_indices(indices: &[i64], target_count: usize) -> Vec<i64> {
    let mut counter = vec![-1; target_count];
    for (index, target) in indices.iter().copied().enumerate() {
        if target >= 0 {
            counter[target as usize] = index as i64;
        }
    }
    counter
}

fn write_link(
    model: &mut Value,
    owner: u32,
    target: u32,
    indices: &[i64],
    weight: f64,
) -> Result<(), String> {
    let node = nodes::find_node_mut(model, owner).ok_or_else(|| format!("node {owner} was not found"))?;
    let object = node.as_object_mut().ok_or("node must be an object")?;
    let links = object
        .entry("weldedLinks".to_string())
        .or_insert_with(|| json!([]));
    let links = links.as_array_mut().ok_or("weldedLinks must be an array")?;
    links.retain(|link| link.get("targetUUID").and_then(Value::as_u64) != Some(u64::from(target)));
    links.push(json!({
        "targetUUID": target,
        "indices": indices,
        "weight": weight,
    }));
    Ok(())
}

fn remove_link(model: &mut Value, owner: u32, target: u32) -> Result<(), String> {
    let Some(node) = nodes::find_node_mut(model, owner) else {
        return Ok(());
    };
    let Some(object) = node.as_object_mut() else {
        return Ok(());
    };
    let Some(links) = object.get_mut("weldedLinks").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    links.retain(|link| link.get("targetUUID").and_then(Value::as_u64) != Some(u64::from(target)));
    if links.is_empty() {
        object.remove("weldedLinks");
    }
    Ok(())
}

fn reject_shared_vertex(model: &Value, id: u32) -> Result<(), String> {
    let Some(node) = find_node(model, id) else {
        return Ok(());
    };
    let Some(links) = node.get("weldedLinks").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut claimed = std::collections::HashMap::<usize, u32>::new();
    for link in links {
        let target = link
            .get("targetUUID")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("node {id} weld target must be a UUID"))?;
        let indices = link
            .get("indices")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("node {id} weld indices must be an array"))?;
        for (index, value) in indices.iter().enumerate() {
            let mapped = value
                .as_i64()
                .ok_or_else(|| format!("node {id} weld index must be an integer"))?;
            if mapped < 0 {
                continue;
            }
            if let Some(previous) = claimed.insert(index, target) {
                if previous != target {
                    return Err("a vertex cannot be welded to two targets".into());
                }
            }
        }
    }
    Ok(())
}

fn prune_node_links(node: &mut Value, removed: &std::collections::HashSet<u32>) {
    let child_count = node
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.len())
        .unwrap_or(0);
    for index in 0..child_count {
        let Some(child) = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
        else {
            continue;
        };
        prune_node_links(child, removed);
    }
    let Some(object) = node.as_object_mut() else {
        return;
    };
    let Some(links) = object.get_mut("weldedLinks").and_then(Value::as_array_mut) else {
        return;
    };
    links.retain(|link| {
        link.get("targetUUID")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .is_none_or(|id| !removed.contains(&id))
    });
    if links.is_empty() {
        object.remove("weldedLinks");
    }
}

fn has_link(node: &Value, target: u32) -> bool {
    node.get("weldedLinks")
        .and_then(Value::as_array)
        .is_some_and(|links| {
            links
                .iter()
                .any(|link| link.get("targetUUID").and_then(Value::as_u64) == Some(u64::from(target)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(id: u64) -> Value {
        json!({
            "uuid": id, "type": "Part", "enabled": true, "zsort": 0,
            "textures": [u32::MAX, u32::MAX, u32::MAX],
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "mesh": {
                "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                "indices": [0, 1, 2],
                "origin": [0.0, 0.0]
            }
        })
    }

    fn model() -> Value {
        json!({
            "nodes": {
                "uuid": 1, "type": "Node", "enabled": true,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [part(4), part(5)]
            },
            "param": [], "automation": []
        })
    }

    #[test]
    fn set_weld_writes_both_links_and_one_undo_restores_them() {
        let mut session = crate::Session::new();
        let opened = session.handle_request(&json!({"op": "new", "model": model()}));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let before = session.handle_request(&json!({"op": "snapshot"}));
        let welded = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5,
            "indices": [0, -1, -1], "weight": 0.25
        }));
        assert_eq!(welded["succeeded"], true, "{welded}");
        let part = session.handle_request(&json!({"op": "node.get", "id": 4}));
        let target = session.handle_request(&json!({"op": "node.get", "id": 5}));
        assert_eq!(part["result"]["weldedLinks"][0]["weight"], 0.25);
        assert_eq!(part["result"]["weldedLinks"][0]["indices"], json!([0, -1, -1]));
        assert_eq!(target["result"]["weldedLinks"][0]["weight"], 0.75);
        assert_eq!(target["result"]["weldedLinks"][0]["indices"], json!([0, -1, -1]));
        assert_eq!(target["result"]["weldedLinks"][0]["targetUUID"], 4);
        session.handle_request(&json!({"op": "undo"}));
        assert_eq!(
            session.handle_request(&json!({"op": "snapshot"}))["result"]["model"],
            before["result"]["model"]
        );

        let again = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5,
            "indices": [0, -1, -1], "weight": 0.25
        }));
        assert_eq!(again["succeeded"], true, "{again}");
        let held = session.handle_request(&json!({"op": "snapshot"}));
        let rejected = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5,
            "indices": [0, 0, -1], "weight": 0.25
        }));
        assert_eq!(rejected["succeeded"], false, "{rejected}");
        assert!(rejected["message"].as_str().unwrap_or("").contains("named twice"));
        assert_eq!(session.handle_request(&json!({"op": "snapshot"})), held);
        let cleared = session.handle_request(&json!({"op": "part.clear_weld", "part": 4, "target": 5}));
        assert_eq!(cleared["succeeded"], true, "{cleared}");
        let gone = session.handle_request(&json!({"op": "node.get", "id": 4}));
        assert!(gone["result"].get("weldedLinks").is_none());
        let other = session.handle_request(&json!({"op": "node.get", "id": 5}));
        assert!(other["result"].get("weldedLinks").is_none());
        session.handle_request(&json!({"op": "undo"}));
        let restored = session.handle_request(&json!({"op": "node.get", "id": 5}));
        assert_eq!(restored["result"]["weldedLinks"][0]["weight"], 0.75);
    }

    #[test]
    fn set_weld_without_indices_welds_the_nearest_vertices_within_four_units() {
        let at = |x: f64| {
            let mut model = model();
            model["nodes"]["children"][1]["transform"]["trans"] = json!([x, 0.0, 0.0]);
            let mut session = crate::Session::new();
            assert_eq!(session.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
            let welded = session.handle_request(&json!({"op": "part.set_weld", "part": 4, "target": 5}));
            (session, welded)
        };
        let (mut session, welded) = at(0.4);
        assert_eq!(welded["result"]["indices"], json!([0, 1, 2]), "{welded}");
        let part = session.handle_request(&json!({"op": "node.get", "id": 4}));
        assert_eq!(part["result"]["weldedLinks"][0]["weight"], 0.5);
        // Vertices 0 and 1 are both 0.5 from target vertex 0; it goes to the first, and vertex 1 stays loose.
        assert_eq!(at(0.5).1["result"]["indices"], json!([0, -1, 2]));
        let (mut session, far) = at(10.0);
        assert_eq!(far["succeeded"], false);
        assert!(far["message"].as_str().unwrap().contains("within 4 units"), "{far}");
        assert!(session.handle_request(&json!({"op": "node.get", "id": 4}))["result"].get("weldedLinks").is_none());
    }

    #[test]
    fn weight_one_pulls_the_other_part_and_a_one_sided_link_does_not() {
        let mut session = crate::Session::new();
        assert_eq!(
            session.handle_request(&json!({"op": "new", "model": model()}))["succeeded"],
            true
        );
        assert_eq!(
            session.handle_request(&json!({
                "op": "part.set_weld", "part": 4, "target": 5,
                "indices": [0, 1, 2], "weight": 1.0
            }))["succeeded"],
            true
        );
        let welded = session.handle_request(&json!({"op": "snapshot"}))["result"]["model"].clone();
        let mut deformations = std::collections::BTreeMap::new();
        deformations.insert(5, vec![[4.0, 0.0], [4.0, 0.0], [4.0, 0.0]]);
        let pulled = crate::render::scene_bounds_with(&welded, &deformations).unwrap();
        let mut loose = welded.clone();
        loose["nodes"]["children"][1]
            .as_object_mut()
            .unwrap()
            .remove("weldedLinks");
        let one_sided = crate::render::scene_bounds_with(&loose, &deformations).unwrap();
        let mut none = loose.clone();
        none["nodes"]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("weldedLinks");
        let apart = crate::render::scene_bounds_with(&none, &deformations).unwrap();
        assert!((apart[2] - 5.5).abs() < 1e-9, "{apart:?}");
        assert!((one_sided[2] - 5.5).abs() < 1e-9, "{one_sided:?}");
        assert!((pulled[2] - 1.1).abs() < 1e-9, "{pulled:?}");
        assert_eq!(
            session.handle_request(&json!({"op": "snapshot"}))["result"]["model"]["nodes"]["children"][1]["mesh"]["verts"],
            json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0])
        );
    }

    #[test]
    fn set_weld_counts_the_vertices_the_raster_uses() {
        let mut mixed = model();
        let part = mixed["nodes"]["children"][0].as_object_mut().unwrap();
        part.insert("vertices".into(), json!([]));
        part.insert("uvs".into(), json!([]));
        part.insert("indices".into(), json!([]));
        let rejected = set_weld(&mixed, 4, 5, &[], 1.0).unwrap_err();
        assert!(rejected.contains("vertex count"), "{rejected}");
        let accepted = set_weld(&mixed, 4, 5, &[0, -1, -1], 1.0).unwrap();
        assert_eq!(
            accepted["nodes"]["children"][0]["weldedLinks"][0]["indices"],
            json!([0, -1, -1])
        );

        let mut mismatched = model();
        mismatched["nodes"]["children"][0]["vertices"] = json!([[0.0, 0.0], [1.0, 0.0]]);
        let mismatch = set_weld(&mismatched, 4, 5, &[0, -1], 1.0).unwrap_err();
        assert!(mismatch.contains("vertex count"), "{mismatch}");

        let mut vertices_only = model();
        for child in vertices_only["nodes"]["children"].as_array_mut().unwrap() {
            let object = child.as_object_mut().unwrap();
            object.remove("mesh");
            object.insert(
                "vertices".into(),
                json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            );
            object.insert("uvs".into(), json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
            object.insert("indices".into(), json!([0, 1, 2]));
        }
        let welded = set_weld(&vertices_only, 4, 5, &[0, 1, 2], 1.0).unwrap();
        assert_eq!(
            welded["nodes"]["children"][1]["weldedLinks"][0]["indices"],
            json!([0, 1, 2])
        );
        let short = set_weld(&vertices_only, 4, 5, &[0, -1], 1.0).unwrap_err();
        assert!(short.contains("vertex count"), "{short}");
    }

    #[test]
    fn weld_counts_the_mesh_filled_from_part_arrays_and_a_converted_mask() {
        let mut session = crate::Session::new();
        let opened = session.handle_request(&json!({
            "op": "new",
            "model": {
                "nodes": {
                    "uuid": 1, "type": "Node",
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "children": [{
                        "uuid": 4, "type": "Part", "enabled": true,
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                        "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                        "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                        "indices": [0, 1, 2]
                    }, {
                        "uuid": 5, "type": "Mask", "enabled": true,
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                        "mesh": {
                            "verts": [0.0, 0.0, 2.0, 0.0, 0.0, 2.0],
                            "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                            "indices": [0, 1, 2],
                            "origin": [0.0, 0.0]
                        }
                    }]
                },
                "param": []
            }
        }));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let card = session.handle_request(&json!({"op": "node.get", "id": 4}));
        assert_eq!(card["result"]["mesh"]["verts"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        let converted = session.handle_request(&json!({"op": "node.convert", "nodes": [5], "className": "Part"}));
        assert_eq!(converted["succeeded"], true, "{converted}");
        let short = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5, "indices": [0, -1], "weight": 0.25
        }));
        assert_eq!(short["succeeded"], false, "{short}");
        let welded = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5, "indices": [0, 1, 2], "weight": 0.25
        }));
        assert_eq!(welded["succeeded"], true, "{welded}");
        let part = session.handle_request(&json!({"op": "node.get", "id": 4}));
        assert_eq!(part["result"]["weldedLinks"][0]["indices"], json!([0, 1, 2]));
        let cover = session.handle_request(&json!({"op": "node.get", "id": 5}));
        assert_eq!(cover["result"]["type"], "Part");
        assert_eq!(cover["result"]["mesh"]["verts"], json!([0.0, 0.0, 2.0, 0.0, 0.0, 2.0]));
        assert_eq!(cover["result"]["weldedLinks"][0]["weight"], 0.75);
    }

    #[test]
    fn delete_prunes_the_surviving_link_and_undo_restores_it() {
        let mut session = crate::Session::new();
        assert_eq!(
            session.handle_request(&json!({"op": "new", "model": model()}))["succeeded"],
            true
        );
        assert_eq!(
            session.handle_request(&json!({
                "op": "part.set_weld", "part": 4, "target": 5,
                "indices": [0, -1, -1], "weight": 0.25
            }))["succeeded"],
            true
        );
        let before = session.handle_request(&json!({"op": "snapshot"}));
        let deleted = session.handle_request(&json!({"op": "node.delete", "nodes": [5]}));
        assert_eq!(deleted["succeeded"], true, "{deleted}");
        let survivor = session.handle_request(&json!({"op": "node.get", "id": 4}));
        assert!(survivor["result"].get("weldedLinks").is_none(), "{survivor}");
        session.handle_request(&json!({"op": "undo"}));
        assert_eq!(
            session.handle_request(&json!({"op": "snapshot"}))["result"]["model"],
            before["result"]["model"]
        );
        let restored = session.handle_request(&json!({"op": "node.get", "id": 5}));
        assert_eq!(restored["result"]["weldedLinks"][0]["targetUUID"], 4);
    }

    #[test]
    fn paste_remaps_weld_targets_to_the_copied_pair() {
        let mut session = crate::Session::new();
        assert_eq!(
            session.handle_request(&json!({"op": "new", "model": model()}))["succeeded"],
            true
        );
        assert_eq!(
            session.handle_request(&json!({
                "op": "part.set_weld", "part": 4, "target": 5,
                "indices": [0, -1, -1], "weight": 0.25
            }))["succeeded"],
            true
        );
        let before = session.handle_request(&json!({"op": "snapshot"}));
        assert_eq!(
            session.handle_request(&json!({"op": "node.copy", "nodes": [4, 5]}))["succeeded"],
            true
        );
        let pasted = session.handle_request(&json!({"op": "node.paste", "parent": 1}));
        assert_eq!(pasted["succeeded"], true, "{pasted}");
        let created = pasted["result"]["created"].as_array().unwrap();
        let ids: Vec<u64> = created.iter().map(|node| node["uuid"].as_u64().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        for node in created {
            let target = node["weldedLinks"][0]["targetUUID"].as_u64().unwrap();
            assert!(ids.contains(&target), "{node}");
            assert!(target != 4 && target != 5, "{node}");
            assert_eq!(node["weldedLinks"][0]["indices"], json!([0, -1, -1]));
        }
        assert_eq!(
            session.handle_request(&json!({"op": "node.get", "id": 4}))["result"]["weldedLinks"][0]["targetUUID"],
            5
        );
        session.handle_request(&json!({"op": "undo"}));
        assert_eq!(
            session.handle_request(&json!({"op": "snapshot"}))["result"]["model"],
            before["result"]["model"]
        );
    }

    #[test]
    fn a_vertex_welded_to_two_targets_fails_closed() {
        let mut document = model();
        document["nodes"]["children"]
            .as_array_mut()
            .unwrap()
            .push(part(6));
        let first = set_weld(&document, 4, 5, &[0, -1, -1], 1.0).unwrap();
        let overlap = set_weld(&first, 4, 6, &[0, -1, -1], 1.0).unwrap_err();
        assert!(overlap.contains("two targets"), "{overlap}");
        assert_eq!(first["nodes"]["children"][0]["weldedLinks"].as_array().unwrap().len(), 1);
        let disjoint = set_weld(&first, 4, 6, &[-1, 0, -1], 1.0).unwrap();
        assert_eq!(disjoint["nodes"]["children"][0]["weldedLinks"].as_array().unwrap().len(), 2);
        let mut doubled = disjoint.clone();
        doubled["nodes"]["children"][0]["weldedLinks"][0]["indices"] = json!([0, -1, -1]);
        doubled["nodes"]["children"][0]["weldedLinks"][1]["indices"] = json!([0, -1, -1]);
        let draw = crate::render::scene_bounds_with(&doubled, &std::collections::BTreeMap::new());
        assert!(draw.unwrap_err().contains("two targets"));
    }

    #[test]
    fn weight_outside_zero_to_one_leaves_the_model_unchanged() {
        let mut session = crate::Session::new();
        assert_eq!(
            session.handle_request(&json!({"op": "new", "model": model()}))["succeeded"],
            true
        );
        let before = session.handle_request(&json!({"op": "snapshot"}));
        let rejected = session.handle_request(&json!({
            "op": "part.set_weld", "part": 4, "target": 5,
            "indices": [0, 1, 2], "weight": 2.0
        }));
        assert_eq!(rejected["succeeded"], false, "{rejected}");
        assert!(rejected["message"].as_str().unwrap_or("").contains("between 0 and 1"));
        assert_eq!(session.handle_request(&json!({"op": "snapshot"})), before);
        let mut loaded = model();
        loaded["nodes"]["children"][0]["weldedLinks"] = json!([{
            "targetUUID": 5, "indices": [0, 1, 2], "weight": 2.0
        }]);
        loaded["nodes"]["children"][1]["weldedLinks"] = json!([{
            "targetUUID": 4, "indices": [0, 1, 2], "weight": -1.0
        }]);
        let draw = crate::render::scene_bounds_with(&loaded, &std::collections::BTreeMap::new());
        assert!(draw.unwrap_err().contains("between 0 and 1"));
    }
}
