use crate::bone_deform::{self, Mat4};
use crate::render;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

type Vec2 = [f64; 2];

const AXIS_TOLERANCE: f64 = 1e-4;
const MAX_AXIS_POINTS: usize = 256;
const MAX_MESH_MASK_CELLS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct SceneDeform {
    pub(crate) world_matrices: BTreeMap<u32, Mat4>,
    /// Effective local vertex offsets. Ordinary nodes use a one-element array
    /// for their filter-produced translation offset.
    pub(crate) local_offsets: BTreeMap<u32, Vec<Vec2>>,
    /// Offsets in model coordinates that a Part's vertices take after all deformation and glue (form edits in
    /// Adjustment Mode 2), one per vertex.
    pub(crate) world_offsets: BTreeMap<u32, Vec<Vec2>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PathUpdate {
    pub(crate) offsets: Vec<Vec2>,
    pub(crate) reset_reference: bool,
}

#[cfg(test)]
pub(crate) fn evaluate(
    model: &Value,
    input_deformations: &BTreeMap<u32, Vec<Vec2>>,
) -> Result<SceneDeform, String> {
    let mut no_path_update = |_: u32, _: &Value, _: Mat4, _: f64, _: &[Vec2]| Ok(None);
    let root = model.get("nodes").ok_or("model.nodes is missing")?;
    evaluate_with_path_update(model, root, input_deformations, &mut no_path_update)
}

pub(crate) fn evaluate_with_path_update(
    model: &Value,
    original_root: &Value,
    input_deformations: &BTreeMap<u32, Vec<Vec2>>,
    path_update: &mut dyn FnMut(
        u32,
        &Value,
        Mat4,
        f64,
        &[Vec2],
    ) -> Result<Option<PathUpdate>, String>,
) -> Result<SceneDeform, String> {
    let root = model
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    let root_id = node_id(root)?;
    let mut index = NodeIndex {
        root_local: render::node_matrix(original_root, u64::from(root_id))?,
        root_rotation_z: local_rotation_z(original_root, root_id)?,
        ..NodeIndex::default()
    };
    index_all(root, None, &mut index)?;
    let mut visible = HashSet::new();
    collect_visible(root, true, &mut visible)?;
    let mut required = HashSet::from([root_id]);
    required.extend(visible.iter().copied());
    let geometry = collect_draw_dependencies(&index, &visible);
    required.extend(geometry.iter().copied());
    let mut dependencies = required.clone();
    for id in dependencies.clone() {
        let mut current = Some(id);
        while let Some(node_id) = current {
            dependencies.insert(node_id);
            current = index.parents.get(&node_id).copied().flatten();
        }
    }
    required = dependencies;
    let mut output = SceneDeform {
        world_matrices: BTreeMap::new(),
        local_offsets: input_deformations.clone(),
        world_offsets: BTreeMap::new(),
    };
    let mut plans = HashMap::new();
    let mut order = Vec::new();
    walk_pre_filters(
        root,
        None,
        None,
        &[],
        &[],
        root_id,
        &required,
        &visible,
        &geometry,
        &index,
        input_deformations,
        path_update,
        &mut plans,
        &mut order,
        &mut output,
    )?;

    let post0 = build_dynamic_filters(FilterKind::MeshGroup, &plans, &order, &index)?;
    let post1_path = build_dynamic_filters(FilterKind::Path, &plans, &order, &index)?;
    apply_post_filters(
        root_id,
        FilterStage::MeshGroupPost0,
        &post0,
        &mut plans,
        &order,
        &index,
        &mut output,
    )?;
    apply_post_filters(
        root_id,
        FilterStage::GridPathPost1,
        &post1_path,
        &mut plans,
        &order,
        &index,
        &mut output,
    )?;
    refresh_world_matrices(root_id, &order, &mut plans, &index, &mut output)?;
    Ok(output)
}

#[derive(Default)]
struct NodeIndex<'a> {
    nodes: HashMap<u32, &'a Value>,
    parents: HashMap<u32, Option<u32>>,
    root_local: Mat4,
    root_rotation_z: f64,
}

fn index_all<'a>(
    node: &'a Value,
    parent: Option<u32>,
    index: &mut NodeIndex<'a>,
) -> Result<(), String> {
    let id = node
        .get("uuid")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok());
    if let Some(id) = id {
        if index.nodes.insert(id, node).is_some() {
            return Err(format!("duplicate node id {id}"));
        }
        index.parents.insert(id, parent);
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            index_all(child, id.or(parent), index)?;
        }
    }
    Ok(())
}

fn collect_visible(
    node: &Value,
    parent_visible: bool,
    visible: &mut HashSet<u32>,
) -> Result<(), String> {
    if !parent_visible {
        return Ok(());
    }
    let id = node_id(node)?;
    let enabled = node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    if !enabled {
        return Ok(());
    }
    visible.insert(id);
    if let Some(raw) = node.get("children") {
        let children = raw
            .as_array()
            .ok_or_else(|| format!("node {id}.children must be an array"))?;
        for child in children {
            collect_visible(child, true, visible)?;
        }
    }
    Ok(())
}

fn collect_draw_dependencies(index: &NodeIndex<'_>, visible: &HashSet<u32>) -> HashSet<u32> {
    let mut geometry = Vec::new();
    for id in visible {
        let Some(node) = index.nodes.get(id) else {
            continue;
        };
        if !matches!(node_type(node), "Part" | "Composite" | "DynamicComposite") {
            continue;
        }
        geometry.push(*id);
    }

    let mut visited = HashSet::new();
    while let Some(id) = geometry.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(node) = index.nodes.get(&id) else {
            continue;
        };
        if !matches!(node_type(node), "Part" | "Mask" | "Composite" | "DynamicComposite") {
            continue;
        }
        if matches!(node_type(node), "Composite" | "DynamicComposite") {
            include_source_subtree(node, &mut geometry);
        }
        match node.get("masks") {
            Some(Value::Array(masks)) => {
                geometry.extend(masks.iter().filter_map(|mask| as_u32(mask.get("source"))));
            }
            Some(_) => {}
            None => {
                if let Some(sources) = node.get("masked_by").and_then(Value::as_array) {
                    geometry.extend(sources.iter().filter_map(|source| as_u32(Some(source))));
                }
            }
        }
        if let Some(links) = node.get("weldedLinks").and_then(Value::as_array) {
            geometry.extend(
                links
                    .iter()
                    .filter_map(|link| as_u32(link.get("targetUUID"))),
            );
        }
    }
    visited
}

fn include_source_subtree(node: &Value, geometry: &mut Vec<u32>) {
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            if matches!(node_type(child), "Part" | "Mask" | "Composite" | "DynamicComposite") {
                if let Some(id) = as_u32(child.get("uuid")) {
                    geometry.push(id);
                }
            }
            include_source_subtree(child, geometry);
        }
    }
}

fn as_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
}

struct NodePlan {
    base_local: Mat4,
    local: Mat4,
    pinned_local: Option<Mat4>,
    pin_translation_offset: Vec2,
    world: Mat4,
    filter_target_world: Mat4,
    translation_offset: Vec2,
    local_rotation_z: f64,
    world_rotation_z: f64,
    deformable: bool,
    preserve_geometry_offsets: bool,
    points: Option<Vec<Vec2>>,
    offsets: Vec<Vec2>,
    filter_offsets: Vec<Vec2>,
    path_reference: Option<Vec<Vec2>>,
    incoming_sources: Vec<u32>,
}

fn walk_pre_filters(
    node: &Value,
    parent_id: Option<u32>,
    parent_world: Option<Mat4>,
    incoming_filters: &[Rc<SceneFilter>],
    incoming_sources: &[u32],
    root_id: u32,
    required: &HashSet<u32>,
    visible: &HashSet<u32>,
    geometry: &HashSet<u32>,
    index: &NodeIndex<'_>,
    input: &BTreeMap<u32, Vec<Vec2>>,
    path_update: &mut dyn FnMut(
        u32,
        &Value,
        Mat4,
        f64,
        &[Vec2],
    ) -> Result<Option<PathUpdate>, String>,
    plans: &mut HashMap<u32, NodePlan>,
    order: &mut Vec<u32>,
    output: &mut SceneDeform,
) -> Result<(), String> {
    let id = node_id(node)?;
    if !required.contains(&id) {
        return Ok(());
    }
    let kind = node_type(node);
    let scheduled_as_deformable = is_deformable(kind);
    let is_composite = matches!(kind, "Composite" | "DynamicComposite");
    let auto_composite = kind == "Composite" || kind == "DynamicComposite"
        && node.get("auto_resized").and_then(Value::as_bool).unwrap_or(false);
    let deformable = scheduled_as_deformable && !auto_composite;
    let accepted = incoming_filters
        .iter()
        .filter(|filter| filter_accepts(filter, node))
        .collect::<Vec<_>>();
    let accepted_pre = accepted
        .iter()
        .copied()
        .filter(|filter| !scheduled_as_deformable || !filter.dynamic)
        .collect::<Vec<_>>();
    let is_filter_source = has_required_child(node, required)
        && matches!(kind, "GridDeformer" | "PathDeformer" | "MeshGroup");
    let needs_offsets = input.contains_key(&id)
        || !accepted.is_empty()
        || is_filter_source
        || visible.contains(&id)
        || geometry.contains(&id)
        || (deformable && !incoming_sources.is_empty());
    let points = if auto_composite {
        Some(vec![local_translation(node, id)?])
    } else if needs_offsets {
        Some(if deformable {
            geometry_points(node, id)?
        } else if !accepted.is_empty() || input.contains_key(&id)
            || node.get("pinToMesh").and_then(Value::as_bool).unwrap_or(false) {
            vec![local_translation(node, id)?]
        } else {
            Vec::new()
        })
    } else {
        None
    };
    let mut offsets = if auto_composite {
        if input.get(&id).is_some_and(|values| values.iter().flatten().any(|value| !value.is_finite())) {
            return Err(format!("node {id} deformation is not finite"));
        }
        vec![[0.0, 0.0]]
    } else if let Some(points) = points.as_ref() {
        offsets_for(id, points.len(), input.get(&id))?
    } else {
        Vec::new()
    };
    if offsets.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("node {id} deformation is not finite"));
    }
    let baseline_offsets = offsets.clone();
    let mut touched = input.contains_key(&id);
    let base_local = render::node_matrix(node, u64::from(id))?;
    let base_rotation_z = local_rotation_z(node, id)?;
    let parent_world = parent_world.unwrap_or_else(identity);
    let root_local = index.root_local;
    let locked = node
        .get("lockToRoot")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && id != root_id;
    let pre_local = if !deformable && offsets.len() == 1 && offsets[0] != [0.0, 0.0] {
        bone_deform::mul_mat(translation_matrix(offsets[0]), base_local)
    } else {
        base_local
    };
    let mut pre_world = if locked {
        bone_deform::mul_mat(root_local, pre_local)
    } else if parent_id.is_some() {
        bone_deform::mul_mat(parent_world, pre_local)
    } else {
        pre_local
    };
    let mut local_rotation_z = base_rotation_z;
    let mut pinned_local = None;
    let mut pin_translation_offset = [0.0, 0.0];
    if node.get("pinToMesh").and_then(Value::as_bool).unwrap_or(false) {
        let initial_offset = if !deformable && offsets.len() == 1 { offsets[0] } else { [0.0, 0.0] };
        let pin_node = node_with_translation_offset(node, initial_offset)?;
        let pinned = render::pin_to_parent_mesh(
            &pin_node, u64::from(id), parent_id.and_then(|parent| index.nodes.get(&parent).copied()),
            parent_world, pre_world, locked, &output.local_offsets,
        )?;
        if !deformable && offsets.len() == 1 {
            offsets[0][0] += pinned.local_matrix[0][3] - pre_local[0][3];
            offsets[0][1] += pinned.local_matrix[1][3] - pre_local[1][3];
            pin_translation_offset = offsets[0];
            touched = true;
        }
        local_rotation_z = pinned.local_rotation_z;
        pinned_local = Some(pinned.local_matrix);
        pre_world = bone_deform::mul_mat(if locked { root_local } else { parent_world }, pinned.local_matrix);
    }
    let target_world = if deformable { pre_world } else { parent_world };
    if let Some(points) = points.as_ref() {
        for filter in accepted_pre {
            touched |= apply_filter_with_target_world(
                filter,
                filter.id,
                points,
                &mut offsets,
                target_world,
            )?;
        }
    }
    let mut path_reference = if kind == "PathDeformer" {
        points.as_ref().map(|points| {
            points
                .iter()
                .zip(&offsets)
                .zip(&baseline_offsets)
                .map(|((point, filtered), baseline)| {
                    [
                        point[0] + filtered[0] - baseline[0],
                        point[1] + filtered[1] - baseline[1],
                    ]
                })
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    let translation_offset = if !deformable && offsets.len() == 1 {
        offsets[0]
    } else {
        [0.0, 0.0]
    };
    let local = if let Some(pinned) = pinned_local {
        bone_deform::mul_mat(translation_matrix([
            translation_offset[0] - pin_translation_offset[0],
            translation_offset[1] - pin_translation_offset[1],
        ]), pinned)
    } else if translation_offset == [0.0, 0.0] {
        base_local
    } else {
        bone_deform::mul_mat(translation_matrix(translation_offset), base_local)
    };
    let world = if locked {
        bone_deform::mul_mat(root_local, local)
    } else if parent_id.is_some() {
        bone_deform::mul_mat(parent_world, local)
    } else {
        local
    };
    let parent_rotation_z = parent_id
        .and_then(|parent| plans.get(&parent))
        .map(|plan| plan.world_rotation_z)
        .unwrap_or(0.0);
    let world_rotation_z = if locked {
        local_rotation_z + index.root_rotation_z
    } else if parent_id.is_some() {
        local_rotation_z + parent_rotation_z
    } else {
        local_rotation_z
    };
    if !world_rotation_z.is_finite() {
        return Err(format!("node {id} rotation is not finite"));
    }
    if kind == "PathDeformer" {
        if let Some(points) = points.as_ref() {
            if let Some(update) = path_update(id, node, world, world_rotation_z, &offsets)? {
                offsets = offsets_for(id, points.len(), Some(&update.offsets))?;
                if update.reset_reference {
                    path_reference = Some(points.clone());
                }
                touched = true;
            }
        }
    }
    if touched && !auto_composite {
        output.local_offsets.insert(id, offsets.clone());
    }
    if world.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("node {id} transform is not finite"));
    }
    let filter_target_world = if deformable { world } else { parent_world };
    output.world_matrices.insert(id, world);
    plans.insert(
        id,
        NodePlan {
            base_local,
            local,
            pinned_local,
            pin_translation_offset,
            world,
            filter_target_world,
            translation_offset,
            local_rotation_z,
            world_rotation_z,
            deformable,
            preserve_geometry_offsets: auto_composite,
            points: points.clone(),
            offsets: offsets.clone(),
            filter_offsets: offsets.clone(),
            path_reference: path_reference.clone(),
            incoming_sources: incoming_sources.to_vec(),
        },
    );
    order.push(id);

    let mut child_filters = if must_propagate(node) {
        incoming_filters.to_vec()
    } else {
        Vec::new()
    };
    let mut child_sources = if must_propagate(node) {
        incoming_sources.to_vec()
    } else {
        Vec::new()
    };
    if is_filter_source {
        child_sources.push(id);
        if !is_composite {
            if let Some(filter) = make_filter(
                node,
                id,
                world,
                points.as_deref().unwrap_or(&[]),
                &offsets,
                path_reference.as_deref(),
            )? {
                child_filters.push(Rc::new(filter));
            }
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            let Some(child_id) = as_u32(child.get("uuid")) else {
                continue;
            };
            if required.contains(&child_id) {
                walk_pre_filters(
                    child,
                    Some(id),
                    Some(world),
                    &child_filters,
                    &child_sources,
                    root_id,
                    required,
                    visible,
                    geometry,
                    index,
                    input,
                    path_update,
                    plans,
                    order,
                    output,
                )?;
            }
        }
    }
    Ok(())
}

fn build_dynamic_filters(
    kind: FilterKind,
    plans: &HashMap<u32, NodePlan>,
    order: &[u32],
    index: &NodeIndex<'_>,
) -> Result<HashMap<u32, SceneFilter>, String> {
    let expected_kind = match kind {
        FilterKind::Grid => "GridDeformer",
        FilterKind::Path => "PathDeformer",
        FilterKind::MeshGroup => "MeshGroup",
    };
    let mut filters = HashMap::new();
    for id in order {
        let node = index.nodes.get(id).ok_or("scene node was not indexed")?;
        if node_type(node) != expected_kind {
            continue;
        }
        let plan = plans.get(id).ok_or("scene node plan was not created")?;
        let Some(points) = plan.points.as_deref() else {
            continue;
        };
        let Some(filter) = make_filter(
            node,
            *id,
            plan.world,
            points,
            &plan.filter_offsets,
            plan.path_reference.as_deref(),
        )?
        else {
            continue;
        };
        if filter.dynamic {
            filters.insert(*id, filter);
        }
    }
    Ok(filters)
}

#[derive(Clone, Copy)]
enum FilterStage {
    MeshGroupPost0,
    GridPathPost1,
}

fn apply_post_filters(
    root_id: u32,
    stage: FilterStage,
    filters: &HashMap<u32, SceneFilter>,
    plans: &mut HashMap<u32, NodePlan>,
    order: &[u32],
    index: &NodeIndex<'_>,
    output: &mut SceneDeform,
) -> Result<(), String> {
    for id in order {
        let node = index.nodes.get(id).ok_or("scene node was not indexed")?;
        if !is_deformable(node_type(node)) {
            continue;
        }
        let incoming_sources = plans.get(id).ok_or("scene node plan was not created")?.incoming_sources.clone();
        for source_id in &incoming_sources {
            let source_node = index.nodes.get(source_id).ok_or("scene source was not indexed")?;
            let is_live_grid = matches!(stage, FilterStage::GridPathPost1)
                && node_type(source_node) == "GridDeformer"
                && source_node.get("dynamic").and_then(Value::as_bool).unwrap_or(false);
            let live_grid = if is_live_grid {
                let source = plans.get(source_id).ok_or("scene source plan was not created")?;
                make_filter(source_node, *source_id, source.filter_target_world,
                    source.points.as_deref().unwrap_or(&[]), &source.offsets, None)?
            } else {
                None
            };
            let filter = if is_live_grid { live_grid.as_ref() } else { filters.get(source_id) };
            let Some(filter) = filter else {
                continue;
            };
            let belongs_to_stage = match stage {
                FilterStage::MeshGroupPost0 => filter.kind == FilterKind::MeshGroup,
                FilterStage::GridPathPost1 => {
                    matches!(filter.kind, FilterKind::Grid | FilterKind::Path)
                }
            };
            if !belongs_to_stage || !filter_accepts(filter, node) {
                continue;
            }
            let plan = plans.get_mut(id).ok_or("scene node plan was not created")?;
            let Some(points) = plan.points.as_deref() else {
                continue;
            };
            let changed = apply_filter_with_target_world(
                filter,
                *source_id,
                points,
                &mut plan.offsets,
                plan.filter_target_world,
            )?;
            if changed {
                if !plan.preserve_geometry_offsets {
                    output.local_offsets.insert(*id, plan.offsets.clone());
                }
                if !plan.deformable && plan.offsets.len() == 1 {
                    plan.translation_offset = plan.offsets[0];
                }
            }
        }
    }
    refresh_world_matrices(root_id, order, plans, index, output)?;
    Ok(())
}

fn refresh_world_matrices(
    root_id: u32,
    order: &[u32],
    plans: &mut HashMap<u32, NodePlan>,
    index: &NodeIndex<'_>,
    output: &mut SceneDeform,
) -> Result<(), String> {
    output.world_matrices.clear();
    for id in order {
        let node = index.nodes.get(id).ok_or("scene node was not indexed")?;
        let parent_id = index.parents.get(id).copied().flatten();
        let parent_rotation_z = parent_id
            .and_then(|parent| plans.get(&parent))
            .map(|parent| parent.world_rotation_z)
            .unwrap_or(0.0);
        let plan = plans.get_mut(id).ok_or("scene node plan was not created")?;
        let parent_world = parent_id
            .and_then(|parent| output.world_matrices.get(&parent).copied())
            .unwrap_or_else(identity);
        plan.translation_offset = if !plan.deformable && plan.offsets.len() == 1 {
            plan.offsets[0]
        } else {
            [0.0, 0.0]
        };
        plan.local = if let Some(pinned_local) = plan.pinned_local {
            let post_translation = [
                plan.translation_offset[0] - plan.pin_translation_offset[0],
                plan.translation_offset[1] - plan.pin_translation_offset[1],
            ];
            if post_translation == [0.0, 0.0] {
                pinned_local
            } else {
                bone_deform::mul_mat(translation_matrix(post_translation), pinned_local)
            }
        } else if plan.translation_offset == [0.0, 0.0] {
            plan.base_local
        } else {
            bone_deform::mul_mat(translation_matrix(plan.translation_offset), plan.base_local)
        };
        let locked = node
            .get("lockToRoot")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && *id != root_id;
        let world = if locked {
            bone_deform::mul_mat(index.root_local, plan.local)
        } else if parent_id.is_some() {
            bone_deform::mul_mat(parent_world, plan.local)
        } else {
            plan.local
        };
        plan.world_rotation_z = if locked {
            plan.local_rotation_z + index.root_rotation_z
        } else if parent_id.is_some() {
            plan.local_rotation_z + parent_rotation_z
        } else {
            plan.local_rotation_z
        };
        if !plan.world_rotation_z.is_finite() {
            return Err(format!("node {id} rotation is not finite"));
        }
        plan.world = world;
        plan.filter_target_world = if plan.deformable { world } else { parent_world };
        if world.iter().flatten().any(|value| !value.is_finite()) {
            return Err(format!("node {id} transform is not finite"));
        }
        output.world_matrices.insert(*id, world);
    }
    Ok(())
}

fn has_required_child(node: &Value, required: &HashSet<u32>) -> bool {
    node.get("children")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children
                .iter()
                .any(|child| as_u32(child.get("uuid")).is_some_and(|id| required.contains(&id)))
        })
}

fn node_id(node: &Value) -> Result<u32, String> {
    node.get("uuid")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| "node.uuid must be an unsigned integer".into())
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("Node")
}

fn is_deformable(kind: &str) -> bool {
    matches!(
        kind,
        "Part"
            | "Mask"
            | "MeshGroup"
            | "GridDeformer"
            | "PathDeformer"
            | "Composite"
            | "DynamicComposite"
    )
}

fn must_propagate(node: &Value) -> bool {
    match node_type(node) {
        "Part" | "Mask" | "PathDeformer" => true,
        "Composite" => node
            .get("propagate_meshgroup")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn local_translation(node: &Value, id: u32) -> Result<Vec2, String> {
    let values = node
        .get("transform")
        .and_then(|transform| transform.get("trans"))
        .and_then(Value::as_array);
    let mut result = [0.0; 2];
    if let Some(values) = values {
        for axis in 0..2 {
            if let Some(value) = values.get(axis) {
                result[axis] = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| {
                        format!("node {id}.transform.trans must contain finite numbers")
                    })?;
            }
        }
    }
    Ok(result)
}

fn local_rotation_z(node: &Value, id: u32) -> Result<f64, String> {
    let value = node
        .get("transform")
        .and_then(|transform| transform.get("rot"))
        .and_then(Value::as_array)
        .and_then(|rotation| rotation.get(2));
    match value {
        None => Ok(0.0),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("node {id}.transform.rot must contain finite numbers")),
    }
}

fn node_with_translation_offset(node: &Value, offset: Vec2) -> Result<Value, String> {
    if offset == [0.0, 0.0] {
        return Ok(node.clone());
    }
    let mut adjusted = node.clone();
    let transform = adjusted
        .get_mut("transform")
        .and_then(Value::as_object_mut)
        .ok_or("node transform must be an object")?;
    if !transform.get("trans").is_some_and(Value::is_array) {
        transform.insert("trans".into(), serde_json::json!([0.0, 0.0, 0.0]));
    }
    let translation = transform
        .get_mut("trans")
        .and_then(Value::as_array_mut)
        .ok_or("node transform translation must be an array")?;
    for axis in 0..2 {
        let value = translation.get(axis).and_then(Value::as_f64).unwrap_or(0.0) + offset[axis];
        if !value.is_finite() {
            return Err("node transform is not finite".into());
        }
        if axis < translation.len() {
            translation[axis] = serde_json::json!(value);
        } else {
            translation.push(serde_json::json!(value));
        }
    }
    Ok(adjusted)
}

pub(crate) fn geometry_points(node: &Value, id: u32) -> Result<Vec<Vec2>, String> {
    match node_type(node) {
        "GridDeformer" => grid_geometry(node, id).map(|(_, _, points)| points),
        "PathDeformer" => bone_deform::target_vertices(node),
        "Part" | "Mask" | "MeshGroup" | "Composite" | "DynamicComposite" => {
            let raw = node
                .get("mesh")
                .and_then(|mesh| mesh.get("verts"))
                .or_else(|| node.get("vertices"));
            match raw {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(value) => flat_points(Some(value))
                    .or_else(|| nested_points(Some(value)))
                    .ok_or_else(|| {
                        format!("node {id} mesh vertices must contain finite x,y pairs")
                    }),
            }
        }
        _ => Ok(Vec::new()),
    }
}

fn flat_points(value: Option<&Value>) -> Option<Vec<Vec2>> {
    let values = value?.as_array()?;
    if values.first().is_some_and(Value::is_array) {
        return nested_points(Some(value?));
    }
    if values.len() % 2 != 0 {
        return None;
    }
    values
        .chunks_exact(2)
        .map(|pair| Some([finite_value(&pair[0])?, finite_value(&pair[1])?]))
        .collect()
}

fn nested_points(value: Option<&Value>) -> Option<Vec<Vec2>> {
    value?
        .as_array()?
        .iter()
        .map(|pair| {
            let pair = pair.as_array()?;
            if pair.len() < 2 {
                return None;
            }
            Some([finite_value(&pair[0])?, finite_value(&pair[1])?])
        })
        .collect()
}

fn finite_value(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn offsets_for(id: u32, count: usize, input: Option<&Vec<Vec2>>) -> Result<Vec<Vec2>, String> {
    let offsets = input.cloned().unwrap_or_else(|| vec![[0.0, 0.0]; count]);
    if offsets.len() != count {
        return Err(format!("node {id} deform offset length mismatch"));
    }
    if offsets.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("node {id} deformation is not finite"));
    }
    Ok(offsets)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    Grid,
    Path,
    MeshGroup,
}

struct SceneFilter {
    id: u32,
    kind: FilterKind,
    dynamic: bool,
    translate_children: bool,
    world: Mat4,
    geometry: FilterGeometry,
}

enum FilterGeometry {
    Grid {
        axis_x: Vec<f64>,
        axis_y: Vec<f64>,
        rest: Vec<Vec2>,
        deformed: Vec<Vec2>,
    },
    Path {
        curve: CurveKind,
        rest: Vec<Vec2>,
        deformed: Vec<Vec2>,
    },
    MeshGroup {
        rest: Vec<Vec2>,
        deformed: Vec<Vec2>,
        triangles: Vec<[usize; 3]>,
        bounds: [f64; 4],
    },
}

fn make_filter(
    node: &Value,
    id: u32,
    world: Mat4,
    points: &[Vec2],
    offsets: &[Vec2],
    path_reference: Option<&[Vec2]>,
) -> Result<Option<SceneFilter>, String> {
    if points.len() != offsets.len() || points.is_empty() {
        return Ok(None);
    }
    let dynamic = match node_type(node) {
        "GridDeformer" => node
            .get("dynamic")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => node
            .get("dynamic_deformation")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let deformed = points
        .iter()
        .zip(offsets)
        .map(|(point, offset)| [point[0] + offset[0], point[1] + offset[1]])
        .collect::<Vec<_>>();
    if deformed.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("deformer node {id} geometry is not finite"));
    }
    let translate_children = match node_type(node) {
        "MeshGroup" => node
            .get("translate_children")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => node
            .get("translate_children")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    };
    let geometry = match node_type(node) {
        "GridDeformer" => {
            let (axis_x, axis_y, rest) = grid_geometry(node, id)?;
            if !has_displacement(&rest, &deformed) {
                return Ok(None);
            }
            FilterGeometry::Grid {
                axis_x,
                axis_y,
                rest,
                deformed,
            }
        }
        "PathDeformer" => {
            if points.len() < 2 {
                return Ok(None);
            }
            let reference = path_reference.unwrap_or(points);
            if reference.len() != points.len() {
                return Err(format!(
                    "PathDeformer node {id} reference curve length mismatch"
                ));
            }
            if !has_displacement(reference, &deformed) {
                return Ok(None);
            }
            let curve = match node
                .get("curve_type")
                .and_then(Value::as_str)
                .unwrap_or("Spline")
            {
                "Bezier" => CurveKind::Bezier,
                "Spline" => CurveKind::Spline,
                other => {
                    return Err(format!(
                        "PathDeformer node {id} has unsupported curve type {other:?}"
                    ))
                }
            };
            FilterGeometry::Path {
                curve,
                rest: reference.to_vec(),
                deformed,
            }
        }
        "MeshGroup" => {
            let triangles = mesh_triangles(node, id, points.len())?;
            if triangles.is_empty() || !has_displacement(points, &deformed) {
                return Ok(None);
            }
            let bounds = mesh_bounds(points);
            let width = (bounds[2] - bounds[0] + 1.0).max(0.0);
            let height = (bounds[3] - bounds[1] + 1.0).max(0.0);
            if !width.is_finite()
                || !height.is_finite()
                || width * height > MAX_MESH_MASK_CELLS as f64
            {
                return Err(format!(
                    "MeshGroup node {id} exceeds the deformation mask limit"
                ));
            }
            FilterGeometry::MeshGroup {
                rest: points.to_vec(),
                deformed,
                triangles,
                bounds,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(SceneFilter {
        id,
        kind: match node_type(node) {
            "GridDeformer" => FilterKind::Grid,
            "PathDeformer" => FilterKind::Path,
            _ => FilterKind::MeshGroup,
        },
        dynamic,
        translate_children,
        world,
        geometry,
    }))
}

/// GEO14: the vertices of each node a GridDeformer deforms that lie outside its lattice, at rest or with any keyed
/// deform form of the node's own (`forms`). The grid clamps such a vertex to its border. `model` is the evaluated
/// model the scene was evaluated from; a node the scene did not place is skipped.
pub(crate) fn overhanging_children(
    model: &Value,
    scene: &SceneDeform,
    forms: &BTreeMap<u32, Vec<Vec<Vec2>>>,
) -> Result<Vec<Value>, String> {
    let mut found = Vec::new();
    let root = model.get("nodes").ok_or("model.nodes is missing")?;
    overhang_walk(root, None, scene, forms, &mut found)?;
    Ok(found)
}

fn overhang_walk(
    node: &Value,
    grid: Option<(u32, &Value)>,
    scene: &SceneDeform,
    forms: &BTreeMap<u32, Vec<Vec<Vec2>>>,
    found: &mut Vec<Value>,
) -> Result<(), String> {
    let id = node_id(node)?;
    let kind = node_type(node);
    if let Some((grid_id, grid_node)) = grid.filter(|_| is_deformable(kind)) {
        if let (Some(grid_world), Some(world)) = (scene.world_matrices.get(&grid_id), scene.world_matrices.get(&id)) {
            let center = bone_deform::mul_mat(bone_deform::invert(*grid_world)?, *world);
            let (axis_x, axis_y, _) = grid_geometry(grid_node, grid_id)?;
            let (x_range, y_range) = ([axis_x[0], axis_x[axis_x.len() - 1]], [axis_y[0], axis_y[axis_y.len() - 1]]);
            let outside = |value: f64, range: [f64; 2]| value < range[0] - AXIS_TOLERANCE || value > range[1] + AXIS_TOLERANCE;
            let points = geometry_points(node, id)?;
            let shapes = forms.get(&id).map(Vec::as_slice).unwrap_or(&[]);
            let mut vertices = Vec::new();
            let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for (index, point) in points.iter().enumerate() {
                let offsets = std::iter::once([0.0, 0.0])
                    .chain(shapes.iter().filter(|form| form.len() == points.len()).map(|form| form[index]));
                for offset in offsets {
                    let sample = transform_point(center, [point[0] + offset[0], point[1] + offset[1]])?;
                    bounds = [bounds[0].min(sample[0]), bounds[1].min(sample[1]),
                        bounds[2].max(sample[0]), bounds[3].max(sample[1])];
                    if (outside(sample[0], x_range) || outside(sample[1], y_range)) && vertices.last() != Some(&index) {
                        vertices.push(index);
                    }
                }
            }
            if !vertices.is_empty() {
                found.push(serde_json::json!({
                    "deformer": grid_id, "deformerName": grid_node.get("name"), "child": id,
                    "childName": node.get("name"), "vertices": vertices, "bounds": bounds,
                }));
            }
        }
    }
    let next = if kind == "GridDeformer" {
        Some((id, node))
    } else if must_propagate(node) {
        grid
    } else {
        None
    };
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        overhang_walk(child, next, scene, forms, found)?;
    }
    Ok(())
}

/// A direct child of a GridDeformer seen from the grid: its rest points in grid space, and the map back to its own
/// space (issue #167).
pub(crate) struct GridChild {
    pub(crate) id: u32,
    points: Vec<Vec2>,
    inverse_center: Mat4,
}

impl GridChild {
    /// The offsets, in the child's own space, that a grid moved from `rest` to `deformed` adds to the child's rest
    /// points. A non-dynamic grid samples at the rest point, so these do not depend on the child's own offsets.
    pub(crate) fn displacement(&self, axis_x: &[f64], axis_y: &[f64], rest: &[Vec2], deformed: &[Vec2])
    -> Result<Vec<Vec2>, String> {
        self.points.iter().map(|point| {
            let (base, target) = sample_grid(axis_x, axis_y, rest, deformed, *point).ok_or("the grid is invalid")?;
            Ok(transform_vector(self.inverse_center, [target[0] - base[0], target[1] - base[1]]))
        }).collect()
    }
}

/// The direct children of GridDeformer `grid`, refusing a child whose motion under the grid is not carried by its own
/// points alone: one the grid only translates, and one that passes the grid on to children of its own.
pub(crate) fn grid_children(model: &Value, scene: &SceneDeform, grid: u32) -> Result<Vec<GridChild>, String> {
    let node = crate::nodes::find_node(model, grid).ok_or_else(|| format!("node {grid} was not found"))?;
    let grid_world = scene.world_matrices.get(&grid).ok_or_else(|| format!("node {grid} has no world matrix"))?;
    let inverse_grid = bone_deform::invert(*grid_world)?;
    let mut children = Vec::new();
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        let id = node_id(child)?;
        let kind = node_type(child);
        if !matches!(kind, "Part" | "Mask" | "MeshGroup" | "GridDeformer" | "PathDeformer") {
            return Err(format!("child {id} is a {kind}, which the grid does not deform point by point"));
        }
        if must_propagate(child) && child.get("children").and_then(Value::as_array).is_some_and(|nodes| !nodes.is_empty()) {
            return Err(format!("child {id} passes the grid on to its own children"));
        }
        let world = scene.world_matrices.get(&id).ok_or_else(|| format!("node {id} has no world matrix"))?;
        let center = bone_deform::mul_mat(inverse_grid, *world);
        let points = geometry_points(child, id)?
            .into_iter()
            .map(|point| transform_point(center, point))
            .collect::<Result<_, _>>()?;
        children.push(GridChild { id, points, inverse_center: bone_deform::invert(center)? });
    }
    Ok(children)
}

fn has_displacement(rest: &[Vec2], deformed: &[Vec2]) -> bool {
    rest.iter().zip(deformed).any(|(a, b)| a != b)
}

fn filter_accepts(filter: &SceneFilter, node: &Value) -> bool {
    let kind = node_type(node);
    if kind == "Composite"
        && filter.kind != FilterKind::Path
        && node
            .get("propagate_meshgroup")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return false;
    }
    if kind == "PathDeformer"
        && filter.kind != FilterKind::Path
        && node.get("physics").is_none_or(Value::is_null)
    {
        return false;
    }
    match filter.kind {
        FilterKind::Grid => is_deformable(kind) || filter.translate_children,
        FilterKind::Path => matches!(
            kind,
            "Part" | "Mask" | "MeshGroup" | "Composite" | "DynamicComposite" | "GridDeformer"
        ),
        FilterKind::MeshGroup => is_deformable(kind) || filter.translate_children,
    }
}

fn apply_filter_with_target_world(
    filter: &SceneFilter,
    filter_id: u32,
    points: &[Vec2],
    offsets: &mut [Vec2],
    target_matrix: Mat4,
) -> Result<bool, String> {
    let Ok(filter_inverse) = bone_deform::invert(filter.world) else {
        return Ok(false);
    };
    let center = bone_deform::mul_mat(filter_inverse, target_matrix);
    let Ok(inverse_center) = bone_deform::invert(center) else {
        return Ok(false);
    };
    match &filter.geometry {
        FilterGeometry::Grid {
            axis_x,
            axis_y,
            rest,
            deformed,
        } => apply_grid(
            filter_id,
            center,
            inverse_center,
            axis_x,
            axis_y,
            rest,
            deformed,
            points,
            offsets,
            filter.dynamic,
        ),
        FilterGeometry::Path {
            curve,
            rest,
            deformed,
        } => apply_path(
            filter_id,
            center,
            inverse_center,
            *curve,
            rest,
            deformed,
            points,
            offsets,
        ),
        FilterGeometry::MeshGroup {
            rest,
            deformed,
            triangles,
            bounds,
        } => apply_mesh_group(
            filter_id,
            center,
            inverse_center,
            rest,
            deformed,
            triangles,
            *bounds,
            points,
            offsets,
            filter.dynamic,
        ),
    }
}

fn apply_grid(
    id: u32,
    center: Mat4,
    inverse_center: Mat4,
    axis_x: &[f64],
    axis_y: &[f64],
    rest: &[Vec2],
    deformed: &[Vec2],
    points: &[Vec2],
    offsets: &mut [Vec2],
    dynamic: bool,
) -> Result<bool, String> {
    let mut changed = false;
    for (point, offset) in points.iter().zip(offsets) {
        let mut sample = transform_point(center, *point)?;
        if dynamic {
            let current = transform_vector(center, *offset);
            sample[0] += current[0];
            sample[1] += current[1];
        }
        let (base, target) = sample_grid(axis_x, axis_y, rest, deformed, [sample[0], sample[1]])
            .ok_or_else(|| format!("GridDeformer node {id} has an invalid grid"))?;
        let local = transform_vector(inverse_center, [target[0] - base[0], target[1] - base[1]]);
        if !local.iter().all(|value| value.is_finite()) {
            return Err(format!("GridDeformer node {id} deformation is not finite"));
        }
        if local != [0.0, 0.0] {
            offset[0] += local[0];
            offset[1] += local[1];
            if !offset.iter().all(|value| value.is_finite()) {
                return Err(format!("GridDeformer node {id} deformation is not finite"));
            }
            changed = true;
        }
    }
    Ok(changed)
}

fn sample_grid(
    axis_x: &[f64],
    axis_y: &[f64],
    rest: &[Vec2],
    deformed: &[Vec2],
    point: Vec2,
) -> Option<(Vec2, Vec2)> {
    let cols = axis_x.len();
    let rows = axis_y.len();
    if cols < 2 || rows < 2 || rest.len() != cols * rows || deformed.len() != rest.len() {
        return None;
    }
    let x = point[0].clamp(axis_x[0], *axis_x.last()?);
    let y = point[1].clamp(axis_y[0], *axis_y.last()?);
    let xi = axis_interval(axis_x, x)?;
    let yi = axis_interval(axis_y, y)?;
    let dx = axis_x[xi + 1] - axis_x[xi];
    let dy = axis_y[yi + 1] - axis_y[yi];
    let mut u = if dx > 0.0 { (x - axis_x[xi]) / dx } else { 0.0 };
    let mut v = if dy > 0.0 { (y - axis_y[yi]) / dy } else { 0.0 };
    if u <= AXIS_TOLERANCE {
        u = 0.0;
    } else if u >= 1.0 - AXIS_TOLERANCE {
        u = 1.0;
    }
    if v <= AXIS_TOLERANCE {
        v = 0.0;
    } else if v >= 1.0 - AXIS_TOLERANCE {
        v = 1.0;
    }
    let indices = [
        yi * cols + xi,
        yi * cols + xi + 1,
        (yi + 1) * cols + xi,
        (yi + 1) * cols + xi + 1,
    ];
    let weights = [(1.0 - u) * (1.0 - v), u * (1.0 - v), (1.0 - u) * v, u * v];
    let mut base = [0.0; 2];
    let mut target = [0.0; 2];
    for (index, weight) in indices.into_iter().zip(weights) {
        for axis in 0..2 {
            base[axis] += rest[index][axis] * weight;
            target[axis] += deformed[index][axis] * weight;
        }
    }
    Some((base, target))
}

fn axis_interval(axis: &[f64], value: f64) -> Option<usize> {
    if axis.len() < 2 {
        return None;
    }
    axis.windows(2)
        .position(|pair| value <= pair[1])
        .or(Some(axis.len() - 2))
}

fn grid_geometry(node: &Value, id: u32) -> Result<(Vec<f64>, Vec<f64>, Vec<Vec2>), String> {
    let axis = |key: &str| -> Result<Vec<f64>, String> {
        let mut values = match node.get(key).and_then(Value::as_array) {
            Some(values) if !values.is_empty() => values
                .iter()
                .map(|value| {
                    finite_value(value).ok_or_else(|| {
                        format!("GridDeformer node {id}.{key} must contain finite numbers")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => vec![-0.5, 0.5],
        };
        values.sort_by(f64::total_cmp);
        values.dedup_by(|a, b| close_axis(*a, *b));
        if values.len() < 2 || values.len() > MAX_AXIS_POINTS {
            return Err(format!("GridDeformer node {id}.{key} is invalid"));
        }
        Ok(values)
    };
    let axis_x = axis("grid_axis_x")?;
    let axis_y = axis("grid_axis_y")?;
    let count = axis_x
        .len()
        .checked_mul(axis_y.len())
        .ok_or_else(|| format!("GridDeformer node {id} has an invalid grid"))?;
    let mut points = Vec::with_capacity(count);
    for y in &axis_y {
        for x in &axis_x {
            points.push([*x, *y]);
        }
    }
    Ok((axis_x, axis_y, points))
}

fn close_axis(left: f64, right: f64) -> bool {
    let difference = (left - right).abs();
    difference <= AXIS_TOLERANCE || (right != 0.0 && difference / right.abs() <= AXIS_TOLERANCE)
}

fn mesh_triangles(node: &Value, id: u32, vertex_count: usize) -> Result<Vec<[usize; 3]>, String> {
    let raw = node
        .get("mesh")
        .and_then(|mesh| mesh.get("indices"))
        .or_else(|| node.get("indices"));
    let Some(indices) = raw.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    indices
        .chunks_exact(3)
        .map(|triangle| {
            let mut result = [0; 3];
            for (slot, value) in result.iter_mut().zip(triangle) {
                *slot = value
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < vertex_count)
                    .ok_or_else(|| format!("MeshGroup node {id} index is out of range"))?;
            }
            Ok(result)
        })
        .collect()
}

fn mesh_bounds(points: &[Vec2]) -> [f64; 4] {
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for point in points {
        bounds[0] = bounds[0].min(point[0]);
        bounds[1] = bounds[1].min(point[1]);
        bounds[2] = bounds[2].max(point[0]);
        bounds[3] = bounds[3].max(point[1]);
    }
    [
        bounds[0].floor(),
        bounds[1].floor(),
        bounds[2].ceil(),
        bounds[3].ceil(),
    ]
}

fn apply_mesh_group(
    id: u32,
    center: Mat4,
    inverse_center: Mat4,
    rest: &[Vec2],
    deformed: &[Vec2],
    triangles: &[[usize; 3]],
    bounds: [f64; 4],
    points: &[Vec2],
    offsets: &mut [Vec2],
    dynamic: bool,
) -> Result<bool, String> {
    let mut changed = false;
    for (point, offset) in points.iter().zip(offsets) {
        let raw = transform_point(center, *point)?;
        let mut sample = [raw[0], raw[1]];
        if dynamic {
            let current = transform_vector(center, *offset);
            sample[0] += current[0];
            sample[1] += current[1];
        }
        let Some(mapped) = sample_mesh_group(rest, deformed, triangles, bounds, sample)? else {
            continue;
        };
        let local = transform_vector(
            inverse_center,
            [mapped[0] - sample[0], mapped[1] - sample[1]],
        );
        if !local.iter().all(|value| value.is_finite()) {
            return Err(format!("MeshGroup node {id} deformation is not finite"));
        }
        if local != [0.0, 0.0] {
            offset[0] += local[0];
            offset[1] += local[1];
            if !offset.iter().all(|value| value.is_finite()) {
                return Err(format!("MeshGroup node {id} deformation is not finite"));
            }
            changed = true;
        }
    }
    Ok(changed)
}

fn sample_mesh_group(
    rest: &[Vec2],
    deformed: &[Vec2],
    triangles: &[[usize; 3]],
    bounds: [f64; 4],
    point: Vec2,
) -> Result<Option<Vec2>, String> {
    if point[0] < bounds[0]
        || point[0] >= bounds[2]
        || point[1] < bounds[1]
        || point[1] >= bounds[3]
    {
        return Ok(None);
    }
    let pixel = [point[0].floor(), point[1].floor()];
    let mut selected = None;
    for (index, triangle) in triangles.iter().enumerate() {
        if point_in_triangle(
            pixel,
            [rest[triangle[0]], rest[triangle[1]], rest[triangle[2]]],
        ) {
            let id = (index + 1) as u16;
            selected = if id == 0 {
                None
            } else {
                Some(usize::from(id) - 1)
            };
        }
    }
    let Some(index) = selected else {
        return Ok(None);
    };
    let tri = triangles[index];
    affine_point(
        [rest[tri[0]], rest[tri[1]], rest[tri[2]]],
        [deformed[tri[0]], deformed[tri[1]], deformed[tri[2]]],
        point,
    )
    .map(Some)
}

fn point_in_triangle(point: Vec2, triangle: [Vec2; 3]) -> bool {
    let sign =
        |a: Vec2, b: Vec2, c: Vec2| (a[0] - c[0]) * (b[1] - c[1]) - (a[1] - c[1]) * (b[0] - c[0]);
    let d1 = sign(point, triangle[0], triangle[1]);
    let d2 = sign(point, triangle[1], triangle[2]);
    let d3 = sign(point, triangle[2], triangle[0]);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

fn affine_point(from: [Vec2; 3], to: [Vec2; 3], point: Vec2) -> Result<Vec2, String> {
    let ab = [from[1][0] - from[0][0], from[1][1] - from[0][1]];
    let ac = [from[2][0] - from[0][0], from[2][1] - from[0][1]];
    let ap = [point[0] - from[0][0], point[1] - from[0][1]];
    let det = ab[0] * ac[1] - ab[1] * ac[0];
    let edge = ab[0]
        .hypot(ab[1])
        .max(ac[0].hypot(ac[1]))
        .max((from[2][0] - from[1][0]).hypot(from[2][1] - from[1][1]));
    if !det.is_finite() || !edge.is_finite() || det.abs() <= 1e-12 * edge * edge {
        return Err("MeshGroup triangle is not finite".into());
    }
    let u = (ap[0] * ac[1] - ap[1] * ac[0]) / det;
    let v = (ab[0] * ap[1] - ab[1] * ap[0]) / det;
    let mapped = [
        to[0][0] + u * (to[1][0] - to[0][0]) + v * (to[2][0] - to[0][0]),
        to[0][1] + u * (to[1][1] - to[0][1]) + v * (to[2][1] - to[0][1]),
    ];
    if mapped.iter().any(|value| !value.is_finite()) {
        return Err("MeshGroup deformation is not finite".into());
    }
    Ok(mapped)
}

#[derive(Clone, Copy)]
enum CurveKind {
    Bezier,
    Spline,
}

fn apply_path(
    id: u32,
    center: Mat4,
    inverse_center: Mat4,
    kind: CurveKind,
    rest: &[Vec2],
    deformed: &[Vec2],
    points: &[Vec2],
    offsets: &mut [Vec2],
) -> Result<bool, String> {
    if rest.len() < 2 || rest.len() != deformed.len() || offsets.len() != points.len() {
        return Ok(false);
    }
    let mut changed = false;
    for (point, offset) in points.iter().zip(offsets) {
        let raw = transform_point(center, *point)?;
        let current = transform_vector(center, *offset);
        let centered = [raw[0] + current[0], raw[1] + current[1]];
        let t = closest_curve_t(kind, rest, raw);
        let (reference, reference_tangent) = curve_sample(kind, rest, t);
        let (target, target_tangent) = curve_sample(kind, deformed, t);
        let tangent_reference = normalize_or(reference_tangent, [1.0, 0.0]);
        let tangent_target = normalize_or(target_tangent, tangent_reference);
        let normal_reference = [-tangent_reference[1], tangent_reference[0]];
        let normal_target = [-tangent_target[1], tangent_target[0]];
        let relative = [centered[0] - reference[0], centered[1] - reference[1]];
        let normal_distance = dot(relative, normal_reference);
        let tangent_distance = dot(relative, tangent_reference);
        let mapped = [
            target[0] + normal_target[0] * normal_distance + tangent_target[0] * tangent_distance,
            target[1] + normal_target[1] * normal_distance + tangent_target[1] * tangent_distance,
        ];
        let local = transform_vector(
            inverse_center,
            [mapped[0] - centered[0], mapped[1] - centered[1]],
        );
        if !local.iter().all(|value| value.is_finite()) {
            return Err(format!("PathDeformer node {id} deformation is not finite"));
        }
        if local != [0.0, 0.0] {
            offset[0] += local[0];
            offset[1] += local[1];
            if !offset.iter().all(|value| value.is_finite()) {
                return Err(format!("PathDeformer node {id} deformation is not finite"));
            }
            changed = true;
        }
    }
    Ok(changed)
}

fn normalize_or(vector: Vec2, fallback: Vec2) -> Vec2 {
    let length = vector[0].hypot(vector[1]);
    if length > 1e-8 && length.is_finite() {
        [vector[0] / length, vector[1] / length]
    } else {
        fallback
    }
}

fn dot(a: Vec2, b: Vec2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

fn closest_curve_t(kind: CurveKind, points: &[Vec2], query: Vec2) -> f64 {
    let c = ((3.0_f64 - 5.0_f64.sqrt()) * 0.5) as f32;
    let cm1 = (1.0_f64 - (3.0_f64 - 5.0_f64.sqrt()) * 0.5) as f32;
    let rel_tolerance = f32::EPSILON.sqrt();
    let abs_tolerance = f32::EPSILON.sqrt();
    let mut left = 0.0_f32;
    let mut right = 1.0_f32;
    let mut v = left * cm1 + right * c;
    let mut fv = curve_distance(kind, points, query, f64::from(v)) as f32;
    let mut w = v;
    let mut fw = fv;
    let mut x = v;
    let mut fx = fv;
    let mut d = 0.0_f32;
    let mut e = 0.0_f32;
    for _ in 0..256 {
        let midpoint = (left + right) * 0.5;
        let tolerance = abs_tolerance * x.abs() + rel_tolerance;
        let tolerance2 = tolerance * 2.0;
        if !((x - midpoint).abs() > tolerance2 - (right - left) * 0.5) {
            break;
        }
        if e.abs() > tolerance {
            let xw = x - w;
            let fxw = fx - fw;
            let xv = x - v;
            let fxv = fx - fv;
            let xwfxv = xw * fxv;
            let xvfxw = xv * fxw;
            let mut p = xv * xvfxw - xw * xwfxv;
            let mut q = (xvfxw - xwfxv) * 2.0;
            if q > 0.0 {
                p = -p;
            } else {
                q = -q;
            }
            let previous_step = e;
            e = d;
            if p.abs() < (q * previous_step * 0.5).abs()
                && p > q * (left - x)
                && p < q * (right - x)
            {
                d = p / q;
                let u = x + d;
                if u - left < tolerance2 || right - u < tolerance2 {
                    d = if x < midpoint { tolerance } else { -tolerance };
                }
            } else {
                e = (if x < midpoint { right } else { left }) - x;
                d = c * e;
            }
        } else {
            e = (if x < midpoint { right } else { left }) - x;
            d = c * e;
        }
        let u = x + if d.abs() >= tolerance {
            d
        } else if d > 0.0 {
            tolerance
        } else {
            -tolerance
        };
        let fu = curve_distance(kind, points, query, f64::from(u)) as f32;
        if fu <= fx {
            if u < x {
                right = x;
            } else {
                left = x;
            }
            v = w;
            fv = fw;
            w = x;
            fw = fx;
            x = u;
            fx = fu;
        } else {
            if u < x {
                left = u;
            } else {
                right = u;
            }
            if fu <= fw || w == x {
                v = w;
                fv = fw;
                w = u;
                fw = fu;
            } else if fu <= fv || v == x || v == w {
                v = u;
                fv = fu;
            }
        }
    }
    f64::from(x)
}

fn curve_distance(kind: CurveKind, points: &[Vec2], query: Vec2, t: f64) -> f64 {
    let point = curve_sample(kind, points, t).0;
    (point[0] - query[0]).powi(2) + (point[1] - query[1]).powi(2)
}

fn curve_sample(kind: CurveKind, points: &[Vec2], t: f64) -> (Vec2, Vec2) {
    match kind {
        CurveKind::Bezier => bezier_sample(points, t),
        CurveKind::Spline => spline_sample(points, t),
    }
}

fn bezier_sample(points: &[Vec2], t: f64) -> (Vec2, Vec2) {
    let degree = points.len() - 1;
    let mut point = [0.0; 2];
    for (index, control) in points.iter().enumerate() {
        let coefficient = binomial(degree, index)
            * (1.0 - t).powi((degree - index) as i32)
            * t.powi(index as i32);
        point[0] += coefficient * control[0];
        point[1] += coefficient * control[1];
    }
    let mut tangent = [0.0; 2];
    if degree > 0 {
        for index in 0..degree {
            let coefficient = degree as f64
                * binomial(degree - 1, index)
                * (1.0 - t).powi((degree - 1 - index) as i32)
                * t.powi(index as i32);
            tangent[0] += coefficient * (points[index + 1][0] - points[index][0]);
            tangent[1] += coefficient * (points[index + 1][1] - points[index][1]);
        }
    }
    (point, tangent)
}

fn binomial(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    (0..k).fold(1.0, |result, index| {
        result * (n - index) as f64 / (index + 1) as f64
    })
}

fn spline_sample(points: &[Vec2], t: f64) -> (Vec2, Vec2) {
    if points.len() == 2 {
        return (
            [
                points[0][0] * (1.0 - t) + points[1][0] * t,
                points[0][1] * (1.0 - t) + points[1][1] * t,
            ],
            [points[1][0] - points[0][0], points[1][1] - points[0][1]],
        );
    }
    let segment = t * (points.len() - 1) as f64;
    let segment_index = segment as usize;
    let p1 = segment_index.min(points.len() - 2);
    let p0 = p1.saturating_sub(1);
    let p2 = (p1 + 1).min(points.len() - 1);
    let p3 = (p2 + 1).min(points.len() - 1);
    // D uses the unclamped segment index here; at t=1 this makes lt=0 and
    // selects the penultimate control point for splines with 3+ points.
    let local = segment - segment_index as f64;
    let local2 = local * local;
    let local3 = local2 * local;
    let mut value = [0.0; 2];
    let mut tangent = [0.0; 2];
    for axis in 0..2 {
        let a = 2.0 * points[p1][axis];
        let b = points[p2][axis] - points[p0][axis];
        let c = 2.0 * points[p0][axis] - 5.0 * points[p1][axis] + 4.0 * points[p2][axis]
            - points[p3][axis];
        let d =
            -points[p0][axis] + 3.0 * points[p1][axis] - 3.0 * points[p2][axis] + points[p3][axis];
        value[axis] = 0.5 * (a + b * local + c * local2 + d * local3);
        tangent[axis] = 0.5 * (b + 2.0 * c * local + 3.0 * d * local2);
    }
    (value, tangent)
}

fn transform_point(matrix: Mat4, point: Vec2) -> Result<Vec2, String> {
    let point = bone_deform::transform_point(matrix, [point[0], point[1], 0.0])?;
    Ok([point[0], point[1]])
}

fn transform_vector(matrix: Mat4, vector: Vec2) -> Vec2 {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1],
    ]
}

fn translation_matrix(offset: Vec2) -> Mat4 {
    [
        [1.0, 0.0, 0.0, offset[0]],
        [0.0, 1.0, 0.0, offset[1]],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::{evaluate, evaluate_with_path_update};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn transform() -> Value {
        json!({"trans":[0.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]})
    }

    fn scene(children: Value) -> Value {
        json!({
            "nodes": {
                "uuid":1, "name":"Root", "type":"Node", "enabled":true,
                "transform":transform(), "children":children
            }
        })
    }

    fn offsets(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
        points.to_vec()
    }

    #[test]
    fn dynamic_meshgroup_snapshots_static_child_mapping_before_post0() {
        let triangle = json!({
            "verts":[0.0,0.0, 10.0,0.0, 0.0,10.0],
            "indices":[0,1,2]
        });
        let model = scene(json!([{
            "uuid":2, "type":"MeshGroup", "enabled":true, "dynamic_deformation":true,
            "transform":transform(), "mesh":triangle, "children":[{
                "uuid":3, "type":"MeshGroup", "enabled":true, "dynamic_deformation":false,
                "transform":transform(), "mesh":triangle, "children":[{
                    "uuid":4, "type":"Part", "enabled":true, "transform":transform(),
                    "mesh":{"verts":[1.0,1.0, 2.0,1.0, 1.0,2.0], "indices":[0,1,2]}
                }]
            }]
        }]));
        let input = BTreeMap::from([
            (2, offsets(&[[1.0, 0.0], [1.0, 0.0], [1.0, 0.0]])),
            (3, offsets(&[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]])),
            (4, offsets(&[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]])),
        ]);

        let result = evaluate(&model, &input).unwrap();

        assert_eq!(result.local_offsets[&4], [[0.0, 0.0]; 3]);
    }

    #[test]
    fn dynamic_grid_snapshots_control_offsets_after_meshgroup_post0() {
        let model = scene(json!([{
            "uuid":2, "type":"MeshGroup", "enabled":true, "dynamic_deformation":true,
            "transform":transform(), "mesh":{
                "verts":[0.0,0.0, 100.0,0.0, 0.0,100.0], "indices":[0,1,2]
            }, "children":[{
                "uuid":3, "type":"GridDeformer", "enabled":true, "dynamic":true,
                "transform":transform(), "grid_axis_x":[1.0,2.0], "grid_axis_y":[1.0,2.0],
                "children":[{
                    "uuid":4, "type":"Part", "enabled":true, "transform":transform(),
                    "mesh":{"verts":[1.5,1.5]}
                }]
            }]
        }]));
        let input = BTreeMap::from([
            (2, offsets(&[[3.0, 0.0], [3.0, 0.0], [3.0, 0.0]])),
            (3, offsets(&[[0.0, 0.0]; 4])),
            (4, offsets(&[[0.0, 0.0]])),
        ]);

        let result = evaluate(&model, &input).unwrap();

        assert!((result.local_offsets[&4][0][0] - 3.0).abs() < 1e-6);
        assert!(result.local_offsets[&4][0][1].abs() < 1e-6);
    }

    #[test]
    fn child_pin_uses_parent_deformation_at_preprocess_time() {
        let model = scene(json!([{
            "uuid":2, "type":"MeshGroup", "enabled":true, "dynamic_deformation":true,
            "transform":transform(), "mesh":{
                "verts":[0.0,0.0, 10.0,0.0, 0.0,10.0], "indices":[0,1,2]
            }, "children":[{
                "uuid":3, "type":"Part", "enabled":true, "transform":transform(),
                "mesh":{"verts":[0.0,0.0, 10.0,0.0, 0.0,10.0], "indices":[0,1,2]},
                "children":[{
                    "uuid":4, "type":"Node", "enabled":true, "pinToMesh":true,
                    "transform":{"trans":[1.0,1.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]}
                }]
            }]
        }]));
        let input = BTreeMap::from([
            (2, offsets(&[[3.0, 0.0], [3.0, 0.0], [3.0, 0.0]])),
            (3, offsets(&[[0.0, 0.0]; 3])),
            (4, offsets(&[[0.0, 0.0]])),
        ]);

        let result = evaluate(&model, &input).unwrap();

        let world = result.world_matrices[&4];
        assert!((world[0][3] - 1.0).abs() < 1e-6);
        assert!((world[1][3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nested_dynamic_grids_sample_live_offsets_within_post1() {
        let model = scene(json!([{
            "uuid":2, "type":"GridDeformer", "dynamic":true, "transform":transform(),
            "grid_axis_x":[0.0,10.0], "grid_axis_y":[0.0,10.0], "children":[{
                "uuid":3, "type":"GridDeformer", "dynamic":true, "transform":transform(),
                "grid_axis_x":[0.0,10.0], "grid_axis_y":[0.0,10.0], "children":[{
                    "uuid":4, "type":"Part", "transform":transform(), "mesh":{"verts":[1.0,1.0]}
                }]
            }]
        }]));
        let result = evaluate(&model, &BTreeMap::from([(2, vec![[3.0,0.0];4])])).unwrap();
        assert_eq!(result.local_offsets[&3], vec![[3.0,0.0];4]);
        assert!((result.local_offsets[&4][0][0] - 3.0).abs() < 1e-6);
        let cancelled = evaluate(&model, &BTreeMap::from([
            (2, vec![[3.0,0.0];4]), (3, vec![[-3.0,0.0];4]),
        ])).unwrap();
        assert_eq!(cancelled.local_offsets[&3], vec![[0.0,0.0];4]);
        assert!(cancelled.local_offsets.get(&4).is_none_or(|offsets|
            offsets.iter().flatten().all(|value| value.abs() < 1e-6)));
    }

    #[test]
    fn composite_filters_move_origin_without_replacing_quad_offsets() {
        for dynamic in [false, true] {
            let model = scene(json!([{
                "uuid":2, "type":"GridDeformer", "dynamic":dynamic, "transform":transform(),
                "grid_axis_x":[0.0,10.0], "grid_axis_y":[0.0,10.0], "children":[{
                    "uuid":3, "type":"Composite", "propagate_meshgroup":false,
                    "transform":{"trans":[2.0,2.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                    "children":[{"uuid":4,"type":"Part","transform":transform(),"mesh":{"verts":[0.0,0.0]}}]
                }, {
                    "uuid":5,"type":"Node",
                    "transform":{"trans":[1.0,1.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]}
                }]
            }]));
            let quad_offsets = vec![[1.0,0.0],[2.0,0.0],[3.0,0.0],[4.0,0.0]];
            let result = evaluate(&model, &BTreeMap::from([
                (2, vec![[2.0,0.0];4]), (3, quad_offsets.clone()),
            ])).unwrap();
            assert!((result.world_matrices[&3][0][3] - 4.0).abs() < 1e-6);
            assert!((result.world_matrices[&4][0][3] - 4.0).abs() < 1e-6);
            assert!((result.world_matrices[&5][0][3] - 3.0).abs() < 1e-6);
            assert_eq!(result.local_offsets[&3], quad_offsets);
        }
    }

    #[test]
    fn locked_child_uses_original_root_transform_and_rotation() {
        let mut model = scene(json!([{
            "uuid":2,"type":"PathDeformer","lockToRoot":true,"vertices":[0.0,0.0,0.0,10.0],
            "transform":{"trans":[1.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]}
        }]));
        let mut original_root = model["nodes"].clone();
        original_root["transform"]["trans"] = json!([2.0,0.0,0.0]);
        original_root["transform"]["rot"] = json!([0.0,0.0,0.1]);
        model["nodes"]["transform"]["trans"] = json!([5.0,0.0,0.0]);
        model["nodes"]["transform"]["rot"] = json!([0.0,0.0,0.35]);
        let mut rotation = None;
        let result = evaluate_with_path_update(&model, &original_root, &BTreeMap::new(),
            &mut |_, _, _, world_z, _| { rotation = Some(world_z); Ok(None) }).unwrap();
        assert!((result.world_matrices[&2][0][3] - 2.9950041653).abs() < 1e-6);
        assert!((result.world_matrices[&2][1][3] - 0.0998334166).abs() < 1e-6);
        assert!((rotation.unwrap() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn path_degenerate_curve_uses_d_brent_tie_endpoint() {
        let model = scene(json!([{
            "uuid":2, "type":"PathDeformer", "enabled":true, "curve_type":"Spline",
            "transform":transform(), "vertices":[0.0,0.0, 0.0,0.0], "children":[{
                "uuid":3, "type":"Part", "enabled":true, "transform":transform(),
                "mesh":{"verts":[0.0,1.0]}
            }]
        }]));
        let input = BTreeMap::from([
            (2, offsets(&[[0.0, 0.0], [10.0, 0.0]])),
            (3, offsets(&[[0.0, 0.0]])),
        ]);

        let result = evaluate(&model, &input).unwrap();

        assert!(
            result.local_offsets[&3][0][0] > 9.9,
            "{:?}",
            result.local_offsets[&3]
        );
        assert!((result.local_offsets[&3][0][1]).abs() < 1e-6);
    }

    #[test]
    fn path_filter_skips_nested_path_but_reaches_drawable_descendant() {
        let model = scene(json!([{
            "uuid":2, "type":"PathDeformer", "enabled":true, "curve_type":"Bezier",
            "transform":transform(), "vertices":[0.0,0.0, 10.0,0.0], "children":[{
                "uuid":3, "type":"PathDeformer", "enabled":true, "curve_type":"Bezier",
                "transform":transform(), "vertices":[5.0,0.0, 5.0,10.0], "children":[{
                    "uuid":4, "type":"Part", "enabled":true, "transform":transform(),
                    "mesh":{"verts":[5.0,3.0]}
                }]
            }]
        }]));
        let input = BTreeMap::from([
            (2, offsets(&[[2.0, 0.0], [2.0, 0.0]])),
            (3, offsets(&[[0.0, 0.0], [0.0, 0.0]])),
            (4, offsets(&[[0.0, 0.0]])),
        ]);

        let result = evaluate(&model, &input).unwrap();

        assert_eq!(result.local_offsets[&3], [[0.0, 0.0]; 2]);
        assert!((result.local_offsets[&4][0][0] - 2.0).abs() < 1e-6);
        assert!(result.local_offsets[&4][0][1].abs() < 1e-6);
    }
}
