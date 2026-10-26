//! Scalar settings of CMO3 forms and sources beyond vertices and ArtMesh opacity (issue #144): rotation deformer forms,
//! ArtMesh and existing private ArtPath draw order, private ArtPath opacity, multiply and screen colours, Glue intensity, blend modes,
//! offscreen drawing and clipping
//! inversion. An edit rewrites the one stored token in place, so the rest of the XML keeps its bytes. A field the
//! file does not store (the reader fills a default) is copied from the same field elsewhere in the file, after the
//! field that precedes it there, and refused when the file stores it nowhere. `authoring_edit::rewrite`
//! re-reads the result and refuses it unless it equals the requested model, which also catches shared storage.
//! An ArtPath control point added or deleted in every keyform (issue #159) is written by `sync_points`: a new
//! point is a clone of a stored one, and `rewrite` then patches its values.

use crate::authoring_edit::{self as edit, XmlPatch};
use crate::authoring_graph::Graph;
use crate::authoring_grid::{cloned, inside, owned, private_objects, top_level};
use crate::authoring_reparent::{attribute_patch, source_nodes, stored_field};
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, NodeId, ParsingOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Where one editable value lives: its JSON path inside a form, source or ArtPath point, and the chain of XML
/// fields below that element (empty for the element itself) with the attribute that holds it (`None` for the
/// last field's text).
struct Spec {
    path: &'static [&'static str],
    xml: &'static [&'static str],
    attribute: Option<&'static str>,
}

const fn spec(path: &'static [&'static str], xml: &'static [&'static str], attribute: Option<&'static str>) -> Spec {
    Spec { path, xml, attribute }
}

const FORM_SPECS: &[Spec] = &[
    spec(&["drawOrder"], &["drawOrder"], None),
    spec(&["multiplyColor", "0"], &["multiplyColor"], Some("red")),
    spec(&["multiplyColor", "1"], &["multiplyColor"], Some("green")),
    spec(&["multiplyColor", "2"], &["multiplyColor"], Some("blue")),
    spec(&["multiplyColor", "3"], &["multiplyColor"], Some("alpha")),
    spec(&["screenColor", "0"], &["screenColor"], Some("red")),
    spec(&["screenColor", "1"], &["screenColor"], Some("green")),
    spec(&["screenColor", "2"], &["screenColor"], Some("blue")),
    spec(&["screenColor", "3"], &["screenColor"], Some("alpha")),
    spec(&["data", "angle"], &[], Some("angle")),
    spec(&["data", "origin", "0"], &[], Some("originX")),
    spec(&["data", "origin", "1"], &[], Some("originY")),
    spec(&["data", "scale"], &[], Some("scale")),
    spec(&["data", "reflectX"], &[], Some("isReflectX")),
    spec(&["data", "reflectY"], &[], Some("isReflectY")),
    spec(&["data", "intensity"], &["intensity"], None),
];

/// The control point of an ArtPath stroke (issue #159), relative to its `CArtPathPoint`.
const POINT_SPECS: &[Spec] = &[
    spec(&["position", "0"], &["curvePointPosition", "point", "x"], None),
    spec(&["position", "1"], &["curvePointPosition", "point", "y"], None),
    spec(&["startVelocity", "0"], &["curvePointPosition", "startVelocity", "x"], None),
    spec(&["startVelocity", "1"], &["curvePointPosition", "startVelocity", "y"], None),
    spec(&["endVelocity", "0"], &["curvePointPosition", "endVelocity", "x"], None),
    spec(&["endVelocity", "1"], &["curvePointPosition", "endVelocity", "y"], None),
    spec(&["width"], &["width"], None),
    spec(&["opacity"], &["opacity"], None),
    spec(&["color", "0"], &["colorRed"], None),
    spec(&["color", "1"], &["colorGreen"], None),
    spec(&["color", "2"], &["colorBlue"], None),
    spec(&["color", "3"], &["colorAlpha"], None),
    spec(&["corner"], &["isCorner"], None),
];

const SOURCE_SPECS: &[Spec] = &[
    spec(&["data", "colorComposition"], &["colorComposition"], Some("v")),
    spec(&["data", "alphaComposition"], &["alphaComposition"], Some("v")),
    spec(&["data", "useOffscreen"], &["useOffscreen"], None),
    spec(&["data", "invertClippingMask"], &["invertClippingMask"], None),
];

const ARTPATH_SOURCE_SPECS: &[Spec] = &[
    spec(&["data", "closed"], &["isClosed"], None),
    spec(&["data", "divisionCount"], &["divNum"], None),
];

/// A parameter's range, default and repeat flag, relative to its `CParameterSource`.
const PARAMETER_SPECS: &[Spec] = &[
    spec(&["minimum"], &["minValue"], None),
    spec(&["maximum"], &["maxValue"], None),
    spec(&["default"], &["defaultValue"], None),
    spec(&["repeat"], &["isRepeat"], None),
];

/// Stored ArtPath brush fields (issue #159), relative to the brush object.
const BRUSH_SPECS: &[Spec] = &[
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m00"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m00")),
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m01"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m01")),
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m02"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m02")),
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m10"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m10")),
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m11"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m11")),
    spec(&["textureInput", "currentTextureInputData", "optionalTransformOnCanvas", "m12"], &["textureInput", "currentTextureInputData", "optionalTransformOnCanvas"], Some("m12")),
    spec(&["drawingCurveType"], &["drawingCurveType"], Some("v")),
    spec(&["widthCoefficient"], &["widthCoefficient"], None),
    spec(&["textureUvData", "up"], &["textureUvData", "up"], None),
    spec(&["textureUvData", "upInter"], &["textureUvData", "upInter"], None),
    spec(&["textureUvData", "center"], &["textureUvData", "center"], None),
    spec(&["textureUvData", "downInter"], &["textureUvData", "downInter"], None),
    spec(&["textureUvData", "down"], &["textureUvData", "down"], None),
    spec(&["textureUvData", "inStart"], &["textureUvData", "inStart"], None),
    spec(&["textureUvData", "repeatStart"], &["textureUvData", "repeatStart"], None),
    spec(&["textureUvData", "repeatEnd"], &["textureUvData", "repeatEnd"], None),
    spec(&["textureUvData", "outEnd"], &["textureUvData", "outEnd"], None),
    spec(&["fixWidthData", "upFixedWidth"], &["fixWidthData", "upFixedWidth"], None),
    spec(&["fixWidthData", "downFixedWidth"], &["fixWidthData", "downFixedWidth"], None),
    spec(&["spriteData", "density"], &["spriteData", "density"], None),
    spec(&["spriteData", "useTextureMagnification"], &["spriteData", "useTextureMagnification"], None),
];

/// The drawing curve types the supplied files store; another name could be one the Editor does not know.
const DRAWING_CURVE_TYPES: [&str; 2] = ["SPRITE", "TIP_USE"];

/// The object that holds a changed value.
#[derive(Clone, Copy, PartialEq)]
enum Owner {
    Source,
    Parameter,
    Brush,
}

const SOURCE_SETS: [&str; 4] = ["drawableSourceSet", "deformerSourceSet", "partSourceSet", "affecterSourceSet"];

/// One changed value: the source, the form (GUID and coordinate type) when it belongs to one, the ArtPath point
/// within that form, and the new text.
pub(crate) struct FieldChange(FieldWrite);

enum FieldWrite {
    General(ScalarFieldChange),
    ArtPathDrawOrder {
        source: String,
        form: String,
        coordinate: Option<String>,
        before: i32,
        after: u16,
    },
    ArtPathOpacity {
        source: String,
        form: String,
        coordinate: Option<String>,
        before: f64,
        after: f64,
    },
}

struct ScalarFieldChange {
    /// The GUID of the source, parameter or brush, as `owner` says.
    source: String,
    owner: Owner,
    form: Option<(String, Option<String>)>,
    point: Option<usize>,
    spec: &'static Spec,
    /// Unchanged, carried only so a missing element can be written whole.
    same: bool,
    text: String,
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |value, key| match key.parse::<usize>() {
        Ok(index) => &value[index],
        Err(_) => &value[*key],
    })
}

fn at_mut<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Value {
    path.iter().fold(value, |value, key| match key.parse::<usize>() {
        Ok(index) => &mut value[index],
        Err(_) => &mut value[*key],
    })
}

/// The XML text of a value: numbers as the shortest round-trip decimal, booleans as `true`/`false`, enum names as
/// they are. Anything else, or text that would need escaping, is refused.
fn xml_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Number(number) if number.is_i64() => Ok(number.to_string()),
        Value::Number(_) => {
            let number = value.as_f64().filter(|number| (*number as f32).is_finite())
                .ok_or("an authoring value exceeds the native float range")?;
            Ok(number.to_string())
        }
        Value::String(name) if !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') => {
            Ok(name.clone())
        }
        _ => Err("an authoring value is not a number, boolean or enum name".into()),
    }
}

/// The values of `model` that differ from `initial` in the fields above, and `model` with them put back, so the
/// caller's own check that nothing else changed still holds.
pub(crate) fn changes(initial: &Value, model: &Value) -> Result<(Vec<FieldChange>, Value), String> {
    let mut normalized = model.clone();
    let mut found = Vec::new();
    let (old_parameters, new_parameters) = (initial["parameters"].as_array(), model["parameters"].as_array());
    for (index, (old, new)) in old_parameters.into_iter().flatten().zip(new_parameters.into_iter().flatten()).enumerate() {
        let Some(guid) = old["guid"].as_str().filter(|guid| new["guid"] == *guid) else { continue };
        for spec in PARAMETER_SPECS {
            if at(old, spec.path) != at(new, spec.path) {
                found.push(FieldChange(FieldWrite::General(ScalarFieldChange { source: guid.to_owned(), owner: Owner::Parameter, form: None, point: None, spec, same: false,
                    text: xml_text(at(new, spec.path))? })));
                *at_mut(&mut normalized["parameters"][index], spec.path) = at(old, spec.path).clone();
            }
        }
    }
    let (old_brushes, new_brushes) = (initial["brushes"].as_array(), model["brushes"].as_array());
    for (index, (old, new)) in old_brushes.into_iter().flatten().zip(new_brushes.into_iter().flatten()).enumerate() {
        let Some(guid) = old["guid"].as_str().filter(|guid| new["guid"] == *guid) else { continue };
        for spec in BRUSH_SPECS {
            if at(old, spec.path) != at(new, spec.path) {
                found.push(FieldChange(FieldWrite::General(ScalarFieldChange { source: guid.to_owned(), owner: Owner::Brush, form: None, point: None, spec, same: false,
                    text: xml_text(at(new, spec.path))? })));
                *at_mut(&mut normalized["brushes"][index], spec.path) = at(old, spec.path).clone();
            }
        }
    }
    let (Some(old_sources), Some(new_sources)) = (initial["sources"].as_array(), model["sources"].as_array()) else {
        return Ok((found, normalized));
    };
    for (index, (old, new)) in old_sources.iter().zip(new_sources).enumerate() {
        let Some(source) = old["guid"].as_str() else { continue };
        if new["guid"].as_str() != Some(source) {
            continue;
        }
        let path_specs = if old["data"]["type"] == "ArtPath" { ARTPATH_SOURCE_SPECS } else { &[] };
        for spec in SOURCE_SPECS.iter().chain(path_specs) {
            if at(old, spec.path) != at(new, spec.path) {
                if spec.path == ["data", "invertClippingMask"]
                    && (old["data"]["type"] == "ArtPath" || new["data"]["type"] == "ArtPath")
                    && (old["data"]["type"] != "ArtPath" || new["data"]["type"] != "ArtPath"
                        || !at(old, spec.path).is_boolean() || !at(new, spec.path).is_boolean())
                {
                    return Err("unsupported ArtPath inversion owner or boolean".into());
                }
                if spec.path == ["data", "divisionCount"] {
                    division_count(at(new, spec.path))?;
                }
                found.push(FieldChange(FieldWrite::General(ScalarFieldChange { source: source.to_owned(), owner: Owner::Source, form: None, point: None, spec, same: false, text: xml_text(at(new, spec.path))? })));
                *at_mut(&mut normalized["sources"][index], spec.path) = at(old, spec.path).clone();
            }
        }
        let (Some(old_forms), Some(new_forms)) = (old["forms"].as_array(), new["forms"].as_array()) else { continue };
        for (form_index, (old_form, new_form)) in old_forms.iter().zip(new_forms).enumerate() {
            let Some(guid) = old_form["guid"].as_str() else { continue };
            if new_form["guid"] != old_form["guid"] || new_form["coordinateType"] != old_form["coordinateType"] {
                continue;
            }
            let form = Some((guid.to_owned(), old_form["coordinateType"].as_str().map(str::to_owned)));
            if old["data"]["type"] == "ArtPath" && old_form["opacity"] != new_form["opacity"] {
                if new["data"]["type"] != "ArtPath" || old_form["data"]["type"] != "ArtPath"
                    || new_form["data"]["type"] != "ArtPath"
                {
                    return Err("unsupported ArtPath opacity owner or form kind".into());
                }
                edit::ensure_form_editable(old, guid)?;
                let before = old_form["opacity"].as_f64().filter(|value| value.is_finite())
                    .ok_or("the ArtPath form has no finite opacity")?;
                let after = edit::form_opacity(&new_form["opacity"])?;
                edit::ensure_shape_opacity(old, guid, after)?;
                found.push(FieldChange(FieldWrite::ArtPathOpacity {
                    source: source.to_owned(), form: guid.to_owned(),
                    coordinate: old_form["coordinateType"].as_str().map(str::to_owned), before, after,
                }));
                normalized["sources"][index]["forms"][form_index]["opacity"] = old_form["opacity"].clone();
            }
            let differs = |spec: &Spec| at(old_form, spec.path) != at(new_form, spec.path);
            for spec in FORM_SPECS {
                // A colour is one element: when one channel changes, all go along in case the file lacks it.
                let element_changed = !spec.xml.is_empty() && FORM_SPECS.iter().any(|other| other.xml == spec.xml && differs(other));
                if !differs(spec) && !element_changed {
                    continue;
                }
                if old["data"]["type"] == "ArtPath" {
                    if spec.path != ["drawOrder"] || new["data"]["type"] != "ArtPath"
                        || new_form["data"]["type"] != old_form["data"]["type"]
                    {
                        return Err("unsupported ArtPath form scalar change".into());
                    }
                    edit::ensure_form_editable(old, guid)?;
                    let before = old_form["drawOrder"].as_i64().and_then(|value| i32::try_from(value).ok())
                        .ok_or("the ArtPath form has no stored integer drawOrder")?;
                    found.push(FieldChange(FieldWrite::ArtPathDrawOrder {
                        source: source.to_owned(), form: guid.to_owned(),
                        coordinate: old_form["coordinateType"].as_str().map(str::to_owned),
                        before, after: draw_order(&new_form["drawOrder"])?,
                    }));
                } else {
                    found.push(FieldChange(FieldWrite::General(ScalarFieldChange { source: source.to_owned(), owner: Owner::Source, form: form.clone(), point: None, spec,
                        same: !differs(spec), text: xml_text(at(new_form, spec.path))? })));
                }
                *at_mut(&mut normalized["sources"][index]["forms"][form_index], spec.path) =
                    at(old_form, spec.path).clone();
            }
            let (Some(old_points), Some(new_points)) =
                (old_form["data"]["points"].as_array(), new_form["data"]["points"].as_array()) else { continue };
            for (point, (old_point, new_point)) in old_points.iter().zip(new_points).enumerate() {
                if old_point["guid"] != new_point["guid"] {
                    continue;
                }
                for spec in POINT_SPECS {
                    if at(old_point, spec.path) != at(new_point, spec.path) {
                        found.push(FieldChange(FieldWrite::General(ScalarFieldChange { source: source.to_owned(), owner: Owner::Source, form: form.clone(), point: Some(point), spec, same: false,
                            text: xml_text(at(new_point, spec.path))? })));
                        *at_mut(&mut normalized["sources"][index]["forms"][form_index]["data"]["points"][point], spec.path) =
                            at(old_point, spec.path).clone();
                    }
                }
            }
        }
    }
    Ok((found, normalized))
}

/// One in-place patch per change. A missing field or attribute means the file relies on the default.
pub(crate) fn patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    changes: &[FieldChange],
    budget: &mut Budget,
) -> Result<Vec<XmlPatch>, String> {
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    for set in SOURCE_SETS {
        for source in graph.source_list(model, set)? {
            sources.push((graph.guid(source)?, source, Owner::Source));
        }
    }
    for parameter in graph.source_list(model, "parameterSourceSet")? {
        sources.push((graph.guid(parameter)?, parameter, Owner::Parameter));
    }
    for brush in edit::brush_nodes(graph, model)? {
        sources.push((graph.guid(brush)?, brush, Owner::Brush));
    }
    let mut patches = Vec::with_capacity(changes.len());
    // Elements the file lacks: the node that should hold each, its field name and the values to write.
    let mut missing: Vec<(Node<'d, 'i>, &str, Vec<(Option<&str>, String)>)> = Vec::new();
    'changes: for change in changes {
        let change = match &change.0 {
            FieldWrite::General(change) => change,
            FieldWrite::ArtPathDrawOrder { source, form, coordinate, before, after } => {
                patches.push(artpath_draw_order_patch(text, graph, model, source, form,
                    coordinate.as_deref(), *before, *after, budget)?);
                continue;
            }
            FieldWrite::ArtPathOpacity { source, form, coordinate, before, after } => {
                patches.push(artpath_opacity_patch(text, graph, model, source, form,
                    coordinate.as_deref(), *before, *after, budget)?);
                continue;
            }
        };
        let mut owners = sources.iter().filter(|(guid, _, owner)| *guid == Some(change.source.as_str()) && *owner == change.owner);
        let (_, source, _) = owners.next().ok_or("an edited source was not found in the CMO3")?;
        if owners.next().is_some() {
            return Err("an edited source GUID is ambiguous in the CMO3".into());
        }
        if source.has_tag_name("CArtPathSource") && change.spec.xml.contains(&"invertClippingMask") {
            if change.owner != Owner::Source || change.form.is_some() || change.point.is_some()
                || change.spec.xml != ["invertClippingMask"] || change.spec.attribute.is_some() || change.same
            {
                return Err("ArtPath inversion requires a source-only boolean change".into());
            }
            let after = match change.text.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err("ArtPath inversion requires a boolean".into()),
            };
            patches.push(artpath_inversion_patch(text, *source, after)?);
            continue;
        }
        if source.has_tag_name("CArtPathSource") && change.form.is_some() && change.point.is_none() {
            return Err("ArtPath form settings require their private writer".into());
        }
        let holder = match &change.form {
            None => *source,
            Some((guid, coordinate)) => {
                let list = graph.field(*source, "keyforms")?.ok_or("an edited source has no keyforms")?;
                let mut forms = Vec::new();
                for form in values::list(graph, list, budget)? {
                    if graph.guid(form)? == Some(guid.as_str()) && edit::form_coordinate(graph, form)? == *coordinate {
                        forms.push(form);
                    }
                }
                match forms[..] {
                    [form] => form,
                    [] => return Err("an edited form was not found in the CMO3".into()),
                    _ => return Err("an edited form is ambiguous in the CMO3".into()),
                }
            }
        };
        let holder = match change.point {
            None => holder,
            Some(point) => {
                let list = graph.field(holder, "positions")?.ok_or("an edited ArtPath form has no points")?;
                *values::list(graph, list, budget)?.get(point).ok_or("an edited ArtPath point was not found")?
            }
        };
        let name = change.spec.attribute.or(change.spec.xml.last().copied()).unwrap_or_default();
        let mut element = holder;
        for (depth, field) in change.spec.xml.iter().enumerate() {
            if matches!(*field, "isClosed" | "divNum") {
                let tag = if *field == "isClosed" { "b" } else { "i" };
                let mut fields = element.children().filter(|child| child.is_element() && child.attribute("xs.n") == Some(field));
                let stored = fields.next().ok_or_else(|| format!("the CMO3 stores no direct {field} here"))?;
                if fields.next().is_some() || stored.tag_name().name() != tag
                    || stored.attribute("xs.id").is_some() || stored.attribute("xs.ref").is_some()
                {
                    return Err(format!("the CMO3 {field} is not one private direct {tag} field"));
                }
                element = stored;
                continue;
            }
            match graph.field(element, field)? {
                Some(next) => element = next,
                // The last element is missing: it is written whole below, from one the file stores elsewhere.
                None if depth + 1 == change.spec.xml.len() && change.owner != Owner::Brush => {
                    let value = (change.spec.attribute, change.text.clone());
                    match missing.iter_mut().find(|(node, name, _)| node.id() == element.id() && name == field) {
                        Some((_, _, values)) => values.push(value),
                        None => missing.push((element, field, vec![value])),
                    }
                    continue 'changes;
                }
                None => return Err(format!("the CMO3 stores no {field} here; Soul2D edits only stored values")),
            }
        }
        if change.same {
            continue;
        }
        let (range, raw) = match change.spec.attribute {
            Some(attribute) => {
                let found = element.attributes().find(|candidate| candidate.name() == attribute)
                    .ok_or_else(|| format!("the CMO3 stores no {name} here; Soul2D edits only stored values"))?;
                (found.range_value(), found.value())
            }
            None => {
                let mut children = element.children();
                let (Some(content), None) = (children.next(), children.next()) else {
                    return Err(format!("the CMO3 {name} is not one inline text"));
                };
                let decoded = content.text().filter(|_| content.is_text()).ok_or_else(|| format!("the CMO3 {name} is not text"))?;
                let (raw, text_start) = if matches!(name, "isClosed" | "divNum") {
                    let range = raw_inner_span(text, element)?;
                    (&text[range.clone()], range.start)
                } else { (decoded, content.range().start) };
                let start = raw.len() - raw.trim_start().len();
                (text_start + start..text_start + start + raw.trim().len(), raw.trim())
            }
        };
        // An escaped value would not read back the same way; such a file is left alone.
        if text.get(range.clone()) != Some(raw) {
            return Err(format!("the CMO3 {name} is stored escaped"));
        }
        if name == "isClosed" && !matches!(raw, "true" | "false") {
            return Err("the CMO3 isClosed is not a plain boolean".into());
        }
        if name == "divNum" && raw.parse::<u32>().is_err() {
            return Err("the CMO3 divNum is not a plain u32 integer".into());
        }
        patches.push(XmlPatch { range, value: change.text.clone() });
    }
    for (holder, field, values) in missing {
        patches.push(inserted_field(text, graph, holder, field, &values)?);
    }
    Ok(patches)
}

fn draw_order(value: &Value) -> Result<u16, String> {
    value.as_u64().filter(|order| *order <= 1000).map(|order| order as u16)
        .ok_or_else(|| "drawOrder must be an integer in 0..=1000".into())
}

fn raw_inner_span(text: &str, element: Node<'_, '_>) -> Result<std::ops::Range<usize>, String> {
    let stored = &text[element.range()];
    let mut quote = None;
    let begin = stored.char_indices().find_map(|(index, ch)| {
        match (quote, ch) {
            (Some(mark), ch) if mark == ch => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None, '>') => return Some(index + 1),
            _ => {},
        }
        None
    }).ok_or("authoring scalar lacks an opening tag")?;
    let end = stored.rfind("</").filter(|end| *end >= begin)
        .ok_or("authoring scalar lacks a closing tag")?;
    Ok(element.range().start + begin..element.range().start + end)
}

fn artpath_inversion_patch(text: &str, source: Node<'_, '_>, after: bool) -> Result<XmlPatch, String> {
    if !source.has_tag_name("CArtPathSource") { return Err("edited source is not an ArtPath".into()); }
    let mut holder = source;
    let mut stored = None;
    loop {
        if holder.attribute("xs.ref").is_some()
            || (holder != source && holder.attribute("xs.id").is_some())
        {
            return Err("ArtPath source inheritance is not private".into());
        }
        for field in holder.children().filter(|node| node.is_element()
            && node.attribute("xs.n") == Some("invertClippingMask"))
        {
            if stored.replace(field).is_some() { return Err("ArtPath inversion is duplicated or shadowed".into()); }
        }
        let mut supers = holder.children().filter(|node| node.is_element() && node.attribute("xs.n") == Some("super"));
        let next = supers.next();
        if supers.next().is_some() { return Err("ArtPath source has duplicate super fields".into()); }
        let Some(next) = next else { break };
        let expected = match holder.tag_name().name() {
            "CArtPathSource" => "ACDrawableSource",
            "ACDrawableSource" => "ACParameterControllableSource",
            _ => return Err("unsupported ArtPath source inheritance".into()),
        };
        if !next.has_tag_name(expected) { return Err("unsupported ArtPath source inheritance".into()); }
        holder = next;
    }
    let field = stored.ok_or("the ArtPath source has no stored inversion")?;
    if !field.has_tag_name("b") || field.attribute("xs.id").is_some() || field.attribute("xs.ref").is_some()
        || field.children().count() != 1 || !field.first_child().is_some_and(|node| node.is_text())
    {
        return Err("ArtPath inversion is not one private boolean field".into());
    }
    let range = raw_inner_span(text, field)?;
    let raw = &text[range.clone()];
    let token = raw.trim_matches([' ', '\t', '\r', '\n']);
    if !matches!(token, "true" | "false") { return Err("ArtPath inversion is not a plain boolean literal".into()); }
    let start = range.start + raw.len() - raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
    Ok(XmlPatch { range: start..start + token.len(), value: after.to_string() })
}

fn private_artpath_form_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    source_guid: &str,
    form_guid: &str,
    coordinate: Option<&str>,
    field_name: &str,
    budget: &mut Budget,
) -> Result<Node<'d, 'i>, String> {
    let mut matching = Vec::new();
    for source in graph.source_list(model, "drawableSourceSet")? {
        if graph.guid(source)? == Some(source_guid) { matching.push(source); }
    }
    let [source] = matching.as_slice() else { return Err("edited ArtPath source is not unique".into()) };
    if !source.has_tag_name("CArtPathSource") { return Err("edited source is not an ArtPath".into()); }
    let lists: Vec<_> = source.children().filter(|node| node.is_element()
        && node.attribute("xs.n") == Some("keyforms")).collect();
    let [list] = lists.as_slice() else { return Err("ArtPath requires one direct keyforms list".into()) };
    if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
        return Err("ArtPath keyforms list is not private".into());
    }
    let mut matching = Vec::new();
    for form in list.children().filter(|node| node.is_element()) {
        if !form.has_tag_name("CArtPathForm") || form.attribute("xs.id").is_some()
            || form.attribute("xs.ref").is_some()
        {
            return Err("ArtPath form rows are not private inline forms".into());
        }
    }
    for form in values::list(graph, *list, budget)? {
        if graph.guid(form)? == Some(form_guid) && edit::form_coordinate(graph, form)?.as_deref() == coordinate {
            matching.push(form);
        }
    }
    let [form] = matching.as_slice() else { return Err("edited ArtPath form is not unique".into()) };
    let mut holder = *form;
    let mut stored = None;
    loop {
        if holder.attribute("xs.id").is_some() || holder.attribute("xs.ref").is_some() {
            return Err("ArtPath form inheritance is not private".into());
        }
        for field in holder.children().filter(|node| node.is_element() && node.attribute("xs.n") == Some(field_name)) {
            if stored.replace(field).is_some() { return Err(format!("ArtPath {field_name} is duplicated or shadowed")); }
        }
        let mut supers = holder.children().filter(|node| node.is_element() && node.attribute("xs.n") == Some("super"));
        let next = supers.next();
        if supers.next().is_some() { return Err("ArtPath form has duplicate super fields".into()); }
        let Some(next) = next else { break };
        let expected = match holder.tag_name().name() {
            "CArtPathForm" => "ACDrawableForm",
            "ACDrawableForm" => "ACForm",
            _ => return Err("unsupported ArtPath form inheritance".into()),
        };
        if !next.has_tag_name(expected) { return Err("unsupported ArtPath form inheritance".into()); }
        holder = next;
    }
    stored.ok_or_else(|| format!("the ArtPath form has no stored {field_name}"))
}

fn artpath_draw_order_patch(
    text: &str,
    graph: &Graph<'_, '_>,
    model: Node<'_, '_>,
    source_guid: &str,
    form_guid: &str,
    coordinate: Option<&str>,
    before: i32,
    after: u16,
    budget: &mut Budget,
) -> Result<XmlPatch, String> {
    let field = private_artpath_form_field(graph, model, source_guid, form_guid, coordinate, "drawOrder", budget)?;
    if !field.has_tag_name("i") || field.attribute("xs.id").is_some() || field.attribute("xs.ref").is_some()
        || field.children().count() != 1 || !field.first_child().is_some_and(|node| node.is_text())
    {
        return Err("ArtPath drawOrder is not one private integer field".into());
    }
    let range = raw_inner_span(text, field)?;
    let raw = &text[range.clone()];
    let token = raw.trim_matches([' ', '\t', '\r', '\n']);
    if token.parse::<i32>().ok() != Some(before) {
        return Err("ArtPath drawOrder is not its original plain integer".into());
    }
    let start = range.start + raw.len() - raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
    Ok(XmlPatch { range: start..start + token.len(), value: after.to_string() })
}

fn artpath_opacity_patch(
    text: &str,
    graph: &Graph<'_, '_>,
    model: Node<'_, '_>,
    source_guid: &str,
    form_guid: &str,
    coordinate: Option<&str>,
    before: f64,
    after: f64,
    budget: &mut Budget,
) -> Result<XmlPatch, String> {
    let field = private_artpath_form_field(graph, model, source_guid, form_guid, coordinate, "opacity", budget)?;
    if !field.has_tag_name("f") || field.attribute("xs.id").is_some() || field.attribute("xs.ref").is_some()
        || field.children().count() != 1 || !field.first_child().is_some_and(|node| node.is_text())
    {
        return Err("ArtPath opacity is not one private float field".into());
    }
    let range = raw_inner_span(text, field)?;
    let raw = &text[range.clone()];
    let token = raw.trim_matches([' ', '\t', '\r', '\n']);
    if token.parse::<f64>().ok().filter(|value| value.is_finite()).map(f64::to_bits) != Some(before.to_bits()) {
        return Err("ArtPath opacity is not its original plain finite float".into());
    }
    let start = range.start + raw.len() - raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
    Ok(XmlPatch { range: start..start + token.len(), value: after.to_string() })
}

/// The elements the file stores for `field` under the same kind of parent as `holder` or one of its `super`
/// ancestors, each with the offset after the field that precedes it there (where `field` would go on `holder`).
pub(crate) fn layouts<'d, 'i>(
    graph: &Graph<'d, 'i>,
    holder: Node<'d, 'i>,
    field: &str,
) -> Result<Vec<(Node<'d, 'i>, usize)>, String> {
    let mut chain = vec![holder];
    while let Some(parent) = chain.last().and_then(|node| node.children().find(|child| child.attribute("xs.n") == Some("super"))) {
        if chain.len() > 64 {
            return Err("authoring super chain exceeds 64 nodes".into());
        }
        chain.push(graph.resolve(parent)?);
    }
    let mut found = Vec::new();
    for donor in holder.document().descendants().filter(|node| node.attribute("xs.n") == Some(field)) {
        if donor.attribute("xs.id").is_some() || donor.attribute("xs.ref").is_some() {
            continue;
        }
        let (Some(parent), Some(before)) = (donor.parent_element(), donor.prev_sibling_element()) else { continue };
        let Some(target) = chain.iter().find(|node| node.tag_name() == parent.tag_name()) else { continue };
        let Some(anchor) = before.attribute("xs.n")
            .and_then(|name| target.children().find(|child| child.attribute("xs.n") == Some(name))) else { continue };
        found.push((donor, anchor.range().end));
    }
    Ok(found)
}

/// A field the file lacks on `holder` (or on its `super` chain), copied from an element the file stores for the
/// same field under the same kind of parent, after the field that precedes it there, with `values` written in.
fn inserted_field<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    holder: Node<'d, 'i>,
    field: &str,
    values: &[(Option<&str>, String)],
) -> Result<XmlPatch, String> {
    let unknown = || format!("the CMO3 stores no {field} anywhere to copy its layout from");
    for (donor, end) in layouts(graph, holder, field)? {
        let range = donor.range();
        let mut value = String::new();
        let mut at = range.start;
        let mut replace = |span: std::ops::Range<usize>, new: &str| {
            value.push_str(&text[at..span.start]);
            value.push_str(new);
            at = span.end;
        };
        let mut written = 0;
        match values {
            [(None, new)] => {
                let content = donor.first_child().filter(|child| child.is_text() && donor.children().count() == 1)
                    .ok_or_else(unknown)?;
                replace(content.range(), new);
                written = 1;
            }
            _ => {
                for attribute in donor.attributes() {
                    if let Some((_, new)) = values.iter().find(|(name, _)| *name == Some(attribute.name())) {
                        replace(attribute.range_value(), new);
                        written += 1;
                    }
                }
            }
        }
        if written != values.len() {
            continue;
        }
        value.push_str(&text[at..range.end]);
        return Ok(XmlPatch { range: end..end, value });
    }
    Err(unknown())
}

fn mentions_outside(value: &Value, guid: &str, minimum: f64, maximum: f64) -> Option<f64> {
    match value {
        Value::Object(fields) => {
            if fields.get("parameterGuid").and_then(Value::as_str) == Some(guid) {
                let keys = fields.get("keys").and_then(Value::as_array).into_iter().flatten().chain(fields.get("keyValue"));
                if let Some(key) = keys.filter_map(Value::as_f64).find(|key| *key < minimum || *key > maximum) {
                    return Some(key);
                }
            }
            fields.values().find_map(|field| mentions_outside(field, guid, minimum, maximum))
        }
        Value::Array(items) => items.iter().find_map(|item| mentions_outside(item, guid, minimum, maximum)),
        _ => None,
    }
}

/// `live2d.parameter.edit {guid, minimum?, maximum?, default?, repeat?}`: the stored range, default value and
/// repeat flag of one parameter. The default must lie in the range, and so must every key a source has on it.
pub(crate) fn set_parameter(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    let index = model["parameters"].as_array().into_iter().flatten().position(|parameter| parameter["guid"] == guid)
        .ok_or("the parameter was not found")?;
    if !["minimum", "maximum", "default", "repeat"].iter().any(|key| request.get(*key).is_some_and(|value| !value.is_null())) {
        return Err("live2d.parameter.edit names no setting".into());
    }
    let mut result = model.clone();
    let parameter = &mut result["parameters"][index];
    for key in ["minimum", "maximum", "default"] {
        if let Some(value) = number(request, key, f32::MIN as f64, f32::MAX as f64)? {
            parameter[key] = value;
        }
    }
    if let Some(value) = flag(request, "repeat")? {
        parameter["repeat"] = value;
    }
    let [minimum, maximum, default] = ["minimum", "maximum", "default"].map(|key| parameter[key].as_f64().unwrap_or(f64::NAN));
    if !(minimum <= default && default <= maximum) {
        return Err("the parameter needs minimum <= default <= maximum".into());
    }
    if let Some(key) = mentions_outside(&result["sources"], guid, minimum, maximum) {
        return Err(format!("a source has a key at {key}, outside the new range"));
    }
    Ok(result)
}

fn find_source<'a>(model: &'a Value, guid: &str) -> Result<(usize, &'a Value), String> {
    let mut found = model["sources"].as_array().into_iter().flatten().enumerate()
        .filter(|(_, source)| source["guid"].as_str() == Some(guid));
    let source = found.next().ok_or("authoring edit source GUID was not found")?;
    if found.next().is_some() {
        return Err("authoring edit source GUID is ambiguous".into());
    }
    Ok(source)
}

fn number(request: &Value, key: &str, low: f64, high: f64) -> Result<Option<Value>, String> {
    let Some(value) = request.get(key).filter(|value| !value.is_null()) else { return Ok(None) };
    let number = value.as_f64().filter(|number| number.is_finite() && (low..=high).contains(number))
        .ok_or_else(|| format!("{key} must be a number in {low}..={high}"))?;
    Ok(Some(json!(number)))
}

fn flag(request: &Value, key: &str) -> Result<Option<Value>, String> {
    match request.get(key).filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(json!(flag))),
        Some(_) => Err(format!("{key} must be true or false")),
    }
}

fn color(request: &Value, key: &str) -> Result<Option<Vec<f64>>, String> {
    let Some(value) = request.get(key).filter(|value| !value.is_null()) else { return Ok(None) };
    let channels = value.as_array().filter(|channels| matches!(channels.len(), 3 | 4))
        .ok_or_else(|| format!("{key} must be [r, g, b] or [r, g, b, a]"))?;
    channels.iter().map(|channel| {
        channel.as_f64().filter(|channel| (0.0..=1.0).contains(channel))
            .ok_or_else(|| format!("{key} channels must be numbers in 0..=1"))
    }).collect::<Result<_, _>>().map(Some)
}

/// The source and primary form a request names by sourceGuid, formGuid and coordinateType (absent for Part and
/// Glue forms): their indices, the source kind and the form.
fn find_form<'a>(model: &'a Value, request: &Value) -> Result<(usize, usize, &'a str, &'a Value), String> {
    let source_guid = request["sourceGuid"].as_str().filter(|guid| !guid.is_empty()).ok_or("sourceGuid must be a string")?;
    let form_guid = request["formGuid"].as_str().filter(|guid| !guid.is_empty()).ok_or("formGuid must be a string")?;
    let coordinate = request.get("coordinateType").filter(|value| !value.is_null());
    if coordinate.is_some_and(|value| !value.is_string()) {
        return Err("coordinateType must be a string".into());
    }
    let (source_index, source) = find_source(model, source_guid)?;
    edit::ensure_form_editable(source, form_guid)?;
    let mut matching = source["forms"].as_array().into_iter().flatten().enumerate()
        .filter(|(_, form)| form["guid"].as_str() == Some(form_guid) && form.get("coordinateType").filter(|value| !value.is_null()) == coordinate);
    let (form_index, form) = matching.next().ok_or("authoring primary form was not found")?;
    if matching.next().is_some() {
        return Err("authoring edit form and coordinate type are ambiguous".into());
    }
    Ok((source_index, form_index, source["data"]["type"].as_str().unwrap_or_default(), form))
}

fn pair(request: &Value, key: &str) -> Result<Option<Value>, String> {
    let Some(value) = request.get(key).filter(|value| !value.is_null()) else { return Ok(None) };
    let pair = value.as_array().filter(|pair| pair.len() == 2)
        .and_then(|pair| Some([pair[0].as_f64()?, pair[1].as_f64()?]))
        .filter(|pair| pair.iter().all(|value| (*value as f32).is_finite()))
        .ok_or_else(|| format!("{key} must be two finite numbers"))?;
    Ok(Some(json!(pair)))
}

const POINT_KEYS: [&str; 7] = ["position", "startVelocity", "endVelocity", "width", "opacity", "color", "corner"];

/// Puts the control point values a request gives (the keys of `POINT_KEYS`, all optional) on `point`.
fn apply_point_values(request: &Value, point: &mut Value) -> Result<(), String> {
    for key in ["position", "startVelocity", "endVelocity"] {
        if let Some(value) = pair(request, key)? {
            point[key] = value;
        }
    }
    if let Some(width) = number(request, "width", 0.0, 1.0e6)? {
        point["width"] = width;
    }
    if let Some(opacity) = number(request, "opacity", 0.0, 1.0)? {
        point["opacity"] = opacity;
    }
    if let Some(channels) = color(request, "color")? {
        for (channel, value) in channels.into_iter().enumerate() {
            point["color"][channel] = json!(value);
        }
    }
    if let Some(corner) = flag(request, "corner")? {
        point["corner"] = corner;
    }
    Ok(())
}

/// `live2d.artpath.point.set` (issue #159, stroke editing): the stored values of control point `index` of one
/// ArtPath form. Each key is optional: position, startVelocity and endVelocity as [x, y], width (0 or more),
/// opacity (0..=1), color ([r, g, b] or [r, g, b, a] in 0..=1) and corner.
pub(crate) fn set_point(model: &Value, request: &Value) -> Result<Value, String> {
    let (source_index, form_index, kind, form) = find_form(model, request)?;
    if kind != "ArtPath" {
        return Err(format!("live2d.artpath.point.set edits ArtPath forms, not {kind}"));
    }
    if !POINT_KEYS.iter().any(|key| request.get(*key).is_some_and(|value| !value.is_null())) {
        return Err("live2d.artpath.point.set names no setting".into());
    }
    let index = request["index"].as_u64().ok_or("index must be a non-negative integer")? as usize;
    let point = form["data"]["points"].get(index).ok_or("index is outside the ArtPath form's points")?;
    let mut updated = point.clone();
    apply_point_values(request, &mut updated)?;
    let mut result = model.clone();
    result["sources"][source_index]["forms"][form_index]["data"]["points"][index] = updated;
    Ok(result)
}

/// The GUIDs of the control points of one ArtPath form, in order.
fn point_guids(form: &Value) -> Vec<String> {
    form["data"]["points"].as_array().into_iter().flatten()
        .filter_map(|point| point["guid"].as_str().map(str::to_owned)).collect()
}

/// The ArtPath a point request names: its index in the model, its forms and the number of points each holds.
/// One index names one point in each form, so the forms must hold the same number.
fn art_path<'a>(model: &'a Value, request: &Value, op: &str) -> Result<(usize, &'a [Value], usize), String> {
    let guid = request["sourceGuid"].as_str().filter(|guid| !guid.is_empty()).ok_or("sourceGuid must be a string")?;
    let (index, source) = find_source(model, guid)?;
    let kind = source["data"]["type"].as_str().unwrap_or_default();
    if kind != "ArtPath" {
        return Err(format!("{op} edits ArtPath sources, not {kind}"));
    }
    let forms = source["forms"].as_array().filter(|forms| !forms.is_empty()).ok_or("the ArtPath has no forms")?;
    let count = point_guids(&forms[0]).len();
    for form in forms {
        edit::ensure_form_editable(source, form["guid"].as_str().unwrap_or_default())?;
        if point_guids(form).len() != count {
            return Err("the ArtPath's forms hold different numbers of points".into());
        }
    }
    Ok((index, forms.as_slice(), count))
}

/// `live2d.artpath.point.add` (issue #159): a control point at `index` (0 to the number of points) of every
/// keyform of one ArtPath. It starts as a copy of the point it goes before (the last point when it is appended),
/// and the values the request gives (the keys of `live2d.artpath.point.set`) replace that copy's. There is no
/// midpoint insertion: that would need the spline formula (#175). The forms get one new GUID when they list the
/// same point GUIDs, and one each otherwise.
pub(crate) fn add_point(model: &Value, request: &Value, fresh: &mut dyn FnMut() -> Result<String, String>)
    -> Result<Value, String> {
    let (source_index, forms, count) = art_path(model, request, "live2d.artpath.point.add")?;
    let index = request["index"].as_u64().ok_or("index must be a non-negative integer")? as usize;
    if index > count {
        return Err("index is outside the ArtPath form's points".into());
    }
    if count == 0 {
        return Err("the ArtPath stores no point to copy".into());
    }
    let one_list = forms.iter().all(|form| point_guids(form) == point_guids(&forms[0]));
    let shared = if one_list { Some(fresh()?) } else { None };
    let mut result = model.clone();
    for (form_index, form) in forms.iter().enumerate() {
        let mut point = form["data"]["points"][index.min(count - 1)].clone();
        let guid = match &shared {
            Some(guid) => guid.clone(),
            None => fresh()?,
        };
        point["guid"] = json!(guid);
        apply_point_values(request, &mut point)?;
        result["sources"][source_index]["forms"][form_index]["data"]["points"].as_array_mut()
            .ok_or("the ArtPath form stores no points")?.insert(index, point);
    }
    Ok(result)
}

/// `live2d.artpath.point.delete` (issue #159): control point `index` of every keyform of one ArtPath. The last
/// point stays, because an add copies a stored point.
pub(crate) fn delete_point(model: &Value, request: &Value) -> Result<Value, String> {
    let (source_index, forms, count) = art_path(model, request, "live2d.artpath.point.delete")?;
    let index = request["index"].as_u64().ok_or("index must be a non-negative integer")? as usize;
    if index >= count {
        return Err("index is outside the ArtPath form's points".into());
    }
    if count == 1 {
        return Err("the last control point of an ArtPath cannot be deleted".into());
    }
    let mut result = model.clone();
    for form_index in 0..forms.len() {
        result["sources"][source_index]["forms"][form_index]["data"]["points"].as_array_mut()
            .ok_or("the ArtPath form stores no points")?.remove(index);
    }
    Ok(result)
}

/// One ArtPath form whose list of control point GUIDs the model changes: the form, and the old and new lists.
struct PointList {
    source: String,
    form: String,
    coordinate: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
}

fn point_list_changes(initial: &Value, model: &Value) -> Vec<PointList> {
    let mut found = Vec::new();
    for new in model["sources"].as_array().into_iter().flatten().filter(|source| source["data"]["type"] == "ArtPath") {
        let Some(old) = initial["sources"].as_array().into_iter().flatten()
            .find(|old| old["guid"] == new["guid"]) else {
            continue;
        };
        for form in new["forms"].as_array().into_iter().flatten() {
            let Some(before) = old["forms"].as_array().into_iter().flatten()
                .find(|before| {
                    before["guid"] == form["guid"] && before["coordinateType"] == form["coordinateType"]
                }) else {
                continue;
            };
            let (old_guids, new_guids) = (point_guids(before), point_guids(form));
            if old_guids != new_guids {
                found.push(PointList {
                    source: new["guid"].as_str().unwrap_or_default().to_owned(),
                    form: form["guid"].as_str().unwrap_or_default().to_owned(),
                    coordinate: form["coordinateType"].as_str().map(str::to_owned),
                    old: old_guids,
                    new: new_guids,
                });
            }
        }
    }
    found
}

/// Whether `node` is the `ctrlPtId` field of a control point, which names that point and nothing else.
fn names_a_point(node: Node<'_, '_>) -> bool {
    node.attribute("xs.n") == Some("ctrlPtId")
        && node.parent_element().is_some_and(|parent| parent.has_tag_name("CArtPathPoint"))
}

/// The opened file with the control points of every ArtPath form whose list of point GUIDs the model changed
/// rewritten to the model's list, or `None` when no list changed. A removed point goes with the objects only it
/// uses, and is refused when the file names it from anywhere else. A new point is a clone of a stored one (the
/// point it goes before, or the last point), with the objects only that point uses cloned beside it, so its
/// layout comes from the file; the clone keeps the stored values, which `authoring_edit::rewrite` then patches.
pub(crate) fn sync_points(
    container: &CaffDocument,
    initial: &Value,
    model: &Value,
) -> Result<Option<CaffDocument>, String> {
    let changed = point_list_changes(initial, model);
    if changed.is_empty() {
        return Ok(None);
    }
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml).map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let options = ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() };
    let document = Document::parse_with_options(text, options)
        .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let graph = Graph::new(&document)?;
    let root = graph.model()?.ok_or("authoring source contains no model")?;
    let sources = source_nodes(&graph, root)?;
    let mut taken: HashSet<String> =
        document.descendants().filter_map(|node| node.attribute("xs.id")).map(str::to_owned).collect();
    let by_id: HashMap<&str, Node<'_, '_>> =
        document.descendants().filter_map(|node| Some((node.attribute("xs.id")?, node))).collect();
    let mut refs: HashMap<&str, Vec<Node<'_, '_>>> = HashMap::new();
    for node in document.descendants() {
        if let Some(target) = node.attribute("xs.ref") {
            refs.entry(target).or_default().push(node);
        }
    }
    let mut next_idx =
        document.descendants().filter_map(|node| node.attribute("xs.idx")?.parse::<u64>().ok()).max().unwrap_or(0);
    let mut patches = Vec::new();
    // The entries to remove with their GUIDs, and the insertions: where, the entry to clone and the new GUID.
    let mut removed: Vec<Node<'_, '_>> = Vec::new();
    let mut removed_guids: HashSet<&str> = HashSet::new();
    let mut inserts: Vec<(usize, Node<'_, '_>, &str)> = Vec::new();
    for change in &changed {
        let source = *sources.get(change.source.as_str()).ok_or("an edited ArtPath was not found in the CMO3")?;
        let form_list = owned(stored_field(&graph, source, "keyforms")?, "keyforms")?;
        let mut found = None;
        for entry in form_list.children().filter(|entry| entry.is_element()) {
            let form = graph.resolve(entry)?;
            if graph.guid(form)? != Some(change.form.as_str())
                || edit::form_coordinate(&graph, form)? != change.coordinate
            {
                continue;
            }
            if found.replace(form).is_some() {
                return Err("an edited ArtPath form is ambiguous in the CMO3".into());
            }
        }
        let form = found.ok_or("an edited ArtPath form was not found in the CMO3")?;
        let list = owned(stored_field(&graph, form, "positions")?, "positions")?;
        let entries: Vec<Node<'_, '_>> = list.children().filter(|entry| entry.is_element()).collect();
        let mut stored = Vec::new();
        for entry in &entries {
            if graph.resolve(*entry)?.id() != entry.id() || !entry.has_tag_name("CArtPathPoint") {
                return Err("an ArtPath form's points are not stored inline".into());
            }
            stored.push(values::guid_field(&graph, *entry, "ctrlPtId")?);
        }
        if stored.len() != change.old.len()
            || stored.iter().zip(&change.old).any(|(stored, old)| *stored != Some(old.as_str()))
        {
            return Err("an ArtPath form's points do not match the model".into());
        }
        for (entry, guid) in entries.iter().zip(&change.old) {
            if !change.new.contains(guid) {
                removed.push(*entry);
                removed_guids.insert(guid.as_str());
            }
        }
        for (position, guid) in change.new.iter().enumerate() {
            if change.old.contains(guid) {
                continue;
            }
            // The new point goes before the next kept point and copies it; at the end it copies the last stored one.
            let next = change.new[position + 1..].iter().find_map(|next| change.old.iter().position(|old| old == next));
            let (at, template) = match next {
                Some(old) => (entries[old].range().start, entries[old]),
                None => {
                    let last = *entries.last().ok_or("the ArtPath form stores no point to copy")?;
                    (last.range().end, last)
                }
            };
            inserts.push((at, template, guid.as_str()));
        }
        if list.attribute("count").is_some() {
            patches.push(attribute_patch(text, list, "count", change.new.len().to_string())?);
        }
    }
    if !removed.is_empty() {
        let private = private_objects(&removed, &by_id, &refs)?;
        let gone: HashSet<NodeId> =
            removed.iter().chain(&private).flat_map(|root| root.descendants()).map(|node| node.id()).collect();
        let gone_ids: HashSet<&str> = removed.iter().chain(&private).flat_map(|root| root.descendants())
            .filter_map(|node| node.attribute("xs.id")).collect();
        // Another ArtPath's point may carry the same GUID (a duplicated ArtPath keeps its points' GUIDs): that is
        // its own point, not a dangling name.
        let is_point = |node: Node<'_, '_>| names_a_point(node) || node.attribute("xs.id").is_some_and(|id| {
            refs.get(id).is_some_and(|users| !users.is_empty() && users.iter().all(|user| names_a_point(*user)))
        });
        for node in document.descendants().filter(|node| node.is_element() && !gone.contains(&node.id())) {
            let names_it = node.attribute("uuid").is_some_and(|uuid| removed_guids.contains(uuid)) && !is_point(node);
            if names_it || node.attribute("xs.ref").is_some_and(|target| gone_ids.contains(target)) {
                return Err("a deleted control point is named from elsewhere in the file".into());
            }
        }
        for node in private.iter().chain(&removed) {
            patches.push(XmlPatch { range: node.range(), value: String::new() });
        }
    }
    // Each new GUID is written once per form; the objects only the copied points use are cloned once for it.
    let mut written: HashSet<&str> = HashSet::new();
    for (_, _, guid) in &inserts {
        if !written.insert(*guid) {
            continue;
        }
        let group: Vec<(usize, Node<'_, '_>)> = inserts.iter().filter(|(_, _, other)| other == guid)
            .map(|(at, template, _)| (*at, *template)).collect();
        let mut templates: Vec<Node<'_, '_>> = group.iter().map(|(_, template)| *template).collect();
        let mut seen = HashSet::new();
        templates.retain(|template| seen.insert(template.id()));
        let template_guid = values::guid_field(&graph, templates[0], "ctrlPtId")?.ok_or("a control point has no GUID")?;
        for template in &templates {
            if values::guid_field(&graph, *template, "ctrlPtId")? != Some(template_guid) {
                return Err("the control points to copy have different GUIDs".into());
            }
        }
        let mut private = private_objects(&templates, &by_id, &refs)?;
        // The new point needs a GUID object of its own even where something else, such as a point-to-ArtMesh binding,
        // also names the copied point's one.
        for template in &templates {
            let target = template.children().find(|node| node.attribute("xs.n") == Some("ctrlPtId"))
                .and_then(|field| field.attribute("xs.ref"));
            let Some(object) = target.and_then(|target| by_id.get(target)).copied() else { continue };
            if top_level(object) && !inside(object, &templates) && !private.iter().any(|node| node.id() == object.id()) {
                private.push(object);
            }
        }
        let mut ids = HashMap::new();
        let ids_of = templates.iter().chain(&private).flat_map(|root| root.descendants());
        for id in ids_of.filter_map(|node| node.attribute("xs.id")) {
            let fresh =
                (1..).map(|n| format!("{id}-point{n}")).find(|fresh| !taken.contains(fresh)).unwrap_or_default();
            taken.insert(fresh.clone());
            ids.insert(id.to_owned(), fresh);
        }
        let uuids = HashMap::from([(template_guid.to_owned(), (*guid).to_owned())]);
        for node in &private {
            let at = node.range().end;
            let value = cloned(text, *node, &ids, &uuids, &mut next_idx, &|_| None)?;
            patches.push(XmlPatch { range: at..at, value });
        }
        for (at, template) in group {
            let value = cloned(text, template, &ids, &uuids, &mut next_idx, &|_| None)?;
            patches.push(XmlPatch { range: at..at, value });
        }
    }
    // Insertions at one place keep the order they were pushed in; an empty range sorts before one that starts there.
    patches.sort_by_key(|patch| (patch.range.start, patch.range.end));
    if patches.windows(2).any(|pair| pair[0].range.end > pair[1].range.start) {
        return Err("point patch ranges overlap".into());
    }
    let mut rewritten = text.to_owned();
    for patch in patches.into_iter().rev() {
        rewritten.replace_range(patch.range, &patch.value);
    }
    Ok(Some(CaffDocument::parse(&container.replace_main_xml(rewritten.as_bytes())?)?))
}

/// `live2d.form.set`: the settings of one primary form. Each key is optional, and each applies only to its kind
/// of source: drawOrder (0..=1000) to ArtMesh and existing private ArtPath forms, colours to ArtMesh and Part,
/// angle, origin, scale and reflect flags to rotation deformer forms, intensity (0..=1) to Glue forms.
pub(crate) fn set_form(model: &Value, request: &Value) -> Result<Value, String> {
    let (source_index, form_index, kind, form) = find_form(model, request)?;
    let mut values: Vec<(&[&str], Value)> = Vec::new();
    let allowed = |keys: &[&str], kinds: &[&str]| -> Result<(), String> {
        match keys.iter().find(|key| request.get(**key).is_some_and(|value| !value.is_null())) {
            Some(key) if !kinds.contains(&kind) => Err(format!("{key} applies to {} forms, not {kind}", kinds.join(" and "))),
            _ => Ok(()),
        }
    };
    allowed(&["drawOrder"], &["ArtMesh", "ArtPath"])?;
    allowed(&["multiplyColor", "screenColor"], &["ArtMesh", "Part"])?;
    allowed(&["angle", "origin", "scale", "reflectX", "reflectY"], &["Rotation"])?;
    allowed(&["intensity"], &["Glue"])?;
    if let Some(order) = request.get("drawOrder").filter(|value| !value.is_null()) {
        values.push((&["drawOrder"], json!(draw_order(order)?)));
    }
    for key in ["multiplyColor", "screenColor"] {
        if let Some(channels) = color(request, key)? {
            let mut stored = form[key].clone();
            for (index, channel) in channels.into_iter().enumerate() {
                stored[index] = json!(channel);
            }
            values.push((if key == "multiplyColor" { &["multiplyColor"] } else { &["screenColor"] }, stored));
        }
    }
    if let Some(angle) = number(request, "angle", -1.0e6, 1.0e6)? {
        values.push((&["data", "angle"], angle));
    }
    if let Some(origin) = pair(request, "origin")? {
        values.push((&["data", "origin"], origin));
    }
    if let Some(scale) = number(request, "scale", 1.0e-6, 1.0e6)? {
        values.push((&["data", "scale"], scale));
    }
    for (key, path) in [("reflectX", &["data", "reflectX"] as &[&str]), ("reflectY", &["data", "reflectY"])] {
        if let Some(value) = flag(request, key)? {
            values.push((path, value));
        }
    }
    if let Some(intensity) = number(request, "intensity", 0.0, 1.0)? {
        values.push((&["data", "intensity"], intensity));
    }
    if values.is_empty() {
        return Err("live2d.form.set names no setting".into());
    }
    let mut result = model.clone();
    let target = &mut result["sources"][source_index]["forms"][form_index];
    for (path, value) in values {
        *at_mut(target, path) = value;
    }
    Ok(result)
}

/// `live2d.source.set`: the blend mode (colorComposition with alphaComposition, as Cubism names them) of an
/// ArtMesh or Part, offscreen drawing of a Part, clipping inversion and the stored closure of an ArtPath.
pub(crate) fn set_source(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    let (index, source) = find_source(model, guid)?;
    let data = &source["data"];
    let kind = data["type"].as_str().unwrap_or_default();
    let mut result = model.clone();
    let target = &mut result["sources"][index]["data"];
    let mut changed = false;
    for key in ["colorComposition", "alphaComposition"] {
        if let Some(value) = request.get(key).filter(|value| !value.is_null()) {
            let name = value.as_str().ok_or_else(|| format!("{key} must be a string"))?;
            if !matches!(kind, "ArtMesh" | "Part") {
                return Err(format!("{key} applies to ArtMesh and Part sources, not {kind}"));
            }
            target[key] = json!(name);
            changed = true;
        }
    }
    if changed {
        let (color, alpha) = (target["colorComposition"].as_str(), target["alphaComposition"].as_str());
        crate::native_blend::Mode::parse(color.unwrap_or_default(), alpha.unwrap_or_default())?;
    }
    for (key, kinds) in [("useOffscreen", &["Part"] as &[&str]), ("invertClippingMask", &["ArtMesh", "Part", "ArtPath"]),
        ("closed", &["ArtPath"])] {
        if let Some(value) = flag(request, key)? {
            if !kinds.contains(&kind) {
                return Err(format!("{key} applies to {} sources, not {kind}", kinds.join(" and ")));
            }
            target[key] = value;
            changed = true;
        }
    }
    if let Some(value) = request.get("divisionCount").filter(|value| !value.is_null()) {
        if kind != "ArtPath" {
            return Err(format!("divisionCount applies to ArtPath sources, not {kind}"));
        }
        target["divisionCount"] = json!(division_count(value)?);
        changed = true;
    }
    if let Some(value) = request.get("brushGuid").filter(|value| !value.is_null()) {
        if kind != "ArtPath" {
            return Err(format!("brushGuid applies to ArtPath sources, not {kind}"));
        }
        let guid = value.as_str().ok_or("brushGuid must be a string")?;
        edit::ensure_listed_brush(model, guid)?;
        target["brushGuid"] = json!(guid);
        changed = true;
    }
    if let Some(value) = request.get("clipGuids").filter(|value| !value.is_null()) {
        if !matches!(kind, "ArtMesh" | "Part" | "ArtPath") {
            return Err(format!("clipGuids applies to ArtMesh, Part and ArtPath sources, not {kind}"));
        }
        target["clipGuids"] = crate::authoring_clips::mask_list(model, guid, value)?;
        changed = true;
    }
    if !changed {
        return Err("live2d.source.set names no setting".into());
    }
    Ok(result)
}

fn division_count(value: &Value) -> Result<u32, String> {
    value.as_u64().and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "divisionCount must be an integer from 0 through 4294967295".into())
}

/// `live2d.brush.set {guid, drawingCurveType?, widthCoefficient?, textureUvData?, fixWidthData?, spriteData?, textureInput?}`: the
/// stored drawing settings of one ArtPath brush; each group names only the numbers it changes. A setting the file
/// does not store for the brush is refused.
pub(crate) fn set_brush(model: &Value, request: &Value) -> Result<Value, String> {
    let guid = request["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("guid must be a string")?;
    let brushes = model["brushes"].as_array().map(Vec::as_slice).unwrap_or_default();
    let index = brushes.iter().position(|brush| brush["guid"] == guid).ok_or_else(|| format!("brush {guid} was not found"))?;
    let mut parent = request;
    for (name, keys) in [
        ("textureInput", &["currentTextureInputData"][..]),
        ("currentTextureInputData", &["optionalTransformOnCanvas"][..]),
        ("optionalTransformOnCanvas", &["m00", "m01", "m02", "m10", "m11", "m12"][..]),
    ] {
        let Some(value) = parent.get(name) else { break };
        let object = value.as_object().ok_or_else(|| format!("{name} must be an object"))?;
        if let Some(unknown) = object.keys().find(|key| !keys.contains(&key.as_str())) {
            return Err(format!("{name} has no editable setting {unknown}"));
        }
        if name == "optionalTransformOnCanvas" && object.values().any(Value::is_null) {
            return Err("canvas transform coefficients must be finite numbers".into());
        }
        parent = value;
    }
    for (group, keys) in crate::authoring_scene::BRUSH_GROUPS {
        let Some(value) = request.get(group).filter(|value| !value.is_null()) else { continue };
        let names = value.as_object().ok_or_else(|| format!("{group} must be an object"))?;
        if let Some(unknown) = names.keys().find(|key| !keys.contains(&key.as_str())) {
            return Err(format!("{group} has no setting {unknown}"));
        }
    }
    let mut result = model.clone();
    let mut changed = false;
    for spec in BRUSH_SPECS {
        let value = at(request, spec.path);
        if value.is_null() {
            continue;
        }
        let name = spec.path.join(".");
        if at(&brushes[index], spec.path).is_null() {
            return Err(format!("the CMO3 stores no {name} for this brush; Soul2D edits only stored values"));
        }
        let value = if spec.path == ["drawingCurveType"] {
            let kind = value.as_str().filter(|kind| DRAWING_CURVE_TYPES.contains(kind))
                .ok_or_else(|| format!("drawingCurveType must be one of {}", DRAWING_CURVE_TYPES.join(", ")))?;
            json!(kind)
        } else {
            let number = value.as_f64().filter(|number| (*number as f32).is_finite())
                .ok_or_else(|| format!("{name} must be a finite number"))?;
            json!(number)
        };
        *at_mut(&mut result["brushes"][index], spec.path) = value;
        changed = true;
    }
    if !changed {
        return Err("live2d.brush.set names no setting".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring_edit::{rewrite, rewrite_with_copies, tests::test_container};
    use crate::authoring_scene;
    use crate::caff::CaffDocument;

    #[test]
    fn brush_texture_affine_edits_only_stored_attributes() {
        let brush = "<CArtPathBrush xs.id='brush'><CArtPathBrushGuid xs.n='guid' uuid='brush-guid'/>\
            <CArtPathBrush_TextureInput xs.n='textureInput'>\
            <GTexture2D xs.n='texture' xs.ref='texture'/>\
            <TextureState xs.n='textureState' v='MODEL_IMAGE'/>\
            <CTextureInput_ModelImage xs.n='currentTextureInputData' xs.id='current'>\
            <CModelImageGuid xs.n='_modelImageGuid' uuid='image-guid'/>\
            <ACTextureInput xs.n='super'><CArtPathBrush xs.n='_owner' xs.ref='brush'/>\
            <CAffine xs.n='optionalTransformOnCanvas' m00='1' m01='2' m02='3' m10='4' m11='5' m12='6'/>\
            </ACTextureInput></CTextureInput_ModelImage></CArtPathBrush_TextureInput></CArtPathBrush>";
        let original = crate::authoring_edit::tests::artmesh_xml().replace(
            "<CPartSource xs.n='rootPart' xs.ref='part'/>",
            &format!("<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
                <carray_list xs.n='brushes' count='1'>{brush}</carray_list></CArtPathBrushSetting>"));
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let transform = json!({"m00":11.0,"m01":12.0,"m02":13.0,"m10":14.0,"m11":15.0,"m12":16.0});
        let request = |value| json!({"guid":"brush-guid", "textureInput": {
            "currentTextureInputData": {"optionalTransformOnCanvas":value}}});
        let edited = set_brush(&initial, &request(transform.clone())).unwrap();
        let bytes = rewrite(&container, &initial, &edited).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), edited);
        let expected = original.replace("m00='1' m01='2' m02='3' m10='4' m11='5' m12='6'",
            "m00='11' m01='12' m02='13' m10='14' m11='15' m12='16'");
        assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), expected);
        assert_eq!(edited["brushes"][0]["textureInput"]["currentTextureInputData"]["optionalTransformOnCanvas"], transform);
        for value in [json!({"unknown":1}), json!({"m00":"bad"}), json!({"m00":1e300}),
            json!({"m00":2,"m01":null}), json!(null)] {
            assert!(set_brush(&initial, &request(value)).is_err());
        }
        assert!(set_brush(&initial, &json!({"guid":"brush-guid","widthCoefficient":1,
            "textureInput":{"texture":{}}})).unwrap_err().contains("no editable setting texture"));
        let mut missing = initial.clone();
        missing["brushes"][0]["textureInput"]["currentTextureInputData"]["optionalTransformOnCanvas"] = json!({});
        assert!(set_brush(&missing, &request(json!({"m00":2}))).unwrap_err().contains("stores no"));
        for (from, to) in [
            ("m00='1'", "m00='NaN'"),
            ("v='MODEL_IMAGE'", ""),
            ("uuid='image-guid'", ""),
            ("path='mesh.png'", "path='missing.png'"),
        ] {
            assert!(authoring_scene::read(&test_container(&original.replace(from, to))).is_err(), "{from}");
        }
        let partial = authoring_scene::read(&test_container(&original.replace("m00='1'", ""))).unwrap();
        assert!(partial["brushes"][0]["textureInput"]["currentTextureInputData"]["optionalTransformOnCanvas"].get("m00").is_none());
        assert!(set_brush(&partial, &request(json!({"m00":2}))).is_err());
        let transform = "<CAffine xs.n='optionalTransformOnCanvas' m00='1' m01='2' m02='3' m10='4' m11='5' m12='6'/>";
        let null = authoring_scene::read(&test_container(&original.replace(transform,
            "<null xs.n='optionalTransformOnCanvas'/>"))).unwrap();
        assert!(null["brushes"][0]["textureInput"]["currentTextureInputData"]["optionalTransformOnCanvas"].is_null());
        assert!(set_brush(&null, &request(json!({"m00":2}))).is_err());
        let shared = original.replace("count='1'><CArtPathBrush", "count='2'><CArtPathBrush")
            .replace("</CArtPathBrush></carray_list>", "</CArtPathBrush><CArtPathBrush>\
                <CArtPathBrushGuid xs.n='guid' uuid='second-brush'/>\
                <CArtPathBrush_TextureInput xs.n='textureInput'>\
                <CTextureInput_ModelImage xs.n='currentTextureInputData' xs.ref='current'/>\
                </CArtPathBrush_TextureInput></CArtPathBrush></carray_list>");
        let container = test_container(&shared);
        let initial = authoring_scene::read(&container).unwrap();
        assert_eq!(initial["brushes"].as_array().unwrap().len(), 2);
        let edited = set_brush(&initial, &request(json!({"m00":2}))).unwrap();
        assert!(rewrite(&container, &initial, &edited).is_err(), "a shared input cannot silently change the second brush");
    }

    #[test]
    fn brush_settings_rewrite_only_their_tokens_and_unstored_ones_are_refused() {
        let brush = "<CArtPathBrush xs.id='brush'><s xs.n='name'>Pencil</s>\
            <CArtPathBrushGuid xs.n='guid' uuid='brush-guid'/><CArtPathBrushId xs.n='id' idstr='ArtPathBrush'/>\
            <CArtPathDrawingCurveType xs.n='drawingCurveType' v='SPRITE'/>\
            <TextureUvData xs.n='textureUvData'><f xs.n='up'>0.0</f><f xs.n='repeatEnd'>0.75000006</f></TextureUvData>\
            <FixWidthData xs.n='fixWidthData'><f xs.n='upFixedWidth'>0.0</f><f xs.n='downFixedWidth'>0.0</f></FixWidthData>\
            <f xs.n='widthCoefficient'>1.0</f></CArtPathBrush>";
        let original = crate::authoring_edit::tests::artmesh_xml().replace(
            "<CPartSource xs.n='rootPart' xs.ref='part'/>",
            &format!("<CPartSource xs.n='rootPart' xs.ref='part'/><CArtPathBrushSetting xs.n='artPathBrushesSetting'>\
                <carray_list xs.n='brushes' count='1'>{brush}</carray_list></CArtPathBrushSetting>"));
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        assert_eq!(initial["brushes"][0], json!({"guid": "brush-guid", "id": "ArtPathBrush", "name": "Pencil",
            "drawingCurveType": "SPRITE", "widthCoefficient": 1.0,
            "textureUvData": {"up": 0.0, "repeatEnd": 0.75000006}, "fixWidthData": {"upFixedWidth": 0.0, "downFixedWidth": 0.0}}));

        let edited = set_brush(&initial, &json!({"guid": "brush-guid", "drawingCurveType": "TIP_USE", "widthCoefficient": 2.5,
            "textureUvData": {"repeatEnd": 0.5}, "fixWidthData": {"downFixedWidth": 3}})).unwrap();
        let bytes = rewrite(&container, &initial, &edited).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), edited);
        assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), original
            .replace("v='SPRITE'", "v='TIP_USE'")
            .replace("<f xs.n='widthCoefficient'>1.0</f>", "<f xs.n='widthCoefficient'>2.5</f>")
            .replace("<f xs.n='repeatEnd'>0.75000006</f>", "<f xs.n='repeatEnd'>0.5</f>")
            .replace("<f xs.n='downFixedWidth'>0.0</f>", "<f xs.n='downFixedWidth'>3</f>"));

        let refused = |request: Value| set_brush(&initial, &request).unwrap_err();
        assert!(refused(json!({"guid": "brush-guid", "spriteData": {"density": 2}})).contains("stores no spriteData.density"));
        assert!(refused(json!({"guid": "brush-guid", "textureUvData": {"center": 0.5}})).contains("stores no textureUvData.center"));
        assert!(refused(json!({"guid": "brush-guid", "textureUvData": {"middle": 0.5}})).contains("no setting middle"));
        assert!(refused(json!({"guid": "brush-guid", "drawingCurveType": "STROKE"})).contains("SPRITE, TIP_USE"));
        assert!(refused(json!({"guid": "brush-guid", "widthCoefficient": 1e300})).contains("finite number"));
        assert!(refused(json!({"guid": "brush-guid"})).contains("names no setting"));
        assert!(refused(json!({"guid": "nothing", "widthCoefficient": 1})).contains("not found"));
    }

    #[test]
    fn a_missing_colour_is_written_after_the_layout_the_file_uses_elsewhere() {
        let donor = "<CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType></ACDrawableForm>\n            <carray_list xs.n='positions'";
        let original = xml()
            .replacen(donor, "<CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType><f xs.n='opacity'>1</f>\
                <CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>\
                <CFloatColor xs.n='screenColor' red='0.1' green='0.2' blue='0.3' alpha='0.9'/></ACDrawableForm>\n            <carray_list xs.n='positions'", 1)
;
        assert!(original.contains("alpha='0.9'"), "the ArtPath form stores the donor");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let mesh = json!({"sourceGuid": "mesh-guid", "formGuid": "form-guid", "coordinateType": "Canvas", "screenColor": [0.5, 0.25, 0.0]});
        let edited = set_form(&initial, &mesh).unwrap();
        let bytes = rewrite(&container, &initial, &edited).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), edited);
        assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), original
            .replace("<CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>\n          </ACDrawableForm>",
                "<CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/><CFloatColor xs.n='screenColor' red='0.5' green='0.25' blue='0' alpha='1'/>\n          </ACDrawableForm>"));

        // Without an element to copy, the edit is refused.
        let bare = test_container(&xml());
        let bare_initial = authoring_scene::read(&bare).unwrap();
        let refused = rewrite(&bare, &bare_initial, &set_form(&bare_initial, &mesh).unwrap()).unwrap_err();
        assert!(refused.contains("stores no screenColor anywhere"), "{refused}");
    }

    #[test]
    fn a_parameter_range_and_default_are_rewritten_in_place_within_its_keys() {
        let original = crate::authoring_edit::tests::morph_artmesh_xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let edited = set_parameter(&initial, &json!({"guid": "parameter-guid", "maximum": 2.0, "default": 0.5})).unwrap();
        let bytes = rewrite(&container, &initial, &edited).unwrap();
        let reopened = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(authoring_scene::read(&reopened).unwrap(), edited);
        assert_eq!(String::from_utf8(reopened.main_xml()).unwrap(), original
            .replace("<f xs.n='maxValue'>1</f>", "<f xs.n='maxValue'>2</f>")
            .replace("<f xs.n='defaultValue'>0</f>", "<f xs.n='defaultValue'>0.5</f>"));

        let guid = json!({"guid": "parameter-guid"});
        let with = |extra: Value| {
            let mut request = guid.clone();
            request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            set_parameter(&initial, &request)
        };
        assert!(with(json!({"maximum": 0.5})).unwrap_err().contains("key at 1"), "the morph target keys it at 1");
        assert!(with(json!({"default": 3.0})).unwrap_err().contains("minimum <= default"));
        assert!(with(json!({})).unwrap_err().contains("names no setting"));
        let repeat = with(json!({"repeat": true})).unwrap();
        assert!(rewrite(&container, &initial, &repeat).unwrap_err().contains("stores no isRepeat"));
    }

    /// An ArtMesh with a stored draw order, multiply colour and blend mode, and a rotation deformer.
    fn xml() -> String {
        r#"<root fileFormatVersion='401000004'><CModelSource>
          <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
          <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='0'/></CParameterSourceSet>
          <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='2'><CArtMeshSource xs.ref='mesh'/><CArtPathSource xs.ref='path'/></carray_list></CDrawableSourceSet>
          <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='1'><CRotationDeformerSource xs.ref='rotation'/></carray_list></CDeformerSourceSet>
          <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet>
          <CPartSource xs.n='rootPart' xs.ref='part'/>
        </CModelSource>
        <CArtMeshSource xs.id='mesh'>
          <ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/><CArtMeshId xs.n='id' idstr='mesh'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
          <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1</float-array>
          <int-array xs.n='indices' count='3'>0 1 2</int-array><GTexture2D xs.n='texture' xs.ref='texture'/>
          <ColorComposition xs.n='colorComposition' v='NORMAL'/><AlphaComposition xs.n='alphaComposition' v='OVER'/>
          <b xs.n='invertClippingMask'> false </b>
          <carray_list xs.n='keyforms' count='1'><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
            <CFormGuid xs.n='guid' uuid='form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>
          </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>
            <i xs.n='drawOrder'>500</i><f xs.n='opacity'>0.75</f><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>
          </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
        </CArtMeshSource>
        <CRotationDeformerSource xs.id='rotation'>
          <ACDeformerSource xs.n='super'><CRotationDeformerGuid xs.n='guid' uuid='rotation-guid'/><CDeformerId xs.n='id' idstr='rotation'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDeformerSource>
          <carray_list xs.n='keyforms' count='1'><CRotationDeformerForm angle='15' originX='10' originY='20' scale='1' isReflectX='false' isReflectY='false'>
            <ACDeformerForm xs.n='super'><ACForm xs.n='super'><CFormGuid xs.n='guid' uuid='rotation-form'/><CRotationDeformerSource xs.n='_source' xs.ref='rotation'/></ACForm>
            <CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType></ACDeformerForm></CRotationDeformerForm></carray_list>
        </CRotationDeformerSource>
        <CArtPathSource xs.id='path'><ACDrawableSource xs.n='super'><CDrawableGuid xs.n='guid' uuid='path-guid'/>
          <CDrawableId xs.n='id' idstr='path'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
          <CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/><LineAlgorithm xs.n='lineAlgorithm' v='LCNS'/>
          <LineAlignment xs.n='lineAlignment' v='CENTER'/><b xs.n='isClosed'>false</b><i xs.n='divNum'>20</i>
          <carray_list xs.n='keyforms' count='1'><CArtPathForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
            <CFormGuid xs.n='guid' uuid='path-form'/><CArtPathSource xs.n='_source' xs.ref='path'/></ACForm>
            <CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType></ACDrawableForm>
            <carray_list xs.n='positions' count='1'><CArtPathPoint><CArtPathPointGuid xs.n='ctrlPtId' uuid='point-a'/>
              <CSplineCurvePoint xs.n='curvePointPosition'><GVector2 xs.n='point'><f xs.n='x'>1</f><f xs.n='y'>2</f></GVector2>
              <GVector2 xs.n='startVelocity'><f xs.n='x'>0.5</f><f xs.n='y'>-0.5</f></GVector2>
              <GVector2 xs.n='endVelocity'><f xs.n='x'>0.25</f><f xs.n='y'>0.75</f></GVector2></CSplineCurvePoint>
              <b xs.n='isCorner'>false</b><f xs.n='colorRed'>1</f><f xs.n='colorGreen'>0.5</f><f xs.n='colorBlue'>0.25</f>
              <f xs.n='colorAlpha'>1</f><f xs.n='opacity'>0.8</f><f xs.n='width'>3</f></CArtPathPoint></carray_list>
          </CArtPathForm></carray_list>
        </CArtPathSource>
        <CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
        <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
          <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
        </GTexture2D><Opaque xs.id='future-data'>keep-this-xml</Opaque></root>"#.into()
    }

    fn saved(container: &CaffDocument, initial: &Value, candidate: &Value) -> Result<String, String> {
        let bytes = rewrite(container, initial, candidate)?;
        Ok(String::from_utf8(CaffDocument::parse(&bytes)?.main_xml()).unwrap())
    }

    fn artpath_order_xml() -> String {
        xml().replace("<CArtPathForm><ACDrawableForm xs.n='super'>",
            "<CArtPathForm><ACDrawableForm xs.n='super'><i xs.n='drawOrder'>950</i>")
    }

    fn artpath_opacity_xml() -> String {
        artpath_order_xml().replace("<i xs.n='drawOrder'>950</i>",
            "<i xs.n='drawOrder'>950</i><f xs.n='opacity'>1.0</f>")
    }

    fn artpath_inversion_xml() -> String {
        artpath_opacity_xml().replace("<CArtPathSource xs.id='path'><ACDrawableSource xs.n='super'>",
            "<CArtPathSource xs.id='path'><ACDrawableSource xs.n='super'><b xs.n='invertClippingMask'>true</b>")
    }

    #[test]
    fn artpath_inversion_writes_only_its_existing_private_boolean() {
        let original = artpath_inversion_xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":false})).unwrap();
        assert_eq!(saved(&container, &initial, &candidate).unwrap(), original.replace(
            "<b xs.n='invertClippingMask'>true</b>", "<b xs.n='invertClippingMask'>false</b>"));
    }

    #[test]
    fn artpath_inversion_refuses_id_bearing_flag_storage() {
        let xml = artpath_inversion_xml().replace("<b xs.n='invertClippingMask'>true</b>",
            "<b xs.n='invertClippingMask' xs.id='shared-flag'>true</b>");
        let container = test_container(&xml);
        let initial = authoring_scene::read(&container).unwrap();
        let mut candidate = initial.clone();
        candidate["sources"].as_array_mut().unwrap().iter_mut().find(|source| source["guid"] == "path-guid")
            .unwrap()["data"]["invertClippingMask"] = json!(false);
        assert!(saved(&container, &initial, &candidate).is_err());
    }

    #[test]
    fn artpath_inversion_preserves_raw_xml_whitespace_and_attributes() {
        for whitespace in ["", " \t\n", "\r", "\r\n"] {
            let original = artpath_inversion_xml().replace("<b xs.n='invertClippingMask'>true</b>",
                &format!("<b xs.n='invertClippingMask' future='x>y'>{whitespace}true{whitespace}</b>"));
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let candidate = set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":false})).unwrap();
            assert_eq!(saved(&container, &initial, &candidate).unwrap(), original.replace(
                &format!(">{whitespace}true{whitespace}<"), &format!(">{whitespace}false{whitespace}<")));
        }
    }

    #[test]
    fn artpath_inversion_checks_the_entire_private_source_spine() {
        let flag = "<b xs.n='invertClippingMask'>true</b>";
        for body in [flag.to_owned(), format!("<ACDrawableSource xs.n='super'>{flag}</ACDrawableSource>"),
            format!("<ACDrawableSource xs.n='super'><ACParameterControllableSource xs.n='super'>{flag}</ACParameterControllableSource></ACDrawableSource>")]
        {
            let xml = format!("<CArtPathSource xs.id='legal-root'>{body}</CArtPathSource>");
            let document = Document::parse(&xml).unwrap();
            let patch = artpath_inversion_patch(&xml, document.root_element(), false).unwrap();
            assert_eq!(&xml[patch.range], "true");
            assert_eq!(patch.value, "false");
        }
        for body in [
            format!("{flag}{flag}"),
            format!("{flag}<ACDrawableSource xs.n='super'>{flag}</ACDrawableSource>"),
            format!("{flag}<ACDrawableSource xs.n='super' xs.id='shared'/>"),
            format!("{flag}<ACDrawableSource xs.n='super' xs.ref='shared'/>"),
            format!("{flag}<ACDrawableSource xs.n='super'/><ACDrawableSource xs.n='super'/>"),
            format!("{flag}<ACForm xs.n='super'/>"),
            format!("{flag}<ACDrawableSource xs.n='super'><ACForm xs.n='super'/></ACDrawableSource>"),
            format!("{flag}<ACDrawableSource xs.n='super'><ACParameterControllableSource xs.n='super'><ACForm xs.n='super'/></ACParameterControllableSource></ACDrawableSource>"),
            format!("{flag}<ACDrawableSource xs.n='super'><ACParameterControllableSource xs.n='super'/><ACParameterControllableSource xs.n='super'/></ACDrawableSource>"),
        ] {
            let xml = format!("<CArtPathSource>{body}</CArtPathSource>");
            let document = Document::parse(&xml).unwrap();
            assert!(artpath_inversion_patch(&xml, document.root_element(), false).is_err(), "{body}");
        }
    }

    #[test]
    fn artpath_inversion_refuses_nonprivate_or_encoded_boolean_storage() {
        for field in ["", "<i xs.n='invertClippingMask'>true</i>",
            "<b xs.n='invertClippingMask' xs.id='shared'>true</b>",
            "<b xs.n='invertClippingMask' xs.ref='shared'/>",
            "<b xs.n='invertClippingMask'>&#116;rue</b>",
            "<b xs.n='invertClippingMask'><![CDATA[true]]></b>",
            "<b xs.n='invertClippingMask'><!--keep-->true</b>",
            "<b xs.n='invertClippingMask'><b>true</b></b>",
            "<b xs.n='invertClippingMask'>true false</b>",
            "<b xs.n='invertClippingMask'>\u{a0}true</b>"]
        {
            let xml = format!("<CArtPathSource>{field}</CArtPathSource>");
            let document = Document::parse(&xml).unwrap();
            assert!(artpath_inversion_patch(&xml, document.root_element(), false).is_err(), "{field}");
        }
    }

    #[test]
    fn artpath_inversion_keeps_optional_source_api_and_original_model_checks() {
        let container = test_container(&artpath_inversion_xml());
        let initial = authoring_scene::read(&container).unwrap();
        for value in [json!(0), json!("false"), json!([]), json!({})] {
            assert!(set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":value})).is_err());
        }
        for request in [json!({"guid":"path-guid"}), json!({"guid":"path-guid","invertClippingMask":null}),
            json!({"guid":"path-guid","unknown":false}), json!({"invertClippingMask":false}),
            json!({"guid":null,"invertClippingMask":false}), json!({"guid":"","invertClippingMask":false}),
            json!({"guid":12,"invertClippingMask":false}), json!({"guid":"missing","invertClippingMask":false})]
        { assert!(set_source(&initial, &request).is_err(), "{request}"); }
        assert_eq!(set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":true})).unwrap(), initial);
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":false,
            "closed":null,"unknown":true})).unwrap();
        assert!(saved(&container, &initial, &candidate).is_ok());
        for kind in ["ArtMesh", "Part"] {
            let mut forged = candidate.clone();
            let source = forged["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap();
            source["data"]["type"] = json!(kind);
            assert!(changes(&initial, &forged).is_err());
        }
        assert!(saved(&container, &candidate, &initial).is_err());
        let mut duplicate = initial.clone();
        let path = duplicate["sources"].as_array().unwrap().iter().find(|s| s["guid"] == "path-guid").unwrap().clone();
        duplicate["sources"].as_array_mut().unwrap().push(path);
        assert!(set_source(&duplicate, &json!({"guid":"path-guid", "invertClippingMask":false})).is_err());
        let missing = test_container(&artpath_opacity_xml());
        let initial = authoring_scene::read(&missing).unwrap();
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "invertClippingMask":true})).unwrap();
        assert!(saved(&missing, &initial, &candidate).is_err());
    }

    #[test]
    fn artpath_inversion_cannot_bypass_its_private_writer() {
        let xml = artpath_inversion_xml();
        let document = Document::parse(&xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
        static ATTRIBUTE: Spec = spec(&["data", "invertClippingMask"], &["invertClippingMask"], Some("value"));
        static NESTED: Spec = spec(&["data", "invertClippingMask"], &["super", "invertClippingMask"], None);
        let inversion = SOURCE_SPECS.iter().find(|spec| spec.path == ["data","invertClippingMask"]).unwrap();
        let valid = FieldChange(FieldWrite::General(ScalarFieldChange {
            source:"path-guid".into(), owner:Owner::Source, form:None, point:None,
            spec:inversion, same:false, text:"false".into(),
        }));
        let written = patches(&xml, &graph, model, &[valid], &mut Budget::new(10_000)).unwrap();
        assert_eq!(written.len(),1);
        assert_eq!(&xml[written[0].range.clone()],"true");
        assert_eq!(written[0].value,"false");
        for (owner, form, point, spec, same, text, message) in [
            (Owner::Parameter, None, None, inversion, false, "false", "an edited source was not found"),
            (Owner::Source, Some(("path-form".into(),Some("Canvas".into()))), None, inversion, false, "false", "source-only"),
            (Owner::Source, None, Some(0), inversion, false, "false", "source-only"),
            (Owner::Source, None, None, &ATTRIBUTE, false, "false", "source-only"),
            (Owner::Source, None, None, &NESTED, false, "false", "source-only"),
            (Owner::Source, None, None, inversion, true, "false", "source-only"),
            (Owner::Source, None, None, inversion, false, "0", "requires a boolean"),
        ] {
            let forged = FieldChange(FieldWrite::General(ScalarFieldChange {
                source:"path-guid".into(), owner, form, point, spec, same, text:text.into(),
            }));
            let error = patches(&xml, &graph, model, &[forged], &mut Budget::new(10_000)).err().unwrap();
            assert!(error.contains(message),"{error}");
        }
        let initial = authoring_scene::read(&test_container(&artpath_inversion_xml())).unwrap();
        for metadata in ["extendedInterpolations", "morphTargets"] {
            let mut original = initial.clone();
            let source = original["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap();
            source[metadata] = if metadata == "morphTargets" {
                json!({"targets":[{"formGuid":"path-form"}],"blendWeightConstraints":[{}]})
            } else { json!([{}]) };
            let candidate = set_source(&original, &json!({"guid":"path-guid", "invertClippingMask":false})).unwrap();
            assert!(changes(&original, &candidate).is_ok());
        }
    }

    #[test]
    fn artpath_inversion_reverses_with_referenced_source_identity() {
        let original = artpath_inversion_xml().replace("<b xs.n='invertClippingMask'>true</b>",
            "<b xs.n='invertClippingMask'>false</b>")
            .replace("<CDrawableGuid xs.n='guid' uuid='path-guid'/>","<CDrawableGuid xs.n='guid' xs.ref='path-identity'/>")
            .replace("</root>","<CDrawableGuid xs.id='path-identity' uuid='path-guid'/></root>");
        assert!(original.contains("xs.ref='path-identity'"));
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_source(&initial,&json!({"guid":"path-guid","invertClippingMask":true})).unwrap();
        assert_eq!(saved(&container,&initial,&candidate).unwrap(),original.replace(
            "<b xs.n='invertClippingMask'>false</b>","<b xs.n='invertClippingMask'>true</b>"));
    }

    #[test]
    fn artpath_opacity_writes_only_the_existing_private_float() {
        for whitespace in ["", " \t\n", "\r", "\r\n"] {
            let original = artpath_opacity_xml().replace("<f xs.n='opacity'>1.0</f>",
                &format!("<f xs.n='opacity' future='x>y'>{whitespace}1.0{whitespace}</f>"));
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            for opacity in [0.5, -1.25, 1.00005, f32::MAX as f64, -(f32::MAX as f64)] {
                let candidate = edit::set_opacity(&initial, &json!({"sourceGuid":"path-guid",
                    "formGuid":"path-form", "coordinateType":"Canvas", "opacity":opacity})).unwrap();
                let written = saved(&container, &initial, &candidate).unwrap();
                assert_eq!(written, original.replace(&format!(">{whitespace}1.0{whitespace}<"),
                    &format!(">{whitespace}{opacity}{whitespace}<")));
                assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), candidate);
            }
        }
    }

    #[test]
    fn artpath_opacity_classifier_checks_original_cache_metadata() {
        let initial = authoring_scene::read(&test_container(&artpath_opacity_xml())).unwrap();
        let index = initial["sources"].as_array().unwrap().iter().position(|source| source["guid"] == "path-guid").unwrap();
        let mut original = initial.clone();
        original["sources"][index]["extendedInterpolations"] = json!([{}]);
        let mut candidate = initial;
        candidate["sources"][index]["forms"][0]["opacity"] = json!(0.5);
        assert!(changes(&original, &candidate).err().unwrap().contains("extended interpolation"));
    }

    #[test]
    fn artpath_opacity_refuses_nonliteral_shared_and_ambiguous_storage() {
        let original = artpath_opacity_xml();
        let field = "<f xs.n='opacity'>1.0</f>";
        let mut cases: Vec<String> = ["", "<i xs.n='opacity'>1</i>",
            "<f xs.n='opacity' xs.id='opacity'>1.0</f>",
            "<f xs.n='opacity' xs.ref='opacity'/><f xs.id='opacity'>1.0</f>",
            "<f xs.n='opacity'>&#49;.0</f>", "<f xs.n='opacity'>1.0&#32;</f>",
            "<f xs.n='opacity'>1.0<![CDATA[]]></f>", "<f xs.n='opacity'>1.0<!--keep--></f>",
            "<f xs.n='opacity'>1.0<b xs.n='future'>true</b></f>"]
            .iter().map(|stored| original.replace(field, stored)).collect();
        for (before, after) in [
            ("<CArtPathForm>", "<CArtPathForm xs.id='shared-form'>"),
            ("<CArtPathForm><ACDrawableForm xs.n='super'>", "<CArtPathForm><ACDrawableForm xs.n='super' xs.id='shared-base'>"),
            ("<carray_list xs.n='keyforms' count='1'><CArtPathForm>",
                "<carray_list xs.n='keyforms' count='1' xs.id='shared-list'><CArtPathForm>"),
            ("<CFormGuid xs.n='guid' uuid='path-form'/>",
                "<f xs.n='opacity'>0.9</f><CFormGuid xs.n='guid' uuid='path-form'/>"),
        ] {
            cases.push(original.replace(before, after));
        }
        let start = original.find("<CArtPathForm>").unwrap();
        let end = start + original[start..].find("</CArtPathForm>").unwrap() + "</CArtPathForm>".len();
        let form = &original[start..end];
        cases.push(original.replacen(form, "<CArtPathForm xs.ref='aliased-form'/>", 1)
            .replace("</root>", &format!("{}</root>", form.replace("<CArtPathForm>", "<CArtPathForm xs.id='aliased-form'>"))));
        let list = format!("<carray_list xs.n='keyforms' count='1'>{form}</carray_list>");
        cases.push(original.replace(&list, "<carray_list xs.n='keyforms' xs.ref='aliased-list'/>")
            .replace("</root>", &format!("{}</root>", list.replace("xs.n='keyforms'", "xs.id='aliased-list'"))));
        let base_start = form.find("<ACDrawableForm xs.n='super'>").unwrap();
        let base_end = form.find("</ACDrawableForm>").unwrap() + "</ACDrawableForm>".len();
        let base = &form[base_start..base_end];
        cases.push(original.replace(form, &form.replace(base, "<ACDrawableForm xs.n='super' xs.ref='aliased-base'/>"))
            .replace("</root>", &format!("{}</root>", base.replacen("xs.n='super'", "xs.id='aliased-base'", 1))));
        for (index, xml) in cases.into_iter().enumerate() {
            let container = test_container(&xml);
            let initial = authoring_scene::read(&container).unwrap();
            let candidate = edit::set_opacity(&initial, &json!({"sourceGuid":"path-guid",
                "formGuid":"path-form", "coordinateType":"Canvas", "opacity":0.5})).unwrap();
            assert!(saved(&container, &initial, &candidate).is_err(), "accepted storage {index}");
        }
    }

    #[test]
    fn artpath_opacity_preserves_batch_numeric_and_sequential_semantics() {
        let container = test_container(&artpath_opacity_xml());
        let initial = authoring_scene::read(&container).unwrap();
        let path = json!({"sourceGuid":"path-guid", "formGuid":"path-form", "coordinateType":"Canvas"});
        let mesh = json!({"sourceGuid":"mesh-guid", "formGuid":"form-guid", "coordinateType":"Canvas"});
        let index = initial["sources"].as_array().unwrap().iter().position(|source| source["guid"] == "path-guid").unwrap();
        for (method, value, expected) in [("overwrite", -1.25, -1.25), ("overwrite", 1.25, 1.25),
            ("add", -0.2, 0.6000000000000001), ("add", 0.2, 1.0), ("add", 1e300, 1.0), ("add", -1e300, 0.0)] {
            let candidate = edit::set_opacities(&initial, &json!({"forms":[path, mesh, path], "opacity":value,
                "method":method})).unwrap();
            assert_eq!(candidate["sources"][index]["forms"][0]["opacity"], json!(expected));
            assert_eq!(authoring_scene::read(&test_container(&saved(&container, &initial, &candidate).unwrap())).unwrap(), candidate);
        }
        for value in [json!(null), json!(true), json!("0.5"), json!(1e300)] {
            let mut request = path.clone();
            request["opacity"] = value;
            assert!(edit::set_opacity(&initial, &request).is_err());
        }
        let mut integer = path.clone();
        integer["opacity"] = json!(2);
        let candidate = edit::set_opacity(&initial, &integer).unwrap();
        assert_eq!(candidate["sources"][index]["forms"][0]["opacity"], json!(2.0));
        assert!(saved(&container, &initial, &candidate).is_ok());
    }

    #[test]
    fn artpath_opacity_writer_rejects_forged_identity_and_original_morph_bypass() {
        let original_xml = artpath_opacity_xml();
        let container = test_container(&original_xml);
        let initial = authoring_scene::read(&container).unwrap();
        let index = initial["sources"].as_array().unwrap().iter().position(|source| source["guid"] == "path-guid").unwrap();
        for identity in 0..6 {
            let mut candidate = initial.clone();
            let source = &mut candidate["sources"][index];
            source["forms"][0]["opacity"] = json!(0.5);
            match identity {
                0 => source["data"]["type"] = json!("ArtMesh"),
                1 => source["guid"] = json!("forged-source"),
                2 => source["forms"][0]["guid"] = json!("forged-form"),
                3 => source["forms"][0]["coordinateType"] = json!("DeformerLocal"),
                4 => source["forms"][0]["data"]["type"] = json!("ArtMesh"),
                _ => source["forms"][0]["opacity"] = json!(1e300),
            }
            assert!(saved(&container, &initial, &candidate).is_err(), "accepted forgery {identity}");
        }
        for (target, grid, value, accepted) in [(false, false, 0.5, false), (true, true, 0.5, false),
            (true, false, 0.5, true), (true, false, 1.25, false)] {
            let mut original = initial.clone();
            original["sources"][index]["morphTargets"] = json!({"targets":if target {
                json!([{"formGuid":"path-form"}]) } else { json!([]) }, "blendWeightConstraints":[{}]});
            original["sources"][index]["grid"]["cells"] = if grid { json!([{"formGuid":"path-form"}]) } else { json!([]) };
            let mut candidate = initial.clone();
            candidate["sources"][index]["forms"][0]["opacity"] = json!(value);
            assert_eq!(changes(&original, &candidate).is_ok(), accepted);
        }
        let xml = original_xml.replace("<CArtPathSource xs.n='_source' xs.ref='path'/></ACForm>",
            "<CArtPathSource xs.n='_source' xs.ref='path'/><ACForm xs.n='super'/><ACForm xs.n='super'/></ACForm>");
        let document = Document::parse(&xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
        let error = artpath_opacity_patch(&xml, &graph, model, "path-guid", "path-form", Some("Canvas"),
            1.0, 0.5, &mut Budget::new(10_000)).err().unwrap();
        assert!(error.contains("duplicate super"), "{error}");
    }

    #[test]
    fn artpath_opacity_private_spine_literals_and_writer_backstops() {
        let field = "<f xs.n='opacity'>1.0</f>";
        for literal in ["1e0", "1.00000", "+1.0"] {
            for anchor in ["<CArtPathForm>", "<CArtPathForm><ACDrawableForm xs.n='super'>",
                "<CFormGuid xs.n='guid' uuid='path-form'/>"] {
                let original = artpath_opacity_xml().replace(field, "")
                    .replace(anchor, &format!("{anchor}<f xs.n='opacity'>{literal}</f>"))
                    .replace("<CFormGuid xs.n='guid' uuid='path-form'/>", "<CFormGuid xs.n='guid' xs.ref='form-guid-ref'/>")
                    .replace("</root>", "<CFormGuid xs.id='form-guid-ref' uuid='path-form'/></root>");
                let container = test_container(&original);
                let initial = authoring_scene::read(&container).unwrap();
                let candidate = edit::set_opacity(&initial, &json!({"sourceGuid":"path-guid",
                    "formGuid":"path-form", "coordinateType":"Canvas", "opacity":0.5})).unwrap();
                assert_eq!(saved(&container, &initial, &candidate).unwrap(), original.replace(
                    &format!("<f xs.n='opacity'>{literal}</f>"), "<f xs.n='opacity'>0.5</f>"));
            }
        }
        for literal in ["1.0 2.0", "NaN", "\u{a0}1.0", "1.0&#32;", "<![CDATA[1.0]]>"] {
            let xml = artpath_opacity_xml().replace(field, &format!("<f xs.n='opacity'>{literal}</f>"));
            let document = Document::parse(&xml).unwrap();
            let graph = Graph::new(&document).unwrap();
            let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
            assert!(artpath_opacity_patch(&xml, &graph, model, "path-guid", "path-form", Some("Canvas"),
                1.0, 0.5, &mut Budget::new(10_000)).is_err(), "accepted literal {literal}");
        }
        let xml = artpath_opacity_xml();
        let document = Document::parse(&xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
        assert!(artpath_opacity_patch(&xml, &graph, model, "path-guid", "path-form", Some("Canvas"),
            0.75, 0.5, &mut Budget::new(10_000)).is_err());
        let opacity_spec = POINT_SPECS.iter().find(|spec| spec.path == ["opacity"]).unwrap();
        let forged = FieldChange(FieldWrite::General(ScalarFieldChange {
            source:"path-guid".into(), owner:Owner::Source, form:Some(("path-form".into(),Some("Canvas".into()))),
            point:None, spec:opacity_spec, same:false, text:"0.5".into(),
        }));
        assert!(patches(&xml, &graph, model, &[forged], &mut Budget::new(10_000)).is_err());
    }

    #[test]
    fn artpath_opacity_setter_rejects_missing_ambiguous_and_derived_owners() {
        let initial = authoring_scene::read(&test_container(&artpath_opacity_xml())).unwrap();
        let request = json!({"sourceGuid":"path-guid", "formGuid":"path-form", "coordinateType":"Canvas", "opacity":0.5});
        for key in ["sourceGuid", "formGuid", "coordinateType", "opacity"] {
            let mut missing = request.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(edit::set_opacity(&initial, &missing).is_err(), "missing {key}");
            for value in [json!(null), json!(""), json!(true)] {
                let mut invalid = request.clone();
                invalid[key] = value;
                assert!(edit::set_opacity(&initial, &invalid).is_err(), "invalid {key}");
            }
        }
        let index = initial["sources"].as_array().unwrap().iter().position(|source| source["guid"] == "path-guid").unwrap();
        let mut duplicate = initial.clone();
        let source = duplicate["sources"][index].clone();
        duplicate["sources"].as_array_mut().unwrap().push(source);
        assert!(edit::set_opacity(&duplicate, &request).unwrap_err().contains("ambiguous"));
        let mut duplicate = initial.clone();
        let form = duplicate["sources"][index]["forms"][0].clone();
        duplicate["sources"][index]["forms"].as_array_mut().unwrap().push(form);
        assert!(edit::set_opacity(&duplicate, &request).unwrap_err().contains("ambiguous"));
        let mut cached = initial.clone();
        cached["sources"][index]["extendedInterpolations"] = json!([{}]);
        assert!(edit::set_opacity(&cached, &request).unwrap_err().contains("extended interpolation"));
        for (target, grid, value, accepted) in [(false, false, 0.5, false), (true, true, 0.5, false),
            (true, false, 0.5, true), (true, false, 1.25, false)] {
            let mut original = initial.clone();
            original["sources"][index]["morphTargets"] = json!({"targets":if target {
                json!([{"formGuid":"path-form"}]) } else { json!([]) }, "blendWeightConstraints":[{}]});
            original["sources"][index]["grid"]["cells"] = if grid { json!([{"formGuid":"path-form"}]) } else { json!([]) };
            let mut edited = request.clone();
            edited["opacity"] = json!(value);
            assert_eq!(edit::set_opacity(&original, &edited).is_ok(), accepted);
        }
    }

    #[test]
    fn artpath_opacity_refuses_terminal_aliases_and_late_inheritance_ambiguity() {
        let original = artpath_opacity_xml();
        let start = original.find("<CArtPathForm>").unwrap();
        let terminal_start = start + original[start..].find("<ACForm xs.n='super'>").unwrap();
        let terminal_end = terminal_start + original[terminal_start..].find("</ACForm>").unwrap() + "</ACForm>".len();
        let terminal = &original[terminal_start..terminal_end];
        let aliased = original.replacen(terminal, "<ACForm xs.n='super' xs.ref='terminal'/>", 1)
            .replace("</root>", &format!("{}</root>", terminal.replacen("xs.n='super'", "xs.id='terminal'", 1)));
        for xml in [original.replace("<f xs.n='opacity'>1.0</f>",
                "<f xs.n='opacity'>1.0</f><f xs.n='opacity'>0.9</f>"),
            original.replace(terminal, &terminal.replacen("xs.n='super'", "xs.n='super' xs.id='terminal'", 1)),
            aliased,
            original.replace("<CArtPathSource xs.n='_source' xs.ref='path'/></ACForm>",
                "<CArtPathSource xs.n='_source' xs.ref='path'/><ACForm xs.n='super'/></ACForm>")] {
            let document = Document::parse(&xml).unwrap();
            let graph = Graph::new(&document).unwrap();
            let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
            assert!(artpath_opacity_patch(&xml, &graph, model, "path-guid", "path-form", Some("Canvas"),
                1.0, 0.5, &mut Budget::new(10_000)).is_err());
        }
    }

    #[test]
    fn artpath_order_writes_only_the_existing_private_integer() {
        for whitespace in ["", " \t\n", "\r", "\r\n"] {
            let original = artpath_order_xml().replace("<i xs.n='drawOrder'>950</i>",
                &format!("<i xs.n='drawOrder' future='x>y'>{whitespace}950{whitespace}</i>"));
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            for order in [0, 951, 1000] {
                let candidate = set_form(&initial, &json!({"sourceGuid":"path-guid","formGuid":"path-form",
                    "coordinateType":"Canvas","drawOrder":order})).unwrap();
                let written = saved(&container, &initial, &candidate).unwrap();
                assert_eq!(written, original.replace(&format!(">{whitespace}950{whitespace}<"),
                    &format!(">{whitespace}{order}{whitespace}<")));
                assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), candidate);
            }
        }
    }

    #[test]
    fn artpath_order_refuses_shared_escaped_missing_and_shadowed_storage() {
        let original = artpath_order_xml();
        let field = "<i xs.n='drawOrder'>950</i>";
        let mut cases: Vec<String> = ["", "<f xs.n='drawOrder'>950</f>",
            "<i xs.n='drawOrder' xs.id='order'>950</i>",
            "<i xs.n='drawOrder' xs.ref='order'/><i xs.id='order'>950</i>",
            "<i xs.n='drawOrder'>&#57;50</i>", "<i xs.n='drawOrder'>950&#32;</i>",
            "<i xs.n='drawOrder'>950<![CDATA[]]></i>", "<i xs.n='drawOrder'>950<!--keep--></i>",
            "<i xs.n='drawOrder'>950<b xs.n='future'>true</b></i>"]
            .iter().map(|stored| original.replace(field, stored)).collect();
        cases.push(original.replace("<CArtPathForm>", "<CArtPathForm xs.id='shared-form'>"));
        cases.push(original.replace("<CArtPathForm><ACDrawableForm xs.n='super'>",
            "<CArtPathForm><ACDrawableForm xs.n='super' xs.id='shared-base'>"));
        cases.push(original.replace("<carray_list xs.n='keyforms' count='1'><CArtPathForm>",
            "<carray_list xs.n='keyforms' count='1' xs.id='shared-list'><CArtPathForm>"));
        cases.push(original.replace("<CFormGuid xs.n='guid' uuid='path-form'/>",
            "<i xs.n='drawOrder'>940</i><CFormGuid xs.n='guid' uuid='path-form'/>"));
        cases.push(original.replace("<CArtPathForm><ACDrawableForm xs.n='super'>",
            "<CArtPathForm><ACDrawableForm xs.n='super' xs.id='shared-base'>")
            .replace("</root>", "<ACDrawableForm xs.ref='shared-base'/></root>"));
        let start = original.find("<CArtPathForm>").unwrap();
        let end = start + original[start..].find("</CArtPathForm>").unwrap() + "</CArtPathForm>".len();
        let form = &original[start..end];
        let aliased = original.replacen(form, "<CArtPathForm xs.ref='aliased-form'/>", 1)
            .replace("</root>", &format!("{}</root>", form.replace("<CArtPathForm>", "<CArtPathForm xs.id='aliased-form'>")));
        cases.push(aliased);
        let list = format!("<carray_list xs.n='keyforms' count='1'>{form}</carray_list>");
        cases.push(original.replace(&list, "<carray_list xs.n='keyforms' xs.ref='aliased-list'/>")
            .replace("</root>", &format!("{}</root>", list.replace("xs.n='keyforms'", "xs.id='aliased-list'"))));
        let base_start = form.find("<ACDrawableForm xs.n='super'>").unwrap();
        let base_end = form.find("</ACDrawableForm>").unwrap() + "</ACDrawableForm>".len();
        let base = &form[base_start..base_end];
        let aliased_form = form.replace(base, "<ACDrawableForm xs.n='super' xs.ref='aliased-base'/>");
        cases.push(original.replace(form, &aliased_form).replace("</root>",
            &format!("{}</root>", base.replacen("xs.n='super'", "xs.id='aliased-base'", 1))));
        for (index, xml) in cases.into_iter().enumerate() {
            let container = test_container(&xml);
            let initial = authoring_scene::read(&container).unwrap_or_else(|error| panic!("case {index}: {error}"));
            let mut candidate = initial.clone();
            candidate["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid")
                .unwrap()["forms"][0]["drawOrder"] = json!(951);
            assert!(saved(&container, &initial, &candidate).is_err(), "accepted {xml}");
        }
    }

    #[test]
    fn artpath_order_writer_checks_original_cache_metadata_and_rejects_other_scalars() {
        let initial = authoring_scene::read(&test_container(&artpath_order_xml())).unwrap();
        let index = initial["sources"].as_array().unwrap().iter().position(|s| s["guid"] == "path-guid").unwrap();
        let mut cached = initial.clone();
        cached["sources"][index]["extendedInterpolations"] = json!([{"formGuid":"generated"}]);
        let mut candidate = initial.clone();
        candidate["sources"][index]["forms"][0]["drawOrder"] = json!(951);
        assert!(changes(&cached, &candidate).is_err());
        for (field, value) in [("drawOrder", json!(1.0)), ("drawOrder", json!(1001)),
            ("multiplyColor", json!([0.5,1,1,1])), ("screenColor", json!([1,0,0,1]))] {
            let mut candidate = initial.clone();
            candidate["sources"][index]["forms"][0][field] = value;
            assert!(changes(&initial, &candidate).is_err(), "accepted {field}");
        }
        for (targets, on_grid, accepted) in [(true, false, true), (true, true, false), (false, false, false)] {
            let mut original = initial.clone();
            original["sources"][index]["morphTargets"] = json!({"targets":if targets {
                json!([{"formGuid":"path-form"}]) } else { json!([]) },
                "blendWeightConstraints":[{}]});
            original["sources"][index]["grid"]["cells"] = if on_grid { json!([{"formGuid":"path-form"}]) } else { json!([]) };
            let mut candidate = initial.clone();
            candidate["sources"][index]["forms"][0]["drawOrder"] = json!(951);
            assert_eq!(changes(&original, &candidate).is_ok(), accepted);
        }
    }

    #[test]
    fn artpath_order_checks_native_old_domain_forged_identity_and_late_duplicate_supers() {
        for before in [i32::MIN, -1, 1001, i32::MAX] {
            let original = artpath_order_xml().replace("<i xs.n='drawOrder'>950</i>",
                &format!("<i xs.n='drawOrder'>{before}</i>"));
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let candidate = set_form(&initial, &json!({"sourceGuid":"path-guid","formGuid":"path-form",
                "coordinateType":"Canvas","drawOrder":951})).unwrap();
            assert_eq!(saved(&container, &initial, &candidate).unwrap(), original.replace(
                &format!("<i xs.n='drawOrder'>{before}</i>"), "<i xs.n='drawOrder'>951</i>"));
        }
        let container = test_container(&artpath_order_xml());
        let initial = authoring_scene::read(&container).unwrap();
        let index = initial["sources"].as_array().unwrap().iter().position(|source| source["guid"] == "path-guid").unwrap();
        for identity in [0, 1, 2, 3, 4] {
            let mut candidate = initial.clone();
            let source = &mut candidate["sources"][index];
            source["forms"][0]["drawOrder"] = json!(951);
            match identity {
                0 => source["data"]["type"] = json!("ArtMesh"),
                1 => source["guid"] = json!("forged-source"),
                2 => source["forms"][0]["guid"] = json!("forged-form"),
                3 => source["forms"][0]["coordinateType"] = json!("DeformerLocal"),
                _ => source["forms"][0]["data"]["type"] = json!("ArtMesh"),
            }
            assert!(saved(&container, &initial, &candidate).is_err(), "accepted identity {identity}");
        }
        let xml = artpath_order_xml().replace("<CArtPathForm>", "<CArtPathForm>\
            <CFormGuid xs.n='guid' uuid='path-form'/><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>")
            .replace("<CArtPathSource xs.n='_source' xs.ref='path'/></ACForm>",
                "<CArtPathSource xs.n='_source' xs.ref='path'/><ACForm xs.n='super'/><ACForm xs.n='super'/></ACForm>");
        let document = Document::parse(&xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
        let error = artpath_draw_order_patch(&xml, &graph, model, "path-guid", "path-form", Some("Canvas"),
            950, 951, &mut Budget::new(10_000)).err().unwrap();
        assert!(error.contains("duplicate super"), "{error}");
    }

    #[test]
    fn artpath_order_keeps_identity_references_and_general_settings_routes() {
        let original = artpath_order_xml().replace("<CFormGuid xs.n='guid' uuid='path-form'/>",
            "<CFormGuid xs.n='guid' xs.ref='path-form-guid'/>")
            .replace("</root>", "<CFormGuid xs.id='path-form-guid' uuid='path-form'/></root>");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let changed = set_form(&initial, &json!({"sourceGuid":"path-guid","formGuid":"path-form",
            "coordinateType":"Canvas","drawOrder":951,"unknown":17,"screenColor":null})).unwrap();
        assert_eq!(saved(&container, &initial, &changed).unwrap(), original.replace(">950<", ">951<"));
        let mut point = set_point(&changed, &json!({"sourceGuid":"path-guid","formGuid":"path-form",
            "coordinateType":"Canvas","index":0,"width":4})).unwrap();
        point = set_source(&point, &json!({"guid":"path-guid","closed":true,"divisionCount":21})).unwrap();
        let written = saved(&container, &initial, &point).unwrap();
        assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), point);
        let order_spec = FORM_SPECS.iter().find(|spec| spec.path == ["drawOrder"]).unwrap();
        let forged = FieldChange(FieldWrite::General(ScalarFieldChange {
            source:"path-guid".into(), owner:Owner::Source, form:Some(("path-form".into(),Some("Canvas".into()))),
            point:None, spec:order_spec, same:false, text:"951".into(),
        }));
        let xml = container.main_xml();
        let text = std::str::from_utf8(&xml).unwrap();
        let document = Document::parse(text).unwrap();
        let graph = Graph::new(&document).unwrap();
        let model = document.descendants().find(|node| node.has_tag_name("CModelSource")).unwrap();
        assert!(patches(text, &graph, model, &[forged], &mut Budget::new(10_000)).is_err());
    }

    fn xml_with_listed_brushes(slot: &str, destination_id: bool) -> String {
        let destination_id = if destination_id { " xs.id='new'" } else { "" };
        let setting = format!("<CArtPathBrushSetting xs.n='artPathBrushesSetting'><carray_list xs.n='brushes' count='2'>\
            <CArtPathBrush><CArtPathBrushGuid xs.n='guid' xs.id='old' uuid='brush-guid'/></CArtPathBrush>\
            <CArtPathBrush><CArtPathBrushGuid xs.n='guid'{destination_id} uuid='brush-b'/></CArtPathBrush>\
            </carray_list></CArtPathBrushSetting>");
        xml().replace("</CModelSource>", &format!("{setting}</CModelSource>"))
            .replace("<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>", slot)
    }

    #[test]
    fn artpath_settings_division_and_brush_compose_with_closure() {
        let original = xml_with_listed_brushes("<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>", false)
            .replace("<i xs.n='divNum'>20</i>", "<i xs.n='divNum' future='x>y'>\r\n20\r\n</i>");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "divisionCount":21,
            "brushGuid":"brush-b", "closed":true})).unwrap();
        let written = saved(&container, &initial, &candidate).unwrap();
        assert_eq!(written, original.replace("\r\n20\r\n", "\r\n21\r\n")
            .replace("<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>",
                "<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-b'/>")
            .replace("<b xs.n='isClosed'>false</b>", "<b xs.n='isClosed'>true</b>"));
        assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), candidate);
        for count in [0_u32, u32::MAX] {
            let changed = set_source(&initial, &json!({"guid":"path-guid", "divisionCount":count})).unwrap();
            assert_eq!(saved(&container, &initial, &changed).unwrap(),
                original.replace("\r\n20\r\n", &format!("\r\n{count}\r\n")));
        }
    }

    #[test]
    fn artpath_settings_brush_links_preserve_other_bytes_and_shared_definitions() {
        for (slot, destination_id, expected) in [
            ("<CArtPathBrushGuid xs.n='brushGuid' uuid = 'brush-&#103;uid' future='keep &amp; this'/>", false,
             "<CArtPathBrushGuid xs.n='brushGuid' uuid = 'brush-b' future='keep &amp; this'/>"),
            ("<CArtPathBrushGuid xs.n='brushGuid' xs.ref = 'o&#108;d' future='keep'/>", true,
             "<CArtPathBrushGuid xs.n='brushGuid' xs.ref = 'new' future='keep'/>"),
            ("<CArtPathBrushGuid xs.n='brushGuid' xs.ref='old' future='keep'/>", false,
             "<CArtPathBrushGuid xs.n='brushGuid' uuid=\"brush-b\" future='keep'/>"),
        ] {
            let original = xml_with_listed_brushes(slot, destination_id);
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let changed = set_source(&initial, &json!({"guid":"path-guid", "brushGuid":"brush-b"})).unwrap();
            let written = saved(&container, &initial, &changed).unwrap();
            assert_eq!(written, original.replace(slot, expected));
            assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), changed);
        }
    }

    #[test]
    fn artpath_settings_brush_attributes_preserve_encoded_whitespace() {
        let slot = "<CArtPathBrushGuid xs.n='brushGuid' xs.ref='old'/>";
        for (encoded, decoded) in [("&#9;", "\t"), ("&#10;", "\n"), ("&#13;", "\r")] {
            for reference in [true, false] {
                let original = if reference {
                    xml_with_listed_brushes(slot, true).replace("xs.id='new'", &format!("xs.id='new{encoded}id'"))
                } else {
                    xml_with_listed_brushes(slot, false).replace("uuid='brush-b'", &format!("uuid='brush{encoded}b'"))
                };
                let container = test_container(&original);
                let initial = authoring_scene::read(&container).unwrap();
                let guid = if reference { "brush-b".to_owned() } else { format!("brush{decoded}b") };
                let changed = set_source(&initial, &json!({"guid":"path-guid", "brushGuid":guid})).unwrap();
                let written = saved(&container, &initial, &changed).unwrap();
                let expected = if reference { format!("xs.ref='new{encoded}id'") }
                    else { format!("uuid=\"brush{encoded}b\"") };
                assert_eq!(written, original.replace("xs.ref='old'", &expected));
                assert_eq!(authoring_scene::read(&test_container(&written)).unwrap(), changed);
            }
        }
    }

    #[test]
    fn artpath_settings_validate_requests_and_private_storage() {
        let slot = "<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>";
        let original = xml_with_listed_brushes(slot, false);
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        for value in [json!(-1), json!(1.5), json!(1.0), json!(4294967296_u64), json!(true), json!("21"), json!([])] {
            assert!(set_source(&initial, &json!({"guid":"path-guid", "closed":true, "divisionCount":value})).is_err());
        }
        for value in [json!(""), json!("missing"), json!("mesh-guid"), json!(false), json!(1), json!([])] {
            assert!(set_source(&initial, &json!({"guid":"path-guid", "closed":true, "brushGuid":value})).is_err());
        }
        for guid in ["mesh-guid", "part-guid", "rotation-guid"] {
            for setting in [json!({"divisionCount":21}), json!({"brushGuid":"brush-b"})] {
                let mut request = setting;
                request["guid"] = json!(guid);
                assert!(set_source(&initial, &request).is_err());
            }
        }
        let expected = set_source(&initial, &json!({"guid":"path-guid", "closed":true})).unwrap();
        assert_eq!(set_source(&initial, &json!({"guid":"path-guid", "closed":true,
            "divisionCount":null, "brushGuid":null, "unknown":17})).unwrap(), expected);
        let mut ambiguous = initial.clone();
        let duplicate = ambiguous["brushes"][1].clone();
        ambiguous["brushes"].as_array_mut().unwrap().push(duplicate);
        assert!(set_source(&ambiguous, &json!({"guid":"path-guid", "brushGuid":"brush-b"})).is_err());
        for stored in [
            "<i xs.n='divNum' xs.id='division'>20</i>",
            "<i xs.n='divNum' xs.ref='division'/><i xs.id='division'>20</i>",
            "<f xs.n='divNum'>20</f>", "<i xs.n='divNum'>&#50;0</i>",
            "<i xs.n='divNum'>20<![CDATA[]]></i>", "<i xs.n='divNum'>20<!--keep--></i>",
        ] {
            let invalid = test_container(&original.replace("<i xs.n='divNum'>20</i>", stored));
            let read = authoring_scene::read(&invalid).unwrap();
            let candidate = set_source(&read, &json!({"guid":"path-guid", "divisionCount":21})).unwrap();
            assert!(saved(&invalid, &read, &candidate).is_err(), "{stored}");
        }
        for stored in [
            "<CArtPathBrushGuid xs.n='brushGuid' xs.id='slot' uuid='brush-guid'/>",
            "<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'>keep</CArtPathBrushGuid>",
            "<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid' xs.ref='old'/>",
        ] {
            let invalid = test_container(&original.replace(slot, stored));
            let read = authoring_scene::read(&invalid).unwrap();
            let candidate = set_source(&read, &json!({"guid":"path-guid", "brushGuid":"brush-b"})).unwrap();
            assert!(saved(&invalid, &read, &candidate).is_err(), "{stored}");
        }
        for setting in [("divisionCount", json!(21)), ("brushGuid", json!("brush-b"))] {
            let mut forged = initial.clone();
            forged["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "mesh-guid").unwrap()["data"][setting.0] = setting.1;
            assert!(saved(&container, &initial, &forged).is_err());
        }
    }

    #[test]
    fn artpath_closed_rewrites_only_its_private_boolean_token() {
        let original = xml().replace("<b xs.n='isClosed'>false</b>", "<b xs.n='isClosed' future='a>b' other=\"c>d\"> \n false\t </b>")
            .replace("v='LCNS'", "v='FUTURE_ALGORITHM'").replace("v='CENTER'", "v='FUTURE_ALIGNMENT'");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "closed":true})).unwrap();
        let written = saved(&container, &initial, &candidate).unwrap();
        assert_eq!(written, original.replace("false\t </b>", "true\t </b>"));
        let reopened = authoring_scene::read(&test_container(&written)).unwrap();
        assert_eq!(reopened, candidate);
        let restored = set_source(&candidate, &json!({"guid":"path-guid", "closed":false})).unwrap();
        assert_eq!(restored, initial);
        assert_eq!(saved(&container, &initial, &restored).unwrap(), original);
        for bad in [json!(0), json!("true"), json!([]), json!({})] {
            assert!(set_source(&initial, &json!({"guid":"path-guid", "closed":bad})).is_err());
        }
        for guid in ["mesh-guid", "part-guid", "rotation-guid"] {
            assert!(set_source(&initial, &json!({"guid":guid, "closed":true})).is_err());
        }
        assert!(set_source(&initial, &json!({"guid":"path-guid", "closed":null})).is_err());
        assert_eq!(set_source(&initial, &json!({"guid":"path-guid", "closed":true, "unrelated":17})).unwrap(), candidate);
        assert_eq!(set_source(&initial, &json!({"guid":"mesh-guid", "closed":null, "invertClippingMask":true})).unwrap(),
            set_source(&initial, &json!({"guid":"mesh-guid", "invertClippingMask":true})).unwrap());
        let mut forged = initial.clone();
        forged["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "mesh-guid").unwrap()["data"]["closed"] = json!(true);
        assert!(saved(&container, &initial, &forged).is_err());
    }

    #[test]
    fn artpath_closed_preserves_literal_cr_and_crlf_whitespace() {
        for whitespace in ["\r\n", "\r"] {
            let original = xml().replace("<b xs.n='isClosed'>false</b>",
                &format!("<b xs.n='isClosed'>{whitespace}false{whitespace}</b>"));
            let container = test_container(&original);
            let initial = authoring_scene::read(&container).unwrap();
            let candidate = set_source(&initial, &json!({"guid":"path-guid", "closed":true})).unwrap();
            assert_eq!(saved(&container, &initial, &candidate).unwrap(), original.replace(
                &format!("{whitespace}false{whitespace}"), &format!("{whitespace}true{whitespace}")));
        }
    }

    #[test]
    fn artpath_closed_refuses_ambiguous_or_nonprivate_storage() {
        for (replacement, readable) in [
            ("", false), ("<b xs.n='isClosed'>false</b><b xs.n='isClosed'>false</b>", false),
            ("<b xs.n='isClosed' xs.id='closed'>false</b>", true),
            ("<b xs.n='isClosed' xs.ref='closed'/><b xs.id='closed'>false</b>", true),
            ("<b xs.n='isClosed'><b>false</b></b>", false), ("<b xs.n='isClosed'>f&#97;lse</b>", true),
            ("<s xs.n='isClosed'>false</s>", true), ("<b xs.n='isClosed'><![CDATA[false]]></b>", true),
            ("<b xs.n='isClosed'>false<!--keep--></b>", true),
            ("<b xs.n='isClosed'>false<![CDATA[]]></b>", true),
            ("<b xs.n='isClosed'>false<![CDATA[ ]]></b>", true),
            ("<b xs.n='isClosed'>false&#32;</b>", true),
            ("<b xs.n='isClosed'><![CDATA[]]>false</b>", true),
            ("<b xs.n='isClosed'>false&#x20;</b>", true),
        ] {
            let original = xml().replace("<b xs.n='isClosed'>false</b>", replacement);
            let container = test_container(&original);
            let read = authoring_scene::read(&container);
            assert_eq!(read.is_ok(), readable, "{replacement}: {read:?}");
            let Ok(initial) = read else { continue };
            let candidate = set_source(&initial, &json!({"guid":"path-guid", "closed":true})).unwrap();
            assert!(saved(&container, &initial, &candidate).is_err(), "{replacement}");
        }
        let original = xml().replace("<b xs.n='isClosed'>false</b>", "")
            .replace("<CDrawableId xs.n='id' idstr='path'/>",
                "<CDrawableId xs.n='id' idstr='path'/><b xs.n='isClosed'>false</b>");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let candidate = set_source(&initial, &json!({"guid":"path-guid", "closed":true})).unwrap();
        assert!(saved(&container, &initial, &candidate).is_err());
    }

    #[test]
    fn form_and_source_settings_rewrite_only_their_tokens() {
        let original = xml();
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let mesh = json!({"sourceGuid": "mesh-guid", "formGuid": "form-guid", "coordinateType": "Canvas"});
        let mut request = mesh.clone();
        request["drawOrder"] = json!(620);
        request["multiplyColor"] = json!([0.5, 0.25, 1.0]);
        let candidate = set_form(&initial, &request).unwrap();
        let mut request = json!({"sourceGuid": "rotation-guid", "formGuid": "rotation-form", "coordinateType": "Canvas"});
        request["angle"] = json!(-30.5);
        request["origin"] = json!([12.0, 24.0]);
        request["reflectX"] = json!(true);
        let candidate = set_form(&candidate, &request).unwrap();
        let candidate = set_source(&candidate, &json!({"guid": "mesh-guid", "colorComposition": "MULTIPLY_R2",
            "invertClippingMask": true})).unwrap();
        let candidate = set_point(&candidate, &json!({"sourceGuid": "path-guid", "formGuid": "path-form",
            "coordinateType": "Canvas", "index": 0, "position": [7.5, 2.0], "endVelocity": [0.25, -1.0], "width": 4.5,
            "color": [0.0, 0.5, 0.25], "corner": true})).unwrap();
        let written = saved(&container, &initial, &candidate).unwrap();
        assert_eq!(written, original
            .replace("<i xs.n='drawOrder'>500</i>", "<i xs.n='drawOrder'>620</i>")
            .replace("red='1' green='1' blue='1'", "red='0.5' green='0.25' blue='1'")
            .replace("angle='15' originX='10' originY='20' scale='1' isReflectX='false'",
                "angle='-30.5' originX='12' originY='24' scale='1' isReflectX='true'")
            .replace("v='NORMAL'", "v='MULTIPLY_R2'")
            .replace("<b xs.n='invertClippingMask'> false </b>", "<b xs.n='invertClippingMask'> true </b>")
            .replace("<f xs.n='x'>1</f><f xs.n='y'>2</f>", "<f xs.n='x'>7.5</f><f xs.n='y'>2</f>")
            .replace("<f xs.n='x'>0.25</f><f xs.n='y'>0.75</f>", "<f xs.n='x'>0.25</f><f xs.n='y'>-1</f>")
            .replace("<b xs.n='isCorner'>false</b><f xs.n='colorRed'>1</f>", "<b xs.n='isCorner'>true</b><f xs.n='colorRed'>0</f>")
            .replace("<f xs.n='width'>3</f>", "<f xs.n='width'>4.5</f>"));

        // Values the file does not store, and settings of another kind of source, are refused.
        let mut absent = mesh.clone();
        absent["screenColor"] = json!([0.5, 0.5, 0.5]);
        let screen = set_form(&initial, &absent).unwrap();
        assert!(saved(&container, &initial, &screen).unwrap_err().contains("stores no screenColor"));
        assert!(set_source(&initial, &json!({"guid": "mesh-guid", "useOffscreen": true})).unwrap_err().contains("Part"));
        let mut wrong = mesh.clone();
        wrong["angle"] = json!(1.0);
        assert!(set_form(&initial, &wrong).unwrap_err().contains("Rotation"));
        assert!(set_source(&initial, &json!({"guid": "mesh-guid", "colorComposition": "PLUS"})).is_err());
        let mut far = mesh.clone();
        far["drawOrder"] = json!(1001);
        assert!(set_form(&initial, &far).is_err());
        assert!(set_form(&initial, &mesh).unwrap_err().contains("names no setting"));
        let path = json!({"sourceGuid": "path-guid", "formGuid": "path-form", "coordinateType": "Canvas", "index": 1,
            "width": 2.0});
        assert!(set_point(&initial, &path).unwrap_err().contains("outside"));
        let mut opaque = path.clone();
        opaque["index"] = json!(0);
        opaque["opacity"] = json!(1.5);
        assert!(set_point(&initial, &opaque).is_err());
        let mut on_mesh = mesh.clone();
        on_mesh["index"] = json!(0);
        on_mesh["width"] = json!(1.0);
        assert!(set_point(&initial, &on_mesh).unwrap_err().contains("ArtPath"));
    }

    fn named(uuid: &str) -> String {
        format!("<CArtPathPointGuid xs.n='ctrlPtId' uuid='{uuid}'/>")
    }

    fn shared(id: &str) -> String {
        format!("<CArtPathPointGuid xs.n='ctrlPtId' xs.ref='{id}'/>")
    }

    /// A control point with the given `ctrlPtId` element, position and width; the other values are fixed.
    fn point_xml(guid: &str, x: f64, y: f64, width: f64) -> String {
        format!("<CArtPathPoint>{guid}<CSplineCurvePoint xs.n='curvePointPosition'><GVector2 xs.n='point'>\
            <f xs.n='x'>{x}</f><f xs.n='y'>{y}</f></GVector2>\
            <GVector2 xs.n='startVelocity'><f xs.n='x'>0.5</f><f xs.n='y'>-0.5</f></GVector2>\
            <GVector2 xs.n='endVelocity'><f xs.n='x'>0.25</f><f xs.n='y'>0.75</f></GVector2></CSplineCurvePoint>\
            <b xs.n='isCorner'>false</b><f xs.n='colorRed'>1</f><f xs.n='colorGreen'>0.5</f>\
            <f xs.n='colorBlue'>0.25</f>\
            <f xs.n='colorAlpha'>1</f><f xs.n='opacity'>0.8</f><f xs.n='width'>{width}</f></CArtPathPoint>")
    }

    fn form_xml(source: &str, form: &str, points: &[String]) -> String {
        format!("<CArtPathForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'><CFormGuid xs.n='guid' uuid='{form}'/>\
            <CArtPathSource xs.n='_source' xs.ref='{source}'/></ACForm><CoordType xs.n='coordType'>\
            <s xs.n='coordName'>Canvas</s></CoordType></ACDrawableForm>\
            <carray_list xs.n='positions' count='{}'>{}</carray_list></CArtPathForm>", points.len(), points.concat())
    }

    /// The fixture with its ArtPath replaced by one source per `(id, forms)`, whose GUID is `<id>-guid`, and `extra`
    /// (top-level objects) after them.
    fn paths_xml(paths: &[(&str, Vec<String>)], extra: &str) -> String {
        let original = xml();
        let start = original.find("<CArtPathSource xs.id='path'>").unwrap();
        let end = original.find("</CArtPathSource>").unwrap() + "</CArtPathSource>".len();
        let refs: String = paths.iter().map(|(id, _)| format!("<CArtPathSource xs.ref='{id}'/>")).collect();
        let sources: String = paths.iter().map(|(id, forms)| format!("<CArtPathSource xs.id='{id}'>\
            <ACDrawableSource xs.n='super'><CDrawableGuid xs.n='guid' uuid='{id}-guid'/>\
            <CDrawableId xs.n='id' idstr='{id}'/>\
            <CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>\
            <CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/><LineAlgorithm xs.n='lineAlgorithm' v='LCNS'/>\
            <LineAlignment xs.n='lineAlignment' v='CENTER'/><b xs.n='isClosed'>false</b><i xs.n='divNum'>20</i>\
            <carray_list xs.n='keyforms' count='{}'>{}</carray_list></CArtPathSource>",
            forms.len(), forms.concat())).collect();
        let head = original[..start].replace("count='2'><CArtMeshSource xs.ref='mesh'/><CArtPathSource xs.ref='path'/>",
            &format!("count='{}'><CArtMeshSource xs.ref='mesh'/>{refs}", paths.len() + 1));
        format!("{head}{sources}{extra}{}", &original[end..])
    }

    fn source<'a>(model: &'a Value, guid: &str) -> &'a Value {
        model["sources"].as_array().unwrap().iter().find(|source| source["guid"] == guid).unwrap()
    }

    /// The bytes `rewrite_with_copies` writes for `candidate`, as text, after checking that they read back equal.
    fn saved_points(container: &CaffDocument, initial: &Value, candidate: &Value) -> Result<String, String> {
        let bytes = rewrite_with_copies(container, initial, candidate, &[])?;
        let reopened = CaffDocument::parse(&bytes)?;
        assert_eq!(authoring_scene::read(&reopened)?, *candidate);
        Ok(String::from_utf8(reopened.main_xml()).unwrap())
    }

    #[test]
    fn a_control_point_is_added_to_every_form_as_a_copy_of_its_neighbour() {
        let original = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[point_xml(&named("point-a"), 1.0, 2.0, 3.0)]),
            form_xml("path", "form-2", &[point_xml(&named("point-a"), 3.0, 2.0, 3.0)]),
        ])], "");
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let request = json!({"sourceGuid": "path-guid", "index": 0, "position": [9.0, 8.0], "width": 6.0});
        let added = add_point(&initial, &request, &mut || Ok("new-point".into())).unwrap();
        for form in 0..2usize {
            let before = &source(&initial, "path-guid")["forms"][form]["data"]["points"];
            let points = &source(&added, "path-guid")["forms"][form]["data"]["points"];
            assert_eq!(points.as_array().unwrap().len(), 2);
            assert_eq!(points[1], before[0], "the stored point stays");
            assert_eq!((&points[0]["guid"], &points[0]["position"], &points[0]["width"]),
                (&json!("new-point"), &json!([9.0, 8.0]), &json!(6.0)));
            for key in ["startVelocity", "endVelocity", "opacity", "color", "corner"] {
                assert_eq!(points[0][key], before[0][key], "{key} is copied from the neighbour");
            }
        }
        let expected = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[
                point_xml(&named("new-point"), 9.0, 8.0, 6.0),
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
            ]),
            form_xml("path", "form-2", &[
                point_xml(&named("new-point"), 9.0, 8.0, 6.0),
                point_xml(&named("point-a"), 3.0, 2.0, 3.0),
            ]),
        ])], "");
        assert_eq!(saved_points(&container, &initial, &added).unwrap(), expected);

        // At the end, the new point copies the last one.
        let request = json!({"sourceGuid": "path-guid", "index": 1, "corner": true});
        let tail = add_point(&initial, &request, &mut || Ok("tail".into())).unwrap();
        for form in 0..2usize {
            let points = &source(&tail, "path-guid")["forms"][form]["data"]["points"];
            assert_eq!(
                (&points[0]["guid"], &points[1]["guid"], &points[1]["corner"]),
                (&json!("point-a"), &json!("tail"), &json!(true))
            );
        }
        let written = saved_points(&container, &initial, &tail).unwrap();
        assert_eq!(written.matches("<b xs.n='isCorner'>true</b>").count(), 2, "{written}");
        assert!(written.find("uuid='point-a'").unwrap() < written.find("uuid='tail'").unwrap());
    }

    #[test]
    fn a_shared_point_guid_object_is_cloned_once_and_deleted_with_its_points() {
        let object = "<CArtPathPointGuid xs.id='point-a-object' uuid='point-a'/>";
        let clone = "<CArtPathPointGuid xs.id='point-a-object-point1' uuid='new-point'/>";
        let original = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[point_xml(&shared("point-a-object"), 1.0, 2.0, 3.0)]),
            form_xml("path", "form-2", &[point_xml(&shared("point-a-object"), 3.0, 2.0, 3.0)]),
        ])], object);
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let request = json!({"sourceGuid": "path-guid", "index": 0});
        let added = add_point(&initial, &request, &mut || Ok("new-point".into())).unwrap();
        let expected = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[
                point_xml(&shared("point-a-object-point1"), 1.0, 2.0, 3.0),
                point_xml(&shared("point-a-object"), 1.0, 2.0, 3.0),
            ]),
            form_xml("path", "form-2", &[
                point_xml(&shared("point-a-object-point1"), 3.0, 2.0, 3.0),
                point_xml(&shared("point-a-object"), 3.0, 2.0, 3.0),
            ]),
        ])], &format!("{object}{clone}"));
        let two_points = saved_points(&container, &initial, &added).unwrap();
        assert_eq!(two_points, expected);

        // Deleting the stored point removes its entries and the object only they use; the clone stays.
        let after = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[point_xml(&shared("point-a-object-point1"), 1.0, 2.0, 3.0)]),
            form_xml("path", "form-2", &[point_xml(&shared("point-a-object-point1"), 3.0, 2.0, 3.0)]),
        ])], clone);
        let reopened = test_container(&two_points);
        let reopened_initial = authoring_scene::read(&reopened).unwrap();
        let deleted = delete_point(&reopened_initial, &json!({"sourceGuid": "path-guid", "index": 1})).unwrap();
        assert_eq!(saved_points(&reopened, &reopened_initial, &deleted).unwrap(), after);

        // The same result in one save: the point to copy is deleted in the same save, so its clone is made from
        // the original text.
        let at_once = delete_point(&added, &json!({"sourceGuid": "path-guid", "index": 1})).unwrap();
        assert_eq!(saved_points(&container, &initial, &at_once).unwrap(), after);
    }

    #[test]
    fn a_point_guid_object_another_object_names_is_cloned_for_the_new_point() {
        // A binding of the stored point (any other top-level object naming its GUID object) keeps naming the original.
        let object = "<CArtPathPointGuid xs.id='point-a-object' uuid='point-a'/>";
        let binding = "<PointBinding xs.id='binding'><CArtPathPointGuid xs.n='key' xs.ref='point-a-object'/></PointBinding>";
        let clone = "<CArtPathPointGuid xs.id='point-a-object-point1' uuid='new-point'/>";
        let original = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[point_xml(&shared("point-a-object"), 1.0, 2.0, 3.0)]),
            form_xml("path", "form-2", &[point_xml(&shared("point-a-object"), 3.0, 2.0, 3.0)]),
        ])], &format!("{object}{binding}"));
        let container = test_container(&original);
        let initial = authoring_scene::read(&container).unwrap();
        let added = add_point(&initial, &json!({"sourceGuid": "path-guid", "index": 0}), &mut || Ok("new-point".into())).unwrap();
        let expected = paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[
                point_xml(&shared("point-a-object-point1"), 1.0, 2.0, 3.0),
                point_xml(&shared("point-a-object"), 1.0, 2.0, 3.0),
            ]),
            form_xml("path", "form-2", &[
                point_xml(&shared("point-a-object-point1"), 3.0, 2.0, 3.0),
                point_xml(&shared("point-a-object"), 3.0, 2.0, 3.0),
            ]),
        ])], &format!("{object}{clone}{binding}"));
        assert_eq!(saved_points(&container, &initial, &added).unwrap(), expected);
    }

    #[test]
    fn point_add_and_delete_refuse_what_they_cannot_keep_consistent() {
        let mut fresh = || -> Result<String, String> { Ok("new-point".into()) };
        let one = test_container(&xml());
        let initial = authoring_scene::read(&one).unwrap();
        let on_mesh = json!({"sourceGuid": "mesh-guid", "index": 0});
        assert!(add_point(&initial, &on_mesh, &mut fresh).unwrap_err().contains("ArtPath"));
        assert!(delete_point(&initial, &on_mesh).unwrap_err().contains("ArtPath"));
        let path = |index: u64| json!({"sourceGuid": "path-guid", "index": index});
        assert!(add_point(&initial, &path(2), &mut fresh).unwrap_err().contains("outside"));
        assert!(delete_point(&initial, &path(1)).unwrap_err().contains("outside"));
        assert!(delete_point(&initial, &path(0)).unwrap_err().contains("last control point"));
        assert!(add_point(&initial, &json!({"sourceGuid": "path-guid"}), &mut fresh).unwrap_err().contains("index"));
        let negative = json!({"sourceGuid": "path-guid", "index": 0, "width": -1.0});
        assert!(add_point(&initial, &negative, &mut fresh).is_err());

        let uneven = test_container(&paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[point_xml(&named("point-a"), 1.0, 2.0, 3.0)]),
            form_xml("path", "form-2", &[
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
                point_xml(&named("point-b"), 5.0, 2.0, 3.0),
            ]),
        ])], ""));
        let uneven_initial = authoring_scene::read(&uneven).unwrap();
        assert!(add_point(&uneven_initial, &path(0), &mut fresh).unwrap_err().contains("different numbers of points"));
        assert!(delete_point(&uneven_initial, &path(0)).unwrap_err().contains("different numbers of points"));

        // A point the file names from elsewhere stays.
        let both = |extra: &str| paths_xml(&[("path", vec![
            form_xml("path", "form-1", &[
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
                point_xml(&named("point-b"), 5.0, 2.0, 3.0),
            ]),
        ])], extra);
        let named_elsewhere = test_container(&both("<Opaque xs.id='note' uuid='point-a'>keep</Opaque>"));
        let named_initial = authoring_scene::read(&named_elsewhere).unwrap();
        let deleted = delete_point(&named_initial, &path(0)).unwrap();
        let refused = saved_points(&named_elsewhere, &named_initial, &deleted).unwrap_err();
        assert!(refused.contains("named from elsewhere"), "{refused}");

        // A duplicated ArtPath keeps the GUIDs of its points: that is its own point, not a dangling name.
        let twins = paths_xml(&[
            ("path", vec![form_xml("path", "form-1", &[
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
                point_xml(&named("point-b"), 5.0, 2.0, 3.0),
            ])]),
            ("path2", vec![form_xml("path2", "form-2", &[
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
                point_xml(&named("point-b"), 5.0, 2.0, 3.0),
            ])]),
        ], "");
        let twin_container = test_container(&twins);
        let twin_initial = authoring_scene::read(&twin_container).unwrap();
        let deleted = delete_point(&twin_initial, &path(0)).unwrap();
        let expected = paths_xml(&[
            ("path", vec![form_xml("path", "form-1", &[point_xml(&named("point-b"), 5.0, 2.0, 3.0)])]),
            ("path2", vec![form_xml("path2", "form-2", &[
                point_xml(&named("point-a"), 1.0, 2.0, 3.0),
                point_xml(&named("point-b"), 5.0, 2.0, 3.0),
            ])]),
        ], "");
        assert_eq!(saved_points(&twin_container, &twin_initial, &deleted).unwrap(), expected);
    }
}
