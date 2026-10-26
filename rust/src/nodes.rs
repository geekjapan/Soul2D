use crate::{finite_number, fresh_resource_id, valid_id, validate_model};
use serde_json::{json, Value};
use std::collections::HashSet;

type Matrix = [[f64; 3]; 3];

pub(crate) const SUPPORTED_CREATE_TYPES: &[&str] = &[
    "Node",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Camera",
    "Mask",
    "PathDeformer",
    "GridDeformer",
    "DepthRigRoot",
    "DepthBone",
    "SimplePhysics",
];

const CHILD_ACCEPTING_PARENT_TYPES: &[&str] = &[
    "Node",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Camera",
    "PathDeformer",
    "GridDeformer",
    "DepthRigRoot",
    "DepthBone",
    "Part",
    "SimplePhysics",
];

const CLIPBOARD_TYPES: &[&str] = &[
    "Node",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Camera",
    "Mask",
    "Part",
    "PathDeformer",
    "GridDeformer",
    "DepthRigRoot",
    "DepthBone",
];

const CONVERT_TYPES: &[&str] = &[
    "Node",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Camera",
    "Mask",
    "PathDeformer",
    "GridDeformer",
];

const COMPOSITE_FIELDS: &[&str] = &["opacity", "blend_mode", "tint", "screenTint"];

#[derive(Clone, Debug)]
pub struct ClipboardEntry {
    pub source_id: u32,
    pub node: Value,
    world: Matrix,
    absolute_z: f64,
}

impl ClipboardEntry {
    pub fn summary(&self) -> Value {
        json!({
            "uuid": self.node.get("uuid").cloned().unwrap_or(Value::Null),
            "name": self.node.get("name").cloned().unwrap_or(Value::Null),
            "type": self.node.get("type").and_then(Value::as_str).unwrap_or("Node"),
            "source": self.source_id,
        })
    }
}

pub fn clipboard_summaries(entries: &[ClipboardEntry]) -> Value {
    Value::Array(entries.iter().map(ClipboardEntry::summary).collect())
}

pub fn list_nodes(model: &Value) -> Value {
    let mut items = Vec::new();
    if let Some(root) = model.get("nodes") {
        collect_nodes(root, None, &mut items);
    }
    Value::Array(items)
}

fn is_root(model: &Value, id: u32) -> bool {
    model.get("nodes").and_then(|node| node.get("uuid")).and_then(Value::as_u64) == Some(u64::from(id))
}

fn collect_nodes(node: &Value, parent: Option<u32>, items: &mut Vec<Value>) {
    let Some(object) = node.as_object() else {
        return;
    };
    let id = object
        .get("uuid")
        .and_then(Value::as_u64)
        .and_then(valid_id);
    if let Some(id) = id {
        items.push(json!({
            "uuid": id,
            "name": object.get("name").cloned().unwrap_or(Value::Null),
            "type": object.get("type").and_then(Value::as_str).unwrap_or("Node"),
            "parent": parent,
            "enabled": object.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        }));
    }
    if let Some(children) = object.get("children").and_then(Value::as_array) {
        for child in children {
            collect_nodes(child, id, items);
        }
    }
}

/// Rest pose carried by `AddDepthBone`. Absent fields use `ExDepthBone` defaults.
#[derive(Clone, Debug)]
pub struct BoneRest {
    pub bone_id: Option<String>,
    pub rest_head: [f64; 3],
    pub rest_tail: [f64; 3],
    pub rest_roll: f64,
}

pub fn bone_rest_from_request(request: &Value) -> Result<Option<BoneRest>, String> {
    let bone_id = match request.get("boneId") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err("boneId must be a string".into()),
    };
    let rest_head = optional_vec3(request, "restHead")?;
    let rest_tail = optional_vec3(request, "restTail")?;
    let rest_roll = match request.get("restRoll") {
        None | Some(Value::Null) => None,
        Some(value) => Some(finite_number(value, "restRoll")?),
    };
    if bone_id.is_none() && rest_head.is_none() && rest_tail.is_none() && rest_roll.is_none() {
        return Ok(None);
    }
    let mut rest = BoneRest::default();
    rest.bone_id = bone_id;
    if let Some(head) = rest_head {
        rest.rest_head = head;
    }
    if let Some(tail) = rest_tail {
        rest.rest_tail = tail;
    }
    if let Some(roll) = rest_roll {
        rest.rest_roll = roll;
    }
    Ok(Some(rest))
}

fn optional_vec3(request: &Value, key: &str) -> Result<Option<[f64; 3]>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(parse_vec3(value, key)?)),
    }
}

impl Default for BoneRest {
    fn default() -> Self {
        Self {
            bone_id: None,
            rest_head: [0.0, 0.0, 0.0],
            rest_tail: [0.0, 100.0, 0.0],
            rest_roll: 0.0,
        }
    }
}

pub fn create_nodes(
    model: &Value,
    parent_ids: &[u32],
    class_name: &str,
    suffix: Option<&str>,
    explicit_name: Option<&str>,
    bone: Option<&BoneRest>,
) -> Result<(Value, Vec<Value>), String> {
    create_nodes_inner(
        model,
        parent_ids,
        class_name,
        suffix,
        explicit_name,
        bone,
        false,
    )
}

/// Standard-skeleton bones keep D's names even when another node already uses them.
pub(crate) fn create_duplicate_named_nodes(
    model: &Value,
    parent_ids: &[u32],
    class_name: &str,
    suffix: Option<&str>,
    explicit_name: Option<&str>,
    bone: Option<&BoneRest>,
) -> Result<(Value, Vec<Value>), String> {
    create_nodes_inner(
        model,
        parent_ids,
        class_name,
        suffix,
        explicit_name,
        bone,
        true,
    )
}

fn create_nodes_inner(
    model: &Value,
    parent_ids: &[u32],
    class_name: &str,
    suffix: Option<&str>,
    explicit_name: Option<&str>,
    bone: Option<&BoneRest>,
    allow_duplicate_name: bool,
) -> Result<(Value, Vec<Value>), String> {
    if bone.is_some() && class_name != "DepthBone" {
        return Err("rest pose is only valid for DepthBone".into());
    }
    if !SUPPORTED_CREATE_TYPES.contains(&class_name) {
        return Err(format!("unsupported node class for Rust Core mode: {class_name}"));
    }
    if parent_ids.is_empty() {
        return Err("at least one parent node UUID is required".into());
    }
    let mut candidate = model.clone();
    let mut created = Vec::new();
    for parent_id in parent_ids {
        let parent = find_node(&candidate, *parent_id)
            .ok_or_else(|| format!("parent node {parent_id} was not found"))?;
        let parent_name = parent
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Root");
        let name = explicit_name.map(str::to_owned).unwrap_or_else(|| {
            if let Some(suffix) = suffix.filter(|value| !value.is_empty()) {
                format!("{parent_name}{suffix}")
            } else {
                default_node_name(&candidate, class_name)
            }
        });
        let mut name = name;
        if class_name == "DepthBone" {
            if let Some(bone_id) = bone.and_then(|rest| rest.bone_id.as_deref()) {
                if explicit_name.is_none() {
                    name = bone_id.to_owned();
                }
            }
        }
        if !allow_duplicate_name {
            check_node_name(&candidate, &name, None)?;
        }
        let id = fresh_resource_id(&candidate)?;
        let mut node = default_node(id, class_name, &name);
        stamp_created_rig(&mut node, class_name, parent, bone)?;
        insert_child(&mut candidate, *parent_id, node.clone())?;
        created.push(node);
    }
    validate_model(&candidate)?;
    Ok((candidate, created))
}

pub fn insert_nodes(
    model: &Value,
    node_ids: &[u32],
    class_name: &str,
    suffix: Option<&str>,
) -> Result<(Value, Vec<Value>), String> {
    // B1 creates rig nodes. Wrapping an existing node in a rig stays fail-closed.
    // P5-P1 creates SimplePhysics. Inserting one above another node stays fail-closed
    // (pinned by physics::tests::invalid_settings_leave_the_model_and_revision_unchanged).
    if matches!(class_name, "DepthBone" | "DepthRigRoot" | "SimplePhysics") {
        return Err(format!("unsupported node class for Rust Core mode: {class_name}"));
    }
    if !SUPPORTED_CREATE_TYPES.contains(&class_name) {
        return Err(format!("unsupported node class for Rust Core mode: {class_name}"));
    }
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let selected = unique_top_level(model, node_ids);
    if selected.is_empty() {
        return Err("no nodes were found to wrap".into());
    }
    let mut candidate = model.clone();
    let mut created = Vec::new();
    for id in selected {
        if is_root(model, id) {
            return Err("the root node cannot be wrapped in Rust Core mode".into());
        }
        let parent_id = find_parent_id(&candidate, id)?.ok_or_else(|| format!("node {id} has no parent"))?;
        let child_name = find_node(&candidate, id)
            .and_then(|node| node.get("name").and_then(Value::as_str))
            .unwrap_or("Node")
            .to_owned();
        let translation = find_node(&candidate, id)
            .and_then(|node| node.get("transform"))
            .and_then(|transform| transform.get("trans"))
            .cloned()
            .unwrap_or_else(|| json!([0.0, 0.0, 0.0]));
        let name = if let Some(suffix) = suffix.filter(|value| !value.is_empty()) {
            format!("{child_name}{suffix}")
        } else {
            default_node_name(&candidate, class_name)
        };
        check_node_name(&candidate, &name, None)?;
        let new_id = fresh_resource_id(&candidate)?;
        let mut node = default_node(new_id, class_name, &name);
        node.as_object_mut()
            .ok_or("node must be an object")?
            .entry("transform")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("node transform must be an object")?
            .insert("trans".into(), translation);
        insert_child_at(&mut candidate, parent_id, 0, node)?;
        candidate = move_nodes(&candidate, &[id], new_id, Some(0))?;
        created.push(
            find_node(&candidate, new_id)
                .cloned()
                .ok_or_else(|| format!("inserted node {new_id} was not found"))?,
        );
    }
    Ok((candidate, created))
}

pub fn rename_node(model: &Value, id: u32, name: &str) -> Result<Value, String> {
    if name.is_empty() {
        return Err("node name is empty".into());
    }
    if is_root(model, id) {
        return Err("the root node cannot be renamed in Rust Core mode".into());
    }
    let mut candidate = model.clone();
    check_node_name(&candidate, name, Some(id))?;
    let node = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    ensure_transformable_node(node, id)?;
    node.as_object_mut()
        .ok_or("node must be an object")?
        .insert("name".into(), Value::String(name.to_owned()));
    validate_model(&candidate)?;
    Ok(candidate)
}

/// Rename several nodes at once, with the checks of `rename_node`. Each name is judged against the finished
/// document, so a node may take a name another listed node gives up.
pub fn rename_nodes(model: &Value, renames: &[(u32, String)]) -> Result<Value, String> {
    let mut candidate = model.clone();
    for (id, name) in renames {
        let refusal = |why: &str| format!("node {id} cannot be named {:?}: {why}", crate::mcp::echoed([name.as_str()]));
        if name.is_empty() {
            return Err(refusal("the name would be empty"));
        }
        if is_root(model, *id) {
            return Err(refusal("the root node cannot be renamed in Rust Core mode"));
        }
        let node = find_node_mut(&mut candidate, *id).ok_or_else(|| format!("node {id} was not found"))?;
        ensure_transformable_node(node, *id).map_err(|why| refusal(&why))?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("name".into(), Value::String(name.clone()));
    }
    for (id, name) in renames {
        check_node_name(&candidate, name, Some(*id)).map_err(|why| {
            format!("node {id} cannot be named {:?}: {why}", crate::mcp::echoed([name.as_str()]))
        })?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub enum NodeEnabledChange {
    Set(bool),
    Toggle,
}

pub fn change_enabled(
    model: &Value,
    id: u32,
    change: NodeEnabledChange,
) -> Result<(Value, bool), String> {
    let mut candidate = model.clone();
    let next = {
        let node = find_node_mut(&mut candidate, id)
            .ok_or_else(|| format!("node {id} was not found"))?;
        let current = node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let next = match change {
            NodeEnabledChange::Set(value) => value,
            NodeEnabledChange::Toggle => !current,
        };
        if next != current {
            node.as_object_mut()
                .ok_or("node must be an object")?
                .insert("enabled".into(), Value::Bool(next));
        }
        next
    };
    if candidate != *model {
        validate_model(&candidate)?;
    }
    Ok((candidate, next))
}

pub fn move_nodes(
    model: &Value,
    node_ids: &[u32],
    parent_id: u32,
    index: Option<usize>,
) -> Result<Value, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    if node_ids.iter().any(|id| is_root(model, *id)) {
        return Err("the root node cannot be moved in Rust Core mode".into());
    }
    find_node(model, parent_id).ok_or_else(|| format!("parent node {parent_id} was not found"))?;
    let parent_type = find_node(model, parent_id)
        .and_then(|node| node.get("type").and_then(Value::as_str))
        .unwrap_or("Node");
    if !is_root(model, parent_id) && !CHILD_ACCEPTING_PARENT_TYPES.contains(&parent_type) {
        return Err(format!(
            "parent node {parent_id} cannot accept children in Rust Core mode"
        ));
    }
    let mut moving = filter_top_level_selection(model, node_ids);
    if moving.is_empty() {
        return Err("no movable nodes were found".into());
    }
    // Tree order, not UUID order: a multi-node move keeps the order the rows have on screen.
    let order = tree_order(model);
    moving.sort_by_key(|id| order.iter().position(|other| other == id).unwrap_or(usize::MAX));
    for id in &moving {
        if *id == parent_id || is_descendant(model, *id, parent_id) {
            return Err(format!(
                "node {id} cannot be reparented into one of its descendants"
            ));
        }
        find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
    }

    let mut candidate = model.clone();
    // A rig node keeps its 3D local rotation and scale. D NodeMoveAction
    // rewrites only translation, and that translation includes Z. The planar
    // matrix rejects X/Y rotation and stores Z as zero, so it is used only
    // for nodes that are not DepthBone or DepthRigRoot.
    let moves_rig = moving.iter().any(|id| find_node(model, *id).is_some_and(is_rig_node));
    let moves_planar = moving
        .iter()
        .any(|id| find_node(model, *id).is_some_and(|node| !is_rig_node(node)));
    let new_parent_world = if moves_planar {
        Some(world_matrix(&candidate, parent_id)?)
    } else {
        None
    };
    let new_parent_rig_world = if moves_rig {
        Some(crate::bone_deform::world_matrix(&candidate, parent_id)?)
    } else {
        None
    };
    let new_parent_z = accumulated_zsort_inclusive(&candidate, parent_id)?;
    let mut extracted = Vec::new();
    for id in &moving {
        let node_ref = find_node(&candidate, *id).ok_or_else(|| format!("node {id} was not found"))?;
        let parent_id = find_parent_id(&candidate, *id)?.ok_or_else(|| format!("node {id} has no parent"))?;
        let old_parent_z = accumulated_zsort_inclusive(&candidate, parent_id)?;
        let node_z = node_zsort(node_ref)?;
        let absolute_z = old_parent_z + node_z;
        let world = if is_rig_node(node_ref) {
            MovedWorld::Rig(crate::bone_deform::world_matrix(&candidate, *id)?)
        } else {
            MovedWorld::Planar(world_matrix(&candidate, *id)?)
        };
        let node = remove_node(&mut candidate, *id)?.ok_or_else(|| format!("node {id} was not found"))?;
        extracted.push((node, world, absolute_z));
    }

    let insert_at = index.unwrap_or_else(|| {
        find_node(&candidate, parent_id)
            .and_then(|node| node.get("children"))
            .and_then(Value::as_array)
            .map(|children| children.len())
            .unwrap_or(0)
    });
    let mut offset = insert_at;
    for (mut node, world, absolute_z) in extracted {
        if !node
            .get("lockToRoot")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            match world {
                MovedWorld::Planar(world) => {
                    let parent_world = new_parent_world.ok_or("parent world matrix is missing")?;
                    set_local_transform_from_world(&mut node, parent_world, world)?;
                }
                MovedWorld::Rig(world) => {
                    let parent_world = new_parent_rig_world.ok_or("parent world matrix is missing")?;
                    set_rig_translation_from_world(&mut node, parent_world, world)?;
                }
            }
        }
        let new_relative_z = absolute_z - new_parent_z;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("zsort".into(), json!(new_relative_z));
        insert_child_at(&mut candidate, parent_id, offset, node)?;
        offset += 1;
    }
    ensure_finite_tree(candidate.get("nodes").ok_or("model is missing nodes")?)?;
    validate_model(&candidate)?;
    Ok(candidate)
}

pub fn delete_nodes(model: &Value, node_ids: &[u32]) -> Result<Value, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    if node_ids.iter().any(|id| is_root(model, *id)) {
        return Err("the root node cannot be deleted in Rust Core mode".into());
    }
    let removing = filter_top_level_selection(model, node_ids);
    if removing.is_empty() {
        return Err("no deletable nodes were found".into());
    }
    for id in &removing {
        if find_node(model, *id).is_none() {
            return Err(format!("node {id} was not found"));
        }
    }
    let mut candidate = model.clone();
    let mut removed = HashSet::new();
    for id in removing {
        let subtree = remove_node(&mut candidate, id)?.ok_or_else(|| format!("node {id} was not found"))?;
        collect_node_ids(&subtree, &mut removed);
    }
    prune_parameter_bindings(&mut candidate, &removed);
    crate::bindings::drop_deleted_references(&mut candidate, &removed)?;
    crate::weld::prune_deleted_links(&mut candidate, &removed);
    if let Some(root) = candidate.get("nodes") {
        ensure_finite_tree(root)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub fn copy_nodes(model: &Value, node_ids: &[u32]) -> Result<Vec<ClipboardEntry>, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let selected = unique_top_level(model, node_ids);
    if selected.is_empty() {
        return Err("no copyable nodes were found".into());
    }
    let mut used = occupied_ids(model);
    // One map for the whole selection, matching `ngRemapCopiedDepthRigReferences`.
    let mut uuid_map = std::collections::HashMap::new();
    let mut copies = Vec::with_capacity(selected.len());
    for id in selected {
        if is_root(model, id) {
            return Err("the root node cannot be copied in Rust Core mode".into());
        }
        let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
        ensure_clipboard_tree(node)?;
        let parent_id = find_parent_id(model, id)?.ok_or_else(|| format!("node {id} has no parent"))?;
        let world = world_matrix(model, id)?;
        let absolute_z = accumulated_zsort_inclusive(model, parent_id)? + node_zsort(node)?;
        let mut clone = node.clone();
        // Names follow D dup. UUIDs here are only a preview; paste remaps them
        // against the live model so later creates and undos cannot collide.
        rename_cloned_subtree(&mut clone)?;
        remap_uuid_tree(&mut clone, &mut used, &mut uuid_map)?;
        copies.push(ClipboardEntry {
            source_id: id,
            node: clone,
            world,
            absolute_z,
        });
    }
    for entry in &mut copies {
        crate::bindings::remap_copied_bindings(&mut entry.node, &uuid_map)?;
        crate::weld::remap_copied_links(&mut entry.node, &uuid_map);
    }
    Ok(copies)
}

pub fn paste_nodes(
    model: &Value,
    clipboard: &[ClipboardEntry],
    parent_id: u32,
    index: Option<usize>,
) -> Result<(Value, Vec<Value>), String> {
    if clipboard.is_empty() {
        return Err("Clipboard empty".into());
    }
    find_node(model, parent_id).ok_or_else(|| format!("parent node {parent_id} was not found"))?;
    let mut candidate = model.clone();
    let parent_world = world_matrix(&candidate, parent_id)?;
    let parent_z = accumulated_zsort_inclusive(&candidate, parent_id)?;
    let index = index.unwrap_or(0);
    let mut created = Vec::with_capacity(clipboard.len());
    let mut used = occupied_ids(&candidate);
    let mut uuid_map = std::collections::HashMap::new();
    let mut pending = Vec::with_capacity(clipboard.len());
    for entry in clipboard {
        let mut node = entry.node.clone();
        remap_uuid_tree(&mut node, &mut used, &mut uuid_map)?;
        pending.push((node, entry.world, entry.absolute_z));
    }
    for (node, _, _) in &mut pending {
        crate::bindings::remap_copied_bindings(node, &uuid_map)?;
        crate::weld::remap_copied_links(node, &uuid_map);
    }
    for (mut node, world, absolute_z) in pending {
        place_pasted_node(&mut node, parent_world, world, absolute_z, parent_z)?;
        insert_child_at(&mut candidate, parent_id, index, node.clone())?;
        created.insert(0, node);
    }
    validate_model(&candidate)?;
    Ok((candidate, created))
}

pub fn convert_nodes(model: &Value, node_ids: &[u32], to_type: &str) -> Result<(Value, Vec<Value>), String> {
    if !CONVERT_TYPES.contains(&to_type) && to_type != "Part" {
        return Err(format!("unsupported node class for Rust Core mode: {to_type}"));
    }
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let selected = unique_ids(node_ids);
    if selected.is_empty() {
        return Err("no convertible nodes were found".into());
    }
    let mut candidate = model.clone();
    let mut converted = Vec::new();
    for id in selected {
        if is_root(model, id) {
            return Err("the root node cannot be converted in Rust Core mode".into());
        }
        let (current_type, old_points, blocked) = {
            let current = find_node(&candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
            let current_type = current
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("Node")
                .to_owned();
            (
                current_type,
                crate::deformers::node_geometry_points(current),
                has_blocked_convert_payload(current, to_type),
            )
        };
        if blocked {
            return Err(format!(
                "node {id} conversion requires a mesh or mask payload and is outside Rust Core mode"
            ));
        }
        if to_type == "Part"
            && !matches!(
                current_type.as_str(),
                "Node" | "Part" | "Mask" | "DynamicComposite"
            )
        {
            return Err(format!("{current_type} cannot convert to Part"));
        }
        if matches!(current_type.as_str(), "Part" | "Mask" | "DynamicComposite") {
            let current = find_node(&candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
            crate::deformers::parse_part_mesh(current)?;
        }
        if !CONVERT_TYPES.contains(&current_type.as_str())
            && !(current_type == "Part" && to_type == "Part")
        {
            return Err(format!("unsupported node class for Rust Core mode: {current_type}"));
        }
        let refresh = crate::dirty::refreshes_bound_target(&candidate, id);
        if current_type == to_type {
            continue;
        }
        let converted_node = {
            let node = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
            apply_converted_type(node, to_type)?;
            node.clone()
        };
        let new_points = crate::deformers::node_geometry_points(&converted_node);
        if matches!(to_type, "GridDeformer" | "PathDeformer") {
            crate::bindings::reject_vertex_weight_resize(&model, id, new_points.len())?;
        }
        if !refresh {
            crate::deformers::retarget_undriven_convert(
                &mut candidate,
                id,
                to_type,
                &old_points,
                &new_points,
            )?;
        }
        // P2-E6 keeps a dropped rig row dropped. The parameter "deform" row
        // stays on this UUID; only a later rig.add_source regenerates it.
        crate::bindings::retarget_converted_node(&mut candidate, id, to_type)?;
        converted.push(converted_node);
    }
    if converted.is_empty() {
        return Err("No nodes converted".into());
    }
    validate_model(&candidate)?;
    Ok((candidate, converted))
}

fn place_pasted_node(
    node: &mut Value,
    parent_world: Matrix,
    world: Matrix,
    absolute_z: f64,
    parent_z: f64,
) -> Result<(), String> {
    let locked = node.get("lockToRoot").and_then(Value::as_bool).unwrap_or(false);
    if !locked {
        let id = node.get("uuid").and_then(Value::as_u64).unwrap_or(0);
        let current = node_local_matrix(node, id)?;
        let desired = multiply(invert(parent_world)?, world);
        if !matrices_close(current, desired) {
            let translation_z = node
                .get("transform")
                .and_then(|transform| transform.get("trans"))
                .and_then(Value::as_array)
                .and_then(|values| values.get(2))
                .cloned();
            set_local_transform_from_world(node, parent_world, world)?;
            if let Some(translation_z) = translation_z {
                if let Some(translation) = node
                    .get_mut("transform")
                    .and_then(|transform| transform.get_mut("trans"))
                    .and_then(Value::as_array_mut)
                {
                    if translation.len() >= 3 {
                        translation[2] = translation_z;
                    }
                }
            }
        }
    }
    let relative_z = absolute_z - parent_z;
    if (node_zsort(node)? - relative_z).abs() > 1e-9 {
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("zsort".into(), json!(relative_z));
    }
    Ok(())
}

fn apply_converted_type(node: &mut Value, to_type: &str) -> Result<(), String> {
    let from_type = node
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Node")
        .to_owned();
    let object = node.as_object_mut().ok_or("node must be an object")?;
    object.insert("type".into(), Value::String(to_type.to_owned()));
    let composite_like = matches!(
        to_type,
        "Composite" | "MeshGroup" | "DynamicComposite" | "Mask" | "Part"
    );
    if !composite_like {
        for key in COMPOSITE_FIELDS {
            object.remove(*key);
        }
    }
    if !BLENDED_TYPES.contains(&to_type) {
        object.remove("alphaBlend");
    }
    if to_type == "Camera" {
        object
            .entry("viewport".to_owned())
            .or_insert_with(|| json!([1920.0, 1080.0]));
    } else {
        object.remove("viewport");
    }
    if composite_like {
        object.entry("opacity".to_owned()).or_insert_with(|| json!(1.0));
        object
            .entry("blend_mode".to_owned())
            .or_insert_with(|| json!("Normal"));
        object
            .entry("tint".to_owned())
            .or_insert_with(|| json!([1.0, 1.0, 1.0]));
        object
            .entry("screenTint".to_owned())
            .or_insert_with(|| json!([0.0, 0.0, 0.0]));
    }
    crate::deformers::apply_conversion(node, &from_type, to_type)?;
    Ok(())
}

fn has_blocked_convert_payload(node: &Value, to_type: &str) -> bool {
    if to_type == "Part" {
        return crate::deformers::has_blocked_part_convert_payload(node);
    }
    crate::deformers::has_blocked_deformer_payload(node, to_type)
}

fn ensure_clipboard_tree(node: &Value) -> Result<(), String> {
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if !CLIPBOARD_TYPES.contains(&node_type) {
        return Err(format!("unsupported node class for Rust Core mode: {node_type}"));
    }
    if node
        .get("masks")
        .and_then(Value::as_array)
        .is_some_and(|masks| !masks.is_empty())
    {
        return Err("mask bindings are outside Rust Core mode".into());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            ensure_clipboard_tree(child)?;
        }
    }
    Ok(())
}

fn rename_cloned_subtree(node: &mut Value) -> Result<(), String> {
    let object = node.as_object_mut().ok_or("node must be an object")?;
    if let Some(name) = object.get("name").and_then(Value::as_str).map(str::to_owned) {
        object.insert("name".into(), Value::String(format!("{name}'")));
    }
    let child_count = object
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.len())
        .unwrap_or(0);
    for index in 0..child_count {
        let child = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
            .ok_or("node children must be an array")?;
        rename_cloned_subtree(child)?;
    }
    Ok(())
}

fn remap_uuid_tree(
    node: &mut Value,
    used: &mut HashSet<u32>,
    uuid_map: &mut std::collections::HashMap<u32, u32>,
) -> Result<(), String> {
    let old_id = node
        .get("uuid")
        .and_then(Value::as_u64)
        .and_then(valid_id)
        .ok_or("node.uuid must be an unsigned integer")?;
    let id = allocate_id(used)?;
    uuid_map.insert(old_id, id);
    let object = node.as_object_mut().ok_or("node must be an object")?;
    object.insert("uuid".into(), json!(id));
    let child_count = object
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.len())
        .unwrap_or(0);
    for index in 0..child_count {
        let child = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
            .ok_or("node children must be an array")?;
        remap_uuid_tree(child, used, uuid_map)?;
    }
    Ok(())
}

fn unique_ids(ids: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

fn unique_top_level(model: &Value, ids: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    filter_top_level_selection(model, ids)
        .into_iter()
        .filter(|id| seen.insert(*id))
        .collect()
}

fn occupied_ids(model: &Value) -> HashSet<u32> {
    let mut ids = HashSet::new();
    if let Some(root) = model.get("nodes") {
        collect_node_ids(root, &mut ids);
    }
    fn parameters(list: &[Value], ids: &mut HashSet<u32>) {
        for item in list {
            if let Some(id) = item
                .get("uuid")
                .or_else(|| item.get("groupUUID"))
                .and_then(Value::as_u64)
                .and_then(valid_id)
            {
                ids.insert(id);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                parameters(children, ids);
            }
        }
    }
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        parameters(list, &mut ids);
    }
    ids
}

fn allocate_id(used: &mut HashSet<u32>) -> Result<u32, String> {
    let id = (1..u32::MAX)
        .find(|id| !used.contains(id))
        .ok_or("no unused resource UUID remains")?;
    used.insert(id);
    Ok(id)
}

fn matrices_close(left: Matrix, right: Matrix) -> bool {
    left.iter().zip(right).all(|(left_row, right_row)| {
        left_row
            .iter()
            .zip(right_row)
            .all(|(left, right)| (left - right).abs() <= 1e-9)
    })
}

pub fn transform_node(model: &Value, id: u32, request: &Value) -> Result<Value, String> {
    let mut candidate = model.clone();
    let node = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    ensure_transformable_node(node, id)?;
    if let Some(value) = request.get("zsort") {
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert(
                "zsort".into(),
                Value::from(finite_number(value, "zsort")?),
            );
    }
    let transform = node
        .as_object_mut()
        .ok_or("node must be an object")?
        .entry("transform")
        .or_insert_with(|| json!({ "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0] }));
    let transform = transform
        .as_object_mut()
        .ok_or("node transform must be an object")?;
    if let Some(value) = request.get("translation") {
        let next = parse_vec3(value, "translation")?;
        merge_vec3(transform, "trans", next, 3)?;
    }
    if let Some(value) = request.get("rotation") {
        let next = parse_vec3(value, "rotation")?;
        merge_vec3(transform, "rot", next, 3)?;
    }
    if let Some(value) = request.get("scale") {
        let next = parse_vec2(value, "scale")?;
        merge_vec2(transform, "scale", next)?;
    }
    if !request.as_object().is_some_and(|object| {
        object.contains_key("translation")
            || object.contains_key("rotation")
            || object.contains_key("scale")
            || object.contains_key("zsort")
    }) {
        return Err("at least one transform field is required".into());
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub fn apply_transform_component(
    model: &Value,
    id: u32,
    component: &str,
    value: f64,
) -> Result<Value, String> {
    let mut request = json!({});
    match component {
        "translationX" => {
            let current = current_translation(model, id)?;
            request["translation"] = json!([value, current[1], current[2]]);
        }
        "translationY" => {
            let current = current_translation(model, id)?;
            request["translation"] = json!([current[0], value, current[2]]);
        }
        "translationZ" => {
            let current = current_translation(model, id)?;
            request["translation"] = json!([current[0], current[1], value]);
        }
        "rotationX" => {
            let current = current_rotation(model, id)?;
            request["rotation"] = json!([value, current[1], current[2]]);
        }
        "rotationY" => {
            let current = current_rotation(model, id)?;
            request["rotation"] = json!([current[0], value, current[2]]);
        }
        "rotationZ" => {
            let current = current_rotation(model, id)?;
            request["rotation"] = json!([current[0], current[1], value]);
        }
        "scaleX" => {
            let current = current_scale(model, id)?;
            request["scale"] = json!([value, current[1]]);
        }
        "scaleY" => {
            let current = current_scale(model, id)?;
            request["scale"] = json!([current[0], value]);
        }
        "zSort" => {
            request["zsort"] = json!(value);
        }
        other => return Err(format!("unsupported transform component: {other}")),
    }
    transform_node(model, id, &request)
}

fn default_node_name(model: &Value, class_name: &str) -> String {
    let base = format!("Unnamed {class_name}");
    if !node_name_taken(model, &base, None) {
        return base;
    }
    for index in 1..10_000 {
        let candidate = format!("{base} {index}");
        if !node_name_taken(model, &candidate, None) {
            return candidate;
        }
    }
    base
}

fn stamp_created_rig(
    node: &mut Value,
    class_name: &str,
    parent: &Value,
    bone: Option<&BoneRest>,
) -> Result<(), String> {
    let parent_type = parent.get("type").and_then(Value::as_str).unwrap_or("Node");
    match class_name {
        "DepthRigRoot" => Ok(()),
        "DepthBone" => {
            if !matches!(parent_type, "DepthRigRoot" | "DepthBone") {
                return Err("parent must be DepthRigRoot or DepthBone".into());
            }
            let rest = bone.cloned().unwrap_or_default();
            let bone_id = rest.bone_id.clone().unwrap_or_default();
            let parent_head = if parent_type == "DepthBone" {
                parent
                    .get("restHead")
                    .map(|value| parse_vec3(value, "restHead"))
                    .transpose()?
                    .unwrap_or([0.0, 0.0, 0.0])
            } else {
                [0.0, 0.0, 0.0]
            };
            let translation = [
                rest.rest_head[0] - parent_head[0],
                rest.rest_head[1] - parent_head[1],
                rest.rest_head[2] - parent_head[2],
            ];
            let object = node.as_object_mut().ok_or("node must be an object")?;
            object.insert("boneId".into(), Value::String(bone_id));
            object.insert("restHead".into(), json!(rest.rest_head));
            object.insert("restTail".into(), json!(rest.rest_tail));
            object.insert("restRoll".into(), json!(rest.rest_roll));
            object.insert("hingeAxis".into(), json!([0.0, 0.0, 1.0]));
            object.insert("lockRotation".into(), json!(false));
            object.insert("lockTranslation".into(), json!(false));
            object.insert("allowParentToTargets".into(), json!(true));
            object
                .get_mut("transform")
                .and_then(Value::as_object_mut)
                .ok_or("node transform must be an object")?
                .insert("trans".into(), json!(translation));
            Ok(())
        }
        _ => Ok(()),
    }
}

fn default_node(id: u32, class_name: &str, name: &str) -> Value {
    let mut node = json!({
        "uuid": id,
        "name": name,
        "type": class_name,
        "enabled": true,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": false,
        "pinToMesh": false,
        "children": [],
    });
    if matches!(class_name, "Composite" | "MeshGroup" | "DynamicComposite" | "Mask") {
        if let Some(object) = node.as_object_mut() {
            object.insert("opacity".into(), json!(1.0));
            object.insert("blend_mode".into(), json!("Normal"));
            object.insert("tint".into(), json!([1.0, 1.0, 1.0]));
            object.insert("screenTint".into(), json!([0.0, 0.0, 0.0]));
        }
    }
    if class_name == "Camera" {
        if let Some(object) = node.as_object_mut() {
            object.insert("viewport".into(), json!([1920.0, 1080.0]));
        }
    }
    crate::deformers::stamp_new_node(&mut node, class_name);
    if class_name == "SimplePhysics" {
        crate::physics::stamp(&mut node);
    }
    node
}

/// Adds `node`, which an import built whole, below `parent_id` under a uuid no node has, and answers that uuid.
pub(crate) fn add_built_node(model: &mut Value, parent_id: u32, mut node: Value) -> Result<u32, String> {
    let id = fresh_resource_id(model)?;
    node["uuid"] = json!(id);
    insert_child(model, parent_id, node)?;
    Ok(id)
}

fn insert_child(model: &mut Value, parent_id: u32, child: Value) -> Result<(), String> {
    let root = is_root(model, parent_id);
    let parent = find_node_mut(model, parent_id).ok_or_else(|| format!("parent node {parent_id} was not found"))?;
    ensure_parent_accepts_children(parent, parent_id, root)?;
    let children = parent
        .as_object_mut()
        .ok_or("node must be an object")?
        .entry("children")
        .or_insert_with(|| json!([]));
    children
        .as_array_mut()
        .ok_or("node children must be an array")?
        .push(child);
    Ok(())
}

fn insert_child_at(model: &mut Value, parent_id: u32, index: usize, child: Value) -> Result<(), String> {
    let root = is_root(model, parent_id);
    let parent = find_node_mut(model, parent_id).ok_or_else(|| format!("parent node {parent_id} was not found"))?;
    ensure_parent_accepts_children(parent, parent_id, root)?;
    let children = parent
        .as_object_mut()
        .ok_or("node must be an object")?
        .entry("children")
        .or_insert_with(|| json!([]));
    let children = children
        .as_array_mut()
        .ok_or("node children must be an array")?;
    let index = index.min(children.len());
    children.insert(index, child);
    Ok(())
}

fn ensure_parent_accepts_children(parent: &Value, parent_id: u32, root: bool) -> Result<(), String> {
    if root {
        return Ok(());
    }
    let parent_type = parent.get("type").and_then(Value::as_str).unwrap_or("Node");
    if CHILD_ACCEPTING_PARENT_TYPES.contains(&parent_type) {
        Ok(())
    } else {
        Err(format!(
            "parent node {parent_id} cannot accept children in Rust Core mode"
        ))
    }
}

fn ensure_finite_tree(node: &Value) -> Result<(), String> {
    if let Some(transform) = node.get("transform").and_then(Value::as_object) {
        for key in ["trans", "rot", "scale"] {
            if let Some(values) = transform.get(key).and_then(Value::as_array) {
                for value in values {
                    if value.as_f64().is_some_and(|number| !number.is_finite()) {
                        return Err("non-finite tree".into());
                    }
                }
            }
        }
    }
    if node.get("zsort").and_then(Value::as_f64).is_some_and(|zsort| !zsort.is_finite()) {
        return Err("non-finite tree".into());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            ensure_finite_tree(child)?;
        }
    }
    Ok(())
}

fn rig_kind(node: &Value) -> Option<&str> {
    match node.get("type").and_then(Value::as_str) {
        Some(kind @ ("DepthBone" | "DepthRigRoot")) => Some(kind),
        _ => None,
    }
}

/// Save keeps rig nodes. Export drops `DepthBone` and `DepthRigRoot`, lifts the
/// remaining descendants while preserving their world transform, and removes
/// parameter bindings that targeted a removed UUID. This matches
/// `ngINPExportWritePuppetMemoryWithoutDepthRigNodes`.
pub(crate) fn model_without_rig_nodes(model: &Value) -> Result<Value, String> {
    let mut exported = model.clone();
    let mut removed = HashSet::new();
    if let Some(nodes) = exported.get_mut("nodes") {
        strip_rig_nodes(nodes, Pose::identity(), &mut removed)?;
    }
    prune_parameter_bindings(&mut exported, &removed);
    Ok(exported)
}

#[derive(Clone, Copy)]
struct Pose {
    matrix: Matrix,
    z: f64,
}

impl Pose {
    fn identity() -> Self {
        Self {
            matrix: identity(),
            z: 0.0,
        }
    }
}

fn strip_rig_nodes(node: &mut Value, parent: Pose, removed: &mut HashSet<u32>) -> Result<(), String> {
    let world = compose_pose(parent, local_pose(node)?)?;
    let mut index = 0;
    loop {
        let len = node
            .get("children")
            .and_then(Value::as_array)
            .map(|children| children.len())
            .unwrap_or(0);
        if index >= len {
            break;
        }
        let kind = node
            .get("children")
            .and_then(Value::as_array)
            .and_then(|children| children.get(index))
            .and_then(|child| child.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind == "DepthBone" || kind == "DepthRigRoot" {
            let mut rig = node
                .get_mut("children")
                .and_then(Value::as_array_mut)
                .ok_or("node children must be an array")?
                .remove(index);
            let promoted = promote_rig_descendants(&mut rig, world, world, removed)?;
            let count = promoted.len();
            let children = node
                .get_mut("children")
                .and_then(Value::as_array_mut)
                .ok_or("node children must be an array")?;
            for (offset, child) in promoted.into_iter().enumerate() {
                children.insert(index + offset, child);
            }
            // promote_rig_descendants already walked lifted nodes for nested rigs.
            index += count;
        } else {
            let child = node
                .get_mut("children")
                .and_then(Value::as_array_mut)
                .and_then(|children| children.get_mut(index))
                .ok_or("node children must be an array")?;
            strip_rig_nodes(child, world, removed)?;
            index += 1;
        }
    }
    Ok(())
}

fn promote_rig_descendants(
    rig: &mut Value,
    destination: Pose,
    carrier: Pose,
    removed: &mut HashSet<u32>,
) -> Result<Vec<Value>, String> {
    if let Some(id) = rig.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
        removed.insert(id);
    }
    let rig_world = compose_pose(carrier, local_pose(rig)?)?;
    let children = rig
        .get_mut("children")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    let mut promoted = Vec::new();
    for mut child in children {
        if rig_kind(&child).is_some() {
            promoted.extend(promote_rig_descendants(
                &mut child,
                destination,
                rig_world,
                removed,
            )?);
        } else {
            let child_world = compose_pose(rig_world, local_pose(&child)?)?;
            write_local_pose(&mut child, destination, child_world)?;
            strip_rig_nodes(&mut child, destination, removed)?;
            promoted.push(child);
        }
    }
    Ok(promoted)
}

fn local_pose(node: &Value) -> Result<Pose, String> {
    let transform = node.get("transform");
    if transform.is_none() {
        return Ok(Pose::identity());
    }
    let id = node.get("uuid").and_then(Value::as_u64).unwrap_or(0);
    Ok(Pose {
        matrix: node_local_matrix(node, id)?,
        z: translation_z(node),
    })
}

fn translation_z(node: &Value) -> f64 {
    node.get("transform")
        .and_then(|transform| transform.get("trans"))
        .and_then(Value::as_array)
        .and_then(|translation| translation.get(2))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
}

fn compose_pose(parent: Pose, local: Pose) -> Result<Pose, String> {
    Ok(Pose {
        matrix: multiply(parent.matrix, local.matrix),
        z: parent.z + local.z,
    })
}

fn write_local_pose(node: &mut Value, parent: Pose, world: Pose) -> Result<(), String> {
    let local = multiply(invert(parent.matrix)?, world.matrix);
    let z = world.z - parent.z;
    let current = local_pose(node)?;
    if matrices_close(current.matrix, local) && (current.z - z).abs() <= 1e-9 {
        return Ok(());
    }
    set_local_transform_from_world(node, parent.matrix, world.matrix)?;
    let translation = node
        .get_mut("transform")
        .and_then(|transform| transform.get_mut("trans"))
        .and_then(Value::as_array_mut)
        .ok_or("node transform.trans must be an array")?;
    if translation.len() >= 3 {
        translation[2] = json!(z);
    }
    Ok(())
}

fn ensure_transformable_node(node: &Value, id: u32) -> Result<(), String> {
    // DepthBone and DepthRigRoot take the same TRS payload as any other node.
    // Move and delete of the puppet root stay rejected elsewhere.
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    match node_type {
        "Node" | "Root" | "Part" | "Composite" | "MeshGroup" | "DynamicComposite" | "Camera" | "Mask"
        | "PathDeformer" | "GridDeformer" | "DepthBone" | "DepthRigRoot" | "SimplePhysics" => Ok(()),
        other if other.contains("Depth") || other.contains("Bone") => Err(format!(
            "node {id} uses unsupported type {node_type} for Rust Core transform edits"
        )),
        other => Err(format!("node {id} has unsupported type {other:?}")),
    }
}

fn filter_top_level_selection(model: &Value, ids: &[u32]) -> Vec<u32> {
    ids.iter()
        .copied()
        .filter(|id| {
            !ids
                .iter()
                .any(|other| *other != *id && is_descendant(model, *other, *id))
        })
        .collect()
}

fn is_descendant(model: &Value, ancestor_id: u32, candidate_id: u32) -> bool {
    let Some(root) = find_node(model, ancestor_id) else {
        return false;
    };
    fn walk(node: &Value, target: u32) -> bool {
        if node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            == Some(target)
        {
            return true;
        }
        node.get("children")
            .and_then(Value::as_array)
            .is_some_and(|children| children.iter().any(|child| walk(child, target)))
    }
    walk(root, candidate_id)
}

pub(crate) fn find_parent_id(model: &Value, id: u32) -> Result<Option<u32>, String> {
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    fn walk(node: &Value, id: u32) -> Result<Option<u32>, String> {
        let Some(children) = node.get("children").and_then(Value::as_array) else {
            return Ok(None);
        };
        for child in children {
            if child
                .get("uuid")
                .and_then(Value::as_u64)
                .and_then(valid_id)
                == Some(id)
            {
                return node
                    .get("uuid")
                    .and_then(Value::as_u64)
                    .and_then(valid_id)
                    .ok_or_else(|| "parent node is missing uuid".to_string())
                    .map(Some);
            }
            if let Some(found) = walk(child, id)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    walk(root, id)
}

fn remove_node(model: &mut Value, id: u32) -> Result<Option<Value>, String> {
    let root = model.get_mut("nodes").ok_or("model is missing nodes")?;
    remove_node_recursive(root, id)
}

fn remove_node_recursive(node: &mut Value, id: u32) -> Result<Option<Value>, String> {
    let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) else {
        return Ok(None);
    };
    for index in 0..children.len() {
        if children[index]
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            == Some(id)
        {
            return Ok(Some(children.remove(index)));
        }
        if let Some(removed) = remove_node_recursive(&mut children[index], id)? {
            return Ok(Some(removed));
        }
    }
    Ok(None)
}

fn collect_node_ids(node: &Value, output: &mut HashSet<u32>) {
    if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
        output.insert(id);
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_node_ids(child, output);
        }
    }
}

fn prune_parameter_bindings(model: &mut Value, removed: &HashSet<u32>) {
    let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) else {
        return;
    };
    fn walk(parameters: &mut [Value], removed: &HashSet<u32>) {
        for parameter in parameters {
            if parameter.get("groupUUID").is_some() {
                if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                    walk(children, removed);
                }
                continue;
            }
            let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) else {
                continue;
            };
            bindings.retain(|binding| {
                binding
                    .get("node")
                    .and_then(Value::as_u64)
                    .and_then(valid_id)
                    .is_none_or(|id| !removed.contains(&id))
            });
        }
    }
    walk(parameters, removed);
}

fn node_zsort(node: &Value) -> Result<f64, String> {
    finite_number(node.get("zsort").unwrap_or(&Value::from(0)), "node.zsort")
}

fn accumulated_zsort_inclusive(model: &Value, id: u32) -> Result<f64, String> {
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    let mut total = 0.0;
    fn walk(node: &Value, target: u32, total: &mut f64) -> Result<bool, String> {
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or_else(|| "node is missing uuid".to_string())?;
        *total += node_zsort(node)?;
        if id == target {
            return Ok(true);
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                if walk(child, target, total)? {
                    return Ok(true);
                }
            }
        }
        *total -= node_zsort(node)?;
        Ok(false)
    }
    if walk(root, id, &mut total)? {
        Ok(total)
    } else {
        Err(format!("node {id} was not found"))
    }
}

fn world_matrix(model: &Value, id: u32) -> Result<Matrix, String> {
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    let mut found = None;
    fn walk(node: &Value, target: u32, chain: Matrix, found: &mut Option<Matrix>) -> Result<(), String> {
        let node_id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .ok_or_else(|| "node.uuid must be an unsigned integer".to_string())?;
        let local = node_local_matrix(node, node_id)?;
        let chain = multiply(chain, local);
        if node_id == u64::from(target) {
            *found = Some(chain);
            return Ok(());
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                walk(child, target, chain, found)?;
                if found.is_some() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    walk(root, id, identity(), &mut found)?;
    found.ok_or_else(|| format!("node {id} was not found"))
}

enum MovedWorld {
    Planar(Matrix),
    Rig(crate::bone_deform::Mat4),
}

fn is_rig_node(node: &Value) -> bool {
    matches!(
        node.get("type").and_then(Value::as_str),
        Some("DepthBone" | "DepthRigRoot")
    )
}

fn set_local_transform_from_world(
    node: &mut Value,
    parent_world: Matrix,
    world: Matrix,
) -> Result<(), String> {
    let local = multiply(invert(parent_world)?, world);
    let (translation, rotation, scale) = decompose_matrix(local)?;
    let transform = node
        .as_object_mut()
        .ok_or("node must be an object")?
        .entry("transform")
        .or_insert_with(|| json!({ "trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1] }));
    let transform = transform.as_object_mut().ok_or("node transform must be an object")?;
    transform.insert("trans".into(), json!(translation));
    transform.insert("rot".into(), json!(rotation));
    transform.insert("scale".into(), json!(scale));
    Ok(())
}

fn set_rig_translation_from_world(
    node: &mut Value,
    parent_world: crate::bone_deform::Mat4,
    world: crate::bone_deform::Mat4,
) -> Result<(), String> {
    let local = crate::bone_deform::mul_mat(crate::bone_deform::invert(parent_world)?, world);
    let translation = [local[0][3], local[1][3], local[2][3]];
    if translation.iter().any(|value| !value.is_finite()) {
        return Err("non-finite matrix".into());
    }
    let transform = node
        .as_object_mut()
        .ok_or("node must be an object")?
        .entry("transform")
        .or_insert_with(|| json!({ "trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1] }));
    transform
        .as_object_mut()
        .ok_or("node transform must be an object")?
        .insert("trans".into(), json!(translation));
    Ok(())
}

fn node_local_matrix(node: &Value, id: u64) -> Result<Matrix, String> {
    let Some(transform) = node.get("transform") else {
        return Ok(identity());
    };
    let transform = transform
        .as_object()
        .ok_or_else(|| format!("node {id}.transform must be an object"))?;
    let translation = vec3(transform.get("trans"), [0.0, 0.0, 0.0], &format!("node {id}.transform.trans"))?;
    let rotation = vec3(transform.get("rot"), [0.0, 0.0, 0.0], &format!("node {id}.transform.rot"))?;
    let scale = vec2(transform.get("scale"), [1.0, 1.0], &format!("node {id}.transform.scale"))?;
    if rotation[0].abs() > 1e-12 || rotation[1].abs() > 1e-12 {
        return Err(format!("node {id} has unsupported X/Y rotation"));
    }
    let (sin, cos) = rotation[2].sin_cos();
    Ok([
        [cos * scale[0], -sin * scale[1], translation[0]],
        [sin * scale[0], cos * scale[1], translation[1]],
        [0.0, 0.0, 1.0],
    ])
}

fn decompose_matrix(matrix: Matrix) -> Result<([f64; 3], [f64; 3], [f64; 2]), String> {
    let translation = [matrix[0][2], matrix[1][2], 0.0];
    let scale_x = (matrix[0][0].powi(2) + matrix[1][0].powi(2)).sqrt();
    let scale_y = (matrix[0][1].powi(2) + matrix[1][1].powi(2)).sqrt();
    if scale_x <= f64::EPSILON || scale_y <= f64::EPSILON {
        return Err("node transform is degenerate".into());
    }
    let rotation_z = matrix[1][0].atan2(matrix[0][0]);
    Ok((translation, [0.0, 0.0, rotation_z], [scale_x, scale_y]))
}

fn vec2(value: Option<&Value>, fallback: [f64; 2], path: &str) -> Result<[f64; 2], String> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    parse_vec2(value, path)
}

fn vec3(value: Option<&Value>, fallback: [f64; 3], path: &str) -> Result<[f64; 3], String> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    parse_vec3(value, path)
}

fn identity() -> Matrix {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            for k in 0..3 {
                result[row][column] += a[row][k] * b[k][column];
            }
        }
    }
    result
}

fn invert(matrix: Matrix) -> Result<Matrix, String> {
    let det = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    if det.abs() <= f64::EPSILON {
        return Err("node transform is not invertible".into());
    }
    let inv_det = 1.0 / det;
    Ok([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inv_det,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inv_det,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inv_det,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inv_det,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inv_det,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inv_det,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inv_det,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inv_det,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inv_det,
        ],
    ])
}

fn check_node_name(model: &Value, name: &str, except_id: Option<u32>) -> Result<(), String> {
    if node_name_taken(model, name, except_id) {
        return Err("Name already taken".into());
    }
    Ok(())
}

fn node_name_taken(model: &Value, name: &str, except_id: Option<u32>) -> bool {
    fn walk(node: &Value, name: &str, except_id: Option<u32>) -> bool {
        if let Some(object) = node.as_object() {
            let id = object.get("uuid").and_then(Value::as_u64).and_then(valid_id);
            let matches = object.get("name").and_then(Value::as_str) == Some(name)
                && except_id.is_none_or(|except| id != Some(except));
            if matches {
                return true;
            }
            if let Some(children) = object.get("children").and_then(Value::as_array) {
                return children.iter().any(|child| walk(child, name, except_id));
            }
        }
        false
    }
    model
        .get("nodes")
        .is_some_and(|root| walk(root, name, except_id))
}

fn current_translation(model: &Value, id: u32) -> Result<[f64; 3], String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    parse_vec3(node["transform"].get("trans").unwrap_or(&Value::Null), "transform.trans")
        .or(Ok([0.0, 0.0, 0.0]))
}

fn current_rotation(model: &Value, id: u32) -> Result<[f64; 3], String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    parse_vec3(node["transform"].get("rot").unwrap_or(&Value::Null), "transform.rot")
        .or(Ok([0.0, 0.0, 0.0]))
}

fn current_scale(model: &Value, id: u32) -> Result<[f64; 2], String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    parse_vec2(node["transform"].get("scale").unwrap_or(&Value::Null), "transform.scale")
        .or(Ok([1.0, 1.0]))
}

fn parse_vec2(value: &Value, name: &str) -> Result<[f64; 2], String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must contain two finite numbers"))?;
    if values.len() != 2 {
        return Err(format!("{name} must contain two finite numbers"));
    }
    Ok([
        finite_number(&values[0], name)?,
        finite_number(&values[1], name)?,
    ])
}

fn parse_vec3(value: &Value, name: &str) -> Result<[f64; 3], String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must contain three finite numbers"))?;
    if values.len() != 3 {
        return Err(format!("{name} must contain three finite numbers"));
    }
    Ok([
        finite_number(&values[0], name)?,
        finite_number(&values[1], name)?,
        finite_number(&values[2], name)?,
    ])
}

fn merge_vec3(
    transform: &mut serde_json::Map<String, Value>,
    key: &str,
    values: [f64; 3],
    length: usize,
) -> Result<(), String> {
    let current = transform
        .get(key)
        .map(|value| parse_vec3(value, key))
        .transpose()?
        .unwrap_or([0.0, 0.0, 0.0]);
    let merged = [
        values[0],
        if length > 1 { values[1] } else { current[1] },
        if length > 2 { values[2] } else { current[2] },
    ];
    transform.insert(key.into(), json!(merged));
    Ok(())
}

fn merge_vec2(
    transform: &mut serde_json::Map<String, Value>,
    key: &str,
    values: [f64; 2],
) -> Result<(), String> {
    transform.insert(key.into(), json!(values));
    Ok(())
}

pub fn find_node<'a>(model: &'a Value, id: u32) -> Option<&'a Value> {
    model.get("nodes").and_then(|root| find_node_recursive(root, id))
}

fn find_node_recursive<'a>(node: &'a Value, id: u32) -> Option<&'a Value> {
    if node.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
        return Some(node);
    }
    node.get("children")
        .and_then(Value::as_array)
        .and_then(|children| children.iter().find_map(|child| find_node_recursive(child, id)))
}

pub(crate) fn find_node_mut<'a>(model: &'a mut Value, id: u32) -> Option<&'a mut Value> {
    let root = model.get_mut("nodes")?;
    find_node_recursive_mut(root, id)
}

fn find_node_recursive_mut<'a>(node: &'a mut Value, id: u32) -> Option<&'a mut Value> {
    if node.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
        return Some(node);
    }
    let children = node.get_mut("children")?.as_array_mut()?;
    for child in children {
        if let Some(found) = find_node_recursive_mut(child, id) {
            return Some(found);
        }
    }
    None
}

/// Value domain of one inspector property. Parsing a request value into the stored JSON happens here
/// and nowhere else, so a property can only ever hold a value of its own kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropertyKind {
    Bool,
    Number { min: f64, max: f64 },
    /// A string of at most `MAX_META_TEXT` bytes without NUL.
    Text,
    /// Three numbers in 0..=1.
    Color,
    Choice(&'static [&'static str]),
    /// Two finite numbers.
    Pair,
    /// Two sizes, normalised like `ExCamera.normalizeViewport` (absolute, rounded, even, at least 2).
    Viewport,
}

/// One editable inspector property of some node types, and the JSON key it is stored under.
#[derive(Clone, Copy, Debug)]
pub struct PropertySpec {
    pub name: &'static str,
    pub kind: PropertyKind,
    types: &'static [&'static str],
    path: &'static [&'static str],
    /// The edit moves a node in the scene, so deform caches that depend on it are refreshed.
    pub pose: bool,
}

const F32_MAX: f64 = f32::MAX as f64;
const UNIT: PropertyKind = PropertyKind::Number { min: 0.0, max: 1.0 };

/// Types whose Node inspector is shown. DepthRigRoot and DepthBone have their own inspector.
const PLAIN_TYPES: &[&str] = &[
    "Node", "Part", "Composite", "MeshGroup", "DynamicComposite", "Camera", "Mask", "PathDeformer",
    "GridDeformer", "SimplePhysics",
];
const PART_TYPES: &[&str] = &["Part", "DynamicComposite"];
const COMPOSITE_TYPES: &[&str] = &["Composite"];
const BLENDED_TYPES: &[&str] = &["Part", "DynamicComposite", "Composite"];
/// Drawables that serialise a `mesh` (Composite does not, and Mask keeps only the Drawable keys).
const MESH_TYPES: &[&str] = &["Part", "DynamicComposite", "Mask", "MeshGroup"];

/// `BlendMode` member names in declaration order. The Composite inspector stops at DestinationIn. LinearBurn,
/// LinearLight, Hue and Color are not modes of D: a reader of INX that knows the other 19 does not know them.
const BLEND_MODES: &[&str] = &[
    "Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge", "LinearDodge", "AddGlow",
    "ColorBurn", "HardLight", "SoftLight", "Difference", "Exclusion", "Subtract", "LinearBurn", "LinearLight", "Hue",
    "Color", "Inverse", "DestinationIn", "ClipToLower", "SliceFromLower",
];
const COMPOSITE_BLEND_MODES: &[&str] = &[
    "Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge", "LinearDodge", "AddGlow",
    "ColorBurn", "HardLight", "SoftLight", "Difference", "Exclusion", "Subtract", "LinearBurn", "LinearLight", "Hue",
    "Color", "Inverse", "DestinationIn",
];
/// The Cubism 5.3 alpha compositions (ALP01-ALP05). INX has no such setting: absent or Over keeps the INX formula of
/// `blend_mode`, and any other one draws the Cubism colour formula of the same name with that composition.
const ALPHA_BLENDS: &[&str] = &["Over", "Atop", "Out", "Conjoint", "Disjoint"];

const fn spec(
    name: &'static str,
    types: &'static [&'static str],
    path: &'static [&'static str],
    kind: PropertyKind,
    pose: bool,
) -> PropertySpec {
    PropertySpec { name, kind, types, path, pose }
}

/// Every property `node.property.set` can write. Keys follow the `serializeSelfImpl` of the engine node
/// classes; the transform, SimplePhysics (`physics.set`) and deformer (`deformer.set`) settings have
/// their own operations.
static PROPERTIES: &[PropertySpec] = &[
    spec("lockToRoot", PLAIN_TYPES, &["lockToRoot"], PropertyKind::Bool, true),
    spec("pinToMesh", PLAIN_TYPES, &["pinToMesh"], PropertyKind::Bool, true),
    spec("zsort", PLAIN_TYPES, &["zsort"], PropertyKind::Number { min: -F32_MAX, max: F32_MAX }, true),
    spec("tint", BLENDED_TYPES, &["tint"], PropertyKind::Color, false),
    spec("screenTint", BLENDED_TYPES, &["screenTint"], PropertyKind::Color, false),
    spec("emissionStrength", PART_TYPES, &["emissionStrength"], PropertyKind::Number { min: 0.0, max: F32_MAX }, false),
    spec("blend_mode", PART_TYPES, &["blend_mode"], PropertyKind::Choice(BLEND_MODES), false),
    spec("blend_mode", COMPOSITE_TYPES, &["blend_mode"], PropertyKind::Choice(COMPOSITE_BLEND_MODES), false),
    spec("alphaBlend", BLENDED_TYPES, &["alphaBlend"], PropertyKind::Choice(ALPHA_BLENDS), false),
    spec("opacity", BLENDED_TYPES, &["opacity"], UNIT, false),
    spec("mask_threshold", BLENDED_TYPES, &["mask_threshold"], UNIT, false),
    spec("auto_resized", &["DynamicComposite"], &["auto_resized"], PropertyKind::Bool, false),
    spec("propagate_meshgroup", COMPOSITE_TYPES, &["propagate_meshgroup"], PropertyKind::Bool, false),
    spec("origin", MESH_TYPES, &["mesh", "origin"], PropertyKind::Pair, true),
    spec("viewport", &["Camera"], &["viewport"], PropertyKind::Viewport, false),
];

/// The properties the inspector shows for a node type, in table order.
pub fn properties_for(node_type: &str) -> impl Iterator<Item = &'static PropertySpec> + '_ {
    PROPERTIES.iter().filter(move |property| property.types.contains(&node_type))
}

impl PropertyKind {
    fn parse(self, name: &str, value: &Value) -> Result<Value, String> {
        match self {
            Self::Bool => value.as_bool().map(Value::Bool).ok_or_else(|| format!("{name} must be a boolean")),
            Self::Number { min, max } => {
                let number = finite_number(value, name)?;
                if number < min || number > max {
                    return Err(format!("{name} must be between {min} and {max}"));
                }
                Ok(json!(number))
            }
            Self::Text => {
                let text = value.as_str().ok_or_else(|| format!("{name} must be a string"))?;
                if text.len() > MAX_META_TEXT || text.contains('\0') {
                    return Err(format!("{name} must be at most {MAX_META_TEXT} bytes without NUL"));
                }
                Ok(json!(text))
            }
            Self::Color => {
                let channels = number_list(value, name, 3)?;
                if channels.iter().any(|channel| !(0.0..=1.0).contains(channel)) {
                    return Err(format!("{name} channels must be between 0 and 1"));
                }
                Ok(json!(channels))
            }
            Self::Choice(choices) => {
                let text = value.as_str().ok_or_else(|| format!("{name} must be a string"))?;
                if !choices.contains(&text) {
                    return Err(format!("{name} must be one of {}", choices.join(", ")));
                }
                Ok(json!(text))
            }
            Self::Pair => Ok(json!(number_list(value, name, 2)?)),
            Self::Viewport => {
                let size = number_list(value, name, 2)?;
                Ok(json!([even_dimension(size[0]), even_dimension(size[1])]))
            }
        }
    }
}

fn number_list(value: &Value, name: &str, length: usize) -> Result<Vec<f64>, String> {
    let list = value
        .as_array()
        .filter(|list| list.len() == length)
        .ok_or_else(|| format!("{name} must be {length} numbers"))?;
    list.iter().map(|item| finite_number(item, name)).collect()
}

/// `ExCamera.evenDimension`: the viewport is stored as a positive even whole number.
fn even_dimension(value: f64) -> f64 {
    let size = value.abs().round().max(2.0);
    if size % 2.0 != 0.0 {
        size + 1.0
    } else {
        size
    }
}

/// Set one inspector property. The (property, node type) pair must be in [`PROPERTIES`]; the value is
/// parsed by the property's kind. Returns the candidate model and the property that was written.
pub fn set_property(
    model: &Value,
    id: u32,
    property: &str,
    value: &Value,
) -> Result<(Value, &'static PropertySpec), String> {
    if is_root(model, id) {
        return Err("the root node has no node properties".into());
    }
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    let spec = properties_for(node_type)
        .find(|candidate| candidate.name == property)
        .ok_or_else(|| format!("node {id} ({node_type}) has no editable property {property}"))?;
    let stored = spec.kind.parse(property, value)?;
    let mut candidate = model.clone();
    let node = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    let (key, parents) = spec.path.split_last().ok_or("property has no key")?;
    let mut holder = node;
    for parent in parents {
        holder = holder.get_mut(*parent).ok_or_else(|| format!("node {id} has no {parent} to edit"))?;
    }
    holder
        .as_object_mut()
        .ok_or_else(|| format!("node {id} {property} parent must be an object"))?
        .insert((*key).to_owned(), stored);
    if matches!(property, "blend_mode" | "alphaBlend") {
        let node = find_node(&candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        crate::render::check_blend(node, id.into(), node_type)?;
    }
    validate_model(&candidate)?;
    Ok((candidate, spec))
}

/// The stored value of a property, as the inspector shows it.
pub fn property_value<'a>(node: &'a Value, spec: &PropertySpec) -> Option<&'a Value> {
    spec.path.iter().try_fold(node, |value, key| value.get(*key))
}

/// What `node.mask.edit` does to the mask list of a Part, DynamicComposite or Composite.
pub enum MaskEdit {
    Add { source: u32, dodge: bool },
    Remove { source: u32 },
    SetMode { source: u32, dodge: bool },
    Move { source: u32, index: usize },
}

pub const MASKED_TYPES: &[&str] = &["Part", "DynamicComposite", "Composite"];
/// Drawable subclasses: what a mask source may be.
pub const MASK_SOURCE_TYPES: &[&str] = &["Part", "DynamicComposite", "Composite", "Mask", "MeshGroup"];

fn mask_entry(source: u32, dodge: bool) -> Value {
    json!({"source": source, "mode": if dodge { "DodgeMask" } else { "Mask" }})
}

/// Edit the `masks` list the way `PartAddMaskAction` and its siblings do. A legacy `masked_by` list is
/// converted first, because the engine serialises only `masks`.
pub fn edit_masks(model: &Value, id: u32, edit: &MaskEdit) -> Result<(Value, Value), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if !MASKED_TYPES.contains(&node_type) {
        return Err(format!("node {id} ({node_type}) has no mask list"));
    }
    let source = match edit {
        MaskEdit::Add { source, .. } | MaskEdit::Remove { source } | MaskEdit::SetMode { source, .. }
        | MaskEdit::Move { source, .. } => *source,
    };
    let mut masks = mask_list(node)?;
    let position = masks.iter().position(|entry| entry["source"].as_u64() == Some(u64::from(source)));
    match edit {
        MaskEdit::Add { dodge, .. } => {
            if source == id {
                return Err("a node cannot mask itself".into());
            }
            let source_node = find_node(model, source).ok_or_else(|| format!("mask source {source} was not found"))?;
            let source_type = source_node.get("type").and_then(Value::as_str).unwrap_or("Node");
            if !MASK_SOURCE_TYPES.contains(&source_type) {
                return Err(format!("mask source {source} ({source_type}) is not a drawable"));
            }
            if position.is_some() {
                return Err(format!("node {id} is already masked by {source}"));
            }
            masks.push(mask_entry(source, *dodge));
        }
        MaskEdit::Remove { .. } => {
            masks.remove(position.ok_or("Mask entry not found")?);
        }
        MaskEdit::SetMode { dodge, .. } => {
            masks[position.ok_or("Mask entry not found")?] = mask_entry(source, *dodge);
        }
        MaskEdit::Move { index, .. } => {
            let entry = masks.remove(position.ok_or("Mask entry not found")?);
            masks.insert((*index).min(masks.len()), entry);
        }
    }
    let mut candidate = model.clone();
    let object = find_node_mut(&mut candidate, id)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("node {id} was not found"))?;
    object.remove("masked_by");
    object.remove("mask_mode");
    object.insert("masks".into(), Value::Array(masks.clone()));
    validate_model(&candidate)?;
    Ok((candidate, Value::Array(masks)))
}

fn mask_list(node: &Value) -> Result<Vec<Value>, String> {
    if let Some(masks) = node.get("masks").filter(|masks| !masks.is_null()) {
        return masks.as_array().cloned().ok_or_else(|| "masks must be an array".to_owned());
    }
    let Some(legacy) = node.get("masked_by").filter(|legacy| !legacy.is_null()) else {
        return Ok(Vec::new());
    };
    let dodge = match node.get("mask_mode").and_then(Value::as_str) {
        None | Some("Mask") => false,
        Some("DodgeMask") => true,
        Some(other) => return Err(format!("unknown mask_mode {other}")),
    };
    legacy
        .as_array()
        .ok_or("masked_by must be an array")?
        .iter()
        .map(|source| {
            let source = source.as_u64().and_then(valid_id).ok_or("masked_by must hold node UUIDs")?;
            Ok(mask_entry(source, dodge))
        })
        .collect()
}

const MAX_META_TEXT: usize = 4096;

/// How `puppet.meta.set` stores a property: `PuppetMeta` strings and flag under `meta`, `PuppetPhysics`
/// numbers under `physics`.
pub fn puppet_property(name: &str) -> Option<(&'static str, PropertyKind)> {
    Some(match name {
        "name" | "artist" | "rigger" | "contact" | "licenseURL" | "copyright" | "reference" => ("meta", PropertyKind::Text),
        "preservePixels" => ("meta", PropertyKind::Bool),
        "pixelsPerMeter" => ("physics", PropertyKind::Number { min: 1.0, max: F32_MAX }),
        "gravity" => ("physics", PropertyKind::Number { min: 0.0, max: F32_MAX }),
        _ => return None,
    })
}

/// The puppet metadata the Puppet inspector edits.
pub const PUPPET_PROPERTIES: &[&str] = &[
    "name", "artist", "rigger", "contact", "licenseURL", "copyright", "reference", "preservePixels",
    "pixelsPerMeter", "gravity",
];

pub fn set_puppet_property(model: &Value, property: &str, value: &Value) -> Result<Value, String> {
    let (section, kind) = puppet_property(property).ok_or_else(|| format!("unknown puppet property {property}"))?;
    let stored = kind.parse(property, value)?;
    let mut candidate = model.clone();
    let object = candidate.as_object_mut().ok_or("model must be a JSON object")?;
    let holder = object.entry(section).or_insert_with(|| json!({}));
    if holder.is_null() {
        *holder = json!({});
    }
    holder
        .as_object_mut()
        .ok_or_else(|| format!("model {section} must be an object"))?
        .insert(property.to_owned(), stored);
    validate_model(&candidate)?;
    Ok(candidate)
}

// ---- Node panel: multi-node visibility, parameter usage and drag-and-drop targets ----------------

/// Node UUIDs of a request in the order given: `id`, else `ids`, else `nodes`.
fn request_node_ids(request: &Value) -> Result<Vec<u32>, String> {
    if let Some(id) = request.get("id") {
        let id = id.as_u64().and_then(valid_id).ok_or("id must be a nonzero u32 UUID")?;
        return Ok(vec![id]);
    }
    let key = if request.get("ids").is_some() { "ids" } else { "nodes" };
    let list = request
        .get(key)
        .and_then(Value::as_array)
        .ok_or("node UUIDs are required")?;
    let ids = list
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(valid_id)
                .ok_or_else(|| format!("{key} must contain nonzero u32 UUIDs"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    Ok(unique_ids(&ids))
}

/// Pre-order UUIDs of the whole tree: the order the rows have in an unfiltered, unreversed panel.
fn tree_order(model: &Value) -> Vec<u32> {
    fn walk(node: &Value, out: &mut Vec<u32>) {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            out.push(id);
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    if let Some(root) = model.get("nodes") {
        walk(root, &mut out);
    }
    out
}

/// A boolean a node stores under `key`; a node without the key has `default`.
#[derive(Clone, Copy)]
struct NodeFlag {
    key: &'static str,
    default: bool,
}

const ENABLED: NodeFlag = NodeFlag { key: "enabled", default: true };
const LOCKED: NodeFlag = NodeFlag { key: "locked", default: false };

fn flag_value(node: &Value, flag: NodeFlag) -> bool {
    node.get(flag.key).and_then(Value::as_bool).unwrap_or(flag.default)
}

fn set_flag_value(node: &mut Value, flag: NodeFlag, next: bool) -> Result<(), String> {
    if next != flag_value(node, flag) {
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert(flag.key.into(), Value::Bool(next));
    }
    Ok(())
}

fn set_flag_subtree(node: &mut Value, flag: NodeFlag, next: bool, touched: &mut Vec<u32>) -> Result<(), String> {
    if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
        touched.push(id);
    }
    set_flag_value(node, flag, next)?;
    if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            set_flag_subtree(child, flag, next, touched)?;
        }
    }
    Ok(())
}

/// Every node that is locked itself or sits below a locked node: a lock on a parent covers what is in it.
pub(crate) fn locked_ids(model: &Value) -> std::collections::BTreeSet<u32> {
    fn walk(node: &Value, inherited: bool, out: &mut std::collections::BTreeSet<u32>) {
        let locked = inherited || flag_value(node, LOCKED);
        if locked {
            out.extend(node.get("uuid").and_then(Value::as_u64).and_then(valid_id));
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            walk(child, locked, out);
        }
    }
    let mut out = std::collections::BTreeSet::new();
    if let Some(root) = model.get("nodes") {
        walk(root, false, &mut out);
    }
    out
}

/// The nodes of one level: every child of the parent of `id`, in tree order. The root has no level.
pub(crate) fn sibling_ids(model: &Value, id: u32) -> Result<Vec<u32>, String> {
    let parent = find_parent_id(model, id)?.ok_or("the root node has no siblings")?;
    let children = find_node(model, parent).and_then(|node| node.get("children")).and_then(Value::as_array);
    Ok(children.into_iter().flatten().filter_map(|child| child.get("uuid")?.as_u64().and_then(valid_id)).collect())
}

/// Hide or show several nodes in one commit. `enabled` absent toggles: the first listed node
/// decides the new state, which every listed node then receives (D's `ToggleVisibility` reads
/// only `ctx.nodes[0]`). With `recursive`, each node's whole subtree follows (D `treeSetEnabled`).
/// With `siblings`, each listed node stands for its whole level (the Cubism Parts palette's Ctrl-click).
/// Returns the candidate and `{nodes, enabled, recursive}`.
pub(crate) fn set_visibility(model: &Value, request: &Value) -> Result<(Value, Value), String> {
    set_flag(model, request, ENABLED)
}

/// Lock or unlock several nodes in one commit, with the request shape of `set_visibility` and `locked` in the
/// place of `enabled`. The lock is stored on the node as `locked`. Returns the candidate and
/// `{nodes, locked, recursive}`.
pub(crate) fn set_lock(model: &Value, request: &Value) -> Result<(Value, Value), String> {
    set_flag(model, request, LOCKED)
}

fn set_flag(model: &Value, request: &Value, target: NodeFlag) -> Result<(Value, Value), String> {
    let mut ids = request_node_ids(request)?;
    let flag = |key: &str| match request.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    };
    let recursive = flag("recursive")?;
    let siblings = flag("siblings")?;
    let explicit = match request.get(target.key) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(format!("{} must be a boolean", target.key)),
    };
    for id in &ids {
        find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
    }
    let first = find_node(model, ids[0]).is_some_and(|node| flag_value(node, target));
    let next = explicit.unwrap_or(!first);
    if siblings {
        let mut level = Vec::new();
        for id in &ids {
            level.extend(sibling_ids(model, *id)?);
        }
        ids = unique_ids(&level);
    }
    let mut candidate = model.clone();
    let mut touched = Vec::new();
    for id in &ids {
        let node = find_node_mut(&mut candidate, *id).ok_or_else(|| format!("node {id} was not found"))?;
        if recursive {
            set_flag_subtree(node, target, next, &mut touched)?;
        } else {
            set_flag_value(node, target, next)?;
            touched.push(*id);
        }
    }
    if candidate != *model {
        validate_model(&candidate)?;
    }
    let touched = unique_ids(&touched);
    Ok((candidate, json!({"nodes": touched, target.key: next, "recursive": recursive})))
}

impl crate::Session {
    /// `node.visibility.set`: `ids`/`nodes`/`id`, optional `enabled`, optional `recursive`. The editor
    /// controller calls this directly; the root may also route the op name here for MCP.
    pub(crate) fn op_node_visibility(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, result) = set_visibility(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    /// `node.lock.set`: the request of `node.visibility.set` with `locked` for `enabled`. A lock stops picking in
    /// the editor viewport and nothing else: every operation still edits a locked node.
    pub(crate) fn op_node_lock(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, result) = set_lock(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    /// Sends the request through the normal router as `node.visibility.set`.
    pub(crate) fn handle_node_visibility(&mut self, request: &Value) -> Value {
        let mut routed = request.clone();
        if let Some(object) = routed.as_object_mut() {
            object.insert("op".into(), json!("node.visibility.set"));
        }
        self.handle_request(&routed)
    }
}

/// A parameter that has at least one binding on a node.
#[derive(Clone, Debug, PartialEq)]
pub struct ParameterUse {
    pub uuid: u32,
    pub name: String,
}

/// Parameters, in model order and group by group, with a binding whose target is node `id`
/// (D "Parameter Usage" menu). Parameter groups are walked, not listed.
pub(crate) fn parameter_usage(model: &Value, id: u32) -> Vec<ParameterUse> {
    fn walk(list: &[Value], id: u32, out: &mut Vec<ParameterUse>) {
        for item in list {
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                walk(children, id, out);
            }
            let bound = item
                .get("bindings")
                .and_then(Value::as_array)
                .is_some_and(|bindings| {
                    bindings
                        .iter()
                        .any(|binding| binding.get("node").and_then(Value::as_u64) == Some(u64::from(id)))
                });
            if bound {
                if let Some(uuid) = item.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
                    out.push(ParameterUse {
                        uuid,
                        name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    });
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        walk(list, id, &mut out);
    }
    out
}

/// Where a dragged selection lands relative to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropSite {
    /// Onto the row: first child of that node (D `MoveNode(row, 0)`).
    Onto(u64),
    /// Just above the row, as its sibling.
    Before(u64),
    /// Just below the row, as its sibling.
    After(u64),
}

/// Parent and child index (counted after the dragged nodes left their old places, as
/// `node.move` expects) for dropping `moving` at `site`. `Ok(None)` means the drop would leave the
/// tree unchanged. Rejections: the root is dragged or used as a sibling anchor, a node is dropped
/// into itself or its own subtree, or the new parent cannot hold children.
pub(crate) fn resolve_drop(
    model: &Value,
    moving: &[u32],
    site: DropSite,
) -> Result<Option<(u32, usize)>, String> {
    if moving.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    if moving.iter().any(|id| is_root(model, *id)) {
        return Err("the root node cannot be moved".into());
    }
    for id in moving {
        find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
    }
    let order = tree_order(model);
    let mut top = filter_top_level_selection(model, moving);
    top.sort_by_key(|id| order.iter().position(|other| other == id).unwrap_or(usize::MAX));
    top.dedup();
    let site_id = |id: u64| u32::try_from(id).map_err(|_| format!("node {id} was not found"));
    let (parent, raw_index) = match site {
        DropSite::Onto(target) => (site_id(target)?, 0),
        DropSite::Before(anchor) | DropSite::After(anchor) => {
            let anchor = site_id(anchor)?;
            if is_root(model, anchor) {
                return Err("nothing can be dropped beside the root".into());
            }
            let parent = find_parent_id(model, anchor)?.ok_or_else(|| format!("node {anchor} has no parent"))?;
            let position = child_ids(model, parent)
                .iter()
                .position(|id| *id == anchor)
                .ok_or_else(|| format!("node {anchor} was not found"))?;
            (parent, position + usize::from(matches!(site, DropSite::After(_))))
        }
    };
    let parent_node = find_node(model, parent).ok_or_else(|| format!("parent node {parent} was not found"))?;
    for id in &top {
        if *id == parent || is_descendant(model, *id, parent) {
            return Err(format!("node {id} cannot be dropped into itself or its descendants"));
        }
    }
    let parent_type = parent_node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if !is_root(model, parent) && !CHILD_ACCEPTING_PARENT_TYPES.contains(&parent_type) {
        return Err(format!("{parent_type} nodes cannot accept children"));
    }
    let current = child_ids(model, parent);
    let before_anchor = current
        .iter()
        .take(raw_index)
        .filter(|id| top.contains(id))
        .count();
    let index = raw_index - before_anchor;
    let mut next: Vec<u32> = current.iter().copied().filter(|id| !top.contains(id)).collect();
    let at = index.min(next.len());
    for (offset, id) in top.iter().enumerate() {
        next.insert(at + offset, *id);
    }
    Ok((next != current).then_some((parent, index)))
}

fn child_ids(model: &Value, parent: u32) -> Vec<u32> {
    find_node(model, parent)
        .and_then(|node| node.get("children"))
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|child| child.get("uuid").and_then(Value::as_u64).and_then(valid_id))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_delete_and_create_structural_nodes() {
        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": {"trans":[0,0,0], "rot":[0,0,0], "scale":[1,1]},
                "lockToRoot": false, "pinToMesh": false, "children": [],
            },
            "param": [],
        });
        let (created_model, created) = create_nodes(&model, &[1], "Composite", None, Some("Group"), None).unwrap();
        let composite_id = created[0]["uuid"].as_u64().unwrap() as u32;
        assert_eq!(created[0]["type"], "Composite");
        let (with_child, child_created) =
            create_nodes(&created_model, &[composite_id], "Node", None, Some("Child"), None).unwrap();
        let child_id = child_created[0]["uuid"].as_u64().unwrap() as u32;
        let moved = move_nodes(&with_child, &[child_id], 1, Some(0)).unwrap();
        assert!(find_node(&moved, child_id).is_some());
        let deleted = delete_nodes(&moved, &[child_id]).unwrap();
        assert!(find_node(&deleted, child_id).is_none());
        assert!(find_node(&deleted, composite_id).is_some());
    }

    #[test]
    fn create_rename_and_transform_nodes() {
        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
                "transform": {"trans":[0,0,0], "rot":[0,0,0], "scale":[1,1]},
                "lockToRoot": false, "pinToMesh": false,
            },
            "param": [],
        });
        let (created_model, created) = create_nodes(&model, &[1], "Node", None, Some("Child"), None).unwrap();
        assert_eq!(created.len(), 1);
        let id = created[0]["uuid"].as_u64().unwrap() as u32;
        let renamed = rename_node(&created_model, id, "Renamed").unwrap();
        let transformed = transform_node(
            &renamed,
            id,
            &json!({"translation":[4,5,6], "rotation":[0,0,1], "scale":[2,3]}),
        )
        .unwrap();
        let node = find_node(&transformed, id).unwrap();
        assert_eq!(node["name"], "Renamed");
        assert_eq!(node["transform"]["trans"], json!([4.0, 5.0, 6.0]));
        assert_eq!(node["transform"]["rot"], json!([0.0, 0.0, 1.0]));
        assert_eq!(node["transform"]["scale"], json!([2.0, 3.0]));
    }

    fn sample_model() -> Value {
        json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": {"trans":[0.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]},
                "lockToRoot": false, "pinToMesh": false,
                "children": [{
                    "uuid": 4, "name": "Parent", "type": "Node", "enabled": true, "zsort": 1.0,
                    "transform": {"trans":[3.0,4.0,1.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]},
                    "lockToRoot": false, "pinToMesh": false,
                    "children": [{
                        "uuid": 5, "name": "Child", "type": "Node", "enabled": true, "zsort": 0.0,
                        "transform": {"trans":[1.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[2.0,2.0]},
                        "lockToRoot": false, "pinToMesh": false, "children": []
                    }]
                }]
            },
            "param": [],
        })
    }

    #[test]
    fn copy_paste_cut_and_convert_round_trip() {
        let model = sample_model();
        let copies = copy_nodes(&model, &[4]).unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].node["name"], "Parent'");
        assert_ne!(copies[0].node["uuid"], 4);
        assert_eq!(copies[0].node["children"][0]["name"], "Child'");
        assert_ne!(copies[0].node["children"][0]["uuid"], 5);
        assert_eq!(copies[0].node["transform"]["trans"], json!([3.0, 4.0, 1.0]));
        let (pasted, created) = paste_nodes(&model, &copies, 1, Some(0)).unwrap();
        assert_eq!(created[0]["name"], "Parent'");
        assert_eq!(pasted["nodes"]["children"][0]["name"], "Parent'");
        assert_eq!(pasted["nodes"]["children"][1]["uuid"], 4);
        let (converted, nodes) = convert_nodes(&pasted, &[4], "Composite").unwrap();
        assert_eq!(nodes[0]["type"], "Composite");
        assert_eq!(nodes[0]["uuid"], 4);
        assert_eq!(nodes[0]["opacity"], 1.0);
        assert!(find_node(&converted, 5).is_some());
        let undone = convert_nodes(&converted, &[4], "Node").unwrap().0;
        assert_eq!(find_node(&undone, 4).unwrap()["type"], "Node");
        assert!(find_node(&undone, 4).unwrap().get("opacity").is_none());
        let (masked, mask_nodes) = convert_nodes(&undone, &[4], "Mask").unwrap();
        assert_eq!(mask_nodes[0]["type"], "Mask");
        assert_eq!(mask_nodes[0]["opacity"], 1.0);
        assert_eq!(mask_nodes[0]["children"][0]["uuid"], 5);
        let camera = convert_nodes(&masked, &[4], "Camera").unwrap().0;
        assert_eq!(find_node(&camera, 4).unwrap()["type"], "Camera");
        assert!(find_node(&camera, 4).unwrap().get("opacity").is_none());
        assert_eq!(find_node(&camera, 4).unwrap()["viewport"], json!([1920.0, 1080.0]));
        let grid = convert_nodes(&model, &[4], "GridDeformer").unwrap().0;
        assert_eq!(find_node(&grid, 4).unwrap()["type"], "GridDeformer");
        assert_eq!(find_node(&grid, 4).unwrap()["grid_axis_x"], json!([-0.5, 0.5]));
        let part = convert_nodes(&model, &[4], "Part").unwrap().0;
        let converted = find_node(&part, 4).unwrap();
        assert_eq!(converted["type"], "Part");
        assert_eq!(converted["vertices"], json!([]));
        assert_eq!(converted["uvs"], json!([]));
        assert_eq!(converted["indices"], json!([]));
        assert!(copy_nodes(&model, &[1]).is_err());
        let mut blocked = model.clone();
        blocked["nodes"]["children"][0]["type"] = json!("SimplePhysics");
        assert!(copy_nodes(&blocked, &[4]).is_err());
        assert_eq!(blocked["nodes"]["children"][0]["type"], "SimplePhysics");
    }

    #[test]
    fn part_mesh_round_trips_and_grid_convert_stays_closed() {
        let mut session = crate::Session::new();
        let created = ok(&session.handle_request(&json!({
            "op":"node.create","className":"Node","name":"Card"
        })));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({"op":"node.convert","nodes":[id],"className":"Part"})));
        let part = ok(&session.handle_request(&json!({"op":"node.get","id":id})));
        assert_eq!(part["vertices"], json!([]));
        assert_eq!(part["uvs"], json!([]));
        assert_eq!(part["indices"], json!([]));
        assert_eq!(part["opacity"], 1.0);
        assert_eq!(part["blend_mode"], "Normal");
        assert_eq!(part["tint"], json!([1.0, 1.0, 1.0]));
        assert_eq!(part["screenTint"], json!([0.0, 0.0, 0.0]));
        assert_eq!(part["mesh"]["verts"], json!([]));
        assert_eq!(part["mesh"]["indices"], json!([]));
        let bounds = ok(&session.handle_request(&json!({"op":"bounds"})));
        assert_eq!(bounds["bounds"].as_array().unwrap().len(), 4, "{bounds}");
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"})))["model"], before["model"]);

        let triangle = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 2, "name": "Tri", "type": "Part",
                    "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                    "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                    "indices": [0, 1, 2]
                }]
            },
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":triangle})));
        let path = std::env::temp_dir().join(format!("vsoul-part-mesh-{}.inx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        ok(&session.handle_request(&json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true})));
        let mut reloaded = crate::Session::new();
        ok(&reloaded.handle_request(&json!({"op":"open","path":path.to_string_lossy()})));
        let _ = std::fs::remove_file(&path);
        let loaded = ok(&reloaded.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(loaded["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
        assert_eq!(loaded["uvs"], loaded["vertices"]);
        assert_eq!(loaded["indices"], json!([0, 1, 2]));
        let held = ok(&reloaded.handle_request(&json!({"op":"snapshot"})));
        let same = reloaded.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Part"}));
        assert_eq!(same["succeeded"], false, "{same}");
        assert_eq!(ok(&reloaded.handle_request(&json!({"op":"snapshot"}))), held);
        let away = reloaded.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Node"}));
        assert_eq!(away["succeeded"], false, "{away}");
        assert!(away["message"].as_str().unwrap().contains("unsupported"), "{away}");
        let kept = ok(&reloaded.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(kept["type"], "Part");
        assert_eq!(kept["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
        assert_eq!(kept["indices"], json!([0, 1, 2]));
        assert_eq!(ok(&reloaded.handle_request(&json!({"op":"snapshot"}))), held);

        let bare = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Bare", "type": "Part"
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":bare})));
        let opened = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(opened["vertices"], json!([]));
        assert_eq!(opened["uvs"], json!([]));
        assert_eq!(opened["indices"], json!([]));

        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[grid_id],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        let path_node = ok(&session.handle_request(&json!({"op":"node.create","className":"PathDeformer","name":"Path"})));
        let path_id = path_node["created"][0]["uuid"].as_u64().unwrap();
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[path_id],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let bad = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Bad", "type": "Part",
                "vertices": [[0.0, 0.0]],
                "uvs": [[0.0, 0.0]],
                "indices": [1, 0, 0]
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":bad})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Node"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("index past"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let mask = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                "uuid": 2, "name": "Cover", "type": "Mask",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "opacity": 0.25, "blend_mode": "Normal", "tint": [0.2, 0.4, 0.6], "screenTint": [0.0, 0.0, 0.0],
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [4.0, -2.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":mask})));
        ok(&session.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Part"})));
        let kept = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(kept["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
        assert_eq!(kept["mesh"]["verts"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        assert_eq!(kept["mesh"]["origin"], json!([4.0, -2.0]));
        assert_eq!(kept["opacity"], 0.25);
        assert_eq!(kept["blend_mode"], "Normal");
        assert_eq!(kept["tint"], json!([0.2, 0.4, 0.6]));
        assert_eq!(kept["screenTint"], json!([0.0, 0.0, 0.0]));
        let bounds = ok(&session.handle_request(&json!({"op":"bounds"})));
        assert_eq!(bounds["bounds"], json!([-4.1, 1.9, -2.9, 3.1]));

        let dynamic = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                "uuid": 4, "name": "Layer", "type": "DynamicComposite",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "mesh": {"verts": [0.0, 0.0, 2.0, 0.0, 0.0, 2.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [1.0, 0.5]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":dynamic})));
        ok(&session.handle_request(&json!({"op":"node.convert","nodes":[4],"className":"Part"})));
        let layer = ok(&session.handle_request(&json!({"op":"node.get","id":4})));
        assert_eq!(layer["type"], "Part");
        assert_eq!(layer["vertices"], json!([[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]]));
        assert_eq!(layer["indices"], json!([0, 1, 2]));
        assert_eq!(layer["mesh"]["origin"], json!([1.0, 0.5]));
        let bounds = ok(&session.handle_request(&json!({"op":"bounds"})));
        assert_eq!(bounds["bounds"], json!([-1.2, -0.7, 1.2, 1.7]));

        let shifted = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Cover", "type": "Mask",
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [null, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":shifted})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("origin"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let legacy = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Legacy", "type": "Part",
                "mesh": {"verts": [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0], "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0], "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":legacy})));
        let filled = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(filled["vertices"], json!([[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]]));
        assert_eq!(filled["uvs"], json!([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]));
        assert_eq!(filled["indices"], json!([0, 1, 2, 0, 2, 3]));
        assert_eq!(filled["mesh"]["origin"], json!([0.0, 0.0]));

        let partial = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                "uuid": 2, "name": "Partial", "type": "Part",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "origin": [1.0, 2.0], "note": "keep"}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":partial})));
        let filled = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(filled["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
        assert_eq!(filled["uvs"], filled["vertices"]);
        assert_eq!(filled["indices"], json!([]));
        assert_eq!(filled["mesh"]["uvs"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        assert_eq!(filled["mesh"]["indices"], json!([]));
        assert_eq!(filled["mesh"]["origin"], json!([1.0, 2.0]));
        assert_eq!(filled["mesh"]["note"], "keep");
        ok(&session.handle_request(&json!({"op":"render","width":2,"height":2,"bounds":[-1.0,-1.0,1.0,1.0]})));

        let textured = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 3, "name": "Cover", "type": "Mask",
                "textures": [0],
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [0.0, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":textured})));
        ok(&session.handle_request(&json!({"op":"node.convert","nodes":[3],"className":"Part"})));
        let cover = ok(&session.handle_request(&json!({"op":"node.get","id":3})));
        assert_eq!(cover["type"], "Part");
        assert_eq!(cover["textures"], json!([0]));
        assert_eq!(cover["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));

        let welded = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 3, "name": "Cover", "type": "Mask",
                "textures": [0],
                "weldedLinks": [{"target": 1}],
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [0.0, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":welded})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[3],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let masked = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 3, "name": "Cover", "type": "Mask",
                "masked_by": [1],
                "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [0.0, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":masked})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[3],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"node.get","id":3})))["type"], "Mask");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let incomplete = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 3, "name": "Cover", "type": "Mask",
                "mesh": {"uvs": [0.0, 0.0], "indices": [], "origin": [3.0, -1.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":incomplete})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[3],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("verts"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let noted = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 4, "name": "Layer", "type": "DynamicComposite",
                "mesh": {
                    "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                    "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                    "indices": [0, 1, 2],
                    "origin": [3.0, -1.0],
                    "note": "keep"
                }
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":noted})));
        ok(&session.handle_request(&json!({"op":"node.convert","nodes":[4],"className":"Part"})));
        let layer = ok(&session.handle_request(&json!({"op":"node.get","id":4})));
        assert_eq!(layer["type"], "Part");
        assert_eq!(layer["mesh"]["note"], "keep");
        assert_eq!(layer["mesh"]["origin"], json!([3.0, -1.0]));
        assert_eq!(layer["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));

        let group = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 5, "name": "Group", "type": "MeshGroup",
                "mesh": {"verts": [0.0, 0.0, 2.0, 0.0, 0.0, 2.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2], "origin": [0.0, 0.0]}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":group})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[5],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("cannot convert to Part"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"node.get","id":5})))["type"], "MeshGroup");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        let diverged = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Split", "type": "Part",
                "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "indices": [0, 1, 2],
                "mesh": {"verts": [9.0, 9.0, 9.0, 9.0, 9.0, 9.0], "uvs": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0], "indices": [0, 0, 0], "origin": [4.0, -2.0], "note": "keep"}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":diverged})));
        let split = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(split["mesh"]["verts"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        assert_eq!(split["mesh"]["indices"], json!([0, 1, 2]));
        assert_eq!(split["mesh"]["origin"], json!([4.0, -2.0]));
        assert_eq!(split["mesh"]["note"], "keep");
        assert_eq!(split["vertices"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));

        let stale = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                "uuid": 2, "name": "Stale", "type": "Part",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "indices": [],
                "mesh": {"verts": [1.0], "origin": [4.0, -2.0], "note": "keep"}
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":stale})));
        let repaired = ok(&session.handle_request(&json!({"op":"node.get","id":2})));
        assert_eq!(repaired["mesh"]["verts"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        assert_eq!(repaired["mesh"]["uvs"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        assert_eq!(repaired["mesh"]["indices"], json!([]));
        assert_eq!(repaired["mesh"]["origin"], json!([4.0, -2.0]));
        assert_eq!(repaired["mesh"]["note"], "keep");
        ok(&session.handle_request(&json!({"op":"render","width":2,"height":2,"bounds":[-1.0,-1.0,1.0,1.0]})));

        let wide = json!({
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [{
                "uuid": 2, "name": "Wide", "type": "Part",
                "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                "indices": [65536, 0, 0]
            }]},
            "param": []
        });
        ok(&session.handle_request(&json!({"op":"new","model":wide})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"node.convert","nodes":[2],"className":"Part"}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("16-bit"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn paste_remaps_uuids_that_the_live_model_reused() {
        let model = sample_model();
        let copies = copy_nodes(&model, &[4]).unwrap();
        let stolen_root = copies[0].node["uuid"].as_u64().unwrap();
        let stolen_child = copies[0].node["children"][0]["uuid"].as_u64().unwrap();
        let mut collided = model.clone();
        collided["nodes"]["children"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "uuid": stolen_root, "name": "Stolen", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": {"trans":[0.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]},
                "lockToRoot": false, "pinToMesh": false, "children": []
            }));
        let (pasted, created) = paste_nodes(&collided, &copies, 1, Some(0)).unwrap();
        let pasted_root = created[0]["uuid"].as_u64().unwrap();
        let pasted_child = created[0]["children"][0]["uuid"].as_u64().unwrap();
        assert_ne!(pasted_root, stolen_root);
        assert_ne!(pasted_child, stolen_child);
        assert_ne!(pasted_root, pasted_child);
        let mut ids = HashSet::new();
        collect_node_ids(&pasted["nodes"], &mut ids);
        assert!(ids.contains(&(stolen_root as u32)));
        assert!(ids.contains(&(pasted_root as u32)));
        assert!(ids.contains(&(pasted_child as u32)));
    }

    #[test]
    fn convert_includes_explicitly_selected_descendants() {
        let model = sample_model();
        let (converted, nodes) = convert_nodes(&model, &[4, 5, 4], "Composite").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(find_node(&converted, 4).unwrap()["type"], "Composite");
        assert_eq!(find_node(&converted, 5).unwrap()["type"], "Composite");
        assert_eq!(find_node(&converted, 4).unwrap()["children"][0]["uuid"], 5);
        let (reversed, reversed_nodes) = convert_nodes(&model, &[5, 4], "MeshGroup").unwrap();
        assert_eq!(reversed_nodes.len(), 2);
        assert_eq!(find_node(&reversed, 4).unwrap()["type"], "MeshGroup");
        assert_eq!(find_node(&reversed, 5).unwrap()["type"], "MeshGroup");
    }

    fn call(session: &mut crate::Session, request: &serde_json::Value) -> serde_json::Value {
        session.handle_request(request)
    }

    fn ok(response: &serde_json::Value) -> serde_json::Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn deleting_a_rig_root_keeps_external_deform_and_one_undo() {
        let mut session = crate::Session::new();
        let rig_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let bone_id = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let grid_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":bone_id,
                "settings":{"depthOffset":0.25}
            }),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_influence_rule","root":rig_id,"target":grid_id,
                "rule":{"multipliersByBoneUuid":{bone_id.to_string():0.5}}
            }),
        ));
        let parameter_id = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ))["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":0.4
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let deform = before["model"]["param"][0]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform")
            .unwrap()
            .clone();
        let deleted = call(&mut session, &json!({"op":"node.delete","nodes":[rig_id]}));
        assert_eq!(deleted["succeeded"], true, "{deleted}");
        assert_eq!(call(&mut session, &json!({"op":"node.get","id":rig_id}))["succeeded"], false);
        assert_eq!(call(&mut session, &json!({"op":"node.get","id":bone_id}))["succeeded"], false);
        let grid = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(grid["type"], "GridDeformer");
        let parameter = ok(&call(&mut session, &json!({"op":"parameter.get","id":parameter_id})));
        let bindings = parameter["bindings"].as_array().unwrap();
        assert!(bindings.iter().all(|binding| binding["node"] != bone_id));
        assert_eq!(
            bindings.iter().find(|binding| binding["param_name"] == "deform").unwrap(),
            &deform
        );
        let root_delete = call(&mut session, &json!({"op":"node.delete","nodes":[1]}));
        assert_eq!(root_delete["succeeded"], false, "{root_delete}");
        ok(&call(&mut session, &json!({"op":"undo"})));
        let restored = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(restored["model"], before["model"]);
        let sources = ok(&call(
            &mut session,
            &json!({"op":"rig.list_sources","root":rig_id,"target":grid_id}),
        ));
        assert_eq!(sources["sources"][0]["depthOffset"], json!(0.25));
        let rule = ok(&call(
            &mut session,
            &json!({"op":"rig.get_influence_rule","root":rig_id,"target":grid_id}),
        ));
        assert_eq!(rule["multipliersByBoneUuid"][&bone_id.to_string()], json!(0.5));
        ok(&call(&mut session, &json!({"op":"redo"})));
        assert_eq!(call(&mut session, &json!({"op":"node.get","id":rig_id}))["succeeded"], false);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"parameter.get","id":parameter_id})))["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|binding| binding["param_name"] == "deform")
                .unwrap(),
            &deform
        );

        ok(&call(&mut session, &json!({"op":"undo"})));
        let cut = call(&mut session, &json!({"op":"node.cut","nodes":[rig_id]}));
        assert_eq!(cut["succeeded"], true, "{cut}");
        assert_eq!(call(&mut session, &json!({"op":"node.get","id":bone_id}))["succeeded"], false);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"parameter.get","id":parameter_id})))["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|binding| binding["param_name"] == "deform")
                .unwrap(),
            &deform
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before["model"]);
    }

    #[test]
    fn moving_a_bound_bone_keeps_bindings_and_deform() {
        let mut session = crate::Session::new();
        let part = ok(&call(&mut session, &json!({
            "op":"new",
            "model": {
                "nodes": {
                    "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                    "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                    "children": [{
                        "uuid": 2, "name": "Card", "type": "Part", "enabled": true, "zsort": 0.0,
                        "transform": {"trans":[10.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                        "children": []
                    }]
                },
                "param": []
            }
        })));
        let _ = part;
        let rig = ok(&call(&mut session, &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(&mut session, &json!({
            "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
            "restHead":[0.0,0.0,0.0],"restTail":[0.0,10.0,0.0]
        })));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(&mut session, &json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(&mut session, &json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&call(&mut session, &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id})));
        let bindings = ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        let deform = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"].clone();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(&mut session, &json!({"op":"node.move","nodes":[bone_id],"parent":2})));
        let rig_after = ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})));
        assert_eq!(rig_after["bindings"], bindings);
        assert!(rig_after["children"].as_array().unwrap().iter().all(|child| child["uuid"] != bone_id));
        let part_node = ok(&call(&mut session, &json!({"op":"node.get","id":2})));
        assert!(part_node["children"].as_array().unwrap().iter().any(|child| child["uuid"] == bone_id));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"], deform);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before["model"]);
        let root_move = call(&mut session, &json!({"op":"node.move","nodes":[1],"parent":rig_id}));
        assert_eq!(root_move["succeeded"], false, "{root_move}");
        let cycle = call(&mut session, &json!({"op":"node.move","nodes":[rig_id],"parent":bone_id}));
        assert_eq!(cycle["succeeded"], false, "{cycle}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before["model"]);
    }

    #[test]
    fn moving_a_pitched_bone_keeps_depth_rotation_and_bindings() {
        let mut session = crate::Session::new();
        ok(&call(&mut session, &json!({
            "op":"new",
            "model": {
                "nodes": {
                    "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                    "transform": {"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                    "children": [{
                        "uuid": 2, "name": "Card", "type": "Part", "enabled": true, "zsort": 0.0,
                        "transform": {"trans":[10.0,0.0,4.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},
                        "children": []
                    }]
                },
                "param": []
            }
        })));
        let rig = ok(&call(&mut session, &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(&mut session, &json!({
            "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
            "restHead":[0.0,0.0,0.0],"restTail":[0.0,10.0,0.0]
        })));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(&mut session, &json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(&mut session, &json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&call(&mut session, &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id})));
        ok(&call(&mut session, &json!({
            "op":"node.transform","id":bone_id,
            "translation":[1.0,2.0,8.0],"rotation":[0.25,0.5,0.0],"scale":[1.0,1.0]
        })));
        let bindings = ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        let deform = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"].clone();
        ok(&call(&mut session, &json!({"op":"node.move","nodes":[bone_id],"parent":2})));
        let moved = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(moved["transform"]["trans"], json!([-9.0, 2.0, 4.0]));
        assert_eq!(moved["transform"]["rot"], json!([0.25, 0.5, 0.0]));
        assert_eq!(moved["transform"]["scale"], json!([1.0, 1.0]));
        assert_eq!(ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"], bindings);
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"], deform);
        let before_undo = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        ok(&call(&mut session, &json!({"op":"undo"})));
        let restored = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(restored["transform"]["trans"], json!([1.0, 2.0, 8.0]));
        assert_eq!(restored["transform"]["rot"], json!([0.25, 0.5, 0.0]));
        ok(&call(&mut session, &json!({"op":"redo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before_undo);
    }

    #[test]
    fn moving_a_grid_with_a_bone_still_refreshes_that_grid() {
        let mut session = crate::Session::new();
        ok(&call(&mut session, &json!({"op":"new"})));
        let rig = ok(&call(&mut session, &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(&mut session, &json!({
            "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
            "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
        })));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(&mut session, &json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(&mut session, &json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&call(&mut session, &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id})));
        let parameter = ok(&call(&mut session, &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(&mut session, &json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
            "rotation_y":std::f64::consts::FRAC_PI_2
        })));
        ok(&call(&mut session, &json!({"op":"node.transform","id":grid_id,"translation":[0.0,0.0,5.0]})));
        let bindings = ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"].clone();
        let raised = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"].clone();
        ok(&call(&mut session, &json!({"op":"node.move","nodes":[rig_id, grid_id],"parent":1})));
        let deform = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"].clone();
        assert_ne!(deform, raised);
        assert_eq!(ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["bindings"], bindings);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})))["transform"]["trans"][2],
            0.0
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"].clone();
        ok(&call(&mut session, &json!({"op":"node.move","nodes":[rig_id],"parent":1})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"]["param"], held);
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    fn node(uuid: u64, name: &str, kind: &str, children: Value) -> Value {
        json!({
            "uuid": uuid, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
            "transform": {"trans":[0.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": children,
        })
    }

    /// 1 Root: 2 A (5, 6), 3 B (7), 4 M (a Mask), 8 D.
    fn panel_model() -> Value {
        json!({
            "meta": {"name": "panel", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", json!([
                node(2, "A", "Node", json!([node(5, "A1", "Node", json!([])), node(6, "A2", "Node", json!([]))])),
                node(3, "B", "Composite", json!([node(7, "B1", "Node", json!([]))])),
                node(4, "M", "Mask", json!([])),
                node(8, "D", "Node", json!([])),
            ])),
            "param": [],
            "animations": {},
        })
    }

    fn enabled_of(model: &Value, id: u32) -> Option<bool> {
        find_node(model, id).and_then(|node| node.get("enabled")).and_then(Value::as_bool)
    }

    fn child_order(model: &Value, id: u32) -> Vec<u32> {
        child_ids(model, id)
    }

    #[test]
    fn visibility_toggle_applies_the_first_nodes_new_state_to_every_node() {
        let mut model = panel_model();
        find_node_mut(&mut model, 3).unwrap()["enabled"] = json!(false);
        let (candidate, result) = set_visibility(&model, &json!({"nodes": [2, 3]})).unwrap();
        assert_eq!(result, json!({"nodes": [2, 3], "enabled": false, "recursive": false}));
        assert_eq!(enabled_of(&candidate, 2), Some(false));
        assert_eq!(enabled_of(&candidate, 3), Some(false));
        assert_eq!(enabled_of(&candidate, 5), Some(true), "children stay without recursion");
        // Listed the other way round, the hidden node decides: both are shown.
        let (candidate, result) = set_visibility(&model, &json!({"ids": [3, 2]})).unwrap();
        assert_eq!(result["enabled"], true);
        assert_eq!(enabled_of(&candidate, 3), Some(true));
        assert_eq!(enabled_of(&candidate, 2), Some(true));
    }

    #[test]
    fn recursive_visibility_covers_the_subtree_and_reports_every_node() {
        let model = panel_model();
        let (hidden, result) =
            set_visibility(&model, &json!({"id": 2, "enabled": false, "recursive": true})).unwrap();
        assert_eq!(result, json!({"nodes": [2, 5, 6], "enabled": false, "recursive": true}));
        for id in [2, 5, 6] {
            assert_eq!(enabled_of(&hidden, id), Some(false), "node {id}");
        }
        assert_eq!(enabled_of(&hidden, 3), Some(true));
        let (shown, _) = set_visibility(&hidden, &json!({"id": 2, "enabled": true, "recursive": true})).unwrap();
        for id in [2, 5, 6] {
            assert_eq!(enabled_of(&shown, id), Some(true), "node {id}");
        }
    }

    #[test]
    fn sibling_visibility_covers_the_level_of_each_listed_node() {
        let mut model = panel_model();
        find_node_mut(&mut model, 3).unwrap()["enabled"] = json!(false);
        // The level of A is every child of the root. A is shown, so the toggle hides them all, B included.
        let (hidden, result) = set_visibility(&model, &json!({"id": 2, "siblings": true})).unwrap();
        assert_eq!(result, json!({"nodes": [2, 3, 4, 8], "enabled": false, "recursive": false}));
        for id in [2, 3, 4, 8] {
            assert_eq!(enabled_of(&hidden, id), Some(false), "node {id}");
        }
        for id in [1, 5, 6, 7] {
            assert_eq!(enabled_of(&hidden, id), Some(true), "node {id} is on another level");
        }
        // The listed node decides, not the first node of its level: B is hidden, so the level is shown.
        let (shown, result) = set_visibility(&model, &json!({"id": 3, "siblings": true})).unwrap();
        assert_eq!(result["enabled"], true);
        assert_eq!(enabled_of(&shown, 3), Some(true));
        // Two levels in one call, each listed once, and the subtrees with `recursive`.
        let request = json!({"nodes": [5, 7, 6], "siblings": true, "recursive": true, "enabled": false});
        let (hidden, result) = set_visibility(&model, &request).unwrap();
        assert_eq!(result, json!({"nodes": [5, 6, 7], "enabled": false, "recursive": true}));
        assert_eq!(enabled_of(&hidden, 2), Some(true));
        // Every child of the root with its subtree is the whole model below the root.
        let request = json!({"id": 8, "siblings": true, "recursive": true, "enabled": false});
        let (_, result) = set_visibility(&model, &request).unwrap();
        assert_eq!(result["nodes"], json!([2, 5, 6, 3, 7, 4, 8]));
    }

    #[test]
    fn sibling_visibility_runs_through_the_mcp_tool_as_one_undo_step() {
        let mut session = crate::Session::new();
        let loaded = session.handle_request(&json!({"op": "new", "model": panel_model()}));
        assert_eq!(loaded["succeeded"], true, "{loaded}");
        let count = session.snapshot()["history"]["count"].as_u64().unwrap();
        let mut call = |arguments: Value| {
            session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "node_visibility_set", "arguments": arguments}}))
                .unwrap()
        };
        let response = call(json!({"nodes": [5], "siblings": true, "enabled": false}));
        assert_eq!(response["result"]["isError"], false, "{response}");
        let refused = call(json!({"nodes": [1], "siblings": true}));
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert_eq!(refused["result"]["content"][0]["text"], "the root node has no siblings");
        let after = session.snapshot();
        assert_eq!(enabled_of(&after["model"], 5), Some(false));
        assert_eq!(enabled_of(&after["model"], 6), Some(false));
        assert_eq!(enabled_of(&after["model"], 2), Some(true));
        assert_eq!(after["history"]["count"].as_u64(), Some(count + 1));
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(enabled_of(&session.snapshot()["model"], 6), Some(true));
    }

    #[test]
    fn visibility_no_op_returns_an_identical_model() {
        let model = panel_model();
        let (candidate, _) = set_visibility(&model, &json!({"nodes": [2, 5], "enabled": true})).unwrap();
        assert_eq!(candidate, model);
    }

    #[test]
    fn visibility_rejects_bad_requests() {
        let model = panel_model();
        for request in [
            json!({"nodes": []}),
            json!({"nodes": [2, 99]}),
            json!({"nodes": [0]}),
            json!({"nodes": [2], "enabled": "no"}),
            json!({"nodes": [2], "recursive": 1}),
            json!({"nodes": [2], "siblings": "yes"}),
            json!({"nodes": [1], "siblings": true}),
            json!({"nodes": [2, 1], "siblings": true}),
            json!({}),
        ] {
            assert!(set_visibility(&model, &request).is_err(), "{request}");
        }
    }

    #[test]
    fn visibility_is_one_undo_step_in_a_session() {
        let mut session = crate::Session::new();
        let loaded = session.handle_request(&json!({"op": "new", "model": panel_model()}));
        assert_eq!(loaded["succeeded"], true, "{loaded}");
        let before = session.snapshot();
        let response =
            session.handle_node_visibility(&json!({"nodes": [2, 3], "enabled": false, "recursive": true}));
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(response["result"]["nodes"], json!([2, 5, 6, 3, 7]));
        let after = session.snapshot();
        for id in [2, 5, 6, 3, 7] {
            assert_eq!(enabled_of(&after["model"], id), Some(false), "node {id}");
        }
        assert_eq!(after["history"]["count"].as_u64(), Some(before["history"]["count"].as_u64().unwrap() + 1));
        assert_eq!(after["history"]["states"][after["history"]["position"].as_u64().unwrap() as usize]["label"],
            "node.visibility.set");
        let undone = session.handle_request(&json!({"op": "undo"}));
        assert_eq!(undone["succeeded"], true, "{undone}");
        let undone_state = session.snapshot();
        assert_eq!(undone_state["model"], before["model"]);

        // A no-op keeps the redo branch, and a bad id leaves everything alone.
        let noop = session.handle_node_visibility(&json!({"nodes": [2], "enabled": true}));
        assert_eq!(noop["succeeded"], true);
        assert_eq!(session.snapshot(), undone_state);
        let bad = session.handle_node_visibility(&json!({"nodes": [2, 99], "enabled": false}));
        assert_eq!(bad["succeeded"], false);
        assert_eq!(session.snapshot(), undone_state);
    }

    #[test]
    fn parameter_usage_lists_bound_parameters_in_group_order() {
        let mut model = panel_model();
        model["param"] = json!([
            {"uuid": 20, "name": "Eyes", "bindings": [{"node": 5, "param_name": "transform.t.x"}]},
            {"groupUUID": 30, "name": "Group", "children": [
                {"uuid": 21, "name": "Mouth", "bindings": [{"node": 5}, {"node": 6}]},
                {"uuid": 22, "name": "Other", "bindings": [{"node": 6}]},
            ]},
            {"uuid": 23, "name": "Unbound", "bindings": []},
        ]);
        let names = |id| {
            parameter_usage(&model, id)
                .into_iter()
                .map(|usage| (usage.uuid, usage.name))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(5), [(20, "Eyes".to_owned()), (21, "Mouth".to_owned())]);
        assert_eq!(names(6), [(21, "Mouth".to_owned()), (22, "Other".to_owned())]);
        assert_eq!(names(99), []);
    }

    #[test]
    fn multi_node_move_keeps_tree_order_not_uuid_order() {
        // Tree order is 20 then 15; UUID order is the other way round.
        let model = json!({
            "nodes": node(1, "Root", "Node", json!([
                node(10, "X", "Node", json!([node(20, "X1", "Node", json!([]))])),
                node(11, "Y", "Node", json!([node(15, "Y1", "Node", json!([]))])),
            ])),
            "param": [],
        });
        let moved = move_nodes(&model, &[15, 20], 1, Some(0)).unwrap();
        assert_eq!(child_order(&moved, 1), [20, 15, 10, 11]);
        let moved = move_nodes(&model, &[20, 15], 1, None).unwrap();
        assert_eq!(child_order(&moved, 1), [10, 11, 20, 15]);
    }

    #[test]
    fn drop_onto_a_row_makes_the_selection_its_first_children() {
        let model = panel_model();
        assert_eq!(resolve_drop(&model, &[6], DropSite::Onto(3)), Ok(Some((3, 0))));
        // Given out of tree order and with a nested pair: the pair collapses to its top node.
        assert_eq!(resolve_drop(&model, &[7, 5, 2], DropSite::Onto(8)), Ok(Some((8, 0))));
        // The root accepts drops.
        assert_eq!(resolve_drop(&model, &[7], DropSite::Onto(1)), Ok(Some((1, 0))));
    }

    #[test]
    fn drop_between_rows_counts_the_index_after_the_dragged_nodes_left() {
        let model = panel_model();
        assert_eq!(resolve_drop(&model, &[6], DropSite::Before(5)), Ok(Some((2, 0))));
        assert_eq!(resolve_drop(&model, &[5], DropSite::After(6)), Ok(Some((2, 1))));
        assert_eq!(resolve_drop(&model, &[7], DropSite::Before(3)), Ok(Some((1, 1))));
        // Down the root's list: 2 leaves its slot, so "before 8" lands at index 2 of [3, 4, 8].
        assert_eq!(resolve_drop(&model, &[2, 5], DropSite::Before(8)), Ok(Some((1, 2))));
        assert_eq!(resolve_drop(&model, &[4, 3], DropSite::After(8)), Ok(Some((1, 2))));
    }

    #[test]
    fn drop_that_changes_nothing_resolves_to_none() {
        let model = panel_model();
        assert_eq!(resolve_drop(&model, &[5], DropSite::Before(5)), Ok(None));
        assert_eq!(resolve_drop(&model, &[5], DropSite::After(5)), Ok(None));
        assert_eq!(resolve_drop(&model, &[5], DropSite::Before(6)), Ok(None));
        assert_eq!(resolve_drop(&model, &[5, 6], DropSite::Before(5)), Ok(None));
        assert_eq!(resolve_drop(&model, &[8], DropSite::After(4)), Ok(None));
    }

    #[test]
    fn illegal_drops_are_rejected_with_a_reason() {
        let model = panel_model();
        let message = |moving: &[u32], site| resolve_drop(&model, moving, site).unwrap_err();
        assert_eq!(message(&[2], DropSite::Onto(2)), "node 2 cannot be dropped into itself or its descendants");
        assert_eq!(message(&[2], DropSite::Onto(5)), "node 2 cannot be dropped into itself or its descendants");
        assert_eq!(message(&[2], DropSite::Before(5)), "node 2 cannot be dropped into itself or its descendants");
        assert_eq!(message(&[1], DropSite::Onto(8)), "the root node cannot be moved");
        assert_eq!(message(&[5, 1], DropSite::Onto(8)), "the root node cannot be moved");
        assert_eq!(message(&[5], DropSite::Before(1)), "nothing can be dropped beside the root");
        assert_eq!(message(&[5], DropSite::After(1)), "nothing can be dropped beside the root");
        assert_eq!(message(&[5], DropSite::Onto(4)), "Mask nodes cannot accept children");
        assert_eq!(message(&[5], DropSite::Onto(99)), "parent node 99 was not found");
        assert_eq!(message(&[99], DropSite::Onto(8)), "node 99 was not found");
        assert_eq!(message(&[], DropSite::Onto(8)), "at least one node UUID is required");
    }

    #[test]
    fn a_resolved_drop_moves_the_nodes_exactly_there() {
        let model = panel_model();
        let (parent, index) = resolve_drop(&model, &[2, 3], DropSite::After(8)).unwrap().unwrap();
        let moved = move_nodes(&model, &[2, 3], parent, Some(index)).unwrap();
        assert_eq!(child_order(&moved, 1), [4, 8, 2, 3]);
        let (parent, index) = resolve_drop(&model, &[8, 6], DropSite::Before(5)).unwrap().unwrap();
        let moved = move_nodes(&model, &[8, 6], parent, Some(index)).unwrap();
        assert_eq!(child_order(&moved, 2), [6, 8, 5]);
    }
}
