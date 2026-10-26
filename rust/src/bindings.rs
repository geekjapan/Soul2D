//! Depth rig binding table, matching `ExDepthRigBinding` on `DepthRigRoot`.
//!
//! The table lives on the rig root. Targets do not grow a source list. Removing
//! the last source deletes that row. Source `rotation` is normalized on write.
//! `vertexWeights` on a binding is the persisted skinning weight (P4-W1).
//! Payloads named `weights` or `perVertexWeights` stay rejected. Delete and
//! convert rewrite this table before the deform refresh.

use crate::nodes::{self};
use crate::valid_id;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const WEIGHT_ARRAY_KEYS: &[&str] = &["weights", "vertexWeights", "perVertexWeights"];
const LOCKED_WEIGHTS: &str = "vertexWeights must be cleared before the source list changes";

pub(crate) fn reject_weight_arrays(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if WEIGHT_ARRAY_KEYS.contains(&key.as_str()) && child.is_array() {
                    return Err("per-vertex weights are not supported in Rust Core mode".into());
                }
                reject_weight_arrays(child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_weight_arrays(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(crate) fn drop_deleted_references(model: &mut Value, removed: &std::collections::HashSet<u32>) -> Result<(), String> {
    if removed.is_empty() {
        return Ok(());
    }
    let rigs = rig_ids(model);
    for rig_id in rigs {
        if removed.contains(&rig_id) {
            continue;
        }
        let root = nodes::find_node_mut(model, rig_id).ok_or_else(|| format!("node {rig_id} was not found"))?;
        rewrite_deleted_bindings(root, removed)?;
    }
    Ok(())
}

pub(crate) fn retarget_converted_node(model: &mut Value, id: u32, to_type: &str) -> Result<(), String> {
    let kind = match to_type {
        "GridDeformer" => Some("grid"),
        "PathDeformer" => Some("path"),
        _ => None,
    };
    let rigs = rig_ids(model);
    for rig_id in rigs {
        let root = nodes::find_node_mut(model, rig_id).ok_or_else(|| format!("node {rig_id} was not found"))?;
        rewrite_converted_target(root, id, kind)?;
    }
    Ok(())
}

pub(crate) fn remap_copied_bindings(node: &mut Value, uuid_map: &std::collections::HashMap<u32, u32>) -> Result<(), String> {
    if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
        if let Some(bindings) = node.get_mut("bindings").and_then(Value::as_array_mut) {
            for binding in bindings {
                remap_binding(binding, uuid_map)?;
            }
        }
    }
    let child_count = node.get("children").and_then(Value::as_array).map(|children| children.len()).unwrap_or(0);
    for index in 0..child_count {
        let child = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
            .ok_or("node children must be an array")?;
        remap_copied_bindings(child, uuid_map)?;
    }
    Ok(())
}

pub(crate) fn add_source(model: &Value, root_id: u32, target_id: u32, bone_id: u32) -> Result<Value, String> {
    let mut candidate = model.clone();
    add_source_in(&mut candidate, root_id, target_id, bone_id)?;
    Ok(candidate)
}

/// `add_source` on `candidate` itself, for an import that binds many targets and must not copy the model for each.
/// An error may leave `candidate` half changed, so its caller drops it.
pub(crate) fn add_source_in(candidate: &mut Value, root_id: u32, target_id: u32, bone_id: u32) -> Result<(), String> {
    reject_weight_source_edit(candidate, root_id, target_id)?;
    let (target_kind, target) = require_target(candidate, target_id)?;
    // A Part without a mesh has nothing to move, and a key the rig wrote for it would stand in the way of automesh.
    if target_kind == "part" && crate::bone_deform::target_vertices(target)?.is_empty() {
        return Err("target Part has no mesh vertices".into());
    }
    require_source_bone(candidate, root_id, bone_id)?;
    let empty = {
        let root = require_root_mut(candidate, root_id)?;
        let binding = binding_mut(root, target_id, Some(target_kind))?;
        let sources = source_ids(binding)?;
        if !sources.contains(&bone_id) {
            push_source(binding, bone_id)?;
        }
        normalize_binding(binding)?;
        binding_sources(binding)?.is_empty()
    };
    if empty {
        let root = require_root_mut(candidate, root_id)?;
        remove_binding(root, target_id)?;
    }
    Ok(())
}

pub(crate) fn remove_source(model: &Value, root_id: u32, target_id: u32, bone_id: u32) -> Result<Value, String> {
    require_target(model, target_id)?;
    require_source_bone(model, root_id, bone_id)?;
    reject_weight_source_edit(model, root_id, target_id)?;
    let mut candidate = model.clone();
    let missing = {
        let root = require_root_mut(&mut candidate, root_id)?;
        find_binding_index(root, target_id)?.is_none()
    };
    if missing {
        return Ok(candidate);
    }
    let empty = {
        let root = require_root_mut(&mut candidate, root_id)?;
        let binding = binding_mut(root, target_id, None)?;
        let mut sources = source_ids(binding)?;
        sources.retain(|id| *id != bone_id);
        set_sources(binding, &sources)?;
        if let Some(rule) = binding.get_mut("influenceRule").and_then(Value::as_object_mut) {
            if let Some(multipliers) = rule.get_mut("multipliersByBoneUuid").and_then(Value::as_object_mut) {
                multipliers.remove(&bone_id.to_string());
            }
        }
        normalize_binding(binding)?;
        binding_sources(binding)?.is_empty()
    };
    if empty {
        let root = require_root_mut(&mut candidate, root_id)?;
        remove_binding(root, target_id)?;
    }
    Ok(candidate)
}

pub(crate) fn list_sources(model: &Value, root_id: u32, target_id: u32) -> Result<Value, String> {
    require_root(model, root_id)?;
    require_target(model, target_id)?;
    let root = require_root(model, root_id)?;
    let Some(index) = find_binding_index(root, target_id)? else {
        return Ok(json!({
            "target": target_id,
            "sourceBoneUuids": [],
            "sources": [],
        }));
    };
    let mut binding = root["bindings"][index].clone();
    normalize_binding(&mut binding)?;
    let sources = binding_sources(&binding)?;
    Ok(json!({
        "target": target_id,
        "sourceBoneUuids": sources.iter().map(|source| json!(source.uuid)).collect::<Vec<_>>(),
        "sources": sources.iter().map(source_json).collect::<Vec<_>>(),
    }))
}

pub(crate) fn set_source_settings(
    model: &Value,
    root_id: u32,
    target_id: u32,
    bone_id: u32,
    settings: &Value,
) -> Result<Value, String> {
    let settings = parse_json_value(settings, "settings")?;
    reject_weight_arrays(&settings)?;
    let (target_kind, _) = require_target(model, target_id)?;
    require_source_bone(model, root_id, bone_id)?;
    reject_new_source_while_weighted(model, root_id, target_id, bone_id)?;
    let mut candidate = model.clone();
    let root = require_root_mut(&mut candidate, root_id)?;
    let binding = binding_mut(root, target_id, Some(target_kind))?;
    let mut current = binding_sources(binding)?
        .into_iter()
        .find(|source| source.uuid == bone_id)
        .unwrap_or(SourceSetting {
            uuid: bone_id,
            weight: 1.0,
            depth_offset: 0.0,
            depth_scale: 1.0,
            rotation: 0.0,
        });
    apply_settings(&mut current, &settings)?;
    let mut sources = source_ids(binding)?;
    if !sources.contains(&bone_id) {
        sources.push(bone_id);
        set_sources(binding, &sources)?;
    }
    normalize_binding(binding)?;
    let stored = binding
        .get_mut("sourceSettings")
        .and_then(Value::as_array_mut)
        .ok_or("sourceSettings must be an array")?;
    if let Some(entry) = stored.iter_mut().find(|entry| entry.get("bone").and_then(Value::as_u64) == Some(u64::from(bone_id)))
    {
        *entry = source_stored(&current);
    }
    Ok(candidate)
}

pub(crate) fn set_rule(model: &Value, root_id: u32, target_id: u32, rule: &Value) -> Result<Value, String> {
    let rule = parse_json_value(rule, "rule")?;
    reject_weight_arrays(&rule)?;
    let (target_kind, _) = require_target(model, target_id)?;
    let mut candidate = model.clone();
    let root = require_root_mut(&mut candidate, root_id)?;
    let binding = binding_mut(root, target_id, Some(target_kind))?;
    let mut next = read_rule(binding.get("influenceRule"))?;
    apply_rule(&mut next, &rule)?;
    binding
        .as_object_mut()
        .ok_or("binding must be an object")?
        .insert("influenceRule".into(), rule_stored(&next));
    Ok(candidate)
}

pub(crate) fn get_rule(model: &Value, root_id: u32, target_id: u32) -> Result<Value, String> {
    require_root(model, root_id)?;
    require_target(model, target_id)?;
    let root = require_root(model, root_id)?;
    let rule = if let Some(index) = find_binding_index(root, target_id)? {
        read_rule(root["bindings"][index].get("influenceRule"))?
    } else {
        Rule::default()
    };
    Ok(rule_json(&rule))
}

pub(crate) fn set_vertex_weights(
    model: &Value,
    root_id: u32,
    target_id: u32,
    weights: &Value,
) -> Result<Value, String> {
    let mut candidate = model.clone();
    set_vertex_weights_in(&mut candidate, root_id, target_id, weights)?;
    Ok(candidate)
}

/// `set_vertex_weights` on `candidate` itself. Nothing is changed before every check has passed.
pub(crate) fn set_vertex_weights_in(
    candidate: &mut Value,
    root_id: u32,
    target_id: u32,
    weights: &Value,
) -> Result<(), String> {
    let (target_kind, target) = require_target(candidate, target_id)?;
    let root = require_root(candidate, root_id)?;
    let index = find_binding_index(root, target_id)?.ok_or("Depth rig binding is required")?;
    let sources = source_ids(&root["bindings"][index])?;
    if sources.is_empty() {
        return Err("Depth rig binding has no bone sources".into());
    }
    let count = crate::bone_deform::target_vertices(target)?.len();
    let rows = parse_weight_rows(weights, &sources, count)?;
    let root = require_root_mut(candidate, root_id)?;
    let binding = binding_mut(root, target_id, Some(target_kind))?;
    binding
        .as_object_mut()
        .ok_or("binding must be an object")?
        .insert("vertexWeights".into(), json!(rows));
    Ok(())
}

pub(crate) fn clear_vertex_weights(model: &Value, root_id: u32, target_id: u32) -> Result<Value, String> {
    require_root(model, root_id)?;
    require_target(model, target_id)?;
    let root = require_root(model, root_id)?;
    let Some(index) = find_binding_index(root, target_id)? else {
        return Ok(model.clone());
    };
    if root["bindings"][index].get("vertexWeights").is_none() {
        return Ok(model.clone());
    }
    let mut candidate = model.clone();
    let root = require_root_mut(&mut candidate, root_id)?;
    let binding = binding_mut(root, target_id, None)?;
    binding
        .as_object_mut()
        .ok_or("binding must be an object")?
        .remove("vertexWeights");
    Ok(candidate)
}

pub(crate) fn reject_vertex_weight_resize(model: &Value, target_id: u32, new_count: usize) -> Result<(), String> {
    for rig_id in rig_ids(model) {
        let Some(root) = nodes::find_node(model, rig_id) else {
            continue;
        };
        let Some(index) = find_binding_index(root, target_id)? else {
            continue;
        };
        let Some(rows) = root["bindings"][index].get("vertexWeights") else {
            continue;
        };
        let rows = rows.as_array().ok_or("vertexWeights must be an array")?;
        for row in rows {
            let row = row.as_array().ok_or("vertex weight row must be an array")?;
            if row.len() != new_count {
                return Err("vertexWeights length does not match the new vertex count".into());
            }
        }
    }
    Ok(())
}

pub(crate) fn preview_influence(
    model: &Value,
    root_id: u32,
    target_id: u32,
    bone_id: u32,
) -> Result<Value, String> {
    require_source_bone(model, root_id, bone_id)?;
    let listed = list_sources(model, root_id, target_id)?;
    let rule = get_rule(model, root_id, target_id)?;
    Ok(json!({
        "target": target_id,
        "bone": bone_id,
        "sourceBoneUuids": listed["sourceBoneUuids"].clone(),
        "rule": rule,
    }))
}

#[derive(Clone, Copy)]
struct SourceSetting {
    uuid: u32,
    weight: f64,
    depth_offset: f64,
    depth_scale: f64,
    rotation: f64,
}

struct Rule {
    max_influences: u32,
    radius_scale: f64,
    minimum_radius: f64,
    falloff: String,
    multipliers: BTreeMap<String, f64>,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            max_influences: 4,
            radius_scale: 1.0,
            minimum_radius: 1.0,
            falloff: "gaussian".into(),
            multipliers: BTreeMap::new(),
        }
    }
}

fn rig_ids(model: &Value) -> Vec<u32> {
    let mut ids = Vec::new();
    if let Some(root) = model.get("nodes") {
        collect_rig_ids(root, &mut ids);
    }
    ids
}

fn collect_rig_ids(node: &Value, ids: &mut Vec<u32>) {
    if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            ids.push(id);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_rig_ids(child, ids);
        }
    }
}

fn rewrite_deleted_bindings(root: &mut Value, removed: &std::collections::HashSet<u32>) -> Result<(), String> {
    let Some(bindings) = root.get_mut("bindings").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut kept = Vec::new();
    for mut binding in bindings.drain(..) {
        let target = binding.get("target").and_then(Value::as_u64).and_then(valid_id);
        if target.is_some_and(|id| removed.contains(&id)) {
            continue;
        }
        let mut sources = source_ids(&binding)?;
        let before = sources.len();
        sources.retain(|id| !removed.contains(id));
        if sources.len() != before {
            if binding.get("vertexWeights").is_some() {
                return Err(LOCKED_WEIGHTS.into());
            }
            if sources.is_empty() {
                continue;
            }
            set_sources(&mut binding, &sources)?;
            if let Some(rule) = binding.get_mut("influenceRule").and_then(Value::as_object_mut) {
                if let Some(multipliers) = rule.get_mut("multipliersByBoneUuid").and_then(Value::as_object_mut) {
                    multipliers.retain(|key, _| {
                        key.parse::<u64>()
                            .ok()
                            .and_then(valid_id)
                            .is_none_or(|id| !removed.contains(&id))
                    });
                }
            }
            normalize_binding(&mut binding)?;
        }
        kept.push(binding);
    }
    if kept.is_empty() {
        root.as_object_mut().ok_or("node must be an object")?.remove("bindings");
    } else {
        root.as_object_mut()
            .ok_or("node must be an object")?
            .insert("bindings".into(), Value::Array(kept));
    }
    Ok(())
}

fn rewrite_converted_target(root: &mut Value, id: u32, kind: Option<&str>) -> Result<(), String> {
    let Some(bindings) = root.get_mut("bindings").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut kept = Vec::new();
    for mut binding in bindings.drain(..) {
        let target = binding.get("target").and_then(Value::as_u64).and_then(valid_id);
        if target == Some(id) {
            let Some(kind) = kind else {
                continue;
            };
            binding
                .as_object_mut()
                .ok_or("binding must be an object")?
                .insert("targetKind".into(), json!(kind));
        }
        kept.push(binding);
    }
    if kept.is_empty() {
        root.as_object_mut().ok_or("node must be an object")?.remove("bindings");
    } else {
        root.as_object_mut()
            .ok_or("node must be an object")?
            .insert("bindings".into(), Value::Array(kept));
    }
    Ok(())
}

fn remap_binding(binding: &mut Value, uuid_map: &std::collections::HashMap<u32, u32>) -> Result<(), String> {
    let object = binding.as_object_mut().ok_or("binding must be an object")?;
    if let Some(target) = object.get("target").and_then(Value::as_u64).and_then(valid_id) {
        object.insert("target".into(), json!(mapped_id(target, uuid_map)));
    }
    if let Some(sources) = object.get("sourceBoneUuids").and_then(Value::as_array) {
        let remapped: Vec<u32> = sources
            .iter()
            .filter_map(|value| value.as_u64().and_then(valid_id))
            .map(|id| mapped_id(id, uuid_map))
            .collect();
        object.insert("sourceBoneUuids".into(), json!(remapped));
    }
    if let Some(settings) = object.get_mut("sourceSettings").and_then(Value::as_array_mut) {
        for setting in settings {
            if let Some(bone) = setting.get("bone").and_then(Value::as_u64).and_then(valid_id) {
                setting
                    .as_object_mut()
                    .ok_or("source setting must be an object")?
                    .insert("bone".into(), json!(mapped_id(bone, uuid_map)));
            }
        }
    }
    if let Some(rule) = object.get_mut("influenceRule").and_then(Value::as_object_mut) {
        if let Some(multipliers) = rule.get("multipliersByBoneUuid").and_then(Value::as_object) {
            let mut remapped = BTreeMap::new();
            for (key, value) in multipliers {
                let id = key.parse::<u64>().ok().and_then(valid_id).unwrap_or(0);
                let next = if id == 0 { key.clone() } else { mapped_id(id, uuid_map).to_string() };
                remapped.insert(next, value.clone());
            }
            rule.insert("multipliersByBoneUuid".into(), json!(remapped));
        }
    }
    Ok(())
}

fn mapped_id(id: u32, uuid_map: &std::collections::HashMap<u32, u32>) -> u32 {
    uuid_map.get(&id).copied().unwrap_or(id)
}

fn reject_weight_source_edit(model: &Value, root_id: u32, target_id: u32) -> Result<(), String> {
    let root = require_root(model, root_id)?;
    let Some(index) = find_binding_index(root, target_id)? else {
        return Ok(());
    };
    if root["bindings"][index].get("vertexWeights").is_some() {
        return Err(LOCKED_WEIGHTS.into());
    }
    Ok(())
}

fn reject_new_source_while_weighted(
    model: &Value,
    root_id: u32,
    target_id: u32,
    bone_id: u32,
) -> Result<(), String> {
    let root = require_root(model, root_id)?;
    let Some(index) = find_binding_index(root, target_id)? else {
        return Ok(());
    };
    let binding = &root["bindings"][index];
    if binding.get("vertexWeights").is_none() {
        return Ok(());
    }
    if source_ids(binding)?.contains(&bone_id) {
        return Ok(());
    }
    Err(LOCKED_WEIGHTS.into())
}

fn parse_weight_rows(weights: &Value, sources: &[u32], count: usize) -> Result<Vec<Vec<f64>>, String> {
    let items = weights.as_array().ok_or("vertex weights must be an array")?;
    let mut rows = vec![None; sources.len()];
    for item in items {
        let object = item.as_object().ok_or("vertex weight row must be an object")?;
        let bone = object
            .get("bone")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("vertex weight bone must be a UUID")?;
        let Some(index) = sources.iter().position(|id| *id == bone) else {
            return Err(format!("bone {bone} is not a source of this binding"));
        };
        if rows[index].is_some() {
            return Err(format!("vertex weights list bone {bone} more than once"));
        }
        let values = object
            .get("values")
            .and_then(Value::as_array)
            .ok_or("vertex weight row must be an array")?;
        if values.len() != count {
            return Err("vertex weight length must match the vertex count".into());
        }
        let mut parsed = Vec::with_capacity(count);
        for value in values {
            let weight = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or("vertex weight must be finite and >= 0")?;
            if weight < 0.0 {
                return Err("vertex weight must be finite and >= 0".into());
            }
            parsed.push(weight);
        }
        rows[index] = Some(parsed);
    }
    if rows.iter().any(Option::is_none) {
        return Err("vertex weights must list each source once".into());
    }
    let rows: Vec<Vec<f64>> = rows.into_iter().flatten().collect();
    for vertex in 0..count {
        let sum: f64 = rows.iter().map(|row| row[vertex]).sum();
        if rows.iter().all(|row| row[vertex] == 0.0) {
            return Err("vertex weights are all zero".into());
        }
        if (sum - 1.0).abs() > 1e-4 {
            return Err("vertex weights must sum to 1".into());
        }
    }
    Ok(rows)
}

fn require_root<'a>(model: &'a Value, root_id: u32) -> Result<&'a Value, String> {
    let root = nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    Ok(root)
}

fn require_root_mut(model: &mut Value, root_id: u32) -> Result<&mut Value, String> {
    let root = nodes::find_node_mut(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    Ok(root)
}

fn require_target(model: &Value, target_id: u32) -> Result<(&'static str, &Value), String> {
    let target = nodes::find_node(model, target_id).ok_or_else(|| format!("node {target_id} was not found"))?;
    let kind = match target.get("type").and_then(Value::as_str) {
        Some("GridDeformer") => "grid",
        Some("PathDeformer") => "path",
        Some("Part") => "part",
        _ => return Err("target must be GridDeformer, PathDeformer or Part".into()),
    };
    Ok((kind, target))
}

fn require_source_bone(model: &Value, root_id: u32, bone_id: u32) -> Result<(), String> {
    let bone = nodes::find_node(model, bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?;
    if bone.get("type").and_then(Value::as_str) != Some("DepthBone") {
        return Err("Node is not a DepthBone".into());
    }
    let root = require_root(model, root_id)?;
    if !contains_node(root, bone_id) {
        return Err("DepthBone source must belong to the selected DepthRigRoot".into());
    }
    Ok(())
}

fn contains_node(node: &Value, id: u32) -> bool {
    node.get("children")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                child.get("uuid").and_then(Value::as_u64).and_then(valid_id) == Some(id) || contains_node(child, id)
            })
        })
}

fn find_binding_index(root: &Value, target_id: u32) -> Result<Option<usize>, String> {
    let Some(bindings) = root.get("bindings") else {
        return Ok(None);
    };
    let bindings = bindings.as_array().ok_or("bindings must be an array")?;
    Ok(bindings.iter().position(|binding| {
        binding.get("target").and_then(Value::as_u64).and_then(valid_id) == Some(target_id)
    }))
}

fn binding_mut<'a>(
    root: &'a mut Value,
    target_id: u32,
    create_kind: Option<&str>,
) -> Result<&'a mut Value, String> {
    let object = root.as_object_mut().ok_or("node must be an object")?;
    let bindings = object.entry("bindings").or_insert_with(|| json!([]));
    let bindings = bindings.as_array_mut().ok_or("bindings must be an array")?;
    if let Some(index) = bindings.iter().position(|binding| {
        binding.get("target").and_then(Value::as_u64).and_then(valid_id) == Some(target_id)
    }) {
        return Ok(&mut bindings[index]);
    }
    let kind = create_kind.ok_or("binding was not found")?;
    bindings.push(json!({
        "target": target_id,
        "targetKind": kind,
        "sourceBoneUuids": [],
        "influenceRule": rule_stored(&Rule::default()),
    }));
    let index = bindings.len() - 1;
    Ok(&mut bindings[index])
}

fn remove_binding(root: &mut Value, target_id: u32) -> Result<(), String> {
    let object = root.as_object_mut().ok_or("node must be an object")?;
    let Some(bindings) = object.get_mut("bindings") else {
        return Ok(());
    };
    let bindings = bindings.as_array_mut().ok_or("bindings must be an array")?;
    bindings.retain(|binding| binding.get("target").and_then(Value::as_u64).and_then(valid_id) != Some(target_id));
    if bindings.is_empty() {
        object.remove("bindings");
    }
    Ok(())
}

fn source_ids(binding: &Value) -> Result<Vec<u32>, String> {
    let Some(raw) = binding.get("sourceBoneUuids") else {
        return Ok(Vec::new());
    };
    let raw = raw.as_array().ok_or("sourceBoneUuids must be an array")?;
    raw.iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(valid_id)
                .ok_or_else(|| "sourceBoneUuids must contain node UUIDs".to_string())
        })
        .collect()
}

fn push_source(binding: &mut Value, bone_id: u32) -> Result<(), String> {
    let sources = binding
        .as_object_mut()
        .ok_or("binding must be an object")?
        .entry("sourceBoneUuids")
        .or_insert_with(|| json!([]));
    sources
        .as_array_mut()
        .ok_or("sourceBoneUuids must be an array")?
        .push(json!(bone_id));
    Ok(())
}

fn set_sources(binding: &mut Value, sources: &[u32]) -> Result<(), String> {
    binding
        .as_object_mut()
        .ok_or("binding must be an object")?
        .insert("sourceBoneUuids".into(), json!(sources));
    Ok(())
}

fn binding_sources(binding: &Value) -> Result<Vec<SourceSetting>, String> {
    let ids = source_ids(binding)?;
    let stored = binding.get("sourceSettings").and_then(Value::as_array);
    let mut sources = Vec::with_capacity(ids.len());
    for id in ids {
        let found = stored.and_then(|entries| {
            entries.iter().find(|entry| entry.get("bone").and_then(Value::as_u64) == Some(u64::from(id)))
        });
        sources.push(read_source(id, found)?);
    }
    Ok(sources)
}

fn normalize_binding(binding: &mut Value) -> Result<(), String> {
    let sources = binding_sources(binding)?;
    let stored: Vec<Value> = sources.iter().map(source_stored).collect();
    let object = binding.as_object_mut().ok_or("binding must be an object")?;
    if stored.is_empty() {
        object.remove("sourceSettings");
    } else {
        object.insert("sourceSettings".into(), Value::Array(stored));
    }
    if let Some(rule) = object.get_mut("influenceRule") {
        let parsed = read_rule(Some(&*rule))?;
        *rule = rule_stored(&parsed);
    }
    Ok(())
}

fn read_source(uuid: u32, value: Option<&Value>) -> Result<SourceSetting, String> {
    let mut setting = SourceSetting {
        uuid,
        weight: 1.0,
        depth_offset: 0.0,
        depth_scale: 1.0,
        rotation: 0.0,
    };
    let Some(value) = value else {
        return Ok(setting);
    };
    if let Some(weight) = value.get("weight") {
        setting.weight = finite_or(weight, 1.0, "weight")?;
    }
    if let Some(offset) = value.get("depthOffset") {
        setting.depth_offset = finite_or(offset, 0.0, "depthOffset")?;
    }
    if let Some(scale) = value.get("depthScale") {
        setting.depth_scale = finite_or(scale, 1.0, "depthScale")?;
    }
    if let Some(rotation) = value.get("rotation") {
        setting.rotation = f64::from(normalize_rotation(number_or_nan(rotation)));
    }
    clamp_source(&mut setting);
    Ok(setting)
}

fn apply_settings(setting: &mut SourceSetting, value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("settings must be an object")?;
    if let Some(weight) = object.get("weight") {
        setting.weight = finite_or(weight, 1.0, "weight")?;
    }
    if let Some(offset) = object.get("depthOffset") {
        setting.depth_offset = finite_or(offset, 0.0, "depthOffset")?;
    }
    if let Some(scale) = object.get("depthScale") {
        setting.depth_scale = finite_or(scale, 1.0, "depthScale")?;
    }
    if let Some(rotation) = object.get("rotation") {
        setting.rotation = f64::from(normalize_rotation(number_or_nan(rotation)));
    }
    clamp_source(setting);
    Ok(())
}

fn clamp_source(setting: &mut SourceSetting) {
    if setting.weight < 0.0 {
        setting.weight = 0.0;
    }
    if setting.depth_scale < 0.01 {
        setting.depth_scale = 0.01;
    }
}

fn source_stored(setting: &SourceSetting) -> Value {
    json!({
        "bone": setting.uuid,
        "weight": setting.weight,
        "depthOffset": setting.depth_offset,
        "depthScale": setting.depth_scale,
        "rotation": setting.rotation,
    })
}

fn source_json(setting: &SourceSetting) -> Value {
    json!({
        "uuid": setting.uuid,
        "weight": setting.weight,
        "depthOffset": setting.depth_offset,
        "depthScale": setting.depth_scale,
        "rotation": setting.rotation,
    })
}

fn read_rule(value: Option<&Value>) -> Result<Rule, String> {
    let mut rule = Rule::default();
    let Some(value) = value else {
        return Ok(rule);
    };
    if let Some(max) = value.get("maxInfluences") {
        rule.max_influences = influence_count(max)?;
    }
    if let Some(scale) = value.get("radiusScale") {
        rule.radius_scale = finite_or(scale, 1.0, "radiusScale")?;
    }
    if let Some(radius) = value.get("minimumRadius") {
        rule.minimum_radius = finite_or(radius, 1.0, "minimumRadius")?;
    }
    if let Some(falloff) = value.get("falloff") {
        rule.falloff = falloff.as_str().ok_or("falloff must be a string")?.to_owned();
    }
    if let Some(raw) = value.get("multipliersByBoneUuid") {
        let raw = raw.as_object().ok_or("multipliersByBoneUuid must be an object")?;
        for (key, multiplier) in raw {
            let id = key
                .parse::<u64>()
                .ok()
                .and_then(valid_id)
                .ok_or_else(|| format!("multipliersByBoneUuid.{key} must be a node UUID"))?;
            rule.multipliers
                .insert(id.to_string(), finite_or(multiplier, f64::NAN, &format!("multipliersByBoneUuid.{key}"))?);
        }
    }
    if rule.max_influences == 0 {
        rule.max_influences = 1;
    }
    Ok(rule)
}

fn apply_rule(rule: &mut Rule, value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("rule must be an object")?;
    if let Some(max) = object.get("maxInfluences") {
        rule.max_influences = influence_count(max)?;
    }
    if let Some(scale) = object.get("radiusScale") {
        rule.radius_scale = finite_or(scale, 1.0, "radiusScale")?;
    }
    if let Some(radius) = object.get("minimumRadius") {
        rule.minimum_radius = finite_or(radius, 1.0, "minimumRadius")?;
    }
    if let Some(falloff) = object.get("falloff") {
        rule.falloff = falloff.as_str().ok_or("falloff must be a string")?.to_owned();
    }
    if let Some(raw) = object.get("multipliersByBoneUuid") {
        rule.multipliers.clear();
        let raw = raw.as_object().ok_or("multipliersByBoneUuid must be an object")?;
        for (key, multiplier) in raw {
            let id = key
                .parse::<u64>()
                .ok()
                .and_then(valid_id)
                .ok_or_else(|| format!("multipliersByBoneUuid.{key} must be a node UUID"))?;
            rule.multipliers
                .insert(id.to_string(), finite_or(multiplier, f64::NAN, &format!("multipliersByBoneUuid.{key}"))?);
        }
    }
    if rule.max_influences == 0 {
        rule.max_influences = 1;
    }
    Ok(())
}

fn rule_stored(rule: &Rule) -> Value {
    let mut object = Map::new();
    object.insert("maxInfluences".into(), json!(rule.max_influences));
    object.insert("radiusScale".into(), json!(rule.radius_scale));
    object.insert("minimumRadius".into(), json!(rule.minimum_radius));
    object.insert("falloff".into(), json!(rule.falloff));
    if !rule.multipliers.is_empty() {
        object.insert("multipliersByBoneUuid".into(), json!(rule.multipliers));
    }
    Value::Object(object)
}

fn rule_json(rule: &Rule) -> Value {
    json!({
        "maxInfluences": rule.max_influences,
        "radiusScale": rule.radius_scale,
        "minimumRadius": rule.minimum_radius,
        "falloff": rule.falloff,
        "multipliersByBoneUuid": rule.multipliers,
    })
}

fn influence_count(value: &Value) -> Result<u32, String> {
    let number = value.as_u64().ok_or("maxInfluences must be an unsigned integer")?;
    let number = u32::try_from(number).map_err(|_| "maxInfluences is outside the u32 range".to_string())?;
    Ok(if number == 0 { 1 } else { number })
}

fn finite_or(value: &Value, fallback: f64, name: &str) -> Result<f64, String> {
    let number = number_or_nan(value);
    if number.is_nan() && value.is_number() {
        return Ok(fallback);
    }
    if !number.is_finite() {
        if value.is_number() {
            return Ok(fallback);
        }
        return Err(format!("{name} must be numeric"));
    }
    Ok(f64::from(number))
}

fn number_or_nan(value: &Value) -> f32 {
    value.as_f64().map(|number| number as f32).unwrap_or(f32::NAN)
}

fn normalize_rotation(rotation: f32) -> f32 {
    if !rotation.is_finite() {
        return 0.0;
    }
    let pi = std::f32::consts::PI;
    let two_pi = pi * 2.0;
    let mut normalized = (rotation + pi) % two_pi;
    if normalized < 0.0 {
        normalized += two_pi;
    }
    normalized - pi
}

fn parse_json_value(value: &Value, name: &str) -> Result<Value, String> {
    match value {
        Value::String(text) => serde_json::from_str(text).map_err(|_| format!("{name} must be JSON")),
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_wraps_like_the_d_float_helper() {
        let pi = std::f32::consts::PI;
        assert!((normalize_rotation(0.0)).abs() < 1e-6);
        assert!((normalize_rotation(pi + 0.25) - (-pi + 0.25)).abs() < 1e-5);
        assert!((normalize_rotation(-pi - 0.5) - (pi - 0.5)).abs() < 1e-5);
        assert_eq!(normalize_rotation(f32::NAN), 0.0);
    }
}
