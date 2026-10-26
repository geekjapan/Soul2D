//! Inspector panel of the editor: the property editors of D `panels/inspector/*`.
//!
//! `node_sections` and `puppet_sections` are the view model: which fields a node type shows and, via
//! `edit_request`, the session request each edit sends. `inspector_section` only draws them. Ranges and
//! choices come from the `node.property.set` table, so the panel cannot offer a value the session refuses.

use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::{properties_for, property_value, puppet_property, PropertyKind, MASKED_TYPES, MASK_SOURCE_TYPES, PUPPET_PROPERTIES};

const F32_MAX: f64 = f32::MAX as f64;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Plain,
    Percent,
    Degrees,
}

#[derive(Clone, Debug, PartialEq)]
struct Numbers {
    values: Vec<f64>,
    /// One name per value when there is more than one ("R", "G", "B").
    names: &'static [&'static str],
    min: f64,
    max: f64,
    speed: f64,
    unit: Unit,
}

#[derive(Clone, Debug, PartialEq)]
enum Control {
    Flag(bool),
    Numbers(Numbers),
    Choice { value: String, choices: Vec<String> },
    Pick { value: Option<u64>, options: Vec<(u64, String)>, none: &'static str },
    Text(String),
}

/// Where an edit goes. Each variant maps to exactly one session operation in `edit_request`.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Property { id: u64, name: &'static str },
    Transform { id: u64, key: &'static str },
    Physics { id: u64, key: &'static str },
    Deformer { id: u64, property: &'static str },
    Puppet { property: &'static str },
    Constraint { bone: u64, key: &'static str },
    BoneRest { bone: u64, rest: [Value; 3], slot: usize },
    Weld { part: u64, target: u64, indices: Value },
    MaskAdd { id: u64 },
    WeldAdd { part: u64 },
}

#[derive(Clone, Debug, PartialEq)]
struct Field {
    key: String,
    label: String,
    control: Control,
    target: Target,
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Field(Field),
    Note(String),
    Action { label: String, request: Value },
    Row { text: String, actions: Vec<(String, Value)> },
}

#[derive(Clone, Debug, PartialEq)]
struct Section {
    title: String,
    items: Vec<Item>,
}

fn edit_request(target: &Target, value: Value) -> Value {
    match target {
        Target::Property { id, name } => {
            json!({"op": "node.property.set", "id": id, "property": name, "value": value})
        }
        Target::Transform { id, key } => json!({"op": "node.transform", "id": id, *key: value}),
        Target::Physics { id, key } => json!({"op": "physics.set", "node": id, *key: value}),
        Target::Deformer { id, property } => {
            json!({"op": "deformer.set", "id": id, "property": property, "value": value})
        }
        Target::Puppet { property } => json!({"op": "puppet.meta.set", "property": property, "value": value}),
        Target::Constraint { bone, key } => json!({"op": "rig.set_constraint", "bone": bone, "constraint": {*key: value}}),
        Target::BoneRest { bone, rest, slot } => {
            let mut rest = rest.clone();
            rest[*slot] = value;
            json!({"op": "rig.set_rest", "bone": bone, "restHead": rest[0], "restTail": rest[1], "restRoll": rest[2]})
        }
        Target::Weld { part, target, indices } => {
            json!({"op": "part.set_weld", "part": part, "target": target, "indices": indices, "weight": value})
        }
        Target::MaskAdd { id } => json!({"op": "node.mask.edit", "id": id, "action": "add", "source": value}),
        Target::WeldAdd { part } => json!({"op": "part.set_weld", "part": part, "target": value, "weight": 0.5}),
    }
}

// ---------------------------------------------------------------- view model

fn node_type(node: &Value) -> &str {
    node["type"].as_str().unwrap_or("Node")
}

fn number(node: &Value, key: &str, default: f64) -> f64 {
    node.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn numbers(node: &Value, key: &str, count: usize, default: f64) -> Vec<f64> {
    let list = node.get(key).and_then(Value::as_array);
    (0..count).map(|index| list.and_then(|list| list.get(index)).and_then(Value::as_f64).unwrap_or(default)).collect()
}

fn flag(node: &Value, key: &str, default: bool) -> bool {
    node.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn find<'a>(node: &'a Value, id: u64) -> Option<&'a Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find(child, id))
}

fn collect<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(node);
    for child in node["children"].as_array().into_iter().flatten() {
        collect(child, out);
    }
}

fn all_nodes(model: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    collect(&model["nodes"], &mut out);
    out
}

fn plain_parameters(list: &Value, out: &mut Vec<(u64, String)>) {
    for item in list.as_array().into_iter().flatten() {
        if item.get("groupUUID").is_some() {
            plain_parameters(&item["children"], out);
        } else if let Some(id) = item["uuid"].as_u64() {
            out.push((id, item["name"].as_str().unwrap_or("").to_owned()));
        }
    }
}

fn scalar(node: &Value, key: &str, default: f64, min: f64, max: f64, speed: f64, unit: Unit) -> Control {
    Control::Numbers(Numbers { values: vec![number(node, key, default)], names: &[], min, max, speed, unit })
}

fn default_value(name: &str) -> Value {
    match name {
        "tint" => json!([1.0, 1.0, 1.0]),
        "screenTint" => json!([0.0, 0.0, 0.0]),
        "emissionStrength" | "opacity" => json!(1.0),
        "mask_threshold" => json!(0.5),
        "blend_mode" => json!("Normal"),
        "alphaBlend" => json!("Over"),
        "propagate_meshgroup" => json!(true),
        "origin" => json!([0.0, 0.0]),
        "viewport" => json!([1920.0, 1080.0]),
        "zsort" => json!(0.0),
        _ => json!(false),
    }
}

/// A field for a `node.property.set` property, if the node type has it.
fn property_field(node: &Value, name: &'static str, label: &str) -> Option<Item> {
    let id = node["uuid"].as_u64()?;
    let spec = properties_for(node_type(node)).find(|spec| spec.name == name)?;
    let current = property_value(node, spec).cloned().unwrap_or_else(|| default_value(name));
    let list = |names: &'static [&'static str], min, max, speed| {
        let count = names.len();
        let values = (0..count).map(|index| current.get(index).and_then(Value::as_f64).unwrap_or(0.0)).collect();
        Control::Numbers(Numbers { values, names, min, max, speed, unit: Unit::Plain })
    };
    let control = match spec.kind {
        PropertyKind::Bool => Control::Flag(current.as_bool().unwrap_or(false)),
        PropertyKind::Number { min, max } => {
            let unit = if name == "emissionStrength" { Unit::Percent } else { Unit::Plain };
            Control::Numbers(Numbers {
                values: vec![current.as_f64().unwrap_or(0.0)],
                names: &[],
                min,
                max,
                speed: 0.01,
                unit,
            })
        }
        PropertyKind::Color => list(&["R", "G", "B"], 0.0, 1.0, 0.01),
        PropertyKind::Pair => list(&["X", "Y"], -F32_MAX, F32_MAX, 1.0),
        PropertyKind::Viewport => list(&["Width", "Height"], 2.0, F32_MAX, 2.0),
        PropertyKind::Choice(choices) => Control::Choice {
            value: current.as_str().unwrap_or("").to_owned(),
            choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        },
        PropertyKind::Text => Control::Text(current.as_str().unwrap_or("").to_owned()),
    };
    Some(Item::Field(Field {
        key: format!("{id}/{name}"),
        label: label.to_owned(),
        control,
        target: Target::Property { id, name },
    }))
}

fn field(id: u64, name: &str, label: &str, control: Control, target: Target) -> Item {
    Item::Field(Field { key: format!("{id}/{name}"), label: label.to_owned(), control, target })
}

fn deformer_flag(node: &Value, key: &str, default: bool, property: &'static str, label: &str) -> Item {
    let id = node["uuid"].as_u64().unwrap_or(0);
    field(id, property, label, Control::Flag(flag(node, key, default)), Target::Deformer { id, property })
}

fn transform_section(node: &Value) -> Section {
    let id = node["uuid"].as_u64().unwrap_or(0);
    let transform = &node["transform"];
    let vector = |key: &'static str, label: &str, count: usize, default: f64, names, speed, unit| {
        field(
            id,
            key,
            label,
            Control::Numbers(Numbers {
                values: numbers(transform, match key { "translation" => "trans", "rotation" => "rot", _ => "scale" }, count, default),
                names,
                min: -F32_MAX,
                max: F32_MAX,
                speed,
                unit,
            }),
            Target::Transform { id, key },
        )
    };
    let mut items = vec![
        vector("translation", "Translation", 3, 0.0, &["X", "Y", "Z"], 1.0, Unit::Plain),
        vector("rotation", "Rotation", 3, 0.0, &["X", "Y", "Z"], 0.01, Unit::Degrees),
        vector("scale", "Scale", 2, 1.0, &["X", "Y"], 0.01, Unit::Plain),
    ];
    items.extend(property_field(node, "lockToRoot", "Lock to Root Node"));
    items.extend(property_field(node, "pinToMesh", "Pin origin to parent mesh"));
    items.push(field(
        id,
        "zsort",
        "Sorting (z-sort)",
        scalar(node, "zsort", 0.0, -F32_MAX, F32_MAX, 0.01, Unit::Plain),
        Target::Transform { id, key: "zsort" },
    ));
    Section { title: "Transform".into(), items }
}

fn mask_items(model: &Value, node: &Value) -> Vec<Item> {
    let id = node["uuid"].as_u64().unwrap_or(0);
    let masks: Vec<&Value> = node["masks"].as_array().map(|list| list.iter().collect()).unwrap_or_default();
    let name_of = |source: u64| {
        find(&model["nodes"], source).and_then(|n| n["name"].as_str()).unwrap_or("(missing)").to_owned()
    };
    let mut items = vec![Item::Note("Mask Sources".into())];
    if masks.is_empty() {
        items.push(Item::Note("(No mask sources)".into()));
    }
    for (index, mask) in masks.iter().enumerate() {
        let Some(source) = mask["source"].as_u64() else { continue };
        let dodge = mask["mode"].as_str() == Some("DodgeMask");
        let name = name_of(source);
        let edit = |action: &str| json!({"op": "node.mask.edit", "id": id, "action": action, "source": source});
        let mut actions = vec![(
            format!("Make {name} {}", if dodge { "Mask" } else { "Dodge" }),
            json!({"op": "node.mask.edit", "id": id, "action": "mode", "source": source,
                "mode": if dodge { "Mask" } else { "DodgeMask" }}),
        )];
        if index > 0 {
            let mut up = edit("move");
            up["index"] = json!(index - 1);
            actions.push((format!("Move {name} up"), up));
        }
        if index + 1 < masks.len() {
            let mut down = edit("move");
            down["index"] = json!(index + 1);
            actions.push((format!("Move {name} down"), down));
        }
        actions.push((format!("Remove mask {name}"), edit("remove")));
        items.push(Item::Row { text: format!("{name} ({})", if dodge { "Dodge" } else { "Mask" }), actions });
    }
    let taken: Vec<u64> = masks.iter().filter_map(|mask| mask["source"].as_u64()).collect();
    let options: Vec<(u64, String)> = all_nodes(model)
        .into_iter()
        .filter(|candidate| MASK_SOURCE_TYPES.contains(&node_type(candidate)))
        .filter_map(|candidate| Some((candidate["uuid"].as_u64()?, candidate["name"].as_str()?.to_owned())))
        .filter(|(source, _)| *source != id && !taken.contains(source))
        .collect();
    if !options.is_empty() {
        items.push(field(id, "mask-add", "Add mask source", Control::Pick { value: None, options, none: "(choose)" }, Target::MaskAdd { id }));
    }
    items
}

fn weld_items(model: &Value, node: &Value) -> Vec<Item> {
    let part = node["uuid"].as_u64().unwrap_or(0);
    let mut items = vec![Item::Note("Welding".into())];
    let links = node["weldedLinks"].as_array().cloned().unwrap_or_default();
    if links.is_empty() {
        items.push(Item::Note("(No welded parts)".into()));
    }
    for link in &links {
        let Some(target) = link["targetUUID"].as_u64() else { continue };
        let name = find(&model["nodes"], target).and_then(|n| n["name"].as_str()).unwrap_or("(missing)").to_owned();
        items.push(field(
            part,
            &format!("weld-{target}"),
            &format!("Weld weight {name}"),
            scalar(link, "weight", 0.5, 0.0, 1.0, 0.01, Unit::Plain),
            Target::Weld { part, target, indices: link["indices"].clone() },
        ));
        items.push(Item::Action {
            label: format!("Remove weld {name}"),
            request: json!({"op": "part.clear_weld", "part": part, "target": target}),
        });
    }
    // D added a weld by dropping a Part onto this list; the pick welds vertices within 4 units of each other.
    let taken: Vec<u64> = links.iter().filter_map(|link| link["targetUUID"].as_u64()).collect();
    let options: Vec<(u64, String)> = all_nodes(model)
        .into_iter()
        .filter(|candidate| node_type(candidate) == "Part")
        .filter_map(|candidate| Some((candidate["uuid"].as_u64()?, candidate["name"].as_str()?.to_owned())))
        .filter(|(target, _)| *target != part && !taken.contains(target))
        .collect();
    if !options.is_empty() {
        items.push(field(part, "weld-add", "Add weld", Control::Pick { value: None, options, none: "(choose)" }, Target::WeldAdd { part }));
    }
    items
}

fn part_like_section(model: &Value, node: &Value, title: &str) -> Section {
    let mut items = Vec::new();
    items.extend(property_field(node, "tint", "Tint (Multiply)"));
    items.extend(property_field(node, "screenTint", "Tint (Screen)"));
    items.extend(property_field(node, "emissionStrength", "Emission Strength"));
    items.extend(property_field(node, "blend_mode", "Blending"));
    items.extend(property_field(node, "alphaBlend", "Alpha blending"));
    items.extend(property_field(node, "opacity", "Opacity"));
    items.extend(property_field(node, "mask_threshold", "Threshold"));
    items.extend(property_field(node, "auto_resized", "Resize automatically"));
    items.extend(property_field(node, "propagate_meshgroup", "Propagate MeshGroup"));
    if MASKED_TYPES.contains(&node_type(node)) {
        items.extend(mask_items(model, node));
    }
    if node_type(node) == "Part" {
        items.extend(weld_items(model, node));
    }
    Section { title: title.into(), items }
}

fn simple_physics_section(model: &Value, node: &Value) -> Section {
    let id = node["uuid"].as_u64().unwrap_or(0);
    let mut parameters = Vec::new();
    plain_parameters(&model["param"], &mut parameters);
    let param = node["param"].as_u64().filter(|param| parameters.iter().any(|(id, _)| id == param));
    let choice = |name: &'static str, label: &str, key: &'static str, choices: &[&str]| {
        field(
            id,
            name,
            label,
            Control::Choice {
                value: node[key].as_str().unwrap_or(choices[0]).to_owned(),
                choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
            },
            Target::Physics { id, key },
        )
    };
    let number_field = |key: &'static str, label: &str, default: f64, min: f64, max: f64| {
        field(id, key, label, scalar(node, key, default, min, max, 0.01, Unit::Plain), Target::Physics { id, key })
    };
    let scale = numbers(node, "output_scale", 2, 1.0);
    let scale_field = |key: &'static str, label: &str, value: f64| {
        field(
            id,
            key,
            label,
            Control::Numbers(Numbers { values: vec![value], names: &[], min: 0.0, max: F32_MAX, speed: 0.01, unit: Unit::Plain }),
            Target::Physics { id, key },
        )
    };
    Section {
        title: "SimplePhysics".into(),
        items: vec![
            field(id, "param", "Parameter", Control::Pick { value: param, options: parameters, none: "(unassigned)" },
                Target::Physics { id, key: "param" }),
            choice("model_type", "Type", "model_type", &["Pendulum", "SpringPendulum"]),
            choice("map_mode", "Mapping mode", "map_mode", &["AngleLength", "XY", "LengthAngle", "YX"]),
            field(id, "local_only", "Local Transform Lock", Control::Flag(flag(node, "local_only", false)),
                Target::Physics { id, key: "local_only" }),
            number_field("gravity", "Gravity scale", 1.0, -F32_MAX, F32_MAX),
            number_field("length", "Length", 100.0, 0.0, F32_MAX),
            number_field("frequency", "Resonant frequency", 1.0, 0.01, 30.0),
            number_field("angle_damping", "Damping (angle)", 0.5, 0.0, 5.0),
            number_field("length_damping", "Damping (length)", 0.5, 0.0, 5.0),
            scale_field("output_scale_x", "Output scale X", scale[0]),
            scale_field("output_scale_y", "Output scale Y", scale[1]),
        ],
    }
}

fn path_deformer_section(node: &Value) -> Section {
    let id = node["uuid"].as_u64().unwrap_or(0);
    let physics = node.get("physics").filter(|physics| physics.is_object());
    let mut items = vec![
        deformer_flag(node, "dynamic_deformation", false, "PathDeformDynamic", "Dynamic mode(slow)"),
        field(id, "PathDeformPhysicsEnabled", "Auto-Physics", Control::Flag(physics.is_some()),
            Target::Deformer { id, property: "PathDeformPhysicsEnabled" }),
        field(id, "PathDeformCurveType", "Curve type", Control::Choice {
            value: node["curve_type"].as_str().unwrap_or("Spline").to_owned(),
            choices: vec!["Bezier".into(), "Spline".into()],
        }, Target::Deformer { id, property: "PathDeformCurveType" }),
    ];
    if let Some(physics) = physics.filter(|physics| physics["type"].as_str() == Some("Pendulum")) {
        let row = |key: &str, property: &'static str, label: &str, min: f64, max: f64| {
            field(id, property, label, scalar(physics, key, 0.0, min, max, 0.01, Unit::Plain), Target::Deformer { id, property })
        };
        items.push(row("gravity", "PathDeformGravity", "Gravity scale", -F32_MAX, F32_MAX));
        items.push(row("restore_constant", "PathDeformRestoreConstant", "Restore force", 0.01, F32_MAX));
        items.push(row("damping", "PathDeformDamping", "Damping", 0.0, 5.0));
        items.push(row("input_scale", "PathDeformInputScale", "Input scale", 0.0, F32_MAX));
        items.push(row("propagate_scale", "PathDeformPropagateScale", "Propagate scale", 0.0, F32_MAX));
    }
    Section { title: "PathDeformer".into(), items }
}

fn grid_deformer_section(node: &Value) -> Section {
    let axis = |key: &str| node[key].as_array().map_or(0, Vec::len);
    let (columns, rows) = (axis("grid_axis_x"), axis("grid_axis_y"));
    let mut items = vec![deformer_flag(node, "dynamic", false, "GridDeformDynamic", "Dynamic Deformation (slower)")];
    if columns >= 2 && rows >= 2 {
        items.push(Item::Note(format!("Vertices: {}", columns * rows)));
        items.push(Item::Note(format!("Columns: {columns}")));
        items.push(Item::Note(format!("Rows: {rows}")));
    } else {
        items.push(Item::Note("Grid axes are not initialized.".into()));
    }
    Section { title: "GridDeformer".into(), items }
}

fn depth_root_section(node: &Value) -> Section {
    let root = node["uuid"].as_u64().unwrap_or(0);
    let mut nodes = Vec::new();
    collect(node, &mut nodes);
    let bones = nodes.iter().filter(|candidate| node_type(candidate) == "DepthBone").count();
    let bindings = node["bindings"].as_array().map_or(0, Vec::len);
    Section {
        title: "Depth Rig".into(),
        items: vec![
            Item::Note(format!("Bones: {bones}")),
            Item::Note(format!("Bindings: {bindings}")),
            Item::Action { label: "Fit Z to Depth".into(), request: json!({"op": "rig.fit_root_z", "root": root}) },
            Item::Action {
                label: "Add Standard Skeleton".into(),
                request: json!({"op": "rig.add_skeleton", "root": root, "scale": 1.0}),
            },
            Item::Action {
                label: "Add Standard Parameters".into(),
                request: json!({"op": "rig.add_standard_parameters", "root": root}),
            },
        ],
    }
}

fn depth_bone_section(node: &Value) -> Section {
    let bone = node["uuid"].as_u64().unwrap_or(0);
    let head = numbers(node, "restHead", 3, 0.0);
    let tail = numbers(node, "restTail", 3, 0.0);
    let roll = number(node, "restRoll", 0.0);
    let rest = [json!(head), json!(tail), json!(roll)];
    let vector = |slot: usize, name: &str, label: &str, values: Vec<f64>| {
        field(bone, name, label, Control::Numbers(Numbers {
            values, names: &["X", "Y", "Z"], min: -F32_MAX, max: F32_MAX, speed: 1.0, unit: Unit::Plain,
        }), Target::BoneRest { bone, rest: rest.clone(), slot })
    };
    let constraint = |key: &'static str, label: &str, default: bool| {
        field(bone, key, label, Control::Flag(flag(node, key, default)), Target::Constraint { bone, key })
    };
    Section {
        title: "Depth Bone".into(),
        items: vec![
            Item::Note(node["boneId"].as_str().unwrap_or("").to_owned()),
            Item::Action { label: "Fit Z to Depth".into(), request: json!({"op": "rig.fit_bone_z", "bone": bone}) },
            vector(0, "restHead", "Rest Head Position", head.clone()),
            vector(1, "restTail", "Rest Tail Position", tail.clone()),
            field(bone, "restRoll", "Rest Roll Angle", scalar(node, "restRoll", 0.0, -F32_MAX, F32_MAX, 0.01, Unit::Plain),
                Target::BoneRest { bone, rest: rest.clone(), slot: 2 }),
            constraint("lockRotation", "Lock Rotation", false),
            constraint("lockTranslation", "Lock Translation", false),
            constraint("allowParentToTargets", "Allow Parent to Targets", true),
        ],
    }
}

/// Sections for a non-root node, in the order the D inspector shows them.
fn node_sections(model: &Value, node: &Value) -> Vec<Section> {
    let mut sections = vec![transform_section(node)];
    // The Drawable inspector applies to every Drawable that stores a mesh, before the type's own one.
    if node["mesh"].is_object() {
        let items: Vec<Item> = property_field(node, "origin", "Texture Offset").into_iter().collect();
        if !items.is_empty() {
            sections.push(Section { title: "Drawable".into(), items });
        }
    }
    match node_type(node) {
        "Part" if node["mesh"]["indices"].as_array().is_none_or(Vec::is_empty) => sections.push(Section {
            title: "Part".into(),
            items: vec![Item::Note("Cannot inspect an unmeshed part".into())],
        }),
        "Part" | "DynamicComposite" => sections.push(part_like_section(model, node, "Part")),
        "Composite" => sections.push(part_like_section(model, node, "Composite")),
        "MeshGroup" => sections.push(Section {
            title: "MeshGroup".into(),
            items: vec![
                deformer_flag(node, "dynamic_deformation", false, "MeshGroupDynamic", "Dynamic Deformation (slower)"),
                deformer_flag(node, "translate_children", false, "MeshGroupTranslateChildren", "Translate origins"),
            ],
        }),
        "Camera" => {
            let mut items: Vec<Item> = property_field(node, "viewport", "Viewport").into_iter().collect();
            let size = numbers(node, "viewport", 2, 0.0);
            if size.iter().any(|side| side % 2.0 != 0.0) {
                items.push(Item::Note("Warning: Camera size must be divisible by 2".into()));
            }
            sections.push(Section { title: "Camera".into(), items });
        }
        "SimplePhysics" => sections.push(simple_physics_section(model, node)),
        "PathDeformer" => sections.push(path_deformer_section(node)),
        "GridDeformer" => sections.push(grid_deformer_section(node)),
        "DepthRigRoot" => sections.push(depth_root_section(node)),
        "DepthBone" => sections.push(depth_bone_section(node)),
        _ => {}
    }
    sections
}

const PUPPET_LABELS: &[(&str, &str)] = &[
    ("name", "Name"),
    ("artist", "Artist(s)"),
    ("rigger", "Rigger(s)"),
    ("contact", "Contact"),
    ("licenseURL", "License URL"),
    ("copyright", "Copyright"),
    ("reference", "Origin"),
    ("preservePixels", "Use Point Filtering"),
    ("pixelsPerMeter", "Pixels per meter"),
    ("gravity", "Gravity (m/s²)"),
];

/// Puppet metadata, shown when no node (or the root) is selected.
fn puppet_sections(model: &Value) -> Vec<Section> {
    let parts = all_nodes(model).into_iter().filter(|node| node_type(node) == "Part").count();
    let mut sections: Vec<(&str, Vec<Item>)> = vec![
        ("General Info", vec![Item::Note(format!("Part Count: {parts}"))]),
        ("Licensing", Vec::new()),
        ("Physics Globals", Vec::new()),
        ("Rendering Settings", Vec::new()),
    ];
    for property in PUPPET_PROPERTIES {
        let (store, kind) = puppet_property(property).expect("listed puppet property");
        let label = PUPPET_LABELS.iter().find(|(name, _)| name == property).map_or(*property, |(_, label)| *label);
        let current = &model[store][*property];
        let control = match kind {
            PropertyKind::Text => Control::Text(current.as_str().unwrap_or("").to_owned()),
            PropertyKind::Bool => Control::Flag(current.as_bool().unwrap_or(false)),
            PropertyKind::Number { min, max } => {
                let default = if *property == "gravity" { 9.8 } else { 1000.0 };
                Control::Numbers(Numbers {
                    values: vec![current.as_f64().unwrap_or(default)],
                    names: &[],
                    min,
                    max,
                    speed: if *property == "gravity" { 0.01 } else { 1.0 },
                    unit: Unit::Plain,
                })
            }
            _ => continue,
        };
        let group = match *property {
            "licenseURL" | "copyright" | "reference" => 1,
            "pixelsPerMeter" | "gravity" => 2,
            "preservePixels" => 3,
            _ => 0,
        };
        sections[group].1.push(Item::Field(Field {
            key: format!("puppet/{property}"),
            label: label.to_owned(),
            control,
            target: Target::Puppet { property },
        }));
    }
    let version = model["meta"]["version"].as_str().unwrap_or("");
    sections[0].1.insert(0, Item::Note(format!("soul2d_live Ver. {version}")));
    sections.into_iter().map(|(title, items)| Section { title: title.into(), items }).collect()
}

/// The sections for the selection: a node's, or the puppet's for no selection and for the root.
fn sections_for(snapshot: &Value, selected: Option<u64>) -> Vec<Section> {
    let model = &snapshot["model"];
    let root = model["nodes"]["uuid"].as_u64();
    match selected.filter(|id| Some(*id) != root).and_then(|id| find(&model["nodes"], id)) {
        Some(node) => node_sections(model, node),
        None => puppet_sections(model),
    }
}

// ---------------------------------------------------------------- drawing

fn display(unit: Unit, value: f64) -> String {
    match unit {
        Unit::Plain => format!("{value}"),
        Unit::Percent => format!("{:.0}%", value * 100.0),
        Unit::Degrees => format!("{:.2}°", value.to_degrees()),
    }
}

fn parse_display(unit: Unit, text: &str) -> Option<f64> {
    let text = text.trim();
    match unit {
        Unit::Plain => text.parse().ok(),
        Unit::Percent => text.trim_end_matches('%').trim().parse::<f64>().ok().map(|value| value / 100.0),
        Unit::Degrees => text.trim_end_matches('°').trim().parse::<f64>().ok().map(f64::to_radians),
    }
}

/// Edit numbers in place. A change is only returned when the drag or text edit ends, so one gesture is
/// one undo step; until then the draft lives in egui memory and the model is untouched.
fn draw_numbers(ui: &mut egui::Ui, key: &str, label: &str, spec: &Numbers) -> Option<Vec<f64>> {
    let id = egui::Id::new(("inspector-draft", key));
    let mut draft: Vec<f64> = ui
        .data(|data| data.get_temp::<Vec<f64>>(id))
        .filter(|draft| draft.len() == spec.values.len())
        .unwrap_or_else(|| spec.values.clone());
    let (mut touched, mut finished) = (false, false);
    let unit = spec.unit;
    ui.horizontal(|ui| {
        ui.label(label);
        for (index, value) in draft.iter_mut().enumerate() {
            let name = match spec.names.get(index) {
                Some(component) => format!("{label} {component}"),
                None => label.to_owned(),
            };
            let drag = egui::DragValue::new(value)
                .speed(spec.speed)
                .range(spec.min..=spec.max)
                .custom_formatter(move |value, _| display(unit, value))
                .custom_parser(move |text| parse_display(unit, text));
            let response = ui.add(drag);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, &name));
            touched |= response.dragged() || response.has_focus() || response.changed();
            finished |= response.drag_stopped() || response.lost_focus();
        }
    });
    if finished {
        ui.data_mut(|data| data.remove::<Vec<f64>>(id));
        return (draft != spec.values).then_some(draft);
    }
    if touched {
        ui.data_mut(|data| data.insert_temp(id, draft));
    }
    None
}

fn draw_text(ui: &mut egui::Ui, key: &str, label: &str, current: &str) -> Option<String> {
    let id = egui::Id::new(("inspector-draft", key));
    let mut draft = ui.data(|data| data.get_temp::<String>(id)).unwrap_or_else(|| current.to_owned());
    let mut result = None;
    ui.horizontal(|ui| {
        let title = ui.label(label);
        let response = ui.text_edit_singleline(&mut draft).labelled_by(title.id);
        if response.lost_focus() {
            ui.data_mut(|data| data.remove::<String>(id));
            result = (draft != current).then(|| draft.clone());
        } else if response.changed() {
            ui.data_mut(|data| data.insert_temp(id, draft.clone()));
        }
    });
    result
}

/// Draw one field; returns the value the user committed this frame.
fn draw_field(ui: &mut egui::Ui, field: &Field) -> Option<Value> {
    match &field.control {
        Control::Flag(value) => {
            let mut flag = *value;
            ui.checkbox(&mut flag, &field.label).changed().then(|| json!(flag))
        }
        Control::Numbers(spec) => {
            let values = draw_numbers(ui, &field.key, &field.label, spec)?;
            Some(if values.len() == 1 { json!(values[0]) } else { json!(values) })
        }
        Control::Choice { value, choices } => {
            let mut chosen = None;
            ui.push_id(&field.key, |ui| {
                egui::ComboBox::from_label(&field.label).selected_text(value).show_ui(ui, |ui| {
                    for choice in choices {
                        if ui.selectable_label(choice == value, choice).clicked() && choice != value {
                            chosen = Some(json!(choice));
                        }
                    }
                });
            });
            chosen
        }
        Control::Pick { value, options, none } => {
            let mut chosen = None;
            let shown = options.iter().find(|(id, _)| Some(*id) == *value).map_or(*none, |(_, name)| name.as_str());
            ui.push_id(&field.key, |ui| {
                egui::ComboBox::from_label(&field.label).selected_text(shown).show_ui(ui, |ui| {
                    if matches!(field.target, Target::Physics { .. })
                        && ui.selectable_label(value.is_none(), *none).clicked()
                        && value.is_some()
                    {
                        chosen = Some(Value::Null);
                    }
                    for (id, name) in options {
                        if ui.selectable_label(Some(*id) == *value, name).clicked() && Some(*id) != *value {
                            chosen = Some(json!(id));
                        }
                    }
                });
            });
            chosen
        }
        Control::Text(current) => draw_text(ui, &field.key, &field.label, current).map(|text| json!(text)),
    }
}

/// Draw the inspector of the selected node (the puppet's metadata when nothing, or the root, is selected)
/// and return the session requests the user triggered this frame. The model is never edited here.
pub fn inspector_section(ui: &mut egui::Ui, snapshot: &Value, selected: Option<u64>) -> Vec<Value> {
    let mut requests = Vec::new();
    if snapshot["documentFormat"].as_str() != Some("inx") {
        return requests;
    }
    let scope = selected.unwrap_or(0);
    ui.heading("Inspector");
    for section in sections_for(snapshot, selected) {
        egui::CollapsingHeader::new(&section.title).id_salt((scope, &section.title)).default_open(true).show(ui, |ui| {
            for item in &section.items {
                match item {
                    Item::Field(field) => {
                        if let Some(value) = draw_field(ui, field) {
                            requests.push(edit_request(&field.target, value));
                        }
                    }
                    Item::Note(text) => {
                        ui.label(text);
                    }
                    Item::Action { label, request } => {
                        if ui.button(label).clicked() {
                            requests.push(request.clone());
                        }
                    }
                    Item::Row { text, actions } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(text);
                            for (label, request) in actions {
                                if ui.small_button(label).clicked() {
                                    requests.push(request.clone());
                                }
                            }
                        });
                    }
                }
            }
        });
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul2d_core::Session;

    fn transform() -> Value {
        json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
    }

    fn node(uuid: u64, name: &str, node_type: &str, extra: Value) -> Value {
        let mut entry = json!({
            "uuid": uuid, "name": name, "type": node_type, "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": [],
        });
        for (key, value) in extra.as_object().cloned().unwrap_or_default() {
            entry[key] = value;
        }
        entry
    }

    fn mesh() -> Value {
        json!({"verts": [0.0, 0.0, 4.0, 0.0, 4.0, 4.0], "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
            "indices": [0, 1, 2], "origin": [0.0, 0.0]})
    }

    /// Ids: 1 root, 2 and 3 Parts, 4 Composite, 5 DynamicComposite, 6 MeshGroup, 7 Camera, 8 Mask,
    /// 9 SimplePhysics, 10 PathDeformer, 11 GridDeformer, 12 Node, 13 unmeshed Part.
    fn model() -> Value {
        let part = |id, name| {
            node(id, name, "Part", json!({
                "mesh": mesh(), "textures": [], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
                "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0,
            }))
        };
        let composite = |id, name, node_type, extra: Value| {
            let mut entry = node(id, name, node_type, json!({
                "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
                "mask_threshold": 0.5, "opacity": 1.0,
            }));
            for (key, value) in extra.as_object().cloned().unwrap_or_default() {
                entry[key] = value;
            }
            entry
        };
        let mut root = node(1, "Root", "Node", json!({}));
        root["children"] = json!([
            part(2, "Face"),
            part(3, "Hair"),
            composite(4, "Group", "Composite", json!({"propagate_meshgroup": true})),
            composite(5, "Dynamic", "DynamicComposite", json!({"emissionStrength": 1.0, "auto_resized": false})),
            node(6, "Mesh", "MeshGroup", json!({"dynamic_deformation": false, "translate_children": false})),
            node(7, "Cam", "Camera", json!({"viewport": [1921.0, 1080.0]})),
            node(8, "Cutout", "Mask", json!({"mesh": mesh()})),
            node(9, "Hair physics", "SimplePhysics", json!({
                "param": 4294967295u64, "model_type": "Pendulum", "map_mode": "AngleLength",
                "gravity": 1.0, "length": 100.0, "frequency": 1.0, "angle_damping": 0.5,
                "length_damping": 0.5, "output_scale": [1.0, 1.0], "local_only": false,
            })),
            node(10, "Path", "PathDeformer", json!({
                "curve_type": "Spline", "vertices": [0.0, 0.0, 10.0, 0.0], "dynamic_deformation": false,
            })),
            node(11, "Grid", "GridDeformer", json!({
                "grid_axis_x": [0.0, 1.0, 2.0], "grid_axis_y": [0.0, 1.0], "formation": "Bilinear", "dynamic": false,
            })),
            node(12, "Plain", "Node", json!({})),
            node(13, "Empty", "Part", json!({"mesh": {"verts": [], "indices": [], "origin": [0.0, 0.0]}})),
        ]);
        json!({
            "meta": {"name": "fixture", "version": "1.0-alpha", "artist": "Aoi"},
            "physics": {"pixelsPerMeter": 1000.0, "gravity": 9.8},
            "nodes": root, "param": [], "animations": {},
        })
    }

    fn run(session: &mut Session, request: Value) -> Value {
        let response = session.handle_request(&request);
        assert_eq!(response["succeeded"], true, "{request}: {response}");
        response["result"].clone()
    }

    /// The fixture plus a parameter, a weld, a mask and a depth rig, so every inspector has content.
    fn session() -> Session {
        let mut session = Session::new();
        run(&mut session, json!({"op": "new", "model": model()}));
        run(&mut session, json!({"op": "parameter.create", "dimensions": 1, "name": "Hair X"}));
        run(&mut session, json!({"op": "part.set_weld", "part": 2, "target": 3, "indices": [0, 1, 2], "weight": 0.5}));
        run(&mut session, json!({"op": "node.mask.edit", "id": 2, "action": "add", "source": 8}));
        run(&mut session, json!({"op": "node.mask.edit", "id": 2, "action": "add", "source": 4, "mode": "DodgeMask"}));
        let created = run(&mut session, json!({"op": "node.create", "className": "DepthRigRoot", "parents": [1]}));
        let root = created["created"][0]["uuid"].as_u64().unwrap();
        run(&mut session, json!({
            "op": "node.create", "className": "DepthBone", "name": "Spine", "parents": [root], "boneId": "Spine",
            "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 10.0, 0.0],
        }));
        session
    }

    fn node_ids(snapshot: &Value) -> Vec<u64> {
        all_nodes(&snapshot["model"]).iter().filter_map(|node| node["uuid"].as_u64()).collect()
    }

    fn sections(session: &Session, selected: Option<u64>) -> Vec<Section> {
        sections_for(&session.snapshot(), selected)
    }

    fn labels(sections: &[Section]) -> Vec<String> {
        sections
            .iter()
            .flat_map(|section| &section.items)
            .filter_map(|item| match item {
                Item::Field(field) => Some(field.label.clone()),
                Item::Action { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    fn titles(sections: &[Section]) -> Vec<&str> {
        sections.iter().map(|section| section.title.as_str()).collect()
    }

    #[test]
    fn each_node_type_shows_the_fields_of_its_d_inspector() {
        let session = session();
        let transform = ["Translation", "Rotation", "Scale", "Lock to Root Node", "Pin origin to parent mesh", "Sorting (z-sort)"];
        let expect = |id: u64, want_titles: &[&str], own: &[&str]| {
            let sections = sections(&session, Some(id));
            assert_eq!(titles(&sections), want_titles, "node {id}");
            let want: Vec<String> = transform.iter().chain(own).map(|label| (*label).to_owned()).collect();
            assert_eq!(labels(&sections), want, "node {id}");
        };
        expect(12, &["Transform"], &[]);
        expect(2, &["Transform", "Drawable", "Part"], &[
            "Texture Offset", "Tint (Multiply)", "Tint (Screen)", "Emission Strength", "Blending", "Alpha blending",
            "Opacity", "Threshold", "Add mask source", "Weld weight Hair", "Remove weld Hair", "Add weld",
        ]);
        expect(4, &["Transform", "Composite"], &[
            "Tint (Multiply)", "Tint (Screen)", "Blending", "Alpha blending", "Opacity", "Threshold",
            "Propagate MeshGroup", "Add mask source",
        ]);
        // The fixture's DynamicComposite and MeshGroup store no mesh, so they have no texture offset.
        expect(5, &["Transform", "Part"], &[
            "Tint (Multiply)", "Tint (Screen)", "Emission Strength", "Blending", "Alpha blending", "Opacity",
            "Threshold", "Resize automatically", "Add mask source",
        ]);
        expect(6, &["Transform", "MeshGroup"], &["Dynamic Deformation (slower)", "Translate origins"]);
        expect(7, &["Transform", "Camera"], &["Viewport"]);
        expect(8, &["Transform", "Drawable"], &["Texture Offset"]);
        expect(9, &["Transform", "SimplePhysics"], &[
            "Parameter", "Type", "Mapping mode", "Local Transform Lock", "Gravity scale", "Length",
            "Resonant frequency", "Damping (angle)", "Damping (length)", "Output scale X", "Output scale Y",
        ]);
        expect(10, &["Transform", "PathDeformer"], &["Dynamic mode(slow)", "Auto-Physics", "Curve type"]);
        expect(11, &["Transform", "GridDeformer"], &["Dynamic Deformation (slower)"]);
        // An unmeshed part has nothing but its texture offset besides the transform.
        expect(13, &["Transform", "Drawable", "Part"], &["Texture Offset"]);
        // A MeshGroup or DynamicComposite that does store a mesh gets the Drawable section too.
        let mut with_mesh = model();
        with_mesh["nodes"]["children"][4]["mesh"] = mesh();
        with_mesh["nodes"]["children"][3]["mesh"] = mesh();
        let sections = sections_for(&json!({"documentFormat": "inx", "model": with_mesh}), Some(6));
        assert_eq!(titles(&sections), ["Transform", "Drawable", "MeshGroup"]);
        let sections = sections_for(&json!({"documentFormat": "inx", "model": with_mesh}), Some(5));
        assert_eq!(titles(&sections), ["Transform", "Drawable", "Part"]);
    }

    #[test]
    fn composite_blending_stops_at_destination_in_and_part_blending_does_not() {
        let session = session();
        let choice = |id, wanted: &str| {
            sections(&session, Some(id))
                .iter()
                .flat_map(|section| section.items.clone())
                .find_map(|item| match item {
                    Item::Field(Field { label, control: Control::Choice { value, choices }, .. })
                        if label == wanted =>
                    {
                        Some((value, choices))
                    }
                    _ => None,
                })
                .expect("a blending choice")
        };
        let choices = |id| choice(id, "Blending").1;
        assert_eq!(choices(2).len(), 23);
        assert_eq!(choices(2).last().map(String::as_str), Some("SliceFromLower"));
        assert_eq!(choices(4).len(), 21);
        assert_eq!(choices(4).last().map(String::as_str), Some("DestinationIn"));
        for id in [2, 4, 5] {
            assert_eq!(
                choice(id, "Alpha blending"),
                ("Over".to_owned(), ["Over", "Atop", "Out", "Conjoint", "Disjoint"].map(str::to_owned).to_vec())
            );
        }
    }

    #[test]
    fn the_puppet_inspector_shows_metadata_for_no_selection_and_for_the_root() {
        let session = session();
        for selected in [None, Some(1), Some(9999)] {
            let sections = sections(&session, selected);
            assert_eq!(titles(&sections), ["General Info", "Licensing", "Physics Globals", "Rendering Settings"]);
            assert_eq!(labels(&sections), [
                "Name", "Artist(s)", "Rigger(s)", "Contact", "License URL", "Copyright", "Origin",
                "Pixels per meter", "Gravity (m/s²)", "Use Point Filtering",
            ]);
        }
        let sections = sections(&session, None);
        assert_eq!(sections[0].items[0], Item::Note("soul2d_live Ver. 1.0-alpha".into()));
        assert_eq!(sections[0].items[1], Item::Note("Part Count: 3".into()));
        let Item::Field(artist) = &sections[0].items[3] else { panic!("artist field") };
        assert_eq!(artist.control, Control::Text("Aoi".into()));
        let Item::Field(ppm) = &sections[2].items[0] else { panic!("pixels per meter") };
        assert_eq!(ppm.control, Control::Numbers(Numbers {
            values: vec![1000.0], names: &[], min: 1.0, max: F32_MAX, speed: 1.0, unit: Unit::Plain,
        }));
    }

    #[test]
    fn depth_nodes_show_their_rig_buttons_and_bone_fields() {
        let session = session();
        let snapshot = session.snapshot();
        let ids = node_ids(&snapshot);
        let kind = |wanted: &str| {
            *ids.iter().find(|id| node_type(find(&snapshot["model"]["nodes"], **id).unwrap()) == wanted).unwrap()
        };
        let root = sections(&session, Some(kind("DepthRigRoot")));
        assert_eq!(labels(&root)[4..], ["Fit Z to Depth", "Add Standard Skeleton", "Add Standard Parameters"]);
        let bone = sections(&session, Some(kind("DepthBone")));
        assert_eq!(titles(&bone), ["Transform", "Depth Bone"]);
        assert_eq!(labels(&bone)[3..], [
            "Sorting (z-sort)", "Fit Z to Depth", "Rest Head Position", "Rest Tail Position", "Rest Roll Angle",
            "Lock Rotation", "Lock Translation", "Allow Parent to Targets",
        ]);
        // The depth types have no lock-to-root or pin-to-mesh property.
        assert!(!labels(&bone).contains(&"Lock to Root Node".to_owned()));
    }

    #[test]
    fn requests_have_the_exact_wire_shape() {
        let request = |target: Target, value: Value| edit_request(&target, value);
        assert_eq!(
            request(Target::Property { id: 2, name: "opacity" }, json!(0.5)),
            json!({"op": "node.property.set", "id": 2, "property": "opacity", "value": 0.5})
        );
        assert_eq!(
            request(Target::Transform { id: 2, key: "rotation" }, json!([0.0, 0.0, 1.5])),
            json!({"op": "node.transform", "id": 2, "rotation": [0.0, 0.0, 1.5]})
        );
        assert_eq!(
            request(Target::Physics { id: 9, key: "output_scale_x" }, json!(2.0)),
            json!({"op": "physics.set", "node": 9, "output_scale_x": 2.0})
        );
        assert_eq!(
            request(Target::Deformer { id: 6, property: "MeshGroupDynamic" }, json!(true)),
            json!({"op": "deformer.set", "id": 6, "property": "MeshGroupDynamic", "value": true})
        );
        assert_eq!(
            request(Target::Puppet { property: "artist" }, json!("Aoi")),
            json!({"op": "puppet.meta.set", "property": "artist", "value": "Aoi"})
        );
        assert_eq!(
            request(Target::Constraint { bone: 20, key: "lockRotation" }, json!(true)),
            json!({"op": "rig.set_constraint", "bone": 20, "constraint": {"lockRotation": true}})
        );
        assert_eq!(
            request(
                Target::BoneRest { bone: 20, rest: [json!([0.0, 1.0, 2.0]), json!([3.0, 4.0, 5.0]), json!(0.5)], slot: 1 },
                json!([9.0, 9.0, 9.0])
            ),
            json!({"op": "rig.set_rest", "bone": 20, "restHead": [0.0, 1.0, 2.0], "restTail": [9.0, 9.0, 9.0],
                "restRoll": 0.5})
        );
        assert_eq!(
            request(Target::Weld { part: 2, target: 3, indices: json!([0, 1, 2]) }, json!(0.25)),
            json!({"op": "part.set_weld", "part": 2, "target": 3, "indices": [0, 1, 2], "weight": 0.25})
        );
        assert_eq!(
            request(Target::MaskAdd { id: 2 }, json!(8)),
            json!({"op": "node.mask.edit", "id": 2, "action": "add", "source": 8})
        );
        assert_eq!(
            request(Target::WeldAdd { part: 2 }, json!(3)),
            json!({"op": "part.set_weld", "part": 2, "target": 3, "weight": 0.5})
        );
    }

    #[test]
    fn units_convert_between_the_stored_and_the_shown_value() {
        assert_eq!(display(Unit::Percent, 0.755), "76%");
        assert_eq!(parse_display(Unit::Percent, "50 %"), Some(0.5));
        assert_eq!(display(Unit::Degrees, std::f64::consts::FRAC_PI_2), "90.00°");
        assert_eq!(parse_display(Unit::Degrees, "180°"), Some(std::f64::consts::PI));
        assert_eq!(parse_display(Unit::Plain, "abc"), None);
    }

    /// A different value the control would accept.
    fn changed(control: &Control) -> Option<Value> {
        match control {
            Control::Flag(value) => Some(json!(!value)),
            Control::Numbers(spec) => {
                let step = if spec.max - spec.min <= 5.0 { (spec.max - spec.min) * 0.25 } else { 1.0 };
                let moved: Vec<f64> = spec
                    .values
                    .iter()
                    .map(|value| if value + step <= spec.max { value + step } else { value - step })
                    .collect();
                Some(if moved.len() == 1 { json!(moved[0]) } else { json!(moved) })
            }
            Control::Choice { value, choices } => choices.iter().find(|choice| *choice != value).map(|c| json!(c)),
            Control::Pick { value, options, .. } => {
                options.iter().find(|(id, _)| Some(*id) != *value).map(|(id, _)| json!(id))
            }
            Control::Text(text) => Some(json!(format!("{text}x"))),
        }
    }

    #[test]
    fn every_offered_control_sends_a_request_the_session_accepts() {
        let mut session = session();
        let selections: Vec<Option<u64>> =
            std::iter::once(None).chain(node_ids(&session.snapshot()).into_iter().map(Some)).collect();
        let mut sent = 0;
        for selected in selections {
            for section in sections(&session, selected) {
                for item in &section.items {
                    let requests: Vec<Value> = match item {
                        Item::Field(field) => match changed(&field.control) {
                            Some(value) => vec![edit_request(&field.target, value)],
                            None => continue,
                        },
                        Item::Action { request, .. } => vec![request.clone()],
                        Item::Row { actions, .. } => actions.iter().map(|(_, request)| request.clone()).collect(),
                        Item::Note(_) => continue,
                    };
                    for request in requests {
                        let response = session.handle_request(&request);
                        // These rig buttons need depth samples or body-part bounds, and a weld needs nearby vertices,
                        // that the fixture does not have; the refusal must name that, not a malformed request.
                        let message = response["message"].as_str().unwrap_or("");
                        let needs_content = match request["op"].as_str() {
                            Some("rig.fit_root_z" | "rig.fit_bone_z") => message.contains("mapped depth sample"),
                            Some("rig.add_skeleton") => message.contains("parent bounds"),
                            Some("rig.add_standard_parameters") => message.contains("Missing depth bone"),
                            Some("part.set_weld") => message.contains("within 4 units"),
                            _ => false,
                        };
                        assert!(
                            response["succeeded"] == true || needs_content,
                            "{selected:?} {}: {request}: {response}",
                            section.title
                        );
                        sent += 1;
                    }
                }
            }
        }
        assert!(sent > 120, "only {sent} requests were exercised");
    }

    #[test]
    fn a_field_edit_changes_only_its_stored_value() {
        let mut session = session();
        let before = session.snapshot();
        let sections = sections(&session, Some(2));
        let Some(Item::Field(opacity)) =
            sections.iter().flat_map(|s| &s.items).find(|i| matches!(i, Item::Field(f) if f.label == "Opacity"))
        else {
            panic!("no opacity field");
        };
        run(&mut session, edit_request(&opacity.target, json!(0.25)));
        let mut want = before["model"].clone();
        want["nodes"]["children"][0]["opacity"] = json!(0.25);
        assert_eq!(session.snapshot()["model"], want);
        run(&mut session, json!({"op": "undo"}));
        assert_eq!(session.snapshot()["model"], before["model"]);
    }

    // ------------------------------------------------------------ headless egui frames

    fn frame(
        ctx: &egui::Context,
        snapshot: &Value,
        selected: Option<u64>,
        events: Vec<egui::Event>,
    ) -> (Vec<Value>, egui::FullOutput) {
        let mut requests = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 2400.0))),
            events,
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                requests = inspector_section(ui, snapshot, selected);
            });
        });
        (requests, output)
    }

    fn accesskit_nodes(output: &egui::FullOutput) -> Vec<(egui::accesskit::NodeId, egui::accesskit::Node)> {
        output.platform_output.accesskit_update.clone().expect("accesskit is enabled").nodes
    }

    fn named(node: &egui::accesskit::Node) -> bool {
        node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty()
    }

    #[test]
    fn every_control_of_every_node_type_has_an_accessibility_name() {
        let session = session();
        let snapshot = session.snapshot();
        let mut texts = Vec::new();
        let selections: Vec<Option<u64>> = std::iter::once(None).chain(node_ids(&snapshot).into_iter().map(Some)).collect();
        for selected in selections {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (requests, output) = frame(&ctx, &snapshot, selected, Vec::new());
            assert!(requests.is_empty(), "drawing alone sends nothing");
            use egui::accesskit::Role;
            let mut controls = 0;
            for (_, node) in accesskit_nodes(&output) {
                if matches!(
                    node.role(),
                    Role::Button | Role::CheckBox | Role::ComboBox | Role::SpinButton | Role::TextInput
                ) {
                    controls += 1;
                    assert!(named(&node), "{selected:?}: unlabelled {:?} control {:?}", node.role(), node.value());
                }
                texts.extend([node.label(), node.value()].into_iter().flatten().map(str::to_owned));
            }
            assert!(controls > 0, "{selected:?} drew no controls");
        }
        for expected in [
            "Translation X", "Rotation Z", "Scale Y", "Lock to Root Node", "Sorting (z-sort)", "Tint (Multiply) R",
            "Tint (Screen) B", "Emission Strength", "Blending", "Alpha blending", "Opacity", "Threshold",
            "Weld weight Hair", "Remove mask Cutout", "Add mask source", "Propagate MeshGroup", "Resize automatically",
            "Viewport Width", "Texture Offset Y", "Parameter", "Output scale X", "Auto-Physics", "Columns: 3",
            "Fit Z to Depth",
            "Rest Head Position Z", "Allow Parent to Targets", "Name", "Artist(s)", "Pixels per meter",
            "Use Point Filtering", "Warning: Camera size must be divisible by 2", "Cannot inspect an unmeshed part",
        ] {
            let found = texts.iter().any(|text| text.contains(expected));
            assert!(found, "missing {expected:?} in {texts:?}");
        }
    }

    fn center_of(output: &egui::FullOutput, role: egui::accesskit::Role, label: &str) -> egui::Pos2 {
        let (_, node) = accesskit_nodes(output)
            .into_iter()
            .find(|(_, node)| node.role() == role && node.label() == Some(label))
            .unwrap_or_else(|| panic!("no {role:?} named {label:?}"));
        let bounds = node.bounds().expect("bounds");
        egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32)
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_checkbox_click_sends_one_request_and_a_drag_sends_one_request_when_released() {
        let session = session();
        let snapshot = session.snapshot();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        // Two frames: the first lays everything out, the second has stable positions.
        frame(&ctx, &snapshot, Some(12), Vec::new());
        let (_, laid_out) = frame(&ctx, &snapshot, Some(12), Vec::new());

        let at = center_of(&laid_out, egui::accesskit::Role::CheckBox, "Lock to Root Node");
        let (requests, _) = frame(&ctx, &snapshot, Some(12), vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
        assert_eq!(requests, [json!({"op": "node.property.set", "id": 12, "property": "lockToRoot", "value": true})]);

        // Drag the first translation DragValue to the right.
        let start = center_of(&laid_out, egui::accesskit::Role::SpinButton, "Translation X");
        let (during, _) = frame(&ctx, &snapshot, Some(12), vec![egui::Event::PointerMoved(start), press(start, true)]);
        assert!(during.is_empty());
        let moved = start + egui::vec2(40.0, 0.0);
        let (during, _) = frame(&ctx, &snapshot, Some(12), vec![egui::Event::PointerMoved(moved)]);
        assert!(during.is_empty(), "no request while the drag is in progress");
        let (done, _) = frame(&ctx, &snapshot, Some(12), vec![press(moved, false)]);
        assert_eq!(done.len(), 1, "{done:?}");
        assert_eq!(done[0]["op"], "node.transform");
        assert_eq!(done[0]["id"], 12);
        let translation = done[0]["translation"].as_array().expect("translation vector");
        assert_eq!(translation.len(), 3);
        assert!(translation[0].as_f64().unwrap() > 0.0, "{done:?}");
        assert_eq!(translation[1], json!(0.0));
        let mut session = session;
        run(&mut session, done[0].clone());
    }
}
