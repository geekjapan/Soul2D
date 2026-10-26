//! Editing the clipping masks of a CMO3 ArtMesh, Part or ArtPath (`clipGuidList`, issue #144). Masks that leave
//! are removed from the stored list and new ones are appended, each as a reference to the mask ArtMesh's own GUID
//! element (`xs.ref` when it is a shared definition, its `uuid` otherwise); the count follows. A source with no list
//! gets an emptied copy of the inline list another source of the same kind stores, placed after the field that
//! precedes it there (`authoring_fields::layouts`); it is refused when no source of that kind stores one. A shared
//! list, or an entry that defines a shared `xs.id`, is refused. `authoring_edit::rewrite` re-reads the result and
//! refuses it unless it equals the requested model.

use crate::authoring_edit::XmlPatch;
use crate::authoring_fields::layouts;
use crate::authoring_graph::Graph;
use crate::authoring_reparent::{append_patch, attribute_patch, source_nodes, stored_field};
use crate::authoring_values::{self as values, Budget};
use roxmltree::Node;
use serde_json::{json, Value};
use std::ops::Range;

/// The masks one source lost and gained.
pub(crate) struct ClipChange {
    source: String,
    removed: Vec<String>,
    added: Vec<String>,
}

/// The clip list changes from `initial` to `model`, and `model` with them undone. A new list must be the kept
/// masks in their old order followed by the added ones.
pub(crate) fn changes(initial: &Value, model: &Value) -> Result<(Vec<ClipChange>, Value), String> {
    let mut normalized = model.clone();
    let mut found = Vec::new();
    let (Some(old_sources), Some(new_sources)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok((found, normalized));
    };
    for (index, (old, new)) in old_sources.iter().zip(new_sources).enumerate() {
        let (old_clips, new_clips) = (&old["data"]["clipGuids"], &new["data"]["clipGuids"]);
        if old["guid"] != new["guid"] || old_clips == new_clips {
            continue;
        }
        let (Some(source), Some(before), Some(after)) = (old["guid"].as_str(), old_clips.as_array(), new_clips.as_array())
        else {
            return Err("a clipping mask list must stay a list of GUIDs".into());
        };
        let kept: Vec<&Value> = before.iter().filter(|clip| after.contains(clip)).collect();
        let (head, tail) = after.split_at(kept.len().min(after.len()));
        if !head.iter().eq(kept.iter().copied()) || tail.iter().any(|clip| before.contains(clip)) {
            return Err("new clipping masks go after the kept ones, in their old order".into());
        }
        let text = |clips: &mut dyn Iterator<Item = &Value>| -> Result<Vec<String>, String> {
            clips.map(|clip| clip.as_str().map(str::to_owned).ok_or_else(|| "a clipping mask is not a GUID".to_string()))
                .collect()
        };
        found.push(ClipChange {
            source: source.to_owned(),
            removed: text(&mut before.iter().filter(|clip| !after.contains(clip)))?,
            added: text(&mut tail.iter())?,
        });
        normalized["sources"][index]["data"]["clipGuids"] = old_clips.clone();
    }
    Ok((found, normalized))
}

pub(crate) fn patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    changes: &[ClipChange],
    budget: &mut Budget,
) -> Result<Vec<XmlPatch>, String> {
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let sources = source_nodes(graph, model)?;
    let find = |guid: &str| sources.get(guid).copied().ok_or_else(|| format!("source {guid} was not found in the CMO3"));
    let mut patches = Vec::new();
    for change in changes {
        let source = find(&change.source)?;
        let mut arriving = String::new();
        for added in &change.added {
            let stored = stored_field(graph, find(added)?, "guid")?.ok_or("a mask stores no guid")?;
            let definition = graph.resolve(stored)?;
            let tag = definition.tag_name().name();
            match definition.attribute("xs.id") {
                Some(id) if id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_#.".contains(&b)) => {
                    arriving.push_str(&format!("<{tag} xs.ref=\"{id}\"/>"));
                }
                None if added.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => {
                    arriving.push_str(&format!("<{tag} uuid=\"{added}\"/>"));
                }
                _ => return Err("a mask GUID cannot be written as a plain reference".into()),
            }
        }
        // The reader shows no list as an empty one, so a source without one has nothing to remove.
        let Some(list) = stored_field(graph, source, "clipGuidList")? else {
            patches.push(inserted_list(text, graph, source, &arriving, change.added.len())?);
            continue;
        };
        if list.attribute("xs.ref").is_some() || !list.has_tag_name("carray_list") {
            return Err("this source's clipGuidList is not an inline list".into());
        }
        for removed in &change.removed {
            let mut entries = Vec::new();
            for entry in list.children().filter(|entry| entry.is_element()) {
                if graph.resolve(entry)?.attribute("uuid") == Some(removed.as_str()) {
                    entries.push(entry);
                }
            }
            let [entry] = entries[..] else {
                return Err("the clipping mask list does not name the removed mask once".into());
            };
            if entry.attribute("xs.id").is_some() {
                return Err("a removed clipping mask entry defines a shared GUID".into());
            }
            patches.push(XmlPatch { range: entry.range(), value: String::new() });
        }
        let count = values::list(graph, list, budget)?.len() + change.added.len() - change.removed.len();
        if list.attribute("count").is_some() {
            patches.push(attribute_patch(text, list, "count", count.to_string())?);
        }
        if !arriving.is_empty() {
            patches.push(append_patch(text, list, arriving)?);
        }
    }
    Ok(patches)
}

/// The patch that writes the clip list `source` lacks: an emptied copy of one another source of the same kind stores,
/// holding `entries` (already XML) and counting `count`.
fn inserted_list<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    source: Node<'d, 'i>,
    entries: &str,
    count: usize,
) -> Result<XmlPatch, String> {
    let (donor, end) = layouts(graph, source, "clipGuidList")?.into_iter()
        .find(|(donor, _)| donor.has_tag_name("carray_list"))
        .ok_or("the CMO3 stores no clipGuidList anywhere to copy its layout from")?;
    let range = donor.range();
    let slice = |span: Range<usize>| text.get(span).ok_or("a list range is invalid");
    let mut value = String::new();
    let mut at = range.start;
    if let Some(attribute) = donor.attributes().find(|attribute| attribute.name() == "count") {
        let span = attribute.range_value();
        value.push_str(slice(at..span.start)?);
        value.push_str(&count.to_string());
        at = span.end;
    }
    if let (Some(first), Some(last)) = (donor.first_child(), donor.last_child()) {
        value.push_str(slice(at..first.range().start)?);
        at = last.range().end;
    }
    let tail = slice(at..range.end)?;
    if let Some(open) = tail.strip_suffix("/>") {
        value.push_str(&format!("{open}>{entries}</carray_list>"));
    } else if let Some(open) = tail.strip_suffix("</carray_list>") {
        value.push_str(&format!("{open}{entries}</carray_list>"));
    } else {
        return Err("a list does not end the way Soul2D writes".into());
    }
    Ok(XmlPatch { range: end..end, value })
}

/// Checks a requested mask list for `source`: ArtMesh GUIDs of the document, each once, not the source itself.
pub(crate) fn mask_list(model: &Value, source: &str, value: &Value) -> Result<Value, String> {
    let list = value.as_array().ok_or("clipGuids must be a list of ArtMesh GUIDs")?;
    let mut seen = Vec::new();
    for clip in list {
        let guid = clip.as_str().ok_or("clipGuids must be a list of ArtMesh GUIDs")?;
        if guid == source || seen.contains(&guid) {
            return Err("clipGuids names the source itself or a mask twice".into());
        }
        let is_mesh = model["sources"].as_array().into_iter().flatten()
            .any(|candidate| candidate["guid"].as_str() == Some(guid) && candidate["data"]["type"] == "ArtMesh");
        if !is_mesh {
            return Err(format!("clipping mask {guid} is not an ArtMesh of this document"));
        }
        seen.push(guid);
    }
    Ok(json!(seen))
}

#[cfg(test)]
mod tests {
    use crate::authoring_edit::{rewrite, tests::test_container};
    use crate::authoring_fields::set_source;
    use crate::authoring_scene;
    use crate::caff::CaffDocument;
    use serde_json::json;

    /// The reparent fixture plus a second ArtMesh, Mask, whose GUID is a shared definition, and an empty clip list on
    /// the first one; Mask and the Part `Arm` store no clip list.
    fn fixture() -> String {
        let base = crate::authoring_reparent::tests::xml();
        let start = base.find("<CArtMeshSource xs.id='mesh'>").unwrap();
        let end = base.find("</CArtMeshSource>").unwrap() + "</CArtMeshSource>".len();
        let mask = base[start..end].replace("xs.id='mesh'", "xs.id='mask'").replace("xs.ref='mesh'", "xs.ref='mask'")
            .replace("<CArtMeshGuid xs.n='guid' uuid='mesh-guid'/>", "<CArtMeshGuid xs.n='guid' xs.id='mask-guid-def' uuid='mask-guid'/>")
            .replace("idstr='mesh'", "idstr='mask'").replace("uuid='form-guid'", "uuid='mask-form'");
        base
            .replace("count='1'><CArtMeshSource xs.ref='mesh'/>", "count='2'><CArtMeshSource xs.ref='mesh'/><CArtMeshSource xs.ref='mask'/>")
            .replace("<int-array xs.n='indices' count='3'>0 1 2</int-array>",
                "<int-array xs.n='indices' count='3'>0 1 2</int-array><carray_list xs.n='clipGuidList' count='0'/>")
            .replace("</GTexture2D></root>", &format!("</GTexture2D>{mask}</root>"))
            .replace("<CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/>",
                "<CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/><CArtMeshGuid uuid='mask-guid'/>")
            .replace("_childGuids' count='2'", "_childGuids' count='3'")
    }

    #[test]
    fn clipping_masks_are_added_and_removed_in_the_stored_list() {
        let original = fixture();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let masked = set_source(&initial, &json!({"guid": "mesh-guid", "clipGuids": ["mask-guid"]})).unwrap();
        let bytes = rewrite(&container, &initial, &masked).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        let clipped = "<carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid xs.ref=\"mask-guid-def\"/></carray_list>";
        assert_eq!(written.matches(clipped).count(), 1, "{written}");
        assert_eq!(written.replace(clipped, "<carray_list xs.n='clipGuidList' count='0'/>"), original);

        let reopened = CaffDocument::parse(&bytes).unwrap();
        let after = authoring_scene::read(&reopened).unwrap();
        let cleared = set_source(&after, &json!({"guid": "mesh-guid", "clipGuids": []})).unwrap();
        let bytes = rewrite(&reopened, &after, &cleared).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        assert!(written.contains("<carray_list xs.n='clipGuidList' count='0'></carray_list>"), "{written}");

        assert!(set_source(&initial, &json!({"guid": "mesh-guid", "clipGuids": ["mesh-guid"]})).unwrap_err().contains("itself"));
        assert!(set_source(&initial, &json!({"guid": "mesh-guid", "clipGuids": ["arm-guid"]})).unwrap_err().contains("ArtMesh"));
    }

    /// The error `rewrite` gives for `guid` getting `masks` in the file `xml`.
    fn refusal(xml: &str, guid: &str, masks: &[&str]) -> String {
        let container = test_container(xml);
        let initial = authoring_scene::read(&container).unwrap();
        let edited = set_source(&initial, &json!({"guid": guid, "clipGuids": masks})).unwrap();
        rewrite(&container, &initial, &edited).unwrap_err()
    }

    #[test]
    fn a_missing_clip_list_is_copied_from_a_source_of_the_same_kind() {
        let original = fixture();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let masked = set_source(&initial, &json!({"guid": "mask-guid", "clipGuids": ["mesh-guid"]})).unwrap();
        let bytes = rewrite(&container, &initial, &masked).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        // The list goes right after the `indices` field of Mask, where the first ArtMesh stores its own.
        let indices = "<int-array xs.n='indices' count='3'>0 1 2</int-array>";
        let list = "<carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid uuid=\"mesh-guid\"/></carray_list>";
        let (before, after) = original.split_once("xs.id='mask'").unwrap();
        assert_eq!(written, format!("{before}xs.id='mask'{}", after.replacen(indices, &format!("{indices}{list}"), 1)));
        let after = authoring_scene::read(&CaffDocument::parse(&bytes).unwrap()).unwrap();
        let mask = after["sources"].as_array().unwrap().iter().find(|source| source["guid"] == "mask-guid").unwrap();
        assert_eq!(mask["data"]["clipGuids"], json!(["mesh-guid"]));

        // Once the first ArtMesh holds a mask, the copy is emptied before the new entry goes in.
        let masked = set_source(&initial, &json!({"guid": "mesh-guid", "clipGuids": ["mask-guid"]})).unwrap();
        let reopened = CaffDocument::parse(&rewrite(&container, &initial, &masked).unwrap()).unwrap();
        let held = authoring_scene::read(&reopened).unwrap();
        let masked = set_source(&held, &json!({"guid": "mask-guid", "clipGuids": ["mesh-guid"]})).unwrap();
        let bytes = rewrite(&reopened, &held, &masked).unwrap();
        let written = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        let clipped = "<carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid xs.ref=\"mask-guid-def\"/></carray_list>";
        assert_eq!(written.matches(clipped).count(), 1, "{written}");
        assert_eq!(written.matches(list).count(), 1, "{written}");
        assert_eq!(written.matches("mask-guid-def").count(), 2, "{written}");
    }

    #[test]
    fn a_missing_clip_list_is_refused_without_a_source_of_the_same_kind_to_copy() {
        let original = fixture();
        let none = "stores no clipGuidList anywhere";
        // A Part does not borrow the layout of an ArtMesh.
        assert!(refusal(&original, "arm-guid", &["mesh-guid"]).contains(none));
        // No ArtMesh stores a list.
        let bare = original.replace("<carray_list xs.n='clipGuidList' count='0'/>", "");
        assert!(refusal(&bare, "mesh-guid", &["mask-guid"]).contains(none));
        // A list shared through `xs.ref` is not edited in place.
        let shared = original.replace("<carray_list xs.n='clipGuidList' count='0'/>",
            "<carray_list xs.n='clipGuidList' xs.id='shared' count='0'/>");
        let (before, after) = shared.split_once("xs.id='mask'").unwrap();
        let indices = "<int-array xs.n='indices' count='3'>0 1 2</int-array>";
        let shared = format!("{before}xs.id='mask'{}", after.replacen(indices,
            &format!("{indices}<carray_list xs.n='clipGuidList' xs.ref='shared'/>"), 1));
        assert!(refusal(&shared, "mask-guid", &["mesh-guid"]).contains("is not an inline list"));

        // Once the root Part stores a list, the Part `Arm` gets a copy right after its `_childGuids`.
        let root_list = "</carray_list>\n          <carray_list xs.n='keyforms' count='0'/></CPartSource>";
        assert!(original.contains(root_list), "the root Part ends with its keyforms");
        let parted = original.replacen(root_list, &root_list.replacen("</carray_list>\n",
            "</carray_list><carray_list xs.n='clipGuidList' count='0'/>\n", 1), 1);
        let container = test_container(&parted);
        let initial = authoring_scene::read(&container).unwrap();
        let masked = set_source(&initial, &json!({"guid": "arm-guid", "clipGuids": ["mesh-guid"]})).unwrap();
        let bytes = rewrite(&container, &initial, &masked).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        let childless = "<carray_list xs.n='_childGuids' count='0'/>";
        let list = "<carray_list xs.n='clipGuidList' count='1'><CArtMeshGuid uuid=\"mesh-guid\"/></carray_list>";
        assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), parted.replacen(childless, &format!("{childless}{list}"), 1));
        assert_eq!(authoring_scene::read(&reopened).unwrap(), masked);
    }
}
