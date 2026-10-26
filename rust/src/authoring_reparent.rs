//! Moving a CMO3 source to another Part (issue #142, reparent). The child's `parentGuid` is rewritten in place and
//! its entry in the old Part's `_childGuids` list moves, as the same text, to the end of the new Part's list; both
//! list counts follow. A GUID entry that defines a shared `xs.id` is refused rather than moved, since the order of
//! definitions in the file would change. `authoring_edit::rewrite` re-reads the result and refuses it unless it
//! equals the requested model.

use crate::authoring_edit::XmlPatch;
use crate::authoring_graph::Graph;
use crate::authoring_values::{self as values, Budget};
use roxmltree::Node;
use serde_json::{json, Value};
use std::collections::HashMap;

/// One source whose parent Part changed between the opened file and the model.
pub(crate) struct Move {
    child: String,
    from: String,
    to: String,
}

const SOURCE_SETS: [&str; 4] = ["drawableSourceSet", "deformerSourceSet", "partSourceSet", "affecterSourceSet"];

fn child_list(source: &Value) -> Option<&Vec<Value>> {
    source["data"]["childGuids"].as_array()
}

fn source_index(sources: &[Value], guid: &str) -> Result<usize, String> {
    let mut found = sources.iter().enumerate().filter(|(_, source)| source["guid"].as_str() == Some(guid));
    let (index, _) = found.next().ok_or_else(|| format!("source {guid} was not found"))?;
    if found.next().is_some() {
        return Err(format!("source {guid} is ambiguous"));
    }
    Ok(index)
}

/// Reattach one static ArtMesh to a rotation chain without changing its canvas geometry or Part membership.
pub(crate) fn reparent_deformer(model: &Value, request: &Value) -> Result<Value, String> {
    let object = request.as_object().ok_or("deformer reparent request must be an object")?;
    for key in object.keys() {
        if !["op", "sourceGuid", "targetDeformerGuid", "expectedRevision"].contains(&key.as_str()) {
            return Err(format!("live2d.reparent_deformer carries an unknown key {key}"));
        }
    }
    if model["format"] != "live2d.authoring.source.v1" {
        return Err("deformer reparent requires a CMO3 source model".into());
    }
    let guid = request["sourceGuid"].as_str().filter(|s| !s.is_empty()).ok_or("sourceGuid must be a string")?;
    let target = request["targetDeformerGuid"].as_str().filter(|s| !s.is_empty()).ok_or("targetDeformerGuid must be a string")?;
    let sources = model["sources"].as_array().ok_or("authoring source model lacks sources")?;
    let (child, destination) = (source_index(sources, guid)?, source_index(sources, target)?);
    if sources[child]["data"]["type"] != "ArtMesh" || sources[destination]["data"]["type"] != "Rotation" {
        return Err("deformer reparent requires an ArtMesh and a Rotation destination".into());
    }
    if sources[child]["targetDeformerGuid"].as_str() == Some(target) {
        return Ok(model.clone());
    }
    let positions = crate::native_scene::reexpress_rotation_positions(model, guid, target)?;
    let mut result = model.clone();
    result["sources"][child]["targetDeformerGuid"] = json!(target);
    result["sources"][child]["forms"][0]["data"]["positions"] = json!(positions);
    Ok(result)
}

/// The moves from `initial` to `model`, and `model` with them undone. Each Part's child list must be its old list
/// without the children that left, followed by the children that arrived; the moves come back in that arrival
/// order, so the inserted entries keep it.
pub(crate) fn changes(initial: &Value, model: &Value) -> Result<(Vec<Move>, Value), String> {
    let mut normalized = model.clone();
    let mut moves = Vec::new();
    let (Some(old_sources), Some(new_sources)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok((moves, normalized));
    };
    if old_sources.len() != new_sources.len() {
        return Ok((moves, normalized));
    }
    for (index, (old, new)) in old_sources.iter().zip(new_sources).enumerate() {
        if old["guid"] != new["guid"] || old["parentPartGuid"] == new["parentPartGuid"] {
            continue;
        }
        let (Some(child), Some(from), Some(to)) =
            (old["guid"].as_str(), old["parentPartGuid"].as_str(), new["parentPartGuid"].as_str()) else {
            return Err("a reparent needs a source with a parent Part, moved to a Part".into());
        };
        moves.push(Move { child: child.to_owned(), from: from.to_owned(), to: to.to_owned() });
        normalized["sources"][index]["parentPartGuid"] = old["parentPartGuid"].clone();
    }
    if moves.is_empty() {
        return Ok((moves, normalized));
    }
    let mut arrival = HashMap::new();
    for (index, (old, new)) in old_sources.iter().zip(new_sources).enumerate() {
        let (Some(old_children), Some(new_children)) = (child_list(old), child_list(new)) else { continue };
        let guid = old["guid"].as_str();
        let kept: Vec<&Value> = old_children.iter()
            .filter(|child| !moves.iter().any(|m| Some(m.from.as_str()) == guid && child.as_str() == Some(&m.child)))
            .collect();
        let arriving: Vec<&str> = moves.iter().filter(|m| Some(m.to.as_str()) == guid).map(|m| m.child.as_str()).collect();
        let (head, tail) = new_children.split_at(kept.len().min(new_children.len()));
        let follows = head.iter().eq(kept.iter().copied()) && tail.len() == arriving.len()
            && tail.iter().all(|child| child.as_str().is_some_and(|child| arriving.contains(&child)));
        if !follows {
            return Err("a Part's child list does not follow the reparented sources".into());
        }
        for (position, child) in tail.iter().enumerate() {
            arrival.insert(child.as_str().unwrap_or_default().to_owned(), position);
        }
        normalized["sources"][index]["data"]["childGuids"] = json!(old_children);
    }
    moves.sort_by_key(|m| arrival.get(&m.child).copied().unwrap_or_default());
    Ok((moves, normalized))
}

/// The field element as stored on `node` or its `super` chain, without following an `xs.ref` on the field itself.
pub(crate) fn stored_field<'d, 'i>(graph: &Graph<'d, 'i>, node: Node<'d, 'i>, name: &str) -> Result<Option<Node<'d, 'i>>, String> {
    let mut current = graph.resolve(node)?;
    for _ in 0..=64 {
        let mut fields = current.children().filter(|child| child.is_element() && child.attribute("xs.n") == Some(name));
        if let Some(field) = fields.next() {
            return match fields.next() {
                Some(_) => Err(format!("duplicate authoring field {name:?}")),
                None => Ok(Some(field)),
            };
        }
        let Some(parent) = current.children().find(|child| child.is_element() && child.attribute("xs.n") == Some("super"))
        else {
            return Ok(None);
        };
        current = graph.resolve(parent)?;
    }
    Err("authoring super chain exceeds 64 nodes".into())
}

pub(crate) fn attribute_patch(text: &str, node: Node<'_, '_>, name: &str, value: String) -> Result<XmlPatch, String> {
    let attribute = node.attributes().find(|attribute| attribute.name() == name)
        .ok_or_else(|| format!("the CMO3 element lacks {name}"))?;
    let range = attribute.range_value();
    if text.get(range.clone()) != Some(attribute.value()) {
        return Err(format!("the CMO3 {name} is stored escaped"));
    }
    Ok(XmlPatch { range, value })
}

/// Every source of the file by GUID.
pub(crate) fn source_nodes<'d, 'i>(graph: &Graph<'d, 'i>, model: Node<'d, 'i>) -> Result<HashMap<&'d str, Node<'d, 'i>>, String> {
    let mut sources = HashMap::new();
    for set in SOURCE_SETS {
        for source in graph.source_list(model, set)? {
            if let Some(guid) = graph.guid(source)? {
                if sources.insert(guid, source).is_some() {
                    return Err("a source GUID is ambiguous in the CMO3".into());
                }
            }
        }
    }
    Ok(sources)
}

/// The patch that appends `entries` (already XML) to an inline `carray_list`, opening a self-closing one.
pub(crate) fn append_patch(text: &str, list: Node<'_, '_>, entries: String) -> Result<XmlPatch, String> {
    let range = list.range();
    let source = text.get(range.clone()).ok_or("a list range is invalid")?;
    if source.ends_with("/>") {
        Ok(XmlPatch { range: range.end - 2..range.end, value: format!(">{entries}</carray_list>") })
    } else if source.ends_with("</carray_list>") {
        let at = range.end - "</carray_list>".len();
        Ok(XmlPatch { range: at..at, value: entries })
    } else {
        Err("a list does not end the way Soul2D writes".into())
    }
}

/// The patches for every move: one per changed `parentGuid`, one removal per moved list entry, one insertion and
/// one count per changed list.
pub(crate) fn patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    moves: &[Move],
    budget: &mut Budget,
) -> Result<Vec<XmlPatch>, String> {
    if moves.is_empty() {
        return Ok(Vec::new());
    }
    let sources = source_nodes(graph, model)?;
    let find = |guid: &str| sources.get(guid).copied().ok_or_else(|| format!("source {guid} was not found in the CMO3"));
    let child_list = |part: Node<'d, 'i>| -> Result<Node<'d, 'i>, String> {
        let list = stored_field(graph, part, "_childGuids")?.ok_or("a Part stores no _childGuids list")?;
        if list.attribute("xs.ref").is_some() || !list.has_tag_name("carray_list") {
            return Err("a Part's _childGuids is not an inline list".into());
        }
        Ok(list)
    };
    let mut patches = Vec::new();
    // Per list: its node, the number of entries leaving and arriving, and the text of those arriving.
    let mut lists: Vec<(Node<'d, 'i>, usize, usize, String)> = Vec::new();
    for change in moves {
        let child = find(&change.child)?;
        let (from, to) = (find(&change.from)?, find(&change.to)?);
        let parent = stored_field(graph, child, "parentGuid")?.ok_or("a moved source stores no parentGuid")?;
        if parent.attribute("xs.id").is_some() {
            return Err("a moved source's parentGuid defines a shared GUID".into());
        }
        if parent.attribute("xs.ref").is_some() {
            // The reference must point at the new Part's own GUID definition.
            let guid = stored_field(graph, to, "guid")?.ok_or("the new Part stores no guid")?;
            let definition = graph.resolve(guid)?;
            let id = definition.attribute("xs.id").ok_or("the new Part's GUID has no xs.id to refer to")?;
            patches.push(attribute_patch(text, parent, "xs.ref", id.to_owned())?);
        } else {
            patches.push(attribute_patch(text, parent, "uuid", change.to.clone())?);
        }
        let old_list = child_list(from)?;
        let mut entries = Vec::new();
        for entry in old_list.children().filter(|entry| entry.is_element()) {
            if graph.resolve(entry)?.attribute("uuid") == Some(change.child.as_str()) {
                entries.push(entry);
            }
        }
        let [entry] = entries[..] else {
            return Err("the old Part's _childGuids does not name the moved source once".into());
        };
        if entry.attribute("xs.id").is_some() {
            return Err("the moved source's _childGuids entry defines a shared GUID".into());
        }
        let entry_text = text.get(entry.range()).ok_or("a _childGuids entry range is invalid")?.to_owned();
        patches.push(XmlPatch { range: entry.range(), value: String::new() });
        let new_list = child_list(to)?;
        for (list, leaving, arriving, entry) in [(old_list, 1, 0, String::new()), (new_list, 0, 1, entry_text)] {
            match lists.iter_mut().find(|(known, ..)| known.id() == list.id()) {
                Some((_, gone, came, text)) => {
                    *gone += leaving;
                    *came += arriving;
                    text.push_str(&entry);
                }
                None => lists.push((list, leaving, arriving, entry)),
            }
        }
    }
    for (list, leaving, arrived, arriving) in lists {
        let count = values::list(graph, list, budget)?.len();
        let total = (count + arrived).checked_sub(leaving).ok_or("a _childGuids count would be negative")?;
        if list.attribute("count").is_some() {
            patches.push(attribute_patch(text, list, "count", total.to_string())?);
        }
        if !arriving.is_empty() {
            patches.push(append_patch(text, list, arriving)?);
        }
    }
    Ok(patches)
}

/// `live2d.reparent {guid, parent}`: moves one source (ArtMesh, deformer, Part, Glue or ArtPath) under another
/// Part, at the end of its children. The root Part, a move under itself or its own descendant, and a source the
/// old Part does not list are refused.
pub(crate) fn reparent(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    let parent = request["parent"].as_str().filter(|guid| !guid.is_empty()).ok_or("parent must be a Part GUID")?;
    let sources = model["sources"].as_array().ok_or("authoring source model lacks sources")?;
    let (child, target) = (source_index(sources, guid)?, source_index(sources, parent)?);
    if sources[target]["data"]["type"] != "Part" {
        return Err("parent must be a Part".into());
    }
    let from = sources[child]["parentPartGuid"].as_str().ok_or("the root Part has no parent to change")?;
    if from == parent {
        return Ok(model.clone());
    }
    let mut ancestor = Some(parent);
    for _ in 0..=sources.len() {
        let Some(current) = ancestor else { break };
        if current == guid {
            return Err("a Part cannot move under itself or its own descendant".into());
        }
        ancestor = sources[source_index(sources, current)?]["parentPartGuid"].as_str();
    }
    let old_parent = source_index(sources, from)?;
    let listed = child_list(&sources[old_parent]).ok_or("the old parent lists no children")?;
    if listed.iter().filter(|entry| entry.as_str() == Some(guid)).count() != 1 {
        return Err("the old parent does not list the source once".into());
    }
    let mut result = model.clone();
    result["sources"][child]["parentPartGuid"] = json!(parent);
    let kept: Vec<Value> = listed.iter().filter(|entry| entry.as_str() != Some(guid)).cloned().collect();
    result["sources"][old_parent]["data"]["childGuids"] = json!(kept);
    let mut arrived = child_list(&sources[target]).cloned().unwrap_or_default();
    arrived.push(json!(guid));
    result["sources"][target]["data"]["childGuids"] = json!(arrived);
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::authoring_edit::{rewrite, tests::test_container};
    use crate::authoring_scene;
    use crate::caff::CaffDocument;

    /// A root Part holding an ArtMesh and an empty Part `Arm` stored as a self-closing list.
    pub(crate) fn xml() -> String {
        r#"<root fileFormatVersion='401000004'><CModelSource>
          <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
          <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='0'/></CParameterSourceSet>
          <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='1'><CArtMeshSource xs.ref='mesh'/></carray_list></CDrawableSourceSet>
          <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/></CDeformerSourceSet>
          <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='2'><CPartSource xs.ref='part'/><CPartSource xs.ref='arm'/></carray_list></CPartSourceSet>
          <CPartSource xs.n='rootPart' xs.ref='part'/>
        </CModelSource>
        <CArtMeshSource xs.id='mesh'>
          <ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/><CArtMeshId xs.n='id' idstr='mesh'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
          <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1</float-array>
          <int-array xs.n='indices' count='3'>0 1 2</int-array><GTexture2D xs.n='texture' xs.ref='texture'/>
          <carray_list xs.n='keyforms' count='1'><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
            <CFormGuid xs.n='guid' uuid='form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>
          </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType><f xs.n='opacity'>1</f>
          </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
        </CArtMeshSource>
        <CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/>
          <carray_list xs.n='_childGuids' count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/></carray_list>
          <carray_list xs.n='keyforms' count='0'/></CPartSource>
        <CPartSource xs.id='arm'><CPartGuid xs.n='guid' uuid='arm-guid'/><CPartId xs.n='id' idstr='arm'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/>
          <carray_list xs.n='_childGuids' count='0'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
        <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
          <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
        </GTexture2D></root>"#.into()
    }

    #[test]
    fn a_source_moves_to_another_part_and_back() {
        let original = xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let moved = reparent(&initial, &json!({"guid": "mesh-guid", "parent": "arm-guid"})).unwrap();
        let bytes = rewrite(&container, &initial, &moved).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        assert_eq!(written, original
            .replace("<CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>",
                "<CPartGuid xs.n='parentGuid' uuid='arm-guid'/></ACDrawableSource>")
            .replace("count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/>", "count='1'><CPartGuid uuid='arm-guid'/>")
            .replace("<carray_list xs.n='_childGuids' count='0'/>",
                "<carray_list xs.n='_childGuids' count='1'><CArtMeshGuid uuid='mesh-guid'/></carray_list>"));

        // Moving it back restores the old parent; it now sits after Arm in the root's list.
        let reopened = CaffDocument::parse(&bytes).unwrap();
        let after = authoring_scene::read(&reopened).unwrap();
        let back = reparent(&after, &json!({"guid": "mesh-guid", "parent": "part-guid"})).unwrap();
        let root = back["sources"].as_array().unwrap().iter().find(|s| s["guid"] == "part-guid").unwrap();
        assert_eq!(root["data"]["childGuids"], json!(["arm-guid", "mesh-guid"]));
        rewrite(&reopened, &after, &back).unwrap();

        assert!(reparent(&initial, &json!({"guid": "part-guid", "parent": "arm-guid"})).unwrap_err().contains("root"));
        assert!(reparent(&initial, &json!({"guid": "arm-guid", "parent": "arm-guid"})).unwrap_err().contains("itself"));
        assert!(reparent(&initial, &json!({"guid": "arm-guid", "parent": "mesh-guid"})).unwrap_err().contains("Part"));
        let shared = test_container(&original.replace("<CArtMeshGuid uuid='mesh-guid'/>", "<CArtMeshGuid xs.id='g1' uuid='mesh-guid'/>"));
        let shared_initial = authoring_scene::read(&shared).unwrap();
        let shared_move = reparent(&shared_initial, &json!({"guid": "mesh-guid", "parent": "arm-guid"})).unwrap();
        assert!(rewrite(&shared, &shared_initial, &shared_move).unwrap_err().contains("shared GUID"));
    }

    const ROTATION_XML: &str = include_str!("../tests/fixtures/rotation-reparent.xml");

    #[test]
    fn deformer_reparent_writes_only_the_target_token_and_form_positions() {
        for reference in [true, false] {
            let original = if reference { ROTATION_XML.to_owned() } else {
                ROTATION_XML.replace("<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>",
                    "<CDeformerGuid xs.n='targetDeformerGuid' uuid='old-guid'/>")
            };
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let candidate = reparent_deformer(&initial, &json!({"sourceGuid":"mesh-guid","targetDeformerGuid":"new-child-guid"})).unwrap();
            let mesh = candidate["sources"].as_array().unwrap().iter().find(|s| s["guid"] == "mesh-guid").unwrap();
            let positions = mesh["forms"][0]["data"]["positions"].as_array().unwrap();
            for (actual, expected) in positions.iter().zip([-45.25,37.25,-47.25,39.25,-49.25,41.25]) {
                assert!((actual.as_f64().unwrap() - expected).abs() < 1.0e-12);
            }
            let bytes = rewrite(&container, &initial, &candidate).unwrap();
            let reopened = CaffDocument::parse(&bytes).unwrap();
            assert_eq!(authoring_scene::read(&reopened).unwrap(), candidate);
            let positions = positions.iter().map(|n| n.as_f64().unwrap().to_string()).collect::<Vec<_>>().join(" ");
            let expected = if reference {
                original.replace("<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>",
                    "<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='new-child-guid-def'/>")
            } else {
                original.replace("<CDeformerGuid xs.n='targetDeformerGuid' uuid='old-guid'/>",
                    "<CDeformerGuid xs.n='targetDeformerGuid' uuid='new-child-guid'/>")
            }.replace("1 2 3 4 5 6", &positions);
            assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), expected);
            for entry in container.entries().iter().filter(|e| e.name != "main.xml") {
                assert_eq!(reopened.entry_data(&entry.name), container.entry_data(&entry.name));
            }
            assert_eq!(mesh["parentPartGuid"], "part-guid");
            assert_eq!(mesh["forms"][0]["opacity"], 0.75);
            assert_eq!(reparent_deformer(&candidate, &json!({"sourceGuid":"mesh-guid","targetDeformerGuid":"new-child-guid"})).unwrap(), candidate);
        }
    }

    #[test]
    fn deformer_reparent_refuses_shared_link_paths_and_preserves_reference_representation() {
        let target = "<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>";
        let owner_start = ROTATION_XML.find("<ACDrawableSource xs.n='super'>").unwrap();
        let owner_end = owner_start + ROTATION_XML[owner_start..].find("</ACDrawableSource>").unwrap() + "</ACDrawableSource>".len();
        let owner = &ROTATION_XML[owner_start..owner_end];
        let referenced = ROTATION_XML.replace(owner, "<ACDrawableSource xs.n='super' xs.ref='shared-owner'/>")
            .replace("</root>", &format!("{}</root>", owner.replace("xs.n='super'", "xs.id='shared-owner'")));
        let missing_id = ROTATION_XML
            .replace("<CRotationDeformerGuid xs.n='guid' xs.ref='new-child-guid-def'/>",
                "<CRotationDeformerGuid xs.n='guid' uuid='new-child-guid'/>")
            .replace("<CRotationDeformerGuid xs.id='new-child-guid-def' uuid='new-child-guid'/>", "");
        for (label, xml) in [
            ("target definition", ROTATION_XML.replace(target, "<CDeformerGuid xs.n='targetDeformerGuid' xs.id='target-slot' xs.ref='old-guid-def'/>")),
            ("mixed target", ROTATION_XML.replace(target, "<CDeformerGuid xs.n='targetDeformerGuid' uuid='old-guid' xs.ref='old-guid-def'/>")),
            ("shared owner", ROTATION_XML.replacen("<ACDrawableSource xs.n='super'>", "<ACDrawableSource xs.n='super' xs.id='shared-owner'>", 1)
                .replace("</root>", "<Opaque xs.ref='shared-owner'/></root>")),
            ("referenced owner", referenced),
            ("missing destination identity", missing_id),
            ("escaped link", ROTATION_XML.replace(target, "<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-&#100;ef'/>")),
            ("shared positions", ROTATION_XML.replace("<float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array>",
                "<float-array xs.n='positions' xs.id='shared-positions' count='6'>1 2 3 4 5 6</float-array>")
                .replace("</root>", "<float-array xs.ref='shared-positions'/></root>")),
        ] {
            let container = test_container(&xml);
            let initial = authoring_scene::read(&container).unwrap_or_else(|error| panic!("{label}: {error}"));
            let candidate = reparent_deformer(&initial, &json!({"sourceGuid":"mesh-guid","targetDeformerGuid":"new-child-guid"})).unwrap();
            assert!(rewrite(&container,&initial,&candidate).is_err(), "accepted {label}");
            assert_eq!(container.main_xml(), xml.as_bytes());
        }
    }
}
