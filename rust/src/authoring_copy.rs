//! Copying a CMO3 source (issue #142, create). The copy is written by cloning the original's own XML: the vendor
//! layout of a new object comes from the file, never from a guess. `insert` clones every copied source of the model
//! into the opened file with fresh GUIDs and `xs.id`s; `authoring_edit::rewrite` then re-reads that file and patches
//! whatever the model changed since (name, ID, forms, parent) the usual way. A source the file refers to from
//! anywhere but other sources, its source set and its parent's `_childGuids` is refused: such a place may be a
//! registry that should list the copy too, and its layout is unknown.

use crate::authoring_edit::XmlPatch;
use crate::authoring_graph::Graph;
use crate::authoring_delete::named_entries;
use crate::authoring_reparent::{append_patch, attribute_patch, source_nodes};
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

const SOURCE_SETS: [(&str, &[&str]); 4] = [
    ("drawableSourceSet", &["ArtMesh", "ArtPath"]),
    ("deformerSourceSet", &["Warp", "Rotation"]),
    ("partSourceSet", &["Part"]),
    ("affecterSourceSet", &["Glue"]),
];

/// One source the session made by copying a source of the opened file: the GUIDs of that original (the source and
/// its forms) paired with the copy's.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Copy {
    pub template: String,
    pub guids: Vec<(String, String)>,
}

impl Copy {
    pub fn guid(&self) -> &str {
        self.guids.iter().find(|(old, _)| *old == self.template).map(|(_, new)| new.as_str()).unwrap_or_default()
    }
}

fn set_of(source: &Value) -> Option<&'static str> {
    let kind = source["data"]["type"].as_str()?;
    SOURCE_SETS.iter().find(|(_, kinds)| kinds.contains(&kind)).map(|(set, _)| *set)
}

fn replace_strings(value: &mut Value, map: &HashMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some(new) = map.get(text.as_str()) {
                *text = new.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| replace_strings(item, map)),
        Value::Object(fields) => fields.values_mut().for_each(|field| replace_strings(field, map)),
        _ => {}
    }
}

/// `live2d.duplicate {guid, id?}`: a copy of one source (ArtMesh, ArtPath, deformer, Glue or empty Part) under the
/// same parent, after the last source of its kind. `fresh` makes each new GUID. The copy keeps the name; its ID is
/// `id` or the first free `<id>_copy`, `<id>_copy2`, ...
pub(crate) fn duplicate(
    model: &Value,
    request: &Value,
    copies: &[Copy],
    fresh: &mut dyn FnMut() -> Result<String, String>,
) -> Result<(Value, Copy), String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    if model["brushes"].as_array().into_iter().flatten().any(|brush| brush["guid"] == guid) {
        let matches = ["sources", "parameters", "brushes"].iter()
            .flat_map(|list| model[*list].as_array().into_iter().flatten()).filter(|entry| entry["guid"] == guid).count();
        if matches != 1 {
            return Err("the brush GUID is ambiguous".into());
        }
        let brushes = model["brushes"].as_array().unwrap();
        let original = brushes.iter().find(|brush| brush["guid"] == guid).unwrap();
        let previous = copies.iter().find(|copy| copy.guid() == guid);
        let template = previous.map_or(guid, |copy| copy.template.as_str()).to_owned();
        let mut identities = previous.map(|copy| copy.guids.clone()).unwrap_or_else(|| {
            let mut ids = vec![(guid.to_owned(), guid.to_owned())];
            if let Some(texture) = original["textureInput"]["texture"]["guid"].as_str() {
                ids.push((texture.to_owned(), texture.to_owned()));
            }
            ids
        });
        let mut map = HashMap::new();
        for (_, current) in &mut identities {
            let new = fresh()?;
            map.insert(current.clone(), new.clone());
            *current = new;
        }
        let mut brush = original.clone();
        replace_strings(&mut brush, &map);
        brush["id"] = json!(copy_id(brushes, original, request)?);
        let mut result = model.clone();
        result["brushes"].as_array_mut().unwrap().push(brush);
        return Ok((result, Copy { template, guids: identities }));
    }
    if model["parameters"].as_array().into_iter().flatten().any(|parameter| parameter["guid"] == guid) {
        return duplicate_parameter(model, guid, request, fresh);
    }
    let sources = model["sources"].as_array().ok_or("authoring source model lacks sources")?;
    let index = sources.iter().position(|source| source["guid"] == guid).ok_or_else(|| format!("source {guid} was not found"))?;
    let source = &sources[index];
    let parent = source["parentPartGuid"].as_str().ok_or("the root Part cannot be copied")?;
    if source["data"]["childGuids"].as_array().is_some_and(|children| !children.is_empty()) {
        return Err("a Part with children cannot be copied".into());
    }
    let set = set_of(source).ok_or("this kind of source cannot be copied")?;
    // A copy of a copy is cloned from the same original in the file.
    let (template, to_source): (String, HashMap<String, String>) = match copies.iter().find(|copy| copy.guid() == guid) {
        Some(copy) => (copy.template.clone(), copy.guids.iter().cloned().collect()),
        None => {
            let forms = source["forms"].as_array().into_iter().flatten().filter_map(|form| form["guid"].as_str());
            (guid.to_owned(), std::iter::once(guid).chain(forms).map(|g| (g.to_owned(), g.to_owned())).collect())
        }
    };
    let mut renamed = HashMap::new();
    let mut guids = Vec::new();
    let mut originals: Vec<(&String, &String)> = to_source.iter().collect();
    originals.sort();
    for (original, current) in originals {
        let new = fresh()?;
        renamed.insert(current.clone(), new.clone());
        guids.push((original.clone(), new));
    }
    let id = copy_id(sources, source, request)?;
    let mut copy = source.clone();
    replace_strings(&mut copy, &renamed);
    copy["id"] = json!(id);
    let new_guid = copy["guid"].clone();
    // The copy's keyform bindings are new objects; their names take a suffix from the copy's GUID.
    let suffix: String = new_guid.as_str().unwrap_or_default().chars().take(8).collect();
    rename_bindings(&mut copy, &suffix);
    let mut result = model.clone();
    let list = result["sources"].as_array_mut().ok_or("authoring source model lacks sources")?;
    let at = list.iter().rposition(|other| set_of(other) == Some(set)).map_or(list.len(), |last| last + 1);
    list.insert(at, copy);
    let owner = list.iter_mut().find(|other| other["guid"] == parent).ok_or("the parent Part is missing")?;
    owner["data"]["childGuids"].as_array_mut().ok_or("the parent Part lists no children")?.push(new_guid);
    Ok((result, Copy { template, guids }))
}

fn rename_bindings(value: &mut Value, suffix: &str) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                match field {
                    Value::String(name) if key == "bindingRef" => *name = format!("{name}-{suffix}"),
                    _ => rename_bindings(field, suffix),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| rename_bindings(item, suffix)),
        _ => {}
    }
}

/// The binding names the original uses and the names the copy's model gives them, at the same places.
fn binding_names(original: &Value, copy: &Value, names: &mut HashMap<String, String>) {
    match (original, copy) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, field) in a {
                match (key.as_str(), field, b.get(key)) {
                    ("bindingRef", Value::String(old), Some(Value::String(new))) => {
                        names.insert(old.clone(), new.clone());
                    }
                    (_, field, Some(other)) => binding_names(field, other, names),
                    _ => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => a.iter().zip(b).for_each(|(a, b)| binding_names(a, b, names)),
        _ => {}
    }
}

/// Appends `entry` to the entries waiting for `list`.
fn grow<'d, 'i>(lists: &mut Vec<(Node<'d, 'i>, usize, String)>, list: Node<'d, 'i>, entry: String) {
    match lists.iter_mut().find(|(known, ..)| known.id() == list.id()) {
        Some((_, count, entries)) => {
            *count += 1;
            entries.push_str(&entry);
        }
        None => lists.push((list, 1, entry)),
    }
}

/// The copy's ID: `id` from the request, or the first free `<id>_copy`, `<id>_copy2`, ... of `list`.
fn copy_id(list: &[Value], original: &Value, request: &Value) -> Result<String, String> {
    let used: HashSet<&str> = list.iter().filter_map(|entry| entry["id"].as_str()).collect();
    let id = match request["id"].as_str() {
        Some(id) => id.to_owned(),
        None => {
            let base = original["id"].as_str().ok_or("the original has no ID")?;
            (1..).map(|n| if n == 1 { format!("{base}_copy") } else { format!("{base}_copy{n}") })
                .find(|id| !used.contains(id.as_str())).unwrap_or_default()
        }
    };
    if id.is_empty() || id.len() > 63 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return Err("id must be 1 to 63 ASCII letters, digits or underscores".into());
    }
    if used.contains(id.as_str()) {
        return Err(format!("id {id} is already used"));
    }
    Ok(id)
}

/// A copy of a parameter that no source keys and the physics settings do not use, at the end of the parameters
/// and in the original's group.
fn duplicate_parameter(
    model: &Value,
    guid: &str,
    request: &Value,
    fresh: &mut dyn FnMut() -> Result<String, String>,
) -> Result<(Value, Copy), String> {
    let parameters = model["parameters"].as_array().ok_or("authoring source model lacks parameters")?;
    let original = parameters.iter().find(|parameter| parameter["guid"] == guid).ok_or("the parameter was not found")?;
    let id = original["id"].as_str().unwrap_or_default();
    let keyed = model["sources"].as_array().into_iter().flatten().any(|source| crate::authoring_delete::mentions(source, &[guid]));
    if keyed || model["physicsSource"].as_str().is_some_and(|physics| physics.contains(guid) || physics.contains(id)) {
        return Err(format!("parameter {id} is keyed or used by the physics settings; copy an unused parameter"));
    }
    let new_id = copy_id(parameters, original, request)?;
    let new_guid = fresh()?;
    let mut copy = original.clone();
    copy["guid"] = json!(new_guid);
    copy["id"] = json!(new_id);
    let mut result = model.clone();
    result["parameters"].as_array_mut().ok_or("authoring source model lacks parameters")?.push(copy);
    Ok((result, Copy { template: guid.to_owned(), guids: vec![(guid.to_owned(), new_guid)] }))
}

fn inside(node: Node<'_, '_>, roots: &[Node<'_, '_>]) -> bool {
    node.ancestors().any(|ancestor| roots.iter().any(|root| root.id() == ancestor.id()))
}

/// A GUID for an inner definition of a copy that the model does not show; derived, so every save writes the same.
fn derived_guid(copy: &str, original: &str) -> String {
    let mut bytes = [0u8; 16];
    for (half, salt) in bytes.chunks_mut(8).zip([0u8, 1]) {
        let mut hasher = std::hash::DefaultHasher::new();
        (copy, original, salt).hash(&mut hasher);
        half.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// The identifiers of a copy: new `xs.id`s and GUIDs, and the ID string its own ID element takes.
struct Renames<'a> {
    ids: HashMap<String, String>,
    uuids: HashMap<String, String>,
    id_element: Option<roxmltree::NodeId>,
    id: &'a str,
    /// The last `xs.idx` serial in use; every stored object of a Cubism file carries a distinct one.
    idx: std::cell::Cell<u64>,
}

/// `node`'s text with every `xs.id`, `xs.ref` and `uuid` renamed, fresh `xs.idx` serials, and the copy's own `idstr`.
fn substituted(text: &str, node: Node<'_, '_>, renames: &Renames<'_>) -> Result<String, String> {
    let range = node.range();
    let mut out = String::with_capacity(range.len());
    let mut at = range.start;
    for element in node.descendants().filter(|element| element.is_element()) {
        for attribute in element.attributes() {
            let new = match attribute.name() {
                "xs.id" | "xs.ref" => renames.ids.get(attribute.value()),
                "uuid" => renames.uuids.get(attribute.value()),
                "idstr" if Some(element.id()) == renames.id_element => Some(&renames.id.to_owned()),
                _ => None,
            };
            let new = match attribute.name() {
                "xs.idx" => {
                    renames.idx.set(renames.idx.get() + 1);
                    Some(renames.idx.get().to_string())
                }
                _ => new.cloned(),
            };
            let Some(new) = new else { continue };
            let value = attribute.range_value();
            if text.get(value.clone()) != Some(attribute.value()) {
                return Err("a copied identifier is stored escaped".into());
            }
            out.push_str(&text[at..value.start]);
            out.push_str(&new);
            at = value.end;
        }
    }
    out.push_str(&text[at..range.end]);
    Ok(out)
}

/// The GUID objects that `owned` names in a `guid` field.
fn identity_objects(owned: &[Node<'_, '_>], by_id: &HashMap<&str, Node<'_, '_>>) -> HashSet<roxmltree::NodeId> {
    owned.iter().flat_map(|root| root.descendants())
        .filter(|node| node.attribute("xs.n") == Some("guid"))
        .filter_map(|node| by_id.get(node.attribute("xs.ref")?).map(|guid| guid.id())).collect()
}

/// `roots` and the top-level objects they own: every object they reach through references, short of those `stop`
/// accepts, that nothing outside `roots`, `context` and the owned objects refers to. With `identity`, the GUID
/// objects of owned objects are owned whatever else refers to them. Cubism files keep such objects apart: GUIDs,
/// extensions that name their owner back, keyform grids and bindings.
pub(crate) fn owned_objects<'d, 'i>(
    roots: &[Node<'d, 'i>],
    context: &[Node<'d, 'i>],
    by_id: &HashMap<&str, Node<'d, 'i>>,
    refs: &HashMap<&str, Vec<Node<'d, 'i>>>,
    stop: &dyn Fn(Node<'d, 'i>) -> bool,
    identity: bool,
    budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let mut owned = roots.to_vec();
    let mut next = 0;
    while next < owned.len() {
        let targets: Vec<&str> = owned[next].descendants().filter_map(|node| node.attribute("xs.ref")).collect();
        next += 1;
        for target in targets {
            let Some(&node) = by_id.get(target) else { return Err(format!("xs.ref {target} has no definition")) };
            // Objects are cloned or removed in place, which only a top-level object allows.
            let top = node.parent_element().is_some_and(|parent| parent.has_tag_name("shared") || parent.parent_element().is_none());
            if top && !inside(node, &owned) && !stop(node) {
                owned.push(node);
                budget.consume(1)?;
            }
        }
    }
    loop {
        let guids = if identity { identity_objects(&owned, by_id) } else { HashSet::new() };
        let kept: Vec<Node<'d, 'i>> = owned.iter().enumerate().filter(|(index, node)| {
            *index < roots.len() || guids.contains(&node.id())
                || node.attribute("xs.id").and_then(|id| refs.get(id)).into_iter().flatten()
                    .all(|reference| inside(*reference, &owned) || inside(*reference, context))
        }).map(|(_, node)| *node).collect();
        if kept.len() == owned.len() {
            return Ok(owned);
        }
        owned = kept;
    }
}

/// The opened file with every copied source of `model` cloned in, or `None` when the model adds no copy.
pub(crate) fn insert(container: &CaffDocument, initial: &Value, model: &Value, copies: &[Copy]) -> Result<Option<CaffDocument>, String> {
    let entries = |model: &'_ Value| ["sources", "parameters", "brushes"].map(|list| model[list].as_array().cloned().unwrap_or_default()).concat();
    let (initial_entries, model_entries) = (entries(initial), entries(model));
    let known: HashSet<&str> = initial_entries.iter().filter_map(|s| s["guid"].as_str()).collect();
    let added: Vec<&Copy> = model_entries.iter()
        .filter_map(|source| source["guid"].as_str().filter(|guid| !known.contains(guid)))
        .filter_map(|guid| copies.iter().find(|copy| copy.guid() == guid))
        .collect();
    if added.is_empty() {
        return Ok(None);
    }
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml).map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let document = Document::parse_with_options(text, ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() })
        .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let graph = Graph::new(&document)?;
    let mut budget = Budget::new(10_000_000);
    let root = graph.model()?.ok_or("authoring source contains no model")?;
    let sources = source_nodes(&graph, root)?;
    let mut definitions: Vec<Node<'_, '_>> = sources.values().copied().collect();
    definitions.extend(crate::authoring_edit::brush_nodes(&graph, root)?);
    let mut by_id: HashMap<&str, Node<'_, '_>> = HashMap::new();
    let mut refs: HashMap<&str, Vec<Node<'_, '_>>> = HashMap::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        budget.consume(1)?;
        if let Some(id) = node.attribute("xs.id") {
            by_id.insert(id, node);
        }
        if let Some(id) = node.attribute("xs.ref") {
            refs.entry(id).or_default().push(node);
        }
    }
    let entities: HashSet<&str> = ["sources", "parameters", "textures", "brushes"].iter()
        .flat_map(|list| initial[*list].as_array().into_iter().flatten())
        .filter_map(|entry| entry["guid"].as_str()).collect();
    let mut taken: HashSet<String> = by_id.keys().map(|id| (*id).to_owned()).collect();
    let mut next_idx = document.descendants().filter_map(|node| node.attribute("xs.idx")?.parse::<u64>().ok()).max().unwrap_or(0);
    let mut patches = Vec::new();
    let mut lists: Vec<(Node<'_, '_>, usize, String)> = Vec::new();
    for copy in added {
        let parameter = initial["parameters"].as_array().into_iter().flatten().any(|p| p["guid"] == copy.template.as_str());
        let brush = initial["brushes"].as_array().into_iter().flatten().find(|p| p["guid"] == copy.template.as_str());
        let set = if brush.is_some() {
            "artPathBrushesSetting"
        } else if parameter {
            "parameterSourceSet"
        } else {
            let source = initial["sources"].as_array().into_iter().flatten().find(|s| s["guid"] == copy.template.as_str())
                .ok_or("the original of a copy is not in the opened model")?;
            set_of(source).ok_or("this kind of source cannot be copied")?
        };
        let holder = graph.field(root, set)?.ok_or("a source set is missing")?;
        let set_list = graph.field(holder, if brush.is_some() { "brushes" } else { "_sources" })?
            .ok_or("the copied object's list is missing")?;
        let mut set_entries = Vec::new();
        for entry in set_list.children().filter(|entry| entry.is_element()) {
            let definition = graph.resolve(entry)?;
            if graph.guid(definition)? == Some(copy.template.as_str()) {
                set_entries.push((entry, definition));
            }
        }
        let [(set_entry, template)] = set_entries[..] else { return Err("the source set does not list the original once".into()) };
        // The lists that name the original besides its set: the parent Part's children, or a parameter's group.
        let mut members = Vec::new();
        if parameter {
            if let Some(list) = graph.field(root, "parameterGroupSet")?.map(|groups| graph.field(groups, "_groups")).transpose()?.flatten() {
                for group in values::list(&graph, list, &mut budget)? {
                    if let Some((list, entries)) = named_entries(&graph, group, "_childGuids", &copy.template)? {
                        members.extend(entries.into_iter().map(|entry| (list, entry)));
                    }
                }
            }
            if members.len() > 1 {
                return Err("the parameter is listed by more than one group".into());
            }
        } else if brush.is_none() {
            let parent = initial["sources"].as_array().into_iter().flatten().find(|s| s["guid"] == copy.template.as_str())
                .and_then(|source| source["parentPartGuid"].as_str()).and_then(|guid| sources.get(guid).copied())
                .ok_or("the original's parent Part is not in the file")?;
            let (children, entries) = named_entries(&graph, parent, "_childGuids", &copy.template)?
                .ok_or("the parent Part stores no _childGuids list")?;
            let [entry] = entries[..] else { return Err("the parent Part does not list the original once".into()) };
            members.push((children, entry));
        }
        let entries: Vec<Node<'_, '_>> = std::iter::once(set_entry).chain(members.iter().map(|(_, entry)| *entry)).collect();
        // Objects the model knows by GUID (textures, parameters, other sources) stay shared.
        let entity = |node: Node<'_, '_>| node.descendants().any(|inner| {
            inner.attribute("xs.n") == Some("guid")
                && graph.resolve(inner).ok().and_then(|guid| guid.attribute("uuid")).is_some_and(|uuid| entities.contains(uuid))
        });
        let stop = |node: Node<'_, '_>| definitions.iter().any(|d| d.id() == node.id()) || node.id() == root.id() || entity(node);
        // The copy also takes the GUID objects of what it owns: the copy gets new ones, and references from
        // elsewhere (an option list naming the original) stay with the original.
        let mut roots = vec![template];
        if brush.is_some() {
            if let Some(texture) = graph.field(template, "textureInput")?
                .map(|input| graph.field(input, "texture")).transpose()?.flatten()
                .filter(|texture| !texture.has_tag_name("null")) {
                if !inside(texture, &roots) {
                    let owner = texture.ancestors().find(|node| node.parent_element().is_some_and(|parent|
                        parent.has_tag_name("shared") || parent.parent_element().is_none()))
                        .ok_or("the brush texture has no standalone owner")?;
                    if owner == root || definitions.iter().any(|definition| inside(*definition, &[owner])) {
                        return Err("the brush texture is embedded in another model object".into());
                    }
                    roots.push(owner);
                }
            }
        }
        let owned = owned_objects(&roots, &entries, &by_id, &refs, &stop, true, &mut budget)?;
        let guids = identity_objects(&owned, &by_id);
        let elsewhere = |node: Node<'_, '_>| {
            !inside(node, &owned) && !entries.iter().any(|entry| inside(node, &[*entry]))
                && !definitions.iter().any(|definition| definition.id() != template.id() && inside(node, &[*definition]))
        };
        let mut bindings = HashMap::new();
        let original = initial_entries.iter().find(|entry| entry["guid"] == copy.template.as_str());
        let entry = model_entries.iter().find(|entry| entry["guid"] == copy.guid());
        if let (Some(original), Some(entry)) = (original, entry) {
            binding_names(original, entry, &mut bindings);
        }
        let mut ids = HashMap::new();
        for root in owned.iter().chain(&entries) {
            let identity = guids.contains(&root.id());
            for id in root.descendants().filter_map(|node| node.attribute("xs.id")) {
                if !identity && refs.get(id).into_iter().flatten().any(|reference| elsewhere(*reference)) {
                    return Err("the file refers to this source from outside the sources; it cannot be copied".into());
                }
                if let Some(new) = bindings.get(id) {
                    if !taken.insert(new.clone()) {
                        return Err(format!("the copy's binding name {new} is already used in the file"));
                    }
                    ids.insert(id.to_owned(), new.clone());
                    continue;
                }
                let new = (1..).map(|n| format!("{id}-copy{n}")).find(|new| !taken.contains(new)).unwrap_or_default();
                taken.insert(new.clone());
                ids.insert(id.to_owned(), new);
            }
        }
        let image_guid = brush.and_then(|brush| brush["textureInput"]["currentTextureInputData"]["modelImageGuid"].as_str());
        let defined: HashSet<&str> = owned.iter().flat_map(|root| root.descendants())
            .filter(|node| node.attribute("xs.n") == Some("guid") || guids.contains(&node.id()))
            .filter_map(|node| node.attribute("uuid")).filter(|guid| Some(*guid) != image_guid).collect();
        if !defined.contains(copy.template.as_str()) {
            return Err("the original's GUID is not stored with it".into());
        }
        if document.descendants().any(|node| node.attribute("uuid").is_some_and(|uuid| defined.contains(uuid)) && elsewhere(node)
            && !guids.contains(&node.id())) {
            return Err("the file names this source from outside the sources; it cannot be copied".into());
        }
        let id = model_entries.iter().find(|entry| entry["guid"] == copy.guid()).and_then(|entry| entry["id"].as_str())
            .ok_or("a copy has no ID")?;
        let id_element = graph.field(template, "id")?.filter(|element| inside(*element, &owned)).map(|element| element.id());
        let uuids: HashMap<String, String> = defined.iter().map(|old| {
            let new = copy.guids.iter().find(|(original, _)| original == old).map(|(_, new)| new.clone())
                .unwrap_or_else(|| derived_guid(copy.guid(), old));
            ((*old).to_owned(), new)
        }).collect();
        let renames = Renames { ids, uuids, id_element, id, idx: std::cell::Cell::new(next_idx) };
        for node in &owned {
            if node.id() == set_entry.id() {
                continue;
            }
            let at = node.range().end;
            patches.push(XmlPatch { range: at..at, value: substituted(text, *node, &renames)? });
        }
        // An inline definition is its own set entry, so the copy itself goes to the end of the list.
        grow(&mut lists, set_list, substituted(text, set_entry, &renames)?);
        for (list, entry) in &members {
            grow(&mut lists, *list, substituted(text, *entry, &renames)?);
        }
        next_idx = renames.idx.get();
    }
    for (list, added, entries) in lists {
        let count = values::list(&graph, list, &mut budget)?.len() + added;
        if list.attribute("count").is_some() {
            patches.push(attribute_patch(text, list, "count", count.to_string())?);
        }
        patches.push(append_patch(text, list, entries)?);
    }
    patches.sort_by_key(|patch| patch.range.start);
    if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
        return Err("copied source ranges overlap".into());
    }
    let mut rewritten = text.to_owned();
    for patch in patches.into_iter().rev() {
        rewritten.replace_range(patch.range, &patch.value);
    }
    Ok(Some(CaffDocument::parse(&container.replace_main_xml(rewritten.as_bytes())?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring_edit::{rewrite_with_copies, set_vertex, tests::test_container};
    use crate::authoring_scene;

    fn counter() -> impl FnMut() -> Result<String, String> {
        let mut n = 0;
        move || {
            n += 1;
            Ok(format!("00000000-0000-4000-8000-{n:012}"))
        }
    }

    #[test]
    fn brush_copies_preserve_image_data_and_copy_current_settings() {
        let base = crate::authoring_edit::tests::artmesh_xml();
        let start = base.find("<GTexture2D xs.id='texture'>").unwrap();
        let end = start + base[start..].find("</GTexture2D>").unwrap() + "</GTexture2D>".len();
        let inline = base[start..end].replace("xs.id='texture'", "xs.id='brush-texture' xs.n='texture'")
            .replace("uuid='texture-guid'", "uuid='brush-texture-guid'");
        for (texture, referenced_input) in [
            ("<GTexture2D xs.n='texture' xs.ref='texture'/>".to_owned(), false),
            (inline.clone(), false),
            (inline, true),
        ] {
            let input = format!("<CArtPathBrush_TextureInput {}>{texture}\
                <CTextureInput_ModelImage xs.n='currentTextureInputData'>\
                <CModelImageGuid xs.n='_modelImageGuid' uuid='image-guid'/>\
                <CArtPathBrush xs.n='_owner' xs.ref='brush'/></CTextureInput_ModelImage>\
                </CArtPathBrush_TextureInput>",
                if referenced_input { "xs.id='brush-input'" } else { "xs.n='textureInput'" });
            let field = if referenced_input {
                "<CArtPathBrush_TextureInput xs.n='textureInput' xs.ref='brush-input'/>"
            } else {
                &input
            };
            let original = base.replace("<CPartSource xs.n='rootPart' xs.ref='part'/>", &format!(
                "<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
                 <carray_list xs.n='brushes' count='1'><CArtPathBrush xs.id='brush'>\
                 <CArtPathBrushGuid xs.n='guid' uuid='brush-guid'/><CArtPathBrushId xs.n='id' idstr='Brush'/>\
                 <s xs.n='name'>Pencil</s><s xs.n='unknown'>keep this</s><f xs.n='widthCoefficient'>1</f>\
                 {field}</CArtPathBrush></carray_list></CArtPathBrushSetting>"));
            let original = if referenced_input {
                original.replace("</root>", &format!("{input}</root>"))
            } else {
                original
            };
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let mut fresh = counter();
            let (copied, copy) = duplicate(&initial, &json!({"guid":"brush-guid"}), &[], &mut fresh).unwrap();
            let copies = vec![copy.clone()];
            let bytes = rewrite_with_copies(&container, &initial, &copied, &copies).unwrap();
            let reopened = CaffDocument::parse(&bytes).unwrap();
            assert_eq!(authoring_scene::read(&reopened).unwrap(), copied);
            assert_eq!(String::from_utf8(reopened.main_xml()).unwrap().matches("keep this").count(), 2);
            assert_eq!(copied["brushes"][1]["textureInput"]["texture"]["imageEntry"], "mesh.png");
            assert_eq!(copied["brushes"][1]["textureInput"]["currentTextureInputData"]["modelImageGuid"], "image-guid");
            let edited = crate::authoring_fields::set_brush(&copied,
                &json!({"guid":copy.guid(),"widthCoefficient":2})).unwrap();
            let (twice, second) = duplicate(&edited, &json!({"guid":copy.guid()}), &copies, &mut fresh).unwrap();
            let copies = [copy.clone(), second.clone()];
            let bytes = rewrite_with_copies(&container, &initial, &twice, &copies).unwrap();
            assert_eq!(authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), twice);
            assert_eq!(twice["brushes"][2]["widthCoefficient"].as_f64(), Some(2.0));
            for guid in ["brush-guid", copy.guid()] {
                let deleted = crate::authoring_delete::delete(&twice, &json!({"guid":guid})).unwrap();
                let bytes = rewrite_with_copies(&container, &initial, &deleted, &copies).unwrap();
                assert_eq!(authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), deleted);
            }
            let cancelled = crate::authoring_delete::delete(&copied, &json!({"guid":copy.guid()})).unwrap();
            assert_eq!(rewrite_with_copies(&container, &initial, &cancelled, &[copy]).unwrap(), container.encode());
            assert!(duplicate(&initial, &json!({"guid":"brush-guid","id":"Brush"}), &[], &mut fresh).is_err());
            if !referenced_input && texture.contains("xs.id='brush-texture'") {
                let start = original.find("<CArtPathBrushSetting").unwrap();
                let end = original.find("</CArtPathBrushSetting>").unwrap() + "</CArtPathBrushSetting>".len();
                let setting = original[start..end].replace("xs.n='artPathBrushesSetting'", "xs.id='brush-setting'")
                    .replace("xs.n='brushes' count='1'", "xs.n='brushes' count='2'")
                    .replace("</carray_list></CArtPathBrushSetting>", "<CArtPathBrush>\
                        <CArtPathBrushGuid xs.n='guid' uuid='other-brush-guid'/>\
                        <CArtPathBrushId xs.n='id' idstr='OtherBrush'/>\
                        <CArtPathBrush_TextureInput xs.n='textureInput'>\
                        <GTexture2D xs.n='texture' xs.ref='brush-texture'/>\
                        </CArtPathBrush_TextureInput></CArtPathBrush></carray_list></CArtPathBrushSetting>");
                let mut shared = original.clone();
                shared.replace_range(start..end,
                    "<CArtPathBrushSetting xs.n='artPathBrushesSetting' xs.ref='brush-setting'/>");
                shared = shared.replace("</root>", &format!("{setting}</root>"));
                let container = test_container(&shared);
                let initial = authoring_scene::read(&container).unwrap();
                let (copied, copy) = duplicate(&initial, &json!({"guid":"other-brush-guid"}), &[], &mut fresh).unwrap();
                assert!(rewrite_with_copies(&container, &initial, &copied, &[copy]).unwrap_err()
                    .contains("the brush texture is embedded in another model object"));
            }
        }
    }

    #[test]
    fn a_copy_takes_new_guid_objects_extensions_bindings_and_serials_as_cubism_files_store_them() {
        // Shaped like a Cubism file: the GUID is a shared object an option list also names, an extension names its
        // owner, the keyform binding is a separate object, and every object carries an xs.idx serial.
        let original = crate::authoring_grid::tests::keyed_xml()
            .replace("<CArtMeshGuid xs.n='guid' uuid='mesh-guid'/>", "<CArtMeshGuid xs.n='guid' xs.ref='mesh-guid-object'/>\
                <carray_list xs.n='_extensions' count='1'><CTextureInputExtension xs.ref='extension'/></carray_list>")
            .replace("<CArtMeshSource xs.id='mesh'>", "<CArtMeshGuid xs.id='mesh-guid-object' xs.idx='7' uuid='mesh-guid'/>\
                <CTextureInputExtension xs.id='extension' xs.idx='8'><CArtMeshSource xs.n='_owner' xs.ref='mesh'/></CTextureInputExtension>\
                <CArtMeshSource xs.id='mesh' xs.idx='9'>")
            .replace("<CPartId xs.n='id' idstr='root'/>", "<CPartId xs.n='id' idstr='root'/>\
                <carray_list xs.n='_childGuids' count='1'><CArtMeshGuid xs.ref='mesh-guid-object'/></carray_list>")
            .replace("<CPartSource xs.n='rootPart' xs.ref='part'/>", "<CPartSource xs.n='rootPart' xs.ref='part'/>\
                <hash_map xs.n='modelOptions'><CArtMeshGuid xs.ref='mesh-guid-object'/></hash_map>");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let (copied, copy) = duplicate(&initial, &json!({"guid": "mesh-guid"}), &[], &mut counter()).unwrap();
        let binding = copied["sources"][1]["grid"]["axes"][0]["bindingRef"].as_str().unwrap().to_owned();
        assert_eq!(binding, format!("binding-{}", &copy.guid()[..8]));
        let bytes = rewrite_with_copies(&container, &initial, &copied, &[copy.clone()]).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), copied);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        assert!(xml.contains(&format!("<CArtMeshGuid xs.id='mesh-guid-object-copy1' xs.idx='11' uuid='{}'/>", copy.guid())), "{xml}");
        assert!(xml.contains("<CTextureInputExtension xs.id='extension-copy1' xs.idx='12'><CArtMeshSource xs.n='_owner' xs.ref='mesh-copy1'/>"));
        assert!(xml.contains("<CArtMeshSource xs.id='mesh-copy1' xs.idx='10'>"));
        assert!(xml.contains(&format!("<KeyformBindingSource xs.id='{binding}'>")));
        assert!(xml.contains("<hash_map xs.n='modelOptions'><CArtMeshGuid xs.ref='mesh-guid-object'/></hash_map>"), "options keep the original");
        assert!(xml.contains("<CArtMeshGuid xs.ref='mesh-guid-object'/><CArtMeshGuid xs.ref='mesh-guid-object-copy1'/></carray_list>"));
    }

    #[test]
    fn a_copied_source_is_written_from_the_original_and_takes_later_edits() {
        let original = crate::authoring_reparent::tests::xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let mut fresh = counter();
        let (copied, copy) = duplicate(&initial, &json!({"guid": "mesh-guid"}), &[], &mut fresh).unwrap();
        let guid = copy.guid().to_owned();
        assert_eq!(copied["sources"][1]["id"], "mesh_copy");
        let form = copied["sources"][1]["forms"][0]["guid"].as_str().unwrap().to_owned();
        assert_ne!(form, "form-guid");
        let bytes = rewrite_with_copies(&container, &initial, &copied, &[copy.clone()]).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), copied);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        assert_eq!(xml.matches("<GTexture2D xs.id=").count(), 1, "the texture stays shared");
        assert!(xml.contains("<CArtMeshSource xs.id='mesh-copy1'>"), "{xml}");
        assert!(xml.contains("count='3'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/><CArtMeshGuid uuid='"));

        // An unsaved copy takes vertex edits, and a copy of it is cloned from the same original.
        let moved = set_vertex(&copied, &json!({"sourceGuid": guid, "formGuid": form, "coordinateType": "Canvas",
            "vertexIndex": 0, "position": [9.5, 8.25]})).unwrap();
        let (twice, second) = duplicate(&moved, &json!({"guid": guid, "id": "MeshThree"}), &[copy.clone()], &mut fresh).unwrap();
        assert_eq!(second.template, "mesh-guid");
        assert_eq!(twice["sources"][2]["forms"][0]["data"]["positions"][0], 9.5);
        let bytes = rewrite_with_copies(&container, &initial, &twice, &[copy, second]).unwrap();
        assert_eq!(authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), twice);

        let (part, part_copy) = duplicate(&initial, &json!({"guid": "arm-guid"}), &[], &mut fresh).unwrap();
        let bytes = rewrite_with_copies(&container, &initial, &part, &[part_copy]).unwrap();
        assert_eq!(authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), part);

        assert!(duplicate(&initial, &json!({"guid": "part-guid"}), &[], &mut fresh).unwrap_err().contains("root"));
        assert!(duplicate(&initial, &json!({"guid": "mesh-guid", "id": "arm"}), &[], &mut fresh).unwrap_err().contains("already"));
        let named = original.replace("<CPartSource xs.n='rootPart' xs.ref='part'/>",
            "<CPartSource xs.n='rootPart' xs.ref='part'/><CArtMeshSource xs.n='favourite' xs.ref='mesh'/>");
        let named_container = test_container(&named);
        let named_initial = authoring_scene::read(&named_container).unwrap();
        let (model, copy) = duplicate(&named_initial, &json!({"guid": "mesh-guid"}), &[], &mut fresh).unwrap();
        let refused = rewrite_with_copies(&named_container, &named_initial, &model, &[copy]).unwrap_err();
        assert!(refused.contains("outside the sources"), "{refused}");
    }

    #[test]
    fn a_delete_composes_with_an_unsaved_copy() {
        use crate::authoring_delete::delete;
        let original = crate::authoring_reparent::tests::xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let (copied, copy) = duplicate(&initial, &json!({"guid": "mesh-guid"}), &[], &mut counter()).unwrap();
        let written = |model: &Value| {
            let bytes = rewrite_with_copies(&container, &initial, model, &[copy.clone()]).unwrap();
            let reopened = CaffDocument::parse(&bytes).unwrap();
            assert_eq!(&authoring_scene::read(&reopened).unwrap(), model);
            String::from_utf8(reopened.main_xml()).unwrap()
        };

        // (1) The empty Part next to the copied ArtMesh is deleted.
        let xml = written(&delete(&copied, &json!({"guid": "arm-guid"})).unwrap());
        assert!(!xml.contains("arm-guid") && xml.contains("<CArtMeshSource xs.id='mesh-copy1'>"), "{xml}");

        // (2) The original ArtMesh is deleted and the copy stays, with the shared texture.
        let xml = written(&delete(&copied, &json!({"guid": "mesh-guid"})).unwrap());
        assert!(!xml.contains("uuid='mesh-guid'") && xml.contains("<CArtMeshSource xs.id='mesh-copy1'>"), "{xml}");
        assert!(xml.contains("<GTexture2D xs.id='texture'>"), "{xml}");

        // (3) The copy moves under the Part and the original is deleted: copy, move and delete in one write.
        let moved = crate::authoring_reparent::reparent(
            &copied, &json!({"guid": copy.guid(), "parent": "arm-guid"})).unwrap();
        let model = delete(&moved, &json!({"guid": "mesh-guid"})).unwrap();
        assert!(!model["textures"].as_array().unwrap().is_empty());
        let xml = written(&model);
        assert!(!xml.contains("uuid='mesh-guid'") && xml.contains("<GTexture2D xs.id='texture'>"), "{xml}");
        assert!(xml.contains("<carray_list xs.n='_childGuids' count='1'><CArtMeshGuid uuid='"), "{xml}");

        // (4) Each ArtMesh has a texture of its own: once the original goes, the copy, listed last, is the first to
        // use its texture, so that texture moves to the end of the list.
        let mesh_start = original.find("<CArtMeshSource xs.id='mesh'>").unwrap();
        let mesh_end = mesh_start + original[mesh_start..].find("</CArtMeshSource>").unwrap() + "</CArtMeshSource>".len();
        let other_mesh = original[mesh_start..mesh_end].replace("'mesh'", "'other'").replace("mesh-guid", "other-guid")
            .replace("form-guid", "other-form").replace("xs.ref='texture'", "xs.ref='texture2'");
        let texture_start = original.find("<GTexture2D xs.id='texture'>").unwrap();
        let other_texture = original[texture_start..original.len() - "</root>".len()]
            .replace("'texture'", "'texture2'").replace("texture-guid", "texture2-guid");
        let two = original
            .replace("count='1'><CArtMeshSource xs.ref='mesh'/>", "count='2'><CArtMeshSource xs.ref='mesh'/><CArtMeshSource xs.ref='other'/>")
            .replace("count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/>",
                "count='3'><CArtMeshGuid uuid='mesh-guid'/><CArtMeshGuid uuid='other-guid'/><CPartGuid uuid='arm-guid'/>")
            .replace("</CArtMeshSource>", &format!("</CArtMeshSource>{other_mesh}"))
            .replace("</root>", &format!("{other_texture}</root>"));
        let container = test_container(&two);
        let initial = authoring_scene::read(&container).unwrap();
        let (copied, copy) = duplicate(&initial, &json!({"guid": "mesh-guid"}), &[], &mut counter()).unwrap();
        let model = delete(&copied, &json!({"guid": "mesh-guid"})).unwrap();
        let order: Vec<&Value> = model["textures"].as_array().unwrap().iter().map(|texture| &texture["guid"]).collect();
        assert_eq!(order, [&json!("texture2-guid"), &json!("texture-guid")]);
        let bytes = rewrite_with_copies(&container, &initial, &model, &[copy]).unwrap();
        assert_eq!(authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), model);
    }

    #[test]
    fn an_unused_parameter_is_copied_into_its_group() {
        let parameter = "<CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/><CParameterId xs.n='id' idstr='ParamSpare'/>\
            <f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource>";
        let groups = "<CParameterGroupSet xs.n='parameterGroupSet'><carray_list xs.n='_groups' count='1'><CParameterGroup>\
            <CParameterGroupGuid xs.n='guid' uuid='group-guid'/><null xs.n='parentGroupGuid'/>\
            <carray_list xs.n='_childGuids' count='1'><CParameterGuid uuid='parameter-guid'/></carray_list></CParameterGroup>\
            </carray_list></CParameterGroupSet>";
        let original = crate::authoring_reparent::tests::xml().replace(
            "<carray_list xs.n='_sources' count='0'/></CParameterSourceSet>",
            &format!("<carray_list xs.n='_sources' count='1'>{parameter}</carray_list></CParameterSourceSet>{groups}"));
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let (model, copy) = duplicate(&initial, &json!({"guid": "parameter-guid"}), &[], &mut counter()).unwrap();
        assert_eq!(model["parameters"][1]["id"], "ParamSpare_copy");
        let guid = copy.guid().to_owned();
        let renamed = crate::authoring_edit::set_id(&model, &json!({"guid": guid, "id": "ParamNew"})).unwrap();
        let bytes = rewrite_with_copies(&container, &initial, &renamed, &[copy]).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), renamed);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        assert_eq!(xml, original
            .replace(&format!("count='1'>{parameter}</carray_list>"), &format!("count='2'>{parameter}{}</carray_list>",
                parameter.replace("parameter-guid", &guid).replace("ParamSpare", "ParamNew")))
            .replace("count='1'><CParameterGuid uuid='parameter-guid'/></carray_list>",
                &format!("count='2'><CParameterGuid uuid='parameter-guid'/><CParameterGuid uuid='{guid}'/></carray_list>")));

        let mut physical = initial.clone();
        physical["physicsSource"] = json!("<CPhysicsSettingsSourceSet>ParamSpare</CPhysicsSettingsSourceSet>");
        assert!(duplicate(&physical, &json!({"guid": "parameter-guid"}), &[], &mut counter()).unwrap_err().contains("physics"));
    }
}
