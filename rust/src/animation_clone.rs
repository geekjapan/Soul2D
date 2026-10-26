//! New CAN3 curves, model tracks, scenes and linked-model resources written as clones of ones the same file already stores (issue #145), so they
//! take the layout Cubism gives that file: inherited (`super`) fields, shared GUID objects, effects, cached sequence
//! fields and serial numbers. Only donors in Cubism's layout are used; without one the caller writes its minimal
//! template.

use crate::animation_source::{escape, free_storage_id, new_guid, serialize_point, tag_xml, NativeAdditions};
use roxmltree::Node;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// `node`'s text with each `xs.id`/`xs.ref` that `ids` and each `uuid` that `uuids` names renamed, each `xs.idx`
/// serial taken fresh, and each element `replace` answers for written as that answer instead of itself.
fn clone_text(
    text: &str,
    node: Node<'_, '_>,
    ids: &HashMap<&str, String>,
    uuids: &HashMap<&str, String>,
    next_idx: &mut Option<u64>,
    replace: &dyn Fn(Node<'_, '_>) -> Option<String>,
) -> Result<String, String> {
    let range = node.range();
    let mut out = String::with_capacity(range.len());
    let mut at = range.start;
    for element in node.descendants().filter(|element| element.is_element()) {
        if element.range().start < at {
            continue;
        }
        if let Some(xml) = replace(element) {
            out.push_str(&text[at..element.range().start]);
            out.push_str(&xml);
            at = element.range().end;
            continue;
        }
        for attribute in element.attributes() {
            let new = match attribute.name() {
                "xs.id" | "xs.ref" => ids.get(attribute.value()).cloned(),
                "uuid" => uuids.get(attribute.value()).cloned(),
                "xs.idx" => next_idx.as_mut().map(|idx| {
                    *idx += 1;
                    idx.to_string()
                }),
                _ => None,
            };
            let Some(new) = new else { continue };
            let value = attribute.range_value();
            if text.get(value.clone()) != Some(attribute.value()) {
                return Err("a cloned identifier is stored escaped".into());
            }
            out.push_str(&text[at..value.start]);
            out.push_str(&escape(&new));
            at = value.end;
        }
    }
    out.push_str(&text[at..range.end]);
    Ok(out)
}

/// Whether `element` is the field `name` of `owner`, written on it or on one of its inherited `super` parts.
fn owned_field(element: Node<'_, '_>, owner: Node<'_, '_>, name: &str) -> bool {
    if element.attribute("xs.n") != Some(name) {
        return false;
    }
    let mut parent = element.parent_element();
    while let Some(node) = parent {
        if node == owner {
            return true;
        }
        if node.attribute("xs.n") != Some("super") {
            return false;
        }
        parent = node.parent_element();
    }
    false
}

pub(crate) fn string_patch(xml: &str, field: Node<'_, '_>, value: &str) -> Result<(std::ops::Range<usize>, String), String> {
    let children: Vec<_> = field.children().collect();
    Ok(match children.as_slice() {
        [text] if text.is_text() => (text.range(), escape(value)),
        [] if xml[field.range()].ends_with("/>") =>
            (field.range().end - 2..field.range().end, format!(">{}</{}>", escape(value), field.tag_name().name())),
        [] => {
            let end = field.range().start + xml[field.range()].rfind("</").ok_or("cloned string lacks its closing tag")?;
            (end..end, escape(value))
        }
        _ => return Err("cloned string has unsupported content".into()),
    })
}

fn shared_section<'d, 'i>(node: Node<'d, 'i>) -> Option<Node<'d, 'i>> {
    node.document().root_element().children().find(|child| child.has_tag_name("shared"))
}

/// A top-level object for a new GUID, in the form Cubism writes them.
fn guid_object_xml(tag: &str, uuid: &str, id: &str, next_idx: &mut Option<u64>) -> String {
    let serial = next_idx.as_mut().map(|idx| {
        *idx += 1;
        format!(r#" xs.idx="{idx}""#)
    }).unwrap_or_default();
    format!(r#"<{tag} uuid="{}" note="(no debug info)" xs.id="{}"{serial} />"#, escape(uuid), escape(id))
}

/// How a kind of curve names its target: the attribute ID prefix, the model fields for the target's ID and GUID, the
/// option that names the target, and the prefix of the attribute-map key.
pub(crate) struct CurveKind {
    prefix: &'static str,
    target: &'static str,
    guid: &'static str,
    key_name: &'static str,
    key_tag: &'static str,
    map_prefix: &'static str,
}

pub(crate) const PARAMETER: CurveKind = CurveKind {
    prefix: "live2dParam_", target: "parameterId", guid: "parameterGuid", key_name: "KEY_PARAM_ID",
    key_tag: "CParameterId", map_prefix: "live2dParam:",
};

pub(crate) const PART: CurveKind = CurveKind {
    prefix: "live2DPartsOpacity_", target: "partId", guid: "partGuid", key_name: "KEY_PARTS_VISIBLE_ID",
    key_tag: "CPartId", map_prefix: "live2DPartsOpacity:",
};

/// A curve of `kind` in Cubism's layout to clone: a shared CMvAttrF with inherited fields, preferring one that names
/// its target by a typed ID, as the model does, and then one of the track stored as `track_id`.
fn curve_donor<'d, 'i>(anywhere: Node<'d, 'i>, track_id: &str, kind: &CurveKind) -> Option<Node<'d, 'i>> {
    let shared = shared_section(anywhere)?;
    let track_id = Some(track_id);
    let candidates = shared.children().filter(|node| {
        node.has_tag_name("CMvAttrF")
            && node.children().any(|child| child.has_tag_name("ICMvAttr") && child.attribute("xs.n") == Some("super"))
            && node.descendants().any(|field| owned_field(field, *node, "id")
                && field.attribute("idstr").is_some_and(|id| id.starts_with(kind.prefix)))
            && node.descendants().any(|field| owned_field(field, *node, "valueData") && field.has_tag_name("CMutableSequence"))
    });
    candidates.max_by_key(|candidate| {
        let typed = candidate.descendants().any(|node| node.attribute("xs.n") == Some(kind.key_name)
            && node.has_tag_name(kind.key_tag));
        let of_track = candidate.descendants().any(|field| owned_field(field, *candidate, "track")
            && field.attribute("xs.ref") == track_id);
        // `max_by_key` keeps the last of equals; the rank of the document order breaks ties towards the first.
        (typed, of_track, std::cmp::Reverse(candidate.range().start))
    })
}

/// A new curve stored like a donor, and the attribute-map entry naming it when the effect's map lists every curve of
/// the track.
pub(crate) struct CurveClone {
    pub(crate) definition: String,
    pub(crate) map_entry: Option<String>,
}

/// Writes `curve` as a clone of a curve of `kind` in this file (`anywhere` is any node of it), owned by the track
/// stored as `track_id` and stored as `storage`; `None` when the file has no such curve in Cubism's layout, or the
/// donor and a parameter `curve` differ in whether they store a baseValue.
#[allow(clippy::too_many_arguments)]
pub(crate) fn curve(
    text: &str,
    anywhere: Node<'_, '_>,
    kind: &CurveKind,
    track_id: &str,
    map_complete: bool,
    curve: &Value,
    storage: &str,
    additions: &mut NativeAdditions,
) -> Result<Option<CurveClone>, String> {
    let Some(donor) = curve_donor(anywhere, track_id, kind) else { return Ok(None) };
    let donor_id = donor.attribute("xs.id").ok_or("a donor curve lacks shared storage")?;
    if donor.descendants().any(|node| node != donor && node.attribute("xs.id").is_some()) {
        return Ok(None);
    }
    // In the supplied files a stored baseValue equals the parameter's default (3,586 of 3,586 curves); the clone
    // writes the model's value, so the copy and the model must agree on whether the curve stores one, or the donor
    // is not copied.
    let base_value = curve.get("baseValue")
        .map(|base| base.as_f64().filter(|base| base.is_finite()).ok_or("animation curve baseValue must be finite"))
        .transpose()?;
    let sequence = donor.descendants().find(|node| owned_field(*node, donor, "valueData"))
        .ok_or("a donor curve lacks a sequence")?;
    if kind.target == PARAMETER.target && base_value.is_some() != field(sequence, "baseValue").is_some() {
        return Ok(None);
    }
    let donor_attribute_id = field(donor, "id").and_then(|id| id.attribute("idstr")).ok_or("a donor curve lacks an ID")?;
    let text_of = |key: &str| curve[key].as_str().ok_or_else(|| format!("a new animation curve lacks {key}"));
    let (id, parameter, parameter_guid) = (text_of("id")?, text_of(kind.target)?, text_of(kind.guid)?);
    let range = [curve["range"][0].as_f64(), curve["range"][1].as_f64()];
    let [Some(minimum), Some(maximum)] = range else { return Err("a new animation curve lacks a range".into()) };
    let points = curve["points"].as_array().ok_or("a new animation curve lacks points")?;
    let types = curve["curveTypes"].as_array().ok_or("a new animation curve lacks curve types")?;
    let values: Vec<f64> = points.iter().filter_map(|point| point["anchor"]["value"].as_f64()).collect();
    let frames: Vec<i64> = points.iter().filter_map(|point| point["anchor"]["frame"].as_i64()).collect();
    if values.len() != points.len() || frames.len() != points.len() || types.len() != points.len() {
        return Err("a new animation curve has malformed keys".into());
    }
    // The parameter's GUID object is shared by every curve of that parameter; a new one is written when absent.
    let guid_field = donor.descendants().find(|node| owned_field(*node, donor, "guid"))
        .ok_or("a donor curve lacks a GUID")?;
    let guid = if guid_field.attribute("xs.ref").is_some() {
        let shared = shared_section(donor).ok_or("animation XML lacks a shared section")?;
        let existing = shared.children().find(|node| node.has_tag_name(guid_field.tag_name().name())
            && node.attribute("uuid") == Some(parameter_guid));
        let reference = match existing.and_then(|node| node.attribute("xs.id")) {
            Some(id) => id.to_owned(),
            None => {
                let id = free_storage_id(&mut additions.reserved, &format!("soul2d-guid-{parameter_guid}"));
                let object = guid_object_xml(guid_field.tag_name().name(), parameter_guid, &id, &mut additions.next_idx);
                additions.definitions.push_str(&object);
                id
            }
        };
        format!(r#"<{} xs.n="guid" xs.ref="{}"/>"#, guid_field.tag_name().name(), escape(&reference))
    } else {
        format!(r#"<{} xs.n="guid" uuid="{}"/>"#, guid_field.tag_name().name(), escape(parameter_guid))
    };
    // Cubism names a curve after the parameter's display name, which another curve of the parameter carries.
    let shared = shared_section(donor).ok_or("animation XML lacks a shared section")?;
    let sibling_name = shared.children().filter(|node| node.has_tag_name("CMvAttrF"))
        .find(|node| field(*node, "id").and_then(|id| id.attribute("idstr")) == Some(id))
        .and_then(|node| field(node, "name")).and_then(|name| name.text()).map(str::to_owned);
    let display_name = sibling_name.as_deref().unwrap_or(parameter);
    // A target's typed ID is one shared object when the donor names it by reference.
    let key_field = donor.descendants().find(|node| node.attribute("xs.n") == Some(kind.key_name));
    let key_xml = if key_field.is_some_and(|key| key.has_tag_name(kind.key_tag) && key.attribute("xs.ref").is_some()) {
        let existing = shared.children().find(|node| node.has_tag_name(kind.key_tag) && node.attribute("idstr") == Some(parameter))
            .and_then(|node| node.attribute("xs.id"));
        let reference = match existing {
            Some(id) => id.to_owned(),
            None => {
                let id = free_storage_id(&mut additions.reserved, &format!("soul2d-{}-{parameter}", kind.key_tag));
                let serial = additions.next_idx.as_mut().map(|idx| {
                    *idx += 1;
                    format!(r#" xs.idx="{idx}""#)
                }).unwrap_or_default();
                additions.definitions.push_str(&format!(r#"<{} idstr="{}" xs.id="{}"{serial} />"#,
                    kind.key_tag, escape(parameter), escape(&id)));
                id
            }
        };
        format!(r#"<{} xs.n="{}" xs.ref="{}" />"#, kind.key_tag, kind.key_name, escape(&reference))
    } else {
        format!(r#"<{} xs.n="{}" idstr="{}"/>"#, kind.key_tag, kind.key_name, escape(parameter))
    };
    let point_xml = points.iter().map(|point| serialize_point(point, true)).collect::<Result<String, String>>()?;
    let type_xml: String = types.iter().filter_map(Value::as_str).map(|kind| format!(r#"<CCurveType v="{kind}" />"#)).collect();
    let (low, high) = match values.iter().copied().reduce(f64::min).zip(values.iter().copied().reduce(f64::max)) {
        Some(extrema) => extrema,
        None => (minimum, maximum),
    };
    let scalar = |node: Node<'_, '_>, value: String| {
        format!(r#"<{tag} xs.n="{name}">{value}</{tag}>"#, tag = node.tag_name().name(),
            name = node.attribute("xs.n").unwrap_or_default())
    };
    let replace = |node: Node<'_, '_>| -> Option<String> {
        let name = node.attribute("xs.n")?;
        if owned_field(node, donor, name) {
            return match name {
                "id" => Some(format!(r#"<CAttrId xs.n="id" idstr="{}"/>"#, escape(id))),
                "name" => Some(format!(r#"<s xs.n="name">{}</s>"#, escape(display_name))),
                "guid" => Some(guid.clone()),
                "track" => Some(format!(r#"<{} xs.n="track" xs.ref="{}"/>"#, node.tag_name().name(), escape(track_id))),
                "rangeMin" => Some(scalar(node, minimum.to_string())),
                "rangeMax" => Some(scalar(node, maximum.to_string())),
                _ => None,
            };
        }
        if name == kind.key_name {
            return Some(key_xml.clone());
        }
        if !owned_field(node, sequence, name) {
            return None;
        }
        let bound = |frame: Option<&i64>| frame.copied().unwrap_or(0).to_string();
        match name {
            "points" => Some(format!(r#"<array xs.n="points" count="{}" type="CBezierPt">{point_xml}</array>"#, points.len())),
            "curveTypes" => Some(format!(r#"<carray_list xs.n="curveTypes" count="{}">{type_xml}</carray_list>"#, types.len())),
            "curMin" => Some(scalar(node, low.to_string())),
            "curMax" => Some(scalar(node, high.to_string())),
            "baseValue" => base_value.map(|base| scalar(node, base.to_string())),
            "keyPts2" => Some(r#"<null xs.n="keyPts2" />"#.to_owned()),
            "keyMin" => Some(scalar(node, bound(frames.first()))),
            "keyMax" => Some(scalar(node, bound(frames.last()))),
            "lastValue" => Some(scalar(node, "NaN".to_owned())),
            "lastPos" => Some(scalar(node, "-1".to_owned())),
            _ => None,
        }
    };
    let ids = HashMap::from([(donor_id, storage.to_owned())]);
    let definition = clone_text(text, donor, &ids, &HashMap::new(), &mut additions.next_idx, &replace)?;
    // Files key their attribute maps two ways: by the curve's own ID, or by the legacy `prefix:target` form. The
    // donor's own map entry decides which form the new curve gets.
    let donor_key = shared.descendants().filter(|node| node.has_tag_name("entry"))
        .find(|entry| entry.children().any(|node| node.attribute("xs.n") == Some("value") && node.attribute("xs.ref") == Some(donor_id)))
        .and_then(|entry| entry.children().find(|node| node.attribute("xs.n") == Some("key")))
        .and_then(|key| match key.attribute("xs.ref") {
            Some(reference) => shared.children().find(|node| node.attribute("xs.id") == Some(reference))?.attribute("idstr"),
            None => key.attribute("idstr"),
        });
    let map_key = match donor_key {
        Some(key) if key == donor_attribute_id => id.to_owned(),
        _ => format!("{}{}", kind.map_prefix, parameter),
    };
    let map_entry = map_complete.then(|| format!(
        r#"<entry><CAttrId xs.n="key" idstr="{}"/><CMvAttrF xs.n="value" xs.ref="{}"/></entry>"#,
        escape(&map_key), escape(storage)));
    Ok(Some(CurveClone { definition, map_entry }))
}

/// The effects a model track of Cubism's layout may carry; a donor with any other is not cloned.
const TRACK_EFFECTS: [&str; 5] = [
    "CMvEffect_Live2DParameter", "CMvEffect_Live2DPartsVisible", "CMvEffect_LipSync", "CMvEffect_EyeBlink",
    "CMvEffect_VisualDefault",
];

/// The top-level object of `shared` that holds `node`.
fn top_object<'d, 'i>(node: Node<'d, 'i>, shared: Node<'d, 'i>) -> Option<Node<'d, 'i>> {
    node.ancestors().find(|ancestor| ancestor.parent_element() == Some(shared))
}

/// The field `name` of `owner`, written on it or on one of its inherited `super` parts.
fn field<'d, 'i>(owner: Node<'d, 'i>, name: &str) -> Option<Node<'d, 'i>> {
    owner.descendants().find(|node| owned_field(*node, owner, name))
}

/// Each `xs.id` of `shared`, by the top-level object that holds it, and every element naming each one.
struct Index<'d, 'i> {
    by_id: HashMap<&'d str, Node<'d, 'i>>,
    refs: HashMap<&'d str, Vec<Node<'d, 'i>>>,
}

fn index<'d, 'i>(shared: Node<'d, 'i>) -> Index<'d, 'i> {
    let mut by_id = HashMap::new();
    for object in shared.children().filter(|node| node.is_element()) {
        for node in object.descendants().filter(|node| node.is_element()) {
            if let Some(id) = node.attribute("xs.id") {
                by_id.insert(id, object);
            }
        }
    }
    let mut refs: HashMap<&str, Vec<Node>> = HashMap::new();
    for node in shared.document().descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("xs.ref") {
            refs.entry(id).or_default().push(node);
        }
    }
    Index { by_id, refs }
}

/// The top-level objects `roots` reach through references, other than `skip` and objects tagged as in `stop`,
/// less every one that something outside them names too: the objects a clone of `roots` gets its own copy of.
fn owned_objects<'d, 'i>(
    roots: &[Node<'d, 'i>],
    shared: Node<'d, 'i>,
    by_id: &HashMap<&str, Node<'d, 'i>>,
    refs: &HashMap<&str, Vec<Node<'d, 'i>>>,
    stop: &[&str],
    skip: &HashSet<Node<'d, 'i>>,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let mut owned = roots.to_vec();
    let mut next = 0;
    while next < owned.len() {
        let targets: Vec<&str> = owned[next].descendants().filter_map(|node| node.attribute("xs.ref")).collect();
        next += 1;
        for target in targets {
            let Some(&object) = by_id.get(target) else { return Err(format!("xs.ref {target} has no definition")) };
            if !owned.contains(&object) && !skip.contains(&object) && !stop.contains(&object.tag_name().name()) {
                owned.push(object);
            }
        }
    }
    loop {
        let kept: Vec<Node<'d, 'i>> = owned.iter().copied().filter(|object| roots.contains(object) || object.descendants()
            .filter_map(|node| node.attribute("xs.id")).flat_map(|id| refs.get(id).into_iter().flatten())
            .all(|reference| top_object(*reference, shared).is_some_and(|holder| owned.contains(&holder) || skip.contains(&holder))))
            .collect();
        if kept.len() == owned.len() {
            return Ok(owned);
        }
        owned = kept;
    }
}

/// A model track of Cubism's layout to clone, preferring one of the scene stored as `scene_id`.
fn track_donor<'d, 'i>(anywhere: Node<'d, 'i>, scene_id: &str) -> Option<Node<'d, 'i>> {
    let shared = shared_section(anywhere)?;
    let by_id: HashMap<&str, Node<'d, 'i>> = shared.children().filter_map(|node| Some((node.attribute("xs.id")?, node))).collect();
    let candidates = shared.children().filter(|node| {
        node.has_tag_name("CMvTrack_Live2DModel_Source")
            && node.children().any(|child| child.has_tag_name("ICMvTrack_Linked") && child.attribute("xs.n") == Some("super"))
            && field(*node, "effectManager").and_then(|manager| field(manager, "effectList")).is_some_and(|list| {
                list.children().filter(|member| member.is_element()).all(|member| {
                    member.attribute("xs.ref").and_then(|id| by_id.get(id))
                        .is_some_and(|effect| TRACK_EFFECTS.contains(&effect.tag_name().name()))
                })
            })
    });
    let mut first = None;
    for candidate in candidates {
        if field(candidate, "_sceneSource").and_then(|scene| scene.attribute("xs.ref")) == Some(scene_id) {
            return Some(candidate);
        }
        first.get_or_insert(candidate);
    }
    first
}

/// A new model track stored like a model track of this file, and the GUID object a group lists it by.
pub(crate) struct TrackClone {
    pub(crate) definitions: String,
    pub(crate) guid_id: String,
}

/// Copy a verified linked-track closure without rewriting its keys or cached values.
pub(crate) fn linked_track(
    text: &str, definitions: &[Node<'_, '_>], donor: Node<'_, '_>,
    track: &Value, additions: &mut NativeAdditions,
) -> Result<(TrackClone, String), String> {
    let guid = track["guid"].as_str().ok_or("Linked track copy lacks a GUID")?;
    let name = track["name"].as_str().ok_or("Linked track copy lacks a name")?;
    let mut ids = HashMap::new();
    for (index, definition) in definitions.iter().enumerate() {
        let id = definition.attribute("xs.id").ok_or("Linked track copy lacks shared storage")?;
        ids.insert(id, free_storage_id(&mut additions.reserved, &format!("soul2d-linked-{guid}-{index}")));
    }
    let guid_ref = field(donor, "guid").and_then(|node| node.attribute("xs.ref"))
        .ok_or("Linked track copy requires a shared track GUID")?;
    let guid_node = definitions.iter().find(|node| node.attribute("xs.id") == Some(guid_ref))
        .ok_or("Linked track copy track GUID is not owned")?;
    let old_guid = guid_node.attribute("uuid").ok_or("Linked track copy lacks a track UUID")?;
    for node in definitions.iter().flat_map(|node| node.descendants()) {
        if node.attribute("uuid").is_some() && node != *guid_node {
            return Err("Linked track copy contains an unclassified private UUID".into());
        }
    }
    let count = definitions.iter().flat_map(|node| node.descendants())
        .filter(|node| node.attribute("xs.idx").is_some()).count() as u64;
    if additions.next_idx.is_some_and(|index| index.checked_add(count).is_none()) {
        return Err("Linked track copy serial numbers overflow".into());
    }
    let uuids = HashMap::from([(old_guid, guid.to_owned())]);
    let mut output = String::new();
    for definition in definitions {
        let mut cloned = clone_text(text, *definition, &ids, &uuids, &mut additions.next_idx, &|_| None)?;
        if *definition == donor {
            let parsed = roxmltree::Document::parse(&cloned).map_err(|error| error.to_string())?;
            let field = parsed.descendants().find(|node| owned_field(*node, parsed.root_element(), "name"))
                .ok_or("Linked track copy lacks its name field")?;
            let (range, value) = string_patch(&cloned, field, name)?;
            cloned.replace_range(range, &value);
        }
        output.push_str(&cloned);
    }
    let storage = ids[donor.attribute("xs.id").ok_or("Linked track copy lacks track storage")?].clone();
    Ok((TrackClone { definitions: output, guid_id: ids[guid_ref].clone() }, storage))
}

/// Where a new track goes: the scene that owns it and the GUID of the group it joins, if any.
pub(crate) struct TrackPlace<'a> {
    pub(crate) scene_id: &'a str,
    pub(crate) parent_guid: Option<&'a str>,
}

/// Writes `track` as a clone of a model track of this file, stored as `storage`: the donor's effects, the
/// attributes that are not curves and its GUID are cloned with every key dropped, and the parameter curves are
/// `track`'s own. `None` when the file has no model track in Cubism's layout or `track` has Part curves.
pub(crate) fn track(
    text: &str,
    anywhere: Node<'_, '_>,
    place: &TrackPlace<'_>,
    track: &Value,
    storage: &str,
    additions: &mut NativeAdditions,
) -> Result<Option<TrackClone>, String> {
    let Some(donor) = track_donor(anywhere, place.scene_id) else { return Ok(None) };
    let parts = ["partOpacityCurves", "legacyPartOpacityCurves", "underscorePartOpacityCurves"];
    if parts.iter().any(|name| track[*name].as_array().is_some_and(|curves| !curves.is_empty())) {
        return Ok(None);
    }
    let shared = shared_section(donor).ok_or("animation XML lacks a shared section")?;
    let Index { by_id, refs } = index(shared);
    // The curves of the donor's parameter and Part effects are not cloned; the new track brings its own.
    let mut curves = HashSet::new();
    for name in ["keyParamEffect", "partsVisibleEffect"] {
        let Some(effect) = field(donor, name).and_then(|f| f.attribute("xs.ref")).and_then(|id| by_id.get(id)) else { continue };
        for list in ["attrList", "attrMap"].iter().filter_map(|list| field(*effect, list)) {
            for reference in list.descendants().filter_map(|node| node.attribute("xs.ref")) {
                curves.extend(by_id.get(reference).copied());
            }
        }
    }
    let stop = ["CSceneSource", "CAnimation", "CMvTrack_Group_Source", "CMvTrack_Live2DModel_Source", "CMvTrack_Image_Source", "CMvTrack_JavaSoundTrack_Source", "CMvTrack_GuideImage_Source",
        "CResourceManager", "CResourceData", "CResourceGuid", "CSceneGuid", "CTrackGuid"];
    let owned = owned_objects(&[donor], shared, &by_id, &refs, &stop, &curves)?;
    let guid_field = field(donor, "guid").ok_or("a donor track lacks a GUID")?;
    let donor_guid = guid_field.attribute("xs.ref").and_then(|id| by_id.get(id)).copied()
        .filter(|object| object.attribute("uuid").is_some() && !object.has_children())
        .ok_or("a donor track's GUID is not a shared GUID object")?;
    let resource = track["resourceGuid"].as_str().ok_or("a new animation track lacks a resource")?;
    let resource_field = field(donor, "_resourceGuid").ok_or("a donor track lacks a resource")?;
    let resource_xml = match resource_field.attribute("xs.ref") {
        Some(_) => {
            let existing = shared.children().find(|node| node.has_tag_name(resource_field.tag_name().name())
                && node.attribute("uuid") == Some(resource))
                .and_then(|node| node.attribute("xs.id")).map(str::to_owned);
            // A resource added in the same save has its GUID object among the additions, under the ID they reserve.
            let pending = format!("soul2d-guid-{resource}");
            let object = existing.or_else(|| (additions.reserved.contains(&pending)
                && !shared.descendants().any(|node| node.attribute("xs.id") == Some(pending.as_str()))).then_some(pending))
                .ok_or("the track's resource has no shared GUID object")?;
            format!(r#"<{} xs.n="_resourceGuid" xs.ref="{}"/>"#, resource_field.tag_name().name(), escape(&object))
        }
        None => format!(r#"<{} xs.n="_resourceGuid" uuid="{}"/>"#, resource_field.tag_name().name(), escape(resource)),
    };
    let guid = track["guid"].as_str().ok_or("a new animation track lacks a GUID")?;
    let mut ids: HashMap<&str, String> = HashMap::new();
    for object in &owned {
        for id in object.descendants().filter_map(|node| node.attribute("xs.id")) {
            let new = if *object == donor && id == donor.attribute("xs.id").unwrap_or_default() {
                storage.to_owned()
            } else {
                free_storage_id(&mut additions.reserved, &format!("{storage}-{}", ids.len()))
            };
            ids.insert(id, new);
        }
    }
    let guid_id = free_storage_id(&mut additions.reserved, &format!("soul2d-guid-{guid}"));
    ids.insert(donor_guid.attribute("xs.id").unwrap_or_default(), guid_id.clone());
    let mut uuids: HashMap<&str, String> = HashMap::new();
    for uuid in owned.iter().flat_map(|object| object.descendants()).filter_map(|node| node.attribute("uuid")) {
        if !uuids.contains_key(uuid) {
            uuids.insert(uuid, new_guid()?);
        }
    }
    let mut curve_refs = String::new();
    let mut curve_definitions = String::new();
    for curve in track["parameterCurves"].as_array().ok_or("a new animation track lacks curves")? {
        let id = curve["id"].as_str().ok_or("a new animation curve lacks an ID")?;
        let curve_storage = free_storage_id(&mut additions.reserved, &format!("soul2d-curve-{guid}-{id}"));
        let clone = self::curve(text, donor, &PARAMETER, storage, false, curve, &curve_storage, additions)?
            .ok_or("the file has no curve to clone for a new track")?;
        curve_refs.push_str(&format!(r#"<CMvAttrF xs.ref="{}"/>"#, escape(&curve_storage)));
        curve_definitions.push_str(&clone.definition);
    }
    let count = track["parameterCurves"].as_array().map_or(0, Vec::len);
    let scalar = |node: Node<'_, '_>, value: String| {
        format!(r#"<{tag} xs.n="{name}">{value}</{tag}>"#, tag = node.tag_name().name(),
            name = node.attribute("xs.n").unwrap_or_default())
    };
    let number = |key: &str| track[key].as_i64().map(|value| value.to_string()).ok_or(format!("a new animation track lacks {key}"));
    let (start, offset, duration) = (number("start")?, number("internalOffset")?, number("duration")?);
    let name = track["name"].as_str().ok_or("a new animation track lacks a name")?;
    let parameter_effect = field(donor, "keyParamEffect").and_then(|f| f.attribute("xs.ref")).and_then(|id| by_id.get(id)).copied();
    let parts_effect = field(donor, "partsVisibleEffect").and_then(|f| f.attribute("xs.ref")).and_then(|id| by_id.get(id)).copied();
    let replace = |node: Node<'_, '_>| -> Option<String> {
        let name_attr = node.attribute("xs.n")?;
        if owned_field(node, donor, name_attr) {
            return match name_attr {
                "name" => Some(format!(r#"<s xs.n="name">{}</s>"#, escape(name))),
                "start" => Some(scalar(node, start.clone())),
                "internalOffset" => Some(scalar(node, offset.clone())),
                "duration" => Some(scalar(node, duration.clone())),
                "_sceneSource" => Some(format!(r#"<{} xs.n="_sceneSource" xs.ref="{}"/>"#, node.tag_name().name(),
                    escape(place.scene_id))),
                "_resourceGuid" => Some(resource_xml.clone()),
                "parentGuid" => place.parent_guid.map(|parent| format!(r#"<{} xs.n="parentGuid" xs.ref="{}"/>"#,
                    node.tag_name().name(), escape(parent))),
                _ => None,
            };
        }
        for effect in [parameter_effect, parts_effect].into_iter().flatten() {
            if owned_field(node, effect, "attrList") {
                let members = if Some(effect) == parameter_effect { curve_refs.as_str() } else { "" };
                let members_count = if Some(effect) == parameter_effect { count } else { 0 };
                return Some(format!(r#"<{tag} xs.n="attrList" count="{members_count}" type="ICMvAttr">{members}</{tag}>"#,
                    tag = node.tag_name().name()));
            }
            if owned_field(node, effect, "attrMap") {
                return Some(format!(r#"<{} xs.n="attrMap" count="0" />"#, node.tag_name().name()));
            }
        }
        // The cloned attributes keep their values but none of the donor's keys.
        match name_attr {
            "points" => Some(format!(r#"<{tag} xs.n="points" count="0"{kind} />"#, tag = node.tag_name().name(),
                kind = node.attribute("type").map(|kind| format!(r#" type="{}""#, escape(kind))).unwrap_or_default())),
            "curveTypes" => Some(format!(r#"<{} xs.n="curveTypes" count="0" />"#, node.tag_name().name())),
            "keyPts2" => Some(r#"<null xs.n="keyPts2" />"#.to_owned()),
            "keyMin" | "keyMax" => Some(scalar(node, "0".to_owned())),
            "lastValue" => Some(scalar(node, "NaN".to_owned())),
            "lastPos" => Some(scalar(node, "-1".to_owned())),
            _ => None,
        }
    };
    let mut definitions = String::new();
    for object in &owned {
        definitions.push_str(&clone_text(text, *object, &ids, &uuids, &mut additions.next_idx, &replace)?);
    }
    definitions.push_str(&guid_object_xml(donor_guid.tag_name().name(), guid, &guid_id, &mut additions.next_idx));
    definitions.push_str(&curve_definitions);
    Ok(Some(TrackClone { definitions, guid_id }))
}

/// A scene of Cubism's layout to clone and its root group: the group is the scene's own and listed first among its
/// tracks, the GUID is a plain or shared GUID object, and the markers are empty.
fn scene_donor<'d, 'i>(scene: Node<'d, 'i>, index: &Index<'d, 'i>) -> Option<(Node<'d, 'i>, Node<'d, 'i>)> {
    if !scene.has_tag_name("CSceneSource") || scene.attribute("xs.id").is_none()
        || scene.children().any(|child| child.attribute("xs.n") == Some("paramCtrlCalculationOrderSet"))
    {
        return None;
    }
    let group_id = field(scene, "rootTrack")?.attribute("xs.ref")?;
    let group = *index.by_id.get(group_id)?;
    let sources = field(field(scene, "trackSourceSet")?, "_sources")?;
    let first = sources.children().find(|node| node.is_element())?;
    let guid = field(scene, "guid")?;
    let guid_ok = match guid.attribute("xs.ref") {
        Some(id) => index.by_id.get(id).is_some_and(|object| object.attribute("uuid").is_some() && !object.has_children()),
        None => guid.attribute("uuid").is_some(),
    };
    let empty = ["marker", "onionSkinMarker", "_trackSortInfo"].iter()
        .all(|name| scene.descendants().filter(|node| node.attribute("xs.n") == Some(*name)).all(|node| !node.has_children()));
    let own = group.descendants().filter_map(|node| node.attribute("xs.id"))
        .flat_map(|id| index.refs.get(id).into_iter().flatten())
        .all(|reference| reference.ancestors().any(|holder| holder == scene || holder == group));
    let group_ok = group.has_tag_name("CMvTrack_Group_Source") && group.attribute("xs.id") == Some(group_id)
        && field(group, "_childTrackGuids").is_some() && field(group, "guid").and_then(|g| g.attribute("xs.ref")).is_some();
    (first.attribute("xs.ref") == Some(group_id) && guid_ok && empty && own && group_ok).then_some((scene, group))
}

/// Writes `scene` as a clone of the last scene of this file in Cubism's layout, stored as `storage`: its root
/// group, GUID object and settings come with it, its timing, name and tag are `scene`'s, and its tracks are cloned
/// like added tracks. `None` when the file has no such scene, the donor and `scene` differ in whether they store a
/// tag or a loop setting, or a track cannot be cloned.
pub(crate) fn scene(
    text: &str,
    anywhere: Node<'_, '_>,
    scene: &Value,
    storage: &str,
    additions: &mut NativeAdditions,
) -> Result<Option<String>, String> {
    let shared = shared_section(anywhere).ok_or("animation XML lacks a shared section")?;
    let index = index(shared);
    let Some((donor, group)) = shared.children().filter_map(|node| scene_donor(node, &index)).last() else { return Ok(None) };
    let tracks = scene["tracks"].as_array().ok_or("animation tracks missing")?;
    let parts = ["partOpacityCurves", "legacyPartOpacityCurves", "underscorePartOpacityCurves"];
    if tracks.iter().any(|track| parts.iter().any(|name| track[*name].as_array().is_some_and(|curves| !curves.is_empty()))) {
        return Ok(None);
    }
    let invalid = || "a new animation scene has an invalid setting".to_owned();
    let guid = scene["guid"].as_str().ok_or("a new animation scene needs a GUID")?;
    let name = scene["name"].as_str().ok_or_else(invalid)?;
    // The tag and the loop setting are written only where the scene stores them, so the donor must agree with `scene`.
    let scene_tag = scene.get("tag").map(|tag| tag.as_str().ok_or_else(invalid)).transpose()?;
    let looping = scene.get("loop").map(|flag| flag.as_bool().ok_or_else(invalid)).transpose()?;
    let movie = field(donor, "movieInfo").ok_or("a donor scene lacks movie settings")?;
    if scene_tag.is_some() != field(donor, "tag").is_some()
        || looping.is_some() != field(movie, "isLoopMotion").is_some()
    {
        return Ok(None);
    }
    let fps = scene["fps"].as_f64().ok_or_else(invalid)?;
    let duration = scene["durationFrames"].as_i64().ok_or_else(invalid)?;
    let stop = ["CSceneSource", "CAnimation", "CMvTrack_Group_Source", "CMvTrack_Live2DModel_Source", "CMvTrack_Image_Source", "CMvTrack_JavaSoundTrack_Source", "CMvTrack_GuideImage_Source",
        "CResourceManager", "CResourceData", "CResourceGuid", "CSceneGuid", "CTrackGuid"];
    let owned = owned_objects(&[donor, group], shared, &index.by_id, &index.refs, &stop, &HashSet::new())?;
    let mut ids: HashMap<&str, String> = HashMap::new();
    for object in &owned {
        for id in object.descendants().filter_map(|node| node.attribute("xs.id")) {
            let new = if *object == donor && Some(id) == donor.attribute("xs.id") {
                storage.to_owned()
            } else {
                free_storage_id(&mut additions.reserved, &format!("{storage}-{}", ids.len()))
            };
            ids.insert(id, new);
        }
    }
    let mut uuids: HashMap<&str, String> = HashMap::new();
    let guid_field = field(donor, "guid").ok_or("a donor scene lacks a GUID")?;
    let guid_object_of = guid_field.attribute("xs.ref").and_then(|id| index.by_id.get(id)).copied();
    let guid_id = match guid_object_of {
        Some(object) => {
            let id = free_storage_id(&mut additions.reserved, &format!("soul2d-guid-{guid}"));
            ids.insert(object.attribute("xs.id").unwrap_or_default(), id.clone());
            Some((object.tag_name().name(), id))
        }
        None => {
            uuids.insert(guid_field.attribute("uuid").unwrap_or_default(), guid.to_owned());
            None
        }
    };
    for uuid in owned.iter().flat_map(|object| object.descendants()).filter_map(|node| node.attribute("uuid")) {
        if !uuids.contains_key(uuid) {
            uuids.insert(uuid, new_guid()?);
        }
    }
    let parent_guid = field(group, "guid").and_then(|g| g.attribute("xs.ref")).ok_or("a donor scene's group lacks a GUID")?;
    let place = TrackPlace { scene_id: storage, parent_guid: Some(parent_guid) };
    let (mut members, mut sources, mut track_definitions) = (String::new(), String::new(), String::new());
    for track in tracks {
        let track_guid = track["guid"].as_str().ok_or("a new animation track lacks a GUID")?;
        let track_storage = free_storage_id(&mut additions.reserved, &format!("soul2d-track-{track_guid}"));
        let Some(clone) = self::track(text, anywhere, &place, track, &track_storage, additions)? else { return Ok(None) };
        members.push_str(&format!(r#"<CTrackGuid xs.ref="{}"/>"#, escape(&clone.guid_id)));
        sources.push_str(&format!(r#"<CMvTrack_Live2DModel_Source xs.ref="{}"/>"#, escape(&track_storage)));
        track_definitions.push_str(&clone.definitions);
    }
    let number = |name: &str| field(movie, name).and_then(|node| node.text()).and_then(|value| value.parse::<i64>().ok());
    // Cubism leaves the workspace at the whole scene or one frame short of it; the donor's choice is kept.
    let workspace_end = duration + number("workspaceEnd").zip(number("duration")).map_or(0, |(end, length)| end - length);
    let sources_field = field(field(donor, "trackSourceSet").ok_or("a donor scene lacks tracks")?, "_sources")
        .ok_or("a donor scene lacks tracks")?;
    let group_member = sources_field.children().find(|node| node.is_element()).ok_or("a donor scene lacks its group")?;
    let group_ref = format!(r#"<{} xs.ref="{}"/>"#, group_member.tag_name().name(),
        escape(&ids[group.attribute("xs.id").unwrap_or_default()]));
    let scalar = |node: Node<'_, '_>, value: String| {
        format!(r#"<{tag} xs.n="{name}">{value}</{tag}>"#, tag = node.tag_name().name(),
            name = node.attribute("xs.n").unwrap_or_default())
    };
    let list = |node: Node<'_, '_>, count: usize, members: &str| {
        let (tag, name) = (node.tag_name().name(), node.attribute("xs.n").unwrap_or_default());
        if count == 0 {
            format!(r#"<{tag} xs.n="{name}" count="0" />"#)
        } else {
            format!(r#"<{tag} xs.n="{name}" count="{count}">{members}</{tag}>"#)
        }
    };
    let replace = |node: Node<'_, '_>| -> Option<String> {
        if node == sources_field {
            return Some(list(node, tracks.len() + 1, &(group_ref.clone() + &sources)));
        }
        let name_attr = node.attribute("xs.n")?;
        if owned_field(node, donor, "sceneName") {
            return Some(format!(r#"<{tag} xs.n="sceneName">{}</{tag}>"#, escape(name), tag = node.tag_name().name()));
        }
        if owned_field(node, donor, "tag") {
            return scene_tag.map(|text| tag_xml(node.tag_name().name(), text));
        }
        if owned_field(node, group, "_childTrackGuids") {
            return Some(list(node, tracks.len(), &members));
        }
        if !owned_field(node, movie, name_attr) {
            return None;
        }
        match name_attr {
            "fps" => Some(scalar(node, format!("{fps:?}"))),
            "duration" => Some(scalar(node, duration.to_string())),
            "workspaceStart" => Some(scalar(node, "0".to_owned())),
            "workspaceEnd" => Some(scalar(node, workspace_end.to_string())),
            "isLoopMotion" => looping.map(|flag| scalar(node, flag.to_string())),
            _ => None,
        }
    };
    let mut definitions = String::new();
    for object in &owned {
        definitions.push_str(&clone_text(text, *object, &ids, &uuids, &mut additions.next_idx, &replace)?);
    }
    if let Some((tag, id)) = guid_id {
        definitions.push_str(&guid_object_xml(tag, guid, &id, &mut additions.next_idx));
    }
    definitions.push_str(&track_definitions);
    Ok(Some(definitions))
}

/// A new linked-model resource stored like one of this file, and the GUID object it is listed by.
pub(crate) struct ResourceClone {
    pub(crate) definitions: String,
    pub(crate) guid_id: String,
}

pub(crate) fn model_resource_donor<'d, 'i>(anywhere: Node<'d, 'i>) -> Option<Node<'d, 'i>> {
    shared_section(anywhere)?.children().find(|node| {
        node.has_tag_name("CResourceData") && node.attribute("xs.id").is_some()
            && field(*node, "resourceRef").is_some_and(|reference| reference.has_tag_name("CResource_Linked_Model")
                && reference.attribute("xs.ref").is_none())
            && !node.descendants().any(|inner| inner != *node && inner.attribute("xs.id").is_some())
    })
}

/// Writes a linked resource from a validated donor while preserving its scalar storage.
pub(crate) fn resource(
    text: &str,
    donor: Node<'_, '_>,
    resource: &Value,
    storage: &str,
    additions: &mut NativeAdditions,
) -> Result<ResourceClone, String> {
    let shared = shared_section(donor).ok_or("animation XML lacks a shared section")?;
    let reference = field(donor, "resourceRef").ok_or("a donor resource lacks its reference")?;
    let model_resource = reference.has_tag_name("CResource_Linked_Model");
    let text_of = |key: &str| resource[key].as_str().ok_or_else(|| format!("a new animation resource lacks {key}"));
    let (guid, name, source) = (text_of("guid")?, text_of("name")?, text_of("sourceFile")?);
    let guid_field = field(reference, "guid").ok_or("a donor resource lacks a GUID")?;
    let mut ids = HashMap::from([(donor.attribute("xs.id").unwrap_or_default(), storage.to_owned())]);
    let mut uuids = HashMap::new();
    let guid_id = free_storage_id(&mut additions.reserved, &format!("soul2d-guid-{guid}"));
    let guid_object = match guid_field.attribute("xs.ref") {
        Some(target) => {
            let object = shared.children().find(|node| node.attribute("xs.id") == Some(target))
                .filter(|node| node.attribute("uuid").is_some() && !node.has_children())
                .ok_or("a donor resource's GUID is not a shared GUID object")?;
            ids.insert(target, guid_id.clone());
            uuids.insert(object.attribute("uuid").unwrap(), guid.to_owned());
            Some(object)
        }
        None => {
            uuids.insert(guid_field.attribute("uuid").unwrap_or_default(), guid.to_owned());
            None
        }
    };
    let mut serials = usize::from(guid_object.is_some());
    for value in donor.descendants().filter_map(|node| node.attribute("xs.idx")) {
        value.parse::<u64>().map_err(|_| "invalid resource serial number")?;
        serials += 1;
    }
    if additions.next_idx.is_some_and(|index| index.checked_add(serials as u64).is_none()) {
        return Err("resource serial numbers overflow".into());
    }
    let mut definitions = clone_text(text, donor, &ids, &uuids, &mut additions.next_idx, &|_| None)?;
    let parsed = roxmltree::Document::parse(&definitions).map_err(|error| error.to_string())?;
    let reference = field(parsed.root_element(), "resourceRef").ok_or("cloned resource lacks its reference")?;
    let mut patches = Vec::new();
    for (key, value) in [("name", name), ("srcFile", source)] {
        let field = field(reference, key).ok_or("cloned resource lacks a string field")?;
        let patch = match string_patch(&definitions, field, value) {
            Ok(patch) => patch,
            Err(_) if model_resource => {
                // Legacy Model cloning replaced complex scalar contents, including comments.
                let start = field.first_child().ok_or("cloned resource string is missing")?.range().start;
                let end = field.range().start + definitions[field.range()].rfind("</")
                    .ok_or("cloned resource string lacks its closing tag")?;
                (start..end, escape(value))
            }
            Err(error) => return Err(error),
        };
        patches.push(patch);
    }
    patches.sort_by_key(|(range, _)| range.start);
    for (range, value) in patches.into_iter().rev() { definitions.replace_range(range, &value); }
    if let Some(object) = guid_object {
        if model_resource {
            definitions.push_str(&guid_object_xml(object.tag_name().name(), guid, &guid_id, &mut additions.next_idx));
        } else {
            definitions.push_str(&clone_text(text, object, &ids, &uuids, &mut additions.next_idx, &|_| None)?);
        }
    }
    Ok(ResourceClone { definitions, guid_id })
}
