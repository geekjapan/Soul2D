//! Typed editing of the physics settings a CMO3 stores in `physicsSettingsSourceSet` (PHY01, issue #236).
//!
//! The canonical authoring model keeps the set element's verbatim XML in `physicsSource`. This module derives a
//! full-precision typed view from it and applies one typed edit per call, returning the candidate model for
//! `commit_model`. `rewrite` splices the stored string back at the element's byte range, so unknown markup and
//! full-precision numbers survive and the strict `read(reopened) == candidate` gate still holds. Groups have no
//! stable identity: callers address them by document order under `expectedRevision`. A new group is a copy of a
//! stored one (`duplicate`), or the first group can be constructed in an existing empty inline settings list.

use crate::authoring_graph::Graph;
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use crate::preset_store::Kind;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

const MAX_GROUPS: usize = 256;
const MAX_ITEMS_PER_LIST: usize = 1024;
const MAX_TOTAL_ITEMS: usize = 16384;
const MAX_NAME_BYTES: usize = 1024;
const MIN_STRAND_VERTICES: usize = 2;

struct Patch {
    range: Range<usize>,
    value: String,
}

/// What to check in the view of the document after an edit was spliced in.
enum Verify {
    /// A JSON path of object keys and array indices ending at the expected value.
    Value { path: Vec<Value>, expected: Value },
    Groups(usize),
    ListItems { group: usize, list: String, count: usize },
}

fn verify(after: &Value, check: &Verify) -> Result<(), String> {
    let actual = match check {
        Verify::Value { path, .. } => {
            let mut cursor = after;
            for step in path {
                cursor = match step {
                    Value::String(key) => &cursor[key.as_str()],
                    Value::Number(index) => &cursor[index.as_u64().unwrap_or(usize::MAX as u64) as usize],
                    _ => &Value::Null,
                };
            }
            cursor.clone()
        }
        Verify::Groups(count) => after["groups"]
            .as_array()
            .map_or(Value::Null, |groups| json!(groups.len() == *count)),
        Verify::ListItems { group, list, count } => after["groups"][*group][list.as_str()]
            .as_array()
            .map_or(Value::Null, |items| json!(items.len() == *count)),
    };
    let expected = match check {
        Verify::Value { expected, .. } => expected.clone(),
        _ => json!(true),
    };
    let same_number = actual.as_f64().zip(expected.as_f64()).is_some_and(|(a, b)| a == b);
    if actual == expected || same_number {
        Ok(())
    } else {
        Err(format!("authoring physics edit did not apply: expected {expected}, wrote {actual}"))
    }
}

fn parse(text: &str) -> Result<Document<'_>, String> {
    Document::parse_with_options(
        text,
        ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))
}

fn direct_field<'d, 'i>(node: Node<'d, 'i>, name: &str) -> Option<Node<'d, 'i>> {
    node.children().find(|child| child.is_element() && child.attribute("xs.n") == Some(name))
}

/// Every `xs.ref` target name in the document: the set of `xs.id`s some element depends on.
fn inbound_refs<'d>(document: &'d Document<'d>) -> HashSet<&'d str> {
    document
        .descendants()
        .filter(|node| node.is_element())
        .filter_map(|node| node.attribute("xs.ref"))
        .collect()
}

/// Whether an inline element's subtree carries no `xs.id` that another element references. Removing or replacing
/// such a subtree cannot dangle a reference.
fn subtree_unreferenced(node: Node<'_, '_>, inbound: &HashSet<&str>) -> bool {
    node.descendants()
        .filter(|node| node.is_element())
        .all(|node| node.attribute("xs.id").is_none_or(|id| !inbound.contains(id)))
}

/// Whether this element's own `xs.id` is the target of some `xs.ref` elsewhere: shared storage.
fn referenced(node: Node<'_, '_>, inbound: &HashSet<&str>) -> bool {
    node.attribute("xs.id").is_some_and(|id| inbound.contains(id))
}

fn elements<'d, 'i>(node: Node<'d, 'i>) -> Vec<Node<'d, 'i>> {
    node.children().filter(|child| child.is_element()).collect()
}

fn check_count(node: Node<'_, '_>, actual: usize) -> Result<(), String> {
    if let Some(count) = node.attribute("count") {
        if count.parse::<usize>().ok() != Some(actual) {
            return Err("authoring array count does not match content".into());
        }
    }
    Ok(())
}

/// A guid→id and id→guid map of the model's parameters, for translating `CParameterGuid` references.
fn parameter_maps(model: &Value) -> Result<(HashMap<String, String>, HashMap<String, String>), String> {
    let parameters = model
        .get("parameters")
        .and_then(Value::as_array)
        .ok_or("authoring model lacks parameters")?;
    let mut by_guid = HashMap::with_capacity(parameters.len());
    let mut by_id = HashMap::with_capacity(parameters.len());
    for parameter in parameters {
        let guid = parameter["guid"].as_str().ok_or("authoring parameter lacks guid")?;
        let id = parameter["id"].as_str().ok_or("authoring parameter lacks id")?;
        by_guid.insert(guid.to_owned(), id.to_owned());
        by_id.insert(id.to_owned(), guid.to_owned());
    }
    Ok((by_guid, by_id))
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text).replace('\'', "&apos;").replace('"', "&quot;")
}

fn number_text(value: f64) -> String {
    format!("{value}")
}

/// The open-tag span of an element and the span of its content, for patching `count` and insert points.
fn element_spans(text: &str, node: Node<'_, '_>) -> Result<(Range<usize>, Range<usize>), String> {
    let range = node.range();
    let element = &text[range.clone()];
    let open_len = element
        .find('>')
        .map(|index| index + 1)
        .ok_or("authoring element is truncated")?;
    if element[..open_len].ends_with("/>") {
        return Ok((range.start..range.start + open_len, range.end..range.end));
    }
    let close = element
        .rfind("</")
        .ok_or("authoring element lacks a closing tag")?;
    Ok((range.start..range.start + open_len, range.start + open_len..range.start + close))
}

/// The `count='N'` attribute inside a list's open tag, as a patch over the digits only.
fn count_patch(
    text: &str,
    list: Node<'_, '_>,
    new_count: usize,
    patches: &mut Vec<Patch>,
) -> Result<(), String> {
    let (open, _) = element_spans(text, list)?;
    let head = &text[open.clone()];
    let Some(attr) = head.find("count=") else {
        return Ok(());
    };
    let start = open.start + attr + "count=".len();
    let quote = text[start..]
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))
        .ok_or("authoring count attribute lacks a quote")?;
    let digits = start + 1;
    let end = text[digits..]
        .find(quote)
        .map(|end| digits + end)
        .ok_or("authoring count attribute is unterminated")?;
    if text[digits..end].parse::<usize>().is_err() {
        return Err("authoring count attribute is not a number".into());
    }
    patches.push(Patch { range: digits..end, value: new_count.to_string() });
    Ok(())
}

/// The set element when it is a direct inline field of the model: the only layout this writer supports.
fn editable_set<'d, 'i>(
    graph: &Graph<'d, 'i>,
    inbound: &HashSet<&str>,
) -> Result<Option<Node<'d, 'i>>, String> {
    let Some(model) = graph.model()? else {
        return Ok(None);
    };
    let Some(field) = direct_field(model, "physicsSettingsSourceSet") else {
        return Ok(None);
    };
    if field.attribute("xs.ref").is_some() || field.has_tag_name("null") {
        return Err("authoring physics source uses shared or absent storage".into());
    }
    if field.attribute("xs.id").is_some_and(|id| inbound.contains(id)) {
        return Err("authoring physics source is shared storage".into());
    }
    Ok(Some(field))
}

fn editable_list<'d, 'i>(
    parent: Node<'d, 'i>,
    name: &str,
    inbound: &HashSet<&str>,
) -> Result<Node<'d, 'i>, String> {
    let list = direct_field(parent, name)
        .ok_or_else(|| format!("authoring physics {name} list is missing"))?;
    if list.attribute("xs.ref").is_some() || referenced(list, inbound) {
        return Err(format!("authoring physics {name} list uses shared storage"));
    }
    Ok(list)
}

fn inline_slot<'d, 'i>(
    graph: &Graph<'d, 'i>,
    inbound: &HashSet<&str>,
    slots: &[Node<'d, 'i>],
    index: usize,
    what: &str,
) -> Result<Node<'d, 'i>, String> {
    let slot = *slots
        .get(index)
        .ok_or_else(|| format!("authoring physics {what} index is outside the list"))?;
    if slot.attribute("xs.ref").is_some() || referenced(slot, inbound) {
        return Err(format!("authoring physics {what} uses shared storage"));
    }
    let node = graph.resolve(slot)?;
    if referenced(node, inbound) {
        return Err(format!("authoring physics {what} uses shared storage"));
    }
    Ok(node)
}

/// Checks a leaf element is safe to replace whole: right tag and name, no alias, no child elements, no externally
/// referenced `xs.id`, and no attributes beyond `xs.n`, `xs.id` and the listed ones. Returns the `xs.id` attribute
/// fragment to carry over, if any.
fn check_leaf(
    node: Node<'_, '_>,
    tag: &str,
    name: &str,
    extra: &[&str],
    inbound: &HashSet<&str>,
) -> Result<String, String> {
    if !node.has_tag_name(tag) || node.attribute("xs.n") != Some(name) {
        return Err(format!("authoring physics {name} is not inline {tag} storage"));
    }
    if node.attribute("xs.ref").is_some() || node.children().any(|child| !child.is_text()) {
        return Err(format!("authoring physics {name} uses shared storage or carries non-text children"));
    }
    let id = node.attribute("xs.id");
    if id.is_some_and(|id| inbound.contains(id)) {
        return Err(format!("authoring physics {name} is referenced elsewhere"));
    }
    for attribute in node.attributes() {
        let attribute = attribute.name();
        if attribute != "xs.n" && attribute != "xs.id" && !extra.contains(&attribute) {
            return Err(format!("authoring physics {name} carries an unknown attribute"));
        }
    }
    Ok(id.map(|id| format!(" xs.id='{}'", escape_attribute(id))).unwrap_or_default())
}

fn scalar_patch(
    node: Node<'_, '_>,
    tag: &str,
    name: &str,
    content: String,
    inbound: &HashSet<&str>,
) -> Result<Patch, String> {
    let id = check_leaf(node, tag, name, &[], inbound)?;
    Ok(Patch {
        range: node.range(),
        value: format!("<{tag} xs.n='{name}'{id}>{}</{tag}>", escape_text(&content)),
    })
}

fn attribute_leaf_patch(
    node: Node<'_, '_>,
    tag: &str,
    name: &str,
    attribute: &str,
    value: &str,
    inbound: &HashSet<&str>,
) -> Result<Patch, String> {
    let id = check_leaf(node, tag, name, &[attribute], inbound)?;
    if node
        .children()
        .any(|child| child.is_text() && !child.text().unwrap_or_default().trim().is_empty())
    {
        return Err(format!("authoring physics {name} carries non-whitespace text"));
    }
    Ok(Patch {
        range: node.range(),
        value: format!("<{tag} xs.n='{name}'{id} {attribute}='{}'/>", escape_attribute(value)),
    })
}

/// The attribute that names a parameter GUID: a reference to the file's shared GUID object when it keeps one.
fn parameter_reference(guid: &str, guid_objects: &HashMap<&str, &str>) -> String {
    match guid_objects.get(guid) {
        Some(id) => format!("xs.ref='{}'", escape_attribute(id)),
        None => format!("uuid='{}'", escape_attribute(guid)),
    }
}

/// Points a `CParameterGuid` field at another parameter, as an inline GUID or a reference.
fn parameter_leaf_patch(
    node: Node<'_, '_>,
    name: &str,
    guid: &str,
    guid_objects: &HashMap<&str, &str>,
    inbound: &HashSet<&str>,
) -> Result<Patch, String> {
    if !node.has_tag_name("CParameterGuid") || node.attribute("xs.n") != Some(name) {
        return Err(format!("authoring physics {name} is not a CParameterGuid field"));
    }
    if node.attribute("xs.id").is_some() || referenced(node, inbound) {
        return Err(format!("authoring physics {name} defines a shared GUID"));
    }
    if node.children().any(|child| child.is_element()) {
        return Err(format!("authoring physics {name} uses shared storage or carries non-text children"));
    }
    if node.children().any(|child| child.text().is_some_and(|text| !text.trim().is_empty())) {
        return Err(format!("authoring physics {name} carries non-whitespace text"));
    }
    if node.attributes().any(|attribute| !matches!(attribute.name(), "xs.n" | "uuid" | "xs.ref" | "note")) {
        return Err(format!("authoring physics {name} carries an unknown attribute"));
    }
    Ok(Patch {
        range: node.range(),
        value: format!("<CParameterGuid xs.n='{name}' {}/>", parameter_reference(guid, guid_objects)),
    })
}

/// A new list item written as a copy of `donor`, a stored item of the same list, with the requested values (and the
/// defaults of `item_insert`) and new GUIDs. Fields the view does not show keep the donor's values.
#[allow(clippy::too_many_arguments)]
fn cloned_item(
    text: &str,
    inbound: &HashSet<&str>,
    by_id: &HashMap<String, String>,
    guid_objects: &HashMap<&str, &str>,
    group: Node<'_, '_>,
    list: &str,
    donor: Node<'_, '_>,
    value: &Value,
) -> Result<String, String> {
    let mut values = value.as_object().cloned().unwrap_or_default();
    if list != "vertices" {
        for (key, default) in [("weight", json!(100.0)), ("scale", json!(10.0)), ("reflect", json!(false))] {
            values.entry(key).or_insert(default);
        }
    }
    let mut patches = Vec::new();
    for (field, value) in &values {
        item_scalar(inbound, by_id, guid_objects, group, list, donor, field, value, &mut patches)?;
    }
    for node in donor.descendants().filter(|node| node.attribute("xs.n") == Some("guid")) {
        if let Some(attribute) = node.attributes().find(|attribute| attribute.name() == "uuid") {
            let guid = crate::animation_source::new_guid()?;
            patches.push(Patch { range: attribute.range_value(), value: guid });
        }
    }
    let range = donor.range();
    let mut out = text[range.clone()].to_owned();
    patches.sort_by_key(|patch| patch.range.start);
    if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
        return Err("authoring physics item copy produced overlapping changes".into());
    }
    for patch in patches.iter().rev() {
        out.replace_range(patch.range.start - range.start..patch.range.end - range.start, &patch.value);
    }
    Ok(out)
}

/// Replaces an element whose tag itself changes (`<i>` ↔ `<null>`), keeping the same safety checks.
fn replace_leaf(
    node: Node<'_, '_>,
    name: &str,
    new_text: String,
    inbound: &HashSet<&str>,
) -> Result<Patch, String> {
    if node.attribute("xs.ref").is_some() || node.children().any(|child| !child.is_text()) {
        return Err(format!("authoring physics {name} uses shared storage or carries non-text children"));
    }
    if node
        .attribute("xs.id")
        .is_some_and(|id| inbound.contains(id))
    {
        return Err(format!("authoring physics {name} is referenced elsewhere"));
    }
    if node
        .attributes()
        .any(|attribute| !matches!(attribute.name(), "xs.n" | "xs.id"))
    {
        return Err(format!("authoring physics {name} carries an unknown attribute"));
    }
    Ok(Patch { range: node.range(), value: new_text })
}

fn finite(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| "authoring physics value must be a finite number".to_owned())
}

fn native_finite(value: &Value, name: &str) -> Result<f64, String> {
    let value = finite(value)?;
    if !(value as f32).is_finite() {
        return Err(format!("authoring physics {name} exceeds native float range"));
    }
    Ok(value)
}

fn request_index(object: &Map<String, Value>, name: &str) -> Result<Option<usize>, String> {
    object
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| format!("{name} must be a non-negative integer"))
        })
        .transpose()
}

fn request_string<'a>(object: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, String> {
    object
        .get(name)
        .map(|value| value.as_str().ok_or_else(|| format!("{name} must be a string")))
        .transpose()
}

fn optional_number(object: &Map<String, Value>, name: &str, default: f64) -> Result<f64, String> {
    match object.get(name) {
        Some(value) => native_finite(value, name),
        None => Ok(default),
    }
}

fn optional_bool(object: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    match object.get(name) {
        Some(value) => value.as_bool().ok_or_else(|| format!("{name} must be a boolean")),
        None => Ok(default),
    }
}

/// `live2d.physics3.read`: the full-precision typed view of the stored physics source.
pub(crate) fn view(container: &CaffDocument, model: &Value) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let document = parse(text)?;
    let graph = Graph::new(&document)?;
    let (by_guid, _) = parameter_maps(model)?;
    let inbound = inbound_refs(&document);
    view_document(&graph, &inbound, &by_guid)
}

fn view_document(
    graph: &Graph<'_, '_>,
    inbound: &HashSet<&str>,
    by_guid: &HashMap<String, String>,
) -> Result<Value, String> {
    let Some(model) = graph.model()? else {
        return Err("authoring source contains no model".into());
    };
    let Some(set) = graph.field(model, "physicsSettingsSourceSet")? else {
        return Ok(json!({"format": "live2d.authoring.physics.v1", "present": false}));
    };
    if set.has_tag_name("null") {
        return Ok(json!({"format": "live2d.authoring.physics.v1", "present": false}));
    }
    let direct = direct_field(model, "physicsSettingsSourceSet")
        .filter(|field| field.id() == set.id() && field.attribute("xs.ref").is_none());
    let set_editable = direct
        .is_some_and(|field| field.attribute("xs.id").is_none_or(|id| !inbound.contains(id)));
    let mut budget = Budget::new(5_000_000);
    let fps = graph
        .field(set, "settingFPS")?
        .filter(|rate| !rate.has_tag_name("null"))
        .map(|rate| {
            rate.text()
                .unwrap_or("")
                .trim()
                .parse::<u32>()
                .map_err(|_| "invalid authoring physics rate".to_owned())
        })
        .transpose()?;
    let Some(list) = graph.field(set, "_sourceCubismPhysics")? else {
        return Ok(json!({"format": "live2d.authoring.physics.v1", "present": true,
            "editable": false, "fps": fps, "groups": []}));
    };
    let list = graph.resolve(list)?;
    let slots = elements(list);
    check_count(list, slots.len())?;
    if slots.len() > MAX_GROUPS {
        return Err("authoring physics exceeds 256 settings".into());
    }
    let list_inline = list.has_tag_name("carray_list") && direct_field(set, "_sourceCubismPhysics")
        .is_some_and(|field| {
            field.id() == list.id() && field.attribute("xs.ref").is_none()
        })
        && !referenced(list, inbound);
    let mut groups = Vec::with_capacity(slots.len());
    let mut total = 0usize;
    for (index, slot) in slots.iter().enumerate() {
        let group = graph.resolve(*slot)?;
        let shared = slot.attribute("xs.ref").is_some()
            || referenced(*slot, inbound)
            || referenced(group, inbound);
        let editable = set_editable && list_inline && !shared;
        let items = |name: &str,
                     kind: &str,
                     shared: bool,
                     editable: bool,
                     budget: &mut Budget,
                     out: &mut Vec<Value>|
         -> Result<(), String> {
            let Some(field) = graph.field(group, name)? else {
                return Ok(());
            };
            let list = graph.resolve(field)?;
            let item_slots = elements(list);
            check_count(list, item_slots.len())?;
            if item_slots.len() > MAX_ITEMS_PER_LIST {
                return Err("authoring physics list exceeds 1024 entries".into());
            }
            let shared = shared
                || field.attribute("xs.ref").is_some()
                || referenced(field, inbound)
                || referenced(list, inbound);
            for (item_index, item_slot) in item_slots.iter().enumerate() {
                out.push(view_item(
                    graph, inbound, by_guid, *item_slot, item_index, kind, shared,
                    editable && !shared, budget,
                )?);
            }
            Ok(())
        };
        let (mut inputs, mut outputs, mut vertices) = (Vec::new(), Vec::new(), Vec::new());
        items("inputs", "inputs", shared, editable, &mut budget, &mut inputs)?;
        items("outputs", "outputs", shared, editable, &mut budget, &mut outputs)?;
        items("vertices", "vertices", shared, editable, &mut budget, &mut vertices)?;
        total += inputs.len() + outputs.len() + vertices.len();
        if inputs.len() > MAX_ITEMS_PER_LIST
            || outputs.len() > MAX_ITEMS_PER_LIST
            || vertices.len() > MAX_ITEMS_PER_LIST
            || total > MAX_TOTAL_ITEMS
        {
            return Err("authoring physics exceeds entry budget".into());
        }
        let range = |name: &str| -> Result<Value, String> {
            let field = |part: &str| values::number_field(graph, group, &format!("normalized{name}{part}"), None);
            Ok(json!({"min": field("ValueMin")?, "default": field("DefaultValue")?,
                "max": field("ValueMax")?}))
        };
        let name = values::text_field(graph, group, "name")?.unwrap_or("");
        groups.push(json!({
            "index": index,
            "name": budget.text(name)?,
            "editable": editable,
            "shared": shared,
            "normalization": {"position": range("Position")?, "angle": range("Angle")?},
            "inputs": inputs,
            "outputs": outputs,
            "vertices": vertices,
        }));
    }
    Ok(json!({"format": "live2d.authoring.physics.v1", "present": true,
        "editable": set_editable && list_inline, "fps": fps, "groups": groups}))
}

fn view_item(
    graph: &Graph<'_, '_>,
    inbound: &HashSet<&str>,
    by_guid: &HashMap<String, String>,
    slot: Node<'_, '_>,
    index: usize,
    list: &str,
    container_shared: bool,
    editable_path: bool,
    budget: &mut Budget,
) -> Result<Value, String> {
    let item = graph.resolve(slot)?;
    let shared = slot.attribute("xs.ref").is_some()
        || referenced(slot, inbound)
        || container_shared
        || !subtree_unreferenced(item, inbound);
    let mut entry = Map::new();
    entry.insert("index".into(), json!(index));
    entry.insert("shared".into(), json!(shared));
    entry.insert("editable".into(), json!(editable_path && !shared));
    let number = |name: &str| -> Result<Value, String> {
        match values::text_field(graph, item, name)? {
            Some(text) => Ok(json!(values::finite(text)?)),
            None => Ok(Value::Null),
        }
    };
    let parameter = |name: &str, budget: &mut Budget| -> Result<(Value, Value), String> {
        let Some(guid) = values::guid_field(graph, item, name)? else {
            return Ok((Value::Null, Value::Null));
        };
        let id = match by_guid.get(guid) {
            Some(id) => budget.text(id)?,
            None => String::new(),
        };
        Ok((json!(id), json!(budget.text(guid)?)))
    };
    match list {
        "inputs" | "outputs" => {
            let field = if list == "inputs" { "source" } else { "destination" };
            let (id, guid) = parameter(field, budget)?;
            entry.insert(field.into(), id);
            entry.insert(format!("{field}Guid"), guid);
            let token = graph
                .field(item, "type")?
                .and_then(|node| node.attribute("v"))
                .unwrap_or("");
            let token = budget.text(token)?;
            let known = matches!(token.as_str(), "SRC_TO_X" | "SRC_TO_G_ANGLE");
            entry.insert("typeToken".into(), json!(token));
            entry.insert(
                "type".into(),
                match token.as_str() {
                    "SRC_TO_X" => json!("X"),
                    "SRC_TO_G_ANGLE" => json!("Angle"),
                    _ => Value::Null,
                },
            );
            entry.insert("weight".into(), number("weight")?);
            entry.insert("reflect".into(), json!(values::bool_field(graph, item, "isReverse", false)?));
            if let Some(vertex) = values::text_field(graph, item, "vertexIndex")? {
                entry.insert(
                    "vertexIndex".into(),
                    vertex.trim().parse::<u64>().map_or(Value::Null, Value::from),
                );
            }
            if let Some(scale) = values::text_field(graph, item, "angleScale")? {
                entry.insert("scale".into(), json!(values::finite(scale)?));
            }
            if !known || (list == "outputs" && token != "SRC_TO_G_ANGLE") {
                entry.insert("editable".into(), json!(false));
            }
        }
        "vertices" => {
            let position = graph.field(item, "position")?;
            let axis = |name: &str| -> Result<Value, String> {
                let Some(position) = position else {
                    return Ok(Value::Null);
                };
                match values::text_field(graph, position, name)? {
                    Some(text) => Ok(json!(values::finite(text)?)),
                    None => Ok(Value::Null),
                }
            };
            entry.insert("x".into(), axis("x")?);
            entry.insert("y".into(), axis("y")?);
            for name in ["mobility", "delay", "acceleration", "radius"] {
                entry.insert(name.into(), number(name)?);
            }
        }
        _ => return Err("unknown physics list".into()),
    }
    Ok(Value::Object(entry))
}

/// `live2d.physics3.edit` on the document side: validates one typed edit and returns the candidate model whose
/// `physicsSource` holds the patched set text. `rewrite` splices it back.
pub(crate) fn edit(
    container: &CaffDocument,
    model: &Value,
    request: &Value,
) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    edit_text(text, model, request).map(|(candidate, _)| candidate)
}

/// One typed edit of the document text `text`: the candidate model and the whole document text with the edit
/// spliced in, which a following edit takes as its `text`.
fn edit_text(text: &str, model: &Value, request: &Value) -> Result<(Value, String), String> {
    if model.get("format").and_then(Value::as_str) != Some("live2d.authoring.source.v1") {
        return Err("authoring physics edit requires a source.v1 model".into());
    }
    let object = request
        .as_object()
        .ok_or("authoring physics edit request must be an object")?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "op" | "expectedRevision" | "group" | "list" | "index" | "insert" | "remove" | "field" | "value" | "duplicate" | "detach"
        ) {
            return Err(format!("authoring physics edit carries an unknown key {key:?}"));
        }
    }
    if let Some(detach) = object.get("detach") {
        if detach != &json!(true) || object.keys().any(|key| !matches!(key.as_str(), "op" | "expectedRevision" | "detach")) {
            return Err("authoring physics detach requires detach:true without another edit".into());
        }
        return detach_text(text, model);
    }
    let group_index = request_index(object, "group")?;
    let list_name = request_string(object, "list")?;
    let item_index = request_index(object, "index")?;
    let insert_at = request_index(object, "insert")?;
    let remove = optional_bool(object, "remove", false)?;
    let duplicate = optional_bool(object, "duplicate", false)?;
    let field = request_string(object, "field")?;
    let value = object.get("value");
    if insert_at.is_some() && group_index.is_some() && list_name.is_none() {
        return Err("authoring physics insert requires a list".into());
    }

    let document = parse(text)?;
    let graph = Graph::new(&document)?;
    let inbound = inbound_refs(&document);
    let (by_guid, by_id) = parameter_maps(model)?;
    // Cubism files keep a parameter's GUID as a shared object and refer to it; others store the GUID inline.
    let guid_objects: HashMap<&str, &str> = document.descendants()
        .filter(|node| node.parent_element().is_some_and(|p| p.has_tag_name("shared") || p.parent_element().is_none()))
        .filter_map(|node| Some((node.attribute("uuid")?, node.attribute("xs.id")?))).collect();
    let set = editable_set(&graph, &inbound)?
        .ok_or("authoring model has no physics settings")?;
    let list = editable_list(set, "_sourceCubismPhysics", &inbound)?;
    let slots = elements(list);
    check_count(list, slots.len())?;
    if slots.len() > MAX_GROUPS {
        return Err("authoring physics exceeds 256 settings".into());
    }

    let mut patches = Vec::new();
    let check: Verify = match (group_index, list_name, item_index, insert_at, remove, field, value) {
        (None, None, None, Some(0), false, None, Some(value)) if !duplicate => {
            if !list.has_tag_name("carray_list") {
                return Err("donor-free physics creation requires an inline settings list".into());
            }
            if !slots.is_empty() {
                return Err("donor-free physics creation requires an empty settings list".into());
            }
            let used: HashSet<_> = document.descendants().filter_map(|node| node.attribute("idstr")).collect();
            let id = (1..=used.len() + 1).map(|number| format!("PhysicsSetting{number}"))
                .find(|id| !used.contains(id.as_str())).ok_or("no free physics setting ID")?;
            let element = first_group(&by_id, &guid_objects, &id, value)?;
            if item_insert_offset(text, list, &slots, 0, &mut patches, &element)? {
                count_patch(text, list, 1, &mut patches)?;
            }
            Verify::Value {
                path: vec![json!("groups"), json!(0), json!("name")],
                expected: value["name"].clone(),
            }
        }
        // A new group: a copy of one the file stores, at the end, with new GUIDs and the next free
        // `PhysicsSetting<n>` ID.
        (Some(group), None, None, None, false, None, None) if duplicate => {
            let node = inline_slot(&graph, &inbound, &slots, group, "group")?;
            if node.descendants().any(|inner| inner.attribute("xs.id").is_some()) {
                return Err("authoring physics group defines shared objects".into());
            }
            if slots.len() + 1 > MAX_GROUPS {
                return Err("authoring physics exceeds 256 settings".into());
            }
            let ids: Vec<u64> = slots.iter().filter_map(|slot| direct_field(*slot, "id")?.attribute("idstr")?
                .strip_prefix("PhysicsSetting")?.parse().ok()).collect();
            let id = format!("PhysicsSetting{}", ids.iter().max().copied().unwrap_or(slots.len() as u64) + 1);
            let id_field = direct_field(node, "id").ok_or("authoring physics group lacks an id field")?;
            let mut local = Vec::new();
            let idstr = id_field.attributes().find(|attribute| attribute.name() == "idstr")
                .ok_or("authoring physics group id lacks idstr")?;
            local.push(Patch { range: idstr.range_value(), value: id });
            for inner in node.descendants().filter(|inner| inner.attribute("xs.n") == Some("guid")) {
                if let Some(attribute) = inner.attributes().find(|attribute| attribute.name() == "uuid") {
                    local.push(Patch { range: attribute.range_value(), value: crate::animation_source::new_guid()? });
                }
            }
            let range = node.range();
            let mut copy = text[range.clone()].to_owned();
            local.sort_by_key(|patch| patch.range.start);
            for patch in local.iter().rev() {
                copy.replace_range(patch.range.start - range.start..patch.range.end - range.start, &patch.value);
            }
            let end = slots.last().map_or(range.end, |last| last.range().end);
            patches.push(Patch { range: end..end, value: copy });
            count_patch(text, list, slots.len() + 1, &mut patches)?;
            Verify::Groups(slots.len() + 1)
        }
        (Some(group), None, None, None, false, Some(field), Some(value)) => {
            let node = inline_slot(&graph, &inbound, &slots, group, "group")?;
            match field {
                "name" => {
                    let name = value.as_str().ok_or("name must be a string")?;
                    if name.len() > MAX_NAME_BYTES {
                        return Err("authoring physics name exceeds 1024 bytes".into());
                    }
                    let target = direct_field(node, "name")
                        .ok_or("authoring physics group lacks an inline name field")?;
                    patches.push(scalar_patch(target, "s", "name", name.to_owned(), &inbound)?);
                    Verify::Value {
                        path: vec![json!("groups"), json!(group), json!("name")],
                        expected: json!(name),
                    }
                }
                _ => {
                    let (xml_name, kind) = match field {
                        "normalization.position.min" => ("normalizedPositionValueMin", "normalizedPosition"),
                        "normalization.position.default" => ("normalizedPositionDefaultValue", "normalizedPosition"),
                        "normalization.position.max" => ("normalizedPositionValueMax", "normalizedPosition"),
                        "normalization.angle.min" => ("normalizedAngleValueMin", "normalizedAngle"),
                        "normalization.angle.default" => ("normalizedAngleDefaultValue", "normalizedAngle"),
                        "normalization.angle.max" => ("normalizedAngleValueMax", "normalizedAngle"),
                        _ => return Err("unsupported authoring physics group field".into()),
                    };
                    let new = native_finite(value, field)?;
                    let read = |part: &str| values::number_field(&graph, node, &format!("{kind}{part}"), None);
                    let (min, default, max) = match xml_name {
                        name if name.ends_with("ValueMin") => (new, read("DefaultValue")?, read("ValueMax")?),
                        name if name.ends_with("DefaultValue") => (read("ValueMin")?, new, read("ValueMax")?),
                        _ => (read("ValueMin")?, read("DefaultValue")?, new),
                    };
                    if !(min <= default && default <= max) {
                        return Err("authoring physics normalization must keep min <= default <= max".into());
                    }
                    let target = direct_field(node, xml_name)
                        .ok_or("authoring physics group lacks an inline normalization field")?;
                    patches.push(scalar_patch(target, "f", xml_name, number_text(new), &inbound)?);
                    let mut path = vec![json!("groups"), json!(group), json!("normalization")];
                    for part in field["normalization.".len()..].split('.') {
                        path.push(json!(part));
                    }
                    Verify::Value { path, expected: json!(new) }
                }
            }
        }
        (Some(group), None, None, None, true, None, None) => {
            let slot = *slots
                .get(group)
                .ok_or("authoring physics group index is outside the list")?;
            if slot.attribute("xs.ref").is_some() || referenced(slot, &inbound) {
                return Err("authoring physics group uses shared storage".into());
            }
            if !subtree_unreferenced(slot, &inbound) {
                return Err("authoring physics group is referenced elsewhere".into());
            }
            patches.push(Patch { range: slot.range(), value: String::new() });
            count_patch(text, list, slots.len() - 1, &mut patches)?;
            Verify::Groups(slots.len() - 1)
        }
        // Set-level `settingFPS`: an integer rate or null for the vendor's unset value.
        (None, None, None, None, false, Some("fps"), Some(value)) => {
            let (tag, content) = if value.is_null() {
                ("null", String::new())
            } else {
                let fps = value
                    .as_u64()
                    .filter(|fps| (1..=240).contains(fps))
                    .ok_or("fps must be null or an integer in 1..=240")?;
                ("i", fps.to_string())
            };
            match direct_field(set, "settingFPS") {
                Some(node) => {
                    let id = node
                        .attribute("xs.id")
                        .filter(|id| !inbound.contains(id))
                        .map(|id| format!(" xs.id='{}'", escape_attribute(id)))
                        .unwrap_or_default();
                    let text = if content.is_empty() {
                        format!("<null xs.n='settingFPS'{id}/>")
                    } else {
                        format!("<{tag} xs.n='settingFPS'{id}>{content}</{tag}>")
                    };
                    patches.push(replace_leaf(node, "settingFPS", text, &inbound)?);
                }
                None => {
                    let (_, inner) = element_spans(text, set)?;
                    if inner.is_empty() {
                        return Err("authoring physics set is self-closing".into());
                    }
                    patches.push(Patch {
                        range: inner.end..inner.end,
                        value: format!("<{tag} xs.n='settingFPS'>{content}</{tag}>"),
                    });
                }
            }
            Verify::Value { path: vec![json!("fps")], expected: value.clone() }
        }
        (None, None, None, None, false, Some(_), Some(_)) => {
            return Err("unsupported authoring physics set field".into());
        }
        (None, _, _, _, _, _, _) => return Err("authoring physics edit lacks a group".into()),
        (Some(group), Some(list_name), index, insert_at, remove, field, value) => {
            let group_slot = *slots
                .get(group)
                .ok_or("authoring physics group index is outside the list")?;
            if group_slot.attribute("xs.ref").is_some() {
                return Err("authoring physics group uses shared storage".into());
            }
            let group_node = inline_slot(&graph, &inbound, &slots, group, "group")?;
            if !matches!(list_name, "inputs" | "outputs" | "vertices") {
                return Err("unsupported authoring physics list".into());
            }
            let list_node = editable_list(group_node, list_name, &inbound)?;
            let items = elements(list_node);
            check_count(list_node, items.len())?;
            if items.len() > MAX_ITEMS_PER_LIST {
                return Err("authoring physics list exceeds 1024 entries".into());
            }
            match (index, insert_at, remove, field, value) {
                (Some(index), None, false, Some(field), Some(value)) => {
                    let item = inline_slot(&graph, &inbound, &items, index, "list item")?;
                    let key = item_scalar(
                        &inbound, &by_id, &guid_objects, group_node, list_name, item, field, value, &mut patches,
                    )?;
                    Verify::Value {
                        path: vec![
                            json!("groups"),
                            json!(group),
                            json!(list_name),
                            json!(index),
                            json!(key),
                        ],
                        expected: value.clone(),
                    }
                }
                (Some(index), None, true, None, None) => {
                    item_remove(
                        &graph, &inbound, text, group_node, list_name, list_node, &items, index, &mut patches,
                    )?;
                    Verify::ListItems {
                        group,
                        list: list_name.to_owned(),
                        count: items.len() - 1,
                    }
                }
                (None, Some(insert), false, None, Some(value)) => {
                    if insert > items.len() {
                        return Err("authoring physics insert index is outside the list".into());
                    }
                    if items.len() + 1 > MAX_ITEMS_PER_LIST {
                        return Err("authoring physics list exceeds 1024 entries".into());
                    }
                    if list_name == "vertices" && insert == 0 && !items.is_empty() {
                        return Err("authoring physics vertex insert may not displace the strand root".into());
                    }
                    let vertex_count = if list_name == "outputs" {
                        elements(editable_list(group_node, "vertices", &inbound)?).len()
                    } else {
                        0
                    };
                    let element = item_insert(&by_id, &guid_objects, list_name, vertex_count, insert, value)?;
                    // A stored item of the same list is the layout to follow, when the file has one.
                    let donor = slots.iter().filter_map(|slot| {
                        let group = slot.attribute("xs.ref").is_none().then_some(*slot)?;
                        let list = editable_list(group, list_name, &inbound).ok()?;
                        elements(list).into_iter().find(|item| {
                            item.attribute("xs.ref").is_none() && item.descendants().all(|node| node.attribute("xs.id").is_none())
                        })
                    }).next();
                    let element = match donor {
                        Some(donor) => cloned_item(text, &inbound, &by_id, &guid_objects, group_node, list_name, donor, value)?,
                        None => element,
                    };
                    if item_insert_offset(text, list_node, &items, insert, &mut patches, &element)? {
                        count_patch(text, list_node, items.len() + 1, &mut patches)?;
                    }
                    if list_name == "vertices" {
                        renumber_outputs(&graph, &inbound, group_node, insert, 1, &mut patches)?;
                    }
                    Verify::ListItems {
                        group,
                        list: list_name.to_owned(),
                        count: items.len() + 1,
                    }
                }
                _ => return Err("unsupported authoring physics edit".into()),
            }
        }
        _ => return Err("unsupported authoring physics edit".into()),
    };

    let range = set.range();
    let mut body = text[range.clone()].to_owned();
    patches.sort_by_key(|patch| patch.range.start);
    let mut cursor = range.start;
    for patch in &patches {
        if patch.range.start < range.start || patch.range.end > range.end || patch.range.start < cursor {
            return Err("authoring physics edit produced overlapping changes".into());
        }
        cursor = patch.range.end;
    }
    for patch in patches.iter().rev() {
        body.replace_range(patch.range.start - range.start..patch.range.end - range.start, &patch.value);
    }

    // Prove the splice did what the request asked before the candidate leaves: reparse the document with the new
    // set text in place and check the addressed value in the same view the read tool returns.
    let spliced = format!("{}{}{}", &text[..range.start], body, &text[range.end..]);
    {
        let document = parse(&spliced)?;
        let graph = Graph::new(&document)?;
        let inbound = inbound_refs(&document);
        let after = view_document(&graph, &inbound, &by_guid)?;
        verify(&after, &check)?;
    }

    let mut candidate = model.clone();
    candidate["physicsSource"] = Value::String(body);
    Ok((candidate, spliced))
}

/// The view key a scalar field maps to, for the post-edit check.
fn json_key(list: &str, field: &str) -> String {
    match (list, field) {
        ("inputs" | "outputs", "reflect") => "reflect",
        ("outputs", "scale") => "scale",
        ("outputs", "vertexIndex") => "vertexIndex",
        _ => field,
    }
    .to_owned()
}

/// One scalar edit inside an `inputs`, `outputs` or `vertices` item. Returns the view key of the edited field.
fn item_scalar(
    inbound: &HashSet<&str>,
    by_id: &HashMap<String, String>,
    guid_objects: &HashMap<&str, &str>,
    group: Node<'_, '_>,
    list: &str,
    item: Node<'_, '_>,
    field: &str,
    value: &Value,
    patches: &mut Vec<Patch>,
) -> Result<String, String> {
    let target = |name: &str| -> Result<Node<'_, '_>, String> {
        direct_field(item, name)
            .ok_or_else(|| format!("authoring physics item lacks an inline {name} field"))
    };
    match (list, field) {
        ("inputs", "source") | ("outputs", "destination") => {
            let id = value.as_str().ok_or("physics parameter reference must be an ID string")?;
            let guid = by_id
                .get(id)
                .ok_or_else(|| format!("authoring physics names unknown parameter {id:?}"))?;
            patches.push(parameter_leaf_patch(target(field)?, field, guid, guid_objects, inbound)?);
        }
        ("inputs", "type") => {
            let token = match value.as_str() {
                Some("X") => "SRC_TO_X",
                Some("Angle") => "SRC_TO_G_ANGLE",
                _ => return Err("authoring physics input type must be \"X\" or \"Angle\"".into()),
            };
            patches.push(attribute_leaf_patch(target("type")?, "CPhysicsSourceType", "type", "v", token, inbound)?);
        }
        ("outputs", "type") => {
            return Err("authoring physics output type is fixed to Angle".into());
        }
        ("inputs" | "outputs", "weight") => {
            let weight = native_finite(value, "weight")?;
            if !(0.0..=100.0).contains(&weight) {
                return Err("authoring physics weight is outside 0..=100".into());
            }
            patches.push(scalar_patch(target("weight")?, "f", "weight", number_text(weight), inbound)?);
        }
        ("inputs" | "outputs", "reflect") => {
            let reflect = value.as_bool().ok_or("reflect must be a boolean")?;
            patches.push(scalar_patch(
                target("isReverse")?, "b", "isReverse", reflect.to_string(), inbound,
            )?);
        }
        ("inputs" | "outputs", "scale") => {
            let scale = native_finite(value, "scale")?;
            patches.push(scalar_patch(target("angleScale")?, "f", "angleScale", number_text(scale), inbound)?);
        }
        ("outputs", "vertexIndex") => {
            let vertex = value
                .as_u64()
                .filter(|vertex| *vertex >= 1)
                .ok_or("authoring physics vertexIndex must be a positive integer")?;
            let vertices = editable_list(group, "vertices", inbound)?;
            if vertex as usize >= elements(vertices).len() {
                return Err("authoring physics vertexIndex is outside the strand".into());
            }
            patches.push(scalar_patch(target("vertexIndex")?, "i", "vertexIndex", vertex.to_string(), inbound)?);
        }
        ("vertices", "x" | "y") => {
            let position = target("position")?;
            if !position.has_tag_name("GVector2")
                || position.attribute("xs.ref").is_some()
                || referenced(position, inbound)
            {
                return Err("authoring physics vertex position is not inline GVector2 storage".into());
            }
            let axis = direct_field(position, field)
                .ok_or_else(|| format!("authoring physics vertex lacks an inline {field} field"))?;
            let value = native_finite(value, field)?;
            patches.push(scalar_patch(axis, "f", field, number_text(value), inbound)?);
        }
        ("vertices", "mobility") => {
            let value = native_finite(value, "mobility")?;
            if !(0.0..=1.0).contains(&value) {
                return Err("authoring physics mobility is outside 0..=1".into());
            }
            patches.push(scalar_patch(target("mobility")?, "f", "mobility", number_text(value), inbound)?);
        }
        ("vertices", "delay" | "acceleration" | "radius") => {
            let number = native_finite(value, field)?;
            if number < 0.0 {
                return Err(format!("authoring physics {field} must be nonnegative"));
            }
            if field == "radius" && number == 0.0 {
                let items = elements(editable_list(group, "vertices", inbound)?);
                if items.first().is_some_and(|first| first.id() != item.id()) {
                    return Err("authoring physics radius may be zero only on the first vertex".into());
                }
            }
            patches.push(scalar_patch(target(field)?, "f", field, number_text(number), inbound)?);
        }
        _ => return Err("unsupported authoring physics field".into()),
    }
    Ok(json_key(list, field))
}

/// Remove one list item, renumbering outputs when a vertex removal shifts the strand.
fn item_remove(
    graph: &Graph<'_, '_>,
    inbound: &HashSet<&str>,
    text: &str,
    group: Node<'_, '_>,
    list: &str,
    list_node: Node<'_, '_>,
    items: &[Node<'_, '_>],
    index: usize,
    patches: &mut Vec<Patch>,
) -> Result<(), String> {
    let slot = *items
        .get(index)
        .ok_or("authoring physics item index is outside the list")?;
    if slot.attribute("xs.ref").is_some() || referenced(slot, inbound) {
        return Err("authoring physics list item uses shared storage".into());
    }
    if !subtree_unreferenced(slot, inbound) {
        return Err("authoring physics item is referenced elsewhere".into());
    }
    if list == "vertices" {
        if items.len() <= MIN_STRAND_VERTICES {
            return Err("authoring physics strand needs at least two vertices".into());
        }
        renumber_outputs(graph, inbound, group, index, -1, patches)?;
    }
    patches.push(Patch { range: slot.range(), value: String::new() });
    count_patch(text, list_node, items.len() - 1, patches)?;
    Ok(())
}

/// Shift every inline output's `vertexIndex` across a removed or inserted vertex position `at` by `delta`
/// (-1 or +1). A removal landing exactly on a referenced vertex, or an output stored as an alias, refuses.
fn renumber_outputs(
    graph: &Graph<'_, '_>,
    inbound: &HashSet<&str>,
    group: Node<'_, '_>,
    at: usize,
    delta: isize,
    patches: &mut Vec<Patch>,
) -> Result<(), String> {
    if graph.field(group, "outputs")?.is_none() {
        return Ok(());
    }
    let outputs = editable_list(group, "outputs", inbound)
        .map_err(|_| "authoring physics outputs list uses shared storage".to_owned())?;
    for slot in elements(outputs) {
        if slot.attribute("xs.ref").is_some() || referenced(slot, inbound) {
            return Err("authoring physics output needing renumbering uses shared storage".into());
        }
        let output = graph.resolve(slot)?;
        if referenced(output, inbound) {
            return Err("authoring physics output needing renumbering uses shared storage".into());
        }
        let Some(index_text) = values::text_field(graph, output, "vertexIndex")? else {
            continue;
        };
        let index = index_text
            .trim()
            .parse::<usize>()
            .map_err(|_| "authoring physics output vertexIndex is not a number")?;
        if delta < 0 && index == at {
            return Err("an authoring physics output names the removed vertex".into());
        }
        if index >= at {
            let shifted = index as isize + delta;
            if shifted < 1 {
                return Err("authoring physics vertex renumbering leaves the strand".into());
            }
            if slot.attribute("xs.ref").is_some() {
                return Err("authoring physics output needing renumbering uses shared storage".into());
            }
            let field = direct_field(output, "vertexIndex")
                .ok_or("authoring physics output lacks an inline vertexIndex field")?;
            patches.push(scalar_patch(field, "i", "vertexIndex", shifted.to_string(), inbound)?);
        }
    }
    Ok(())
}

fn first_group(
    by_id: &HashMap<String, String>,
    guid_objects: &HashMap<&str, &str>,
    id: &str,
    value: &Value,
) -> Result<String, String> {
    let object = value.as_object().ok_or("new physics group must be an object")?;
    if object.keys().any(|key| !matches!(key.as_str(), "name" | "normalization" | "inputs" | "outputs" | "vertices")) {
        return Err("new physics group carries an unknown field".into());
    }
    let name = object.get("name").and_then(Value::as_str).ok_or("new physics group requires a name")?;
    if name.len() > MAX_NAME_BYTES {
        return Err("authoring physics name exceeds 1024 bytes".into());
    }
    let normalization = object.get("normalization").and_then(Value::as_object)
        .ok_or("new physics group requires normalization")?;
    if normalization.len() != 2 || !normalization.contains_key("position") || !normalization.contains_key("angle") {
        return Err("normalization requires exactly position and angle".into());
    }
    let mut ranges = String::new();
    for (key, native) in [("position", "Position"), ("angle", "Angle")] {
        let range = normalization[key].as_object().ok_or("normalization range must be an object")?;
        if range.len() != 3 || !["min", "default", "max"].iter().all(|key| range.contains_key(*key)) {
            return Err("normalization range requires exactly min, default and max".into());
        }
        let min = native_finite(&range["min"], "min")?;
        let default = native_finite(&range["default"], "default")?;
        let max = native_finite(&range["max"], "max")?;
        if min > default || default > max {
            return Err("authoring physics normalization must keep min <= default <= max".into());
        }
        ranges.push_str(&format!(
            "<f xs.n='normalized{native}ValueMin'>{}</f><f xs.n='normalized{native}DefaultValue'>{}</f>\
             <f xs.n='normalized{native}ValueMax'>{}</f>", number_text(min), number_text(default), number_text(max),
        ));
    }
    let mut lists = String::new();
    let vertices = object.get("vertices").and_then(Value::as_array).ok_or("new physics group requires vertices")?;
    for list in ["inputs", "outputs", "vertices"] {
        let items = object.get(list).and_then(Value::as_array).ok_or_else(|| format!("new physics group requires {list}"))?;
        if items.len() > MAX_ITEMS_PER_LIST { return Err("authoring physics list exceeds 1024 entries".into()); }
        lists.push_str(&format!("<carray_list xs.n='{list}' count='{}'>", items.len()));
        for (index, item) in items.iter().enumerate() {
            lists.push_str(&item_insert(by_id, guid_objects, list, vertices.len(), index, item)?);
        }
        lists.push_str("</carray_list>");
    }
    Ok(format!("<CPhysicsSettingsSource><CPhysicsSettingId xs.n='id' idstr='{id}'/>\
        <s xs.n='name'>{}</s>{lists}{ranges}</CPhysicsSettingsSource>", escape_text(name)))
}

fn item_insert(
    by_id: &HashMap<String, String>,
    guid_objects: &HashMap<&str, &str>,
    list: &str,
    vertex_count: usize,
    insert: usize,
    value: &Value,
) -> Result<String, String> {
    let object = value.as_object().ok_or("inserted physics item must be an object")?;
    let allowed: &[&str] = match list {
        "vertices" => &["x", "y", "mobility", "delay", "acceleration", "radius"],
        "inputs" => &["source", "type", "scale", "weight", "reflect"],
        "outputs" => &["destination", "vertexIndex", "scale", "weight", "reflect"],
        _ => return Err("unsupported authoring physics list".into()),
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("inserted physics {list} item carries an unknown key {key:?}"));
        }
    }
    match list {
        "vertices" => {
            for name in ["x", "y", "mobility", "delay", "acceleration", "radius"] {
                if !object.contains_key(name) {
                    return Err(format!("inserted physics vertex lacks {name}"));
                }
            }
            let mobility = native_finite(&object["mobility"], "mobility")?;
            if !(0.0..=1.0).contains(&mobility) {
                return Err("inserted physics mobility is outside 0..=1".into());
            }
            let delay = optional_number(object, "delay", f64::NAN)?;
            let acceleration = optional_number(object, "acceleration", f64::NAN)?;
            let radius = optional_number(object, "radius", f64::NAN)?;
            if delay < 0.0 || acceleration < 0.0 || radius < 0.0 {
                return Err("inserted physics delay, acceleration and radius must be nonnegative".into());
            }
            if radius == 0.0 && insert != 0 {
                return Err("inserted physics radius may be zero only at index 0".into());
            }
            let x = optional_number(object, "x", f64::NAN)?;
            let y = optional_number(object, "y", f64::NAN)?;
            Ok(format!(
                "<CPhysicsVertex><GVector2 xs.n='position'><f xs.n='x'>{}</f><f xs.n='y'>{}</f></GVector2>\
                 <f xs.n='mobility'>{}</f><f xs.n='delay'>{}</f><f xs.n='acceleration'>{}</f>\
                 <f xs.n='radius'>{}</f></CPhysicsVertex>",
                number_text(x), number_text(y), number_text(mobility),
                number_text(delay), number_text(acceleration), number_text(radius),
            ))
        }
        "inputs" => {
            let source = object
                .get("source")
                .and_then(Value::as_str)
                .ok_or("inserted physics input lacks a source parameter ID")?;
            let guid = by_id
                .get(source)
                .ok_or_else(|| format!("authoring physics names unknown parameter {source:?}"))?;
            let token = match object.get("type").and_then(Value::as_str) {
                Some("X") => "SRC_TO_X",
                Some("Angle") => "SRC_TO_G_ANGLE",
                _ => return Err("inserted physics input type must be \"X\" or \"Angle\"".into()),
            };
            let weight = optional_number(object, "weight", 100.0)?;
            if !(0.0..=100.0).contains(&weight) {
                return Err("inserted physics weight is outside 0..=100".into());
            }
            let scale = optional_number(object, "scale", 10.0)?;
            let reflect = optional_bool(object, "reflect", false)?;
            Ok(format!(
                "<CPhysicsInput><CParameterGuid xs.n='source' {}/>\
                 <CPhysicsSourceType xs.n='type' v='{}'/><f xs.n='angleScale'>{}</f>\
                 <f xs.n='weight'>{}</f><b xs.n='isReverse'>{}</b></CPhysicsInput>",
                parameter_reference(guid, guid_objects), token, number_text(scale), number_text(weight), reflect,
            ))
        }
        "outputs" => {
            let destination = object
                .get("destination")
                .and_then(Value::as_str)
                .ok_or("inserted physics output lacks a destination parameter ID")?;
            let guid = by_id
                .get(destination)
                .ok_or_else(|| format!("authoring physics names unknown parameter {destination:?}"))?;
            let vertex = object
                .get("vertexIndex")
                .and_then(Value::as_u64)
                .filter(|vertex| *vertex >= 1 && (*vertex as usize) < vertex_count)
                .ok_or("inserted physics output vertexIndex is outside the strand")?;
            let weight = optional_number(object, "weight", 100.0)?;
            if !(0.0..=100.0).contains(&weight) {
                return Err("inserted physics weight is outside 0..=100".into());
            }
            let scale = optional_number(object, "scale", 10.0)?;
            let reflect = optional_bool(object, "reflect", false)?;
            Ok(format!(
                "<CPhysicsOutput><CParameterGuid xs.n='destination' {}/>\
                 <CPhysicsSourceType xs.n='type' v='SRC_TO_G_ANGLE'/><f xs.n='angleScale'>{}</f>\
                 <f xs.n='weight'>{}</f><b xs.n='isReverse'>{}</b><i xs.n='vertexIndex'>{}</i></CPhysicsOutput>",
                parameter_reference(guid, guid_objects), number_text(scale), number_text(weight), reflect, vertex,
            ))
        }
        _ => Err("unsupported authoring physics list".into()),
    }
}

/// The byte offset an inserted element goes to: before the slot at `insert`, after the previous slot, or inside an
/// empty list. Returns true when the list element still needs its `count` patched.
fn item_insert_offset(
    text: &str,
    list: Node<'_, '_>,
    items: &[Node<'_, '_>],
    insert: usize,
    patches: &mut Vec<Patch>,
    element: &str,
) -> Result<bool, String> {
    if insert < items.len() {
        let start = items[insert].range().start;
        patches.push(Patch { range: start..start, value: element.to_owned() });
        return Ok(true);
    }
    if let Some(last) = items.last() {
        let end = last.range().end;
        patches.push(Patch { range: end..end, value: element.to_owned() });
        return Ok(true);
    }
    let (open, inner) = element_spans(text, list)?;
    if inner.is_empty() && text[open.clone()].ends_with("/>") {
        // A self-closing empty list expands around the new item, carrying its attributes and the new count.
        let tag = list.tag_name().name();
        let attrs: String = list
            .attributes()
            .filter(|attribute| attribute.name() != "count")
            .map(|attribute| format!(" {}='{}'", attribute.name(), escape_attribute(attribute.value())))
            .collect();
        patches.push(Patch {
            range: list.range(),
            value: format!("<{tag}{attrs} count='1'>{element}</{tag}>"),
        });
        return Ok(false);
    }
    patches.push(Patch { range: inner.end..inner.end, value: element.to_owned() });
    Ok(true)
}

/// The presets of the user for the inputs of a physics group (PHY02, issue #236), in
/// `physics3-input-presets.json` of the settings directory. There is none built in.
pub(crate) const INPUT_PRESETS: Kind = Kind {
    file: "physics3-input-presets.json",
    noun: "physics input preset",
    entry: input_preset_entry,
    builtin: |_| false,
};

/// The presets of the user for the physical model of a group, its pendulum vertices, in
/// `physics3-model-presets.json` of the settings directory. There is none built in.
pub(crate) const MODEL_PRESETS: Kind = Kind {
    file: "physics3-model-presets.json",
    noun: "physics model preset",
    entry: model_preset_entry,
    builtin: |_| false,
};

/// The fields a preset keeps of an input and of a vertex. `scale` is the only one an item may leave out.
const INPUT_KEYS: [&str; 5] = ["source", "type", "weight", "reflect", "scale"];
const VERTEX_KEYS: [&str; 6] = ["x", "y", "mobility", "delay", "acceleration", "radius"];

fn input_preset_entry(request: &Value) -> Result<Value, String> {
    preset_entry("inputs", request)
}

fn model_preset_entry(request: &Value) -> Result<Value, String> {
    preset_entry("vertices", request)
}

/// The list a preset request names: the inputs of a group or its pendulum vertices.
fn preset_list(request: &Value) -> Result<&'static str, String> {
    match request.get("list").and_then(Value::as_str) {
        Some("inputs") => Ok("inputs"),
        Some("vertices") => Ok("vertices"),
        _ => Err("a physics preset list must be inputs or vertices".into()),
    }
}

/// The store kind of the list a preset request names.
pub(crate) fn preset_kind(request: &Value) -> Result<&'static Kind, String> {
    match preset_list(request)? {
        "inputs" => Ok(&INPUT_PRESETS),
        _ => Ok(&MODEL_PRESETS),
    }
}

/// What a preset keeps of `request`: the `items` of one list, checked like the values `live2d.physics3.edit`
/// takes. Every entry read from the file goes through here as well.
fn preset_entry(list: &str, request: &Value) -> Result<Value, String> {
    let keys: &[&str] = if list == "inputs" { &INPUT_KEYS } else { &VERTEX_KEYS };
    let least = if list == "inputs" { 1 } else { MIN_STRAND_VERTICES };
    let items = request.get("items").and_then(Value::as_array).ok_or("items must be an array")?;
    if !(least..=MAX_ITEMS_PER_LIST).contains(&items.len()) {
        return Err(format!("a preset for {list} needs {least} to {MAX_ITEMS_PER_LIST} items"));
    }
    for (index, item) in items.iter().enumerate() {
        let item = item.as_object().ok_or("a preset item must be an object")?;
        if let Some(key) = item.keys().find(|key| !keys.contains(&key.as_str())) {
            return Err(format!("a preset item of {list} carries an unknown key {key:?}"));
        }
        if let Some(key) = keys.iter().find(|key| **key != "scale" && !item.contains_key(**key)) {
            return Err(format!("a preset item of {list} needs {key}"));
        }
        for (key, value) in item {
            check_preset_field(key, value, index)?;
        }
    }
    Ok(json!({ "items": items }))
}

fn check_preset_field(key: &str, value: &Value, index: usize) -> Result<(), String> {
    match key {
        "source" => {
            if !value.as_str().is_some_and(|id| !id.is_empty() && id.len() <= MAX_NAME_BYTES) {
                return Err("a preset item source must be a parameter ID".into());
            }
        }
        "type" => {
            if !matches!(value.as_str(), Some("X" | "Angle")) {
                return Err("a preset item type must be \"X\" or \"Angle\"".into());
            }
        }
        "reflect" => {
            if !value.is_boolean() {
                return Err("a preset item reflect must be a boolean".into());
            }
        }
        _ => {
            let number = native_finite(value, key)?;
            match key {
                "weight" if !(0.0..=100.0).contains(&number) => {
                    return Err("a preset item weight is outside 0..=100".into());
                }
                "mobility" if !(0.0..=1.0).contains(&number) => {
                    return Err("a preset item mobility is outside 0..=1".into());
                }
                "delay" | "acceleration" | "radius" if number < 0.0 => {
                    return Err(format!("a preset item {key} must be nonnegative"));
                }
                "radius" if number == 0.0 && index != 0 => {
                    return Err("a preset item radius may be zero only on the first vertex".into());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The request of `live2d.physics3.preset.save`, with `items` copied from the list of group `group` when it names
/// one. `view` is the typed view of the document, needed only then. A null field counts as absent, and what a
/// preset cannot keep is refused rather than dropped without a word.
pub(crate) fn preset_request(view: Option<&Value>, request: &Value) -> Result<Value, String> {
    let mut object: Map<String, Value> = request
        .as_object()
        .ok_or("a physics preset save request must be an object")?
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "op" | "list" | "name" | "overwrite" | "group" | "items"))
    {
        return Err(format!("a physics preset save request carries an unknown key {key:?}"));
    }
    let list = preset_list(request)?;
    match (object.contains_key("group"), object.contains_key("items")) {
        (true, true) => return Err("a physics preset takes group or items, not both".into()),
        (false, false) => return Err("a physics preset needs group or items".into()),
        _ => {}
    }
    if let Some(group) = request_index(&object, "group")? {
        let view = view.ok_or("physics reading requires an active Live2D document")?;
        let groups = view["groups"].as_array().ok_or("authoring model has no physics settings")?;
        let source = groups.get(group).ok_or("authoring physics group index is outside the list")?;
        let keys: &[&str] = if list == "inputs" { &INPUT_KEYS } else { &VERTEX_KEYS };
        let items: Vec<Value> = source[list]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                let kept: Map<String, Value> = keys
                    .iter()
                    .filter_map(|key| {
                        let value = item.get(*key).filter(|value| !value.is_null())?;
                        Some(((*key).to_owned(), value.clone()))
                    })
                    .collect();
                Value::Object(kept)
            })
            .collect();
        object.insert("items".into(), Value::Array(items));
    }
    Ok(Value::Object(object))
}

/// `live2d.physics3.edit` with `preset`: makes the `list` of group `group` the saved `entry`. The fields both
/// lists share are set one by one, then the items the preset has beyond the group are inserted at the end, and the
/// items the group has beyond the preset are removed from the end. Each step is a `live2d.physics3.edit` on the
/// text the step before it made. A step the edit refuses (a parameter the document lacks, shared storage, an
/// output that names a removed vertex) refuses the whole request, and the one candidate returned is one undo step.
pub(crate) fn apply_preset(
    container: &CaffDocument,
    model: &Value,
    request: &Value,
    entry: &Value,
) -> Result<Value, String> {
    let object = request.as_object().ok_or("a physics preset edit request must be an object")?;
    if object.keys().any(|key| !matches!(key.as_str(), "op" | "expectedRevision" | "group" | "list" | "preset")) {
        return Err("a physics preset edit takes group, list and preset only".into());
    }
    let list = preset_list(request)?;
    let group = request_index(object, "group")?.ok_or("authoring physics edit lacks a group")?;
    let now = view(container, model)?;
    let groups = now["groups"].as_array().ok_or("authoring model has no physics settings")?;
    let now = groups
        .get(group)
        .map(|group| &group[list])
        .and_then(Value::as_array)
        .ok_or("authoring physics group index is outside the list")?;
    let wanted = entry["items"].as_array().ok_or("a physics preset entry lacks items")?;
    let keys: &[&str] = if list == "inputs" { &INPUT_KEYS } else { &VERTEX_KEYS };
    let same = |a: &Value, b: &Value| a == b || a.as_f64().zip(b.as_f64()).is_some_and(|(a, b)| a == b);
    let mut steps = Vec::new();
    for (index, (have, want)) in now.iter().zip(wanted).enumerate() {
        for key in keys {
            if let Some(value) = want.get(*key).filter(|value| !same(&have[*key], value)) {
                steps.push(json!({"group": group, "list": list, "index": index, "field": key, "value": value}));
            }
        }
    }
    for (index, item) in wanted.iter().enumerate().skip(now.len()) {
        steps.push(json!({"group": group, "list": list, "insert": index, "value": item}));
    }
    for index in (wanted.len()..now.len()).rev() {
        steps.push(json!({"group": group, "list": list, "index": index, "remove": true}));
    }
    let xml = container.main_xml();
    let mut text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?
        .to_owned();
    let mut candidate = model.clone();
    // shortcut: each step parses the document again; one shared patch list if a preset of 1000 items is too slow.
    for step in &steps {
        (candidate, text) = edit_text(&text, &candidate, step)?;
    }
    Ok(candidate)
}

#[derive(Clone, Copy, PartialEq)]
enum OwnedRole {
    Set,
    Groups,
    Group,
    Inputs,
    Outputs,
    Vertices,
    Input,
    Output,
    Vertex,
    Position,
    Leaf(&'static str),
    Rate,
    PhysicsGuid(&'static str),
    Selection,
    Parameter,
}

impl OwnedRole {
    fn tag(self, node: Node<'_, '_>) -> bool {
        let tag = match self {
            Self::Set => "CPhysicsSettingsSourceSet",
            Self::Groups | Self::Inputs | Self::Outputs | Self::Vertices => "carray_list",
            Self::Group => "CPhysicsSettingsSource",
            Self::Input => "CPhysicsInput",
            Self::Output => "CPhysicsOutput",
            Self::Vertex => "CPhysicsVertex",
            Self::Position => "GVector2",
            Self::Leaf(tag) | Self::PhysicsGuid(tag) => tag,
            Self::Rate => return node.has_tag_name("i") || node.has_tag_name("null"),
            Self::Selection => return node.has_tag_name("CPhysicsSettingsGuid") || node.has_tag_name("null"),
            Self::Parameter => "CParameterGuid",
        };
        node.has_tag_name(tag)
    }

    fn fields(self) -> &'static [(&'static str, OwnedRole)] {
        use OwnedRole::*;
        match self {
            Set => &[("settingFPS", Rate), ("_sourceCubismPhysics", Groups), ("selectedCubismPhysics", Selection)],
            Group => &[
                ("guid", PhysicsGuid("CPhysicsSettingsGuid")), ("id", Leaf("CPhysicsSettingId")),
                ("name", Leaf("s")), ("inputs", Inputs), ("outputs", Outputs), ("vertices", Vertices),
                ("normalizedPositionValueMin", Leaf("f")), ("normalizedPositionDefaultValue", Leaf("f")),
                ("normalizedPositionValueMax", Leaf("f")), ("normalizedAngleValueMin", Leaf("f")),
                ("normalizedAngleDefaultValue", Leaf("f")), ("normalizedAngleValueMax", Leaf("f")),
            ],
            Input => &[("source", Parameter), ("type", Leaf("CPhysicsSourceType")),
                ("guid", PhysicsGuid("CPhysicsDataGuid")), ("angleScale", Leaf("f")),
                ("weight", Leaf("f")), ("isReverse", Leaf("b")), ("translationScale", Position)],
            Output => &[("destination", Parameter), ("type", Leaf("CPhysicsSourceType")),
                ("guid", PhysicsGuid("CPhysicsDataGuid")), ("angleScale", Leaf("f")),
                ("weight", Leaf("f")), ("isReverse", Leaf("b")), ("vertexIndex", Leaf("i")),
                ("translationScale", Position)],
            Vertex => &[("position", Position), ("guid", PhysicsGuid("CPhysicsDataGuid")),
                ("mobility", Leaf("f")), ("delay", Leaf("f")), ("acceleration", Leaf("f")), ("radius", Leaf("f"))],
            Position => &[("x", Leaf("f")), ("y", Leaf("f"))],
            _ => &[],
        }
    }

    fn item(self) -> Option<Self> {
        match self {
            Self::Groups => Some(Self::Group), Self::Inputs => Some(Self::Input),
            Self::Outputs => Some(Self::Output), Self::Vertices => Some(Self::Vertex), _ => None,
        }
    }
}

struct OwnedPhysics<'d, 'i> {
    slot: Node<'d, 'i>,
    node: Node<'d, 'i>,
    role: OwnedRole,
    children: Vec<OwnedPhysics<'d, 'i>>,
    shared: bool,
    fresh_guid: Option<String>,
}

const MAX_DETACH_BYTES: usize = 16 * 1024 * 1024;
const MAX_DETACH_NODES: usize = 500_000;

fn owned_physics<'d, 'i>(
    graph: &Graph<'d, 'i>, slot: Node<'d, 'i>, role: OwnedRole, inbound: &HashSet<&str>,
    active: &mut HashSet<roxmltree::NodeId>, nodes: &mut usize, bytes: &mut usize,
) -> Result<OwnedPhysics<'d, 'i>, String> {
    let node = graph.resolve(slot)?;
    if !active.insert(node.id()) || active.len() > 32 {
        return Err("authoring physics ownership cycle or depth limit".into());
    }
    if !role.tag(node) {
        return Err(format!("authoring physics detach does not support {} in this ownership role", node.tag_name().name()));
    }
    if role == OwnedRole::Selection && !node.has_tag_name("null")
        && node.attribute("uuid").is_none_or(|value| value.trim().is_empty())
    {
        return Err("authoring physics detach refuses a selected group without identity".into());
    }
    if role != OwnedRole::Parameter && slot.attribute("xs.ref").is_some_and(|id| node.attribute("xs.id") != Some(id)) {
        return Err("authoring physics detach refuses alias chains".into());
    }
    if slot.id() != node.id() && (slot.children().any(|child| !child.is_text() || !child.text().unwrap_or("").trim().is_empty())
        || slot.attributes().any(|attribute| !matches!(attribute.name(), "xs.n" | "xs.ref" | "xs.id" | "xs.idx")))
    {
        return Err("authoring physics detach refuses alias metadata".into());
    }
    let shared = if role == OwnedRole::Parameter {
        slot.attribute("xs.id").is_some()
    } else {
        slot.id() != node.id() || referenced(slot, inbound) || referenced(node, inbound)
    };
    let mut children = Vec::new();
    if let Some(item_role) = role.item() {
        let slots = elements(node);
        check_count(node, slots.len())?;
        if slots.len() > if role == OwnedRole::Groups { MAX_GROUPS } else { MAX_ITEMS_PER_LIST } {
            return Err("authoring physics detach exceeds list budget".into());
        }
        for slot in slots {
            children.push(owned_physics(graph, slot, item_role, inbound, active, nodes, bytes)?);
        }
    } else {
        for (name, child_role) in role.fields() {
            let resolved = graph.field(node, name)?;
            match (direct_field(node, name), resolved) {
                (Some(slot), Some(_)) => children.push(owned_physics(graph, slot, *child_role, inbound, active, nodes, bytes)?),
                (None, Some(_)) => return Err("authoring physics detach refuses inherited fields".into()),
                _ => {},
            }
        }
        if role.fields().is_empty() && node.children().any(|child| child.is_element()) {
            return Err("authoring physics detach refuses structured scalar storage".into());
        }
    }
    let excluded = |candidate: Node<'_, '_>| children.iter().any(|child| {
        let range = child.slot.range();
        range.start <= candidate.range().start && candidate.range().end <= range.end
    });
    *nodes = nodes.checked_add(node.descendants().filter(|candidate| !excluded(*candidate)).count())
        .filter(|count| *count <= MAX_DETACH_NODES).ok_or("authoring physics detach exceeds node budget")?;
    *bytes = bytes.checked_add(node.range().len() - children.iter().map(|child| child.slot.range().len()).sum::<usize>())
        .filter(|count| *count <= MAX_DETACH_BYTES).ok_or("authoring physics detach exceeds byte budget")?;
    active.remove(&node.id());
    Ok(OwnedPhysics { slot, node, role, children, shared, fresh_guid: None })
}

impl OwnedPhysics<'_, '_> {
    fn needs_detach(&self) -> bool {
        self.shared || self.children.iter().any(Self::needs_detach)
    }

    fn copied_ids(&self, ids: &mut HashSet<String>) {
        ids.extend(self.node.descendants().filter_map(|node| node.attribute("xs.id").map(str::to_owned)));
        for child in &self.children { child.copied_ids(ids); }
    }

    fn parameter_ids(&self, ids: &mut HashSet<String>) {
        if self.role == OwnedRole::Parameter {
            if let Some(id) = self.node.attribute("xs.id") { ids.insert(id.to_owned()); }
        }
        for child in &self.children { child.parameter_ids(ids); }
    }

    fn assign_guids(&mut self, used: &mut HashSet<String>, groups: &mut HashMap<String, Vec<String>>) -> Result<(), String> {
        if let OwnedRole::PhysicsGuid(tag) = self.role {
            if let Some(old) = self.node.attribute("uuid") {
                let fresh = crate::animation_source::new_guid()?;
                if !used.insert(fresh.clone()) { return Err("authoring physics identity collision".into()); }
                if tag == "CPhysicsSettingsGuid" { groups.entry(old.to_owned()).or_default().push(fresh.clone()); }
                self.fresh_guid = Some(fresh);
            }
        }
        for child in &mut self.children { child.assign_guids(used, groups)?; }
        Ok(())
    }

    fn guid_ownership(&self, nodes: &mut HashSet<roxmltree::NodeId>, values: &mut HashMap<String, HashSet<String>>) {
        if matches!(self.role, OwnedRole::PhysicsGuid(_) | OwnedRole::Selection) {
            nodes.insert(self.node.id());
            nodes.insert(self.slot.id());
            if matches!(self.role, OwnedRole::PhysicsGuid(_)) {
                if let Some(uuid) = self.node.attribute("uuid") {
                    values.entry(self.node.tag_name().name().to_owned()).or_default().insert(uuid.to_owned());
                }
            }
        }
        for child in &self.children { child.guid_ownership(nodes, values); }
    }

    fn render(&self, text: &str, copied: &HashSet<String>, used: &mut HashSet<String>, next_idx: &mut u64,
        groups: &HashMap<String, Vec<String>>,
    ) -> Result<String, String> {
        if self.role == OwnedRole::Parameter {
            return Ok(if self.slot.attribute("xs.ref").is_some() || self.node.attribute("xs.id").is_none() {
                text[self.slot.range()].to_owned()
            } else {
                format!("<CParameterGuid xs.n='{}' xs.ref='{}'/>", escape_attribute(self.slot.attribute("xs.n").unwrap_or("")),
                    escape_attribute(self.node.attribute("xs.id").unwrap()))
            });
        }
        let mut patches = Vec::new();
        for child in &self.children {
            patches.push(Patch { range: child.slot.range(), value: child.render(text, copied, used, next_idx, groups)? });
        }
        let local = |node: Node<'_, '_>| !self.children.iter().any(|child| {
            let range = child.slot.range();
            range.start <= node.range().start && node.range().end <= range.end
        });
        let mut ids = HashMap::new();
        for node in self.node.descendants().filter(|node| node.is_element() && local(*node) && node.id() != self.node.id()) {
            if let Some(id) = node.attribute("xs.id") {
                let fresh = format!("physics-detached-{}", crate::animation_source::new_guid()?);
                if !used.insert(fresh.clone()) { return Err("authoring physics identity collision".into()); }
                ids.insert(id, fresh);
            }
        }
        for node in self.node.descendants().filter(|node| node.is_element() && local(*node)) {
            for attribute in node.attributes() {
                let replacement = match attribute.name() {
                    "xs.id" if node.id() == self.node.id() => {
                        patches.push(Patch { range: attribute.range(), value: String::new() });
                        None
                    },
                    "xs.id" => ids.get(attribute.value()).cloned(),
                    "xs.ref" => match ids.get(attribute.value()) {
                        Some(id) => Some(id.clone()),
                        None if copied.contains(attribute.value()) => return Err("authoring physics detach refuses an opaque cross-reference".into()),
                        None => None,
                    },
                    "xs.idx" if node.id() == self.node.id() && matches!(self.role, OwnedRole::Leaf(_) | OwnedRole::Rate | OwnedRole::Parameter) => {
                        patches.push(Patch { range: attribute.range(), value: String::new() });
                        None
                    },
                    "xs.idx" => {
                        *next_idx = next_idx.checked_add(1).ok_or("authoring physics serial overflow")?;
                        Some(next_idx.to_string())
                    },
                    "uuid" if node.id() == self.node.id() && matches!(self.role, OwnedRole::PhysicsGuid(_)) => {
                        self.fresh_guid.clone()
                    },
                    "uuid" if node.id() == self.node.id() && self.role == OwnedRole::Selection => {
                        match groups.get(attribute.value()).map(Vec::as_slice) {
                            Some([guid]) => Some(guid.clone()),
                            Some(_) => return Err("authoring physics selected group is ambiguous after copying".into()),
                            None => return Err("authoring physics selected GUID does not name a copied group".into()),
                        }
                    },
                    "xs.n" if node.id() == self.node.id() => match self.slot.attribute("xs.n") {
                        Some(name) => Some(escape_attribute(name)),
                        None => {
                            patches.push(Patch { range: attribute.range(), value: String::new() });
                            None
                        },
                    },
                    _ => None,
                };
                if let Some(value) = replacement { patches.push(Patch { range: attribute.range_value(), value }); }
            }
        }
        if self.node.attribute("xs.n").is_none() {
            if let Some(name) = self.slot.attribute("xs.n") {
                let start = self.node.range().start + 1 + self.node.tag_name().name().len();
                patches.push(Patch { range: start..start, value: format!(" xs.n='{}'", escape_attribute(name)) });
            }
        }
        patches.sort_by_key(|patch| (patch.range.start, patch.range.end));
        if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
            return Err("authoring physics detach produced overlapping changes".into());
        }
        let range = self.node.range();
        let mut output = String::new();
        let mut at = range.start;
        for patch in patches {
            output.push_str(&text[at..patch.range.start]);
            output.push_str(&patch.value);
            at = patch.range.end;
            if output.len() > MAX_DETACH_BYTES { return Err("authoring physics detach exceeds byte budget".into()); }
        }
        output.push_str(&text[at..range.end]);
        if output.len() > MAX_DETACH_BYTES { return Err("authoring physics detach exceeds byte budget".into()); }
        Ok(output)
    }
}

fn without_storage_flags(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            object.remove("shared"); object.remove("editable");
            for value in object.values_mut() { *value = without_storage_flags(value.take()); }
        },
        Value::Array(values) => for value in values { *value = without_storage_flags(value.take()); },
        _ => {},
    }
    value
}

fn detach_text(text: &str, model: &Value) -> Result<(Value, String), String> {
    let document = parse(text)?;
    let graph = Graph::new(&document)?;
    let inbound = inbound_refs(&document);
    let owner = graph.model()?.ok_or("authoring source contains no model")?;
    let slot = direct_field(owner, "physicsSettingsSourceSet").ok_or("authoring physics detach requires a direct settings field")?;
    let mut plan = owned_physics(&graph, slot, OwnedRole::Set, &inbound, &mut HashSet::new(), &mut 0, &mut 0)?;
    let (by_guid, _) = parameter_maps(model)?;
    let before = view_document(&graph, &inbound, &by_guid)?;
    if !plan.needs_detach() { return Ok((model.clone(), text.to_owned())); }
    let mut copied = HashSet::new();
    plan.copied_ids(&mut copied);
    let mut parameters = HashSet::new();
    plan.parameter_ids(&mut parameters);
    copied.retain(|id| !parameters.contains(id));
    let mut used: HashSet<String> = document.descendants().flat_map(|node| [node.attribute("xs.id"), node.attribute("uuid")])
        .flatten().map(str::to_owned).collect();
    let mut next_idx = 0;
    for value in document.descendants().filter_map(|node| node.attribute("xs.idx")) {
        next_idx = next_idx.max(value.parse::<u64>().map_err(|_| "authoring physics has an invalid serial")?);
    }
    let mut groups = HashMap::new();
    let mut guid_nodes = HashSet::new();
    let mut guid_values = HashMap::new();
    plan.guid_ownership(&mut guid_nodes, &mut guid_values);
    for node in document.descendants().filter(|node| node.is_element() && !guid_nodes.contains(&node.id())) {
        let target = graph.resolve(node)?;
        if target.attribute("uuid").is_some_and(|uuid| guid_values.get(target.tag_name().name()).is_some_and(|values| values.contains(uuid))) {
            return Err("authoring physics detach refuses an opaque physics GUID link".into());
        }
    }
    plan.assign_guids(&mut used, &mut groups)?;
    let body = plan.render(text, &copied, &mut used, &mut next_idx, &groups)?;
    let mut candidate = model.clone();
    candidate["physicsSource"] = json!(body);
    let rewritten = splice_source(text, &text[plan.node.range()], candidate["physicsSource"].as_str().unwrap())?;
    let after_document = parse(&rewritten)?;
    let after_graph = Graph::new(&after_document)?;
    let after_inbound = inbound_refs(&after_document);
    let after_owner = after_graph.model()?.ok_or("authoring physics detach lost model")?;
    let after_slot = direct_field(after_owner, "physicsSettingsSourceSet").ok_or("authoring physics detach lost settings")?;
    let after_plan = owned_physics(&after_graph, after_slot, OwnedRole::Set, &after_inbound, &mut HashSet::new(), &mut 0, &mut 0)?;
    if after_plan.needs_detach() || without_storage_flags(before) != without_storage_flags(view_document(&after_graph, &after_inbound, &by_guid)?) {
        return Err("authoring physics detach changed values or retained shared ownership".into());
    }
    Ok((candidate, rewritten))
}

pub(crate) fn splice_source(
    text: &str,
    initial_xml: &str,
    candidate_xml: &str,
) -> Result<String, String> {
    let document = parse(text)?;
    let graph = Graph::new(&document)?;
    let model = graph.model()?.ok_or("authoring source contains no model")?;
    let field = direct_field(model, "physicsSettingsSourceSet")
        .filter(|field| !field.has_tag_name("null"))
        .ok_or("authoring physics source is not direct model storage")?;
    let resolved = graph.resolve(field)?;
    let range = field.range();
    if text[resolved.range()] != *initial_xml {
        return Err("authoring physics source does not match the stored model".into());
    }
    let candidate = parse(candidate_xml)?;
    let ids: HashSet<_> = candidate.descendants().filter_map(|node| node.attribute("xs.id")).collect();
    let retained: Vec<_> = field.descendants().filter(|node| node.attribute("xs.id").is_some_and(|id| !ids.contains(id)))
        .filter(|node| !node.ancestors().skip(1).take_while(|ancestor| range.start <= ancestor.range().start && ancestor.range().end <= range.end)
            .any(|ancestor| ancestor.attribute("xs.id").is_some_and(|id| !ids.contains(id))))
        .collect();
    let mut retention = String::new();
    for node in retained { retention.push_str(&text[node.range()]); }
    let retention_at = if retention.is_empty() { range.end } else {
        let root = document.root_element();
        if root.id() == model.id() { return Err("authoring physics has no external definition storage".into()); }
        let shared = root.children().find(|node| node.has_tag_name("shared"));
        let container = shared.unwrap_or(root);
        let (open, content) = element_spans(text, container)?;
        if content.is_empty() && text[open].ends_with("/>") { return Err("authoring physics shared storage is self-closing".into()); }
        content.end
    };
    let root_start = document.root_element().range().start;
    let imports: Vec<_> = document.root().children().filter(|node| node.range().start < root_start)
        .filter_map(|node| node.pi())
        .filter(|pi| pi.target == "import").filter_map(|pi| pi.value).map(str::trim).collect();
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut missing = String::new();
    // Cubism resolves serialized class names through prolog imports, unlike our field-based reader.
    for (tag, class) in [
        ("CPhysicsSettingsSourceSet", "com.live2d.cubism.doc.gameData.physics.CPhysicsSettingsSourceSet"),
        ("CPhysicsSettingsSource", "com.live2d.cubism.doc.gameData.physics.CPhysicsSettingsSource"),
        ("CPhysicsInput", "com.live2d.cubism.doc.gameData.physics.CPhysicsInput"),
        ("CPhysicsOutput", "com.live2d.cubism.doc.gameData.physics.CPhysicsOutput"),
        ("CPhysicsVertex", "com.live2d.cubism.doc.gameData.physics.CPhysicsVertex"),
        ("CPhysicsSourceType", "com.live2d.cubism.doc.gameData.physics.CPhysicsController$CPhysicsSourceType"),
        ("CPhysicsSettingId", "com.live2d.cubism.doc.model.id.CPhysicsSettingId"),
        ("CPhysicsSettingsGuid", "com.live2d.type.CPhysicsSettingsGuid"),
        ("CPhysicsDataGuid", "com.live2d.type.CPhysicsDataGuid"),
        ("CParameterGuid", "com.live2d.type.CParameterGuid"),
        ("GVector2", "com.live2d.graphics3d.type.GVector2"),
    ] {
        if !candidate.descendants().any(|node| node.has_tag_name(tag)) { continue; }
        if imports.iter().any(|import| import.rsplit(['.', '$']).next() == Some(tag) && *import != class) {
            return Err(format!("authoring physics class import conflicts with {class}"));
        }
        if !imports.contains(&class) && !resolved.descendants().any(|node| node.has_tag_name(tag)) {
            missing.push_str(&format!("<?import {class}?>{newline}"));
        }
    }
    let mut patches = vec![Patch { range, value: candidate_xml.to_owned() },
        Patch { range: root_start..root_start, value: missing }];
    if !retention.is_empty() { patches.push(Patch { range: retention_at..retention_at, value: retention }); }
    patches.sort_by_key(|patch| (patch.range.start, patch.range.end));
    if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
        return Err("authoring physics retention overlaps the active slot".into());
    }
    let mut output = text.to_owned();
    for patch in patches.iter().rev() { output.replace_range(patch.range.clone(), &patch.value); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detachment_bounds_emitted_occurrences_and_detects_ownership_cycles() {
        let xml = "<root><CModelSource><CPhysicsSettingsSourceSet xs.n='physicsSettingsSourceSet' xs.id='set'>\
            <carray_list xs.n='_sourceCubismPhysics' count='0'/></CPhysicsSettingsSourceSet></CModelSource></root>";
        let document = parse(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let inbound = inbound_refs(&document);
        let slot = direct_field(graph.model().unwrap().unwrap(), "physicsSettingsSourceSet").unwrap();
        for (mut nodes, mut bytes, expected) in [(MAX_DETACH_NODES, 0, "node budget"), (0, MAX_DETACH_BYTES, "byte budget")] {
            let error = owned_physics(&graph, slot, OwnedRole::Set, &inbound, &mut HashSet::new(), &mut nodes, &mut bytes).err().unwrap();
            assert!(error.contains(expected), "{error}");
        }
        let cyclic = xml.replace("<carray_list xs.n='_sourceCubismPhysics' count='0'/>",
            "<carray_list xs.n='_sourceCubismPhysics' xs.ref='set'/>");
        let document = parse(&cyclic).unwrap();
        let graph = Graph::new(&document).unwrap();
        let slot = direct_field(graph.model().unwrap().unwrap(), "physicsSettingsSourceSet").unwrap();
        let error = owned_physics(&graph, slot, OwnedRole::Set, &inbound_refs(&document), &mut HashSet::new(), &mut 0, &mut 0).err().unwrap();
        assert!(error.contains("ownership cycle"), "{error}");
    }

    #[test]
    fn view_item_budgets_copied_parameter_ids() {
        let document = parse(
            "<CPhysicsInput><CParameterGuid xs.n='source' uuid='guid-one'/>\
             <CPhysicsSourceType xs.n='type' v='SRC_TO_X'/></CPhysicsInput>",
        )
        .unwrap();
        let graph = Graph::new(&document).unwrap();
        let inbound = inbound_refs(&document);
        let by_guid = HashMap::from([(
            String::from("guid-one"),
            String::from("a-parameter-id-longer-than-the-limit"),
        )]);
        let mut budget = Budget::new(4).with_text_limit(8);
        let error = view_item(
            &graph, &inbound, &by_guid, document.root_element(), 0, "inputs", false, true,
            &mut budget,
        )
        .unwrap_err();
        assert!(error.contains("copied-text"), "{error}");
    }
}
