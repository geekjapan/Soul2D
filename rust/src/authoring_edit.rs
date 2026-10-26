use crate::authoring_graph::Graph;
use crate::authoring_scene;
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, NodeId, ParsingOptions};
use serde_json::{Number, Value};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

const MAX_SCALARS: usize = 5_000_000;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FormKey {
    source_guid: String,
    form_guid: String,
    coordinate_type: Option<String>,
}

struct FormChange {
    key: FormKey,
    positions: Option<(Vec<f64>, Vec<f64>)>,
    opacity: Option<(f64, f64)>,
}

struct TargetChange {
    source_guid: String,
    from: String,
    to: String,
}

struct BrushChange {
    source_guid: String,
    from: String,
    to: String,
}

/// A new text for one source (`localName`, or an ArtMesh's `userData`), one parameter or one ArtPath brush (`name`).
struct NameChange {
    /// The model list that holds the object: `sources`, `parameters` or `brushes`.
    list: &'static str,
    user_data: bool,
    id: bool,
    guid: String,
    name: String,
}

const MAX_NAME_BYTES: usize = 1024;
/// The model lists whose entries carry a name and an ID that `rename` and `set_id` edit.
const NAMED_LISTS: [&str; 3] = ["sources", "parameters", "brushes"];
// ponytail: no vendor limit is known; this one bounds a request, raise it if a real model needs more.
const MAX_USER_DATA_BYTES: usize = 64 * 1024;

/// The setting, parent, clipping and deletion changes a save writes besides form positions, opacity and names.
struct Structure<'a> {
    fields: &'a [crate::authoring_fields::FieldChange],
    moves: &'a [crate::authoring_reparent::Move],
    targets: &'a [TargetChange],
    brushes: &'a [BrushChange],
    clips: &'a [crate::authoring_clips::ClipChange],
    deletions: &'a [crate::authoring_delete::Deletion],
}

pub(crate) struct XmlPatch {
    pub(crate) range: Range<usize>,
    pub(crate) value: String,
}

struct XmlIndex<'doc> {
    inbound_refs: HashSet<&'doc str>,
    position_users: HashMap<NodeId, usize>,
}

pub(crate) fn set_vertex(model: &Value, request: &Value) -> Result<Value, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let object = request
        .as_object()
        .ok_or("authoring vertex edit request must be an object")?;
    let source_guid = required_nonempty_string(object.get("sourceGuid"), "sourceGuid")?;
    let form_guid = required_nonempty_string(object.get("formGuid"), "formGuid")?;
    let coordinate_type = Some(required_nonempty_string(
        object.get("coordinateType"),
        "coordinateType",
    )?);
    let vertex_index = object
        .get("vertexIndex")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or("vertexIndex must be a non-negative integer")?;
    let position = object
        .get("position")
        .and_then(Value::as_array)
        .filter(|position| position.len() == 2)
        .ok_or("position must contain exactly two numbers")?;
    let position = [
        finite_json_number(&position[0])?,
        finite_json_number(&position[1])?,
    ];
    if position.iter().any(|value| !(*value as f32).is_finite()) {
        return Err("authoring vertex position exceeds native float range".into());
    }

    let sources = model
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("authoring source model lacks sources")?;
    let mut owners = sources
        .iter()
        .filter(|source| source.get("guid").and_then(Value::as_str) == Some(source_guid));
    let owner = owners
        .next()
        .ok_or("authoring edit source GUID was not found")?;
    if owners.next().is_some() {
        return Err("authoring edit source GUID is ambiguous".into());
    }
    let owner_data = owner
        .get("data")
        .ok_or("authoring edit source lacks data")?;
    let source_type = required_string(owner_data.get("type"), "source data type")?;
    if !matches!(source_type, "ArtMesh" | "Warp") {
        return Err("authoring vertex edit supports only ArtMesh and Warp sources".into());
    }
    ensure_form_editable(owner, form_guid)?;
    let expected_positions = source_position_count(owner_data, source_type)?;
    let forms = owner
        .get("forms")
        .and_then(Value::as_array)
        .ok_or("authoring edit source lacks primary forms")?;
    let mut selected = None;
    for (form_index, form) in forms.iter().enumerate() {
        if form.get("guid").and_then(Value::as_str) != Some(form_guid)
            || optional_form_coordinate(form)? != coordinate_type
        {
            continue;
        }
        if selected.replace((form_index, form)).is_some() {
            return Err("authoring edit form and coordinate type are ambiguous".into());
        }
    }
    let (form_index, form) = selected.ok_or("authoring primary form was not found")?;
    let form_data = form
        .get("data")
        .ok_or("authoring primary form lacks data")?;
    if required_string(form_data.get("type"), "form data type")? != source_type {
        return Err("authoring primary form type does not match its source".into());
    }
    let positions = finite_positions(form_data.get("positions"), "form positions")?;
    if positions.len() != expected_positions {
        return Err("authoring primary form vertex count differs from its source".into());
    }
    if vertex_index >= positions.len() / 2 {
        return Err("vertexIndex is outside the primary form".into());
    }

    let mut result = model.clone();
    let target = &mut result["sources"][owner_index(sources, source_guid)?]["forms"][form_index]
        ["data"]["positions"];
    target[vertex_index * 2] = json_number(position[0])?;
    target[vertex_index * 2 + 1] = json_number(position[1])?;
    Ok(result)
}

pub(crate) fn set_opacity(model: &Value, request: &Value) -> Result<Value, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let object = request
        .as_object()
        .ok_or("authoring opacity edit request must be an object")?;
    let source_guid = required_nonempty_string(object.get("sourceGuid"), "sourceGuid")?;
    let form_guid = required_nonempty_string(object.get("formGuid"), "formGuid")?;
    let coordinate_type = Some(required_nonempty_string(
        object.get("coordinateType"),
        "coordinateType",
    )?);
    let opacity = form_opacity(object.get("opacity").ok_or("opacity is required")?)?;

    let sources = model
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("authoring source model lacks sources")?;
    let mut owners = sources
        .iter()
        .filter(|source| source.get("guid").and_then(Value::as_str) == Some(source_guid));
    let owner = owners
        .next()
        .ok_or("authoring edit source GUID was not found")?;
    if owners.next().is_some() {
        return Err("authoring edit source GUID is ambiguous".into());
    }
    let owner_data = owner
        .get("data")
        .ok_or("authoring edit source lacks data")?;
    let source_type = required_string(owner_data.get("type"), "source data type")?;
    if !matches!(source_type, "ArtMesh" | "ArtPath") {
        return Err("authoring opacity edit supports only ArtMesh or ArtPath sources".into());
    }
    ensure_form_editable(owner, form_guid)?;
    ensure_shape_opacity(owner, form_guid, opacity)?;
    let forms = owner
        .get("forms")
        .and_then(Value::as_array)
        .ok_or("authoring edit source lacks primary forms")?;
    let mut selected = None;
    for (form_index, form) in forms.iter().enumerate() {
        if form.get("guid").and_then(Value::as_str) != Some(form_guid)
            || optional_form_coordinate(form)? != coordinate_type
        {
            continue;
        }
        if selected.replace((form_index, form)).is_some() {
            return Err("authoring edit form and coordinate type are ambiguous".into());
        }
    }
    let (form_index, form) = selected.ok_or("authoring primary form was not found")?;
    let form_data = form
        .get("data")
        .ok_or("authoring primary form lacks data")?;
    if required_string(form_data.get("type"), "form data type")? != source_type {
        return Err("authoring primary form type does not match its source".into());
    }
    finite_json_number(
        form.get("opacity")
            .ok_or("authoring primary form lacks opacity")?,
    )?;

    let mut result = model.clone();
    result["sources"][owner_index(sources, source_guid)?]["forms"][form_index]["opacity"] =
        json_number(opacity)?;
    Ok(result)
}

/// Paste Form and Blend Form on one ArtMesh or Warp source (issue #178): the target form becomes target + ratio ×
/// (copied − target), vertex by vertex, and so does an ArtMesh form's opacity. `invert` ("vertical" or
/// "horizontal") first mirrors the copied shape about that line through the centre of its bounds (Paste Form
/// Special, #179). `properties` limits the paste to "vertices" or "opacity" (default both). Both forms must belong
/// to the source and use the same coordinate type.
pub(crate) fn paste_form(model: &Value, request: &Value) -> Result<Value, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let object = request.as_object().ok_or("authoring paste request must be an object")?;
    let source_guid = required_nonempty_string(object.get("sourceGuid"), "sourceGuid")?;
    let target_guid = required_nonempty_string(object.get("formGuid"), "formGuid")?;
    let coordinate_type = required_nonempty_string(object.get("coordinateType"), "coordinateType")?;
    let copied_guid = required_nonempty_string(object.get("fromFormGuid"), "fromFormGuid")?;
    let ratio = object.get("ratio").map(finite_json_number).transpose()?.unwrap_or(1.0);
    if !(0.0..=1.0).contains(&ratio) {
        return Err("ratio must be between 0 and 1".into());
    }
    let axis = match object.get("invert").map(|value| value.as_str()) {
        None => None,
        Some(Some("vertical")) => Some(0),
        Some(Some("horizontal")) => Some(1),
        Some(_) => return Err("invert must be vertical or horizontal".into()),
    };
    let properties = match object.get("properties") {
        None => vec!["vertices", "opacity"],
        Some(list) => list.as_array().into_iter().flatten().map(|name| match name.as_str() {
            Some(name @ ("vertices" | "opacity")) => Ok(name),
            _ => Err("properties may hold only vertices and opacity".to_string()),
        }).collect::<Result<_, _>>()?,
    };
    let sources = model.get("sources").and_then(Value::as_array).ok_or("authoring source model lacks sources")?;
    let source_index = owner_index(sources, source_guid)?;
    let owner = &sources[source_index];
    let source_type = required_string(owner["data"].get("type"), "source data type")?;
    if !matches!(source_type, "ArtMesh" | "Warp") {
        return Err("authoring paste supports only ArtMesh and Warp sources".into());
    }
    ensure_form_editable(owner, target_guid)?;
    let expected = source_position_count(&owner["data"], source_type)?;
    let forms = owner.get("forms").and_then(Value::as_array).ok_or("authoring edit source lacks primary forms")?;
    let select = |guid: &str| -> Result<(usize, Vec<f64>), String> {
        let mut found = forms.iter().enumerate().filter(|(_, form)| {
            form.get("guid").and_then(Value::as_str) == Some(guid)
                && optional_form_coordinate(form).ok().flatten() == Some(coordinate_type)
        });
        let (index, form) = found.next().ok_or_else(|| format!("form {guid} was not found in {coordinate_type}"))?;
        if found.next().is_some() {
            return Err("authoring edit form and coordinate type are ambiguous".into());
        }
        if required_string(form["data"].get("type"), "form data type")? != source_type {
            return Err("authoring primary form type does not match its source".into());
        }
        let positions = finite_positions(form["data"].get("positions"), "form positions")?;
        if positions.len() != expected {
            return Err("authoring primary form vertex count differs from its source".into());
        }
        Ok((index, positions))
    };
    let (target_index, target) = select(target_guid)?;
    let (copied_index, mut copied) = select(copied_guid)?;
    if let Some(axis) = axis {
        let (low, high) = copied.iter().skip(axis).step_by(2)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| (low.min(*value), high.max(*value)));
        copied.iter_mut().skip(axis).step_by(2).for_each(|value| *value = low + high - *value);
    }
    let mut result = model.clone();
    let form = &mut result["sources"][source_index]["forms"][target_index];
    if properties.contains(&"vertices") {
        let blended = target.iter().zip(&copied).map(|(now, from)| now + ratio * (from - now));
        form["data"]["positions"] = Value::Array(blended.map(json_number).collect::<Result<_, _>>()?);
    }
    if source_type == "ArtMesh" && properties.contains(&"opacity") {
        let now = finite_json_number(&forms[target_index]["opacity"])?;
        let opacity = now + ratio * (finite_json_number(&forms[copied_index]["opacity"])? - now);
        ensure_shape_opacity(owner, target_guid, opacity)?;
        form["opacity"] = json_number(opacity)?;
    }
    Ok(result)
}

/// Multiple key editing of opacity (issue #183): each listed ArtMesh or ArtPath form gets `opacity` (method "overwrite") or
/// its own opacity plus `opacity` clamped to 0..1 (method "add").
pub(crate) fn set_opacities(model: &Value, request: &Value) -> Result<Value, String> {
    let forms = request.get("forms").and_then(Value::as_array).filter(|forms| !forms.is_empty())
        .ok_or("forms must be a non-empty array")?;
    let value = finite_json_number(request.get("opacity").ok_or("opacity is required")?)?;
    let add = match request.get("method").and_then(Value::as_str).unwrap_or("overwrite") {
        "overwrite" => false,
        "add" => true,
        _ => return Err("method must be overwrite or add".into()),
    };
    let mut result = model.clone();
    for form in forms {
        let (source, guid) = (form.get("sourceGuid"), form.get("formGuid"));
        let current = result["sources"].as_array().into_iter().flatten()
            .filter(|owner| owner.get("guid") == source)
            .flat_map(|owner| owner["forms"].as_array().into_iter().flatten())
            .find(|candidate| candidate.get("guid") == guid && candidate.get("coordinateType") == form.get("coordinateType"))
            .and_then(|candidate| candidate["opacity"].as_f64())
            .ok_or("a listed form was not found")?;
        let opacity = if add { (current + value).clamp(0.0, 1.0) } else { value };
        let mut edit = form.clone();
        edit["opacity"] = json_number(opacity)?;
        result = set_opacity(&result, &edit)?;
    }
    Ok(result)
}

/// Sets the display name of the one source, parameter or ArtPath brush with this GUID. The ID string is left alone:
/// motions, expressions and physics address a parameter by ID, never by name.
pub(crate) fn rename(model: &Value, request: &Value) -> Result<Value, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let guid = required_nonempty_string(request.get("guid"), "guid")?;
    let name = required_string(request.get("name"), "name")?;
    if name.len() > MAX_NAME_BYTES {
        return Err(format!("name must be at most {MAX_NAME_BYTES} bytes"));
    }
    // U+FFFE and U+FFFF are no control characters, but XML 1.0 has no way to write them.
    if name.chars().any(|c| c.is_control() || matches!(c, '\u{FFFE}' | '\u{FFFF}')) {
        return Err("name must not contain control characters".into());
    }
    let mut found = Vec::new();
    for list in NAMED_LISTS {
        for (index, entry) in model.get(list).and_then(Value::as_array).into_iter().flatten().enumerate() {
            if entry.get("guid").and_then(Value::as_str) == Some(guid) {
                found.push((list, index));
            }
        }
    }
    let [(list, index)] = found[..] else {
        return Err(if found.is_empty() {
            "authoring rename GUID was not found among sources, parameters and brushes".into()
        } else {
            "authoring rename GUID is ambiguous".into()
        });
    };
    required_string(model[list][index].get("name"), "current name")?;
    let mut result = model.clone();
    result[list][index]["name"] = Value::String(name.to_owned());
    Ok(result)
}

/// Sets the ID string of the one source, parameter or ArtPath brush with this GUID. A parameter's keyform axes and
/// morph weights follow; motions, expressions and physics files outside the CMO3 that name the old ID do not.
pub(crate) fn set_id(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = required_nonempty_string(request.get("guid"), "guid")?;
    let id = required_string(request.get("id"), "id")?;
    // A .moc3 stores each ID in a 64-byte slot with a terminating zero.
    if id.is_empty() || id.len() > 63 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return Err("id must be 1 to 63 ASCII letters, digits or underscores".into());
    }
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let holds = |list: &str| model[list].as_array().into_iter().flatten().filter(|entry| entry["guid"] == guid).count();
    let list = match NAMED_LISTS.map(holds) {
        [1, 0, 0] => "sources",
        [0, 1, 0] => "parameters",
        [0, 0, 1] => "brushes",
        [0, 0, 0] => return Err("authoring GUID was not found among sources, parameters and brushes".into()),
        _ => return Err("authoring GUID is ambiguous".into()),
    };
    let mut result = model.clone();
    let entries = result[list].as_array_mut().ok_or("authoring model lacks its lists")?;
    if entries.iter().any(|entry| entry["id"] == id && entry["guid"] != guid) {
        return Err(format!("id {id} is already used"));
    }
    let entry = entries.iter_mut().find(|entry| entry["guid"] == guid).ok_or("authoring GUID was not found")?;
    let old = entry["id"].as_str().ok_or("the object has no ID")?.to_owned();
    entry["id"] = id.into();
    if list == "parameters" {
        rename_parameter_references(&mut result, &old, id);
    }
    Ok(result)
}

/// Sets the user data text of the one ArtMesh with this GUID; an empty text clears it.
pub(crate) fn set_user_data(model: &Value, request: &Value) -> Result<Value, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let guid = required_nonempty_string(request.get("guid"), "guid")?;
    let value = required_string(request.get("value"), "value")?;
    if value.len() > MAX_USER_DATA_BYTES {
        return Err(format!("value must be at most {MAX_USER_DATA_BYTES} bytes"));
    }
    // XML turns a carriage return into a line feed, so only tabs and line feeds survive a save.
    if value.chars().any(|c| (c.is_control() && !matches!(c, '\t' | '\n')) || matches!(c, '\u{FFFE}' | '\u{FFFF}')) {
        return Err("value must not contain control characters other than tab and line feed".into());
    }
    let sources = model.get("sources").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let index = owner_index(sources, guid)?;
    if sources[index]["data"]["type"] != "ArtMesh" {
        return Err("user data can be set on an ArtMesh only".into());
    }
    let mut result = model.clone();
    match value.is_empty() {
        true => result["sources"][index].as_object_mut().map(|source| source.remove("userData")),
        false => result["sources"][index].as_object_mut().map(|source| source.insert("userData".into(), value.into())),
    };
    Ok(result)
}

/// The names `model` changed against `initial`, and `model` with those names put back, for `form_changes`.
fn name_changes(initial: &Value, model: &Value) -> (Vec<NameChange>, Value) {
    let mut rest = model.clone();
    let mut changes = Vec::new();
    let mut renamed = Vec::new();
    for list in NAMED_LISTS {
        let old = initial.get(list).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let Some(new) = rest.get_mut(list).and_then(Value::as_array_mut) else { continue };
        for (old, new) in old.iter().zip(new) {
            let Some(guid) = old["guid"].as_str().filter(|guid| new["guid"].as_str() == Some(guid)) else { continue };
            if let Some(name) = new["name"].as_str().filter(|_| old["name"].is_string() && old["name"] != new["name"]) {
                changes.push(NameChange { list, user_data: false, id: false, guid: guid.to_owned(), name: name.to_owned() });
                new["name"] = old["name"].clone();
            }
            if let (Some(old_id), Some(id)) = (old["id"].as_str(), new["id"].as_str().filter(|id| old["id"] != *id)) {
                changes.push(NameChange { list, user_data: false, id: true, guid: guid.to_owned(), name: id.to_owned() });
                if list == "parameters" {
                    renamed.push((id.to_owned(), old_id.to_owned()));
                }
                new["id"] = old["id"].clone();
            }
            if list == "sources" && old.get("userData") != new.get("userData") {
                let text = new.get("userData").and_then(Value::as_str).unwrap_or_default().to_owned();
                changes.push(NameChange { list, user_data: true, id: false, guid: guid.to_owned(), name: text });
                let object = new.as_object_mut().expect("a source is an object");
                match old.get("userData") {
                    Some(old) => object.insert("userData".into(), old.clone()),
                    None => object.remove("userData"),
                };
            }
        }
    }
    for (from, to) in renamed {
        rename_parameter_references(&mut rest, &from, &to);
    }
    (changes, rest)
}

/// Every `parameterId` the model derives from a parameter's ID, so an ID change reaches its keyform axes and morph
/// weights. Parameter IDs are unique, so the old ID names one parameter only.
fn rename_parameter_references(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if key == "parameterId" && field.as_str() == Some(from) {
                    *field = Value::String(to.to_owned());
                } else {
                    rename_parameter_references(field, from, to);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| rename_parameter_references(item, from, to)),
        _ => {}
    }
}

fn name_patch(text: &str, graph: &Graph<'_, '_>, model: Node<'_, '_>, change: &NameChange) -> Result<XmlPatch, String> {
    let (sets, field_name): (&[&str], &str) = match change.list {
        "parameters" => (&["parameterSourceSet"], if change.id { "id" } else { "name" }),
        "brushes" => (&[], if change.id { "id" } else { "name" }),
        _ if change.user_data => (&["drawableSourceSet"], "userData"),
        _ => (&["drawableSourceSet", "deformerSourceSet", "partSourceSet", "affecterSourceSet"],
            if change.id { "id" } else { "localName" }),
    };
    let mut candidates = Vec::new();
    for set in sets {
        candidates.extend(graph.source_list(model, set)?);
    }
    if change.list == "brushes" {
        candidates.extend(brush_nodes(graph, model)?);
    }
    let mut owners = Vec::new();
    for candidate in candidates {
        if graph.guid(candidate)? == Some(change.guid.as_str()) {
            owners.push(candidate);
        }
    }
    let [owner] = owners[..] else {
        return Err(format!("authoring {field_name} target could not be located in XML"));
    };
    if change.id {
        // A shared ID element changes for every holder; the re-read in `rewrite` refuses any the model does not expect.
        let field = graph.field(owner, "id")?.ok_or("authoring object has no ID field")?;
        return crate::authoring_reparent::attribute_patch(text, field, "idstr", change.name.clone());
    }
    // The name sits on the object or on one of its inline `super` parts (an ArtMesh keeps it in ACDrawableSource).
    let mut holder = owner;
    let mut field = None;
    for _ in 0..=64 {
        field = direct_field(holder, field_name);
        if field.is_some() {
            break;
        }
        let Some(parent) = direct_field(holder, "super") else { break };
        if parent.attribute("xs.ref").is_some() {
            return Err(format!("authoring {field_name} path uses xs.ref storage"));
        }
        holder = parent;
    }
    let field = field.ok_or_else(|| format!("authoring object has no inline {field_name} field"))?;
    // One attribute: no xs.ref to a shared string and no xs.id another object could point at.
    if !field.has_tag_name("s") || field.attributes().len() != 1 || field.children().any(|child| child.is_element()) {
        return Err(format!("authoring {field_name} is not an inline string field of its own"));
    }
    let escaped = change.name.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    Ok(XmlPatch { range: field.range(), value: format!("<s xs.n=\"{field_name}\">{escaped}</s>") })
}

/// The brushes of the model's ArtPath brush setting, the list `authoring_scene` reads them from.
pub(crate) fn brush_nodes<'d, 'i>(graph: &Graph<'d, 'i>, model: Node<'d, 'i>) -> Result<Vec<Node<'d, 'i>>, String> {
    let Some(setting) = graph.field(model, "artPathBrushesSetting")? else { return Ok(Vec::new()) };
    let Some(list) = graph.field(setting, "brushes")? else { return Ok(Vec::new()) };
    list.children().filter(|child| child.is_element()).map(|child| graph.resolve(child)).collect()
}

fn physics_field(model: &Value) -> Option<&str> {
    model.get("physicsSource").and_then(Value::as_str)
}

/// `rewrite` for a model that may hold sources the session copied: they are cloned into the file first, and the
/// rest of the model is patched onto that file. Keyform grids that changed shape and ArtPath control points that
/// were added or deleted are written before that.
pub(crate) fn rewrite_with_copies(
    container: &CaffDocument,
    initial: &Value,
    model: &Value,
    copies: &[crate::authoring_copy::Copy],
) -> Result<Vec<u8>, String> {
    let expanded = crate::authoring_copy::insert(container, initial, model, copies)?;
    let expanded_initial = expanded.as_ref().map(authoring_scene::read).transpose()?;
    let (container, initial) = match (&expanded, &expanded_initial) {
        (Some(expanded), Some(read)) => (expanded, read),
        _ => (container, initial),
    };
    let synced = crate::authoring_grid::sync(container, initial, model)?;
    let synced_initial = synced.as_ref().map(authoring_scene::read).transpose()?;
    let (container, initial) = match (&synced, &synced_initial) {
        (Some(synced), Some(read)) => (synced, read),
        _ => (container, initial),
    };
    match crate::authoring_fields::sync_points(container, initial, model)? {
        None => rewrite(container, initial, model),
        Some(synced) => rewrite(&synced, &authoring_scene::read(&synced)?, model),
    }
}

pub(crate) fn rewrite(
    container: &CaffDocument,
    initial: &Value,
    model: &Value,
) -> Result<Vec<u8>, String> {
    if &authoring_scene::read(container)? != initial {
        return Err("authoring initial model does not match the CAFF scene".into());
    }
    if model == initial {
        return Ok(container.encode());
    }
    // Deletions go onto a file that already holds the session's other edits, so a delete composes with a reparent
    // into or out of the same Part and the two never patch one list. The inner call cannot stage again: `restored`
    // keeps every entry of `initial`, so it has no deletions.
    let (deletions, restored) = crate::authoring_delete::changes(initial, model)?;
    let staged;
    let staged_initial;
    let (container, initial) = if !deletions.is_empty() && restored != *initial {
        staged = CaffDocument::parse(&rewrite(container, initial, &restored)?)?;
        staged_initial = authoring_scene::read(&staged)?;
        (&staged, &staged_initial)
    } else {
        (container, initial)
    };
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    // The physics source is verbatim XML in the model: a change splices the stored string in place. Creation and
    // removal of the set element are refused; `authoring_physics::edit` never produces them either.
    let spliced;
    if physics_field(model) != physics_field(initial) {
        let initial_xml =
            physics_field(initial).ok_or("authoring physics source cannot be added where none exists")?;
        let candidate_xml =
            physics_field(model).ok_or("authoring physics source cannot be removed")?;
        spliced = crate::authoring_physics::splice_source(text, initial_xml, candidate_xml)?;
    } else {
        spliced = text.to_owned();
    }
    let text = spliced.as_str();
    let mut rest = model.clone();
    match initial.get("physicsSource") {
        Some(value) => rest["physicsSource"] = value.clone(),
        None => {
            if let Some(object) = rest.as_object_mut() {
                object.remove("physicsSource");
            }
        }
    }
    let (deletions, rest) = crate::authoring_delete::changes(initial, &rest)?;
    let (names, named_model) = name_changes(initial, &rest);
    let (fields, fields_model) = crate::authoring_fields::changes(initial, &named_model)?;
    let (moves, moved_model) = crate::authoring_reparent::changes(initial, &fields_model)?;
    let (targets, targeted_model) = target_changes(initial, &moved_model)?;
    let (brushes, brushed_model) = brush_changes(initial, &targeted_model)?;
    let (clips, forms_model) = crate::authoring_clips::changes(initial, &brushed_model)?;
    let changes = form_changes(initial, &forms_model)?;
    let structure = Structure { fields: &fields, moves: &moves, targets: &targets, brushes: &brushes,
        clips: &clips, deletions: &deletions };
    let rewritten_xml = patch_primary_fields(text, &changes, &names, &structure)?;
    let rewritten = container.replace_main_xml(rewritten_xml.as_bytes())?;
    let reopened = CaffDocument::parse(&rewritten)?;
    if authoring_scene::read(&reopened)? != *model {
        return Err("rewritten CAFF scene does not match the requested authoring model".into());
    }
    Ok(rewritten)
}

fn target_changes(initial: &Value, model: &Value) -> Result<(Vec<TargetChange>, Value), String> {
    let mut normalized = model.clone();
    let mut changes = Vec::new();
    let (Some(old), Some(new)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok((changes, normalized));
    };
    let rotations = new.iter().filter(|s| s["data"]["type"] == "Rotation")
        .filter_map(|s| s["guid"].as_str()).collect::<HashSet<_>>();
    for (index, (old, new)) in old.iter().zip(new).enumerate() {
        if old["guid"] != new["guid"] || old["targetDeformerGuid"] == new["targetDeformerGuid"] {
            continue;
        }
        let from = required_nonempty_string(old.get("targetDeformerGuid"), "old targetDeformerGuid")?;
        let to = required_nonempty_string(new.get("targetDeformerGuid"), "targetDeformerGuid")?;
        if old["data"]["type"] != "ArtMesh" || new["data"]["type"] != "ArtMesh" || !rotations.contains(to) {
            return Err("authoring target edits require an ArtMesh and a Rotation destination".into());
        }
        changes.push(TargetChange { source_guid: required_nonempty_string(old.get("guid"), "source GUID")?.to_owned(),
            from: from.to_owned(), to: to.to_owned() });
        normalized["sources"][index]["targetDeformerGuid"] = old["targetDeformerGuid"].clone();
    }
    Ok((changes, normalized))
}

fn owner_index(sources: &[Value], source_guid: &str) -> Result<usize, String> {
    sources
        .iter()
        .position(|source| source.get("guid").and_then(Value::as_str) == Some(source_guid))
        .ok_or_else(|| "authoring edit source GUID was not found".into())
}

pub(crate) fn ensure_listed_brush(model: &Value, guid: &str) -> Result<(), String> {
    if guid.trim().is_empty() || model["brushes"].as_array().into_iter().flatten()
        .filter(|brush| brush["guid"].as_str() == Some(guid)).count() != 1
    {
        return Err("brushGuid must name exactly one existing brush in the model".into());
    }
    Ok(())
}

fn brush_changes(initial: &Value, model: &Value) -> Result<(Vec<BrushChange>, Value), String> {
    let mut normalized = model.clone();
    let mut changes = Vec::new();
    let (Some(old), Some(new)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok((changes, normalized));
    };
    for (index, (old, new)) in old.iter().zip(new).enumerate() {
        if old["guid"] != new["guid"] || old["data"]["brushGuid"] == new["data"]["brushGuid"] {
            continue;
        }
        if old["data"]["type"] != "ArtPath" || new["data"]["type"] != "ArtPath" {
            return Err("authoring brush selection requires an ArtPath source".into());
        }
        let from = required_nonempty_string(old["data"].get("brushGuid"), "old brushGuid")?;
        let to = required_nonempty_string(new["data"].get("brushGuid"), "brushGuid")?;
        ensure_listed_brush(initial, to)?;
        ensure_listed_brush(model, to)?;
        changes.push(BrushChange { source_guid: required_nonempty_string(old.get("guid"), "source GUID")?.to_owned(),
            from: from.to_owned(), to: to.to_owned() });
        normalized["sources"][index]["data"]["brushGuid"] = old["data"]["brushGuid"].clone();
    }
    Ok((changes, normalized))
}

fn required_nonempty_string<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a str, String> {
    let value = required_string(value, name)?;
    if value.trim().is_empty() {
        return Err(format!("{name} must be a non-empty string"));
    }
    Ok(value)
}

fn required_string<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a str, String> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string"))
}

fn optional_string<'a>(value: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(format!("{name} must be a string or null")),
    }
}

/// Whether the writer may change this form of `owner`.
///
/// A source with blend shapes stores every shape as a whole form and evaluates it as the difference to the base
/// form. Changing a shape's own form changes that one shape. Changing a base form would change every shape of
/// the source with it, and what Cubism Editor does to the shapes in that case was not observed, so a base form
/// stays refused. Generated extended interpolation forms are caches of the primary ones and are not rebuilt here.
pub(crate) fn ensure_form_editable(owner: &Value, form_guid: &str) -> Result<(), String> {
    if owner
        .get("extendedInterpolations")
        .and_then(Value::as_array)
        .is_some_and(|extensions| !extensions.is_empty())
    {
        return Err("authoring source has extended interpolation forms".into());
    }
    let Some(morph_targets) = owner.get("morphTargets") else {
        return Ok(());
    };
    if morph_targets.is_null() {
        return Ok(());
    }
    let targets = morph_targets
        .get("targets")
        .and_then(Value::as_array)
        .ok_or("authoring morph target metadata is invalid")?;
    let constraints = morph_targets
        .get("blendWeightConstraints")
        .and_then(Value::as_array)
        .ok_or("authoring morph constraint metadata is invalid")?;
    if targets.is_empty() && constraints.is_empty() {
        return Ok(());
    }
    let is_target = targets.iter().any(|target| target.get("formGuid").and_then(Value::as_str) == Some(form_guid));
    let on_grid = owner["grid"]["cells"]
        .as_array()
        .is_some_and(|cells| cells.iter().any(|cell| cell["formGuid"].as_str() == Some(form_guid)));
    if !is_target || on_grid {
        return Err("authoring source has primary morph metadata: only a morph target form of it can be edited".into());
    }
    Ok(())
}

/// The evaluator refuses a blend shape whose opacity leaves 0..=1, and it evaluates every shape of a document
/// for each render. A plain form keeps the authored value unclamped; a shape must stay in range.
pub(crate) fn ensure_shape_opacity(owner: &Value, form_guid: &str, opacity: f64) -> Result<(), String> {
    let is_shape = owner["morphTargets"]["targets"]
        .as_array()
        .is_some_and(|targets| targets.iter().any(|target| target["formGuid"].as_str() == Some(form_guid)));
    if is_shape && !(0.0..=1.0).contains(&opacity) {
        return Err("authoring morph target form opacity must be in 0..=1".into());
    }
    Ok(())
}

fn optional_form_coordinate(form: &Value) -> Result<Option<&str>, String> {
    optional_string(
        form.get("coordinateType")
            .ok_or("authoring primary form lacks coordinateType")?,
        "form coordinateType",
    )
}

fn finite_json_number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| "authoring scalar must be a finite number".into())
}

pub(crate) fn form_opacity(value: &Value) -> Result<f64, String> {
    let opacity = finite_json_number(value)?;
    if !(opacity as f32).is_finite() {
        return Err("authoring form opacity exceeds native float range".into());
    }
    Ok(opacity)
}

fn json_number(value: f64) -> Result<Value, String> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| "authoring scalar is not representable".into())
}

fn finite_positions(value: Option<&Value>, name: &str) -> Result<Vec<f64>, String> {
    let positions = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name} must be an array"))?;
    if positions.is_empty() || positions.len() % 2 != 0 {
        return Err(format!("{name} must contain coordinate pairs"));
    }
    positions.iter().map(finite_json_number).collect()
}

fn source_position_count(data: &Value, source_type: &str) -> Result<usize, String> {
    match source_type {
        "ArtMesh" => Ok(finite_positions(data.get("positions"), "source positions")?.len()),
        "Warp" => {
            let dimension = |name| {
                data.get(name)
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| format!("authoring Warp source has invalid {name}"))
            };
            let columns = dimension("columns")?;
            let rows = dimension("rows")?;
            columns
                .checked_add(1)
                .and_then(|columns| {
                    rows.checked_add(1)
                        .and_then(|rows| columns.checked_mul(rows))
                })
                .and_then(|vertices| vertices.checked_mul(2))
                .ok_or_else(|| "authoring Warp grid size is invalid".into())
        }
        _ => Err("authoring vertex edit supports only ArtMesh and Warp sources".into()),
    }
}

fn form_changes(initial: &Value, model: &Value) -> Result<Vec<FormChange>, String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring edit requires a source.v1 model".into());
    }
    let initial_sources = initial
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("authoring initial model lacks sources")?;
    let candidate_sources = model
        .get("sources")
        .and_then(Value::as_array)
        .filter(|sources| sources.len() == initial_sources.len())
        .ok_or("authoring model changed the source list")?;
    let mut normalized = model.clone();
    let mut budget = Budget::new(MAX_SCALARS);
    let mut changes = Vec::new();

    for (source_index, (old_source, new_source)) in
        initial_sources.iter().zip(candidate_sources).enumerate()
    {
        let source_guid = required_nonempty_string(old_source.get("guid"), "source GUID")?;
        if new_source.get("guid").and_then(Value::as_str) != Some(source_guid) {
            return Err("authoring model changed source identity or order".into());
        }
        let old_data = old_source
            .get("data")
            .ok_or("authoring source lacks data")?;
        let source_type = required_string(old_data.get("type"), "source data type")?;
        if !matches!(source_type, "ArtMesh" | "Warp") {
            continue;
        }
        let expected_positions = source_position_count(old_data, source_type)?;
        let old_forms = old_source
            .get("forms")
            .and_then(Value::as_array)
            .ok_or("authoring source lacks primary forms")?;
        let new_forms = new_source
            .get("forms")
            .and_then(Value::as_array)
            .filter(|forms| forms.len() == old_forms.len())
            .ok_or("authoring model changed the primary form list")?;
        for (form_index, (old_form, new_form)) in old_forms.iter().zip(new_forms).enumerate() {
            let form_guid = required_nonempty_string(old_form.get("guid"), "form GUID")?;
            if new_form.get("guid").and_then(Value::as_str) != Some(form_guid)
                || optional_form_coordinate(new_form)? != optional_form_coordinate(old_form)?
            {
                return Err("authoring model changed primary form identity".into());
            }
            let old_data = old_form
                .get("data")
                .ok_or("authoring primary form lacks data")?;
            let new_data = new_form
                .get("data")
                .ok_or("authoring primary form lacks data")?;
            if required_string(old_data.get("type"), "form data type")? != source_type
                || required_string(new_data.get("type"), "form data type")? != source_type
            {
                return Err("authoring primary form type does not match its source".into());
            }
            let old_positions = finite_positions(old_data.get("positions"), "form positions")?;
            let new_positions = finite_positions(new_data.get("positions"), "form positions")?;
            if old_positions.len() != expected_positions
                || new_positions.len() != old_positions.len()
            {
                return Err("authoring primary form vertex count changed".into());
            }
            if old_positions
                .iter()
                .zip(&new_positions)
                .any(|(old, new)| old.to_bits() != new.to_bits() && !(*new as f32).is_finite())
            {
                return Err("authoring vertex position exceeds native float range".into());
            }
            budget.consume(new_positions.len())?;
            let changed_positions = (!same_positions(&old_positions, &new_positions))
                .then(|| (old_positions, new_positions));
            let old_opacity = finite_json_number(
                old_form
                    .get("opacity")
                    .ok_or("authoring primary form lacks opacity")?,
            )?;
            let new_opacity = finite_json_number(
                new_form
                    .get("opacity")
                    .ok_or("authoring primary form lacks opacity")?,
            )?;
            let changed_opacity = (old_opacity.to_bits() != new_opacity.to_bits())
                .then_some((old_opacity, new_opacity));
            if let Some((_, value)) = changed_opacity {
                if source_type != "ArtMesh" {
                    return Err("authoring opacity edit supports only ArtMesh forms".into());
                }
                if !(value as f32).is_finite() {
                    return Err("authoring form opacity exceeds native float range".into());
                }
                ensure_shape_opacity(old_source, form_guid, value)?;
            }
            if changed_positions.is_some() || changed_opacity.is_some() {
                ensure_form_editable(old_source, form_guid)?;
                changes.push(FormChange {
                    key: FormKey {
                        source_guid: source_guid.to_owned(),
                        form_guid: form_guid.to_owned(),
                        coordinate_type: optional_form_coordinate(old_form)?.map(str::to_owned),
                    },
                    positions: changed_positions,
                    opacity: changed_opacity,
                });
            }
            normalized["sources"][source_index]["forms"][form_index]["data"]["positions"] =
                old_data["positions"].clone();
            normalized["sources"][source_index]["forms"][form_index]["opacity"] =
                old_form["opacity"].clone();
        }
    }
    if normalized != *initial {
        return Err(
            "authoring edit may change only primary form positions or ArtMesh opacity".into(),
        );
    }
    Ok(changes)
}

fn patch_primary_fields(
    text: &str,
    changes: &[FormChange],
    names: &[NameChange],
    structure: &Structure<'_>,
) -> Result<String, String> {
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 1_000_000,
            ..Default::default()
        },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))?;
    if !document.root_element().has_tag_name("root") {
        return Err("authoring XML root must be root".into());
    }
    let graph = Graph::new(&document)?;
    let mut budget = Budget::new(MAX_SCALARS);
    budget.consume(
        changes
            .len()
            .checked_mul(2)
            .ok_or("authoring edit index exceeds scalar budget")?,
    )?;
    let mut remaining = HashMap::with_capacity(changes.len());
    let mut changed_sources = HashSet::with_capacity(changes.len());
    for change in changes {
        remaining.insert(change.key.clone(), change);
        changed_sources.insert(change.key.source_guid.as_str());
    }
    if remaining.len() != changes.len() {
        return Err("authoring edit contains duplicate form changes".into());
    }
    let xml_index = build_xml_index(&document, &graph, &mut budget)?;
    let mut patches = Vec::new();

    let Some(model) = graph.model()? else {
        return Err("authoring source contains no model".into());
    };
    for set_name in ["drawableSourceSet", "deformerSourceSet"] {
        for source in graph.source_list(model, set_name)? {
            let tag = source.tag_name().name();
            if !matches!(tag, "CArtMeshSource" | "CWarpDeformerSource") {
                continue;
            }
            let Some(source_guid) = graph.guid(source)? else {
                return Err("authoring editable source lacks GUID".into());
            };
            if !changed_sources.contains(source_guid) {
                continue;
            }
            let source_type = if tag == "CArtMeshSource" {
                "ArtMesh"
            } else {
                "Warp"
            };
            let form_list = direct_field(source, "keyforms")
                .ok_or("authoring primary forms must use direct source keyforms storage")?;
            if form_list.attribute("xs.ref").is_some() {
                return Err("authoring primary forms use shared keyforms storage".into());
            }
            let form_list = graph.resolve(form_list)?;
            if !form_list.has_tag_name("carray_list") {
                return Err("authoring primary forms must use a carray_list".into());
            }
            reject_inbound_refs(&xml_index, form_list)?;
            if form_list
                .children()
                .filter(|child| child.is_element())
                .any(|row| row.attribute("xs.ref").is_some())
            {
                return Err("authoring primary form list contains a shared row".into());
            }
            for form in values::list(&graph, form_list, &mut budget)? {
                let Some(form_guid) = graph.guid(form)? else {
                    return Err("authoring editable form lacks GUID".into());
                };
                let coordinate_type = form_coordinate(&graph, form)?;
                let key = FormKey {
                    source_guid: source_guid.to_owned(),
                    form_guid: form_guid.to_owned(),
                    coordinate_type,
                };
                let Some(change) = remaining.remove(&key) else {
                    continue;
                };
                let expected_tag = if source_type == "ArtMesh" {
                    "CArtMeshForm"
                } else {
                    "CWarpDeformerForm"
                };
                if !form.has_tag_name(expected_tag) {
                    return Err("authoring primary form type does not match its source".into());
                }
                if let Some((old_positions, positions)) = &change.positions {
                    let raw_array = direct_field(form, "positions")
                        .ok_or("authoring primary positions must be form-local")?;
                    if raw_array.attribute("xs.ref").is_some() {
                        return Err("authoring primary positions use a shared xs.ref array".into());
                    }
                    let array = graph.resolve(raw_array)?;
                    let current_positions = values::float_array(&graph, array, &mut budget)?;
                    if !same_positions(&current_positions, old_positions)
                        || !array.has_tag_name("float-array")
                        || array.attribute("xs.n") != Some("positions")
                        || array
                            .attribute("count")
                            .and_then(|count| count.parse::<usize>().ok())
                            != Some(positions.len())
                    {
                        return Err(
                            "authoring primary positions are not an inline float-array".into()
                        );
                    }
                    reject_shared_positions(&xml_index, form, array)?;
                    patches.extend(position_token_patches(text, array, positions)?);
                }
                if let Some((old_opacity, opacity)) = change.opacity {
                    if source_type != "ArtMesh" {
                        return Err("authoring opacity edit supports only ArtMesh forms".into());
                    }
                    let field = inline_form_field(&graph, &xml_index, form, "opacity")?
                        .ok_or("authoring primary form lacks an explicit opacity field")?;
                    if !field.has_tag_name("f") || field.attribute("xs.n") != Some("opacity") {
                        return Err("authoring primary opacity is not an inline float field".into());
                    }
                    patches.push(opacity_token_patch(text, field, old_opacity, opacity)?);
                }
            }
        }
    }
    if !remaining.is_empty() {
        return Err("authoring primary form fields could not be located in XML".into());
    }
    for change in names {
        patches.push(name_patch(text, &graph, model, change)?);
    }
    if !structure.targets.is_empty() || !structure.brushes.is_empty() {
        let sources = crate::authoring_reparent::source_nodes(&graph, model)?;
        for change in structure.targets {
            patches.push(target_patch(text, &graph, &xml_index, &sources, change)?);
        }
        if !structure.brushes.is_empty() {
            let brushes = brush_nodes(&graph, model)?;
            for change in structure.brushes {
                patches.push(brush_patch(text, &graph, &sources, &brushes, change)?);
            }
        }
    }
    patches.extend(crate::authoring_fields::patches(text, &graph, model, structure.fields, &mut budget)?);
    patches.extend(crate::authoring_reparent::patches(text, &graph, model, structure.moves, &mut budget)?);
    patches.extend(crate::authoring_clips::patches(text, &graph, model, structure.clips, &mut budget)?);
    patches.extend(crate::authoring_delete::patches(text, &graph, model, structure.deletions, &mut budget)?);
    patches.sort_by_key(|patch| patch.range.start);
    if patches
        .windows(2)
        .any(|pair| pair[0].range.end > pair[1].range.start)
    {
        return Err("authoring primary field ranges overlap".into());
    }
    let mut rewritten = text.to_owned();
    for patch in patches.into_iter().rev() {
        rewritten.replace_range(patch.range, &patch.value);
    }
    Ok(rewritten)
}

fn inline_form_field<'doc, 'input>(
    graph: &Graph<'doc, 'input>,
    index: &XmlIndex<'_>,
    form: Node<'doc, 'input>,
    name: &str,
) -> Result<Option<Node<'doc, 'input>>, String> {
    reject_inbound_refs(index, form)?;
    let Some(field) = private_field_slot(index, form, name)? else { return Ok(None) };
    if field.attribute("xs.ref").is_some() {
        return Err("authoring opacity field uses xs.ref storage".into());
    }
    reject_inbound_refs(index, field)?;
    let resolved = graph.field(form, name)?.ok_or("authoring primary form lacks an explicit opacity field")?;
    if resolved.id() != field.id() {
        return Err("authoring opacity field does not resolve inline".into());
    }
    Ok(Some(resolved))
}

/// Locate a stored slot without following aliases; callers validate their own root and terminal storage.
fn private_field_slot<'doc, 'input>(
    index: &XmlIndex<'_>,
    owner: Node<'doc, 'input>,
    name: &str,
) -> Result<Option<Node<'doc, 'input>>, String> {
    let mut current = owner;
    let mut visited = HashSet::new();
    for _ in 0..=64 {
        if !visited.insert(current.id()) {
            return Err("authoring super cycle".into());
        }
        if current.attribute("xs.ref").is_some() {
            return Err(format!("authoring {name} path uses xs.ref storage"));
        }
        if current.id() != owner.id() {
            reject_inbound_refs(index, current)?;
        }
        let fields: Vec<_> = current
            .children()
            .filter(|child| child.is_element() && child.attribute("xs.n") == Some(name))
            .collect();
        if fields.len() > 1 {
            return Err(format!("authoring {name} path has duplicate fields"));
        }
        let parents: Vec<_> = current
            .children()
            .filter(|child| child.is_element() && child.attribute("xs.n") == Some("super"))
            .collect();
        if parents.len() > 1 {
            return Err(format!("authoring {name} path has duplicate super fields"));
        }
        if let Some(field) = fields.first().copied() {
            return Ok(Some(field));
        }
        let Some(parent) = parents.first().copied() else {
            return Ok(None);
        };
        if parent.attribute("xs.ref").is_some() {
            return Err(format!("authoring {name} path uses xs.ref storage"));
        }
        reject_inbound_refs(index, parent)?;
        current = parent;
    }
    Err("authoring super chain exceeds 64 nodes".into())
}

fn target_patch<'doc, 'input>(
    text: &str,
    graph: &Graph<'doc, 'input>,
    index: &XmlIndex<'_>,
    sources: &HashMap<&str, Node<'doc, 'input>>,
    change: &TargetChange,
) -> Result<XmlPatch, String> {
    let source = *sources.get(change.source_guid.as_str()).ok_or("authoring target source was not found")?;
    let slot = private_field_slot(index, source, "targetDeformerGuid")?
        .ok_or("authoring source has no targetDeformerGuid storage")?;
    if !matches!(slot.tag_name().name(), "CDeformerGuid" | "CRotationDeformerGuid") || slot.attribute("xs.id").is_some()
        || slot.children().any(|child| child.is_element())
    {
        return Err("authoring target link is not a private CDeformerGuid slot".into());
    }
    if graph.resolve(slot)?.attribute("uuid") != Some(change.from.as_str()) {
        return Err("authoring old target link differs from its source model".into());
    }
    let (attribute, value) = match (slot.attribute("uuid"), slot.attribute("xs.ref")) {
        (Some(_), None) => ("uuid", change.to.as_str()),
        (None, Some(_)) => {
            let destination = *sources.get(change.to.as_str()).ok_or("authoring destination was not found")?;
            let guid = graph.field(destination, "guid")?.ok_or("authoring destination has no GUID field")?;
            if !matches!(guid.tag_name().name(), "CDeformerGuid" | "CRotationDeformerGuid")
                || guid.attribute("uuid") != Some(change.to.as_str())
            {
                return Err("authoring destination has unsupported GUID storage".into());
            }
            ("xs.ref", guid.attribute("xs.id").ok_or("authoring destination GUID has no stored xs.id")?)
        }
        _ => return Err("authoring target link has ambiguous UUID/reference storage".into()),
    };
    let value = value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('\'', "&apos;").replace('"', "&quot;");
    crate::authoring_reparent::attribute_patch(text, slot, attribute, value)
}

fn brush_patch<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    sources: &HashMap<&str, Node<'d, 'i>>,
    brushes: &[Node<'d, 'i>],
    change: &BrushChange,
) -> Result<XmlPatch, String> {
    let source = *sources.get(change.source_guid.as_str()).ok_or("authoring brush source was not found")?;
    if !source.has_tag_name("CArtPathSource") {
        return Err("authoring brush selection requires an ArtPath source".into());
    }
    let mut slots = source.children().filter(|node| node.is_element() && node.attribute("xs.n") == Some("brushGuid"));
    let slot = slots.next().ok_or("authoring ArtPath has no direct brushGuid")?;
    if slots.next().is_some() || !slot.has_tag_name("CArtPathBrushGuid")
        || slot.attribute("xs.id").is_some() || slot.children().next().is_some()
    {
        return Err("authoring brush link is not one private direct CArtPathBrushGuid slot".into());
    }
    let attribute = match (slot.attribute("uuid"), slot.attribute("xs.ref")) {
        (Some(_), None) => "uuid",
        (None, Some(_)) => "xs.ref",
        _ => return Err("authoring brush link has ambiguous UUID/reference storage".into()),
    };
    let old = graph.resolve(slot)?;
    if !old.has_tag_name("CArtPathBrushGuid") || old.attribute("uuid") != Some(change.from.as_str()) {
        return Err("authoring old brush link differs from its source model".into());
    }
    let mut matches = Vec::new();
    for brush in brushes {
        if graph.guid(*brush)? == Some(change.to.as_str()) {
            matches.push(*brush);
        }
    }
    let [destination] = matches.as_slice() else {
        return Err("authoring brush destination is not unique in the model brush list".into());
    };
    if !destination.has_tag_name("CArtPathBrush") {
        return Err("authoring brush destination is not a brush".into());
    }
    let guid = graph.field(*destination, "guid")?.ok_or("authoring destination brush lacks GUID")?;
    if !guid.has_tag_name("CArtPathBrushGuid") || guid.attribute("uuid") != Some(change.to.as_str()) {
        return Err("authoring destination brush has an invalid GUID".into());
    }
    let destination_id = guid.attribute("xs.id");
    let value = if attribute == "xs.ref" { destination_id.unwrap_or(&change.to) } else { &change.to };
    let escaped = value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('\'', "&apos;").replace('"', "&quot;")
        .replace('\t', "&#9;").replace('\n', "&#10;").replace('\r', "&#13;");
    let stored = slot.attributes().find(|candidate| candidate.name() == attribute).unwrap();
    let range = stored.range();
    if attribute == "xs.ref" && destination_id.is_none() {
        return Ok(XmlPatch { range, value: format!("uuid=\"{escaped}\"") });
    }
    let raw = &text[range.clone()];
    let opening = raw.find(['\'', '"']).ok_or("authoring brush attribute lacks a quote")?;
    let quote = raw[opening..].chars().next().unwrap();
    let closing = raw.rfind(quote).filter(|end| *end > opening).ok_or("authoring brush attribute lacks its closing quote")?;
    Ok(XmlPatch { range: range.start + opening + 1..range.start + closing, value: escaped })
}

pub(crate) fn form_coordinate(graph: &Graph<'_, '_>, form: Node<'_, '_>) -> Result<Option<String>, String> {
    graph
        .field(form, "coordType")?
        .map(|coordinate| {
            values::text_field(graph, coordinate, "coordName")?
                .map(str::to_owned)
                .ok_or_else(|| "authoring coordType lacks coordName".into())
        })
        .transpose()
}

fn build_xml_index<'doc, 'input>(
    document: &'doc Document<'input>,
    graph: &Graph<'doc, 'input>,
    budget: &mut Budget,
) -> Result<XmlIndex<'doc>, String> {
    let inbound_count = document
        .descendants()
        .filter(|node| node.is_element() && node.attribute("xs.ref").is_some())
        .count();
    let form_count = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && matches!(node.tag_name().name(), "CArtMeshForm" | "CWarpDeformerForm")
        })
        .count();
    budget.consume(
        inbound_count
            .checked_add(form_count)
            .ok_or("authoring XML index exceeds scalar budget")?,
    )?;

    let mut inbound_refs = HashSet::with_capacity(inbound_count);
    for node in document.descendants().filter(|node| node.is_element()) {
        if let Some(reference) = node.attribute("xs.ref") {
            inbound_refs.insert(reference);
        }
    }
    let mut position_users = HashMap::with_capacity(form_count);
    for form in document.descendants().filter(|node| {
        node.is_element() && matches!(node.tag_name().name(), "CArtMeshForm" | "CWarpDeformerForm")
    }) {
        if let Some(array) = graph.field(form, "positions")? {
            let count = position_users.entry(array.id()).or_insert(0usize);
            *count = count
                .checked_add(1)
                .ok_or("authoring form position use count overflow")?;
        }
    }
    Ok(XmlIndex {
        inbound_refs,
        position_users,
    })
}

fn reject_shared_positions(
    index: &XmlIndex<'_>,
    form: Node<'_, '_>,
    array: Node<'_, '_>,
) -> Result<(), String> {
    reject_inbound_refs(index, form)?;
    reject_inbound_refs(index, array)?;
    if index.position_users.get(&array.id()).copied().unwrap_or(0) > 1 {
        return Err("authoring primary positions are shared by multiple forms".into());
    }
    Ok(())
}

fn direct_field<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.attribute("xs.n") == Some(name))
}

fn reject_inbound_refs(index: &XmlIndex<'_>, node: Node<'_, '_>) -> Result<(), String> {
    if node
        .attribute("xs.id")
        .is_some_and(|id| index.inbound_refs.contains(id))
    {
        return Err("authoring edit target has inbound xs.ref aliases".into());
    }
    Ok(())
}

fn position_token_patches(
    text: &str,
    array: Node<'_, '_>,
    positions: &[f64],
) -> Result<Vec<XmlPatch>, String> {
    let mut children = array.children();
    let content = children
        .next()
        .ok_or("authoring primary positions must be one inline text node")?;
    if !content.is_text() || children.next().is_some() {
        return Err("authoring primary positions must be one inline text node".into());
    }
    let range = content.range();
    let raw = text
        .get(range.clone())
        .ok_or("authoring position text range is invalid")?;
    if array.text() != Some(raw) {
        return Err("authoring position text is normalized by XML parsing".into());
    }
    let mut tokens = Vec::new();
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if start < cursor {
            let value = raw[start..cursor]
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or("authoring position token is not plain finite numeric text")?;
            tokens.push((range.start + start..range.start + cursor, value));
        }
    }
    if tokens.len() != positions.len() {
        return Err("authoring primary position token count changed".into());
    }
    Ok(tokens
        .into_iter()
        .zip(positions)
        .filter_map(|((range, old), new)| {
            (old.to_bits() != new.to_bits()).then(|| XmlPatch {
                range,
                value: new.to_string(),
            })
        })
        .collect())
}

fn opacity_token_patch(
    text: &str,
    field: Node<'_, '_>,
    old_opacity: f64,
    opacity: f64,
) -> Result<XmlPatch, String> {
    let mut children = field.children();
    let content = children
        .next()
        .ok_or("authoring opacity must be one inline text node")?;
    if !content.is_text() || children.next().is_some() {
        return Err("authoring opacity must be one inline text node".into());
    }
    let range = content.range();
    let raw = text
        .get(range.clone())
        .ok_or("authoring opacity text range is invalid")?;
    if field.text() != Some(raw) {
        return Err("authoring opacity text is normalized by XML parsing".into());
    }
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut token = None;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if start < cursor {
            if token.is_some() {
                return Err("authoring opacity must contain one numeric token".into());
            }
            let value = raw[start..cursor]
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or("authoring opacity token is not plain finite numeric text")?;
            token = Some((range.start + start..range.start + cursor, value));
        }
    }
    let (range, actual) = token.ok_or("authoring opacity must contain one numeric token")?;
    if actual.to_bits() != old_opacity.to_bits() {
        return Err("authoring primary opacity differs from the source model".into());
    }
    if !opacity.is_finite() || !(opacity as f32).is_finite() {
        return Err("authoring form opacity exceeds native float range".into());
    }
    Ok(XmlPatch {
        range,
        value: opacity.to_string(),
    })
}

fn same_positions(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn model() -> Value {
        json!({
            "format":"live2d.authoring.source.v1",
            "sources":[{
                "guid":"mesh-guid",
                "data":{"type":"ArtMesh","positions":[0.0,0.0,1.0,1.0]},
                "forms":[{
                    "guid":"form-guid","coordinateType":"Canvas","opacity":0.75,
                    "data":{"type":"ArtMesh","positions":[2.0,3.0,4.0,5.0]}
                }]
            }]
        })
    }

    fn warp_xml() -> String {
        "<root fileFormatVersion='401000004'><CModelSource><s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas><CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='0'/></CParameterSourceSet><CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='0'/></CDrawableSourceSet><CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='1'><CWarpDeformerSource xs.ref='warp'/></carray_list></CDeformerSourceSet><CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet><CPartSource xs.n='rootPart' xs.ref='part'/></CModelSource><CWarpDeformerSource xs.id='warp'><CWarpGuid xs.n='guid' uuid='warp-guid'/><CWarpId xs.n='id' idstr='warp'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/><i xs.n='col'>1</i><i xs.n='row'>1</i><carray_list xs.n='keyforms' count='1'><CWarpDeformerForm><CFormGuid xs.n='guid' uuid='form-guid'/><CWarpDeformerSource xs.n='_source' xs.ref='warp'/><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType><float-array xs.n='positions' count='8'> 1.00\t2  3 4 5 6 7 8 </float-array><s xs.n='future'>keep-this-field</s></CWarpDeformerForm></carray_list></CWarpDeformerSource><CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource><Opaque xs.id='future-data'>keep-this-xml</Opaque></root>".into()
    }

    pub(crate) fn artmesh_xml() -> String {
        r#"<root fileFormatVersion='401000004'><CModelSource>
          <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
          <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='0'/></CParameterSourceSet>
          <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='1'><CArtMeshSource xs.ref='mesh'/></carray_list></CDrawableSourceSet>
          <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/></CDeformerSourceSet>
          <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet>
          <CPartSource xs.n='rootPart' xs.ref='part'/>
        </CModelSource>
        <CArtMeshSource xs.id='mesh'>
          <ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/><CArtMeshId xs.n='id' idstr='mesh'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
          <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1</float-array>
          <int-array xs.n='indices' count='3'>0 1 2</int-array><GTexture2D xs.n='texture' xs.ref='texture'/>
          <carray_list xs.n='keyforms' count='1'><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
            <CFormGuid xs.n='guid' uuid='form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>
          </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>
            <f xs.n='opacity'>0.75</f><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>
          </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
        </CArtMeshSource>
        <CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
        <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
          <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
        </GTexture2D><Opaque xs.id='future-data'>keep-this-xml</Opaque></root>"#.into()
    }

    pub(crate) fn test_container(xml: &str) -> CaffDocument {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        let crc = crc32fast::hash(xml.as_bytes());
        let mut zip = Vec::new();
        zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0x0808u16.to_le_bytes());
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&[0; 4]);
        zip.extend_from_slice(&[0; 12]);
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(b"contents");
        zip.extend_from_slice(&compressed);
        zip.extend_from_slice(b"PK\x07\x08");
        zip.extend_from_slice(&crc.to_le_bytes());
        zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(xml.len() as u32).to_le_bytes());

        let seed = 0x562e_099f_u32;
        let mask = seed as u8;
        let entries = [
            ("main.xml", "main_xml", [0x01, 0x21], zip),
            ("mesh.png", "", [0x01, 0x10], vec![0x89, b'P', b'N', b'G']),
            ("opaque.bin", "", [0x01, 0x10], vec![0, 1, 2, 0xff]),
        ];
        let mut bytes = vec![0; 58];
        bytes[..4].copy_from_slice(b"CAFF");
        bytes[4..8].copy_from_slice(&45u32.to_be_bytes());
        bytes[14..18].copy_from_slice(&seed.to_be_bytes());
        bytes[54..58].copy_from_slice(&((entries.len() as u32) ^ seed).to_be_bytes());
        let mut fields = Vec::new();
        for (name, alias, flags, _) in &entries {
            bytes.push(name.len() as u8 ^ mask);
            bytes.extend(name.as_bytes().iter().map(|byte| byte ^ mask));
            bytes.push(alias.len() as u8 ^ mask);
            bytes.extend(alias.as_bytes().iter().map(|byte| byte ^ mask));
            bytes.extend_from_slice(&(0x1234_5678u32 ^ seed).to_be_bytes());
            let offset_field = bytes.len();
            bytes.extend_from_slice(&seed.to_be_bytes());
            let size_field = bytes.len();
            bytes.extend_from_slice(&seed.to_be_bytes());
            bytes.extend(flags.iter().map(|byte| byte ^ mask));
            bytes.extend([0; 8].iter().map(|byte| byte ^ mask));
            fields.push((offset_field, size_field));
        }
        for (index, (_, _, _, payload)) in entries.iter().enumerate() {
            let offset = bytes.len();
            bytes[fields[index].0..fields[index].0 + 4]
                .copy_from_slice(&((offset as u32 ^ seed).to_be_bytes()));
            bytes[fields[index].1..fields[index].1 + 4]
                .copy_from_slice(&((payload.len() as u32 ^ seed).to_be_bytes()));
            bytes.extend(payload.iter().map(|byte| byte ^ mask));
        }
        bytes.extend_from_slice(b"bc");
        CaffDocument::parse(&bytes).unwrap()
    }

    fn warp_xml_with_second_form() -> String {
        let mut xml = warp_xml().replace("keyforms' count='1'", "keyforms' count='2'");
        let second_form = "<CWarpDeformerForm><CFormGuid xs.n='guid' uuid='form-guid-2'/><CWarpDeformerSource xs.n='_source' xs.ref='warp'/><CoordType xs.n='coordType'><s xs.n='coordName'>DeformerLocal</s></CoordType><float-array xs.n='positions' count='8'>9 10 11 12 13 14 15 16</float-array></CWarpDeformerForm>";
        xml = xml.replace(
            "</carray_list></CWarpDeformerSource>",
            &format!("{second_form}</carray_list></CWarpDeformerSource>"),
        );
        xml
    }

    #[test]
    fn set_vertex_changes_one_primary_vertex_and_preserves_the_model() {
        let before = model();
        let after = set_vertex(
            &before,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":1,"position":[8.5,-9.25]}),
        )
        .unwrap();
        assert_eq!(
            after["sources"][0]["forms"][0]["data"]["positions"],
            json!([2.0, 3.0, 8.5, -9.25])
        );
        assert_eq!(
            before["sources"][0]["forms"][0]["data"]["positions"],
            json!([2.0, 3.0, 4.0, 5.0])
        );
        assert_eq!(after["sources"][0]["data"], before["sources"][0]["data"]);
    }

    #[test]
    fn set_vertex_requires_exact_form_coordinate_and_finite_pair() {
        let before = model();
        for request in [
            json!({"sourceGuid":"missing","formGuid":"form-guid","coordinateType":"Canvas","vertexIndex":0,"position":[1,2]}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":null,"vertexIndex":0,"position":[1,2]}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas","vertexIndex":2,"position":[1,2]}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas","vertexIndex":0,"position":[1,"bad"]}),
        ] {
            assert!(set_vertex(&before, &request).is_err());
        }
    }

    #[test]
    fn set_opacity_changes_only_the_exact_primary_artmesh_form() {
        let before = model();
        let after = set_opacity(
            &before,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":1.00005}),
        )
        .unwrap();
        assert_eq!(after["sources"][0]["forms"][0]["opacity"], json!(1.00005));
        assert_eq!(after["sources"][0]["data"], before["sources"][0]["data"]);
        assert_eq!(
            after["sources"][0]["forms"][0]["data"],
            before["sources"][0]["forms"][0]["data"]
        );
        assert_eq!(before["sources"][0]["forms"][0]["opacity"], json!(0.75));
        let negative = set_opacity(
            &before,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":-0.25}),
        )
        .unwrap();
        assert_eq!(negative["sources"][0]["forms"][0]["opacity"], json!(-0.25));
    }

    #[test]
    fn paste_form_blends_inverts_and_multi_key_opacity_adds() {
        let mut before = model();
        let mut second = before["sources"][0]["forms"][0].clone();
        second["guid"] = json!("form-guid-2");
        second["data"]["positions"] = json!([6.0, 7.0, 8.0, 9.0]);
        second["opacity"] = json!(0.25);
        before["sources"][0]["forms"].as_array_mut().unwrap().push(second);
        let paste = json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas",
            "fromFormGuid":"form-guid-2"});
        let mut blend = paste.clone();
        blend["ratio"] = json!(0.5);
        let after = paste_form(&before, &blend).unwrap();
        assert_eq!(after["sources"][0]["forms"][0]["data"]["positions"], json!([4.0, 5.0, 6.0, 7.0]));
        assert_eq!(after["sources"][0]["forms"][0]["opacity"], json!(0.5));
        assert_eq!(after["sources"][0]["forms"][1], before["sources"][0]["forms"][1]);
        let mut invert = paste.clone();
        invert["invert"] = json!("vertical");
        let after = paste_form(&before, &invert).unwrap();
        assert_eq!(after["sources"][0]["forms"][0]["data"]["positions"], json!([8.0, 7.0, 6.0, 9.0]));
        let mut opacity_only = paste.clone();
        opacity_only["properties"] = json!(["opacity"]);
        let after = paste_form(&before, &opacity_only).unwrap();
        assert_eq!(after["sources"][0]["forms"][0]["data"], before["sources"][0]["forms"][0]["data"]);
        assert_eq!(after["sources"][0]["forms"][0]["opacity"], json!(0.25));
        for bad in [json!({"ratio": 2.0}), json!({"invert": "up"}), json!({"properties": ["colors"]}), json!({"fromFormGuid": "missing"}),
            json!({"coordinateType": "DeformerLocal"})] {
            let mut request = paste.clone();
            request.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
            assert!(paste_form(&before, &request).is_err(), "{request}");
        }
        let forms = json!([
            {"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas"},
            {"sourceGuid":"mesh-guid","formGuid":"form-guid-2","coordinateType":"Canvas"}]);
        let after = set_opacities(&before, &json!({"forms": forms, "opacity": 0.5, "method": "add"})).unwrap();
        assert_eq!(after["sources"][0]["forms"][0]["opacity"], json!(1.0));
        assert_eq!(after["sources"][0]["forms"][1]["opacity"], json!(0.75));
        let after = set_opacities(&before, &json!({"forms": forms, "opacity": 0.1})).unwrap();
        assert_eq!(after["sources"][0]["forms"][1]["opacity"], json!(0.1));
    }

    #[test]
    fn set_opacity_rejects_invalid_identity_type_and_derived_sources() {
        let before = model();
        for request in [
            json!({"sourceGuid":"missing","formGuid":"form-guid","coordinateType":"Canvas","opacity":0.5}),
            json!({"sourceGuid":"mesh-guid","formGuid":"missing","coordinateType":"Canvas","opacity":0.5}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Other","opacity":0.5}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas","opacity":"0.5"}),
            json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas","opacity":1e300}),
        ] {
            assert!(set_opacity(&before, &request).is_err(), "{request}");
        }
        let mut missing_opacity = before.clone();
        missing_opacity["sources"][0]["forms"][0]
            .as_object_mut()
            .unwrap()
            .remove("opacity");
        assert!(set_opacity(
            &missing_opacity,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":0.5}),
        )
        .is_err());

        let mut warp = before.clone();
        warp["sources"][0]["data"]["type"] = json!("Warp");
        assert!(set_opacity(
            &warp,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":0.5}),
        )
        .unwrap_err()
        .contains("only ArtMesh"));

        let request = json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
            "coordinateType":"Canvas","opacity":0.5});
        let mut cached = before.clone();
        cached["sources"][0]["extendedInterpolations"] = json!([{}]);
        assert!(set_opacity(&cached, &request)
            .unwrap_err()
            .contains("extended interpolation"));
        let mut morphed = before;
        morphed["sources"][0]["morphTargets"] = json!({"targets":[{}],"blendWeightConstraints":[]});
        assert!(set_opacity(&morphed, &request)
            .unwrap_err()
            .contains("primary morph metadata"));
        morphed["sources"][0]["morphTargets"]["targets"] = json!([{"formGuid":"form-guid"}]);
        assert!(set_opacity(&morphed, &request).is_ok());
        // A form that is a shape and also a cell of the keyform grid is a base form too.
        morphed["sources"][0]["grid"] = json!({"axes":[],"cells":[{"indices":[],"formGuid":"form-guid"}]});
        assert!(set_opacity(&morphed, &request).unwrap_err().contains("primary morph metadata"));
    }

    /// `artmesh_xml` with one blend shape: a second form that a morph target of the source names.
    pub(crate) fn morph_artmesh_xml() -> String {
        artmesh_xml()
            .replace(
                "<CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='0'/>",
                "<CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='1'><CParameterSource>\
                 <CParameterGuid xs.n='guid' uuid='parameter-guid'/><CParameterId xs.n='id' idstr='ParamMorph'/>\
                 <f xs.n='minValue'>0</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource></carray_list>",
            )
            .replace(
                "<CArtMeshSource xs.id='mesh'>",
                "<CArtMeshSource xs.id='mesh'><KeyFormMorphTargetSet xs.n='keyformMorphTargetSet'>\
                 <carray_list xs.n='_morphTargets' count='1'><KeyFormMorphTarget>\
                 <CParameterGuid xs.n='parameterGuid' uuid='parameter-guid'/><f xs.n='keyValue'>1</f>\
                 <CFormGuid xs.n='keyformGuid' uuid='shape-guid'/><CArtMeshSource xs.n='owner' xs.ref='mesh'/>\
                 <MorphTargetEditBaseParameterMap xs.n='editBaseParameterMap'><null xs.n='baseParameterMap'/></MorphTargetEditBaseParameterMap>\
                 </KeyFormMorphTarget></carray_list>\
                 <MorphTargetBlendWeightConstraintSet xs.n='blendWeightConstraintSet'><carray_list xs.n='_constraints' count='0'/>\
                 </MorphTargetBlendWeightConstraintSet></KeyFormMorphTargetSet>",
            )
            .replace(
                "<float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array>",
                "<KeyformGridSource xs.n='keyformGridSource'><array_list xs.n='keyformBindings' count='0'/>\
                 <array_list xs.n='keyformsOnGrid' count='1'><KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'>\
                 <array_list xs.n='_keyOnParameterList' count='0'/></KeyformGridAccessKey>\
                 <CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid></array_list></KeyformGridSource>\
                 <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array>",
            )
            .replace("<carray_list xs.n='keyforms' count='1'>", "<carray_list xs.n='keyforms' count='2'>")
            .replace(
                "</CArtMeshForm></carray_list>",
                "</CArtMeshForm><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>\
                 <CFormGuid xs.n='guid' uuid='shape-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>\
                 </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>\
                 <f xs.n='opacity'>0.5</f><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>\
                 </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 7 8 5 6</float-array></CArtMeshForm></carray_list>",
            )
    }

    #[test]
    fn an_id_change_rewrites_one_idstr_and_reaches_the_morph_targets() {
        let original = morph_artmesh_xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let parameter = set_id(&initial, &json!({"guid": "parameter-guid", "id": "ParamShape"})).unwrap();
        assert_eq!(parameter["parameters"][0]["id"], "ParamShape");
        let mesh = set_id(&parameter, &json!({"guid": "mesh-guid", "id": "ArtMeshFace"})).unwrap();
        let bytes = rewrite(&container, &initial, &mesh).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), mesh);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original.replace("idstr='ParamMorph'", "idstr='ParamShape'").replace("idstr='mesh'", "idstr='ArtMeshFace'")
        );
        assert!(set_id(&initial, &json!({"guid": "mesh-guid", "id": "root"})).unwrap_err().contains("already used"));
        for bad in ["", "Param Shape", "Param.Shape", "ä", &"P".repeat(64)] {
            assert!(set_id(&initial, &json!({"guid": "mesh-guid", "id": bad})).unwrap_err().contains("ASCII"), "{bad}");
        }
        assert!(set_id(&initial, &json!({"guid": "nothing", "id": "Param"})).unwrap_err().contains("not found"));
    }

    #[test]
    fn a_brush_name_and_id_rewrite_only_their_tokens() {
        let brush = |id: &str, guid: &str, fields: &str| format!(
            "<CArtPathBrush xs.id='{id}'>{fields}<CArtPathBrushGuid xs.n='guid' uuid='{guid}'/>\
             <CArtPathBrush_TextureInput xs.n='textureInput'><CArtPathBrush xs.n='_owner' xs.ref='{id}'/>\
             </CArtPathBrush_TextureInput></CArtPathBrush>");
        let setting = format!(
            "<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
             <carray_list xs.n='brushes' count='2'>{}{}</carray_list></CArtPathBrushSetting>",
            brush("brush-a", "brush-a-guid", "<s xs.n='name'>Watercolour</s><CArtPathBrushId xs.n='id' idstr='ArtPathBrush'/>"),
            brush("brush-b", "brush-b-guid", "<CArtPathBrushId xs.n='id' idstr='ArtPathBrush2'/>"));
        let original = artmesh_xml().replace("<CPartSource xs.n='rootPart' xs.ref='part'/>", &setting);
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        assert_eq!(initial["brushes"][0], json!({"guid": "brush-a-guid", "id": "ArtPathBrush", "name": "Watercolour", "textureInput":{}}));

        let renamed = rename(&initial, &json!({"guid": "brush-a-guid", "name": "Pencil & <ink>"})).unwrap();
        let edited = set_id(&renamed, &json!({"guid": "brush-a-guid", "id": "BrushInk"})).unwrap();
        assert_eq!(edited["brushes"][0], json!({"guid": "brush-a-guid", "id": "BrushInk", "name": "Pencil & <ink>", "textureInput":{}}));
        let bytes = rewrite(&container, &initial, &edited).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), edited);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original
                .replace("<s xs.n='name'>Watercolour</s>", "<s xs.n=\"name\">Pencil &amp; &lt;ink&gt;</s>")
                .replace("idstr='ArtPathBrush'", "idstr='BrushInk'")
        );

        assert!(set_id(&initial, &json!({"guid": "brush-a-guid", "id": "ArtPathBrush2"})).unwrap_err().contains("already used"));
        let unnamed = rename(&initial, &json!({"guid": "brush-b-guid", "name": "Pencil"})).unwrap();
        assert!(rewrite(&container, &initial, &unnamed).unwrap_err().contains("no inline name field"));
    }

    #[test]
    fn a_blend_shape_form_is_rewritten_and_its_base_form_is_refused() {
        let original_xml = morph_artmesh_xml();
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        assert_eq!(initial["sources"][0]["morphTargets"]["targets"][0]["formGuid"], "shape-guid");
        assert_eq!(initial["sources"][0]["grid"]["cells"][0]["formGuid"], "form-guid");
        let shape = json!({"sourceGuid":"mesh-guid","formGuid":"shape-guid","coordinateType":"Canvas"});
        let with = |extra: Value| {
            let mut request = shape.clone();
            request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            request
        };
        let vertex = set_vertex(&initial, &with(json!({"vertexIndex":1,"position":[70.5,-8.25]}))).unwrap();
        let candidate = set_opacity(&vertex, &with(json!({"opacity":0.25}))).unwrap();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), candidate);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original_xml
                .replace("<f xs.n='opacity'>0.5</f>", "<f xs.n='opacity'>0.25</f>")
                .replace("1 2 7 8 5 6", "1 2 70.5 -8.25 5 6"),
            "only the two tokens of the shape form change"
        );

        // The base form carries every shape of the source, so it stays refused, in the edit and in the writer.
        let base = json!({"sourceGuid":"mesh-guid","formGuid":"form-guid","coordinateType":"Canvas",
            "vertexIndex":1,"position":[30.0,40.0]});
        assert!(set_vertex(&initial, &base).unwrap_err().contains("only a morph target form"));
        let mut forced = initial.clone();
        forced["sources"][0]["forms"][0]["data"]["positions"][2] = json!(30.0);
        assert!(rewrite(&container, &initial, &forced).unwrap_err().contains("only a morph target form"));

        // A shape opacity outside 0..=1 would stop every evaluation of the document.
        for opacity in [1.5, -0.25] {
            let error = set_opacity(&initial, &with(json!({"opacity":opacity}))).unwrap_err();
            assert!(error.contains("must be in 0..=1"), "{error}");
            let mut forced = initial.clone();
            forced["sources"][0]["forms"][1]["opacity"] = json!(opacity);
            assert!(rewrite(&container, &initial, &forced).unwrap_err().contains("must be in 0..=1"));
        }

        // A source with generated extended interpolation forms is refused for every form.
        let mut cached = initial.clone();
        cached["sources"][0]["extendedInterpolations"] = json!([{}]);
        let error = set_vertex(&cached, &with(json!({"vertexIndex":1,"position":[70.5,-8.25]}))).unwrap_err();
        assert!(error.contains("extended interpolation"), "{error}");
    }

    #[test]
    fn xml_token_patches_preserve_all_other_bytes_and_reject_encoded_values() {
        let text =
            "<root><float-array xs.n='positions' count='4'> 1.0\t2  3 4 </float-array></root>";
        let document = Document::parse(text).unwrap();
        let array = document
            .root_element()
            .children()
            .find(|node| node.is_element())
            .unwrap();
        let patches = position_token_patches(text, array, &[1.0, 2.0, -3.5, 4.0]).unwrap();
        let mut rewritten = text.to_owned();
        for patch in patches.into_iter().rev() {
            rewritten.replace_range(patch.range, &patch.value);
        }
        assert_eq!(
            rewritten,
            "<root><float-array xs.n='positions' count='4'> 1.0\t2  -3.5 4 </float-array></root>"
        );

        let encoded = "<root><float-array xs.n='positions' count='2'>&#49; 2</float-array></root>";
        let document = Document::parse(encoded).unwrap();
        let array = document
            .root_element()
            .children()
            .find(|node| node.is_element())
            .unwrap();
        assert!(position_token_patches(encoded, array, &[3.0, 2.0]).is_err());
    }

    #[test]
    fn opacity_token_patch_preserves_surrounding_xml_and_rejects_non_scalar_fields() {
        let text = "<root><f xs.n='opacity'> 0.75 \t</f></root>";
        let document = Document::parse(text).unwrap();
        let field = document
            .root_element()
            .children()
            .find(|node| node.is_element())
            .unwrap();
        let patch = opacity_token_patch(text, field, 0.75, 1.00005).unwrap();
        let mut rewritten = text.to_owned();
        rewritten.replace_range(patch.range, &patch.value);
        assert_eq!(rewritten, "<root><f xs.n='opacity'> 1.00005 \t</f></root>");

        for malformed in [
            "<root><f xs.n='opacity'>0.75 0.5</f></root>",
            "<root><f xs.n='opacity'>&#48;.75</f></root>",
            "<root><f xs.n='opacity'><s>0.75</s></f></root>",
        ] {
            let document = Document::parse(malformed).unwrap();
            let field = document
                .root_element()
                .children()
                .find(|node| node.is_element())
                .unwrap();
            assert!(opacity_token_patch(malformed, field, 0.75, 0.5).is_err());
        }
    }

    #[test]
    fn rewrite_roundtrips_scene_and_preserves_unknown_xml_and_caff_entries() {
        let original_xml = warp_xml();
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_vertex(
            &initial,
            &json!({"sourceGuid":"warp-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":1,"position":[30.25,-40.5]}),
        )
        .unwrap();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), candidate);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original_xml.replace("  3 4", "  30.25 -40.5")
        );
        assert_eq!(reopened.entry_data("opaque.bin"), Some(vec![0, 1, 2, 0xff]));
    }

    #[test]
    fn rewrite_mixes_artmesh_opacity_and_vertex_tokens_in_one_save() {
        let original_xml = artmesh_xml();
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        let vertex = set_vertex(
            &initial,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":1,"position":[30.25,-40.5]}),
        )
        .unwrap();
        let candidate = set_opacity(
            &vertex,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":1.00005}),
        )
        .unwrap();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), candidate);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original_xml
                .replace(
                    "<f xs.n='opacity'>0.75</f>",
                    "<f xs.n='opacity'>1.00005</f>"
                )
                .replace("1 2 3 4 5 6", "1 2 30.25 -40.5 5 6")
        );
        assert_eq!(
            reopened.entry_data("mesh.png"),
            Some(vec![0x89, b'P', b'N', b'G'])
        );
        assert_eq!(reopened.entry_data("opaque.bin"), Some(vec![0, 1, 2, 0xff]));
    }

    #[test]
    fn missing_opacity_stays_untouched_for_vertex_edits_and_cannot_be_inserted() {
        let original_xml = artmesh_xml().replace("<f xs.n='opacity'>0.75</f>", "");
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        assert_eq!(initial["sources"][0]["forms"][0]["opacity"], json!(1.0));
        let vertex = set_vertex(
            &initial,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":0,"position":[9,10]}),
        )
        .unwrap();
        let rewritten = rewrite(&container, &initial, &vertex).unwrap();
        assert_eq!(
            String::from_utf8(CaffDocument::parse(&rewritten).unwrap().main_xml()).unwrap(),
            original_xml.replace("1 2 3 4 5 6", "9 10 3 4 5 6")
        );
        let opacity = set_opacity(
            &initial,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":0.5}),
        )
        .unwrap();
        assert!(rewrite(&container, &initial, &opacity)
            .unwrap_err()
            .contains("explicit opacity"));
    }

    #[test]
    fn opacity_writer_rejects_inbound_aliases_and_referenced_field_paths() {
        let aliased = artmesh_xml().replace(
            "<f xs.n='opacity'>0.75</f>",
            "<f xs.n='opacity' xs.id='opacity-field'>0.75</f><Opaque xs.ref='opacity-field'/>",
        );
        let container = test_container(&aliased);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_opacity(
            &initial,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":0.5}),
        )
        .unwrap();
        assert!(rewrite(&container, &initial, &candidate)
            .unwrap_err()
            .contains("inbound xs.ref"));

        let referenced = artmesh_xml().replace(
            "<f xs.n='opacity'>0.75</f>",
            "<f xs.n='opacity' xs.ref='opacity-field'/><f xs.id='opacity-field'>0.75</f>",
        );
        let container = test_container(&referenced);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_opacity(
            &initial,
            &json!({"sourceGuid":"mesh-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","opacity":0.5}),
        )
        .unwrap();
        assert!(rewrite(&container, &initial, &candidate)
            .unwrap_err()
            .contains("xs.ref"));
    }

    #[test]
    fn rewrite_indexes_and_applies_multiple_form_edits() {
        let original_xml = warp_xml_with_second_form();
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        let first = set_vertex(
            &initial,
            &json!({"sourceGuid":"warp-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":1,"position":[30.25,-40.5]}),
        )
        .unwrap();
        let candidate = set_vertex(
            &first,
            &json!({"sourceGuid":"warp-guid","formGuid":"form-guid-2",
                "coordinateType":"DeformerLocal","vertexIndex":0,"position":[-70.25,80.5]}),
        )
        .unwrap();
        let rewritten = rewrite(&container, &initial, &candidate).unwrap();
        let reopened = CaffDocument::parse(&rewritten).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), candidate);
        assert_eq!(
            String::from_utf8(reopened.main_xml()).unwrap(),
            original_xml
                .replace("  3 4", "  30.25 -40.5")
                .replace("9 10", "-70.25 80.5")
        );
    }

    #[test]
    fn rewrite_rejects_an_inbound_alias_to_edited_positions() {
        let xml = warp_xml()
            .replace(
                "xs.n='positions' count='8'",
                "xs.n='positions' count='8' xs.id='shared-positions'",
            )
            .replace("</root>", "<Opaque xs.ref='shared-positions'/></root>");
        let container = test_container(&xml);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_vertex(
            &initial,
            &json!({"sourceGuid":"warp-guid","formGuid":"form-guid",
                "coordinateType":"Canvas","vertexIndex":0,"position":[9,10]}),
        )
        .unwrap();
        assert!(rewrite(&container, &initial, &candidate)
            .unwrap_err()
            .contains("inbound xs.ref"));
    }
}
