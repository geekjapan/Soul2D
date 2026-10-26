//! Adding and deleting a key of a CMO3 keyform grid (issue #143). A new key gets one new form per combination of
//! the other axes, interpolated between its neighbours on the axis (or copied from the end form); deleting a key
//! drops those forms. `sync` makes the opened file's grid structure match the model before the usual passes run:
//! it rewrites the binding's keys, shifts the key indices of the cells, removes cells and forms, and writes new
//! cells and forms by cloning ones the source already stores, so their layout comes from the file. The form values
//! are then patched by `authoring_edit::rewrite`, which re-reads the result and refuses it unless it equals the
//! requested model.

use crate::authoring_edit::XmlPatch;
use crate::authoring_graph::Graph;
use crate::authoring_reparent::{append_patch, attribute_patch, source_nodes, stored_field};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// The source, keyed axis and keys a request names.
struct Target<'a> {
    index: usize,
    source: &'a Value,
    axis: usize,
    keys: Vec<f64>,
    cells: Vec<(Vec<usize>, String)>,
}

fn target<'a>(model: &'a Value, request: &Value) -> Result<Target<'a>, String> {
    let guid = request["sourceGuid"].as_str().filter(|guid| !guid.is_empty()).ok_or("sourceGuid must be a string")?;
    let parameter = request["parameterGuid"].as_str().filter(|guid| !guid.is_empty()).ok_or("parameterGuid must be a string")?;
    let sources = model["sources"].as_array().ok_or("authoring source model lacks sources")?;
    let index = sources.iter().position(|source| source["guid"] == guid).ok_or_else(|| format!("source {guid} was not found"))?;
    let source = &sources[index];
    if !matches!(source["data"]["type"].as_str(), Some("ArtMesh" | "Warp")) {
        return Err("keys can be added to and deleted from ArtMesh and warp deformer grids only".into());
    }
    if source["morphTargets"]["targets"].as_array().is_some_and(|targets| !targets.is_empty()) {
        return Err("the source has blend shapes; their keys are not edited here".into());
    }
    if source["extendedInterpolations"].as_array().is_some_and(|forms| !forms.is_empty()) {
        return Err("the source has generated extended interpolation forms, which would need rebuilding".into());
    }
    let axes = source["grid"]["axes"].as_array().ok_or("the source has no keyform grid")?;
    let axis = axes.iter().position(|axis| axis["parameterGuid"] == parameter).ok_or("the source is not keyed on that parameter")?;
    let keys = axes[axis]["keys"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
    let mut cells = Vec::new();
    for cell in source["grid"]["cells"].as_array().into_iter().flatten() {
        let indices = cell["indices"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|i| i as usize).collect();
        let form = cell["formGuid"].as_str().ok_or("a grid cell names no form")?;
        if source["forms"].as_array().into_iter().flatten().filter(|f| f["guid"] == form).count() != 1 {
            return Err("a grid cell does not name one form of the source".into());
        }
        cells.push((indices, form.to_owned()));
    }
    Ok(Target { index, source, axis, keys, cells })
}

fn requested_key(request: &Value) -> Result<f64, String> {
    request["key"].as_f64().filter(|key| (*key as f32).is_finite()).ok_or_else(|| "key must be a finite number".into())
}

fn with_grid(model: &Value, target: &Target<'_>, keys: Vec<f64>, cells: Vec<(Vec<usize>, String)>, forms: Vec<Value>) -> Value {
    let mut result = model.clone();
    let source = &mut result["sources"][target.index];
    source["grid"]["axes"][target.axis]["keys"] = json!(keys);
    source["grid"]["cells"] = cells.into_iter().map(|(indices, form)| json!({"indices": indices, "formGuid": form})).collect();
    source["forms"] = json!(forms);
    result
}

fn lerp(a: &Value, b: &Value, t: f64) -> Value {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => json!(a.as_f64().unwrap_or_default() * (1.0 - t) + b.as_f64().unwrap_or_default() * t),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a.iter().zip(b).map(|(a, b)| lerp(a, b, t)).collect(),
        _ => a.clone(),
    }
}

/// `live2d.keyform.add {sourceGuid, parameterGuid, key}`: a key inside the parameter's range on the source's axis
/// for that parameter. Each new form takes the vertices and opacity interpolated between its neighbours on the
/// axis, or those of the end form, and the other settings of the lower neighbour.
pub(crate) fn add_key(model: &Value, request: &Value, fresh: &mut dyn FnMut() -> Result<String, String>) -> Result<Value, String> {
    let target = target(model, request)?;
    let key = requested_key(request)?;
    let parameter = model["parameters"].as_array().into_iter().flatten().find(|p| p["guid"] == request["parameterGuid"])
        .ok_or("the parameter was not found")?;
    if !(parameter["minimum"].as_f64().unwrap_or(f64::NAN) <= key && key <= parameter["maximum"].as_f64().unwrap_or(f64::NAN)) {
        return Err("key must lie in the parameter's range".into());
    }
    if target.keys.contains(&key) {
        return Err(format!("the source already has a key at {key}"));
    }
    let at = target.keys.partition_point(|k| *k < key);
    let (lower, upper) = (at.checked_sub(1), (at < target.keys.len()).then_some(at));
    let forms = target.source["forms"].as_array().cloned().unwrap_or_default();
    let form = |guid: &str| forms.iter().find(|form| form["guid"] == guid).cloned().unwrap_or_default();
    let neighbour = |cell: &Vec<usize>, index: usize| {
        target.cells.iter().find(|(other, _)| other.iter().enumerate().all(|(axis, i)| if axis == target.axis { *i == index } else { *i == cell[axis] }))
    };
    let mut cells: Vec<(Vec<usize>, String)> = target.cells.iter()
        .map(|(indices, form)| {
            let mut indices = indices.clone();
            if indices[target.axis] >= at {
                indices[target.axis] += 1;
            }
            (indices, form.clone())
        }).collect();
    let mut new_forms = forms.clone();
    for (indices, _) in target.cells.iter().filter(|(indices, _)| Some(indices[target.axis]) == lower.or(upper)) {
        let below = lower.and_then(|index| neighbour(indices, index)).map(|(_, guid)| form(guid));
        let above = upper.and_then(|index| neighbour(indices, index)).map(|(_, guid)| form(guid));
        let mut made = match (&below, &above) {
            (Some(below), Some(above)) => {
                let t = (key - target.keys[at - 1]) / (target.keys[at] - target.keys[at - 1]);
                let mut made = below.clone();
                made["data"]["positions"] = lerp(&below["data"]["positions"], &above["data"]["positions"], t);
                made["opacity"] = lerp(&below["opacity"], &above["opacity"], t);
                made
            }
            (Some(only), None) | (None, Some(only)) => only.clone(),
            (None, None) => return Err("a grid cell has no neighbour on the axis".into()),
        };
        let guid = fresh()?;
        made["guid"] = json!(guid);
        new_forms.push(made);
        let mut new_indices = indices.clone();
        new_indices[target.axis] = at;
        cells.push((new_indices, guid));
    }
    let mut keys = target.keys.clone();
    keys.insert(at, key);
    Ok(with_grid(model, &target, keys, cells, new_forms))
}

/// `live2d.keyform.delete {sourceGuid, parameterGuid, key}`: removes one key of the source's axis for that
/// parameter and the forms on it. The last key of an axis stays.
pub(crate) fn delete_key(model: &Value, request: &Value) -> Result<Value, String> {
    let target = target(model, request)?;
    let key = requested_key(request)?;
    let at = target.keys.iter().position(|k| *k == key).ok_or_else(|| format!("the source has no key at {key}"))?;
    if target.keys.len() < 2 {
        return Err("the last key of an axis cannot be deleted".into());
    }
    let gone: HashSet<&str> = target.cells.iter().filter(|(indices, _)| indices[target.axis] == at).map(|(_, f)| f.as_str()).collect();
    let cells = target.cells.iter().filter(|(indices, _)| indices[target.axis] != at)
        .map(|(indices, form)| {
            let mut indices = indices.clone();
            if indices[target.axis] > at {
                indices[target.axis] -= 1;
            }
            (indices, form.clone())
        }).collect();
    let forms = target.source["forms"].as_array().into_iter().flatten()
        .filter(|form| !form["guid"].as_str().is_some_and(|guid| gone.contains(guid))).cloned().collect();
    let mut keys = target.keys.clone();
    keys.remove(at);
    Ok(with_grid(model, &target, keys, cells, forms))
}

/// The structure of a grid that `sync` writes: keys per axis and cells.
fn structure(source: &Value) -> (Vec<Value>, Vec<Value>) {
    let axes = source["grid"]["axes"].as_array().into_iter().flatten().map(|axis| axis["keys"].clone()).collect();
    (axes, source["grid"]["cells"].as_array().cloned().unwrap_or_default())
}

fn number_text(value: f64) -> Result<String, String> {
    if !(value as f32).is_finite() {
        return Err("a key exceeds the native float range".into());
    }
    Ok(value.to_string())
}

/// `node`'s text with `xs.id`/`xs.ref`/`uuid` attributes renamed, fresh `xs.idx` serials, and the texts `content`
/// answers for replaced.
pub(crate) fn cloned(
    text: &str,
    node: Node<'_, '_>,
    ids: &HashMap<String, String>,
    uuids: &HashMap<String, String>,
    next_idx: &mut u64,
    content: &dyn Fn(Node<'_, '_>) -> Option<String>,
) -> Result<String, String> {
    let range = node.range();
    let mut out = String::new();
    let mut at = range.start;
    let mut put = |span: std::ops::Range<usize>, new: &str, out: &mut String| {
        out.push_str(&text[at..span.start]);
        out.push_str(new);
        at = span.end;
    };
    for element in node.descendants().filter(|element| element.is_element()) {
        for attribute in element.attributes() {
            let new = match attribute.name() {
                "xs.id" | "xs.ref" => ids.get(attribute.value()).cloned(),
                "uuid" => uuids.get(attribute.value()).cloned(),
                // Every stored object of a Cubism file carries a distinct serial number; a clone takes a new one.
                "xs.idx" => {
                    *next_idx += 1;
                    Some(next_idx.to_string())
                }
                _ => None,
            };
            if let Some(new) = new {
                if text.get(attribute.range_value()) != Some(attribute.value()) {
                    return Err("a cloned identifier is stored escaped".into());
                }
                put(attribute.range_value(), &new, &mut out);
            }
        }
        if let Some(new) = content(element) {
            let child = element.first_child().filter(|child| child.is_text() && element.children().count() == 1)
                .ok_or("a cloned key index is not one text")?;
            put(child.range(), &new, &mut out);
        }
    }
    out.push_str(&text[at..range.end]);
    Ok(out)
}

/// Replaces the content of an inline key list (`array_list` of `f` or a `float-array`) and its count.
fn keys_patches(text: &str, graph: &Graph<'_, '_>, binding: Node<'_, '_>, keys: &[f64]) -> Result<Vec<XmlPatch>, String> {
    let list = stored_field(graph, binding, "keys")?.ok_or("a keyform binding stores no keys")?;
    if list.attribute("xs.ref").is_some() || list.attribute("xs.id").is_some() {
        return Err("a keyform binding's keys are shared".into());
    }
    let texts = keys.iter().map(|key| number_text(*key)).collect::<Result<Vec<_>, _>>()?;
    let (range, value) = if list.has_tag_name("array_list") {
        let items: Vec<Node<'_, '_>> = list.children().filter(|child| child.is_element()).collect();
        let (Some(first), Some(last)) = (items.first(), items.last()) else { return Err("a keyform binding has no keys".into()) };
        (first.range().start..last.range().end, texts.iter().map(|key| format!("<f>{key}</f>")).collect::<String>())
    } else if list.has_tag_name("float-array") {
        let child = list.first_child().filter(|child| child.is_text()).ok_or("a key array is not one text")?;
        (child.range(), texts.join(" "))
    } else {
        return Err("a keyform binding's keys are neither an array_list nor a float-array".into());
    };
    let mut patches = vec![XmlPatch { range, value }];
    if list.attribute("count").is_some() {
        patches.push(attribute_patch(text, list, "count", keys.len().to_string())?);
    }
    Ok(patches)
}

/// An object stored directly in the file's shared list (or under the root), the only place clones and removals
/// happen in place.
pub(crate) fn top_level(node: Node<'_, '_>) -> bool {
    node.parent_element().is_some_and(|parent| parent.has_tag_name("shared") || parent.parent_element().is_none())
}

pub(crate) fn inside(node: Node<'_, '_>, roots: &[Node<'_, '_>]) -> bool {
    node.ancestors().any(|ancestor| roots.iter().any(|root| root.id() == ancestor.id()))
}

/// The stored objects outside `roots` that only `roots` (and these objects) refer to, such as the shared GUID
/// element a Cubism file keeps for each form.
pub(crate) fn private_objects<'d, 'i>(
    roots: &[Node<'d, 'i>],
    by_id: &HashMap<&str, Node<'d, 'i>>,
    refs: &HashMap<&str, Vec<Node<'d, 'i>>>,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let mut owned = roots.to_vec();
    loop {
        let before = owned.len();
        let targets: Vec<&str> = owned.iter().flat_map(|root| root.descendants()).filter_map(|node| node.attribute("xs.ref")).collect();
        for target in targets {
            let node = *by_id.get(target).ok_or_else(|| format!("xs.ref {target} has no definition"))?;
            if !inside(node, &owned) && refs.get(target).into_iter().flatten().all(|reference| inside(*reference, &owned)) {
                if !top_level(node) {
                    return Err("an object a keyform uses is stored inside another object".into());
                }
                owned.push(node);
            }
        }
        if owned.len() == before {
            return Ok(owned.split_off(roots.len()));
        }
    }
}

/// A list stored inline on its owner: the grid lists are edited in place, so they must belong to one source alone.
pub(crate) fn owned<'d, 'i>(list: Option<Node<'d, 'i>>, name: &str) -> Result<Node<'d, 'i>, String> {
    list.filter(|list| list.attribute("xs.ref").is_none() && list.attribute("xs.id").is_none())
        .ok_or_else(|| format!("the source's {name} is not stored inline"))
}

/// The opened file with every grid whose keys or cells the model changed rewritten to the model's structure, or
/// `None` when no grid changed.
pub(crate) fn sync(container: &CaffDocument, initial: &Value, model: &Value) -> Result<Option<CaffDocument>, String> {
    let (Some(old_sources), Some(new_sources)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok(None);
    };
    let changed: Vec<(&Value, &Value)> = new_sources.iter()
        .filter_map(|new| Some((old_sources.iter().find(|old| old["guid"] == new["guid"])?, new)))
        .filter(|(old, new)| !old["grid"].is_null() && structure(old) != structure(new))
        .collect();
    if changed.is_empty() {
        return Ok(None);
    }
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml).map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let document = Document::parse_with_options(text, ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() })
        .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let graph = Graph::new(&document)?;
    let root = graph.model()?.ok_or("authoring source contains no model")?;
    let sources = source_nodes(&graph, root)?;
    let mut taken: HashSet<String> = document.descendants().filter_map(|node| node.attribute("xs.id")).map(str::to_owned).collect();
    let by_id: HashMap<&str, Node<'_, '_>> = document.descendants().filter_map(|node| Some((node.attribute("xs.id")?, node))).collect();
    let mut refs: HashMap<&str, Vec<Node<'_, '_>>> = HashMap::new();
    for node in document.descendants() {
        if let Some(target) = node.attribute("xs.ref") {
            refs.entry(target).or_default().push(node);
        }
    }
    let mut next_idx = document.descendants().filter_map(|node| node.attribute("xs.idx")?.parse::<u64>().ok()).max().unwrap_or(0);
    let mut patches = Vec::new();
    for (old, new) in changed {
        let guid = old["guid"].as_str().unwrap_or_default();
        let definition = *sources.get(guid).ok_or("a re-keyed source is not in the file")?;
        let (Some(old_axes), Some(new_axes)) = (old["grid"]["axes"].as_array(), new["grid"]["axes"].as_array()) else {
            return Err("a keyform grid lost its axes".into());
        };
        if old_axes.len() != new_axes.len()
            || old_axes.iter().zip(new_axes).any(|(a, b)| a["parameterGuid"] != b["parameterGuid"] || a["bindingRef"] != b["bindingRef"])
        {
            return Err("only the keys of a grid's axes can change, not the axes".into());
        }
        let grid = graph.field(definition, "keyformGridSource")?.ok_or("a re-keyed source stores no grid")?;
        let binding_list = stored_field(&graph, grid, "keyformBindings")?.ok_or("a grid stores no keyformBindings")?;
        let mut bindings = Vec::new();
        for entry in binding_list.children().filter(|entry| entry.is_element()) {
            bindings.push(graph.resolve(entry)?);
        }
        if bindings.len() != old_axes.len() {
            return Err("a grid's bindings do not match its axes".into());
        }
        for (axis, (a, b)) in old_axes.iter().zip(new_axes).enumerate() {
            if a["keys"] != b["keys"] {
                let keys: Vec<f64> = b["keys"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
                patches.extend(keys_patches(text, &graph, bindings[axis], &keys)?);
            }
        }
        let cell_of = |cells: &Value| -> HashMap<String, Vec<usize>> {
            cells.as_array().into_iter().flatten().filter_map(|cell| Some((cell["formGuid"].as_str()?.to_owned(),
                cell["indices"].as_array()?.iter().filter_map(Value::as_u64).map(|i| i as usize).collect()))).collect()
        };
        let (old_cells, new_cells) = (cell_of(&old["grid"]["cells"]), cell_of(&new["grid"]["cells"]));
        let axis_of = |key: Node<'_, '_>| -> Result<usize, String> {
            let binding = graph.field(key, "binding")?.ok_or("a cell key names no binding")?;
            bindings.iter().position(|b| b.id() == binding.id()).ok_or_else(|| "a cell key names a binding outside the grid".into())
        };
        let cell_list = owned(stored_field(&graph, grid, "keyformsOnGrid")?, "keyformsOnGrid")?;
        let form_list = owned(stored_field(&graph, definition, "keyforms")?, "keyforms")?;
        // Existing cells: removed, or their key indices follow the model.
        let mut cells = HashMap::new();
        let mut removed = Vec::new();
        let mut kept_cells = 0;
        for cell in cell_list.children().filter(|cell| cell.is_element()) {
            if graph.resolve(cell)?.id() != cell.id() {
                return Err("a grid cell is stored as a shared reference".into());
            }
            let form_guid = graph.field(cell, "keyformGuid")?.and_then(|guid| guid.attribute("uuid")).ok_or("a grid cell names no form")?;
            cells.insert(form_guid.to_owned(), cell);
            let Some(indices) = new_cells.get(form_guid) else {
                removed.push(cell);
                continue;
            };
            kept_cells += 1;
            let access = graph.field(cell, "accessKey")?.ok_or("a grid cell has no accessKey")?;
            let keys = owned(stored_field(&graph, access, "_keyOnParameterList")?, "cell key list")?;
            for key in keys.children().filter(|key| key.is_element()) {
                if graph.resolve(key)?.id() != key.id() {
                    return Err("a cell key is stored as a shared reference".into());
                }
                let axis = axis_of(key)?;
                if old_cells.get(form_guid).map(|old| old[axis]) != Some(indices[axis]) {
                    let index = stored_field(&graph, key, "keyIndex")?.ok_or("a cell key has no keyIndex")?;
                    let child = index.first_child().filter(|child| child.is_text() && index.children().count() == 1)
                        .ok_or("a keyIndex is not one text")?;
                    patches.push(XmlPatch { range: child.range(), value: indices[axis].to_string() });
                }
            }
        }
        // Forms: removed ones go with the objects only they use; a new one is a clone of the stored form the model
        // copied it from, with its cell and the objects only those two use.
        let mut form_entries = HashMap::new();
        for entry in form_list.children().filter(|entry| entry.is_element()) {
            form_entries.insert(graph.guid(entry)?.ok_or("a keyform has no GUID")?.to_owned(), entry);
        }
        let new_forms = new["forms"].as_array().cloned().unwrap_or_default();
        let mut kept_forms = 0;
        for (form_guid, entry) in &form_entries {
            if new_forms.iter().any(|form| form["guid"] == form_guid.as_str()) {
                kept_forms += 1;
            } else {
                removed.push(*entry);
            }
        }
        for node in private_objects(&removed, &by_id, &refs)?.into_iter().chain(removed) {
            patches.push(XmlPatch { range: node.range(), value: String::new() });
        }
        let old_forms = old["forms"].as_array().cloned().unwrap_or_default();
        let (mut new_form_text, mut new_cell_text, mut added) = (String::new(), String::new(), 0);
        for form in &new_forms {
            let form_guid = form["guid"].as_str().ok_or("a form has no GUID")?;
            if form_entries.contains_key(form_guid) {
                continue;
            }
            let indices = new_cells.get(form_guid).ok_or("a new form is on no grid cell")?;
            // The template: a stored form equal to the new one but for its GUID, vertices and opacity.
            let like = |stored: &Value| {
                let mut stored = stored.clone();
                for key in ["guid", "opacity"] {
                    stored[key] = form[key].clone();
                }
                stored["data"]["positions"] = form["data"]["positions"].clone();
                stored == *form
            };
            let template_guid = old_forms.iter().find(|stored| like(stored)).or(old_forms.first())
                .and_then(|stored| stored["guid"].as_str()).ok_or("the grid stores no form to copy")?;
            let template = *form_entries.get(template_guid).ok_or("a template form is not in the keyforms list")?;
            let template_cell = *cells.get(template_guid).ok_or("a template form is on no grid cell")?;
            let private = private_objects(&[template, template_cell], &by_id, &refs)?;
            let mut ids = HashMap::new();
            let all = [template, template_cell].into_iter().chain(private.iter().copied());
            for id in all.flat_map(|root| root.descendants()).filter_map(|node| node.attribute("xs.id")) {
                let fresh = (1..).map(|n| format!("{id}-key{n}")).find(|fresh| !taken.contains(fresh)).unwrap_or_default();
                taken.insert(fresh.clone());
                ids.insert(id.to_owned(), fresh);
            }
            let uuids = HashMap::from([(template_guid.to_owned(), form_guid.to_owned())]);
            for node in private {
                let at = node.range().end;
                patches.push(XmlPatch { range: at..at, value: cloned(text, node, &ids, &uuids, &mut next_idx, &|_| None)? });
            }
            new_form_text.push_str(&cloned(text, template, &ids, &uuids, &mut next_idx, &|_| None)?);
            let index_of = |node: Node<'_, '_>| -> Option<String> {
                if node.attribute("xs.n") != Some("keyIndex") {
                    return None;
                }
                Some(indices[axis_of(node.parent_element()?).ok()?].to_string())
            };
            new_cell_text.push_str(&cloned(text, template_cell, &ids, &uuids, &mut next_idx, &index_of)?);
            added += 1;
        }
        for (list, kept, entries) in [(cell_list, kept_cells, new_cell_text), (form_list, kept_forms, new_form_text)] {
            if list.attribute("count").is_some() {
                patches.push(attribute_patch(text, list, "count", (kept + added).to_string())?);
            }
            if entries.is_empty() {
                continue;
            }
            let close = format!("</{}>", list.tag_name().name());
            if list.has_tag_name("carray_list") {
                patches.push(append_patch(text, list, entries)?);
            } else if text[..list.range().end].ends_with(&close) {
                let end = list.range().end - close.len();
                patches.push(XmlPatch { range: end..end, value: entries });
            } else {
                return Err("a grid list does not end the way Soul2D writes".into());
            }
        }
    }
    patches.sort_by_key(|patch| patch.range.start);
    if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
        return Err("grid patch ranges overlap".into());
    }
    let mut rewritten = text.to_owned();
    for patch in patches.into_iter().rev() {
        rewritten.replace_range(patch.range, &patch.value);
    }
    Ok(Some(CaffDocument::parse(&container.replace_main_xml(rewritten.as_bytes())?)?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::authoring_edit::tests::{artmesh_xml, test_container};
    use crate::authoring_edit::rewrite_with_copies;
    use crate::authoring_scene;

    /// The fixture ArtMesh keyed at -1 and 1 on one parameter.
    pub(crate) fn keyed_xml() -> String {
        artmesh_xml()
            .replace("<carray_list xs.n='_sources' count='0'/></CParameterSourceSet>",
                "<carray_list xs.n='_sources' count='1'><CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/>\
                 <CParameterId xs.n='id' idstr='ParamKey'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f>\
                 <f xs.n='defaultValue'>0</f></CParameterSource></carray_list></CParameterSourceSet>")
            .replace("<float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array>",
                "<KeyformGridSource xs.n='keyformGridSource'><array_list xs.n='keyformBindings' count='1'>\
                 <KeyformBindingSource xs.id='binding'><CParameterGuid xs.n='parameterGuid' uuid='parameter-guid'/>\
                 <array_list xs.n='keys' count='2'><f>-1</f><f>1</f></array_list>\
                 <InterpolationType xs.n='interpolationType' v='LINEAR'/><ExtendedInterpolationType xs.n='extendedInterpolationType' v='LINEAR'/>\
                 <i xs.n='insertPointCount'>0</i><f xs.n='extendedInterpolationScale'>1</f></KeyformBindingSource></array_list>\
                 <array_list xs.n='keyformsOnGrid' count='2'>\
                 <KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='1'>\
                 <KeyOnParameter><KeyformBindingSource xs.n='binding' xs.ref='binding'/><i xs.n='keyIndex'>0</i></KeyOnParameter>\
                 </array_list></KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid>\
                 <KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='1'>\
                 <KeyOnParameter><KeyformBindingSource xs.n='binding' xs.ref='binding'/><i xs.n='keyIndex'>1</i></KeyOnParameter>\
                 </array_list></KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='upper-guid'/></KeyformOnGrid>\
                 </array_list></KeyformGridSource><float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array>")
            .replace("<carray_list xs.n='keyforms' count='1'>", "<carray_list xs.n='keyforms' count='2'>")
            .replace("</CArtMeshForm></carray_list>",
                "</CArtMeshForm><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>\
                 <CFormGuid xs.n='guid' uuid='upper-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>\
                 </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>\
                 <f xs.n='opacity'>0.25</f><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>\
                 </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 30 40 5 6</float-array></CArtMeshForm></carray_list>")
    }

    fn request(key: f64) -> Value {
        json!({"sourceGuid": "mesh-guid", "parameterGuid": "parameter-guid", "key": key})
    }

    #[test]
    fn a_new_key_shifts_the_cells_and_writes_a_copy_of_a_stored_form() {
        let container = test_container(&keyed_xml());
        let initial = authoring_scene::read(&container).unwrap();
        let added = add_key(&initial, &request(0.5), &mut || Ok("new-guid".into())).unwrap();
        let form = added["sources"][0]["forms"].as_array().unwrap().iter().find(|form| form["guid"] == "new-guid").unwrap();
        assert_eq!(form["data"]["positions"], json!([1.0, 2.0, 23.25, 31.0, 5.0, 6.0]));
        assert_eq!(form["opacity"], json!(0.375));
        let bytes = rewrite_with_copies(&container, &initial, &added, &[]).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), added);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        assert!(xml.contains("<array_list xs.n='keys' count='3'><f>-1</f><f>0.5</f><f>1</f></array_list>"), "{xml}");
        assert!(xml.contains("<i xs.n='keyIndex'>2</i></KeyOnParameter></array_list></KeyformGridAccessKey>\
            <CFormGuid xs.n='keyformGuid' uuid='upper-guid'/>"), "the upper cell moves up");
        assert!(xml.contains("<i xs.n='keyIndex'>1</i></KeyOnParameter></array_list></KeyformGridAccessKey>\
            <CFormGuid xs.n='keyformGuid' uuid='new-guid'/></KeyformOnGrid></array_list>"), "the new cell is appended");
        assert!(xml.contains("<CFormGuid xs.n='guid' uuid='new-guid'/>"), "the new form is written");
        assert!(xml.contains("<array_list xs.n='keyformsOnGrid' count='3'>") && xml.contains("<carray_list xs.n='keyforms' count='3'>"));

        let deleted = delete_key(&added, &request(-1.0)).unwrap();
        let bytes = rewrite_with_copies(&container, &initial, &deleted, &[]).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), deleted);
        assert!(!String::from_utf8(reopened.main_xml()).unwrap().contains("uuid='form-guid'"), "the form on the key goes");
        assert!(delete_key(&delete_key(&deleted, &request(0.5)).unwrap(), &request(1.0)).unwrap_err().contains("last key"));
    }
}
