//! Deleting a CMO3 source or parameter that nothing else uses (issue #142, delete). The entry in its set, its
//! definition when the entry refers to one, and its entries in the parent Part's `_childGuids` (a source) or in
//! parameter groups (a parameter) are removed, and the counts follow. A source that another source names (as
//! parent, deformer, clipping mask or Glue target), a parameter a source or the physics uses, a Part with
//! children, the root Part, and a file that names the removed elements from anywhere else are refused.
//! `authoring_edit::rewrite` re-reads the result and refuses it unless it equals the requested model.

use crate::authoring_copy::owned_objects;
use crate::authoring_edit::XmlPatch;
use crate::authoring_graph::Graph;
use crate::authoring_reparent::{attribute_patch, source_nodes, stored_field};
use crate::authoring_values::{self as values, Budget};
use roxmltree::Node;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const SOURCE_SETS: [&str; 4] = ["drawableSourceSet", "deformerSourceSet", "partSourceSet", "affecterSourceSet"];

enum Collection {
    Source(String),
    Parameter,
    Brush,
}

pub(crate) struct Deletion {
    guid: String,
    collection: Collection,
}

impl Deletion {
    fn parent(&self) -> Option<&str> {
        match &self.collection {
            Collection::Source(parent) => Some(parent),
            Collection::Parameter | Collection::Brush => None,
        }
    }
}

pub(crate) fn mentions(value: &Value, wanted: &[&str]) -> bool {
    match value {
        Value::String(text) => wanted.contains(&text.as_str()),
        Value::Array(items) => items.iter().any(|item| mentions(item, wanted)),
        Value::Object(fields) => fields.values().any(|field| mentions(field, wanted)),
        _ => false,
    }
}

/// The textures the reader lists for `sources`: each ArtMesh's texture once, in the order of first use.
fn used_textures(sources: &[Value], textures: &[Value]) -> Vec<Value> {
    let mut used: Vec<Value> = Vec::new();
    for guid in sources.iter().map(|source| &source["data"]["textureGuid"]).filter(|guid| !guid.is_null()) {
        if !used.iter().any(|texture| texture["guid"] == *guid) {
            used.extend(textures.iter().find(|texture| texture["guid"] == *guid).cloned());
        }
    }
    used
}

fn find_once<'a>(list: &'a [Value], guid: &str, what: &str) -> Result<usize, String> {
    let matches: Vec<usize> = list.iter().enumerate()
        .filter(|(_, entry)| entry["guid"].as_str() == Some(guid)).map(|(index, _)| index).collect();
    match matches[..] {
        [index] => Ok(index),
        _ => Err(format!("{what} {guid} was not found once")),
    }
}

/// `live2d.delete {guid}`: removes one source (ArtMesh, deformer, empty Part, Glue or ArtPath) that no other
/// source names, with its texture when no other ArtMesh uses it.
pub(crate) fn delete(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    if let Some(brushes) = model["brushes"].as_array().filter(|list| list.iter().any(|brush| brush["guid"] == guid)) {
        let index = find_once(brushes, guid, "brush")?;
        if ["sources", "parameters"].iter().any(|list| model[*list].as_array().into_iter().flatten().any(|entry| entry["guid"] == guid)) {
            return Err("brush GUID is ambiguous across object collections".into());
        }
        if model["sources"].as_array().into_iter().flatten().any(|source| mentions(source, &[guid])) {
            return Err("the brush is used by a source; change that reference first".into());
        }
        let mut result = model.clone();
        result["brushes"].as_array_mut().unwrap().remove(index);
        return Ok(result);
    }
    let sources = model["sources"].as_array().ok_or("authoring source model lacks sources")?;
    let index = find_once(sources, guid, "source")?;
    let source = &sources[index];
    let parent = source["parentPartGuid"].as_str().ok_or("the root Part cannot be deleted")?;
    if source["data"]["childGuids"].as_array().is_some_and(|children| !children.is_empty()) {
        return Err("a Part with children cannot be deleted; move or delete its children first".into());
    }
    for other in sources.iter().filter(|other| other["guid"].as_str() != Some(guid)) {
        let mut named = other.clone();
        if other["guid"].as_str() == Some(parent) {
            named["data"]["childGuids"] = json!([]);
        }
        if mentions(&named, &[guid]) {
            return Err(format!("source {} names {guid}; change that first", other["guid"]));
        }
    }
    let mut result = model.clone();
    let list = result["sources"].as_array_mut().ok_or("authoring source model lacks sources")?;
    let removed = list.remove(index);
    let owner = list.iter_mut().find(|other| other["guid"].as_str() == Some(parent)).ok_or("the parent Part is missing")?;
    let children = owner["data"]["childGuids"].as_array_mut().ok_or("the parent Part lists no children")?;
    let before = children.len();
    children.retain(|child| child.as_str() != Some(guid));
    if children.len() + 1 != before {
        return Err("the parent Part does not list the source once".into());
    }
    // An unused texture goes; a kept one may move, when the deleted source was the first to use it.
    if !removed["data"]["textureGuid"].is_null() {
        let textures = result["textures"].as_array().cloned().unwrap_or_default();
        result["textures"] = json!(used_textures(result["sources"].as_array().map_or(&[][..], Vec::as_slice), &textures));
    }
    Ok(result)
}

/// `live2d.parameter.delete {guid}`: removes one parameter that no source keys and the physics does not use,
/// and its places in parameter groups.
pub(crate) fn delete_parameter(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    let parameters = model["parameters"].as_array().ok_or("authoring source model lacks parameters")?;
    let index = find_once(parameters, guid, "parameter")?;
    let id = parameters[index]["id"].as_str().unwrap_or(guid);
    if model["sources"].as_array().into_iter().flatten().any(|source| mentions(source, &[guid, id])) {
        return Err(format!("parameter {id} keys a source; remove its keys first"));
    }
    if model["physicsSource"].as_str().is_some_and(|physics| physics.contains(guid) || physics.contains(id)) {
        return Err(format!("parameter {id} is used by the physics settings"));
    }
    let mut result = model.clone();
    result["parameters"].as_array_mut().ok_or("authoring source model lacks parameters")?.remove(index);
    Ok(result)
}

/// The deletions from `initial` to `model`, and `model` with the deleted sources and parameters, their parents'
/// entries and their textures put back and the other edits kept, so the later checks compare entry by entry. A
/// deleted Part comes back holding only its deleted children.
pub(crate) fn changes(initial: &Value, model: &Value) -> Result<(Vec<Deletion>, Value), String> {
    let mut normalized = model.clone();
    let mut deletions = Vec::new();
    for list in ["parameters", "sources", "brushes"] {
        let (Some(old), Some(new)) = (initial[list].as_array(), model[list].as_array()) else { continue };
        if old.len() == new.len() {
            continue;
        }
        let kept: HashSet<&str> = new.iter().filter_map(|entry| entry["guid"].as_str()).collect();
        let mut restored = Vec::with_capacity(old.len());
        let mut next = new.iter();
        for entry in old {
            let guid = entry["guid"].as_str().ok_or("an authoring entry lacks a GUID")?;
            if kept.contains(guid) {
                let now = next.next().filter(|now| now["guid"] == entry["guid"])
                    .ok_or("entries changed order as well as count")?;
                restored.push(now.clone());
            } else {
                let collection = match list {
                    "sources" => Collection::Source(entry["parentPartGuid"].as_str().ok_or("the root Part cannot be deleted")?.to_owned()),
                    "brushes" => Collection::Brush,
                    _ => Collection::Parameter,
                };
                deletions.push(Deletion { guid: guid.to_owned(), collection });
                restored.push(entry.clone());
            }
        }
        if next.next().is_some() {
            return Err("entries were added as well as deleted".into());
        }
        normalized[list] = json!(restored);
    }
    if deletions.iter().all(|deletion| deletion.parent().is_none()) {
        return Ok((deletions, normalized));
    }
    let old_sources = initial["sources"].as_array().cloned().unwrap_or_default();
    let deleted: HashSet<&str> = deletions.iter().map(|deletion| deletion.guid.as_str()).collect();
    let restored = normalized["sources"].as_array_mut().ok_or("authoring source model lacks sources")?;
    for (index, source) in restored.iter_mut().enumerate() {
        let Some(children) = old_sources[index]["data"]["childGuids"].as_array() else { continue };
        let owner = old_sources[index]["guid"].as_str().unwrap_or_default();
        let gone: Vec<(usize, &Value)> = children.iter().enumerate()
            .filter(|(_, child)| deletions.iter()
                .any(|d| d.parent() == Some(owner) && child.as_str() == Some(d.guid.as_str())))
            .collect();
        if deleted.contains(owner) {
            // A deleted Part is empty in the model, so before the deletion it holds only its deleted children.
            source["data"]["childGuids"] = json!(gone.iter().map(|(_, child)| (*child).clone()).collect::<Vec<Value>>());
            continue;
        }
        if gone.is_empty() {
            continue;
        }
        // Each deleted child goes back before the first later sibling it had and before any child that arrived, so
        // the reparent pass sees the old list minus the departures, followed by the arrivals.
        let now = source["data"]["childGuids"].as_array().cloned().unwrap_or_default();
        let mut gone = gone.into_iter().peekable();
        let mut list = Vec::with_capacity(now.len() + gone.len());
        for child in now {
            let before = children.iter().position(|old| *old == child).unwrap_or(children.len());
            while let Some((_, entry)) = gone.next_if(|&(position, _)| position < before) {
                list.push(entry.clone());
            }
            list.push(child);
        }
        list.extend(gone.map(|(_, entry)| entry.clone()));
        source["data"]["childGuids"] = json!(list);
    }
    if normalized["textures"] != initial["textures"] {
        let old_textures = initial["textures"].as_array().cloned().unwrap_or_default();
        let now = model["textures"].as_array().cloned().unwrap_or_default();
        let sources = model["sources"].as_array().cloned().unwrap_or_default();
        if now != used_textures(&sources, &old_textures) {
            return Err("textures changed beyond those of the deleted sources".into());
        }
        normalized["textures"] = initial["textures"].clone();
    }
    Ok((deletions, normalized))
}

fn inside(node: Node<'_, '_>, roots: &[Node<'_, '_>]) -> bool {
    node.ancestors().any(|ancestor| roots.iter().any(|root| root.id() == ancestor.id()))
}

/// The entries of an inline `carray_list` field of `holder` that name `guid`.
pub(crate) fn named_entries<'d, 'i>(graph: &Graph<'d, 'i>, holder: Node<'d, 'i>, field: &str, guid: &str)
    -> Result<Option<(Node<'d, 'i>, Vec<Node<'d, 'i>>)>, String> {
    let Some(list) = stored_field(graph, holder, field)? else { return Ok(None) };
    if list.attribute("xs.ref").is_some() || !list.has_tag_name("carray_list") {
        return Err(format!("a {field} list is not inline"));
    }
    let mut entries = Vec::new();
    for entry in list.children().filter(|entry| entry.is_element()) {
        if graph.resolve(entry)?.attribute("uuid") == Some(guid) {
            entries.push(entry);
        }
    }
    Ok(Some((list, entries)))
}

/// Removal patches for every deletion, and the counts of the lists they shrink.
pub(crate) fn patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    deletions: &[Deletion],
    budget: &mut Budget,
) -> Result<Vec<XmlPatch>, String> {
    if deletions.is_empty() {
        return Ok(Vec::new());
    }
    let guids: HashSet<&str> = deletions.iter().map(|deletion| deletion.guid.as_str()).collect();
    let brush_guids: HashSet<&str> = deletions.iter()
        .filter(|deletion| matches!(deletion.collection, Collection::Brush))
        .map(|deletion| deletion.guid.as_str()).collect();
    let mut removed: Vec<Node<'d, 'i>> = Vec::new();
    let mut shrunk: Vec<(Node<'d, 'i>, usize)> = Vec::new();
    let mut shrink = |list: Node<'d, 'i>, gone: usize| match shrunk.iter_mut().find(|(known, _)| known.id() == list.id()) {
        Some((_, count)) => *count += gone,
        None => shrunk.push((list, gone)),
    };
    for deletion in deletions {
        let sets: &[&str] = match deletion.collection {
            Collection::Source(_) => &SOURCE_SETS,
            Collection::Parameter => &["parameterSourceSet"],
            Collection::Brush => &[],
        };
        let mut found = None;
        let mut parent = None;
        if matches!(deletion.collection, Collection::Brush) {
            let setting = graph.field(model, "artPathBrushesSetting")?.ok_or("the brush setting is missing")?;
            let list = graph.field(setting, "brushes")?.ok_or("the brush list is missing")?;
            for entry in list.children().filter(|entry| entry.is_element()) {
                let definition = graph.resolve(entry)?;
                if graph.guid(definition)? == Some(deletion.guid.as_str())
                    && found.replace((list, entry, definition)).is_some() {
                    return Err("a deleted brush is listed twice".into());
                }
            }
        }
        for set in sets {
            let Some(holder) = graph.field(model, set)? else { continue };
            let list = graph.field(holder, "_sources")?.ok_or("a source set lacks _sources")?;
            for entry in list.children().filter(|entry| entry.is_element()) {
                let source = graph.resolve(entry)?;
                let guid = graph.guid(source)?;
                if guid == Some(deletion.guid.as_str()) {
                    if found.replace((list, entry, source)).is_some() {
                        return Err("a deleted entry is listed twice".into());
                    }
                } else if guid.is_some() && guid == deletion.parent() {
                    parent = Some(source);
                }
            }
        }
        let (list, entry, definition) = found.ok_or("a deleted entry was not found in the CMO3")?;
        removed.push(entry);
        if definition.id() != entry.id() {
            removed.push(definition);
        }
        shrink(list, 1);
        if deletion.parent().is_some_and(|name| guids.contains(name)) {
            // The entry goes with the deleted parent Part's definition.
        } else if deletion.parent().is_some() {
            let parent = parent.ok_or("a deleted source's parent was not found in the CMO3")?;
            let (children, entries) = named_entries(graph, parent, "_childGuids", &deletion.guid)?
                .ok_or("the parent Part stores no _childGuids list")?;
            let [child] = entries[..] else {
                return Err("the parent Part does not list the deleted source once".into());
            };
            removed.push(child);
            shrink(children, 1);
        } else if let Some(groups) = graph.field(model, "parameterGroupSet")?.filter(|_| matches!(deletion.collection, Collection::Parameter)) {
            if let Some(list) = graph.field(groups, "_groups")? {
                for group in values::list(graph, list, budget)? {
                    if let Some((members, entries)) = named_entries(graph, group, "_childGuids", &deletion.guid)? {
                        shrink(members, entries.len());
                        removed.extend(entries);
                    }
                }
            }
        }
    }
    // The objects only the removed elements use go with them: in a Cubism file the GUID objects, the extensions
    // that name their owner back, and the keyform grid and bindings.
    let mut by_id = HashMap::new();
    let mut refs: HashMap<&str, Vec<Node<'d, 'i>>> = HashMap::new();
    for node in model.document().descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("xs.id") {
            by_id.insert(id, node);
        }
        if let Some(id) = node.attribute("xs.ref") {
            refs.entry(id).or_default().push(node);
        }
    }
    let mut kept: Vec<Node<'d, 'i>> = source_nodes(graph, model)?.into_iter()
        .filter(|(guid, _)| !deletions.iter().any(|deletion| deletion.guid == *guid)).map(|(_, node)| node).collect();
    for brush in crate::authoring_edit::brush_nodes(graph, model)? {
        if !graph.guid(brush)?.is_some_and(|guid| guids.contains(guid)) {
            kept.push(brush);
        }
    }
    let stop = |node: Node<'d, 'i>| node.id() == model.id() || kept.iter().any(|source| source.id() == node.id());
    // What a removed element defines must be used only from removed elements, and nothing left may name a
    // deleted GUID. A Cubism 2.1 conversion record (an IdSet) that names a removed object goes with it, and with it
    // the objects only it used.
    let roots = removed;
    let mut records: Vec<Node<'d, 'i>> = Vec::new();
    let removed = loop {
        let removed = owned_objects(&[roots.clone(), records.clone()].concat(), &[], &by_id, &refs, &stop, false, budget)?;
        let ids: HashSet<&str> = removed.iter().flat_map(|root| root.descendants())
            .filter_map(|node| node.attribute("xs.id")).collect();
        let naming: Vec<Node<'d, 'i>> = model.document().descendants()
            .filter(|node| node.is_element() && !inside(*node, &removed))
            .filter(|node| node.attribute("xs.ref").is_some_and(|id| ids.contains(id))
                || node.attribute("uuid").is_some_and(|uuid| guids.contains(uuid)))
            .collect();
        let found = records.len();
        let mut other = None;
        let record = |node: Node<'d, 'i>| {
            if graph.resolve(node).ok().is_some_and(|resolved| {
                resolved.attribute("uuid").is_some_and(|guid| brush_guids.contains(guid))
                    || graph.guid(resolved).ok().flatten().is_some_and(|guid| brush_guids.contains(guid))
            }) {
                return None;
            }
            node.parent_element().filter(|set| set.has_tag_name("IdSet"))
                .filter(|set| set.parent_element().is_some_and(|list| list.attribute("xs.n") == Some("idSets")))
        };
        for node in naming {
            // The record names the deleted object, or the GUID object that carries the deleted GUID.
            let by_reference = node.attribute("xs.id").and_then(|id| refs.get(id)).into_iter().flatten()
                .filter(|reference| !inside(**reference, &removed)).map(|reference| record(*reference)).collect::<Option<Vec<_>>>();
            let sets = match record(node) {
                Some(set) => vec![set],
                None => by_reference.filter(|sets| !sets.is_empty()).unwrap_or_default(),
            };
            if sets.is_empty() {
                other = other.or(Some(node));
            }
            for set in sets {
                if !records.iter().any(|known| known.id() == set.id()) {
                    records.push(set);
                }
            }
        }
        if records.len() > found {
            continue;
        }
        if let Some(node) = other {
            // Name the referrer of a GUID object rather than the object, and the model field or object it is in.
            let node = node.attribute("xs.id").and_then(|id| refs.get(id)).into_iter().flatten()
                .find(|reference| !inside(**reference, &removed) && record(**reference).is_none()).copied().unwrap_or(node);
            let place = node.ancestors().filter(|n| n.is_element()).find_map(|n| {
                let parent = n.parent_element()?;
                if parent.id() == model.id() {
                    n.attribute("xs.n").map(str::to_owned)
                } else if parent.has_tag_name("shared") || parent.parent_element().is_none() {
                    Some(n.tag_name().name().to_owned())
                } else {
                    None
                }
            }).unwrap_or_else(|| node.tag_name().name().to_owned());
            return Err(format!("the deleted entry is named from elsewhere in the file: {place} ({})",
                node.attribute("xs.n").unwrap_or("list entry")));
        }
        break removed;
    };
    for set in &records {
        shrink(set.parent_element().ok_or("an IdSet has no list")?, 1);
    }
    let mut patches: Vec<XmlPatch> =
        removed.iter().map(|node| XmlPatch { range: node.range(), value: String::new() }).collect();
    for (list, gone) in shrunk {
        let count = values::list(graph, list, budget)?.len().checked_sub(gone).ok_or("a list count would be negative")?;
        if list.attribute("count").is_some() {
            patches.push(attribute_patch(text, list, "count", count.to_string())?);
        }
    }
    Ok(patches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring_edit::{rewrite, tests::test_container};

    #[test]
    fn unused_brush_deletion_preserves_later_edits_and_refuses_external_references() {
        let original = crate::authoring_edit::tests::artmesh_xml().replace(
            "<CPartSource xs.n='rootPart' xs.ref='part'/>",
            "<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
             <carray_list xs.n='brushes' count='2'>\
             <CArtPathBrush xs.id='brush-a'><CArtPathBrushGuid xs.n='guid' uuid='brush-a-guid'/></CArtPathBrush>\
             <CArtPathBrush xs.id='brush-b'><CArtPathBrushGuid xs.n='guid' uuid='brush-b-guid'/>\
             <s xs.n='name'>Old</s></CArtPathBrush></carray_list></CArtPathBrushSetting>");
        let container = test_container(&original);
        let initial = crate::authoring_scene::read(&container).unwrap();
        let renamed = crate::authoring_edit::rename(&initial, &json!({"guid":"brush-b-guid","name":"New"})).unwrap();
        let deleted = delete(&renamed, &json!({"guid":"brush-a-guid"})).unwrap();
        let bytes = rewrite(&container, &initial, &deleted).unwrap();
        let reopened = crate::caff::CaffDocument::parse(&bytes).unwrap();
        assert_eq!(crate::authoring_scene::read(&reopened).unwrap(), deleted);
        assert_eq!(deleted["brushes"].as_array().unwrap().len(), 1);
        assert_eq!(deleted["brushes"][0]["name"], "New");
        let empty = delete(&deleted, &json!({"guid":"brush-b-guid"})).unwrap();
        let empty_bytes = rewrite(&reopened, &deleted, &empty).unwrap();
        assert_eq!(crate::authoring_scene::read(&crate::caff::CaffDocument::parse(&empty_bytes).unwrap()).unwrap(), empty);
        let referenced = original.replace("</CModelSource>",
            "<CArtPathBrushGuid xs.n='unknownSelection' uuid='brush-a-guid'/></CModelSource>");
        assert_ne!(referenced, original);
        let referenced = test_container(&referenced);
        let model = crate::authoring_scene::read(&referenced).unwrap();
        let candidate = delete(&model, &json!({"guid":"brush-a-guid"})).unwrap();
        assert!(rewrite(&referenced, &model, &candidate).is_err());
        let registry = original.replace("</CModelSource>",
            "<carray_list xs.n='idSets' count='1'><IdSet><CArtPathBrushGuid uuid='brush-a-guid'/>\
             </IdSet></carray_list></CModelSource>");
        let registry = test_container(&registry);
        let model = crate::authoring_scene::read(&registry).unwrap();
        let candidate = delete(&model, &json!({"guid":"brush-a-guid"})).unwrap();
        assert!(rewrite(&registry, &model, &candidate).is_err(), "unknown brush registries must not be removed");
    }
    use crate::authoring_scene;
    use crate::caff::CaffDocument;

    #[test]
    fn an_unused_source_is_deleted_with_its_entries() {
        let original = crate::authoring_reparent::tests::xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let without_arm = delete(&initial, &json!({"guid": "arm-guid"})).unwrap();
        let bytes = rewrite(&container, &initial, &without_arm).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        let arm_start = original.find("<CPartSource xs.id='arm'>").unwrap();
        let arm_end = arm_start + original[arm_start..].find("</CPartSource>").unwrap() + "</CPartSource>".len();
        let expected = format!("{}{}", &original[..arm_start], &original[arm_end..])
            .replace("count='2'><CPartSource xs.ref='part'/><CPartSource xs.ref='arm'/>", "count='1'><CPartSource xs.ref='part'/>")
            .replace("count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/>", "count='1'><CArtMeshGuid uuid='mesh-guid'/>");
        assert_eq!(written, expected);

        // The ArtMesh goes with its texture; a mask user, the root and a Part with children are refused.
        let without_mesh = delete(&initial, &json!({"guid": "mesh-guid"})).unwrap();
        assert_eq!(without_mesh["textures"], json!([]));
        rewrite(&container, &initial, &without_mesh).unwrap();
        assert!(delete(&initial, &json!({"guid": "part-guid"})).unwrap_err().contains("root"));
        let moved = crate::authoring_reparent::reparent(&initial, &json!({"guid": "mesh-guid", "parent": "arm-guid"})).unwrap();
        assert!(delete(&moved, &json!({"guid": "arm-guid"})).unwrap_err().contains("children"));
        let referred = original.replace("<CPartSource xs.n='rootPart' xs.ref='part'/>",
            "<CPartSource xs.n='rootPart' xs.ref='part'/><CPartSource xs.n='favourite' xs.ref='arm'/>");
        let referred_container = test_container(&referred);
        let referred_initial = authoring_scene::read(&referred_container).unwrap();
        let gone = delete(&referred_initial, &json!({"guid": "arm-guid"})).unwrap();
        assert!(rewrite(&referred_container, &referred_initial, &gone).unwrap_err().contains("elsewhere"));
    }

    #[test]
    fn a_source_goes_with_the_objects_only_it_uses_and_its_conversion_record() {
        // Shaped like a Cubism file: a shared GUID object, an extension that names its owner, a separate binding,
        // and a Cubism 2.1 conversion record naming the GUID object.
        let original = crate::authoring_grid::tests::keyed_xml()
            .replace("<CArtMeshGuid xs.n='guid' uuid='mesh-guid'/>", "<CArtMeshGuid xs.n='guid' xs.ref='mesh-guid-object'/>\
                <carray_list xs.n='_extensions' count='1'><CTextureInputExtension xs.ref='extension'/></carray_list>")
            .replace("<CArtMeshSource xs.id='mesh'>", "<CArtMeshGuid xs.id='mesh-guid-object' uuid='mesh-guid'/>\
                <CTextureInputExtension xs.id='extension'><CArtMeshSource xs.n='_owner' xs.ref='mesh'/></CTextureInputExtension>\
                <CArtMeshSource xs.id='mesh'>")
            .replace("<CPartId xs.n='id' idstr='root'/>", "<CPartId xs.n='id' idstr='root'/>\
                <carray_list xs.n='_childGuids' count='1'><CArtMeshGuid xs.ref='mesh-guid-object'/></carray_list>")
            .replace("<CPartSource xs.n='rootPart' xs.ref='part'/>", "<CPartSource xs.n='rootPart' xs.ref='part'/>\
                <hash_map xs.n='modelOptions'><Option><carray_list xs.n='idSets' count='2'><IdSet><s xs.n='idString21'>D_MESH</s>\
                <CArtMeshGuid xs.n='guid30' xs.ref='mesh-guid-object'/></IdSet><IdSet><s xs.n='idString21'>PARAM</s></IdSet>\
                </carray_list></Option></hash_map>");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let gone = delete(&initial, &json!({"guid": "mesh-guid"})).unwrap();
        let bytes = rewrite(&container, &initial, &gone).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), gone);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        for removed in ["mesh-guid", "xs.id='extension'", "xs.id='binding'", "D_MESH", "<CArtMeshSource"] {
            assert!(!xml.contains(removed), "{removed} goes: {xml}");
        }
        assert!(xml.contains("<carray_list xs.n='idSets' count='1'>"), "{xml}");

        // An object of another source that names the ArtMesh's GUID keeps it, and the refusal says where.
        let named = original.replace("<Opaque xs.id='future-data'>",
            "<CControllerExtension xs.id='controller'><CArtMeshGuid xs.n='artMeshGuid' xs.ref='mesh-guid-object'/></CControllerExtension>\
             <Opaque xs.id='future-data'>");
        let container = test_container(&named);
        let initial = authoring_scene::read(&container).unwrap();
        let refused = rewrite(&container, &initial, &delete(&initial, &json!({"guid": "mesh-guid"})).unwrap()).unwrap_err();
        assert!(refused.contains("CControllerExtension (artMeshGuid)"), "{refused}");
    }

    #[test]
    fn an_unused_parameter_is_deleted_with_its_group_places() {
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
        let without = delete_parameter(&initial, &json!({"guid": "parameter-guid"})).unwrap();
        assert_eq!(without["parameters"], json!([]));
        let bytes = rewrite(&container, &initial, &without).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        assert_eq!(written, original
            .replace(&format!("count='1'>{parameter}</carray_list>"), "count='0'></carray_list>")
            .replace("<carray_list xs.n='_childGuids' count='1'><CParameterGuid uuid='parameter-guid'/></carray_list>",
                "<carray_list xs.n='_childGuids' count='0'></carray_list>"));

        let mut physical = initial.clone();
        physical["physicsSource"] = json!("<CPhysicsSettingsSourceSet>ParamSpare</CPhysicsSettingsSourceSet>");
        assert!(delete_parameter(&physical, &json!({"guid": "parameter-guid"})).unwrap_err().contains("physics"));
        let named = test_container(&original.replace("<null xs.n='parentGroupGuid'/>",
            "<null xs.n='parentGroupGuid'/><CParameterGuid xs.n='favourite' uuid='parameter-guid'/>"));
        let named_initial = authoring_scene::read(&named).unwrap();
        let gone = delete_parameter(&named_initial, &json!({"guid": "parameter-guid"})).unwrap();
        assert!(rewrite(&named, &named_initial, &gone).unwrap_err().contains("elsewhere"));
    }

    /// The reparent fixture with a second empty Part `leg` under the root, after `arm`.
    fn three_parts() -> String {
        let leg = "<CPartSource xs.id='leg'><CPartGuid xs.n='guid' uuid='leg-guid'/><CPartId xs.n='id' idstr='leg'/>\
            <CPartGuid xs.n='parentGuid' uuid='part-guid'/><carray_list xs.n='_childGuids' count='0'/>\
            <carray_list xs.n='keyforms' count='0'/></CPartSource>";
        crate::authoring_reparent::tests::xml()
            .replace("count='2'><CPartSource xs.ref='part'/><CPartSource xs.ref='arm'/>",
                "count='3'><CPartSource xs.ref='part'/><CPartSource xs.ref='arm'/><CPartSource xs.ref='leg'/>")
            .replace("count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/>",
                "count='3'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/><CPartGuid uuid='leg-guid'/>")
            .replace("<GTexture2D xs.id='texture'>", &format!("{leg}<GTexture2D xs.id='texture'>"))
    }

    fn written(container: &CaffDocument, initial: &Value, model: &Value) -> (CaffDocument, String) {
        let bytes = rewrite(container, initial, model).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(&authoring_scene::read(&reopened).unwrap(), model);
        let xml = String::from_utf8(reopened.main_xml()).unwrap();
        (reopened, xml)
    }

    #[test]
    fn a_delete_composes_with_a_reparent_into_or_out_of_its_part() {
        use crate::authoring_reparent::reparent;
        let container = test_container(&three_parts());
        let initial = authoring_scene::read(&container).unwrap();

        // A Part is deleted after a sibling moved under another Part: both change the root's child list.
        let moved = reparent(&initial, &json!({"guid": "mesh-guid", "parent": "arm-guid"})).unwrap();
        let model = delete(&moved, &json!({"guid": "leg-guid"})).unwrap();
        let (reopened, xml) = written(&container, &initial, &model);
        assert!(xml.contains("<carray_list xs.n='_childGuids' count='1'><CPartGuid uuid='arm-guid'/></carray_list>"), "{xml}");
        assert!(xml.contains("<carray_list xs.n='_childGuids' count='1'><CArtMeshGuid uuid='mesh-guid'/></carray_list>"), "{xml}");
        assert!(xml.contains("count='2'><CPartSource xs.ref='part'/><CPartSource xs.ref='arm'/></carray_list>"), "{xml}");
        assert!(!xml.contains("leg-guid") && !xml.contains("xs.id='leg'"), "{xml}");

        // The child moves out of the Part and the Part is deleted afterwards.
        let initial = authoring_scene::read(&reopened).unwrap();
        let out = reparent(&initial, &json!({"guid": "mesh-guid", "parent": "part-guid"})).unwrap();
        let model = delete(&out, &json!({"guid": "arm-guid"})).unwrap();
        let (_, xml) = written(&reopened, &initial, &model);
        assert!(xml.contains("count='1'><CArtMeshGuid uuid='mesh-guid'/></carray_list>"), "{xml}");
        assert!(!xml.contains("arm-guid") && !xml.contains("xs.id='arm'"), "{xml}");
    }

    #[test]
    fn a_part_and_its_children_are_deleted_in_one_session() {
        use crate::authoring_reparent::reparent;
        let container = test_container(&three_parts());
        let initial = authoring_scene::read(&container).unwrap();
        let moved = reparent(&initial, &json!({"guid": "mesh-guid", "parent": "arm-guid"})).unwrap();
        let (container, xml) = written(&container, &initial, &moved);
        assert!(xml.contains("count='1'><CArtMeshGuid uuid='mesh-guid'/></carray_list>"), "{xml}");
        let initial = authoring_scene::read(&container).unwrap();
        assert!(delete(&initial, &json!({"guid": "arm-guid"})).unwrap_err().contains("children"));

        // The child and then its Part: the child's entry goes with the Part's definition, and its texture with it.
        let model = delete(&delete(&initial, &json!({"guid": "mesh-guid"})).unwrap(), &json!({"guid": "arm-guid"})).unwrap();
        let (_, xml) = written(&container, &initial, &model);
        for gone in ["arm-guid", "mesh-guid", "<GTexture2D xs.id='texture'>"] {
            assert!(!xml.contains(gone), "{gone} goes: {xml}");
        }
        assert!(xml.contains("<carray_list xs.n='_childGuids' count='1'><CPartGuid uuid='leg-guid'/></carray_list>"), "{xml}");

        // The child moves out, is deleted, and its old Part is deleted: the Part was empty when it went.
        let out = reparent(&initial, &json!({"guid": "mesh-guid", "parent": "part-guid"})).unwrap();
        let model = delete(&delete(&out, &json!({"guid": "mesh-guid"})).unwrap(), &json!({"guid": "arm-guid"})).unwrap();
        let (_, xml) = written(&container, &initial, &model);
        assert!(!xml.contains("arm-guid") && !xml.contains("mesh-guid"), "{xml}");
    }
}
