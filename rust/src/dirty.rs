//! One-shot Depth Bone dirty queue.
//!
//! A request marks `Keypoint` or `AllKeypoints` for a rig and parameter, then
//! flushes once before the model is committed. `AllKeypoints` absorbs
//! `Keypoint` for the same rig and parameter. An empty queue does not
//! evaluate. Flush failure is returned to the caller so the causing edit is
//! not committed.
//!
//! There is no frame loop and no GPU. An all-keypoints flush writes whole
//! cells up to `ALL_KEYPOINTS_BUDGET` samples. A cell is one target at one
//! key. A cell larger than that budget, and at most `bone_deform::MAX_VERTICES`,
//! is assembled in vertex chunks on the session. The document receives that
//! cell only when every vertex has an offset. A larger cell fails before any
//! write. The rest of the cells stay on the session cursor for
//! `deform.continue` or `save`.

use crate::bone_deform::{self, DeformOutcome};
use crate::nodes;
use crate::valid_id;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Samples in one ordinary flush. A cell is one target at one key, so its
/// sample count is that target's vertex count. D spreads `AllKeypoints` across
/// frames (`DepthBoneAllKeypointsPerFrame`). Rust fills whole cells up to this
/// cap and continues on a later request. 262144 is a 256×256 key grid times a
/// 2×2 target, and 256 is the maximum axis length.
pub(crate) const ALL_KEYPOINTS_BUDGET: u64 = 262_144;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub root: u32,
    pub parameter: u32,
    pub keypoint: [usize; 2],
    pub target: u32,
}

impl Cell {
    /// What one call of the rig is for: the cells that share it are written together.
    fn key(&self) -> (u32, u32, [usize; 2]) {
        (self.root, self.parameter, self.keypoint)
    }
}

pub(crate) struct Flush {
    pub model: Value,
    pub preview: BTreeMap<u32, Vec<[f64; 2]>>,
    pub pending: Vec<Cell>,
    /// Offsets already computed for `pending[0]` and not yet written. Empty
    /// when the head cell has not started. This is session memory: a prefix
    /// is never stored on the model.
    pub partial: Vec<[f64; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Keypoint,
    AllKeypoints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    pub root: u32,
    pub parameter: Option<u32>,
    pub keypoint: [usize; 2],
    pub target: Option<u32>,
    pub scope: Scope,
}

pub(crate) fn mark_bone(model: &Value, bone: u32) -> Option<Mark> {
    ancestor_rig(model, bone).map(mark_root)
}

/// The node itself is a Grid, Path or Part target of a driven rig. Convert may drop
/// that row and refresh the rig. An ancestor of a target is not enough: its
/// own `"deform"` row would not be rewritten.
pub(crate) fn refreshes_bound_target(model: &Value, target: u32) -> bool {
    rig_ids(model).into_iter().any(|root| {
        binding_targets(model, root).contains(&target)
            && driving_parameters(model, root).is_ok_and(|parameters| !parameters.is_empty())
    })
}

/// Rig roots whose binding table or bone set changed. Delete and convert rewrite
/// the table before this mark, so the following flush does not pack a target
/// that is no longer a binding target.
pub(crate) fn marks_for_structure(before: &Value, after: &Value) -> Vec<Mark> {
    let mut roots = BTreeSet::new();
    roots.extend(rig_ids(before));
    roots.extend(rig_ids(after));
    let mut marks = Vec::new();
    for root in roots {
        if nodes::find_node(after, root).is_none() {
            continue;
        }
        if bindings_json(before, root) != bindings_json(after, root)
            || bone_ids(before, root) != bone_ids(after, root)
        {
            marks.push(mark_root(root));
        }
    }
    marks
}

fn bindings_json(model: &Value, root: u32) -> Value {
    nodes::find_node(model, root)
        .and_then(|node| node.get("bindings"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn bone_ids(model: &Value, root: u32) -> BTreeSet<u32> {
    bone_deform::bone_order(model, root)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub(crate) fn mark_root(root: u32) -> Mark {
    Mark {
        root,
        parameter: None,
        keypoint: [0, 0],
        target: None,
        scope: Scope::AllKeypoints,
    }
}

pub(crate) fn mark_target(model: &Value, target: u32) -> Vec<Mark> {
    let mut marks = Vec::new();
    for root in rig_ids(model) {
        if affects_root(model, root, target) {
            marks.push(Mark {
                root,
                parameter: None,
                keypoint: [0, 0],
                target: Some(target),
                scope: Scope::AllKeypoints,
            });
        }
    }
    marks
}

pub(crate) fn mark_pose(
    model: &Value,
    parameter: u32,
    nodes: &[u32],
    keypoint: [usize; 2],
) -> Vec<Mark> {
    let mut marks = Vec::new();
    for node_id in nodes {
        let Some(node) = nodes::find_node(model, *node_id) else {
            continue;
        };
        if node.get("type").and_then(Value::as_str) != Some("DepthBone") {
            continue;
        }
        let Some(root) = ancestor_rig(model, *node_id) else {
            continue;
        };
        if binding_targets(model, root).is_empty() {
            continue;
        }
        marks.push(Mark {
            root,
            parameter: Some(parameter),
            keypoint,
            target: None,
            scope: Scope::Keypoint,
        });
    }
    marks
}

pub(crate) fn flush(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    marks: &[Mark],
) -> Result<Flush, String> {
    let marks = absorb(marks);
    let mut candidate = model.clone();
    let mut preview = BTreeMap::new();
    if marks.is_empty() {
        return Ok(Flush {
            model: candidate,
            preview,
            pending: Vec::new(),
            partial: Vec::new(),
        });
    }
    preview_undriven(&mut preview, &candidate, values, &marks)?;
    let cells = plan_cells(&candidate, &marks)?;
    write_cells(
        &mut candidate,
        values,
        &mut preview,
        &cells,
        Vec::new(),
        Some(ALL_KEYPOINTS_BUDGET),
    )
}

pub(crate) fn continue_cells(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    cells: &[Cell],
    partial: Vec<[f64; 2]>,
    limit: Option<u64>,
) -> Result<Flush, String> {
    if cells.is_empty() {
        return Ok(Flush {
            model: model.clone(),
            preview: BTreeMap::new(),
            pending: Vec::new(),
            partial: Vec::new(),
        });
    }
    for cell in cells {
        reject_cell_size(model, cell.target)?;
    }
    if !partial.is_empty() {
        let vertices = vertex_count(
            nodes::find_node(model, cells[0].target)
                .ok_or_else(|| format!("node {} was not found", cells[0].target))?,
        )?;
        if partial.len() as u64 >= vertices {
            return Err("deform refresh cursor is corrupt".into());
        }
    }
    let mut candidate = model.clone();
    let mut preview = BTreeMap::new();
    write_cells(&mut candidate, values, &mut preview, cells, partial, limit)
}

pub(crate) fn preview_pose(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    parameter: u32,
) -> Result<BTreeMap<u32, Vec<[f64; 2]>>, String> {
    let mut preview = BTreeMap::new();
    for root in rig_ids(model) {
        if !bone_deform::parameter_drives_rig(model, root, parameter)? {
            continue;
        }
        if binding_targets(model, root).is_empty() {
            continue;
        }
        let key = bone_deform::closest_keypoint(model, values, parameter)?;
        let outcome = bone_deform::run_with_key(
            model,
            values,
            root,
            &[],
            Some(parameter),
            false,
            &BTreeMap::new(),
            Some(key),
            false,
        )?;
        merge_preview(&mut preview, &outcome);
    }
    Ok(preview)
}

fn plan_cells(model: &Value, marks: &[Mark]) -> Result<Vec<Cell>, String> {
    let mut cells = Vec::new();
    for mark in marks {
        let targets = scoped_targets(model, mark.root, mark.target)?;
        if targets.is_empty() {
            continue;
        }
        let parameters = mark_parameters(model, mark)?;
        if parameters.is_empty() {
            continue;
        }
        for parameter in parameters {
            let keys = match mark.scope {
                Scope::Keypoint => vec![mark.keypoint],
                Scope::AllKeypoints => all_keypoints(model, parameter)?,
            };
            for key in keys {
                let mut batch = Vec::new();
                for target in &targets {
                    reject_cell_size(model, *target)?;
                    let cell = Cell {
                        root: mark.root,
                        parameter,
                        keypoint: key,
                        target: *target,
                    };
                    batch.push((ancestor_depth(model, *target), cell));
                }
                // A child cell reads ancestor offsets already stored for this
                // key. Ancestors must be written first when cells are separate.
                batch.sort_by_key(|(depth, _)| *depth);
                cells.extend(batch.into_iter().map(|(_, cell)| cell));
            }
        }
    }
    Ok(cells)
}

fn ancestor_depth(model: &Value, id: u32) -> usize {
    let mut depth = 0usize;
    let mut cursor = Some(id);
    while let Some(current) = cursor {
        depth += 1;
        cursor = bone_deform::parent_of(model, current).ok().flatten();
        if depth > 4096 {
            break;
        }
    }
    depth
}

fn reject_cell_size(model: &Value, target: u32) -> Result<(), String> {
    let node = nodes::find_node(model, target).ok_or_else(|| format!("node {target} was not found"))?;
    let vertices = vertex_count(node)?;
    if vertices > bone_deform::MAX_VERTICES as u64 {
        return Err(format!(
            "all-keypoints refresh of {vertices} vertices exceeds the maximum of {}",
            bone_deform::MAX_VERTICES
        ));
    }
    Ok(())
}

fn mark_parameters(model: &Value, mark: &Mark) -> Result<Vec<u32>, String> {
    match mark.parameter {
        Some(parameter) => {
            if bone_deform::parameter_drives_rig(model, mark.root, parameter)? {
                Ok(vec![parameter])
            } else {
                Ok(Vec::new())
            }
        }
        None => driving_parameters(model, mark.root),
    }
}

fn preview_undriven(
    preview: &mut BTreeMap<u32, Vec<[f64; 2]>>,
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    marks: &[Mark],
) -> Result<(), String> {
    for mark in marks {
        let targets = scoped_targets(model, mark.root, mark.target)?;
        if targets.is_empty() || !mark_parameters(model, mark)?.is_empty() {
            continue;
        }
        let outcome = bone_deform::run_with_key(
            model,
            values,
            mark.root,
            &targets,
            None,
            false,
            &BTreeMap::new(),
            None,
            false,
        )?;
        merge_preview(preview, &outcome);
    }
    Ok(())
}

fn write_cells(
    model: &mut Value,
    values: &BTreeMap<u32, [f64; 2]>,
    preview: &mut BTreeMap<u32, Vec<[f64; 2]>>,
    cells: &[Cell],
    mut partial: Vec<[f64; 2]>,
    limit: Option<u64>,
) -> Result<Flush, String> {
    let mut used = 0u64;
    let mut index = 0usize;
    while index < cells.len() {
        let cell = &cells[index];
        let vertices = vertex_count(
            nodes::find_node(model, cell.target)
                .ok_or_else(|| format!("node {} was not found", cell.target))?,
        )?;
        let start = if index == 0 { partial.len() } else { 0 };
        if start as u64 > vertices {
            return Err("deform refresh cursor is corrupt".into());
        }
        if vertices <= ALL_KEYPOINTS_BUDGET {
            if start != 0 {
                return Err("deform refresh cursor is corrupt".into());
            }
            // The cells of one key that fit the call are written together. The rig copies the document for a call
            // and not for a target.
            let mut end = index;
            let mut next = used;
            while let Some(more) = cells.get(end).filter(|more| more.key() == cell.key()) {
                let vertices = vertex_count(
                    nodes::find_node(model, more.target)
                        .ok_or_else(|| format!("node {} was not found", more.target))?,
                )?;
                let total = next.saturating_add(vertices);
                if vertices > ALL_KEYPOINTS_BUDGET || limit.is_some_and(|limit| total > limit) {
                    break;
                }
                next = total;
                end += 1;
            }
            if end == index {
                break;
            }
            write_whole_cells(model, values, preview, &cells[index..end])?;
            used = next;
            index = end;
            continue;
        }
        let remaining = vertices - start as u64;
        let room = match limit {
            Some(limit) => limit.saturating_sub(used),
            None => remaining,
        };
        if room == 0 {
            break;
        }
        let take = usize::try_from(room.min(remaining)).map_err(|_| "offset length mismatch")?;
        let end = start + take;
        let chunk = bone_deform::cell_offset_range(
            model,
            values,
            cell.root,
            cell.target,
            cell.parameter,
            cell.keypoint,
            &BTreeMap::new(),
            start,
            end,
        )?;
        if chunk.len() != take {
            return Err("offset length mismatch".into());
        }
        if index == 0 {
            partial.extend(chunk);
        } else {
            partial = chunk;
        }
        used = used.saturating_add(take as u64);
        if partial.len() as u64 == vertices {
            let offsets = std::mem::take(&mut partial);
            write_assembled_cell(model, values, preview, cell, &offsets)?;
            index += 1;
            if limit.is_some_and(|limit| used >= limit) {
                break;
            }
        } else {
            break;
        }
    }
    if index >= cells.len() {
        partial.clear();
    }
    Ok(Flush {
        model: model.clone(),
        preview: preview.clone(),
        pending: cells[index..].to_vec(),
        partial,
    })
}

/// Writes cells that share one root, parameter and key. `cells` is not empty.
fn write_whole_cells(
    model: &mut Value,
    values: &BTreeMap<u32, [f64; 2]>,
    preview: &mut BTreeMap<u32, Vec<[f64; 2]>>,
    cells: &[Cell],
) -> Result<(), String> {
    let cell = &cells[0];
    let targets: Vec<u32> = cells.iter().map(|cell| cell.target).collect();
    let visual = bone_deform::closest_keypoint(model, values, cell.parameter)?;
    let outcome = bone_deform::run_with_key(
        model,
        values,
        cell.root,
        &targets,
        Some(cell.parameter),
        true,
        &BTreeMap::new(),
        Some(cell.keypoint),
        true,
    )?;
    if cell.keypoint == visual {
        merge_preview(preview, &outcome);
    }
    *model = outcome
        .model
        .ok_or_else(|| "dirty refresh did not write a model".to_string())?;
    Ok(())
}

fn write_assembled_cell(
    model: &mut Value,
    values: &BTreeMap<u32, [f64; 2]>,
    preview: &mut BTreeMap<u32, Vec<[f64; 2]>>,
    cell: &Cell,
    offsets: &[[f64; 2]],
) -> Result<(), String> {
    let visual = bone_deform::closest_keypoint(model, values, cell.parameter)?;
    let outcome = bone_deform::commit_cell_offsets(
        model,
        values,
        cell.root,
        cell.target,
        cell.parameter,
        cell.keypoint,
        offsets,
        &BTreeMap::new(),
    )?;
    if cell.keypoint == visual {
        merge_preview(preview, &outcome);
    }
    *model = outcome
        .model
        .ok_or_else(|| "dirty refresh did not write a model".to_string())?;
    Ok(())
}

fn vertex_count(node: &Value) -> Result<u64, String> {
    match node.get("type").and_then(Value::as_str) {
        Some("GridDeformer") => {
            let xs = node
                .get("grid_axis_x")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let ys = node
                .get("grid_axis_y")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if xs < 2 || ys < 2 {
                return Err("target is not deformable".into());
            }
            (xs as u64).checked_mul(ys as u64).ok_or_else(|| {
                format!(
                    "all-keypoints refresh of vertices exceeds the maximum of {}",
                    bone_deform::MAX_VERTICES
                )
            })
        }
        Some(kind @ ("PathDeformer" | "Part")) => {
            let mesh = node.get("mesh").and_then(|mesh| mesh.get("verts"));
            let raw = if kind == "Part" { mesh } else { node.get("vertices") };
            let raw = raw.and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            if raw % 2 != 0 {
                return Err("offset length mismatch".into());
            }
            Ok((raw / 2) as u64)
        }
        _ => Err("target is not deformable".into()),
    }
}

fn absorb(marks: &[Mark]) -> Vec<Mark> {
    let mut kept: Vec<Mark> = Vec::new();
    for mark in marks {
        if let Some(existing) = kept.iter_mut().find(|existing| {
            existing.root == mark.root
                && existing.parameter == mark.parameter
                && (existing.target == mark.target
                    || existing.target.is_none()
                    || mark.target.is_none())
        }) {
            if existing.scope == Scope::AllKeypoints || mark.scope == Scope::AllKeypoints {
                existing.scope = Scope::AllKeypoints;
                if existing.target != mark.target {
                    existing.target = None;
                }
            } else if existing.keypoint != mark.keypoint {
                kept.push(*mark);
            }
            continue;
        }
        kept.push(*mark);
    }
    let roots_all: BTreeSet<u32> = kept
        .iter()
        .filter(|mark| mark.parameter.is_none() && mark.scope == Scope::AllKeypoints)
        .map(|mark| mark.root)
        .collect();
    kept.retain(|mark| {
        mark.parameter.is_none()
            || !roots_all.contains(&mark.root)
            || mark.scope == Scope::AllKeypoints
    });
    let mut collapsed = Vec::new();
    for mark in kept {
        if mark.parameter.is_some()
            && collapsed.iter().any(|existing: &Mark| {
                existing.root == mark.root
                    && existing.parameter == mark.parameter
                    && existing.scope == Scope::AllKeypoints
            })
        {
            continue;
        }
        if let Some(existing) = collapsed.iter_mut().find(|existing| {
            existing.root == mark.root
                && existing.parameter == mark.parameter
                && existing.scope == mark.scope
                && existing.keypoint == mark.keypoint
                && (existing.target == mark.target
                    || existing.target.is_none()
                    || mark.target.is_none())
        }) {
            if existing.target != mark.target {
                existing.target = None;
            }
            continue;
        }
        collapsed.push(mark);
    }
    collapsed
}

fn all_keypoints(model: &Value, parameter: u32) -> Result<Vec<[usize; 2]>, String> {
    let parameter = find_parameter(model, parameter)
        .ok_or_else(|| format!("parameter {parameter} was not found"))?;
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or("parameter.axis_points must contain two arrays")?;
    let x_count = axes
        .first()
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let y_count = if is_vec2 {
        axes.get(1)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    } else {
        1
    };
    if x_count == 0 || y_count == 0 {
        return Err("missing \"deform\" binding support".into());
    }
    let mut keys = Vec::with_capacity(x_count * y_count);
    for x in 0..x_count {
        for y in 0..y_count {
            keys.push([x, y]);
        }
    }
    Ok(keys)
}

fn driving_parameters(model: &Value, root: u32) -> Result<Vec<u32>, String> {
    let mut ids = Vec::new();
    for parameter in parameter_ids(model) {
        if bone_deform::parameter_drives_rig(model, root, parameter)? {
            ids.push(parameter);
        }
    }
    Ok(ids)
}

fn scoped_targets(model: &Value, root: u32, target: Option<u32>) -> Result<Vec<u32>, String> {
    let targets = binding_targets(model, root);
    let Some(target) = target else {
        return Ok(targets);
    };
    let mut scoped = Vec::new();
    for id in targets {
        if id == target || is_descendant(model, target, id)? {
            scoped.push(id);
        }
    }
    Ok(scoped)
}

fn binding_targets(model: &Value, root: u32) -> Vec<u32> {
    let Some(root) = nodes::find_node(model, root) else {
        return Vec::new();
    };
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for binding in bindings {
        let Some(id) = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(node) = nodes::find_node(model, id) else {
            continue;
        };
        if bone_deform::is_binding_target(node) {
            ids.push(id);
        }
    }
    ids
}

fn affects_root(model: &Value, root: u32, node: u32) -> bool {
    if root == node {
        return !binding_targets(model, root).is_empty();
    }
    binding_targets(model, root)
        .into_iter()
        .any(|target| target == node || is_descendant(model, node, target).unwrap_or(false))
}

fn is_descendant(model: &Value, ancestor: u32, node: u32) -> Result<bool, String> {
    let mut current = bone_deform::parent_of(model, node)?;
    while let Some(id) = current {
        if id == ancestor {
            return Ok(true);
        }
        current = bone_deform::parent_of(model, id)?;
    }
    Ok(false)
}

fn ancestor_rig(model: &Value, node: u32) -> Option<u32> {
    let mut current = Some(node);
    while let Some(id) = current {
        let kind = nodes::find_node(model, id)
            .and_then(|node| node.get("type"))
            .and_then(Value::as_str);
        if kind == Some("DepthRigRoot") {
            return Some(id);
        }
        current = bone_deform::parent_of(model, id).ok().flatten();
    }
    None
}

fn rig_ids(model: &Value) -> Vec<u32> {
    let mut ids = Vec::new();
    if let Some(root) = model.get("nodes") {
        collect_rigs(root, &mut ids);
    }
    ids
}

fn collect_rigs(node: &Value, ids: &mut Vec<u32>) {
    if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            ids.push(id);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_rigs(child, ids);
        }
    }
}

fn parameter_ids(model: &Value) -> Vec<u32> {
    let mut ids = Vec::new();
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        collect_parameters(list, &mut ids);
    }
    ids
}

fn collect_parameters(list: &[Value], ids: &mut Vec<u32>) {
    for item in list {
        if item.get("groupUUID").is_some() {
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                collect_parameters(children, ids);
            }
            continue;
        }
        if let Some(id) = item.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            ids.push(id);
        }
    }
}

fn find_parameter(model: &Value, id: u32) -> Option<&Value> {
    fn walk(list: &[Value], id: u32) -> Option<&Value> {
        for item in list {
            if item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
                && item.get("groupUUID").is_none()
            {
                return Some(item);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                if let Some(found) = walk(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .and_then(|list| walk(list, id))
}

fn merge_preview(preview: &mut BTreeMap<u32, Vec<[f64; 2]>>, outcome: &DeformOutcome) {
    let Some(targets) = outcome.body.get("targets").and_then(Value::as_array) else {
        return;
    };
    for target in targets {
        let Some(id) = target
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(offsets) = target.get("offsets").and_then(Value::as_array) else {
            continue;
        };
        let mut stored = Vec::with_capacity(offsets.len());
        for offset in offsets {
            let Some(pair) = offset.as_array() else {
                return;
            };
            if pair.len() != 2 {
                return;
            }
            let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                return;
            };
            stored.push([x, y]);
        }
        preview.insert(id, stored);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_keypoints_absorbs_a_keypoint_and_duplicate_marks_collapse() {
        let marks = [
            Mark {
                root: 2,
                parameter: Some(9),
                keypoint: [1, 0],
                target: Some(4),
                scope: Scope::Keypoint,
            },
            Mark {
                root: 2,
                parameter: Some(9),
                keypoint: [1, 0],
                target: Some(4),
                scope: Scope::Keypoint,
            },
            Mark {
                root: 2,
                parameter: None,
                keypoint: [0, 0],
                target: Some(4),
                scope: Scope::AllKeypoints,
            },
        ];
        let absorbed = absorb(&marks);
        assert_eq!(absorbed.len(), 1);
        assert_eq!(absorbed[0].scope, Scope::AllKeypoints);
        assert_eq!(absorbed[0].parameter, None);
        let siblings = [
            Mark {
                root: 2,
                parameter: None,
                keypoint: [0, 0],
                target: Some(4),
                scope: Scope::AllKeypoints,
            },
            Mark {
                root: 2,
                parameter: None,
                keypoint: [0, 0],
                target: Some(5),
                scope: Scope::AllKeypoints,
            },
        ];
        let scoped = absorb(&siblings);
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].target, Some(4));
        assert_eq!(scoped[1].target, Some(5));
        assert!(flush(
            &serde_json::json!({"nodes": {"uuid": 1, "type": "Node", "children": []}, "param": []}),
            &BTreeMap::new(),
            &[],
        )
        .unwrap()
        .preview
        .is_empty());
    }

    fn call(session: &mut crate::Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn pose_edit_updates_one_key_and_depth_edit_updates_every_key_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[0.0,0.0,0.0,0.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":1.5707963267948966
            }),
        ));
        let posed = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let deform = posed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform")
            .unwrap();
        assert_eq!(deform["isSet"][2][0], true);
        assert_eq!(deform["isSet"][0][0], false);
        assert_eq!(deform["isSet"][1][0], false);
        let posed_model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[1.0,1.0,1.0,1.0]}),
        ));
        let refreshed = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let deform = refreshed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform")
            .unwrap();
        assert_eq!(deform["isSet"][0][0], true);
        assert_eq!(deform["isSet"][1][0], true);
        assert_eq!(deform["isSet"][2][0], true);
        assert_ne!(deform["values"][0][0], deform["values"][2][0]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        let restored = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(restored["model"], posed_model);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})))["depths"],
            json!([0.0, 0.0, 0.0, 0.0])
        );
        let revision = restored["revision"].clone();
        ok(&call(
            &mut session,
            &json!({"op":"value.set","id":parameter_id,"value":[1.0,0.0]}),
        ));
        let after_pose = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(after_pose["model"], restored["model"]);
        assert_eq!(after_pose["revision"], revision);
        assert_eq!(after_pose["canUndo"], restored["canUndo"]);
    }

    #[test]
    fn vertex_edit_rewrites_deform_keys_instead_of_rolling_back() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":1.5707963267948966
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,0.5,1.0],"axisY":[0.0,1.0]}),
        ));
        let grid_node = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(grid_node["grid_axis_x"], json!([0.0, 0.5, 1.0]));
        let refreshed = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let deform = refreshed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform")
            .unwrap();
        for index in 0..3 {
            assert_eq!(deform["isSet"][index][0], true);
            assert_eq!(deform["values"][index][0].as_array().unwrap().len(), 6);
        }
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before
        );
    }

    fn unit_axis(count: usize) -> Vec<f64> {
        (0..count)
            .map(|index| index as f64 / (count - 1) as f64)
            .collect()
    }

    #[test]
    fn two_by_two_refresh_writes_every_cell_and_previews_the_armed_key() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Pose","dimensions":2,
                "min":[0.0,0.0],"max":[1.0,1.0]
            }),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"value.set","id":parameter_id,"value":[1.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,1],"rotation_y":1.5707963267948966
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[1.0,1.0,1.0,1.0]}),
        ));
        let stored = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let deform = stored["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform")
            .unwrap();
        for x in 0..2 {
            for y in 0..2 {
                assert_eq!(deform["isSet"][x][y], true);
                assert_eq!(deform["values"][x][y].as_array().unwrap().len(), 4);
            }
        }
        assert_ne!(deform["values"][0][0], deform["values"][1][1]);
        let snapshot = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(
            snapshot["previewDeformation"][&grid_id.to_string()],
            deform["values"][1][1]
        );
        let armed =
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})))["deformation"].clone();
        assert_eq!(armed, deform["values"][1][1]);
        let revision = snapshot["revision"].clone();
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_ne!(undone["revision"], revision);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id}))).get("depths"),
            None
        );
    }

    #[test]
    fn over_budget_all_keypoints_leaves_revision_unchanged() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Pose","dimensions":2,
                "min":[0.0,0.0],"max":[1.0,1.0]
            }),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,1],"rotation_y":0.2
            }),
        ));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let failed = call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[grid_id],
                "axisX": unit_axis(1000), "axisY": unit_axis(1001)
            }),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"]
            .as_str()
            .unwrap()
            .contains("exceeds the maximum of 1000000"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn the_cells_of_one_key_written_together_match_the_cells_written_one_by_one() {
        let mut session = crate::Session::new();
        let create = |session: &mut crate::Session, request: Value| created_id(&ok(&call(session, &request)));
        let rig_id = create(&mut session, json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}));
        let bone_id = create(&mut session, json!({
            "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
            "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
        }));
        // The child is listed first and lies in the parent, and the third grid lies beside them.
        let child_id = create(&mut session, json!({"op":"node.create","className":"GridDeformer","name":"Child"}));
        let parent_id = create(&mut session, json!({"op":"node.create","className":"GridDeformer","name":"Parent"}));
        let other_id = create(&mut session, json!({"op":"node.create","className":"GridDeformer","name":"Other"}));
        ok(&call(&mut session, &json!({"op":"node.move","nodes":[child_id],"parent":parent_id})));
        for id in [child_id, parent_id, other_id] {
            ok(&call(&mut session, &json!({
                "op":"deformer.define_grid","nodes":[id],"axisX":[0.0,10.0],"axisY":[0.0,20.0]
            })));
            ok(&call(&mut session, &json!({"op":"rig.add_source","root":rig_id,"target":id,"bone":bone_id})));
        }
        let parameter_id = ok(&call(&mut session, &json!({
            "op":"parameter.create","name":"Yaw","dimensions":1,"min":[0.0,0.0],"max":[1.0,1.0]
        })))["uuid"].as_u64().unwrap();
        for (key, turn) in [(0, -0.3), (1, 0.4)] {
            ok(&call(&mut session, &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[key,0],"rotation_y":turn
            })));
        }
        let snapshot = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let values = runtime_values(&snapshot["values"]);
        // Without the keys the session stored, a child finds the offsets of its parent only when the parent is
        // written before it.
        let mut bare = snapshot["model"].clone();
        for parameter in bare["param"].as_array_mut().unwrap() {
            parameter["bindings"].as_array_mut().unwrap().retain(|binding| binding["param_name"] != "deform");
        }
        let cells = plan_cells(&bare, &[mark_root(rig_id as u32)]).unwrap();
        let keys: Vec<[usize; 2]> = cells.iter().map(|cell| cell.keypoint).collect();
        assert_eq!(keys, [[0, 0], [0, 0], [0, 0], [1, 0], [1, 0], [1, 0]]);
        let write = |model: &mut Value, cells: &[Cell]| {
            let flush = write_cells(model, &values, &mut BTreeMap::new(), cells, Vec::new(), None).unwrap();
            assert!(flush.pending.is_empty());
        };
        let mut together = bare.clone();
        write(&mut together, &cells);
        let mut one_by_one = bare.clone();
        for cell in &cells {
            write(&mut one_by_one, std::slice::from_ref(cell));
        }
        assert_eq!(together, one_by_one);
        // A call with room for the eight vertices of two grids stops inside the first key, and the rest follows.
        let mut in_two = bare.clone();
        let first = write_cells(&mut in_two, &values, &mut BTreeMap::new(), &cells, Vec::new(), Some(8)).unwrap();
        assert_eq!(first.pending, cells[2..]);
        write(&mut in_two, &first.pending);
        assert_eq!(together, in_two);
        assert_eq!(together, snapshot["model"], "the session stored the same keys");
        // The parent does reach the child here: the child alone, without the key of its parent, gets other offsets.
        let child = cells.iter().find(|cell| cell.target == child_id as u32 && cell.keypoint == [1, 0]).unwrap();
        let mut alone = bare.clone();
        write(&mut alone, std::slice::from_ref(child));
        assert_ne!(
            deform_binding(&alone, parameter_id, child_id).unwrap()["values"][1][0],
            deform_binding(&together, parameter_id, child_id).unwrap()["values"][1][0]
        );
    }

    #[test]
    fn chunked_offsets_match_the_small_cell_path() {
        let mut session = crate::Session::new();
        let rig_id = created_id(&ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        )));
        let bone_id = created_id(&ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        )));
        let child_id = created_id(&ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Child"}),
        )));
        let parent_id = created_id(&ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        )));
        ok(&call(
            &mut session,
            &json!({"op":"node.move","nodes":[child_id],"parent":parent_id}),
        ));
        for id in [parent_id, child_id] {
            ok(&call(
                &mut session,
                &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[0.0,10.0],"axisY":[0.0,20.0]}),
            ));
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":id,"bone":bone_id}),
            ));
        }
        let parameter_id = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Yaw","dimensions":1,
                "min":[0.0,0.0],"max":[1.0,1.0]
            }),
        ))["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"rotation_y":0.4
            }),
        ));
        let snapshot = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let model = &snapshot["model"];
        let values = runtime_values(&snapshot["values"]);
        for target in [parent_id, child_id] {
            let whole = whole_cell_offsets(model, &values, rig_id, parameter_id, target, [1, 0]);
            let mut by_one = Vec::new();
            for start in 0..whole.len() {
                by_one.extend(
                    crate::bone_deform::cell_offset_range(
                        model,
                        &values,
                        rig_id as u32,
                        target as u32,
                        parameter_id as u32,
                        [1, 0],
                        &BTreeMap::new(),
                        start,
                        start + 1,
                    )
                    .unwrap(),
                );
            }
            assert_eq!(
                by_one, whole,
                "one-vertex chunks must match the small-cell path"
            );
            let mut uneven = crate::bone_deform::cell_offset_range(
                model,
                &values,
                rig_id as u32,
                target as u32,
                parameter_id as u32,
                [1, 0],
                &BTreeMap::new(),
                0,
                3,
            )
            .unwrap();
            uneven.extend(
                crate::bone_deform::cell_offset_range(
                    model,
                    &values,
                    rig_id as u32,
                    target as u32,
                    parameter_id as u32,
                    [1, 0],
                    &BTreeMap::new(),
                    3,
                    whole.len(),
                )
                .unwrap(),
            );
            assert_eq!(
                uneven, whole,
                "uneven chunks must match the small-cell path"
            );
        }
    }

    #[test]
    fn large_deform_cell_matches_the_small_cell_path() {
        const WIDTH: u64 = 5;
        const HEIGHT: u64 = 52_429;
        const VERTICES: usize = (WIDTH * HEIGHT) as usize;
        assert_eq!(VERTICES, 262_145);

        let mut session = crate::Session::new();
        let rig_id = created_id(&ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        )));
        let bone_id = created_id(&ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        )));
        let grid_id = created_id(&ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        )));
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter_id = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Pose","dimensions":1,
                "min":[0.0,0.0],"max":[1.0,1.0]
            }),
        ))["uuid"]
            .as_u64()
            .unwrap();
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        let xs: Vec<f64> = (0..WIDTH).map(|index| index as f64).collect();
        let ys: Vec<f64> = (0..HEIGHT).map(|index| index as f64).collect();
        assert!(replace_grid_axes(&mut model, grid_id, &xs, &ys));
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(before["deformPending"], false);
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[4.0,0.0]
            }),
        ));
        let pending = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(pending["deformPending"], true);
        assert_eq!(
            pending["revision"],
            before["revision"].as_u64().unwrap() + 1
        );
        assert_ne!(
            deform_vertex_len(&pending["model"], parameter_id, grid_id, 1),
            Some(VERTICES)
        );
        let idle = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(idle["revision"], pending["revision"]);
        assert_eq!(idle["deformPending"], true);
        let blocked = call(
            &mut session,
            &json!({"op":"node.rename","id":grid_id,"name":"Renamed"}),
        );
        assert_eq!(blocked["succeeded"], false, "{blocked}");
        assert!(blocked["message"]
            .as_str()
            .unwrap()
            .contains("deform refresh is still pending"));
        // The presets of the user are no part of the document, so they do not wait for the refresh.
        let settings = std::env::temp_dir().join(format!("soul2d-dirty-presets-{}", std::process::id()));
        session.set_config_dir(settings.clone());
        let kept = json!({"algorithm": "contour"});
        let saved = ok(&call(&mut session, &json!({"op":"automesh.preset.save","name":"A","algorithm":"contour"})));
        assert_eq!(saved, json!({"name": "A", "preset": kept, "replaced": false}));
        assert_eq!(ok(&call(&mut session, &json!({"op":"automesh.preset.list"}))), json!({"presets": {"A": kept}}));
        assert_eq!(ok(&call(&mut session, &json!({"op":"automesh.preset.delete","name":"A"}))), json!({"name": "A"}));
        let saved = ok(&call(&mut session, &json!({"op":"physics.preset.save","name":"P","gravity":2.5})));
        assert_eq!(saved, json!({"name": "P", "preset": {"gravity": 2.5}, "replaced": false}));
        let listed = ok(&call(&mut session, &json!({"op":"physics.preset.list"})));
        assert_eq!(listed, json!({"presets": {"P": {"gravity": 2.5}}}));
        assert_eq!(ok(&call(&mut session, &json!({"op":"physics.preset.delete","name":"P"}))), json!({"name": "P"}));
        let items = json!([
            {"x": 0, "y": 0, "mobility": 1, "delay": 1, "acceleration": 1, "radius": 0},
            {"x": 0, "y": 10, "mobility": 0.9, "delay": 0.5, "acceleration": 1.5, "radius": 5}
        ]);
        let saved = ok(&call(&mut session,
            &json!({"op":"live2d.physics3.preset.save","list":"vertices","name":"V","items":items})));
        assert_eq!(saved["replaced"], false);
        let listed = ok(&call(&mut session, &json!({"op":"live2d.physics3.preset.list"})));
        assert_eq!(listed["vertices"]["V"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(listed["inputs"], json!({}));
        let deleted = ok(&call(&mut session, &json!({"op":"live2d.physics3.preset.delete","list":"vertices","name":"V"})));
        assert_eq!(deleted, json!({"name": "V"}));
        let keys = json!([{"frame": 0, "value": 0.5}]);
        let template = json!({"length": 1, "lanes": [{"parameter": "A", "axis": 0, "keyframes": keys}]});
        std::fs::write(settings.join("animation-templates.json"), json!({"presets": {"T": template}}).to_string()).unwrap();
        let listed = ok(&call(&mut session, &json!({"op":"animation.template.list"})));
        assert_eq!(listed, json!({"presets": {"T": template}}));
        let deleted = ok(&call(&mut session, &json!({"op":"animation.template.delete","template":"T"})));
        assert_eq!(deleted, json!({"template": "T"}));
        std::fs::remove_file(settings.join("animation-templates.json")).unwrap();
        std::fs::remove_file(settings.join("physics-presets.json")).unwrap();
        std::fs::remove_file(settings.join("physics3-model-presets.json")).unwrap();
        std::fs::remove_file(settings.join("automesh-presets.json")).unwrap();
        std::fs::remove_dir(&settings).unwrap();
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            pending["model"]
        );

        let continued = ok(&call(&mut session, &json!({"op":"deform.continue"})));
        assert_eq!(continued["pending"], false);
        let done = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(done["deformPending"], false);
        assert_eq!(done["canUndo"], pending["canUndo"]);
        let stored = deform_offsets(&done["model"], parameter_id, grid_id, 1);
        assert_eq!(stored.len(), VERTICES);
        let values = runtime_values(&pending["values"]);
        let whole = whole_cell_offsets(
            &pending["model"],
            &values,
            rig_id,
            parameter_id,
            grid_id,
            [1, 0],
        );
        assert_eq!(
            stored, whole,
            "the assembled cell must match the small-cell path"
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(undone["model"], before["model"]);
        assert_eq!(undone["deformPending"], false);
        ok(&call(&mut session, &json!({"op":"redo"})));
        let redone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(redone["model"], done["model"]);
        assert_eq!(redone["deformPending"], false);

        ok(&call(&mut session, &json!({"op":"undo"})));
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[6.0,0.0]
            }),
        ));
        let again = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(again["deformPending"], true);
        assert_ne!(
            deform_vertex_len(&again["model"], parameter_id, grid_id, 1),
            Some(VERTICES)
        );
        let path =
            std::env::temp_dir().join(format!("vsoul-p5c1-{}-{}.inx", std::process::id(), grid_id));
        let _ = std::fs::remove_file(&path);
        session.fail_save_after_staging = true;
        let failed = call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(!path.exists());
        let kept = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(kept["model"], again["model"]);
        assert_eq!(kept["revision"], again["revision"]);
        assert_eq!(kept["deformPending"], true);
        ok(&call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        ));
        let saved = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(saved["deformPending"], false);
        assert_eq!(
            deform_offsets(&saved["model"], parameter_id, grid_id, 1).len(),
            VERTICES
        );
        let mut other = crate::Session::new();
        ok(&call(
            &mut other,
            &json!({"op":"open","path":path.to_string_lossy()}),
        ));
        let opened = ok(&call(&mut other, &json!({"op":"snapshot"})));
        assert_eq!(opened["deformPending"], false);
        assert_eq!(
            deform_offsets(&opened["model"], parameter_id, grid_id, 1).len(),
            VERTICES
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );
        let _ = std::fs::remove_file(&path);
    }

    fn created_id(created: &Value) -> u64 {
        created["created"][0]["uuid"].as_u64().unwrap()
    }

    fn runtime_values(values: &Value) -> BTreeMap<u32, [f64; 2]> {
        values
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, raw)| {
                let pair = raw.as_array().unwrap();
                (
                    key.parse().unwrap(),
                    [pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()],
                )
            })
            .collect()
    }

    fn whole_cell_offsets(
        model: &Value,
        values: &BTreeMap<u32, [f64; 2]>,
        rig_id: u64,
        parameter_id: u64,
        target_id: u64,
        key: [usize; 2],
    ) -> Vec<[f64; 2]> {
        let outcome = crate::bone_deform::run_with_key(
            model,
            values,
            rig_id as u32,
            &[target_id as u32],
            Some(parameter_id as u32),
            false,
            &BTreeMap::new(),
            Some(key),
            false,
        )
        .unwrap();
        json_offsets(&outcome.body["targets"][0]["offsets"])
    }

    fn json_offsets(value: &Value) -> Vec<[f64; 2]> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| [pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()])
            .collect()
    }

    fn deform_binding<'a>(model: &'a Value, parameter_id: u64, grid_id: u64) -> Option<&'a Value> {
        model["param"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|parameter| {
                if parameter["uuid"].as_u64() != Some(parameter_id) {
                    return None;
                }
                parameter["bindings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|binding| {
                        binding["param_name"] == "deform"
                            && binding["node"].as_u64() == Some(grid_id)
                    })
            })
    }

    fn deform_vertex_len(
        model: &Value,
        parameter_id: u64,
        grid_id: u64,
        key_x: usize,
    ) -> Option<usize> {
        let binding = deform_binding(model, parameter_id, grid_id)?;
        if binding["isSet"][key_x][0].as_bool() != Some(true) {
            return None;
        }
        binding["values"][key_x][0]
            .as_array()
            .map(|values| values.len())
    }

    fn deform_offsets(
        model: &Value,
        parameter_id: u64,
        grid_id: u64,
        key_x: usize,
    ) -> Vec<[f64; 2]> {
        json_offsets(&deform_binding(model, parameter_id, grid_id).unwrap()["values"][key_x][0])
    }

    fn replace_grid_axes(model: &mut Value, grid_id: u64, xs: &[f64], ys: &[f64]) -> bool {
        fn walk(node: &mut Value, grid_id: u64, xs: &[f64], ys: &[f64]) -> bool {
            if node.get("uuid").and_then(Value::as_u64) == Some(grid_id) {
                node["grid_axis_x"] = json!(xs);
                node["grid_axis_y"] = json!(ys);
                return true;
            }
            let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) else {
                return false;
            };
            children
                .iter_mut()
                .any(|child| walk(child, grid_id, xs, ys))
        }
        walk(model.get_mut("nodes").unwrap(), grid_id, xs, ys)
    }

    #[test]
    fn over_budget_flush_continues_on_the_same_history_entry() {
        let mut session = crate::Session::new();
        let source_dir = std::env::temp_dir().join(format!("soul2d-staged-split-{}", std::process::id()));
        std::fs::create_dir(&source_dir).unwrap();
        let source = source_dir.join("source.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 8, 7, 255])).save(&source).unwrap();
        ok(&call(&mut session, &json!({"op":"split.open", "path":source})));
        ok(&call(&mut session, &json!({"op":"split.part.set", "name":"Split", "steps":[{"op":"alpha"}]})));
        ok(&call(&mut session, &json!({"op":"split.apply"})));
        assert_eq!(session.snapshot()["partSplit"]["applied"], true);
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[grid_id],
                "axisX": unit_axis(400), "axisY": unit_axis(500)
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Pose","dimensions":1,
                "min":[-1.0,0.0],"max":[1.0,1.0]
            }),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"translation":[4.0,0.0]
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(before["deformPending"], false);
        assert_eq!(before["partSplit"]["applied"], true);
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"translation":[12.0,0.0,0.0]}),
        ));
        let partial = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(partial["deformPending"], true);
        assert_eq!(partial["partSplit"]["applied"], true);
        assert_eq!(partial["revision"], before["revision"].as_u64().unwrap() + 1);
        let partial_row = deform_row(&mut session, parameter_id, grid_id);
        assert_eq!(partial_row["isSet"][0][0], true);
        assert_eq!(partial_row["isSet"][1][0], false);
        assert_eq!(partial_row["isSet"][2][0], true);
        let idle = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(idle["revision"], partial["revision"]);
        assert_eq!(idle["deformPending"], true);
        let blocked = call(
            &mut session,
            &json!({"op":"node.rename","id":grid_id,"name":"Renamed"}),
        );
        assert_eq!(blocked["succeeded"], false, "{blocked}");
        assert!(blocked["message"]
            .as_str()
            .unwrap()
            .contains("deform refresh is still pending"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["revision"], partial["revision"]);

        let continued = ok(&call(&mut session, &json!({"op":"deform.continue"})));
        assert_eq!(continued["pending"], true);
        let mid = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(mid["canUndo"], partial["canUndo"]);
        assert_eq!(mid["partSplit"]["applied"], true);
        let mid_row = deform_row(&mut session, parameter_id, grid_id);
        assert_eq!(mid_row["isSet"][1][0], true);
        assert_eq!(mid_row["values"][1][0].as_array().unwrap().len(), 200_000);
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(undone["model"], before["model"]);
        assert_eq!(undone["deformPending"], false);
        assert_eq!(undone["partSplit"]["applied"], true);
        ok(&call(&mut session, &json!({"op":"redo"})));
        let redone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(redone["model"], mid["model"]);
        assert_eq!(redone["deformPending"], false);
        assert_eq!(redone["partSplit"]["applied"], true);

        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"translation":[18.0,0.0,0.0]}),
        ));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["deformPending"], true);
        let path = std::env::temp_dir().join(format!(
            "vsoul-e8-{}-{}.inx",
            std::process::id(),
            grid_id
        ));
        let _ = std::fs::remove_file(&path);
        ok(&call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        ));
        let saved = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(saved["deformPending"], false);
        assert_eq!(saved["partSplit"]["applied"], true);
        assert_eq!(deform_row(&mut session, parameter_id, grid_id)["isSet"][2][0], true);
        assert_eq!(saved["canUndo"], true);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], redone["model"]);
        let mut other = crate::Session::new();
        ok(&call(
            &mut other,
            &json!({"op":"open","path":path.to_string_lossy()}),
        ));
        let opened = ok(&call(&mut other, &json!({"op":"snapshot"})));
        assert_eq!(opened["deformPending"], false);
        assert_eq!(opened["canUndo"], false);
        ok(&call(&mut other, &json!({"op":"split.open", "path":source})));
        ok(&call(&mut other, &json!({"op":"split.part.set", "name":"Split", "steps":[{"op":"alpha"}]})));
        assert_eq!(other.snapshot()["partSplit"]["applied"], false);
        let _ = std::fs::remove_file(&path);
        std::fs::remove_dir_all(source_dir).unwrap();
    }

    #[test]
    fn budget_counts_axis_lengths_without_building_the_vertex_grid() {
        let width = 50_000u64;
        let axes: Vec<f64> = (0..width).map(|index| index as f64).collect();
        let model = json!({
            "nodes": {
                "uuid": 1, "type": "Node", "children": [{
                    "uuid": 2, "type": "DepthRigRoot",
                    "bindings": [{"target": 4, "sourceBoneUuids": [3]}],
                    "children": [{
                        "uuid": 3, "type": "DepthBone", "children": []
                    }]
                }, {
                    "uuid": 4, "type": "GridDeformer",
                    "grid_axis_x": axes, "grid_axis_y": axes
                }]
            },
            "param": [{
                "uuid": 9, "is_vec2": false,
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{"node": 3, "param_name": "transform.r.y"}]
            }]
        });
        let marks = [mark_root(2)];
        let error = plan_cells(&model, &absorb(&marks)).unwrap_err();
        assert!(error.contains("exceeds the maximum of 1000000"), "{error}");
    }

    #[test]
    fn convert_drops_the_node_binding_and_refreshes_the_sibling_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Head","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let target = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Eye"}),
        ));
        let target_id = target["created"][0]["uuid"].as_u64().unwrap();
        let sibling = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Sibling"}),
        ));
        let sibling_id = sibling["created"][0]["uuid"].as_u64().unwrap();
        for id in [target_id, sibling_id] {
            ok(&call(
                &mut session,
                &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[-10.0,10.0],"axisY":[0.0,100.0]}),
            ));
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":id,"bone":bone_id}),
            ));
        }
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":target_id,"bone":bone_id,
                "settings":{"depthOffset":0.25}
            }),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_influence_rule","root":rig_id,"target":target_id,
                "rule":{"multipliersByBoneUuid":{bone_id.to_string():0.75}}
            }),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Shift","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[5.0,0.0]
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[target_id],"className":"Node"}),
        ));
        assert_eq!(
            ok(&call(
                &mut session,
                &json!({"op":"node.get","id":target_id})
            ))["type"],
            "Node"
        );
        let bindings =
            ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        assert_eq!(bindings.as_array().unwrap().len(), 1);
        assert_eq!(bindings[0]["target"], sibling_id);
        let deform = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let sibling_deform = deform["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == sibling_id)
            .unwrap();
        assert_eq!(sibling_deform["isSet"][1][0], true);
        assert!(bindings
            .as_array()
            .unwrap()
            .iter()
            .all(|binding| binding["target"] != target_id));
        ok(&call(&mut session, &json!({"op":"undo"})));
        let restored = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(restored["model"], before["model"]);
        assert_eq!(
            ok(&call(
                &mut session,
                &json!({"op":"node.get","id":target_id})
            ))["type"],
            "GridDeformer"
        );
        let restored_bindings =
            ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        let target_binding = restored_bindings
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["target"] == target_id)
            .unwrap();
        assert_eq!(target_binding["sourceSettings"][0]["depthOffset"], 0.25);
        assert_eq!(
            target_binding["influenceRule"]["multipliersByBoneUuid"][&bone_id.to_string()],
            0.75
        );
        let again = call(
            &mut session,
            &json!({"op":"node.convert","nodes":[target_id],"className":"Node"}),
        );
        assert_eq!(again["succeeded"], true, "{again}");
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let idle = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(idle, held);
    }

    #[test]
    fn leftover_deform_survives_convert_until_add_source() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Head","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let target = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Eye"}),
        ));
        let target_id = target["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[target_id],"axisX":[-10.0,10.0],"axisY":[0.0,100.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":target_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Shift","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[5.0,0.0]
            }),
        ));
        let posed = deform_row(&mut session, parameter_id, target_id);
        assert!(posed["values"][1][0]
            .as_array()
            .unwrap()
            .iter()
            .any(|pair| pair != &json!([0.0, 0.0])));
        let before_node = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[target_id],"className":"Node"}),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":target_id})))["type"],
            "Node"
        );
        assert!(rig_targets(&mut session, rig_id).is_empty());
        assert_eq!(deform_row(&mut session, parameter_id, target_id), posed);
        let as_node = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let rejected = call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":target_id,"bone":bone_id}),
        );
        assert_eq!(rejected["succeeded"], false, "{rejected}");
        assert!(rejected["message"]
            .as_str()
            .unwrap()
            .contains("GridDeformer, PathDeformer or Part"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), as_node);

        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[target_id],"className":"GridDeformer"}),
        ));
        let restored_grid = ok(&call(&mut session, &json!({"op":"node.get","id":target_id})));
        assert_eq!(restored_grid["type"], "GridDeformer");
        assert_eq!(restored_grid["grid_axis_x"], json!([-0.5, 0.5]));
        assert!(rig_targets(&mut session, rig_id).is_empty());
        assert_eq!(deform_row(&mut session, parameter_id, target_id), posed);
        let as_grid = ok(&call(&mut session, &json!({"op":"snapshot"})));

        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":target_id,"bone":bone_id}),
        ));
        assert_eq!(rig_targets(&mut session, rig_id), vec![target_id]);
        let refreshed = deform_row(&mut session, parameter_id, target_id);
        assert_eq!(refreshed["isSet"], json!([[true], [true], [true]]));
        assert_ne!(refreshed["isSet"], posed["isSet"]);
        assert_eq!(refreshed["values"][1][0], json!(vec![[5.0, 0.0]; 4]));
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], as_grid["model"]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":target_id})))["type"],
            "Node"
        );
        assert!(rig_targets(&mut session, rig_id).is_empty());
        assert_eq!(deform_row(&mut session, parameter_id, target_id), posed);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before_node["model"]);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":target_id})))["type"],
            "GridDeformer"
        );
        assert_eq!(rig_targets(&mut session, rig_id), vec![target_id]);
    }

    fn deform_row(session: &mut crate::Session, parameter_id: u64, node_id: u64) -> Value {
        ok(&call(session, &json!({"op":"parameter.get","id":parameter_id})))["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node_id)
            .cloned()
            .expect("deform row")
    }

    fn rig_targets(session: &mut crate::Session, rig_id: u64) -> Vec<u64> {
        ok(&call(session, &json!({"op":"node.get","id":rig_id})))
            .get("bindings")
            .and_then(Value::as_array)
            .map(|bindings| {
                bindings
                    .iter()
                    .filter_map(|binding| binding.get("target").and_then(Value::as_u64))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn child_cells_follow_ancestor_offsets_when_the_child_was_bound_first() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let child = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Child"}),
        ));
        let child_id = child["created"][0]["uuid"].as_u64().unwrap();
        let parent = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        ));
        let parent_id = parent["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"node.move","nodes":[child_id],"parent":parent_id}),
        ));
        for id in [child_id, parent_id] {
            ok(&call(
                &mut session,
                &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
            ));
        }
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":child_id,"bone":bone_id}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":parent_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[0.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        ));
        let parent_offsets = deform_row(&mut session, parameter_id, parent_id)["values"][1][0].clone();
        let child_offsets = deform_row(&mut session, parameter_id, child_id)["values"][1][0].clone();
        let parent_corner = &parent_offsets[1];
        assert!(
            (parent_corner[0].as_f64().unwrap() + 1.0).abs() < 1e-4
                && parent_corner[1].as_f64().unwrap().abs() < 1e-4,
            "{parent_corner}"
        );
        assert!(child_offsets.as_array().unwrap().iter().all(|pair| {
            pair[0].as_f64().unwrap().abs() < 1e-4 && pair[1].as_f64().unwrap().abs() < 1e-4
        }));
    }

    #[test]
    fn failed_save_keeps_the_pending_cursor() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[grid_id],
                "axisX": unit_axis(400), "axisY": unit_axis(328)
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Pose","dimensions":1,"min":[0.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[3.0,0.0]
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"translation":[9.0,0.0,0.0]}),
        ));
        let pending = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(pending["deformPending"], true);
        let path = std::env::temp_dir().join(format!("vsoul-e8-fail-{}-{}.inx", std::process::id(), grid_id));
        let _ = std::fs::remove_file(&path);
        session.fail_save_after_staging = true;
        let failed = call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(!path.exists());
        let after = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(after["model"], pending["model"]);
        assert_eq!(after["revision"], pending["revision"]);
        assert_eq!(after["deformPending"], true);
        assert_eq!(after["canUndo"], pending["canUndo"]);
    }

    #[test]
    fn failed_convert_refresh_is_not_retried() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Head","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let target = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Eye"}),
        ));
        let target_id = target["created"][0]["uuid"].as_u64().unwrap();
        let sibling = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Huge"}),
        ));
        let sibling_id = sibling["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[target_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":target_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({
                "op":"parameter.create","name":"Pose","dimensions":2,
                "min":[0.0,0.0],"max":[1.0,1.0]
            }),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[0,0],"translation":[1.0,0.0]
            }),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_grid","nodes":[sibling_id],
                "axisX": unit_axis(1000), "axisY": unit_axis(1001)
            }),
        ));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let failed = call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":sibling_id,"bone":bone_id}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"]
            .as_str()
            .unwrap()
            .contains("exceeds the maximum of 1000000"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
        let retried = call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":sibling_id,"bone":bone_id}),
        );
        assert_eq!(retried["succeeded"], false, "{retried}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn delete_drops_one_bone_source_and_refreshes_deform_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let mut bone_ids = Vec::new();
        for name in ["Bone.A", "Bone.B"] {
            let bone = ok(&call(
                &mut session,
                &json!({
                    "op":"node.create","className":"DepthBone","name":name,"parents":[rig_id],
                    "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
                }),
            ));
            bone_ids.push(bone["created"][0]["uuid"].as_u64().unwrap());
        }
        let (bone_a, bone_b) = (bone_ids[0], bone_ids[1]);
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Target"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[-10.0,10.0],"axisY":[0.0,100.0]}),
        ));
        for bone_id in [bone_a, bone_b] {
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
            ));
        }
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_influence_rule","root":rig_id,"target":grid_id,
                "rule":{"multipliersByBoneUuid":{
                    bone_a.to_string(): 0.25,
                    bone_b.to_string(): 0.75
                }}
            }),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Shift","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_b],
                "key":[1,0],"translation":[5.0,0.0]
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let before_deform = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let posed = before_deform["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == grid_id)
            .unwrap();
        assert_eq!(posed["isSet"][1][0], true);
        assert_eq!(posed["isSet"][0][0], false);
        ok(&call(
            &mut session,
            &json!({"op":"node.delete","nodes":[bone_a]}),
        ));
        let sources = ok(&call(
            &mut session,
            &json!({"op":"rig.list_sources","root":rig_id,"target":grid_id}),
        ));
        assert_eq!(sources["sourceBoneUuids"], json!([bone_b]));
        let rule = ok(&call(
            &mut session,
            &json!({"op":"rig.get_influence_rule","root":rig_id,"target":grid_id}),
        ));
        assert!(rule["multipliersByBoneUuid"]
            .get(&bone_a.to_string())
            .is_none());
        assert_eq!(rule["multipliersByBoneUuid"][&bone_b.to_string()], 0.75);
        let refreshed = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let deform = refreshed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == grid_id)
            .unwrap();
        assert_eq!(deform["isSet"][0][0], true);
        assert_eq!(deform["isSet"][1][0], true);
        assert_eq!(deform["isSet"][2][0], true);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );
        let restored = ok(&call(
            &mut session,
            &json!({"op":"rig.get_influence_rule","root":rig_id,"target":grid_id}),
        ));
        assert_eq!(restored["multipliersByBoneUuid"][&bone_a.to_string()], 0.25);
        assert_eq!(restored["multipliersByBoneUuid"][&bone_b.to_string()], 0.75);
        ok(&call(&mut session, &json!({"op":"redo"})));
        let redone = ok(&call(
            &mut session,
            &json!({"op":"rig.list_sources","root":rig_id,"target":grid_id}),
        ));
        assert_eq!(redone["sourceBoneUuids"], json!([bone_b]));
        let redone_rule = ok(&call(
            &mut session,
            &json!({"op":"rig.get_influence_rule","root":rig_id,"target":grid_id}),
        ));
        assert!(redone_rule["multipliersByBoneUuid"]
            .get(&bone_a.to_string())
            .is_none());
    }

    #[test]
    fn converting_an_undriven_deform_row_keeps_the_offsets() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Head","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let parent = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        ));
        let parent_id = parent["created"][0]["uuid"].as_u64().unwrap();
        let child = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Child","parents":[parent_id]}),
        ));
        let child_id = child["created"][0]["uuid"].as_u64().unwrap();
        for id in [parent_id, child_id] {
            ok(&call(
                &mut session,
                &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[-10.0,10.0],"axisY":[0.0,100.0]}),
            ));
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":id,"bone":bone_id}),
            ));
        }
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Shift","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"translation":[5.0,0.0]
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[parent_id],"className":"Node"}),
        ));
        assert_eq!(
            ok(&call(
                &mut session,
                &json!({"op":"node.get","id":parent_id})
            ))["type"],
            "Node"
        );
        let bindings =
            ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        assert_eq!(bindings.as_array().unwrap().len(), 1);
        assert_eq!(bindings[0]["target"], child_id);
        let deform = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let parent_deform = deform["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == parent_id)
            .cloned()
            .expect("leftover deform row");
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[parent_id],"className":"Composite"}),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":parent_id})))["type"],
            "Composite"
        );
        let after = ok(&call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        let kept = after["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == parent_id)
            .cloned()
            .expect("convert keeps the undriven deform row");
        assert_eq!(kept["values"], parent_deform["values"]);
        assert_eq!(kept["isSet"], parent_deform["isSet"]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":parent_id})))["type"],
            "Node"
        );
    }
}
