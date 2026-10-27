use crate::animation_clone;
use crate::authoring_graph::Graph;
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(crate) fn set_key(model: &Value, request: &Value) -> Result<Value, String> {
    let value = finite(&request["value"])?;
    edit_point(model, request, |curve, index| {
        curve["points"][index]["anchor"]["value"] = json!(value);
        Ok(None)
    })
}

pub(crate) fn set_point(model: &Value, request: &Value) -> Result<Value, String> {
    edit_visual_point(model, request, VisualPointEdit::Set)
}

pub(crate) fn delete_point(model: &Value, request: &Value) -> Result<Value, String> {
    edit_visual_point(model, request, VisualPointEdit::Delete)
}

pub(crate) fn add_point(model: &Value, request: &Value) -> Result<Value, String> {
    edit_visual_point(model, request, VisualPointEdit::Add)
}

enum VisualPointEdit { Set, Add, Delete }

fn edit_visual_point(model: &Value, request: &Value, edit: VisualPointEdit) -> Result<Value, String> {
    let allowed = ["op", "expectedRevision", "sceneGuid", "trackGuid", "curveId", "anchorFrame", "x", "y", "corner"];
    let allowed = &allowed[..match edit { VisualPointEdit::Delete => 6, VisualPointEdit::Set => 8, VisualPointEdit::Add => 9 }];
    if request.as_object().into_iter().flat_map(|object| object.keys()).any(|key| !allowed.contains(&key.as_str())) {
        return Err("Image point edit carries an unknown field".into());
    }
    if model["format"] != "live2d.animation.source.v1" { return Err("point editing requires a CAN3 source model".into()); }
    let coordinates = if matches!(edit, VisualPointEdit::Delete) { None } else { Some([point_coordinate(&request["x"])?, point_coordinate(&request["y"])?]) };
    let frame = request["anchorFrame"].as_i64().ok_or("anchorFrame must be a signed integer")?;
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let matches: Vec<_> = TRACK_FIELDS.into_iter().flat_map(|field| scene[field].as_array().into_iter().flatten()
        .filter(|track| track["guid"] == request["trackGuid"]).map(move |_| field)).collect();
    if matches.len() != 1 || !matches!(matches[0], "imageTracks" | "guideTracks") {
        return Err("Image point trackGuid is missing or ambiguous".into());
    }
    let track = select_json(&mut scene[matches[0]], "guid", request, "trackGuid")?;
    let channel = select_json(&mut track["pointChannels"], "id", request, "curveId")?;
    if !VISUAL_POINT_CHANNELS.iter().any(|(id, _)| Some(*id) == request["curveId"].as_str()) {
        return Err("unsupported visual point channel".into());
    }
    let points = channel["points"].as_array_mut().ok_or("Image point channel lacks points")?;
    if matches!(edit, VisualPointEdit::Add) {
        let frame = i32::try_from(frame).map_err(|_| "visual point frame must fit signed i32")?;
        let corner = request["corner"].as_bool().ok_or("corner must be a boolean")?;
        let frames = points.iter().map(|p| p["frame"].as_i64().ok_or("visual point frame missing")).collect::<Result<Vec<_>, _>>()?;
        if frames.windows(2).any(|p| p[0] >= p[1]) { return Err("point insertion requires increasing existing frames".into()); }
        if frames.contains(&i64::from(frame)) { return Err("visual point frame already exists".into()); }
        let at = frames.partition_point(|old| *old < i64::from(frame));
        let [x, y] = coordinates.unwrap();
        points.insert(at, json!({"frame":frame,"x":x,"y":y,"corner":corner}));
        return Ok(candidate);
    }
    let indices: Vec<_> = points.iter().enumerate().filter(|(_, point)| point["frame"].as_i64() == Some(frame))
        .map(|(index, _)| index).collect();
    if indices.len() != 1 { return Err("anchorFrame must identify one Image point".into()); }
    if let Some(coordinates) = coordinates {
        for (axis, value) in ["x", "y"].into_iter().zip(coordinates) {
            let old = finite(&points[indices[0]][axis])?;
            if old as f32 != value as f32 { points[indices[0]][axis] = json!(value); }
        }
    } else {
        points.remove(indices[0]);
    }
    Ok(candidate)
}

fn point_coordinate(value: &Value) -> Result<f64, String> {
    let value = finite(value)? as f32;
    if !value.is_finite() { return Err("Image point coordinate exceeds finite f32 storage".into()); }
    value.to_string().parse().map_err(|_| "invalid Image point coordinate".into())
}

/// Moves one Bezier handle of an editable key: `side` is `previous` or `next`, and `frame` and `value` are its
/// position. An originally ordered smooth or Bezier segment must keep its handles in frame order.
/// An omitted or `null` field keeps its value.
pub(crate) fn set_handle(model: &Value, request: &Value) -> Result<Value, String> {
    let side = request["side"]
        .as_str()
        .filter(|side| matches!(*side, "previous" | "next"))
        .ok_or("side must be previous or next")?;
    let changes = ["frame", "value"]
        .into_iter()
        .filter_map(|key| request.get(key).filter(|value| !value.is_null()).map(|value| (key, value)))
        .map(|(key, value)| finite(value).map(|value| (key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    if changes.is_empty() {
        return Err("live2d.animation.handle.set requires frame or value".into());
    }
    edit_point(model, request, |curve, index| {
        for (key, value) in &changes {
            curve["points"][index][side][*key] = json!(value);
        }
        Ok(if side == "next" { Some(index) } else { index.checked_sub(1) })
    })
}

/// The interpolation types a CAN3 segment may take; Soul2D plays SMOOTH and BEZIER the same way.
pub(crate) const CURVE_TYPES: [&str; 4] = ["SMOOTH", "BEZIER", "LINEAR", "STEP"];

const ONLY_EDITS: &str = "animation edit may change only supported parameter or modern part-opacity curves, legacy or underscore part-opacity keys, tracks and scene settings, add or delete scenes, and add linked-model resources";
const TRACK_FIELDS: [&str; 4] = ["tracks", "imageTracks", "soundTracks", "guideTracks"];

/// Sets the interpolation of the segment that starts at an editable key.
pub(crate) fn set_curve_type(model: &Value, request: &Value) -> Result<Value, String> {
    let kind = request["type"]
        .as_str()
        .filter(|kind| CURVE_TYPES.contains(kind))
        .ok_or("type must be SMOOTH, BEZIER, LINEAR or STEP")?;
    edit_point(model, request, |curve, index| {
        let types = curve["curveTypes"].as_array_mut().ok_or("animation curve lacks curve types")?;
        *types.get_mut(index).ok_or("animation point and curve-type counts differ")? = json!(kind);
        Ok(Some(index))
    })
}

pub(crate) fn add_key(model: &Value, request: &Value) -> Result<Value, String> {
    key_structure(model, request, KeyEdit::Add)
}

pub(crate) fn delete_key(model: &Value, request: &Value) -> Result<Value, String> {
    key_structure(model, request, KeyEdit::Delete)
}

enum KeyEdit {
    Add,
    Delete,
}

fn new_key_point(frame: i64, value: f64) -> Value {
    json!({
        "anchor": {"frame": frame, "value": value},
        "previous": {"frame": frame as f64, "integerFrame": frame, "value": value, "positionOptimized": false},
        "next": {"frame": frame as f64, "integerFrame": frame, "value": value, "positionOptimized": false},
        "corner": true,
    })
}

fn key_structure(model: &Value, request: &Value, edit: KeyEdit) -> Result<Value, String> {
    let name = match edit {
        KeyEdit::Add => "live2d.animation.key.add",
        KeyEdit::Delete => "live2d.animation.key.delete",
    };
    let allowed: &[&str] = match edit {
        KeyEdit::Add => &[
            "op", "expectedRevision", "sceneGuid", "trackGuid", "curveId", "partOpacityMapKey", "anchorFrame", "value", "type",
        ],
        KeyEdit::Delete => &["op", "expectedRevision", "sceneGuid", "trackGuid", "curveId", "partOpacityMapKey", "anchorFrame"],
    };
    if let Some(object) = request.as_object() {
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("{name} carries an unknown key {key}"));
            }
        }
    }
    if model["format"] != "live2d.animation.source.v1" {
        return Err("key editing requires a CAN3 source model".into());
    }
    let frame = request["anchorFrame"]
        .as_i64()
        .ok_or("anchorFrame must be a signed integer")?;
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let curve = track_key_curve(scene, request)?;
    let before = curve.clone();
    {
        let points = before["points"].as_array().ok_or("animation curve lacks points")?;
        let types = before["curveTypes"]
            .as_array()
            .ok_or("animation curve lacks curve types")?;
        if types.len() != points.len() {
            return Err("animation point and curve-type counts differ".into());
        }
        if !types.iter().all(|kind| kind.as_str().map_or(false, |kind| CURVE_TYPES.contains(&kind)))
        {
            return Err("type must be SMOOTH, BEZIER, LINEAR or STEP".into());
        }
        let frames: Vec<i64> = points
            .iter()
            .map(|point| {
                point["anchor"]["frame"].as_i64().ok_or("anchorFrame must be a signed integer")
            })
            .collect::<Result<_, _>>()?;
        if frames.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("animation anchor frames must be strictly increasing".into());
        }
    }
    match edit {
        KeyEdit::Add => {
            if !(i32::MIN as i64..=i32::MAX as i64).contains(&frame) {
                return Err("anchorFrame must fit a signed 32-bit integer".into());
            }
            let value = finite(&request["value"])?;
            let kind = match request.get("type") {
                None => "LINEAR",
                Some(kind) => kind
                    .as_str()
                    .filter(|kind| CURVE_TYPES.contains(kind))
                    .ok_or("type must be SMOOTH, BEZIER, LINEAR or STEP")?,
            };
            let points = before["points"].as_array().unwrap();
            let count = points.len();
            if points.iter().any(|point| point["anchor"]["frame"].as_i64() == Some(frame)) {
                return Err("animation anchor frame already exists".into());
            }
            let at = points
                .iter()
                .position(|point| {
                    point["anchor"]["frame"].as_i64().map(|f| f > frame).unwrap_or(true)
                })
                .unwrap_or(count);
            if at > 0 && at < count {
                check_segment(&before, at - 1)?;
            }
            curve["points"]
                .as_array_mut()
                .ok_or("animation curve lacks points")?
                .insert(at, new_key_point(frame, value));
            curve["curveTypes"]
                .as_array_mut()
                .ok_or("animation curve lacks curve types")?
                .insert(at, json!(kind));
            if at > 0 {
                check_segment(curve, at - 1)?;
            }
            if at < count {
                check_segment(curve, at)?;
            }
        }
        KeyEdit::Delete => {
            let points = before["points"].as_array().unwrap();
            let count = points.len();
            let index = points
                .iter()
                .position(|point| point["anchor"]["frame"].as_i64() == Some(frame))
                .ok_or("animation anchor frame was not found")?;
            if index > 0 {
                check_segment(&before, index - 1)?;
            }
            if index + 1 < count {
                check_segment(&before, index)?;
            }
            curve["points"]
                .as_array_mut()
                .ok_or("animation curve lacks points")?
                .remove(index);
            curve["curveTypes"]
                .as_array_mut()
                .ok_or("animation curve lacks curve types")?
                .remove(index);
            if index > 0 && index + 1 < count {
                check_segment(curve, index - 1)?;
            }
        }
    }
    Ok(candidate)
}

struct CurveParameter {
    guid: String,
    id: String,
    range: [f64; 2],
    default: f64,
}

pub(crate) fn strict_keys(request: &Value, name: &str, allowed: &[&str]) -> Result<(), String> {
    if let Some(object) = request.as_object() {
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("{name} carries an unknown key {key}"));
            }
        }
    }
    Ok(())
}

pub(crate) fn add_curve(
    model: &Value,
    initial: &Value,
    request: &Value,
    source_model: &Value,
) -> Result<Value, String> {
    strict_keys(
        request,
        "live2d.animation.curve.add",
        &["op", "expectedRevision", "sceneGuid", "trackGuid", "modelPath", "parameterId", "partId"],
    )?;
    if model["format"] != "live2d.animation.source.v1" {
        return Err("curve editing requires a CAN3 source model".into());
    }
    match (request.get("parameterId"), request.get("partId")) {
        (None, Some(_)) => return add_part_curve(model, initial, request, source_model),
        (Some(_), None) => {}
        _ => return Err("give exactly one of parameterId or partId".into()),
    }
    let parameter_id = request["parameterId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("parameterId must be a nonempty string")?;
    let parameters = source_model["parameters"]
        .as_array()
        .ok_or("source model lacks parameters")?;
    let mut found = parameters.iter().filter(|p| p["id"].as_str() == Some(parameter_id));
    let parameter = found
        .next()
        .ok_or("source parameter was not found")?;
    if found.next().is_some() {
        return Err("source parameter is ambiguous".into());
    }
    let parameter = CurveParameter {
        guid: parameter["guid"]
            .as_str()
            .filter(|guid| !guid.is_empty())
            .ok_or("source parameter GUID missing")?
            .to_owned(),
        id: parameter_id.to_owned(),
        range: [
            finite(&parameter["minimum"])?,
            finite(&parameter["maximum"])?,
        ],
        default: finite(&parameter["default"])?,
    };
    if parameter.range[0] > parameter.range[1] {
        return Err("source parameter range is unordered".into());
    }
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let track = select_json(&mut scene["tracks"], "guid", request, "trackGuid")?;
    let curves = track["parameterCurves"]
        .as_array_mut()
        .ok_or("animation track lacks parameter curves")?;
    let id = format!("live2dParam_{parameter_id}");
    if curves.iter().any(|curve| curve["id"].as_str() == Some(id.as_str())) {
        return Err("animation parameter curve already exists".into());
    }
    let mut curve = json!({
        "id": id,
        "parameterId": parameter.id,
        "parameterGuid": parameter.guid,
        "sourceParameter": {"type": "CParameterId", "value": parameter.id},
        "range": parameter.range,
        "points": [],
        "curveTypes": [],
    });
    // A curve the track stored before keeps the baseValue it stored (or none), as the save compares kept curves
    // whole; a new curve takes the parameter's default (3,586 of 3,586 stored baseValues equal it in the supplied files).
    match initial_track(initial, request)
        .and_then(|old| old["parameterCurves"].as_array())
        .and_then(|old| old.iter().find(|old| old["id"] == id))
    {
        Some(old) => {
            if let Some(base) = old.get("baseValue") {
                curve["baseValue"] = base.clone();
            }
        }
        None => curve["baseValue"] = json!(parameter.default),
    }
    curves.push(curve);
    Ok(candidate)
}

pub(crate) fn delete_curve(model: &Value, request: &Value) -> Result<Value, String> {
    strict_keys(
        request,
        "live2d.animation.curve.delete",
        &["op", "expectedRevision", "sceneGuid", "trackGuid", "curveId", "partOpacityMapKey"],
    )?;
    if model["format"] != "live2d.animation.source.v1" {
        return Err("curve editing requires a CAN3 source model".into());
    }
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let track = select_json(&mut scene["tracks"], "guid", request, "trackGuid")?;
    let (field, index) = key_curve_slot(track, request)?;
    track[field].as_array_mut().unwrap().remove(index);
    track["skippedLegacyPartOpacityCount"] = json!(track["legacyPartOpacityCurves"].as_array().unwrap().len()
        + track["underscorePartOpacityCurves"].as_array().unwrap().len());
    Ok(candidate)
}

/// The track the request names as the opened file stored it, if it did.
fn initial_track<'a>(initial: &'a Value, request: &Value) -> Option<&'a Value> {
    initial["scenes"].as_array().into_iter().flatten()
        .find(|s| s["guid"] == request["sceneGuid"])
        .and_then(|s| s["tracks"].as_array())
        .and_then(|tracks| tracks.iter().find(|t| t["guid"] == request["trackGuid"]))
}

fn add_part_curve(model: &Value, initial: &Value, request: &Value, source: &Value) -> Result<Value, String> {
    let part_id = request["partId"].as_str().filter(|id| !id.is_empty() && !id.contains(':'))
        .ok_or("partId must be a nonempty string without a colon")?;
    let parts = model_parts(source)?;
    let guid = parts.guids_by_id.get(part_id).ok_or("source Part was not found")?;
    let id = format!("live2DPartsOpacity_{part_id}");
    let mut curve = json!({"id":id,"partId":part_id,"partGuid":guid,
        "sourcePart":{"type":"CPartId","value":part_id},"range":[0.0,1.0],"points":[],"curveTypes":[]});
    if let Some(old) = initial_track(initial, request).and_then(|t| t["partOpacityCurves"].as_array())
        .and_then(|curves| curves.iter().find(|c| c["id"] == id))
    {
        if old["partGuid"].as_str() != Some(*guid) || old["partId"] != part_id {
            return Err("re-added Part curve identity differs from its original source".into());
        }
        curve = old.clone();
        curve["points"] = json!([]);
        curve["curveTypes"] = json!([]);
    }
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let track = select_json(&mut scene["tracks"], "guid", request, "trackGuid")?;
    for field in ["partOpacityCurves", "legacyPartOpacityCurves", "underscorePartOpacityCurves"] {
        if track[field].as_array().into_iter().flatten()
            .any(|c| c["id"] == id || c["partGuid"].as_str() == Some(*guid))
        {
            return Err("animation Part opacity curve already exists".into());
        }
    }
    track["partOpacityCurves"].as_array_mut().ok_or("animation track lacks part curves")?.push(curve);
    Ok(candidate)
}

struct ModelParts<'a> {
    ids_by_guid: HashMap<&'a str, &'a str>,
    guids_by_id: HashMap<&'a str, &'a str>,
}

fn model_parts(model: &Value) -> Result<ModelParts<'_>, String> {
    let mut parts = ModelParts { ids_by_guid: HashMap::new(), guids_by_id: HashMap::new() };
    for source in model["sources"].as_array().ok_or("CMO3 source model lacks sources")?
        .iter().filter(|source| source["data"]["type"] == "Part")
    {
        let guid = source["guid"].as_str().filter(|v| !v.is_empty()).ok_or("CMO3 Part source lacks a GUID")?;
        let id = source["id"].as_str().filter(|v| !v.is_empty()).ok_or("CMO3 Part source lacks an ID")?;
        if parts.ids_by_guid.insert(guid, id).is_some() { return Err("CMO3 Part GUIDs are ambiguous".into()); }
        if parts.guids_by_id.insert(id, guid).is_some() { return Err("CMO3 Part IDs are ambiguous".into()); }
    }
    Ok(parts)
}

fn scalar_curve_location(scene: &Value, request: &Value) -> Result<(&'static str, usize, &'static str, usize), String> {
    let guid = request["trackGuid"].as_str().filter(|id| !id.is_empty()).ok_or("trackGuid must be a nonempty string")?;
    let families: Vec<_> = TRACK_FIELDS.into_iter().flat_map(|field| {
        scene[field].as_array().into_iter().flatten()
            .enumerate().filter(|(_, track)| track["guid"] == guid).map(move |(index, _)| (field, index))
    }).collect();
    if families.is_empty() { return Err("animation trackGuid was not found".into()); }
    if families.len() != 1 { return Err("animation trackGuid is ambiguous".into()); }
    let (family, track_index) = families[0];
    if !matches!(family, "tracks" | "imageTracks" | "guideTracks") {
        return Err("animation operation does not support this track family".into());
    }
    let track = &scene[family][track_index];
    let (field, index) = if family != "tracks" {
        if request.get("partOpacityMapKey").is_some() { return Err("visual keys do not take partOpacityMapKey".into()); }
        let id = request["curveId"].as_str().filter(|id| !id.is_empty()).ok_or("curveId must be a nonempty string")?;
        let curves = track["visualCurves"].as_array().ok_or("visual track has no readable scalar curves")?;
        let found: Vec<_> = curves.iter().enumerate().filter(|(_, curve)| curve["id"] == id).map(|(i, _)| i).collect();
        if found.len() != 1 { return Err("curveId must identify one readable visual scalar curve".into()); }
        ("visualCurves", found[0])
    } else { key_curve_slot(track, request)? };
    Ok((family, track_index, field, index))
}

pub(crate) fn scalar_curve<'a>(scene: &'a Value, request: &Value) -> Result<&'a Value, String> {
    let (family, track, field, curve) = scalar_curve_location(scene, request)?;
    Ok(&scene[family][track][field][curve])
}

fn track_key_curve<'a>(scene: &'a mut Value, request: &Value) -> Result<&'a mut Value, String> {
    let (family, track, field, curve) = scalar_curve_location(scene, request)?;
    Ok(&mut scene[family][track][field][curve])
}

fn edit_point(
    model: &Value,
    request: &Value,
    change: impl FnOnce(&mut Value, usize) -> Result<Option<usize>, String>,
) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("key editing requires a CAN3 source model".into());
    }
    let frame = request["anchorFrame"]
        .as_i64()
        .ok_or("anchorFrame must be a signed integer")?;
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let curve = track_key_curve(scene, request)?;
    let index = curve["points"]
        .as_array()
        .ok_or("animation curve lacks points")?
        .iter()
        .position(|p| p["anchor"]["frame"].as_i64() == Some(frame))
        .ok_or("animation anchor frame was not found")?;
    let before = curve.clone();
    if let Some(segment) = change(curve, index)? {
        if check_segment(&before, segment).is_ok() {
            check_segment(curve, segment)?;
        }
    }
    Ok(candidate)
}

/// Refuses a smooth or Bezier segment whose handles are out of frame order, as playback would.
fn check_segment(curve: &Value, segment: usize) -> Result<(), String> {
    let (left, right) = (&curve["points"][segment], &curve["points"][segment + 1]);
    let kind = curve["curveTypes"][segment].as_str().unwrap_or("");
    if right.is_null() || !matches!(kind, "SMOOTH" | "BEZIER" | "Smooth" | "Bezier") {
        return Ok(());
    }
    let frames = [&left["anchor"], &left["next"], &right["previous"], &right["anchor"]]
        .map(|point| point["frame"].as_f64().unwrap_or(f64::NAN));
    if !frames.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Err(format!(
            "the {kind} segment from frame {} would have its handles out of frame order",
            frames[0]
        ));
    }
    Ok(())
}

/// Changes the name, frame rate, length, loop setting or stored tag of one scene.
pub(crate) fn set_scene(model: &Value, request: &Value) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("scene editing requires a CAN3 source model".into());
    }
    let mut candidate = model.clone();
    let scene = select_json(&mut candidate["scenes"], "guid", request, "sceneGuid")?;
    let mut changed = false;
    for key in ["name", "fps", "durationFrames", "loop", "tag"] {
        if let Some(value) = request.get(key) {
            if key == "loop" && scene.get("loop").is_none() {
                return Err("this scene stores no loop setting".into());
            }
            if key == "tag" && scene.get("tag").is_none() {
                return Err("this scene stores no tag".into());
            }
            scene[key] = scene_setting(key, value)?;
            changed = true;
        }
    }
    if !changed {
        return Err("live2d.animation.scene.set requires name, fps, durationFrames, loop or tag".into());
    }
    Ok(candidate)
}

/// Appends an empty scene; `guid` is the new scene's identity.
pub(crate) fn add_scene(model: &Value, request: &Value, guid: &str) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("scene editing requires a CAN3 source model".into());
    }
    let mut candidate = model.clone();
    let scenes = candidate["scenes"].as_array_mut().ok_or("animation source lacks scenes")?;
    if scenes.iter().any(|scene| scene["guid"] == guid) {
        return Err("animation sceneGuid is already used".into());
    }
    let mut scene = json!({"guid": guid, "tracks": [], "imageTracks": [], "soundTracks": [], "guideTracks": []});
    for key in ["name", "fps", "durationFrames"] {
        let value = request.get(key).ok_or_else(|| format!("{key} is required"))?;
        scene[key] = scene_setting(key, value)?;
    }
    // A new scene stores a loop setting, off as in every scene of the supplied files, when the others do.
    if scenes.iter().any(|scene| scene.get("loop").is_some()) {
        scene["loop"] = json!(false);
    }
    // The save copies a stored scene, tag included; showing the tag lets scene.set clear it. No measured rule
    // resets it, so the document's last scene decides.
    if let Some(tag) = scenes.last().and_then(|last| last.get("tag")) {
        scene["tag"] = tag.clone();
    }
    scenes.push(scene);
    Ok(candidate)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceKind { Model, Image }

impl ResourceKind {
    pub(crate) fn field(self) -> &'static str {
        match self { Self::Model => "resources", Self::Image => "imageResources" }
    }
}

/// Appends a linked resource; the caller validates the explicit input file.
pub(crate) fn add_resource(model: &Value, kind: ResourceKind, path: &str, guid: &str) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("resource editing requires a CAN3 source model".into());
    }
    if guid.is_empty() || guid.trim() != guid {
        return Err("resource GUID is invalid".into());
    }
    let name = std::path::Path::new(path).file_name().and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
        .ok_or(match kind { ResourceKind::Model => "modelPath must name a file", ResourceKind::Image => "imagePath must name a file" })?;
    if ["resources", "imageResources"].into_iter().any(|field| model[field].as_array().into_iter().flatten()
        .any(|resource| resource["guid"] == guid)) {
        return Err("animation resourceGuid is already used".into());
    }
    let mut candidate = model.clone();
    let resources = candidate[kind.field()].as_array_mut().ok_or("animation source lacks resources")?;
    resources.push(json!({"guid": guid, "name": name, "sourceFile": path}));
    Ok(candidate)
}

pub(crate) fn relink_image_resource(model: &Value, guid: &str, path: &str) -> Result<Value, String> {
    let mut candidate = model.clone();
    let resources = candidate["imageResources"].as_array_mut().ok_or("animation source lacks Image resources")?;
    let matches: Vec<_> = resources.iter().enumerate().filter(|(_, resource)| resource["guid"] == guid)
        .map(|(index, _)| index).collect();
    if guid.is_empty() || matches.len() != 1 { return Err("resourceGuid must identify one Image resource".into()); }
    let resource = &mut resources[matches[0]];
    if resource["sourceFile"].as_str().is_none() { return Err("Image resource has no stored sourceFile".into()); }
    resource["sourceFile"] = json!(path);
    Ok(candidate)
}

#[derive(Clone, Debug)]
pub(crate) struct AnimationTrackCopy {
    pub family: CopyFamily,
    pub scene_guid: String,
    pub template_track_guid: String,
    pub track: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyFamily { Image, Guide, Sound }

impl CopyFamily {
    fn field(self) -> &'static str {
        match self { Self::Image => "imageTracks", Self::Guide => "guideTracks", Self::Sound => "soundTracks" }
    }

    fn native_tag(self) -> &'static str {
        match self { Self::Image => "CMvTrack_Image_Source", Self::Guide => "CMvTrack_GuideImage_Source",
            Self::Sound => "CMvTrack_JavaSoundTrack_Source" }
    }
}

pub(crate) fn duplicate_track(
    model: &Value, request: &Value, copies: &[AnimationTrackCopy], guid: &str,
) -> Result<(Value, AnimationTrackCopy), String> {
    strict_keys(request, "live2d.animation.track.duplicate", &["op", "sceneGuid", "trackGuid", "name", "expectedRevision"])?;
    let scene_guid = request["sceneGuid"].as_str().ok_or("sceneGuid is required")?;
    let donor_guid = request["trackGuid"].as_str().ok_or("trackGuid is required")?;
    let name = scene_setting("name", request.get("name").ok_or("name is required")?)?;
    let mut candidate = model.clone();
    let scenes = candidate["scenes"].as_array_mut().ok_or("animation scenes missing")?;
    if guid.is_empty() || scenes.iter().flat_map(|scene| TRACK_FIELDS.into_iter()
        .flat_map(move |field| scene[field].as_array().into_iter().flatten()))
        .any(|track| track["guid"] == guid) || copies.iter().any(|copy| copy.track["guid"] == guid) {
        return Err("animation track GUID is invalid or already used".into());
    }
    let scene = scenes.iter_mut().find(|scene| scene["guid"] == scene_guid)
        .ok_or("animation sceneGuid was not found")?;
    let families: Vec<_> = [CopyFamily::Image, CopyFamily::Guide, CopyFamily::Sound].into_iter().filter(|family|
        scene[family.field()].as_array().is_some_and(|tracks| tracks.iter().any(|track| track["guid"] == donor_guid))).collect();
    if families.len() != 1 { return Err("trackGuid must name one existing Image, Guide or Sound track".into()); }
    let family = families[0];
    let images = scene[family.field()].as_array_mut().ok_or("animation tracks missing")?;
    let mut track = images.iter().find(|track| track["guid"] == donor_guid)
        .ok_or("trackGuid must name an existing linked track")?.clone();
    track["guid"] = json!(guid);
    track["name"] = name;
    let template_track_guid = copies.iter().find(|copy| copy.family == family && copy.scene_guid == scene_guid && copy.track["guid"] == donor_guid)
        .map_or(donor_guid, |copy| copy.template_track_guid.as_str()).to_owned();
    images.push(track.clone());
    Ok((candidate, AnimationTrackCopy { family, scene_guid: scene_guid.to_owned(), template_track_guid, track }))
}

pub(crate) fn add_track(model: &Value, request: &Value, guid: &str) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("track editing requires a CAN3 source model".into());
    }
    strict_keys(request, "live2d.animation.track.add", &["op", "sceneGuid", "resourceGuid", "name", "expectedRevision"])?;
    let scene_guid = request["sceneGuid"].as_str().ok_or("sceneGuid is required")?;
    let resource = request["resourceGuid"].as_str().ok_or("resourceGuid is required")?;
    if !model["resources"]
        .as_array()
        .is_some_and(|all| all.iter().any(|entry| entry["guid"] == resource))
    {
        return Err("animation resourceGuid was not found".into());
    }
    if guid.is_empty() || guid.trim() != guid {
        return Err("track GUID is invalid".into());
    }
    let name = scene_setting("name", request.get("name").ok_or("name is required")?)?;
    let mut candidate = model.clone();
    let scenes = candidate["scenes"].as_array_mut().ok_or("animation source lacks scenes")?;
    if scenes
        .iter()
        .flat_map(|scene| TRACK_FIELDS.into_iter().filter_map(|field| scene[field].as_array()))
        .flatten()
        .any(|track| track["guid"] == guid)
    {
        return Err("animation trackGuid is already used".into());
    }
    let scene = scenes
        .iter_mut()
        .find(|scene| scene["guid"] == scene_guid)
        .ok_or("animation sceneGuid was not found")?;
    let duration = scene["durationFrames"]
        .as_i64()
        .filter(|duration| i32::try_from(*duration).is_ok())
        .ok_or("animation track duration is out of range")?;
    scene["tracks"]
        .as_array_mut()
        .ok_or("animation tracks missing")?
        .push(json!({
            "guid": guid, "name": name, "resourceGuid": resource,
            "start": 0, "internalOffset": 0, "duration": duration,
            "parameterCurves": [], "partOpacityCurves": [],
            "legacyPartOpacityCurves": [], "underscorePartOpacityCurves": [], "skippedLegacyPartOpacityCount": 0,
        }));
    Ok(candidate)
}

pub(crate) fn delete_scene(model: &Value, request: &Value) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("scene editing requires a CAN3 source model".into());
    }
    strict_keys(request, "live2d.animation.scene.delete", &["op", "sceneGuid", "expectedRevision"])?;
    let guid = request["sceneGuid"]
        .as_str()
        .filter(|guid| !guid.is_empty())
        .ok_or("sceneGuid must be a nonempty string")?;
    let mut candidate = model.clone();
    let scenes = candidate["scenes"].as_array_mut().ok_or("animation source lacks scenes")?;
    let mut matches = scenes
        .iter()
        .enumerate()
        .filter(|(_, scene)| scene["guid"] == guid);
    let index = matches.next().map(|(index, _)| index)
        .ok_or("animation sceneGuid was not found")?;
    if matches.next().is_some() {
        return Err("animation sceneGuid is ambiguous".into());
    }
    scenes.remove(index);
    Ok(candidate)
}

pub(crate) fn delete_track(model: &Value, request: &Value) -> Result<Value, String> {
    if model["format"] != "live2d.animation.source.v1" {
        return Err("track editing requires a CAN3 source model".into());
    }
    strict_keys(request, "live2d.animation.track.delete", &["op", "sceneGuid", "trackGuid", "expectedRevision"])?;
    let mut candidate = model.clone();
    let scene = candidate["scenes"]
        .as_array_mut()
        .ok_or("animation source lacks scenes")?
        .iter_mut()
        .find(|scene| scene["guid"] == request["sceneGuid"])
        .ok_or("animation sceneGuid was not found")?;
    let mut matches = Vec::new();
    for field in TRACK_FIELDS {
        let tracks = scene[field].as_array().ok_or("animation tracks missing")?;
        matches.extend(tracks.iter().enumerate()
            .filter(|(_, track)| track["guid"] == request["trackGuid"])
            .map(|(index, _)| (field, index)));
    }
    let (field, index) = match matches.as_slice() {
        [target] => *target,
        [] => return Err("animation trackGuid was not found".into()),
        _ => return Err("animation trackGuid is ambiguous".into()),
    };
    scene[field].as_array_mut().unwrap().remove(index);
    Ok(candidate)
}

/// A random version 4 UUID in its usual text form.
pub(crate) fn new_guid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| format!("cannot read random bytes: {error}"))?;
    Ok(guid_text(bytes))
}

fn guid_text(mut bytes: [u8; 16]) -> String {
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// Checks one scene setting and returns it as `read` projects it. The bounds are those of the
/// stored types: a 32-bit float frame rate and a 32-bit integer length.
fn scene_setting(key: &str, value: &Value) -> Result<Value, String> {
    match key {
        "name" => value
            .as_str()
            .filter(|name| !name.chars().any(char::is_control))
            .map(|name| json!(name))
            .ok_or_else(|| "name must be a string without control characters".into()),
        "loop" => value.as_bool().map(Value::Bool).ok_or_else(|| "loop must be a boolean".into()),
        "tag" => value
            .as_str()
            .filter(|tag| !tag.chars().any(char::is_control))
            .map(|tag| json!(tag))
            .ok_or_else(|| "tag must be a string without control characters".into()),
        "fps" => value
            .as_f64()
            .filter(|fps| fps.is_finite() && (*fps as f32) > 0.0 && (*fps as f32).is_finite())
            .map(|fps| json!(fps))
            .ok_or_else(|| "fps must be a finite number above 0".into()),
        _ => value
            .as_i64()
            .filter(|frames| (0..=i32::MAX as i64).contains(frames))
            .map(|frames| json!(frames))
            .ok_or_else(|| "durationFrames must be an integer from 0 to 2147483647".into()),
    }
}

fn key_curve_slot(track: &Value, request: &Value) -> Result<(&'static str, usize), String> {
    let map_key = request.get("partOpacityMapKey")
        .map(|value| value.as_str().ok_or("partOpacityMapKey must be a nonempty string"))
        .transpose()?;
    editable_curve_slot(track, request["curveId"].as_str().unwrap_or(""), map_key)
}

pub(crate) fn editable_curve_slot(track: &Value, id: &str, map_key: Option<&str>) -> Result<(&'static str, usize), String> {
    if id.is_empty() {
        return Err("curveId must be a nonempty string".into());
    }
    if map_key == Some("") {
        return Err("partOpacityMapKey must be a nonempty string".into());
    }
    let mut found = None;
    for field in ["parameterCurves", "partOpacityCurves", "legacyPartOpacityCurves", "underscorePartOpacityCurves"] {
        for (index, curve) in track[field].as_array().into_iter().flatten().enumerate() {
            if curve["id"].as_str() == Some(id)
                && map_key.is_none_or(|key| field == "underscorePartOpacityCurves" && curve["mapKey"].as_str() == Some(key))
            {
                if found.replace((field, index)).is_some() {
                    return Err("animation curveId is ambiguous".into());
                }
            }
        }
    }
    found.ok_or_else(|| "animation curveId was not found".into())
}

fn select_json<'a>(
    array: &'a mut Value,
    field: &str,
    request: &Value,
    key: &str,
) -> Result<&'a mut Value, String> {
    let id = request[key]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("{key} must be a nonempty string"))?;
    let values = array
        .as_array_mut()
        .ok_or("animation source lacks an array")?;
    let mut matches = values.iter_mut().filter(|v| v[field].as_str() == Some(id));
    let result = matches
        .next()
        .ok_or_else(|| format!("animation {key} was not found"))?;
    if matches.next().is_some() {
        return Err(format!("animation {key} is ambiguous"));
    }
    Ok(result)
}

fn finite(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| "animation value must be finite".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingResult {
    format: &'static str,
    resource_guid: String,
    model_name: String,
    binding_basis: &'static str,
    bindings: Vec<Binding>,
    part_curve_count: usize,
    part_bindings: Vec<PartBinding>,
    legacy_part_bindings: Vec<PartBinding>,
    unbound_legacy_part_curves: Vec<UnboundLegacyPartCurve>,
    underscore_part_bindings: Vec<UnderscorePartBinding>,
    unbound_underscore_part_curves: Vec<UnboundUnderscorePartCurve>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    scene_guid: String,
    track_guid: String,
    curve_id: String,
    parameter_id: String,
    parameter_guid: String,
    source_parameter: Option<BindingSourceParameter>,
    source_parameter_status: SourceParameterStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PartBinding {
    scene_guid: String,
    track_guid: String,
    curve_id: String,
    part_id: String,
    part_guid: String,
    source_part: BindingSourceParameter,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnboundLegacyPartCurve {
    scene_guid: String,
    track_guid: String,
    curve_id: String,
    part_guid: String,
    reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnderscorePartBinding {
    #[serde(flatten)]
    binding: PartBinding,
    map_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnboundUnderscorePartCurve {
    #[serde(flatten)]
    curve: UnboundLegacyPartCurve,
    map_key: String,
    source_part: BindingSourceParameter,
}

#[derive(Serialize)]
struct BindingSourceParameter {
    #[serde(rename = "type")]
    kind: String,
    value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum SourceParameterStatus {
    MatchesId,
    DifferentId,
    LegacyText,
    Missing,
}

pub(crate) fn bindings(
    animation: &Value,
    model: &Value,
    resource_guid: &str,
) -> Result<Value, String> {
    if animation["format"].as_str() != Some("live2d.animation.source.v1") {
        return Err("binding query requires a CAN3 source projection".into());
    }
    if model["format"].as_str() != Some("live2d.authoring.source.v1") {
        return Err("binding query requires a CMO3 source model".into());
    }
    let mut parameters = HashMap::new();
    let mut parameter_ids = HashSet::new();
    for parameter in model["parameters"]
        .as_array()
        .ok_or("CMO3 source model lacks parameters")?
    {
        let guid = parameter["guid"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("CMO3 parameter lacks a GUID")?;
        let id = parameter["id"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("CMO3 parameter lacks an ID")?;
        if !parameter_ids.insert(id) {
            return Err("CMO3 parameter IDs are ambiguous".into());
        }
        if parameters.insert(guid, id).is_some() {
            return Err("CMO3 parameter GUIDs are ambiguous".into());
        }
    }
    let ModelParts { ids_by_guid: part_ids_by_guid, guids_by_id: part_guids_by_id } = model_parts(model)?;
    let resources = animation["resources"]
        .as_array()
        .ok_or("animation source lacks resources")?;
    let mut matching = resources
        .iter()
        .filter(|resource| resource["guid"].as_str() == Some(resource_guid));
    if matching.next().is_none() {
        return Err("animation model resource was not found".into());
    }
    if matching.next().is_some() {
        return Err("animation model resource is ambiguous".into());
    }
    let model_name = model["name"]
        .as_str()
        .ok_or("CMO3 source model lacks a name")?
        .to_owned();
    let mut bound = Vec::new();
    let mut part_bound = Vec::new();
    let mut legacy_part_bound = Vec::new();
    let mut unbound_legacy_part_curves = Vec::new();
    let mut underscore_part_bound = Vec::new();
    let mut unbound_underscore_part_curves = Vec::new();
    let mut part_curve_count = 0;
    for scene in animation["scenes"]
        .as_array()
        .ok_or("animation source lacks scenes")?
    {
        let scene_guid = scene["guid"]
            .as_str()
            .ok_or("animation scene lacks a GUID")?;
        for track in scene["tracks"]
            .as_array()
            .ok_or("animation scene lacks tracks")?
        {
            if track["resourceGuid"].as_str() != Some(resource_guid) {
                continue;
            }
            let track_guid = track["guid"]
                .as_str()
                .ok_or("animation track lacks a GUID")?;
            for curve in track["parameterCurves"]
                .as_array()
                .ok_or("animation track lacks parameter curves")?
            {
                let parameter_guid = curve["parameterGuid"]
                    .as_str()
                    .ok_or("animation parameter curve lacks a GUID")?;
                let parameter_id = parameters
                    .get(parameter_guid)
                    .ok_or("animation parameter GUID was not found in the CMO3 model")?;
                let curve_id = curve["id"]
                    .as_str()
                    .ok_or("animation parameter curve lacks an ID")?;
                if curve["parameterId"].as_str() != Some(*parameter_id) {
                    return Err(
                        "animation parameter identity differs from the CMO3 model".into()
                    );
                }
                let source = curve
                    .get("sourceParameter")
                    .ok_or("animation parameter curve lacks source parameter metadata")?;
                let (source_parameter, status) = if source.is_null() {
                    (None, SourceParameterStatus::Missing)
                } else {
                    let kind = source["type"]
                        .as_str()
                        .ok_or("animation source parameter lacks a type")?;
                    let value = source["value"]
                        .as_str()
                        .ok_or("animation source parameter lacks a value")?;
                    let status = match kind {
                        "CParameterId" if value == *parameter_id => {
                            SourceParameterStatus::MatchesId
                        }
                        "CParameterId" => SourceParameterStatus::DifferentId,
                        "s" => SourceParameterStatus::LegacyText,
                        _ => return Err("unsupported animation source parameter identity".into()),
                    };
                    (
                        Some(BindingSourceParameter {
                            kind: kind.to_owned(),
                            value: value.to_owned(),
                        }),
                        status,
                    )
                };
                bound.push(Binding {
                    scene_guid: scene_guid.to_owned(),
                    track_guid: track_guid.to_owned(),
                    curve_id: curve_id.to_owned(),
                    parameter_id: (*parameter_id).to_owned(),
                    parameter_guid: parameter_guid.to_owned(),
                    source_parameter,
                    source_parameter_status: status,
                });
            }
            let part_curves = match track.get("partOpacityCurves") {
                Some(Value::Array(curves)) => curves.as_slice(),
                Some(_) => return Err("animation track part opacity curves are invalid".into()),
                None => &[],
            };
            for curve in part_curves {
                let curve_id = curve["id"]
                    .as_str()
                    .ok_or("animation part opacity curve lacks an ID")?;
                let part_id = curve["partId"]
                    .as_str()
                    .ok_or("animation part opacity curve lacks a part ID")?;
                let part_guid = curve["partGuid"]
                    .as_str()
                    .ok_or("animation part opacity curve lacks a part GUID")?;
                if curve_id != format!("live2DPartsOpacity_{part_id}") {
                    return Err("animation part opacity attribute ID differs from its part ID".into());
                }
                let source = &curve["sourcePart"];
                let kind = source["type"]
                    .as_str()
                    .filter(|kind| matches!(*kind, "s" | "CPartId"))
                    .ok_or("animation source part lacks a supported type")?;
                let value = source["value"]
                    .as_str()
                    .ok_or("animation source part lacks a value")?;
                if value.trim() != part_id {
                    return Err("animation source part identity differs from its part ID".into());
                }
                let id_from_guid = part_ids_by_guid
                    .get(part_guid)
                    .copied()
                    .ok_or("animation part GUID was not found in the CMO3 model")?;
                let guid_from_id = part_guids_by_id
                    .get(part_id)
                    .copied()
                    .ok_or("animation part ID was not found in the CMO3 model")?;
                if id_from_guid != part_id || guid_from_id != part_guid {
                    return Err("animation part GUID and ID identify different CMO3 Parts".into());
                }
                part_bound.push(PartBinding {
                    scene_guid: scene_guid.to_owned(),
                    track_guid: track_guid.to_owned(),
                    curve_id: curve_id.to_owned(),
                    part_id: part_id.to_owned(),
                    part_guid: part_guid.to_owned(),
                    source_part: BindingSourceParameter {
                        kind: kind.to_owned(),
                        value: value.to_owned(),
                    },
                });
                part_curve_count += 1;
            }
            let legacy_part_curves = match track.get("legacyPartOpacityCurves") {
                Some(Value::Array(curves)) => curves.as_slice(),
                Some(_) => {
                    return Err("animation track legacy part opacity curves are invalid".into())
                }
                None => &[],
            };
            for curve in legacy_part_curves {
                let curve_id = curve["id"]
                    .as_str()
                    .ok_or("animation legacy part opacity curve lacks an ID")?;
                let _map_part_id = curve["legacyMapPartId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("animation legacy part opacity curve lacks a map part ID")?;
                let attribute_id = curve["legacyAttributeId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("animation legacy part opacity curve lacks an attribute ID")?;
                if curve_id != format!("live2DPartsOpacity:{attribute_id}") {
                    return Err(
                        "animation legacy part opacity attribute ID differs from its suffix".into(),
                    );
                }
                let part_guid = curve["partGuid"]
                    .as_str()
                    .filter(|guid| !guid.is_empty())
                    .ok_or("animation legacy part opacity curve lacks a Part GUID")?;
                let source = &curve["sourcePart"];
                let kind = source["type"]
                    .as_str()
                    .filter(|kind| matches!(*kind, "s" | "CPartId"))
                    .ok_or("animation legacy source part lacks a supported type")?;
                let value = source["value"]
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("animation legacy source part lacks a value")?;
                let part_id = part_ids_by_guid.get(part_guid).copied();
                if let Some(part_id) = part_id {
                    legacy_part_bound.push(PartBinding {
                        scene_guid: scene_guid.to_owned(),
                        track_guid: track_guid.to_owned(),
                        curve_id: curve_id.to_owned(),
                        part_id: part_id.to_owned(),
                        part_guid: part_guid.to_owned(),
                        source_part: BindingSourceParameter {
                            kind: kind.to_owned(),
                            value: value.to_owned(),
                        },
                    });
                } else {
                    unbound_legacy_part_curves.push(UnboundLegacyPartCurve {
                        scene_guid: scene_guid.to_owned(),
                        track_guid: track_guid.to_owned(),
                        curve_id: curve_id.to_owned(),
                        part_guid: part_guid.to_owned(),
                        reason: "partGuidNotFound",
                    });
                }
            }
            let underscore_curves = match track.get("underscorePartOpacityCurves") {
                Some(Value::Array(curves)) => curves.as_slice(),
                Some(_) => return Err("animation track underscore PartOpacity curves are invalid".into()),
                None => &[],
            };
            let mut map_keys = HashSet::new();
            for curve in underscore_curves {
                let curve_id = curve["id"].as_str().filter(|id| !id.is_empty())
                    .ok_or("animation underscore PartOpacity attribute ID is missing")?;
                let map_key = curve["mapKey"].as_str()
                    .filter(|key| key.strip_prefix("live2DPartsOpacity_").is_some_and(|suffix| !suffix.is_empty()))
                    .ok_or("animation underscore PartOpacity map key is invalid")?;
                if !map_keys.insert(map_key) {
                    return Err("animation underscore PartOpacity map key is ambiguous".into());
                }
                let part_guid = curve["partGuid"].as_str().filter(|guid| !guid.is_empty())
                    .ok_or("animation underscore PartOpacity Part GUID is missing")?;
                let source = &curve["sourcePart"];
                let kind = source["type"].as_str().filter(|kind| matches!(*kind, "s" | "CPartId"))
                    .ok_or("animation underscore source Part lacks a supported type")?;
                let value = source["value"].as_str().filter(|value| !value.trim().is_empty())
                    .ok_or("animation underscore source Part lacks a value")?;
                if let Some(part_id) = part_ids_by_guid.get(part_guid) {
                    underscore_part_bound.push(UnderscorePartBinding {
                        binding: PartBinding {
                            scene_guid: scene_guid.to_owned(), track_guid: track_guid.to_owned(),
                            curve_id: curve_id.to_owned(), part_id: (*part_id).to_owned(), part_guid: part_guid.to_owned(),
                            source_part: BindingSourceParameter { kind: kind.to_owned(), value: value.to_owned() },
                        },
                        map_key: map_key.to_owned(),
                    });
                } else {
                    unbound_underscore_part_curves.push(UnboundUnderscorePartCurve {
                        curve: UnboundLegacyPartCurve {
                            scene_guid: scene_guid.to_owned(), track_guid: track_guid.to_owned(),
                            curve_id: curve_id.to_owned(), part_guid: part_guid.to_owned(), reason: "partGuidNotFound",
                        },
                        map_key: map_key.to_owned(),
                        source_part: BindingSourceParameter { kind: kind.to_owned(), value: value.to_owned() },
                    });
                }
            }
        }
    }
    serde_json::to_value(BindingResult {
        format: "live2d.animation.bindings.v1",
        resource_guid: resource_guid.to_owned(),
        model_name,
        binding_basis: "parameterGuid+attributeId",
        bindings: bound,
        part_curve_count,
        part_bindings: part_bound,
        legacy_part_bindings: legacy_part_bound,
        unbound_legacy_part_curves,
        underscore_part_bindings: underscore_part_bound,
        unbound_underscore_part_curves,
    })
    .map_err(|error| error.to_string())
}

pub(crate) fn rewrite_with_copies(
    container: &CaffDocument, initial: &Value, model: &Value, copies: &[AnimationTrackCopy],
) -> Result<Vec<u8>, String> {
    let active: Vec<_> = copies.iter().filter(|copy| model["scenes"].as_array().into_iter().flatten()
        .any(|scene| TRACK_FIELDS.into_iter().flat_map(|field| scene[field].as_array().into_iter().flatten())
            .any(|track| track["guid"] == copy.track["guid"]))).collect();
    if active.is_empty() { return rewrite(container, initial, model); }
    if read(container)? != *initial { return Err("animation template differs from initial source".into()); }
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml).map_err(|_| "animation XML is not UTF-8")?;
    let document = xml_document(text)?;
    let graph = Graph::new(&document)?;
    let shared = document.root_element().children().find(|node| node.has_tag_name("shared"))
        .ok_or("animation XML lacks a shared section")?;
    let mut budget = Budget::new(5_000_000);
    let animation = graph.resolve(document.root_element().children().find(|node| node.has_tag_name("main"))
        .and_then(|node| node.children().find(Node::is_element)).ok_or("animation main is missing")?)?;
    let mut additions = NativeAdditions { reserved: document.descendants()
        .flat_map(|node| [node.attribute("xs.id"), node.attribute("xs.ref")]).flatten().map(str::to_owned).collect(),
        definitions: String::new(), track_guid_tag: String::new(), next_idx: None };
    for node in document.descendants() {
        if let Some(value) = node.attribute("xs.idx") {
            let value = value.parse::<u64>().map_err(|_| "invalid animation serial number")?;
            additions.next_idx = Some(additions.next_idx.map_or(value, |old| old.max(value)));
        }
    }
    let mut insertions: HashMap<_, (Node, String, usize)> = HashMap::new();
    let mut seen = HashSet::new();
    for copy in active {
        if !seen.insert(copy.track["guid"].as_str().ok_or("Image copy GUID is missing")?) {
            return Err("duplicate Image copy provenance".into());
        }
        let current_scene = model["scenes"].as_array().into_iter().flatten()
            .find(|scene| scene["guid"] == copy.scene_guid).ok_or("Image copy changed scene")?;
        let current = current_scene[copy.family.field()].as_array().into_iter().flatten()
            .find(|track| track["guid"] == copy.track["guid"]).ok_or("Image copy is missing")?;
        let mut metadata = copy.track.clone();
        if matches!(copy.family, CopyFamily::Image | CopyFamily::Guide) {
            metadata["visualCurves"] = current["visualCurves"].clone();
        }
        if matches!(copy.family, CopyFamily::Image | CopyFamily::Guide) {
            metadata["pointChannels"] = current["pointChannels"].clone();
        }
        if metadata != *current { return Err("Image copy metadata changed".into()); }
        let original_scene = initial["scenes"].as_array().into_iter().flatten()
            .find(|scene| scene["guid"] == copy.scene_guid).ok_or("Image donor scene is missing")?;
        let donor = original_scene[copy.family.field()].as_array().into_iter().flatten()
            .find(|track| track["guid"] == copy.template_track_guid).ok_or("Image donor is missing")?;
        let scene = select_guid(&graph, list(&graph, animation, "_scenes", &mut budget)?, &original_scene["guid"])?;
        let sources = raw_inherited_field(required(&graph, scene, "trackSourceSet")?, "_sources")?;
        values::list(&graph, sources, &mut budget)?;
        let owner = sources.children().filter(Node::is_element).find(|node|
            graph.resolve(*node).ok().and_then(|node| graph.guid(node).ok().flatten()).as_deref()
                == donor["guid"].as_str()).ok_or("Image donor membership is missing")?;
        let definition = graph.resolve(owner)?;
        if !definition.has_tag_name(copy.family.native_tag()) {
            return Err("animation copy native track family differs from provenance".into());
        }
        if copy.family != CopyFamily::Sound {
            if !graph.field(definition, "visualEffect")?.is_some_and(|node| node.has_tag_name("CMvEffect_VisualDefault")) {
                return Err("Image copying requires VisualDefault".into());
            }
            for field in ["keyParamEffect", "partsVisibleEffect", "soundEffect", "formEditEffect", "lipSyncEffect",
                "eyeBlinkEffect", "paramCtrlGroupEffect", "paramCtrlGroupSet"] {
                if graph.field(definition, field)?.is_some_and(|node| !node.has_tag_name("null")) {
                    return Err("Image copying only supports VisualDefault effects".into());
                }
            }
        }
        let mut owned = track_definition_ownership(&graph, owner, scene, shared, donor, &mut budget)?;
        if !owned.items.is_empty() { return Err("Image copy has unsupported calculation-order memberships".into()); }
        owned.definitions.sort_by_key(|node| node.range().start);
        let private: HashSet<_> = owned.definitions.iter().map(Node::id).collect();
        let mut allowed = HashSet::new();
        for field in ["_sceneSource", "_resourceGuid", "parentGuid"] {
            if let Ok(node) = raw_inherited_field(definition, field) { allowed.insert(node.id()); }
        }
        for object in &owned.definitions {
            for node in object.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
                budget.consume(1)?;
                let target = graph.resolve(node)?;
                if private.contains(&target.id()) || allowed.contains(&node.id()) { continue; }
                let attribute = matches!(object.tag_name().name(), "CMvAttrF" | "CMvAttrI" | "CMvAttrPt");
                let shared_kind = (attribute && raw_inherited_field(*object, "adaptType").ok() == Some(node)
                    && target.has_tag_name("AdaptType"))
                    || (raw_inherited_field(*object, "id").ok() == Some(node)
                        && (target.has_tag_name("CEffectId")
                            || (copy.family != CopyFamily::Image && attribute && target.has_tag_name("CAttrId"))))
                    || (node.attribute("xs.n") == Some("key") && target.has_tag_name("CAttrId")
                        && node.parent().is_some_and(|entry| entry.parent().is_some_and(|map| map.attribute("xs.n") == Some("attrMap"))));
                if !shared_kind || target.parent() != Some(shared) {
                    return Err("Image copy contains an unclassified outgoing reference".into());
                }
                direct_reference(&graph, node, target)?;
            }
        }
        let (cloned, storage) = animation_clone::linked_track(text, &owned.definitions, definition, &copy.track, &mut additions)?;
        let item = insertions.entry(sources.id()).or_insert((sources, String::new(), 0));
        item.1.push_str(&format!(r#"<{} xs.ref="{}"/>"#, copy.family.native_tag(), escape(&storage)));
        item.2 += 1;
        if let Some((list, member)) = owned.parent_member {
            let item = insertions.entry(list.id()).or_insert((list, String::new(), 0));
            item.1.push_str(&format!(r#"<{} xs.ref="{}"/>"#, member.tag_name().name(), escape(&cloned.guid_id)));
            item.2 += 1;
        }
        additions.definitions.push_str(&cloned.definitions);
    }
    let mut patches = Vec::new();
    for (_, (list, xml, count)) in insertions {
        patches.push(topology_count_patch(list, values::list(&graph, list, &mut budget)?.len() + count)?);
        emit_inserts(&mut patches, [None].into_iter(), &[], &list_end(list, text)?, [xml].into_iter());
    }
    let at = list_end(shared, text)?;
    emit_inserts(&mut patches, [None].into_iter(), &[], &at, [additions.definitions].into_iter());
    patches.sort_by_key(|(range, _)| range.start);
    if patches.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return Err("Image copy patch ranges overlap".into());
    }
    let mut expanded = text.to_owned();
    for (range, value) in patches.into_iter().rev() { expanded.replace_range(range, &value); }
    let expanded = CaffDocument::parse(&container.replace_main_xml(expanded.as_bytes())?)?;
    rewrite(&expanded, &read(&expanded)?, model)
}

pub(crate) fn rewrite(
    container: &CaffDocument,
    initial: &Value,
    model: &Value,
) -> Result<Vec<u8>, String> {
    if &read(container)? != initial {
        return Err("animation template differs from initial source".into());
    }
    if model == initial {
        return Ok(container.encode());
    }
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml).map_err(|_| "animation XML is not UTF-8")?;
    let document = xml_document(text)?;
    let graph = Graph::new(&document)?;
    let mut budget = Budget::new(5_000_000);
    let mut additions = NativeAdditions {
        reserved: document
            .root_element()
            .descendants()
            .filter(|node| node.is_element())
            .flat_map(|node| [node.attribute("xs.id"), node.attribute("xs.ref")])
            .flatten()
            .map(str::to_owned)
            .collect(),
        definitions: String::new(),
        track_guid_tag: String::new(),
        next_idx: document.root_element().descendants()
            .filter_map(|node| node.attribute("xs.idx")?.parse::<u64>().ok()).max(),
    };
    let animation = graph.resolve(
        document
            .root_element()
            .children()
            .find(|n| n.has_tag_name("main"))
            .and_then(|n| n.children().find(Node::is_element))
            .ok_or("animation main is missing")?,
    )?;
    let initial_scenes = initial["scenes"].as_array().ok_or("animation scenes missing")?;
    let candidate_scenes = model["scenes"]
        .as_array()
        .ok_or("animation scenes missing")?;
    let plan = scene_set_plan(initial_scenes, candidate_scenes)?;
    let mut normalized = model.clone();
    normalized["scenes"] = initial["scenes"].clone();
    additions.track_guid_tag = "CGuid".to_owned();
    'outer: for scene in initial_scenes {
        let scene_node = select_guid(
            &graph,
            list(&graph, animation, "_scenes", &mut budget)?,
            &scene["guid"],
        )?;
        let tracks = required(&graph, scene_node, "trackSourceSet")?;
        for node in list(&graph, tracks, "_sources", &mut budget)? {
            let resolved = graph.resolve(node)?;
            if resolved.has_tag_name("CMvTrack_Live2DModel_Source") {
                additions.track_guid_tag = resolved
                    .children()
                    .find(|child| child.is_element() && child.attribute("xs.n") == Some("guid"))
                    .map(|child| child.tag_name().name().to_owned())
                    .unwrap_or_else(|| "CGuid".to_owned());
                break 'outer;
            }
        }
    }
    let mut patches = Vec::new();
    resource_patches(text, &graph, animation, initial, model, &mut additions, &mut patches, &mut budget)?;
    normalized["resources"] = initial["resources"].clone();
    normalized["imageResources"] = initial["imageResources"].clone();
    scene_set_patches(
        text, &graph, animation, initial_scenes, candidate_scenes, &plan,
        &mut additions, &mut patches, &mut budget,
    )?;
    for slot in &plan.ordered {
        let (si, ci) = match slot {
            SceneSlot::Kept { original, candidate } => (*original, *candidate),
            _ => continue,
        };
        normalized["scenes"][si] = candidate_scenes[ci].clone();
        let scene = &initial_scenes[si];
        let candidate_scene = &candidate_scenes[ci];
        let scene_node = select_guid(
            &graph,
            list(&graph, animation, "_scenes", &mut budget)?,
            &scene["guid"],
        )?;
        for (key, path, tags) in [
            ("name", &["sceneName"][..], &["s"][..]),
            ("fps", &["movieInfo", "fps"][..], &["f", "d"][..]),
            ("durationFrames", &["movieInfo", "duration"][..], &["i"][..]),
            ("loop", &["movieInfo", "isLoopMotion"][..], &["b"][..]),
            ("tag", &["tag"][..], &["s"][..]),
        ] {
            if scene.get(key).is_none() && candidate_scene.get(key).is_none() {
                continue;
            }
            let value = &candidate_scene[key];
            if value != &scene[key] {
                patches.push(setting_patch(scene_node, path, tags, value)?);
            }
            normalized["scenes"][si][key] = scene[key].clone();
        }
        let tracks = required(&graph, scene_node, "trackSourceSet")?;
        let old_tracks = scene["tracks"].as_array().ok_or("animation tracks missing")?;
        let new_tracks = candidate_scene["tracks"]
            .as_array()
            .ok_or("animation tracks missing")?;
        let track_set = track_set_plan(old_tracks, new_tracks)?;
        let old_images = scene["imageTracks"].as_array().ok_or("animation image tracks missing")?;
        let new_images = candidate_scene["imageTracks"].as_array().ok_or("animation image tracks missing")?;
        let image_set = track_set_plan(old_images, new_images)?;
        let mut removed_extras: Vec<_> = image_set.removed.iter().map(|index| &old_images[*index]).collect();
        for field in ["soundTracks"] {
            let old = scene[field].as_array().ok_or("animation tracks missing")?;
            let new = candidate_scene[field].as_array().ok_or("animation tracks missing")?;
            let set = track_set_plan(old, new)?;
            for slot in &set.ordered {
                match slot {
                    TrackSlot::Kept { original, candidate } if old[*original] == new[*candidate] => {}
                    _ => return Err(format!("animation {field} only support deletion")),
                }
            }
            removed_extras.extend(set.removed.iter().map(|index| &old[*index]));
        }
        let old_guides = scene["guideTracks"].as_array().ok_or("animation Guide tracks missing")?;
        let new_guides = candidate_scene["guideTracks"].as_array().ok_or("animation Guide tracks missing")?;
        let guide_set = track_set_plan(old_guides, new_guides)?;
        for slot in &guide_set.ordered {
            match slot {
                TrackSlot::Kept { original, candidate } => {
                    let old = &old_guides[*original];
                    let new = &new_guides[*candidate];
                    if old != new {
                        guide_curve_changes(text, &graph, scene_node, old, new, &mut patches, &mut budget)?;
                    }
                }
                _ => return Err("animation Guide tracks only support existing anchors and deletion".into()),
            }
        }
        removed_extras.extend(guide_set.removed.iter().map(|index| &old_guides[*index]));
        for slot in &image_set.ordered {
            match slot {
                TrackSlot::Kept { original, candidate } => {
                    let old = &old_images[*original];
                    let new = &new_images[*candidate];
                    if old != new {
                        let track = select_guid(&graph, list(&graph, tracks, "_sources", &mut budget)?, &old["guid"])?;
                        image_curve_changes(text, &graph, track, old, new, &mut patches, &mut budget)?;
                    }
                }
                _ => return Err("animation image tracks only support deletion".into()),
            }
        }
        if track_set.ordered.iter().all(|slot| matches!(slot, TrackSlot::Kept { .. }))
            && track_set.removed.is_empty() && removed_extras.is_empty()
        {
            for (ti, track) in old_tracks.iter().enumerate() {
                let track_node = select_guid(
                    &graph,
                    list(&graph, tracks, "_sources", &mut budget)?,
                    &track["guid"],
                )?;
                track_curve_changes(
                    text, &graph, track_node, track, &new_tracks[ti], &mut patches,
                    &mut budget, &mut additions,
                )?;
                normalized["scenes"][si]["tracks"][ti]["parameterCurves"] =
                    track["parameterCurves"].clone();
                normalized["scenes"][si]["tracks"][ti]["partOpacityCurves"] =
                    track["partOpacityCurves"].clone();
                normalized["scenes"][si]["tracks"][ti]["legacyPartOpacityCurves"] =
                    track["legacyPartOpacityCurves"].clone();
                normalized["scenes"][si]["tracks"][ti]["underscorePartOpacityCurves"] =
                    track["underscorePartOpacityCurves"].clone();
                normalized["scenes"][si]["tracks"][ti]["skippedLegacyPartOpacityCount"] =
                    track["skippedLegacyPartOpacityCount"].clone();
            }
        } else {
            track_set_patches(
                text, &graph, scene_node, old_tracks, new_tracks, &track_set,
                &removed_extras,
                &mut additions, &mut patches, &mut budget,
            )?;
            normalized["scenes"][si]["tracks"] = scene["tracks"].clone();
        }
        normalized["scenes"][si]["imageTracks"] = scene["imageTracks"].clone();
        normalized["scenes"][si]["soundTracks"] = scene["soundTracks"].clone();
        normalized["scenes"][si]["guideTracks"] = scene["guideTracks"].clone();
    }
    if normalized != *initial {
        return Err(ONLY_EDITS.into());
    }
    if !additions.definitions.is_empty() {
        let shared = document
            .root_element()
            .children()
            .find(|node| node.has_tag_name("shared"))
            .ok_or("animation XML lacks a shared section")?
            .range();
        if !text[shared.clone()].ends_with("</shared>") {
            return Err("animation shared section is malformed".into());
        }
        let at = shared.end - "</shared>".len();
        patches.push((at..at, additions.definitions));
    }
    patches.sort_by_key(|(range, _)| range.start);
    if patches.windows(2).any(|p| p[0].0.end > p[1].0.start) {
        return Err("animation scalar patch ranges overlap".into());
    }
    let mut rewritten = text.to_owned();
    for (range, value) in patches.into_iter().rev() {
        rewritten.replace_range(range, &value);
    }
    let bytes = container.replace_main_xml(rewritten.as_bytes())?;
    if read(&CaffDocument::parse(&bytes)?)? != *model {
        return Err("rewritten animation differs from requested source".into());
    }
    Ok(bytes)
}

fn image_visual_attribute<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, id: &str, budget: &mut Budget,
) -> Result<Node<'d, 'i>, String> {
    let (name, kind, sequence_field, sequence_kind) = if let Some((_, name)) = VISUAL_SCALAR_CHANNELS.iter().find(|(key, _)| *key == id) {
        (*name, "CMvAttrF", "valueData", "CMutableSequence")
    } else if let Some((_, name)) = VISUAL_POINT_CHANNELS.iter().find(|(key, _)| *key == id) {
        (*name, "CMvAttrPt", "valueDataXY", "CXY_TNSSequence")
    } else { return Err("unsupported Image visual channel".into()); };
    let effect = track_effect(graph, track, "visualEffect", "CMvEffect_VisualDefault", "Image visual", budget)?
        .ok_or("Image visual effect is missing")?;
    if !effect.has_tag_name("CMvEffect_VisualDefault") { return Err("unsupported Image visual effect".into()); }
    direct_reference(graph, raw_inherited_field(effect, "track")?, track)?;
    let members = raw_inherited_field(effect, "attrList")?;
    let map = raw_inherited_field(effect, "attrMap")?;
    for list in [members, map] {
        if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
            return Err("Image visual membership uses shared storage".into());
        }
        values::list(graph, list, budget)?;
    }
    let mut found = Vec::new();
    for member in members.children().filter(Node::is_element) {
        let attribute = graph.resolve(member)?;
        if required(graph, attribute, "id")?.attribute("idstr") == Some(id) { found.push((member, attribute)); }
    }
    if found.len() != 1 { return Err("Image scalar member is missing or ambiguous".into()); }
    let (member, attribute) = found[0];
    if !attribute.has_tag_name(kind) || member.attributes().count() != 1 {
        return Err("Image scalar member is unsupported".into());
    }
    direct_reference(graph, member, attribute)?;
    let shared = track.document().root_element().children().find(|node| node.has_tag_name("shared"))
        .ok_or("animation XML lacks a shared section")?;
    parent_attribute_storage(graph, attribute, track, shared, budget)?;
    let sequence = raw_inherited_field(attribute, sequence_field)?;
    if !sequence.has_tag_name(sequence_kind) { return Err("Image visual channel has unsupported sequence storage".into()); }
    let backlink = raw_inherited_field(sequence, "attr")?;
    direct_reference(graph, backlink, attribute)?;
    let named = raw_inherited_field(effect, name)?;
    direct_reference(graph, named, attribute)?;
    let mut mapped = Vec::new();
    for entry in map.children().filter(Node::is_element) {
        let key = required(graph, entry, "key")?;
        let value = required(graph, entry, "value")?;
        if key.attribute("idstr") == Some(id) || value == attribute {
            if !entry.has_tag_name("entry") || entry.attributes().len() != 0
                || entry.children().filter(Node::is_element).count() != 2
                || !key.has_tag_name("CAttrId") || key.attribute("idstr") != Some(id) || value != attribute {
                return Err("Image scalar map identity differs from its member".into());
            }
            let value = raw_field(entry, "value")?;
            direct_reference(graph, value, attribute)?;
            mapped.push(value);
        }
    }
    let empty_point_map = kind == "CMvAttrPt" && map.has_tag_name("hash_map")
        && map.attribute("count").and_then(|value| value.parse::<usize>().ok()) == Some(0)
        && !map.children().any(|node| node.is_element());
    if mapped.len() != 1 && !empty_point_map { return Err("Image scalar map is missing or ambiguous".into()); }
    let mut references = vec![member, named, backlink];
    references.extend(mapped);
    exclusive_references(attribute.attribute("xs.id").unwrap(), &references, "Image scalar", budget)?;
    let track_reference = raw_inherited_field(attribute, "track")?;
    let mut outgoing = vec![track_reference, backlink];
    if graph.field(attribute, "adaptType")?.is_some() {
        let reference = raw_inherited_field(attribute, "adaptType")?;
        if reference.attribute("xs.ref").is_some() {
            let target = graph.resolve(reference)?;
            if !target.has_tag_name("AdaptType") || target.parent() != Some(shared) {
                return Err("Image scalar adaptation uses unsupported storage".into());
            }
            direct_reference(graph, reference, target)?;
            outgoing.push(reference);
        }
    }
    for reference in attribute.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !outgoing.contains(&reference) { return Err("Image scalar contains an unclassified reference".into()); }
    }
    Ok(attribute)
}

fn guide_curve_changes(
    text: &str, graph: &Graph<'_, '_>, scene: Node<'_, '_>, old: &Value, new: &Value,
    patches: &mut Vec<(Range<usize>, String)>, budget: &mut Budget,
) -> Result<(), String> {
    let before = old["visualCurves"].as_array().ok_or("Guide scalar curves are missing")?;
    let after = new["visualCurves"].as_array().ok_or("Guide scalar curves are missing")?;
    if before.len() != after.len() { return Err("Guide scalar structure changes are unsupported".into()); }
    let mut metadata = old.clone();
    metadata["visualCurves"] = new["visualCurves"].clone();
    metadata["pointChannels"] = new["pointChannels"].clone();
    if metadata != *new { return Err("Guide track metadata changes are unsupported".into()); }
    let sources = raw_inherited_field(required(graph, scene, "trackSourceSet")?, "_sources")?;
    values::list(graph, sources, budget)?;
    let mut owners = Vec::new();
    for member in sources.children().filter(Node::is_element) {
        if graph.guid(graph.resolve(member)?)?.as_deref() == old["guid"].as_str() { owners.push(member); }
    }
    if owners.len() != 1 || !graph.resolve(owners[0])?.has_tag_name("CMvTrack_GuideImage_Source") {
        return Err("Guide track membership is missing or ambiguous".into());
    }
    let shared = scene.document().root_element().children().find(|node| node.has_tag_name("shared"))
        .ok_or("animation XML lacks a shared section")?;
    let owned = track_definition_ownership(graph, owners[0], scene, shared, old, budget)?;
    for (old_curve, new_curve) in before.iter().zip(after) {
        if old_curve == new_curve { continue; }
        visual_key_changes(old_curve, new_curve, "Guide")?;
        let id = old_curve["id"].as_str().ok_or("Guide scalar ID is missing")?;
        if !VISUAL_SCALAR_CHANNELS.iter().any(|(key, _)| *key == id) {
            return Err("unsupported Guide scalar channel".into());
        }
        let mut attributes = Vec::new();
        for attribute in &owned.definitions {
            budget.consume(1)?;
            if attribute.has_tag_name("CMvAttrF") && required(graph, *attribute, "id")?.attribute("idstr") == Some(id) {
                attributes.push(*attribute);
            }
        }
        if attributes.len() != 1 { return Err("Guide scalar storage is missing or ambiguous".into()); }
        curve_patches(text, graph, attributes[0], old_curve, new_curve, patches, budget)?;
    }
    let before = old["pointChannels"].as_array().ok_or("Guide point channels are missing")?;
    let after = new["pointChannels"].as_array().ok_or("Guide point channels are missing")?;
    if before.len() != after.len() { return Err("Guide point channel structure changes are unsupported".into()); }
    for (old, new) in before.iter().zip(after) {
        if old == new { continue; }
        let (before, after) = point_channel_points(old, new, "Guide")?;
        let id = old["id"].as_str().ok_or("Guide point ID is missing")?;
        if !VISUAL_POINT_CHANNELS.iter().any(|(key, _)| *key == id) {
            return Err("unsupported Guide point channel".into());
        }
        let mut attributes = Vec::new();
        for attribute in &owned.definitions {
            budget.consume(1)?;
            if attribute.has_tag_name("CMvAttrPt") && required(graph, *attribute, "id")?.attribute("idstr") == Some(id) {
                attributes.push(*attribute);
            }
        }
        if attributes.len() != 1 { return Err("Guide point storage is missing or ambiguous".into()); }
        point_channel_patches(text, graph, attributes[0], before, after, "Guide", patches, budget)?;
    }
    Ok(())
}

fn image_curve_changes(
    text: &str, graph: &Graph<'_, '_>, track: Node<'_, '_>, old: &Value, new: &Value,
    patches: &mut Vec<(Range<usize>, String)>, budget: &mut Budget,
) -> Result<(), String> {
    let mut metadata = old.clone();
    metadata["visualCurves"] = new["visualCurves"].clone();
    metadata["pointChannels"] = new["pointChannels"].clone();
    if metadata != *new { return Err("Image track metadata changes are unsupported".into()); }
    let before = old["visualCurves"].as_array().ok_or("Image scalar curves are missing")?;
    let after = new["visualCurves"].as_array().ok_or("Image scalar curves are missing")?;
    if before.len() != after.len() { return Err("Image scalar curve structure changes are unsupported".into()); }
    for (old, new) in before.iter().zip(after) {
        if old == new { continue; }
        visual_key_changes(old, new, "Image")?;
        let attribute = image_visual_attribute(graph, track, old["id"].as_str().ok_or("Image scalar ID is missing")?, budget)?;
        curve_patches(text, graph, attribute, old, new, patches, budget)?;
    }
    image_point_changes(text, graph, track, old, new, patches, budget)?;
    Ok(())
}

fn visual_key_changes(old: &Value, new: &Value, family: &str) -> Result<(), String> {
    let points = old["points"].as_array().ok_or_else(|| format!("{family} scalar points are missing"))?;
    let target = new["points"].as_array().ok_or_else(|| format!("{family} scalar points are missing"))?;
    let types = old["curveTypes"].as_array().ok_or_else(|| format!("{family} scalar curve types are missing"))?;
    let target_types = new["curveTypes"].as_array().ok_or_else(|| format!("{family} scalar curve types are missing"))?;
    if types.len() != points.len() || target_types.len() != target.len() {
        return Err(format!("{family} scalar point and curve-type counts differ"));
    }
    let mut allowed = old.clone();
    allowed["points"] = new["points"].clone();
    allowed["curveTypes"] = new["curveTypes"].clone();
    if allowed != *new { return Err(format!("{family} scalar curve metadata changes are unsupported")); }
    for slot in curve_plan(points, target)?.ordered {
        let (candidate, original) = match slot {
            KeySlot::Existing { original, candidate } => (candidate, Some(original)),
            KeySlot::Insert { candidate } => (candidate, None),
        };
        let point = &target[candidate];
        finite(&point["anchor"]["value"])?;
        for side in ["previous", "next"] {
            for key in ["frame", "value"] { finite(&point[side][key])?; }
        }
        if original.is_none_or(|index| types[index] != target_types[candidate])
            && !target_types[candidate].as_str().is_some_and(|kind| CURVE_TYPES.contains(&kind)) {
            return Err(format!("{family} scalar curve type is unsupported"));
        }
        if let Some(original) = original {
            let mut retained = point.clone();
            retained["anchor"]["value"] = points[original]["anchor"]["value"].clone();
            for side in ["previous", "next"] {
                for key in ["frame", "value"] { retained[side][key] = points[original][side][key].clone(); }
            }
            if retained == points[original] { continue; }
        }
        if !target_types[candidate].as_str().is_some_and(|kind| CURVE_TYPES.contains(&kind)) {
            return Err(format!("{family} scalar curve type is unsupported"));
        }
        let frame = point["anchor"]["frame"].as_i64().ok_or("animation anchor frame missing")?;
        let mut replacement = new_key_point(frame, finite(&point["anchor"]["value"])?);
        for side in ["previous", "next"] {
            for key in ["frame", "value"] { replacement[side][key] = point[side][key].clone(); }
        }
        if *point != replacement { return Err(format!("{family} scalar changes require retained or canonical protected fields")); }
    }
    Ok(())
}

fn image_point_changes(
    text: &str, graph: &Graph<'_, '_>, track: Node<'_, '_>, old: &Value, new: &Value,
    patches: &mut Vec<(Range<usize>, String)>, budget: &mut Budget,
) -> Result<(), String> {
    let before = old["pointChannels"].as_array().ok_or("Image point channels are missing")?;
    let after = new["pointChannels"].as_array().ok_or("Image point channels are missing")?;
    if before.len() != after.len() { return Err("Image point channel structure changes are unsupported".into()); }
    for (old, new) in before.iter().zip(after) {
        if old == new { continue; }
        let (before, after) = point_channel_points(old, new, "Image")?;
        let attribute = image_visual_attribute(graph, track, old["id"].as_str().ok_or("Image point ID is missing")?, budget)?;
        point_channel_patches(text, graph, attribute, before, after, "Image", patches, budget)?;
    }
    Ok(())
}

fn point_channel_points<'a>(old: &'a Value, new: &'a Value, family: &str) -> Result<(&'a [Value], &'a [Value]), String> {
    let mut allowed = old.clone();
    allowed["points"] = new["points"].clone();
    if allowed != *new { return Err(format!("{family} point channel metadata changes are unsupported")); }
    let before = old["points"].as_array().ok_or_else(|| format!("{family} points are missing"))?;
    let after = new["points"].as_array().ok_or_else(|| format!("{family} points are missing"))?;
    Ok((before, after))
}

fn point_channel_patches(
    text: &str, graph: &Graph<'_, '_>, attribute: Node<'_, '_>, before: &[Value], after: &[Value], family: &str,
    patches: &mut Vec<(Range<usize>, String)>, budget: &mut Budget,
) -> Result<(), String> {
    let sequence = raw_inherited_field(attribute, "valueDataXY")?;
    let native = list(graph, sequence, "points", budget)?;
    if native.len() != before.len() { return Err(format!("{family} point storage differs from projection")); }
    let mut frames = HashMap::new();
    for (index, point) in before.iter().enumerate() {
        let frame = point["frame"].as_i64().ok_or("visual point frame missing")?;
        if frames.insert(frame, index).is_some() { return Err("duplicate visual point frame".into()); }
    }
    let mut used = HashSet::new();
    let mut last = None;
    let mut slots = Vec::new();
    let mut insertion = false;
    for (candidate, point) in after.iter().enumerate() {
        let frame = point["frame"].as_i64().ok_or("visual point frame missing")?;
        if !used.insert(frame) { return Err("duplicate visual point frame".into()); }
        if let Some(&original) = frames.get(&frame) {
            if last.replace(original).is_some_and(|old| old >= original) { return Err("visual points must retain their original frames and order".into()); }
            insertion |= before[original]["corner"] != point["corner"];
            slots.push(KeySlot::Existing { original, candidate });
        } else {
            insertion = true;
            slots.push(KeySlot::Insert { candidate });
        }
    }
    let removed: Vec<_> = before.iter().enumerate().filter(|(_, p)| !used.contains(&p["frame"].as_i64().unwrap())).map(|(i, _)| i).collect();
    if insertion || !removed.is_empty() {
        point_structure_storage(graph, attribute, sequence, before.len())?;
    }
    if insertion {
        for points in [before, after] {
            if points.windows(2).any(|p| p[0]["frame"].as_i64() >= p[1]["frame"].as_i64()) {
                return Err("point insertion requires increasing frames".into());
            }
        }
        for point in &native { visual_point_record_storage(*point)?; }
    }
    if before.len() != after.len() {
        patches.push(count_patch(raw_field(sequence, "points")?, after.len())?);
    }
    for index in removed {
            let point = native[index];
            if point.descendants().any(|node| node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some()) {
                return Err("removed visual point storage is shared or referenced".into());
            }
            patches.push((point.range(), String::new()));
    }
    let mut inserts = Vec::new();
    for slot in &slots {
        let KeySlot::Existing { original, candidate } = *slot else {
            let KeySlot::Insert { candidate } = *slot else { unreachable!() };
            inserts.push(serialize_visual_point(&after[candidate])?);
            continue;
        };
        let (old, new, point) = (&before[original], &after[candidate], native[original]);
        if old == new { continue; }
        let mut allowed = old.clone();
        allowed["x"] = new["x"].clone();
        allowed["y"] = new["y"].clone();
        if old["corner"] != new["corner"] {
            serialize_visual_point(new)?;
            allowed["corner"] = new["corner"].clone();
            patches.push(native_scalar_patch(text, raw_field(point, "isCorner")?, sequence,
                NativeScalar::Boolean(old["corner"].as_bool().ok_or("visual corner missing")?),
                NativeScalar::Boolean(new["corner"].as_bool().ok_or("visual corner missing")?))?);
        }
        if allowed != *new { return Err(format!("{family} point metadata changes are unsupported")); }
        for axis in ["x", "y"] {
            let previous = finite(&old[axis])?;
            let value = point_coordinate(&new[axis])?;
            if old[axis] == new[axis] { continue; }
            if new[axis].as_f64() != Some(value) { return Err(format!("{family} point coordinate is not normalized to f32")); }
            for name in ["super", "value"] {
                let vector = raw_field(point, name)?;
                if !point.has_tag_name("CPtTNS") || !vector.has_tag_name("GVector2") {
                    return Err(format!("{family} point requires CPtTNS and GVector2 storage"));
                }
                patches.push(native_scalar_patch(text, raw_field(vector, axis)?, sequence,
                    NativeScalar::Float(previous), NativeScalar::Float(value))?);
            }
        }
    }
    if !inserts.is_empty() {
        let end = list_end(raw_field(sequence, "points")?, text)?;
        emit_inserts(patches, slots.iter().map(|slot| match slot {
            KeySlot::Existing { original, .. } => Some(*original), KeySlot::Insert { .. } => None,
        }), &native, &end, inserts.into_iter());
    }
    Ok(())
}

fn serialize_visual_point(point: &Value) -> Result<String, String> {
    let frame = point["frame"].as_i64().and_then(|f| i32::try_from(f).ok()).ok_or("visual point frame must fit signed i32")?;
    let corner = point["corner"].as_bool().ok_or("visual corner must be boolean")?;
    let (x, y) = (point_coordinate(&point["x"])?, point_coordinate(&point["y"])?);
    if *point != json!({"frame":frame,"x":x,"y":y,"corner":corner}) { return Err("visual point must have canonical fields and normalized coordinates".into()); }
    Ok(format!("<CPtTNS><GVector2 xs.n=\"super\"><f xs.n=\"x\">{x}</f><f xs.n=\"y\">{y}</f></GVector2><b xs.n=\"isCorner\">{corner}</b><GVector2 xs.n=\"value\"><f xs.n=\"x\">{x}</f><f xs.n=\"y\">{y}</f></GVector2><i xs.n=\"pos\">{frame}</i></CPtTNS>"))
}

fn visual_point_record_storage(point: Node<'_, '_>) -> Result<(), String> {
    if !point.has_tag_name("CPtTNS") || point.attributes().len() != 0
        || point.children().filter(Node::is_element).count() != 4 {
        return Err("unsupported visual point insertion record".into());
    }
    for (name, tag) in [("super", "GVector2"), ("value", "GVector2"), ("isCorner", "b"), ("pos", "i")] {
        let field = raw_field(point, name)?;
        if !field.has_tag_name(tag) || field.attributes().any(|a| a.name() != "xs.n") {
            return Err("unsupported visual point insertion field".into());
        }
        let vector = tag == "GVector2";
        if field.children().filter(Node::is_element).count() != if vector { 2 } else { 0 } {
            return Err("unsupported visual point insertion field shape".into());
        }
        if vector {
            for axis in ["x", "y"] {
                let coordinate = raw_field(field, axis)?;
                if !coordinate.has_tag_name("f") || coordinate.attributes().any(|a| a.name() != "xs.n")
                    || coordinate.children().any(|n| n.is_element()) {
                    return Err("unsupported visual insertion coordinate".into());
                }
            }
        }
    }
    Ok(())
}

fn point_structure_storage(
    graph: &Graph<'_, '_>, attribute: Node<'_, '_>, sequence: Node<'_, '_>, count: usize,
) -> Result<(), String> {
    if !sequence.has_tag_name("CXY_TNSSequence")
        || sequence.attributes().any(|a| a.name() != "xs.n")
        || sequence.children().filter(Node::is_element).count() != 3 {
        return Err("unsupported visual point structure sequence layout".into());
    }
    let points = raw_field(sequence, "points")?;
    let base = raw_field(sequence, "basePt")?;
    direct_reference(graph, raw_field(sequence, "attr")?, attribute)?;
    if !points.has_tag_name("array") || points.attribute("type") != Some("CPtTNS")
        || points.attributes().any(|a| !matches!(a.name(), "xs.n" | "type" | "count"))
        || points.attribute("count").and_then(|n| n.parse::<usize>().ok()) != Some(count) {
        return Err("unsupported visual point structure array layout".into());
    }
    let items = inline_items(points, "visual points")?;
    if items.len() != count || items.iter().any(|p| !p.has_tag_name("CPtTNS")) {
        return Err("visual point structure storage count differs".into());
    }
    if !base.has_tag_name("GVector2") || base.attributes().any(|a| a.name() != "xs.n")
        || base.children().filter(Node::is_element).count() != 2 {
        return Err("unsupported visual base point layout".into());
    }
    for axis in ["x", "y"] {
        let field = raw_field(base, axis)?;
        if !field.has_tag_name("f") || field.attributes().any(|a| a.name() != "xs.n")
            || field.children().any(|n| n.is_element()) {
            return Err("unsupported visual base coordinate layout".into());
        }
    }
    Ok(())
}

fn curve_patches(
    text: &str,
    graph: &Graph<'_, '_>,
    attribute: Node<'_, '_>,
    curve: &Value,
    target: &Value,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
) -> Result<(), String> {

    let old_points = curve["points"]
        .as_array()
        .ok_or("animation points missing")?;
    let new_points = target["points"]
        .as_array()
        .ok_or("animation points missing")?;
    if old_points != new_points || curve["curveTypes"] != target["curveTypes"] {
        cache_patches(text, required(graph, attribute, "valueData")?, new_points, patches)?;
    }
    let plan = curve_plan(old_points, new_points)?;
    let mut normal = target.clone();
    if plan_structural(&plan, old_points, new_points) {
        if target["points"] == curve["points"]
            && target["curveTypes"] == curve["curveTypes"]
        {
            if normal != *curve {
                return Err(ONLY_EDITS.into());
            }
            return Ok(());
        }
        structural_curve_patches(
            text,
            &graph,
            attribute,
            curve,
            target,
            &plan,
            patches,
        )?;
        normal["points"] = curve["points"].clone();
        normal["curveTypes"] = curve["curveTypes"].clone();
        if normal != *curve {
            return Err(ONLY_EDITS.into());
        }
        return Ok(());
    }
    let mut anchors_changed = false;
    let mut handles_changed = false;
    for (pi, (old, new)) in old_points.iter().zip(new_points).enumerate() {
        finite(&new["anchor"]["value"])?;
        anchors_changed |= old["anchor"]["value"] != new["anchor"]["value"];
        normal["points"][pi]["anchor"]["value"] = old["anchor"]["value"].clone();
        for side in ["previous", "next"] {
            for key in ["frame", "value"] {
                handles_changed |= old[side][key] != new[side][key];
                normal["points"][pi][side][key] = old[side][key].clone();
            }
        }
    }
    let types_changed = target["curveTypes"] != curve["curveTypes"];
    if types_changed {
        normal["curveTypes"] = curve["curveTypes"].clone();
    }
    if !(anchors_changed || handles_changed || types_changed) {
        if normal != *curve {
            return Err(ONLY_EDITS.into());
        }
        return Ok(());
    }
    let sequence = required(&graph, attribute, "valueData")?;
    let points = list(&graph, sequence, "points", budget)?;
    if points.len() != old_points.len() {
        return Err("animation source point count differs".into());
    }
    for ((point, old), new) in points.into_iter().zip(old_points).zip(new_points) {
        let anchor = required(&graph, point, "anchor")?;
        if Some(integer(&graph, anchor, "pos")?) != old["anchor"]["frame"].as_i64() {
            return Err("animation source anchor identity differs".into());
        }
        for (side, control) in [("previous", "prev"), ("next", "next")] {
            for (key, field) in [("frame", "posF"), ("value", "doubleValue")] {
                if old[side][key] != new[side][key] {
                    let control = required(&graph, point, control)?;
                    patches.push(scalar_patch(text, required(&graph, control, field)?, sequence,
                        finite(&old[side][key])?, finite(&new[side][key])?)?);
                }
            }
        }
        let old_value = finite(&old["anchor"]["value"])?;
        let new_value = finite(&new["anchor"]["value"])?;
        if old_value != new_value {
            patches.push(scalar_patch(
                text,
                required(&graph, anchor, "doubleValue")?,
                sequence,
                old_value,
                new_value,
            )?);
        }
    }
    if types_changed {
        let nodes = list(&graph, sequence, "curveTypes", budget)?;
        let types = |curve: &Value| curve["curveTypes"].as_array().cloned().unwrap_or_default();
        let pairs = types(curve).into_iter().zip(types(target));
        for (node, (old, new)) in nodes.into_iter().zip(pairs) {
            if old != new {
                patches.push(curve_type_patch(node, sequence, &new)?);
            }
        }
    }
    if !anchors_changed {
        if normal != *curve {
            return Err(ONLY_EDITS.into());
        }
        return Ok(());
    }
    for (name, minimum) in [("curMin", true), ("curMax", false)] {
        let extrema = |points: &[Value]| -> Result<f64, String> {
            let values = points
                .iter()
                .map(|p| finite(&p["anchor"]["value"]))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(values
                .into_iter()
                .reduce(|a, b| if minimum { a.min(b) } else { a.max(b) })
                .ok_or("empty animation curve cannot be edited")?)
        };
        let old = extrema(old_points)?;
        let new = extrema(new_points)?;
        let field = required(&graph, sequence, name)?;
        if number(&graph, sequence, name)? != old {
            return Err(format!(
                "animation {name} cache differs from anchor extrema"
            ));
        }
        if old != new {
            patches.push(scalar_patch(text, field, sequence, old, new)?);
        }
    }
    if normal != *curve {
        return Err(ONLY_EDITS.into());
    }
    Ok(())
}

/// Cubism caches a sequence's key frames and its last evaluation in `keyPts2`, `lastPos` and `lastValue`, and
/// writes `<null>`, -1 and NaN there while the cache is unbuilt; an edited curve goes back to that state. `keyMin`
/// and `keyMax` hold the first and last key frames, 0 for an empty curve. A sequence without them is left as it is.
fn cache_patches<'d, 'i>(
    text: &str,
    sequence: Node<'d, 'i>,
    points: &[Value],
    patches: &mut Vec<(Range<usize>, String)>,
) -> Result<(), String> {
    fn named<'d, 'i>(node: Node<'d, 'i>, name: &str) -> Option<Node<'d, 'i>> {
        node.children().find(|child| child.is_element() && child.attribute("xs.n") == Some(name))
    }
    let Some(base) = named(sequence, "super").filter(|base| base.has_tag_name("ACValueSequence")) else {
        return Ok(());
    };
    let frames = points
        .iter()
        .map(|point| point["anchor"]["frame"].as_i64().ok_or("anchorFrame must be a signed integer"))
        .collect::<Result<Vec<_>, _>>()?;
    let bound = |frame: Option<&i64>| frame.copied().unwrap_or(0).to_string();
    let wanted = [
        ("keyPts2", "null", String::new()),
        ("lastValue", "d", "NaN".to_owned()),
        ("lastPos", "i", "-1".to_owned()),
        ("keyMin", "i", bound(frames.first())),
        ("keyMax", "i", bound(frames.last())),
    ];
    for (name, tag, value) in wanted {
        let Some(field) = named(base, name) else { continue };
        if field.has_tag_name(tag) && (tag == "null" || field.text().map(str::trim) == Some(value.as_str())) {
            continue;
        }
        let shared = field.descendants().chain(field.ancestors().take_while(|node| node.id() != sequence.id()))
            .any(|node| node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some());
        if shared || text.get(field.range()).is_none() {
            return Err(format!("animation {name} cache uses shared or aliased storage"));
        }
        let xml = if tag == "null" {
            format!(r#"<null xs.n="{name}" />"#)
        } else {
            format!(r#"<{tag} xs.n="{name}">{value}</{tag}>"#)
        };
        patches.push((field.range(), xml));
    }
    Ok(())
}

fn curve_attribute<'d, 'i>(
    graph: &Graph<'d, 'i>,
    effect: Node<'d, 'i>,
    id: &Value,
    budget: &mut Budget,
) -> Result<Node<'d, 'i>, String> {
    let attributes = list(graph, effect, "attrList", budget)?;
    let mut matches = Vec::new();
    for attribute in attributes {
        if required(graph, attribute, "id")?.attribute("idstr") == id.as_str() {
            matches.push(attribute);
        }
    }
    if matches.len() != 1 {
        return Err("animation channel identity is ambiguous".into());
    }
    Ok(matches[0])
}

fn select_guid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    nodes: Vec<Node<'d, 'i>>,
    id: &Value,
) -> Result<Node<'d, 'i>, String> {
    let id = id.as_str().ok_or("animation GUID missing")?;
    let mut result = None;
    for node in nodes {
        if graph.guid(node)? == Some(id) && result.replace(node).is_some() {
            return Err("animation GUID is ambiguous".into());
        }
    }
    result.ok_or_else(|| "animation GUID was not found".into())
}

#[derive(PartialEq)]
enum NativeScalar {
    Double(f64),
    Float(f64),
    Integer(i64),
    Boolean(bool),
}

impl NativeScalar {
    fn tag(&self) -> &'static str {
        match self {
            NativeScalar::Double(_) => "d",
            NativeScalar::Float(_) => "f",
            NativeScalar::Integer(_) => "i",
            NativeScalar::Boolean(_) => "b",
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            NativeScalar::Double(_) => "double",
            NativeScalar::Float(_) => "float",
            NativeScalar::Integer(_) => "integer",
            NativeScalar::Boolean(_) => "boolean",
        }
    }

    fn parse(tag: &str, token: &str) -> Result<NativeScalar, String> {
        match tag {
            "d" | "f" => token
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(if tag == "d" { NativeScalar::Double } else { NativeScalar::Float })
                .ok_or_else(|| "invalid animation scalar token".into()),
            "i" => token
                .parse::<i64>()
                .map(NativeScalar::Integer)
                .map_err(|_| "invalid animation scalar token".into()),
            "b" => token
                .parse::<bool>()
                .map(NativeScalar::Boolean)
                .map_err(|_| "invalid animation scalar token".into()),
            _ => Err("invalid animation scalar token".into()),
        }
    }

    fn text(&self) -> String {
        match self {
            NativeScalar::Double(value) | NativeScalar::Float(value) => value.to_string(),
            NativeScalar::Integer(value) => value.to_string(),
            NativeScalar::Boolean(value) => value.to_string(),
        }
    }
}

fn scalar_patch(
    text: &str,
    field: Node<'_, '_>,
    sequence: Node<'_, '_>,
    old: f64,
    new: f64,
) -> Result<(Range<usize>, String), String> {
    // Cubism stores some real fields, such as a handle's posF, as floats.
    let scalar = if field.has_tag_name("f") { NativeScalar::Float } else { NativeScalar::Double };
    native_scalar_patch(text, field, sequence, scalar(old), scalar(new))
}

fn native_scalar_patch(
    text: &str,
    field: Node<'_, '_>,
    sequence: Node<'_, '_>,
    old: NativeScalar,
    new: NativeScalar,
) -> Result<(Range<usize>, String), String> {
    let tag = old.tag();
    if new.tag() != tag {
        return Err("animation scalar kind differs from its storage".into());
    }
    if !field.has_tag_name(tag) {
        return Err(format!("animation edit requires a native {} scalar", old.kind()));
    }
    let mut owned = false;
    for ancestor in field.ancestors() {
        if ancestor.attribute("xs.ref").is_some() || ancestor.attribute("xs.id").is_some() {
            return Err("animation edit target uses shared or aliased storage".into());
        }
        if ancestor.id() == sequence.id() {
            owned = true;
            break;
        }
    }
    if !owned {
        return Err("animation scalar is outside its inline sequence".into());
    }
    let children: Vec<_> = field.children().collect();
    if children.len() != 1 || !children[0].is_text() {
        return Err("animation scalar requires one inline text node".into());
    }
    let range = children[0].range();
    let raw = text
        .get(range.clone())
        .ok_or("animation scalar range is invalid")?;
    if field.text() != Some(raw) || raw.split_ascii_whitespace().count() != 1 {
        return Err("animation scalar requires plain numeric text".into());
    }
    let token = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    let actual = NativeScalar::parse(tag, token)?;
    if actual != old {
        return Err("animation scalar differs from source or is not finite".into());
    }
    match new {
        NativeScalar::Float(value) if !(value as f32).is_finite() => {
            return Err("animation float does not fit finite 32-bit storage".into());
        }
        NativeScalar::Integer(value) if !(i32::MIN as i64..=i32::MAX as i64).contains(&value) => {
            return Err("animation integer does not fit a signed 32-bit integer".into());
        }
        NativeScalar::Double(value) | NativeScalar::Float(value) if !value.is_finite() => {
            return Err("animation scalar differs from source or is not finite".into());
        }
        _ => {}
    }
    let start = range.start + raw.len()
        - raw
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .len();
    Ok((start..start + token.len(), new.text()))
}

/// The element named `name` written inside `owner` (`read` refuses a repeated one); a shared or
/// aliased one is refused.
fn inline_field<'d, 'i>(owner: Node<'d, 'i>, name: &str) -> Result<Node<'d, 'i>, String> {
    let field = owner
        .children()
        .find(|node| node.is_element() && node.attribute("xs.n") == Some(name))
        .ok_or_else(|| format!("animation source lacks {name}"))?;
    if field.attribute("xs.ref").is_some() || field.attribute("xs.id").is_some() {
        return Err(format!("animation {name} is shared or aliased"));
    }
    Ok(field)
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A scene's tag element; the supplied files store an empty tag (34 of 49 scenes) as a self-closing element.
pub(crate) fn tag_xml(element: &str, text: &str) -> String {
    if text.is_empty() {
        format!(r#"<{element} xs.n="tag" />"#)
    } else {
        format!(r#"<{element} xs.n="tag">{}</{element}>"#, escape(text))
    }
}

/// Writes one scene setting element again whole, so an empty name can take text.
fn setting_patch(
    scene: Node<'_, '_>,
    path: &[&str],
    tags: &[&str],
    value: &Value,
) -> Result<(Range<usize>, String), String> {
    let mut field = scene;
    for name in path {
        field = inline_field(field, name)?;
    }
    let name = path[path.len() - 1];
    let tag = field.tag_name().name();
    if !tags.contains(&tag) || field.attributes().count() != 1 || field.children().any(|child| !child.is_text()) {
        return Err(format!("animation scene {name} requires plain inline storage"));
    }
    if name == "tag" && value.as_str() == Some("") {
        return Ok((field.range(), tag_xml(tag, "")));
    }
    let token = match value {
        Value::String(text) => Some(escape(text)),
        // An f64 prints a whole number without a fraction, as an `i` element needs.
        Value::Number(number) => number.as_f64().map(|value| value.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
    .ok_or_else(|| format!("animation scene {name} has an invalid value"))?;
    Ok((field.range(), format!(r#"<{tag} xs.n="{name}">{token}</{tag}>"#)))
}

struct SceneReference {
    storage_id: String,
    reference: String,
}

fn image_resource_donor<'d, 'i>(
    graph: &Graph<'d, 'i>, animation: Node<'d, 'i>, budget: &mut Budget,
) -> Result<Node<'d, 'i>, String> {
    let manager = required(graph, animation, "resourceManager")?;
    let shared = manager.document().root_element().children().find(|node| node.has_tag_name("shared"))
        .ok_or("animation XML lacks a shared section")?;
    let members = raw_inherited_field(manager, "_resourceRefList")?;
    let map = raw_inherited_field(manager, "resourceGuidMap")?;
    let root = required(graph, manager, "rootGroup")?;
    if root != raw_inherited_field(manager, "rootGroup")? || !root.has_tag_name("CResourceGroup") {
        return Err("Image resource root group uses unsupported storage".into());
    }
    let children = raw_inherited_field(root, "_childGuids")?;
    for list in [members, map, children] {
        if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
            return Err("Image resource lists use shared storage".into());
        }
        values::list(graph, list, budget)?;
    }
    let root_guid = required(graph, root, "guid")?;
    if !root_guid.has_tag_name("CResourceGroupGuid") || root_guid.parent() != Some(shared)
        || root_guid.attribute("uuid").is_none() || root_guid.has_children() {
        return Err("Image resource root GUID uses unsupported storage".into());
    }
    direct_reference(graph, raw_inherited_field(root, "guid")?, root_guid)?;
    direct_reference(graph, raw_inherited_field(root, "_resourceManager")?, manager)?;
    for member in members.children().filter(Node::is_element) {
        let donor = graph.resolve(member)?;
        let Some(reference) = graph.field(donor, "resourceRef")? else { continue };
        if !reference.has_tag_name("CResource_Linked_Image") { continue; }
        direct_reference(graph, member, donor)?;
        if member.attributes().count() != 1 || !donor.has_tag_name("CResourceData")
            || donor.parent() != Some(shared) || reference != raw_inherited_field(donor, "resourceRef")? {
            return Err("Image resource donor uses unsupported storage".into());
        }
        reject_nested_shared_storage(donor, budget)?;
        if let Some(custom) = graph.field(donor, "customName")? {
            if !custom.has_tag_name("null") || custom != raw_inherited_field(donor, "customName")? {
                return Err("Image resource donor has an unsupported custom name".into());
            }
        }
        let parent = raw_inherited_field(donor, "parentGuid")?;
        let manager_ref = raw_inherited_field(donor, "_resourceManager")?;
        let guid_ref = raw_inherited_field(reference, "guid")?;
        let guid = graph.resolve(guid_ref)?;
        direct_reference(graph, parent, root_guid)?;
        direct_reference(graph, manager_ref, manager)?;
        direct_reference(graph, guid_ref, guid)?;
        if !guid.has_tag_name("CResourceGuid") || guid.parent() != Some(shared)
            || guid.attribute("uuid").is_none() || guid.has_children() {
            return Err("Image resource GUID uses unsupported storage".into());
        }
        for (key, tag) in [("name", "s"), ("srcFile", "file")] {
            let field = raw_inherited_field(reference, key)?;
            if !field.has_tag_name(tag) || field.attribute("xs.id").is_some() || field.attribute("xs.ref").is_some() {
                return Err("Image resource strings use unsupported storage".into());
            }
        }
        for node in donor.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
            if ![parent, manager_ref, guid_ref].contains(&node) {
                return Err("Image resource donor contains an unclassified reference".into());
            }
        }
        if donor.descendants().any(|node| node.attribute("uuid").is_some()) {
            return Err("Image resource donor has an unclassified private UUID".into());
        }
        let member_count = members.children().filter(Node::is_element)
            .filter(|node| node.attribute("xs.ref") == donor.attribute("xs.id")).count();
        let mut mapped = 0;
        for entry in map.children().filter(Node::is_element) {
            let key = raw_field(entry, "key")?;
            let value = raw_field(entry, "value")?;
            if graph.resolve(key)? == guid || graph.resolve(value)? == donor {
                if entry.attributes().len() != 0 || entry.children().filter(Node::is_element).count() != 2 {
                    return Err("Image resource map entry uses unsupported storage".into());
                }
                direct_reference(graph, key, guid)?;
                direct_reference(graph, value, donor)?;
                mapped += 1;
            }
        }
        let mut grouped = 0;
        for child in children.children().filter(Node::is_element) {
            if graph.resolve(child)? == guid {
                direct_reference(graph, child, guid)?;
                if child.attributes().count() != 1 { return Err("Image resource group member is aliased".into()); }
                grouped += 1;
            }
        }
        if (member_count, mapped, grouped) != (1, 1, 1) {
            return Err("Image resource donor has inconsistent memberships".into());
        }
        for node in manager.document().descendants() {
            if let Some(index) = node.attribute("xs.idx") {
                index.parse::<u64>().map_err(|_| "invalid resource serial number")?;
            }
        }
        return Ok(donor);
    }
    Err("the file has no linked-image resource entry to copy for a new resource".into())
}

fn image_resource_path_patch(
    text: &str, graph: &Graph<'_, '_>, animation: Node<'_, '_>, resource: &Value, budget: &mut Budget,
) -> Result<(Range<usize>, String), String> {
    let manager = required(graph, animation, "resourceManager")?;
    for entry in list(graph, manager, "_resourceRefList", budget)? {
        let reference = required(graph, entry, "resourceRef")?;
        if !reference.has_tag_name("CResource_Linked_Image") || guid(graph, reference, budget)? != resource["guid"] {
            continue;
        }
        if reference != raw_inherited_field(entry, "resourceRef")?
            || reference.attribute("xs.id").is_some() || reference.attribute("xs.ref").is_some() {
            return Err("Image resource link uses shared or aliased storage".into());
        }
        let field = raw_inherited_field(reference, "srcFile")?;
        if !matches!(field.tag_name().name(), "s" | "file")
            || field.attribute("xs.id").is_some() || field.attribute("xs.ref").is_some() {
            return Err("Image resource path uses shared or aliased storage".into());
        }
        return animation_clone::string_patch(text, field, resource["sourceFile"].as_str()
            .ok_or("Image resource sourceFile must be a string")?);
    }
    Err("Image resource is missing from native storage".into())
}

/// Writes each resource `model` adds to `initial` as a clone of the file's own resource entry, listed in the
/// manager's reference list, GUID map and root group as Cubism lists them. Other resource changes are refused.
#[allow(clippy::too_many_arguments)]
fn resource_patches(
    text: &str,
    graph: &Graph<'_, '_>,
    animation: Node<'_, '_>,
    initial: &Value,
    model: &Value,
    additions: &mut NativeAdditions,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
) -> Result<(), String> {
    let mut pending = Vec::new();
    let mut guids = HashSet::new();
    for kind in [ResourceKind::Model, ResourceKind::Image] {
        let old = initial[kind.field()].as_array().ok_or("animation resources missing")?;
        let new = model[kind.field()].as_array().ok_or("animation resources missing")?;
        if new.len() < old.len() { return Err(ONLY_EDITS.into()); }
        for (before, after) in old.iter().zip(new) {
            if before == after { continue; }
            let mut allowed = before.clone();
            allowed["sourceFile"] = after["sourceFile"].clone();
            if kind != ResourceKind::Image || before["sourceFile"].as_str().is_none() || allowed != *after {
                return Err(ONLY_EDITS.into());
            }
            patches.push(image_resource_path_patch(text, graph, animation, after, budget)?);
        }
        for resource in new {
            let guid = resource["guid"].as_str().filter(|guid| !guid.is_empty())
                .ok_or("animation resource needs a GUID")?;
            if !guids.insert(guid) { return Err("duplicate animation resource GUID".into()); }
        }
        pending.extend(new[old.len()..].iter().map(|resource| (kind, resource)));
    }
    if pending.is_empty() { return Ok(()); }
    let added = pending.len();
    let (mut member_xml, mut entry_xml, mut group_xml) = (String::new(), String::new(), String::new());
    for (kind, resource) in pending {
        let guid = resource["guid"].as_str().filter(|guid| !guid.is_empty()).ok_or("a new animation resource needs a GUID")?;
        let storage = free_storage_id(&mut additions.reserved, &format!("soul2d-resource-{guid}"));
        let donor = match kind {
            ResourceKind::Model => animation_clone::model_resource_donor(animation)
                .ok_or("the file has no linked-model resource entry to copy for a new resource")?,
            ResourceKind::Image => image_resource_donor(graph, animation, budget)?,
        };
        let clone = animation_clone::resource(text, donor, resource, &storage, additions)?;
        additions.definitions.push_str(&clone.definitions);
        member_xml.push_str(&format!(r#"<CResourceData xs.ref="{}"/>"#, escape(&storage)));
        entry_xml.push_str(&format!(r#"<entry><CResourceGuid xs.n="key" xs.ref="{}"/><CResourceData xs.n="value" xs.ref="{}"/></entry>"#,
            escape(&clone.guid_id), escape(&storage)));
        group_xml.push_str(&format!(r#"<CResourceGuid xs.ref="{}"/>"#, escape(&clone.guid_id)));
    }
    let manager = required(graph, animation, "resourceManager")?;
    let members = raw_field(manager, "_resourceRefList")?;
    let map = raw_field(manager, "resourceGuidMap")?;
    let group_list = match graph.field(manager, "rootGroup")? {
        Some(group) if !group.has_tag_name("null") => Some(raw_field(group, "_childGuids")?),
        _ => None,
    };
    for list in [Some(members), Some(map), group_list].into_iter().flatten() {
        if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
            return Err("animation resource lists use shared storage".into());
        }
        values::list(graph, list, budget)?;
    }
    for (list, xml) in [(Some(members), member_xml), (Some(map), entry_xml), (group_list, group_xml)] {
        let Some(list) = list else { continue };
        let existing: Vec<Node> = list.children().filter(Node::is_element).collect();
        patches.push(count_patch(list, existing.len() + added)?);
        let end = list_end(list, text)?;
        emit_inserts(patches, [None].into_iter(), &existing, &end, [xml].into_iter());
    }
    Ok(())
}

/// Writes each added scene as a clone of a scene the file stores, or else as a new shared scene holding only the
/// fields `read` takes, and lists it in the project's `_scenes`.
fn add_scene_refs<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    animation: Node<'d, 'i>,
    added: &[&Value],
    budget: &mut Budget,
    additions: &mut NativeAdditions,
) -> Result<Vec<SceneReference>, String> {
    let mut refs = Vec::new();
    if added.is_empty() {
        return Ok(refs);
    }
    let animation_id = animation
        .attribute("xs.id")
        .map(escape)
        .ok_or("the animation project has no shared identity a new scene can name")?;
    let existing = list(graph, animation, "_scenes", budget)?;
    // ponytail: element names copy the first scene, or are fixed guesses in a project without
    // one (so is the tag's place after the name); Cubism Editor has not read a scene written here, and real scenes
    // hold more fields.
    let tag = |path: &[&str], fallback: &str| -> String {
        let mut node = existing.first().copied();
        for name in path {
            node = node.and_then(|n| n.children().find(|c| c.is_element() && c.attribute("xs.n") == Some(name)));
        }
        node.map_or(fallback, |n| n.tag_name().name()).to_owned()
    };
    let (guid_tag, movie_tag, set_tag) = (tag(&["guid"], "CSceneGuid"), tag(&["movieInfo"], "MovieInfo"),
        tag(&["trackSourceSet"], "CTrackSourceSet"));
    let fps_tag = if tag(&["movieInfo", "fps"], "f") == "d" { "d" } else { "f" };
    for scene in added {
        let guid = scene["guid"]
            .as_str()
            .filter(|guid| !guid.is_empty() && guid.trim() == *guid)
            .ok_or("a new animation scene needs a GUID")?;
        let id = free_storage_id(&mut additions.reserved, &format!("soul2d-scene-{guid}"));
        let storage_id = id.clone();
        let invalid = || "a new animation scene has an invalid setting".to_owned();
        let name = escape(scene["name"].as_str().ok_or_else(invalid)?);
        let scene_tag = scene.get("tag")
            .map(|text| text.as_str().map(|text| tag_xml("s", text)).ok_or_else(invalid))
            .transpose()?
            .unwrap_or_default();
        let fps = scene["fps"].as_f64().ok_or_else(invalid)?;
        let duration = scene["durationFrames"].as_i64().ok_or_else(invalid)?;
        let looping = scene.get("loop").map(|flag| flag.as_bool().map(|flag| format!(r#"<b xs.n="isLoopMotion">{flag}</b>"#)))
            .unwrap_or(Some(String::new())).ok_or_else(invalid)?;
        let tracks = scene["tracks"].as_array().ok_or("animation tracks missing")?;
        if !scene["imageTracks"].as_array().ok_or("animation image tracks missing")?.is_empty() {
            return Err("new animation scenes cannot contain image tracks".into());
        }
        if !scene["soundTracks"].as_array().ok_or("animation sound tracks missing")?.is_empty() {
            return Err("new animation scenes cannot contain sound tracks".into());
        }
        if !scene["guideTracks"].as_array().ok_or("animation guide tracks missing")?.is_empty() {
            return Err("new animation scenes cannot contain guide tracks".into());
        }
        if let Some(definitions) = animation_clone::scene(text, animation, scene, &id, additions)? {
            additions.definitions.push_str(&definitions);
            refs.push(SceneReference { reference: format!(r#"<CSceneSource xs.ref="{}"/>"#, escape(&id)), storage_id: id });
            continue;
        }
        let mut track_refs = String::new();
        let mut track_definitions = String::new();
        for track in tracks {
            let serialized = serialize_track(additions, &id, track)?;
            track_definitions.push_str(&serialized.definitions);
            track_refs.push_str(&serialized.reference);
        }
        let track_list = if tracks.is_empty() {
            r#"<carray_list xs.n="_sources" count="0"/>"#.to_owned()
        } else {
            format!(
                r#"<carray_list xs.n="_sources" count="{}">{track_refs}</carray_list>"#,
                tracks.len(),
            )
        };
        let (id, guid, animation_tag) = (escape(&id), escape(guid), animation.tag_name().name());
        additions.definitions.push_str(&format!(
            r#"<CSceneSource xs.id="{id}"><{guid_tag} xs.n="guid" uuid="{guid}"/><{animation_tag} xs.n="_animation" xs.ref="{animation_id}"/><s xs.n="sceneName">{name}</s>{scene_tag}<{movie_tag} xs.n="movieInfo"><{fps_tag} xs.n="fps">{fps}</{fps_tag}><i xs.n="duration">{duration}</i>{looping}</{movie_tag}><{set_tag} xs.n="trackSourceSet">{track_list}</{set_tag}></CSceneSource>"#
        ));
        additions.definitions.push_str(&track_definitions);
        refs.push(SceneReference { storage_id, reference: format!(r#"<CSceneSource xs.ref="{id}"/>"#) });
    }
    Ok(refs)
}

/// Writes one CCurveType again whole with a new type token; a shared or aliased one is refused.
fn curve_type_patch(
    node: Node<'_, '_>,
    sequence: Node<'_, '_>,
    value: &Value,
) -> Result<(Range<usize>, String), String> {
    let token = value
        .as_str()
        .filter(|token| CURVE_TYPES.contains(token))
        .ok_or("animation curve type must be SMOOTH, BEZIER, LINEAR or STEP")?;
    // A node outside the sequence sits under a shared element with `xs.id`, so it is refused here too.
    let owned = node
        .ancestors()
        .take_while(|ancestor| ancestor.id() != sequence.id())
        .chain([sequence])
        .all(|ancestor| ancestor.attribute("xs.ref").is_none() && ancestor.attribute("xs.id").is_none());
    if !owned {
        return Err("animation curve type uses shared or aliased storage".into());
    }
    if node.attributes().count() != 1 || node.attribute("v").is_none() || node.has_children() {
        return Err("animation curve type requires plain inline storage".into());
    }
    Ok((node.range(), format!(r#"<{} v="{token}"/>"#, node.tag_name().name())))
}

enum KeySlot {
    Existing { original: usize, candidate: usize },
    Insert { candidate: usize },
}

struct CurvePlan {
    ordered: Vec<KeySlot>,
    removed: Vec<usize>,
}

fn curve_plan(old_points: &[Value], new_points: &[Value]) -> Result<CurvePlan, String> {
    let mut map = HashMap::new();
    for (index, point) in old_points.iter().enumerate() {
        let frame = point["anchor"]["frame"]
            .as_i64()
            .ok_or("animation anchor frame missing")?;
        if map.insert(frame, index).is_some() {
            return Err("duplicate animation anchor frame".into());
        }
    }
    let mut used = vec![false; old_points.len()];
    let mut ordered = Vec::with_capacity(new_points.len());
    for (candidate, point) in new_points.iter().enumerate() {
        let frame = point["anchor"]["frame"]
            .as_i64()
            .ok_or("animation anchor frame missing")?;
        match map.get(&frame) {
            Some(&original) if !used[original] => {
                used[original] = true;
                ordered.push(KeySlot::Existing { original, candidate });
            }
            Some(_) => return Err("duplicate animation anchor frame".into()),
            None => ordered.push(KeySlot::Insert { candidate }),
        }
    }
    let removed = (0..old_points.len()).filter(|index| !used[*index]).collect();
    Ok(CurvePlan { ordered, removed })
}

fn plan_structural(plan: &CurvePlan, old_points: &[Value], new_points: &[Value]) -> bool {
    if !plan.removed.is_empty()
        || plan
            .ordered
            .iter()
            .any(|slot| matches!(slot, KeySlot::Insert { .. }))
    {
        return true;
    }
    let mut last = None;
    plan.ordered.iter().any(|slot| match slot {
        KeySlot::Existing { original, candidate } => {
            if last.replace(*original).map_or(false, |last| last >= *original) {
                return true;
            }
            let (old, new) = (&old_points[*original], &new_points[*candidate]);
            if old["corner"] != new["corner"] {
                return true;
            }
            ["previous", "next"].iter().any(|side| {
                old[side]["integerFrame"] != new[side]["integerFrame"]
                    || old[side]["positionOptimized"] != new[side]["positionOptimized"]
            })
        }
        _ => false,
    })
}

fn inline_items<'d, 'i>(
    list_node: Node<'d, 'i>,
    name: &str,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let items: Vec<_> = list_node.children().filter(Node::is_element).collect();
    for item in &items {
        if item.attribute("xs.ref").is_some() || item.attribute("xs.id").is_some() {
            return Err(format!("animation {name} slot is shared or aliased"));
        }
    }
    Ok(items)
}

fn count_patch(list_node: Node<'_, '_>, count: usize) -> Result<(Range<usize>, String), String> {
    let attribute = list_node
        .attribute_node("count")
        .ok_or("animation list lacks a count attribute")?;
    attribute
        .value()
        .parse::<usize>()
        .map_err(|_| "animation list count attribute is malformed")?;
    Ok((attribute.range_value(), count.to_string()))
}

/// One key in the layout Cubism writes; `float_frames` stores handle frames as `f`, as Cubism does, instead of `d`.
pub(crate) fn serialize_point(point: &Value, float_frames: bool) -> Result<String, String> {
    let frame = point["anchor"]["frame"]
        .as_i64()
        .ok_or("anchorFrame must be a signed integer")?;
    if !(i32::MIN as i64..=i32::MAX as i64).contains(&frame) {
        return Err("animation integer does not fit a signed 32-bit integer".into());
    }
    let value = finite(&point["anchor"]["value"])?;
    let corner = point["corner"]
        .as_bool()
        .ok_or("animation point lacks corner")?;
    let frame_tag = if float_frames { "f" } else { "d" };
    let mut controls = String::new();
    for (side, field) in [("next", "next"), ("previous", "prev")] {
        let control = &point[side];
        let pos = control["integerFrame"]
            .as_i64()
            .ok_or("animation control integer frame missing")?;
        if !(i32::MIN as i64..=i32::MAX as i64).contains(&pos) {
            return Err("animation integer does not fit a signed 32-bit integer".into());
        }
        let pos_f = finite(&control["frame"])?;
        if float_frames && !(pos_f as f32).is_finite() {
            return Err("animation float does not fit finite 32-bit storage".into());
        }
        let double = finite(&control["value"])?;
        let optimized = control["positionOptimized"]
            .as_bool()
            .ok_or("animation control lacks positionOptimized")?;
        controls.push_str(&format!(
            r#"<CBezierCtrlPt xs.n="{field}"><{frame_tag} xs.n="posF">{pos_f}</{frame_tag}><i xs.n="pos">{pos}</i><d xs.n="doubleValue">{double}</d><b xs.n="isPosOptimized">{optimized}</b></CBezierCtrlPt>"#
        ));
    }
    Ok(format!(
        r#"<CBezierPt><CSeqPt xs.n="anchor"><b xs.n="isCorner">{corner}</b><i xs.n="pos">{frame}</i><d xs.n="doubleValue">{value}</d></CSeqPt>{controls}</CBezierPt>"#
    ))
}

fn check_new_pairs(old_points: &[Value], target: &Value) -> Result<(), String> {
    let frame = |point: &Value| point["anchor"]["frame"].as_i64().unwrap_or(i64::MIN);
    let pairs: HashSet<(i64, i64)> = old_points
        .windows(2)
        .map(|pair| (frame(&pair[0]), frame(&pair[1])))
        .collect();
    let new_points = target["points"].as_array().ok_or("animation points missing")?;
    for segment in 0..new_points.len().saturating_sub(1) {
        let pair = (frame(&new_points[segment]), frame(&new_points[segment + 1]));
        if !pairs.contains(&pair) {
            check_segment(target, segment)?;
        }
    }
    Ok(())
}

fn extrema(points: &[Value], minimum: bool) -> Result<f64, String> {
    points
        .iter()
        .map(|point| finite(&point["anchor"]["value"]))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .reduce(|a, b| if minimum { a.min(b) } else { a.max(b) })
        .ok_or("empty animation curve cannot be edited".into())
}

fn point_field_patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    point: Node<'d, 'i>,
    sequence: Node<'d, 'i>,
    old: &Value,
    new: &Value,
    patches: &mut Vec<(Range<usize>, String)>,
) -> Result<(), String> {
    let anchor = required(graph, point, "anchor")?;
    if old["anchor"]["value"] != new["anchor"]["value"] {
        patches.push(scalar_patch(
            text,
            required(graph, anchor, "doubleValue")?,
            sequence,
            finite(&old["anchor"]["value"])?,
            finite(&new["anchor"]["value"])?,
        )?);
    }
    if old["corner"] != new["corner"] {
        patches.push(native_scalar_patch(
            text,
            required(graph, anchor, "isCorner")?,
            sequence,
            NativeScalar::Boolean(
                old["corner"].as_bool().ok_or("animation point lacks corner")?,
            ),
            NativeScalar::Boolean(
                new["corner"].as_bool().ok_or("animation point lacks corner")?,
            ),
        )?);
    }
    for (side, field) in [("previous", "prev"), ("next", "next")] {
        if old[side] == new[side] {
            continue;
        }
        let control = required(graph, point, field)?;
        if old[side]["frame"] != new[side]["frame"] {
            patches.push(scalar_patch(
                text,
                required(graph, control, "posF")?,
                sequence,
                finite(&old[side]["frame"])?,
                finite(&new[side]["frame"])?,
            )?);
        }
        if old[side]["integerFrame"] != new[side]["integerFrame"] {
            patches.push(native_scalar_patch(
                text,
                required(graph, control, "pos")?,
                sequence,
                NativeScalar::Integer(
                    old[side]["integerFrame"]
                        .as_i64()
                        .ok_or("animation control integer frame missing")?,
                ),
                NativeScalar::Integer(
                    new[side]["integerFrame"]
                        .as_i64()
                        .ok_or("animation control integer frame missing")?,
                ),
            )?);
        }
        if old[side]["value"] != new[side]["value"] {
            patches.push(scalar_patch(
                text,
                required(graph, control, "doubleValue")?,
                sequence,
                finite(&old[side]["value"])?,
                finite(&new[side]["value"])?,
            )?);
        }
        if old[side]["positionOptimized"] != new[side]["positionOptimized"] {
            patches.push(native_scalar_patch(
                text,
                required(graph, control, "isPosOptimized")?,
                sequence,
                NativeScalar::Boolean(
                    old[side]["positionOptimized"]
                        .as_bool()
                        .ok_or("animation control lacks positionOptimized")?,
                ),
                NativeScalar::Boolean(
                    new[side]["positionOptimized"]
                        .as_bool()
                        .ok_or("animation control lacks positionOptimized")?,
                ),
            )?);
        }
    }
    Ok(())
}

fn structural_curve_patches<'d, 'i>(
    text: &str,
    graph: &Graph<'d, 'i>,
    attribute: Node<'d, 'i>,
    old_curve: &Value,
    target: &Value,
    plan: &CurvePlan,
    patches: &mut Vec<(Range<usize>, String)>,
) -> Result<(), String> {
    let sequence = inline_field(attribute, "valueData")?;
    if !sequence.has_tag_name("CMutableSequence") {
        return Err("animation channel lacks a mutable sequence".into());
    }
    let old_points = old_curve["points"]
        .as_array()
        .ok_or("animation points missing")?;
    let old_types = old_curve["curveTypes"]
        .as_array()
        .ok_or("animation curve lacks curve types")?;
    let new_points = target["points"].as_array().ok_or("animation points missing")?;
    let new_types = target["curveTypes"]
        .as_array()
        .ok_or("animation curve lacks curve types")?;
    if new_types.len() != new_points.len() {
        return Err("animation point and curve-type counts differ".into());
    }
    if !new_types
        .iter()
        .all(|kind| kind.as_str().map_or(false, |kind| CURVE_TYPES.contains(&kind)))
    {
        return Err("animation curve type must be SMOOTH, BEZIER, LINEAR or STEP".into());
    }
    let points_list = inline_field(sequence, "points")?;
    let types_list = inline_field(sequence, "curveTypes")?;
    let point_items = inline_items(points_list, "points")?;
    let type_items = inline_items(types_list, "curveTypes")?;
    if point_items.len() != old_points.len() || type_items.len() != old_points.len() {
        return Err("animation source point count differs".into());
    }
    let mut last = None;
    for slot in &plan.ordered {
        if let KeySlot::Existing { original, .. } = slot {
            if last.replace(*original).map_or(false, |last| last >= *original) {
                return Err("animation key order differs from source".into());
            }
        }
    }
    for &removed in &plan.removed {
        if point_items[removed]
            .descendants()
            .any(|node| node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some())
            || type_items[removed]
                .descendants()
                .any(|node| node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some())
        {
            return Err("animation removed key storage is shared or referenced".into());
        }
    }
    if !old_points.is_empty() {
        for (name, minimum) in [("curMin", true), ("curMax", false)] {
            if number(graph, sequence, name)? != extrema(old_points, minimum)? {
                return Err(format!("animation {name} cache differs from anchor extrema"));
            }
        }
    }
    check_new_pairs(old_points, target)?;
    if !new_points.is_empty() {
        for (name, minimum) in [("curMin", true), ("curMax", false)] {
            let field = required(graph, sequence, name)?;
            let actual = number(graph, sequence, name)?;
            let wanted = extrema(new_points, minimum)?;
            if actual != wanted {
                patches.push(scalar_patch(text, field, sequence, actual, wanted)?);
            }
        }
    }
    if new_points.len() != old_points.len() {
        patches.push(count_patch(points_list, new_points.len())?);
        patches.push(count_patch(types_list, new_points.len())?);
    }
    for &removed in &plan.removed {
        patches.push((point_items[removed].range(), String::new()));
        patches.push((type_items[removed].range(), String::new()));
    }
    let points_end = list_end(points_list, text)?;
    let types_end = list_end(types_list, text)?;
    let float_frames = match point_items.first() {
        Some(point) => required(graph, required(graph, *point, "next")?, "posF")?.has_tag_name("f"),
        None => true,
    };
    let mut point_inserts = Vec::new();
    let mut type_inserts = Vec::new();
    for slot in plan.ordered.iter() {
        match slot {
            KeySlot::Existing { original, candidate } => {
                point_field_patches(
                    text,
                    graph,
                    point_items[*original],
                    sequence,
                    &old_points[*original],
                    &new_points[*candidate],
                    patches,
                )?;
                if old_types[*original] != new_types[*candidate] {
                    patches.push(curve_type_patch(
                        type_items[*original],
                        sequence,
                        &new_types[*candidate],
                    )?);
                }
            }
            KeySlot::Insert { candidate } => {
                let kind = new_types[*candidate]
                    .as_str()
                    .ok_or("animation curve type must be SMOOTH, BEZIER, LINEAR or STEP")?;
                point_inserts.push(serialize_point(&new_points[*candidate], float_frames)?);
                type_inserts.push(format!(r#"<CCurveType v="{kind}"/>"#));
            }
        }
    }
    let slots = plan.ordered.iter().map(|slot| match slot {
        KeySlot::Existing { original, .. } => Some(*original),
        KeySlot::Insert { .. } => None,
    });
    emit_inserts(patches, slots.clone(), &point_items, &points_end, point_inserts.into_iter());
    emit_inserts(patches, slots, &type_items, &types_end, type_inserts.into_iter());
    Ok(())
}

enum ListEnd {
    Inner(usize),
    Empty { range: Range<usize>, tag: String },
}

fn list_end(list_node: Node<'_, '_>, text: &str) -> Result<ListEnd, String> {
    let range = list_node.range();
    let raw = text
        .get(range.clone())
        .ok_or("animation list range is invalid")?;
    if raw.ends_with("/>") {
        return Ok(ListEnd::Empty {
            range: range.end - 2..range.end,
            tag: list_node.tag_name().name().to_owned(),
        });
    }
    let tag = list_node.tag_name().name();
    let close = format!("</{tag}>");
    if !raw.ends_with(&close) {
        return Err(format!("animation {tag} list is malformed"));
    }
    Ok(ListEnd::Inner(range.end - close.len()))
}

fn topology_count_patch(
    list_node: Node<'_, '_>,
    count: usize,
) -> Result<(Range<usize>, String), String> {
    let attribute = list_node
        .attribute("count")
        .ok_or("animation list lacks a count attribute")?;
    let (range, _) = count_patch(list_node, count)?;
    let value = if attribute.len() > 1 && attribute.starts_with('0') {
        format!("{:0>width$}", count, width = attribute.len())
    } else {
        count.to_string()
    };
    Ok((range, value))
}

fn emit_inserts<'d, 'i>(
    patches: &mut Vec<(Range<usize>, String)>,
    slots: impl Iterator<Item = Option<usize>>,
    items: &[Node<'d, 'i>],
    end: &ListEnd,
    mut inserts: impl Iterator<Item = String>,
) {
    let mut pending = String::new();
    for slot in slots {
        match slot {
            None => pending.push_str(&inserts.next().expect("insert payload")),
            Some(original) => {
                if !pending.is_empty() {
                    let at = items[original].range().start;
                    patches.push((at..at, std::mem::take(&mut pending)));
                }
            }
        }
    }
    if !pending.is_empty() {
        match end {
            ListEnd::Inner(at) => patches.push((*at..*at, pending)),
            ListEnd::Empty { range, tag } => {
                patches.push((range.clone(), format!(">{pending}</{tag}>")))
            }
        }
    }
}

pub(crate) fn read(container: &CaffDocument) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("animation XML is not UTF-8: {error}"))?;
    read_xml(text)
}

pub(crate) fn is_project(container: &CaffDocument) -> Result<bool, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let document = xml_document(text)?;
    let graph = Graph::new(&document)?;
    for main in document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name("main"))
    {
        for entry in main.children().filter(Node::is_element) {
            if graph.resolve(entry)?.has_tag_name("CAnimation") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// One key takes about 27 nodes (issue #149): a scene keying 100 parameters on 600 frames needs 1,620,000, and the
// 64 MiB text cap of `authoring_values` would allow about 3,800,000 at 7.2 GB of peak memory on a debug build.
const MAX_XML_NODES: u32 = 2_000_000;

fn xml_document(text: &str) -> Result<Document<'_>, String> {
    xml_document_with_limit(text, MAX_XML_NODES)
}

fn xml_document_with_limit(text: &str, nodes_limit: u32) -> Result<Document<'_>, String> {
    Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit,
            ..Default::default()
        },
    )
    .map_err(|error| format!("invalid animation XML: {error}"))
}

fn read_xml(text: &str) -> Result<Value, String> {
    let document = xml_document(text)?;
    let root = document.root_element();
    if !root.has_tag_name("root") {
        return Err("animation XML root must be root".into());
    }
    let graph = Graph::new(&document)?;
    let mut budget = Budget::new(5_000_000);
    let mains: Vec<_> = root
        .children()
        .filter(|node| node.has_tag_name("main"))
        .collect();
    if mains.len() != 1 {
        return Err("animation source requires one main section".into());
    }
    let entries: Vec<_> = mains[0].children().filter(Node::is_element).collect();
    if entries.len() != 1 {
        return Err("animation main requires one CAnimation reference".into());
    }
    let animation = graph.resolve(entries[0])?;
    if !animation.has_tag_name("CAnimation") || graph.model()?.is_some() {
        return Err("animation source requires a distinct CAnimation project".into());
    }
    let version = root
        .attribute("fileFormatVersion")
        .ok_or("animation lacks fileFormatVersion")?
        .parse::<u64>()
        .map_err(|_| "invalid animation fileFormatVersion")?;
    let resource_manager = required(&graph, animation, "resourceManager")?;
    let mut resource_ids = HashSet::new();
    let mut image_resource_ids = HashSet::new();
    let mut sound_resource_ids = HashSet::new();
    let mut guide_resource_ids = HashSet::new();
    let mut resources = Vec::new();
    let mut image_resources = Vec::new();
    for entry in list(&graph, resource_manager, "_resourceRefList", &mut budget)? {
        let resource = required(&graph, entry, "resourceRef")?;
        if resource.has_tag_name("CResource_Linked_GuideImages") {
            let id = guid(&graph, resource, &mut budget)?;
            if resource_ids.contains(&id) || image_resource_ids.contains(&id) || sound_resource_ids.contains(&id)
                || !guide_resource_ids.insert(id) {
                return Err("duplicate animation guide resource GUID".into());
            }
            continue;
        }
        if resource.has_tag_name("CResource_Linked_Sound") {
            let id = guid(&graph, resource, &mut budget)?;
            if resource_ids.contains(&id) || image_resource_ids.contains(&id) || guide_resource_ids.contains(&id)
                || !sound_resource_ids.insert(id) {
                return Err("duplicate animation sound resource GUID".into());
            }
            continue;
        }
        if resource.has_tag_name("CResource_Linked_Image") {
            let id = guid(&graph, resource, &mut budget)?;
            if resource_ids.contains(&id) || sound_resource_ids.contains(&id) || guide_resource_ids.contains(&id)
                || !image_resource_ids.insert(id.clone()) {
                return Err("duplicate animation image resource GUID".into());
            }
            let mut image = json!({"guid": id});
            for (field, key) in [("name", "name"), ("srcFile", "sourceFile")] {
                if graph.field(resource, field)?.is_some() {
                    image[key] = json!(string(&graph, resource, field, &mut budget)?);
                }
            }
            image_resources.push(image);
            continue;
        }
        if !resource.has_tag_name("CResource_Linked_Model") {
            continue;
        }
        let id = guid(&graph, resource, &mut budget)?;
        if image_resource_ids.contains(&id) || sound_resource_ids.contains(&id) || guide_resource_ids.contains(&id)
            || !resource_ids.insert(id.clone()) {
            return Err("duplicate animation model resource GUID".into());
        }
        resources.push(json!({
            "guid": id,
            "name": string(&graph, resource, "name", &mut budget)?,
            "sourceFile": string(&graph, resource, "srcFile", &mut budget)?,
        }));
    }
    let mut scene_ids = HashSet::new();
    let mut track_ids = HashSet::new();
    let mut scenes = Vec::new();
    for scene in list(&graph, animation, "_scenes", &mut budget)? {
        if !scene.has_tag_name("CSceneSource") {
            return Err("animation scene list contains a non-scene".into());
        }
        let scene_guid = guid(&graph, scene, &mut budget)?;
        if !scene_ids.insert(scene_guid.clone()) {
            return Err("duplicate animation scene GUID".into());
        }
        if required(&graph, scene, "_animation")?.id() != animation.id() {
            return Err("animation scene project backlink differs from its owner".into());
        }
        let movie = required(&graph, scene, "movieInfo")?;
        let fps = number(&graph, movie, "fps")?;
        let duration = integer(&graph, movie, "duration")?;
        if fps <= 0.0 || duration < 0 {
            return Err("animation FPS must be positive and duration nonnegative".into());
        }
        let set = required(&graph, scene, "trackSourceSet")?;
        let mut tracks = Vec::new();
        let mut image_tracks = Vec::new();
        let mut sound_tracks = Vec::new();
        let mut guide_tracks = Vec::new();
        for track in list(&graph, set, "_sources", &mut budget)? {
            if track.has_tag_name("CMvTrack_GuideImage_Source") {
                let track_guid = guid(&graph, track, &mut budget)?;
                if !track_ids.insert(track_guid.clone()) { return Err("duplicate animation track GUID".into()); }
                if required(&graph, track, "_sceneSource")? != scene {
                    return Err("animation track scene backlink differs from its owner".into());
                }
                let resource = required(&graph, track, "_resourceGuid")?.attribute("uuid")
                    .ok_or("animation track lacks resource UUID")?;
                if !guide_resource_ids.contains(resource) { return Err("animation track guide resource was not found".into()); }
                guide_tracks.push(json!({"guid":track_guid,"name":string(&graph, track, "name", &mut budget)?,
                    "resourceGuid":budget.text(resource)?,"start":integer(&graph, track, "start")?,
                    "internalOffset":integer(&graph, track, "internalOffset")?,"duration":integer(&graph, track, "duration")?,
                    "visualCurves":visual_scalar_curves(&graph, track, &mut budget)?,
                    "pointChannels":visual_point_channels(&graph, track, &mut budget)?}));
                continue;
            }
            if track.has_tag_name("CMvTrack_JavaSoundTrack_Source") {
                let track_guid = guid(&graph, track, &mut budget)?;
                if !track_ids.insert(track_guid.clone()) { return Err("duplicate animation track GUID".into()); }
                if required(&graph, track, "_sceneSource")? != scene {
                    return Err("animation track scene backlink differs from its owner".into());
                }
                let resource = required(&graph, track, "_resourceGuid")?.attribute("uuid")
                    .ok_or("animation track lacks resource UUID")?;
                if !sound_resource_ids.contains(resource) { return Err("animation track sound resource was not found".into()); }
                sound_tracks.push(json!({"guid":track_guid,"name":string(&graph, track, "name", &mut budget)?,
                    "resourceGuid":budget.text(resource)?,"start":integer(&graph, track, "start")?,
                    "internalOffset":integer(&graph, track, "internalOffset")?,"duration":integer(&graph, track, "duration")?}));
                continue;
            }
            if track.has_tag_name("CMvTrack_Image_Source") {
                let track_guid = guid(&graph, track, &mut budget)?;
                if !track_ids.insert(track_guid.clone()) {
                    return Err("duplicate animation track GUID".into());
                }
                if required(&graph, track, "_sceneSource")?.id() != scene.id() {
                    return Err("animation track scene backlink differs from its owner".into());
                }
                let resource = required(&graph, track, "_resourceGuid")?
                    .attribute("uuid").ok_or("animation track lacks resource UUID")?;
                if !image_resource_ids.contains(resource) {
                    return Err("animation track image resource was not found".into());
                }
                image_tracks.push(json!({
                    "guid": track_guid, "name": string(&graph, track, "name", &mut budget)?,
                    "resourceGuid": budget.text(resource)?,
                    "start": integer(&graph, track, "start")?,
                    "internalOffset": integer(&graph, track, "internalOffset")?,
                    "duration": integer(&graph, track, "duration")?,
                    "visualCurves": visual_scalar_curves(&graph, track, &mut budget)?,
                    "pointChannels": visual_point_channels(&graph, track, &mut budget)?,
                }));
                continue;
            }
            if !track.has_tag_name("CMvTrack_Live2DModel_Source") {
                continue;
            }
            let track_guid = guid(&graph, track, &mut budget)?;
            if !track_ids.insert(track_guid.clone()) {
                return Err("duplicate animation model-track GUID".into());
            }
            if required(&graph, track, "_sceneSource")?.id() != scene.id() {
                return Err("animation track scene backlink differs from its owner".into());
            }
            let resource = required(&graph, track, "_resourceGuid")?
                .attribute("uuid")
                .ok_or("animation track lacks resource UUID")?;
            if !resource_ids.contains(resource) {
                return Err("animation track model resource was not found".into());
            }
            let effect = required(&graph, track, "keyParamEffect")?;
            if required(&graph, effect, "track")?.id() != track.id() {
                return Err("animation effect track backlink differs from its owner".into());
            }
            let mut channel_ids = HashSet::new();
            let mut curves = Vec::new();
            for attribute in list(&graph, effect, "attrList", &mut budget)? {
                let id = required(&graph, attribute, "id")?
                    .attribute("idstr")
                    .ok_or("animation attribute lacks an ID")?;
                let Some(parameter) = id.strip_prefix("live2dParam_").filter(|id| !id.is_empty())
                else {
                    continue;
                };
                if !attribute.has_tag_name("CMvAttrF") || !channel_ids.insert(id.to_owned()) {
                    return Err("unsupported or duplicate animation parameter attribute".into());
                }
                if required(&graph, attribute, "track")?.id() != track.id() {
                    return Err("animation parameter track backlink differs from its owner".into());
                }
                let (points, types) = sequence_data(&graph, attribute, &mut budget)?;
                let parameter_guid = guid(&graph, attribute, &mut budget)?;
                let option_param = required(&graph, attribute, "optionParam")?;
                if !option_param.has_tag_name("hash_map") && !option_param.has_tag_name("COptionParam") {
                    return Err("unsupported animation parameter identity container".into());
                }
                let options = values::list(&graph, option_param, &mut budget)?;
                let source_parameter =
                    if option_param.has_tag_name("hash_map") && options.is_empty() && option_param.attribute("count") == Some("0") {
                        Value::Null
                    } else {
                        let source = required(&graph, option_param, "KEY_PARAM_ID")?;
                        let value = if source.has_tag_name("CParameterId") {
                            source
                                .attribute("idstr")
                                .ok_or("animation source parameter lacks idstr")?
                        } else if source.has_tag_name("s") {
                            source
                                .text()
                                .ok_or("animation source parameter lacks text")?
                        } else {
                            return Err("unsupported animation source parameter identity".into());
                        };
                        json!({
                            "type": source.tag_name().name(),
                            "value": budget.text(value)?,
                        })
                    };
                let mut curve = json!({
                    "id": budget.text(id)?, "parameterId": budget.text(parameter)?,
                    "parameterGuid": parameter_guid,
                    "sourceParameter": source_parameter,
                    "range": [number(&graph, attribute, "rangeMin")?, number(&graph, attribute, "rangeMax")?],
                    "points": points, "curveTypes": types,
                });
                // In the supplied files a stored baseValue equals the parameter's default in 3,586 of 3,586
                // curves; it is shown where the curve stores one.
                let sequence = required(&graph, attribute, "valueData")?;
                if graph.field(sequence, "baseValue")?.is_some() {
                    curve["baseValue"] = json!(number(&graph, sequence, "baseValue")?);
                }
                curves.push(curve);
            }
            let (part_curves, legacy_part_curves, underscore_part_curves, skipped_legacy_parts) =
                parts_visible_curves(&graph, track, &mut budget)?;
            tracks.push(json!({
                "guid": track_guid, "name": string(&graph, track, "name", &mut budget)?,
                "resourceGuid": budget.text(resource)?,
                "start": integer(&graph, track, "start")?,
                "internalOffset": integer(&graph, track, "internalOffset")?,
                "duration": integer(&graph, track, "duration")?,
                "parameterCurves": curves,
                "partOpacityCurves": part_curves,
                "legacyPartOpacityCurves": legacy_part_curves,
                "underscorePartOpacityCurves": underscore_part_curves,
                "skippedLegacyPartOpacityCount": skipped_legacy_parts,
            }));
        }
        let mut entry = json!({
            "guid": scene_guid, "name": string(&graph, scene, "sceneName", &mut budget)?,
            "fps": fps, "durationFrames": duration, "tracks": tracks, "imageTracks": image_tracks, "soundTracks": sound_tracks,
            "guideTracks": guide_tracks,
        });
        if graph.field(movie, "isLoopMotion")?.is_some() {
            entry["loop"] = json!(boolean(&graph, movie, "isLoopMotion")?);
        }
        // Every scene of the four supplied files stores a tag: 15 hold text and 34 an empty element.
        if graph.field(scene, "tag")?.is_some() {
            entry["tag"] = json!(string(&graph, scene, "tag", &mut budget)?);
        }
        scenes.push(entry);
    }
    Ok(json!({
        "format": "live2d.animation.source.v1", "fileFormatVersion": version,
        "name": string(&graph, animation, "name", &mut budget)?,
        "resources": resources, "imageResources": image_resources, "scenes": scenes,
    }))
}

fn required<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<Node<'d, 'i>, String> {
    values::required_field(graph, node, name)
}

fn guid(graph: &Graph<'_, '_>, node: Node<'_, '_>, budget: &mut Budget) -> Result<String, String> {
    budget.text(graph.guid(node)?.ok_or("animation source lacks a GUID")?)
}

fn string(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    budget: &mut Budget,
) -> Result<String, String> {
    budget.text(required(graph, node, name)?.text().unwrap_or(""))
}

fn number(graph: &Graph<'_, '_>, node: Node<'_, '_>, name: &str) -> Result<f64, String> {
    values::number_field(graph, node, name, None)
}

fn integer(graph: &Graph<'_, '_>, node: Node<'_, '_>, name: &str) -> Result<i64, String> {
    required(graph, node, name)?
        .text()
        .ok_or("animation integer lacks text")?
        .trim()
        .parse()
        .map_err(|_| "invalid animation integer".into())
}

fn boolean(graph: &Graph<'_, '_>, node: Node<'_, '_>, name: &str) -> Result<bool, String> {
    required(graph, node, name)?
        .text()
        .ok_or("animation boolean lacks text")?
        .trim()
        .parse()
        .map_err(|_| "invalid animation boolean".into())
}

fn control(graph: &Graph<'_, '_>, node: Node<'_, '_>) -> Result<Value, String> {
    if !node.has_tag_name("CBezierCtrlPt") {
        return Err("animation control is not CBezierCtrlPt".into());
    }
    Ok(
        json!({ "frame": number(graph, node, "posF")?, "integerFrame": integer(graph, node, "pos")?,
        "value": number(graph, node, "doubleValue")?, "positionOptimized": boolean(graph, node, "isPosOptimized")? }),
    )
}

const VISUAL_SCALAR_CHANNELS: [(&str, &str); 6] = [
    ("scalex", "attrScaleX"), ("scaley", "attrScaleY"), ("rotate", "attrRotate"),
    ("shear", "attrShear"), ("opacity", "attrOpacity"), ("artPathWidth", "attrArtPathWidth"),
];

const VISUAL_POINT_CHANNELS: [(&str, &str); 2] = [("xy", "attrXY"), ("anchor", "attrAnchorXY")];

fn visual_point_channels(graph: &Graph<'_, '_>, track: Node<'_, '_>, budget: &mut Budget) -> Result<Vec<Value>, String> {
    let attributes: Result<Vec<Node<'_, '_>>, String> = (|| {
        let effect = required(graph, track, "visualEffect")?;
        if !effect.has_tag_name("CMvEffect_VisualDefault") { return Err("unsupported Image visual effect".into()); }
        list(graph, effect, "attrList", budget)
    })();
    let attributes = match attributes {
        Ok(attributes) => attributes,
        Err(error) if budget.exhausted() => return Err(error),
        Err(_) => return Ok(Vec::new()),
    };
    let mut channels = Vec::new();
    for (id, _) in VISUAL_POINT_CHANNELS {
        let channel = (|| -> Result<Value, String> {
            let mut found = Vec::new();
            for &attribute in &attributes {
                budget.consume(1)?;
                if graph.field(attribute, "id").ok().flatten().and_then(|field| field.attribute("idstr")) == Some(id) {
                    found.push(attribute);
                }
            }
            if found.len() != 1 { return Err("Image point channel is missing or ambiguous".into()); }
            let attribute = found[0];
            if !attribute.has_tag_name("CMvAttrPt") || required(graph, attribute, "track")? != track {
                return Err("Image point channel has unsupported ownership".into());
            }
            let sequence = required(graph, attribute, "valueDataXY")?;
            if !sequence.has_tag_name("CXY_TNSSequence") || required(graph, sequence, "attr")? != attribute {
                return Err("Image point channel has unsupported sequence storage".into());
            }
            let mut frames = HashSet::new();
            let mut points = Vec::new();
            for point in list(graph, sequence, "points", budget)? {
                if !point.has_tag_name("CPtTNS") { return Err("Image point is not CPtTNS".into()); }
                let frame = integer(graph, point, "pos")?;
                if !frames.insert(frame) { return Err("duplicate Image point frame".into()); }
                let vectors = [required(graph, point, "super")?, required(graph, point, "value")?];
                let mut values = [[0.0; 2]; 2];
                for (coordinates, vector) in values.iter_mut().zip(vectors) {
                    if !vector.has_tag_name("GVector2") { return Err("Image point vector is not GVector2".into()); }
                    for (coordinate, axis) in coordinates.iter_mut().zip(["x", "y"]) {
                        if !required(graph, vector, axis)?.has_tag_name("f") { return Err("Image coordinate is not f32".into()); }
                        let value = number(graph, vector, axis)?;
                        if !(value as f32).is_finite() { return Err("Image coordinate exceeds finite f32 storage".into()); }
                        *coordinate = value;
                    }
                }
                if values[0] != values[1] { return Err("Image point coordinate copies differ".into()); }
                points.push(json!({"frame":frame,"x":values[0][0],"y":values[0][1],"corner":boolean(graph, point, "isCorner")?}));
            }
            Ok(json!({"id":budget.text(id)?,"points":points}))
        })();
        match channel {
            Ok(channel) => channels.push(channel),
            Err(error) if budget.exhausted() => return Err(error),
            Err(_) => {}
        }
    }
    Ok(channels)
}

fn visual_scalar_curves<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, budget: &mut Budget,
) -> Result<Vec<Value>, String> {
    let attributes = (|| -> Result<Vec<Node<'d, 'i>>, String> {
        let effect = required(graph, track, "visualEffect")?;
        if !effect.has_tag_name("CMvEffect_VisualDefault") { return Err("unsupported Image visual effect".into()); }
        list(graph, effect, "attrList", budget)
    })();
    let attributes = match attributes {
        Ok(attributes) => attributes,
        Err(error) if budget.exhausted() => return Err(error),
        Err(_) => return Ok(Vec::new()),
    };
    let mut curves = Vec::new();
    for (id, _) in VISUAL_SCALAR_CHANNELS {
        let curve = (|| -> Result<Value, String> {
            let mut found = Vec::new();
            for &attribute in &attributes {
                budget.consume(1)?;
                if graph.field(attribute, "id").ok().flatten().and_then(|field| field.attribute("idstr")) == Some(id) {
                    found.push(attribute);
                }
            }
            if found.len() != 1 { return Err("Image scalar channel is missing or ambiguous".into()); }
            let attribute = found[0];
            if !attribute.has_tag_name("CMvAttrF") || required(graph, attribute, "track")? != track {
                return Err("Image scalar channel has unsupported ownership".into());
            }
            let (points, types) = sequence_data(graph, attribute, budget)?;
            Ok(json!({"id": budget.text(id)?, "points": points, "curveTypes": types}))
        })();
        match curve {
            Ok(curve) => curves.push(curve),
            Err(error) if budget.exhausted() => return Err(error),
            Err(_) => {}
        }
    }
    Ok(curves)
}

fn sequence_data(
    graph: &Graph<'_, '_>,
    attribute: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<(Vec<Value>, Vec<String>), String> {
    let sequence = required(graph, attribute, "valueData")?;
    if !sequence.has_tag_name("CMutableSequence") {
        return Err("animation channel lacks a mutable sequence".into());
    }
    if required(graph, sequence, "attr")?.id() != attribute.id() {
        return Err("animation sequence attribute backlink differs from its owner".into());
    }
    let mut frames = HashSet::new();
    let mut points = Vec::new();
    for point in list(graph, sequence, "points", budget)? {
        if !point.has_tag_name("CBezierPt") {
            return Err("animation point is not CBezierPt".into());
        }
        let anchor = required(graph, point, "anchor")?;
        if !anchor.has_tag_name("CSeqPt") {
            return Err("animation anchor is not CSeqPt".into());
        }
        let frame = integer(graph, anchor, "pos")?;
        if !frames.insert(frame) {
            return Err("duplicate animation anchor frame".into());
        }
        points.push(json!({
            "anchor": { "frame": frame, "value": number(graph, anchor, "doubleValue")? },
            "previous": control(graph, required(graph, point, "prev")?)?,
            "next": control(graph, required(graph, point, "next")?)?,
            "corner": boolean(graph, anchor, "isCorner")?,
        }));
    }
    let mut types = Vec::new();
    for curve_type in list(graph, sequence, "curveTypes", budget)? {
        if !curve_type.has_tag_name("CCurveType") {
            return Err("animation curve type is not CCurveType".into());
        }
        let value = curve_type
            .attribute("v")
            .ok_or("animation curve type lacks v")?;
        types.push(budget.text(value)?);
    }
    if types.len() != points.len() {
        return Err("animation point and curve-type counts differ".into());
    }
    Ok((points, types))
}

fn source_part_identity(
    graph: &Graph<'_, '_>,
    attribute: Node<'_, '_>,
    budget: &mut Budget,
    allow_other_options: bool,
) -> Result<(String, String, String), String> {
    let option = required(graph, attribute, "optionParam")?;
    if !option.has_tag_name("hash_map") {
        return Err("animation part identity is not a hash map".into());
    }
    let options = values::list(graph, option, budget)?;
    let raw_name = option.children().find(Node::is_element).and_then(|node| node.attribute("xs.n"));
    if !allow_other_options && (options.len() != 1 || raw_name != Some("KEY_PARTS_VISIBLE_ID")) {
        return Err("animation part identity requires KEY_PARTS_VISIBLE_ID".into());
    }
    let source = required(graph, option, "KEY_PARTS_VISIBLE_ID")?;
    let kind = source.tag_name().name();
    let value = match kind {
        "CPartId" => source
            .attribute("idstr")
            .ok_or("animation source part lacks idstr")?,
        "s" => source
            .text()
            .ok_or("animation source part lacks text")?,
        _ => return Err("unsupported animation source part identity".into()),
    };
    let part_id = value.trim();
    if part_id.is_empty() {
        return Err("animation source part ID is empty".into());
    }
    Ok((
        budget.text(part_id)?,
        budget.text(kind)?,
        budget.text(value)?,
    ))
}

fn parts_visible_curves(
    graph: &Graph<'_, '_>,
    track: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>, usize), String> {
    let Some(effect) = graph.field(track, "partsVisibleEffect")? else {
        return Ok((Vec::new(), Vec::new(), Vec::new(), 0));
    };
    if effect.has_tag_name("null") {
        return Ok((Vec::new(), Vec::new(), Vec::new(), 0));
    }
    if !effect.has_tag_name("CMvEffect_Live2DPartsVisible") {
        return Err("animation parts-visible effect has an unsupported type".into());
    }
    if required(graph, effect, "track")?.id() != track.id() {
        return Err("animation parts-visible track backlink differs from its owner".into());
    }
    let attr_map = required(graph, effect, "attrMap")?;
    if !attr_map.has_tag_name("hash_map") {
        return Err("animation parts-visible attributes lack a hash map".into());
    }
    let mut map_keys = HashSet::new();
    let mut channel_ids = HashSet::new();
    let mut part_ids = HashSet::new();
    let mut part_guids = HashSet::new();
    let mut legacy_channel_ids = HashSet::new();
    let mut legacy_part_guids = HashSet::new();
    let mut curves = Vec::new();
    let mut legacy_curves = Vec::new();
    let mut underscore_curves = Vec::new();
    let mut skipped_legacy_count = 0;
    for entry in values::list(graph, attr_map, budget)? {
        if !entry.has_tag_name("entry") {
            return Err("animation parts-visible map contains a non-entry".into());
        }
        let key = required(graph, entry, "key")?;
        if !key.has_tag_name("CAttrId") {
            return Err("animation parts-visible map key is not CAttrId".into());
        }
        let key = key
            .attribute("idstr")
            .filter(|key| !key.is_empty())
            .ok_or("animation parts-visible map key lacks an ID")?;
        let key = budget.text(key)?;
        if !map_keys.insert(key.clone()) {
            return Err("duplicate animation parts-visible map key".into());
        }
        if key.starts_with("live2DPartsOpacity_") {
            if key == "live2DPartsOpacity_" {
                return Err("animation underscore part-opacity map key is empty".into());
            }
            let attribute = required(graph, entry, "value")?;
            if !attribute.has_tag_name("CMvAttrF")
                || required(graph, attribute, "track")?.id() != track.id()
            {
                return Err("animation underscore part-opacity attribute type or owner is invalid".into());
            }
            let id = required(graph, attribute, "id")?;
            let curve_id = id.attribute("idstr").filter(|id| !id.is_empty())
                .ok_or("animation underscore part-opacity attribute lacks an ID")?;
            if !id.has_tag_name("CAttrId") {
                return Err("animation underscore part-opacity attribute ID is not CAttrId".into());
            }
            let guid = required(graph, attribute, "guid")?;
            let part_guid = guid.attribute("uuid").filter(|guid| !guid.is_empty())
                .ok_or("animation underscore part-opacity GUID lacks uuid")?;
            if !guid.has_tag_name("CPartGuid") {
                return Err("animation underscore part-opacity GUID is not CPartGuid".into());
            }
            let (_, source_kind, source_value) = source_part_identity(graph, attribute, budget, true)?;
            let (points, curve_types) = sequence_data(graph, attribute, budget)?;
            underscore_curves.push(json!({"id":budget.text(curve_id)?,"mapKey":key,
                "partGuid":budget.text(part_guid)?,"sourcePart":{"type":source_kind,"value":source_value},
                "range":[number(graph,attribute,"rangeMin")?,number(graph,attribute,"rangeMax")?],
                "points":points,"curveTypes":curve_types}));
            skipped_legacy_count += 1;
            continue;
        }
        let Some(part_id_from_key) = key.strip_prefix("live2DPartsOpacity:") else {
            continue;
        };
        let attribute = required(graph, entry, "value")?;
        if !attribute.has_tag_name("CMvAttrF") {
            return Err("animation parts-visible map value is not CMvAttrF".into());
        }
        let id = required(graph, attribute, "id")?;
        if !id.has_tag_name("CAttrId") {
            return Err("animation part opacity ID is not CAttrId".into());
        }
        let id = id
            .attribute("idstr")
            .filter(|id| !id.is_empty())
            .ok_or("animation part opacity attribute lacks an ID")?;
        if let Some(legacy_attribute_id) = id.strip_prefix("live2DPartsOpacity:") {
            if part_id_from_key.is_empty() || legacy_attribute_id.is_empty() {
                return Err("animation legacy part opacity map or attribute ID is empty".into());
            }
            if required(graph, attribute, "track")?.id() != track.id() {
                return Err(
                    "animation legacy part opacity track backlink differs from its owner".into(),
                );
            }
            if !legacy_channel_ids.insert(id.to_owned()) {
                return Err("duplicate animation legacy part opacity attribute ID".into());
            }
            let guid_field = required(graph, attribute, "guid")?;
            if !guid_field.has_tag_name("CPartGuid") {
                return Err("animation legacy part opacity GUID is not CPartGuid".into());
            }
            let part_guid = guid_field
                .attribute("uuid")
                .filter(|guid| !guid.is_empty())
                .ok_or("animation legacy part opacity GUID lacks uuid")?;
            if !legacy_part_guids.insert(part_guid) {
                return Err("duplicate animation legacy part opacity GUID".into());
            }
            let (_, source_kind, source_value) =
                source_part_identity(graph, attribute, budget, false)?;
            let min = number(graph, attribute, "rangeMin")?;
            let max = number(graph, attribute, "rangeMax")?;
            let (points, curve_types) = sequence_data(graph, attribute, budget)?;
            legacy_curves.push(json!({
                "id": budget.text(id)?,
                "legacyMapPartId": budget.text(part_id_from_key)?,
                "legacyAttributeId": budget.text(legacy_attribute_id)?,
                "partGuid": budget.text(part_guid)?,
                "sourcePart": { "type": source_kind, "value": source_value },
                "range": [min, max],
                "points": points,
                "curveTypes": curve_types,
            }));
            skipped_legacy_count += 1;
            continue;
        }
        if !id.starts_with("live2DPartsOpacity_") {
            return Err("unsupported animation part opacity attribute ID".into());
        }
        if part_id_from_key.is_empty() || part_id_from_key.contains(':') {
            return Err("animation parts-visible map key has an invalid part ID".into());
        }
        if required(graph, attribute, "track")?.id() != track.id() {
            return Err("animation part opacity track backlink differs from its owner".into());
        }
        if id != format!("live2DPartsOpacity_{part_id_from_key}") {
            return Err("animation part opacity map key differs from its attribute ID".into());
        }
        if !channel_ids.insert(id.to_owned()) {
            return Err("duplicate animation part opacity attribute ID".into());
        }
        let guid_field = required(graph, attribute, "guid")?;
        if !guid_field.has_tag_name("CPartGuid") {
            return Err("animation part opacity GUID is not CPartGuid".into());
        }
        let part_guid = guid_field
            .attribute("uuid")
            .filter(|guid| !guid.is_empty())
            .ok_or("animation part opacity GUID lacks uuid")?;
        if !part_guids.insert(part_guid) {
            return Err("duplicate animation part opacity GUID".into());
        }
        let (part_id, source_kind, source_value) = source_part_identity(graph, attribute, budget, false)?;
        if part_id != part_id_from_key || !part_ids.insert(part_id.clone()) {
            return Err("animation parts-visible map key differs from its source part".into());
        }
        let min = number(graph, attribute, "rangeMin")?;
        let max = number(graph, attribute, "rangeMax")?;
        let (points, curve_types) = sequence_data(graph, attribute, budget)?;
        curves.push(json!({
            "id": budget.text(id)?,
            "partId": part_id,
            "partGuid": budget.text(part_guid)?,
            "sourcePart": { "type": source_kind, "value": source_value },
            "range": [min, max],
            "points": points,
            "curveTypes": curve_types,
        }));
    }
    Ok((curves, legacy_curves, underscore_curves, skipped_legacy_count))
}

fn list<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
    budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let list = required(graph, node, name)?;
    values::list(graph, list, budget)
}

enum CurveSlot {
    Kept { original: usize, candidate: usize },
    Relocated { original: usize, candidate: usize },
    Added { candidate: usize },
}

struct CurveSetPlan {
    ordered: Vec<CurveSlot>,
    removed: Vec<usize>,
}

fn curve_set_plan(old_curves: &[Value], new_curves: &[Value]) -> Result<CurveSetPlan, String> {
    let mut positions = HashMap::new();
    for (index, curve) in old_curves.iter().enumerate() {
        let id = curve["id"].as_str().ok_or("animation curve lacks an ID")?;
        if positions.insert(id, index).is_some() {
            return Err("duplicate animation parameter curve ID".into());
        }
    }
    let mut ordered = Vec::with_capacity(new_curves.len());
    let mut used = HashSet::new();
    let mut candidate_ids = HashSet::new();
    let mut last: Option<usize> = None;
    for (candidate, curve) in new_curves.iter().enumerate() {
        let id = curve["id"].as_str().ok_or("animation curve lacks an ID")?;
        if !candidate_ids.insert(id) {
            return Err("duplicate animation parameter curve ID".into());
        }
        match positions.get(id) {
            Some(&original) => {
                if !used.insert(original) {
                    return Err("duplicate animation parameter curve ID".into());
                }
                if last.map_or(true, |last| original > last) {
                    last = Some(original);
                    ordered.push(CurveSlot::Kept { original, candidate });
                } else {
                    ordered.push(CurveSlot::Relocated { original, candidate });
                }
            }
            None => ordered.push(CurveSlot::Added { candidate }),
        }
    }
    let removed = (0..old_curves.len()).filter(|index| !used.contains(index)).collect();
    Ok(CurveSetPlan { ordered, removed })
}

fn check_new_curve(curve: &Value) -> Result<(), String> {
    const KEYS: &[&str] = &[
        "id", "parameterId", "parameterGuid", "sourceParameter", "range", "points", "curveTypes",
    ];
    let object = curve.as_object().ok_or("animation curve must be an object")?;
    let base = object.get("baseValue");
    if object.len() != KEYS.len() + usize::from(base.is_some())
        || !KEYS.iter().all(|key| object.contains_key(*key))
    {
        return Err("animation curve carries unsupported keys".into());
    }
    if base.is_some_and(|base| !base.as_f64().is_some_and(f64::is_finite)) {
        return Err("animation curve baseValue must be finite".into());
    }
    let id = curve["id"].as_str().ok_or("animation curve lacks an ID")?;
    let parameter_id = curve["parameterId"]
        .as_str()
        .ok_or("animation curve lacks a parameterId")?;
    if id != format!("live2dParam_{parameter_id}") {
        return Err("animation curve ID differs from its parameterId".into());
    }
    curve["parameterGuid"]
        .as_str()
        .filter(|guid| !guid.is_empty())
        .ok_or("animation curve lacks a parameterGuid")?;
    let source = &curve["sourceParameter"];
    if source["type"].as_str() != Some("CParameterId")
        || source["value"].as_str() != Some(parameter_id)
    {
        return Err("animation source parameter identity differs".into());
    }
    let range = curve["range"]
        .as_array()
        .filter(|range| range.len() == 2)
        .ok_or("animation curve range missing")?;
    let (minimum, maximum) = (finite(&range[0])?, finite(&range[1])?);
    if minimum > maximum {
        return Err("animation curve range is unordered".into());
    }
    check_curve_keys(curve)
}

fn check_curve_keys(curve: &Value) -> Result<(), String> {
    let points = curve["points"].as_array().ok_or("animation points missing")?;
    let types = curve["curveTypes"]
        .as_array()
        .ok_or("animation curve lacks curve types")?;
    if types.len() != points.len() {
        return Err("animation point and curve-type counts differ".into());
    }
    if !types
        .iter()
        .all(|kind| kind.as_str().map_or(false, |kind| CURVE_TYPES.contains(&kind)))
    {
        return Err("type must be SMOOTH, BEZIER, LINEAR or STEP".into());
    }
    let frames: Vec<i64> = points
        .iter()
        .map(|point| {
            point["anchor"]["frame"]
                .as_i64()
                .ok_or("anchorFrame must be a signed integer")
        })
        .collect::<Result<_, _>>()?;
    if frames.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("animation anchor frames must be strictly increasing".into());
    }
    Ok(())
}

fn raw_field<'d, 'i>(owner: Node<'d, 'i>, name: &str) -> Result<Node<'d, 'i>, String> {
    owner
        .children()
        .find(|node| node.is_element() && node.attribute("xs.n") == Some(name))
        .ok_or_else(|| format!("animation source lacks {name}"))
}

fn curve_storage(
    graph: &Graph<'_, '_>,
    attribute: Node<'_, '_>,
    track: Node<'_, '_>,
    shared: Node<'_, '_>,
) -> Result<(), String> {
    if !attribute.has_tag_name("CMvAttrF") {
        return Err("animation curve attribute is not CMvAttrF".into());
    }
    let id = attribute
        .attribute("xs.id")
        .ok_or("animation curve attribute lacks shared storage")?;
    if attribute.attribute("xs.ref").is_some() {
        return Err("animation curve attribute is aliased".into());
    }
    if attribute.parent() != Some(shared) {
        return Err("animation curve attribute is not a direct shared entry".into());
    }
    for name in ["guid", "id", "optionParam", "rangeMin", "rangeMax"] {
        let field = raw_field(attribute, name)?;
        if field.attribute("xs.ref").is_some() || field.attribute("xs.id").is_some() {
            return Err("animation curve field uses shared or aliased storage".into());
        }
    }
    let track_field = raw_field(attribute, "track")?;
    if track_field.attribute("xs.ref").is_none()
        || track_field.attribute("xs.id").is_some()
        || track_field.has_children()
    {
        return Err("animation curve track is not a direct reference".into());
    }
    if graph.resolve(track_field)?.id() != track.id() {
        return Err("animation parameter track backlink differs from its owner".into());
    }
    let sequence = raw_field(attribute, "valueData")?;
    if sequence.attribute("xs.ref").is_some() || sequence.attribute("xs.id").is_some() {
        return Err("animation curve storage is shared or aliased".into());
    }
    if !sequence.has_tag_name("CMutableSequence") {
        return Err("animation curve storage is not CMutableSequence".into());
    }
    let backref = raw_field(sequence, "attr")?;
    if backref.attribute("xs.ref") != Some(id)
        || backref.attribute("xs.id").is_some()
        || backref.has_children()
    {
        return Err("animation curve storage backlink is missing".into());
    }
    if graph.resolve(backref)?.id() != attribute.id() {
        return Err("animation curve storage backlink is aliased".into());
    }
    if attribute
        .descendants()
        .filter(|node| node.is_element())
        .any(|node| {
            node.id() != attribute.id()
                && (node.attribute("xs.id").is_some() || node.attribute("xs.n") == Some("super"))
        })
    {
        return Err("animation curve storage contains nested shared or inherited data".into());
    }
    Ok(())
}

fn curve_entry<'d, 'i>(
    graph: &Graph<'d, 'i>,
    owner: Node<'d, 'i>,
    track: Node<'d, 'i>,
    shared: Node<'d, 'i>,
) -> Result<Node<'d, 'i>, String> {
    if owner.attribute("xs.ref").is_none()
        || owner.attributes().count() != 1
        || owner.has_children()
    {
        return Err("animation curve member is not a bare reference".into());
    }
    let definition = graph.resolve(owner)?;
    if definition.attribute("xs.id") != owner.attribute("xs.ref") {
        return Err("animation curve reference is aliased".into());
    }
    curve_storage(graph, definition, track, shared)?;
    Ok(definition)
}

/// The definition of a parameter curve to delete and the attribute-map entries that name it. The definition is a
/// shared CMvAttrF of `track` that nothing but its list member, those entries and itself refers to; what it refers to
/// stays.
fn curve_removal<'d, 'i>(
    graph: &Graph<'d, 'i>,
    owner: Node<'d, 'i>,
    track: Node<'d, 'i>,
    map: Option<Node<'d, 'i>>,
    budget: &mut Budget,
) -> Result<(Node<'d, 'i>, Vec<Node<'d, 'i>>), String> {
    if owner.attribute("xs.ref").is_none() || owner.attributes().count() != 1 || owner.has_children() {
        return Err("animation curve member is not a bare reference".into());
    }
    let definition = graph.resolve(owner)?;
    let id = definition.attribute("xs.id").filter(|id| owner.attribute("xs.ref") == Some(*id))
        .ok_or("animation curve reference is aliased")?;
    if !definition.has_tag_name("CMvAttrF") {
        return Err("animation curve attribute is not CMvAttrF".into());
    }
    if definition.parent().is_none_or(|parent| !parent.has_tag_name("shared")) {
        return Err("animation curve attribute is not a direct shared entry".into());
    }
    if graph.resolve(raw_inherited_field(definition, "track")?)?.id() != track.id() {
        return Err("animation parameter track backlink differs from its owner".into());
    }
    let mut entries = Vec::new();
    for entry in map.iter().flat_map(|map| map.children().filter(Node::is_element)) {
        if raw_field(entry, "value")?.attribute("xs.ref") == Some(id) {
            if entry.descendants().any(|node| node.attribute("xs.id").is_some()) || entry.attribute("xs.ref").is_some() {
                return Err("animation attribute map entry is shared or aliased".into());
            }
            entries.push(entry);
        }
    }
    let ids: HashSet<&str> = definition.descendants().filter_map(|node| node.attribute("xs.id")).collect();
    let removed = |node: Node<'_, '_>| node.ancestors()
        .any(|ancestor| ancestor.id() == definition.id() || ancestor.id() == owner.id()
            || entries.iter().any(|entry| entry.id() == ancestor.id()));
    for node in definition.document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if node.attribute("xs.ref").is_some_and(|target| ids.contains(target)) && !removed(node) {
            return Err("animation curve definition is referenced outside its storage".into());
        }
    }
    Ok((definition, entries))
}

fn serialize_curve(
    storage: &str,
    track_ref: &str,
    curve: &Value,
) -> Result<String, String> {
    check_new_curve(curve)?;
    let keys = serialize_keys(curve)?;
    Ok(format!(
        r#"<CMvAttrF xs.id="{storage}"><CGuid xs.n="guid" uuid="{guid}"/><CAttrId xs.n="id" idstr="{id}"/><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="{track_ref}"/><CMutableSequence xs.n="valueData"><CMvAttrF xs.n="attr" xs.ref="{storage}"/><array xs.n="points" count="{count}" type="CBezierPt">{points}</array><carray_list xs.n="curveTypes" count="{count}">{types}</carray_list><d xs.n="curMin">{min}</d><d xs.n="curMax">{max}</d>{base}</CMutableSequence><COptionParam xs.n="optionParam"><CParameterId xs.n="KEY_PARAM_ID" idstr="{parameter}"/></COptionParam><f xs.n="rangeMin">{rmin}</f><f xs.n="rangeMax">{rmax}</f></CMvAttrF>"#,
        storage = escape(storage), guid = escape(curve["parameterGuid"].as_str().unwrap()),
        id = escape(curve["id"].as_str().unwrap()), track_ref = escape(track_ref),
        count = keys.count, points = keys.points, types = keys.types, min = keys.min, max = keys.max,
        base = curve["baseValue"].as_f64().map_or(String::new(), |base| format!(r#"<d xs.n="baseValue">{base}</d>"#)),
        parameter = escape(curve["parameterId"].as_str().unwrap()),
        rmin = curve["range"][0], rmax = curve["range"][1],
    ))
}

struct SerializedKeys {
    points: String,
    types: String,
    count: usize,
    min: f64,
    max: f64,
}

fn serialize_keys(curve: &Value) -> Result<SerializedKeys, String> {
    check_curve_keys(curve)?;
    check_new_pairs(&[], curve)?;
    let points = curve["points"].as_array().unwrap();
    let types = curve["curveTypes"].as_array().unwrap();
    let mut inner = String::new();
    for point in points {
        inner.push_str(&serialize_point(point, true)?);
    }
    let types_xml: String = types
        .iter()
        .map(|kind| format!(r#"<CCurveType v="{}"/>"#, kind.as_str().unwrap()))
        .collect();
    let extrema = if points.is_empty() {
        (0.0, 0.0)
    } else {
        let values: Vec<f64> = points
            .iter()
            .map(|point| finite(&point["anchor"]["value"]))
            .collect::<Result<_, _>>()?;
        (
            values.iter().cloned().reduce(f64::min).unwrap(),
            values.iter().cloned().reduce(f64::max).unwrap(),
        )
    };
    Ok(SerializedKeys { points: inner, types: types_xml, count: points.len(), min: extrema.0, max: extrema.1 })
}

fn check_new_part_curve(curve: &Value) -> Result<(), String> {
    let keys = ["id", "partId", "partGuid", "sourcePart", "range", "points", "curveTypes"];
    let object = curve.as_object().ok_or("animation Part curve must be an object")?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err("animation Part curve carries unsupported keys".into());
    }
    let part = curve["partId"].as_str().filter(|id| !id.is_empty() && !id.contains(':'))
        .ok_or("animation Part curve has an invalid partId")?;
    if curve["id"] != format!("live2DPartsOpacity_{part}")
        || curve["partGuid"].as_str().is_none_or(|guid| guid.is_empty())
        || curve["sourcePart"] != json!({"type":"CPartId","value":part})
        || curve["range"] != json!([0.0,1.0])
    {
        return Err("animation Part curve identity or range differs".into());
    }
    check_curve_keys(curve)
}

fn serialize_part_curve(storage: &str, track: &str, curve: &Value) -> Result<String, String> {
    check_new_part_curve(curve)?;
    let keys = serialize_keys(curve)?;
    Ok(format!(r#"<CMvAttrF xs.id="{storage}"><ICMvAttr xs.n="super"><CAttrId xs.n="id" idstr="{id}"/><CPartGuid xs.n="guid" uuid="{guid}"/><hash_map xs.n="optionParam" count="1" keyType="string"><CPartId xs.n="KEY_PARTS_VISIBLE_ID" idstr="{part}"/></hash_map></ICMvAttr><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="{track}"/><d xs.n="rangeMin">0</d><d xs.n="rangeMax">1</d><CMutableSequence xs.n="valueData"><ACValueSequence xs.n="super"><CMvAttrF xs.n="attr" xs.ref="{storage}"/><d xs.n="curMin">{min}</d><d xs.n="curMax">{max}</d></ACValueSequence><array xs.n="points" count="{count}" type="CBezierPt">{points}</array><carray_list xs.n="curveTypes" count="{count}">{types}</carray_list></CMutableSequence></CMvAttrF>"#,
        storage=escape(storage),track=escape(track),id=escape(curve["id"].as_str().unwrap()),
        guid=escape(curve["partGuid"].as_str().unwrap()),part=escape(curve["partId"].as_str().unwrap()),
        min=keys.min,max=keys.max,count=keys.count,points=keys.points,types=keys.types))
}

fn serialize_parts_effect(additions: &mut NativeAdditions, track: &str, curves: &[Value]) -> Result<String, String> {
    let mut storage = Vec::new();
    for curve in curves {
        let id = free_storage_id(&mut additions.reserved,
            &format!("soul2d-part-curve-{track}-{}", curve["id"].as_str().ok_or("Part curve ID missing")?));
        additions.definitions.push_str(&serialize_part_curve(&id, track, curve)?);
        storage.push((id, curve));
    }
    serialize_parts_effect_refs(additions, track, &storage)
}

fn serialize_parts_effect_refs(
    additions: &mut NativeAdditions, track: &str, curves: &[(String, &Value)],
) -> Result<String, String> {
    let effect = free_storage_id(&mut additions.reserved, &format!("soul2d-parts-{track}"));
    let mut members = String::new();
    let mut entries = String::new();
    for (id, curve) in curves {
        members.push_str(&format!(r#"<CMvAttrF xs.ref="{}"/>"#, escape(id)));
        entries.push_str(&part_map_entry(id, curve)?);
    }
    additions.definitions.push_str(&format!(r#"<CMvEffect_Live2DPartsVisible xs.id="{effect}"><ICMvEffect xs.n="super"><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="{track}"/><array xs.n="attrList" count="{count}" type="ICMvAttr">{members}</array></ICMvEffect><hash_map xs.n="attrMap" count="{count}">{entries}</hash_map></CMvEffect_Live2DPartsVisible>"#,
        effect=escape(&effect),track=escape(track),count=curves.len()));
    Ok(format!(r#"<CMvEffect_Live2DPartsVisible xs.n="partsVisibleEffect" xs.ref="{}"/>"#, escape(&effect)))
}

fn part_map_entry(storage: &str, curve: &Value) -> Result<String, String> {
    let part = curve["partId"].as_str().ok_or("Part curve lacks partId")?;
    Ok(format!(r#"<entry><CAttrId xs.n="key" idstr="live2DPartsOpacity:{}"/><CMvAttrF xs.n="value" xs.ref="{}"/></entry>"#,
        escape(part),escape(storage)))
}

fn curve_set_patches(
    text: &str,
    graph: &Graph<'_, '_>,
    track_node: Node<'_, '_>,
    track: &Value,
    old_curves: &[Value],
    new_curves: &[Value],
    plan: &CurveSetPlan,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
    additions: &mut NativeAdditions,
) -> Result<(), String> {
    let effect = track_effect(graph, track_node, "keyParamEffect", "CMvEffect_Live2DParameter", "parameter", budget)?
        .ok_or("animation source lacks keyParamEffect")?;
    let attr_list = raw_inherited_field(effect, "attrList")?;
    let attr_map = match graph.field(effect, "attrMap")? {
        Some(_) => Some(raw_inherited_field(effect, "attrMap")?),
        None => None,
    };
    for list in std::iter::once(attr_list).chain(attr_map) {
        if list.attribute("xs.ref").is_some() || list.attribute("xs.id").is_some() {
            return Err("animation attribute list is shared or aliased".into());
        }
    }
    let shared = track_node
        .document()
        .root_element()
        .children()
        .find(|node| node.is_element() && node.has_tag_name("shared"))
        .ok_or("animation source lacks a shared section")?;
    let raw: Vec<Node> = attr_list.children().filter(Node::is_element).collect();
    let mut member_curve: Vec<Option<usize>> = Vec::with_capacity(raw.len());
    let mut seen = HashSet::new();
    for entry in &raw {
        let resolved = graph.resolve(*entry)?;
        if !resolved.has_tag_name("CMvAttrF") {
            member_curve.push(None);
            continue;
        }
        let Some(id) = raw_inherited_field(resolved, "id")
            .ok()
            .and_then(|field| field.attribute("idstr"))
        else {
            member_curve.push(None);
            continue;
        };
        if !id.starts_with("live2dParam_") {
            member_curve.push(None);
            continue;
        }
        if !seen.insert(id.to_owned()) {
            return Err("animation parameter attribute is ambiguous".into());
        }
        let original = old_curves
            .iter()
            .position(|curve| curve["id"].as_str() == Some(id))
            .ok_or("animation attribute has no matching parameter curve")?;
        member_curve.push(Some(original));
    }
    let mut owner_of = vec![None; old_curves.len()];
    for (raw_index, member) in member_curve.iter().enumerate() {
        if let Some(original) = member {
            if owner_of[*original].replace(raw_index).is_some() {
                return Err("animation parameter curve is aliased".into());
            }
        }
    }
    let mut map_entries = 0;
    for removed in &plan.removed {
        let raw_index = owner_of[*removed].ok_or("animation curve lacks owning storage")?;
        let owner = raw[raw_index];
        let (definition, entries) = curve_removal(graph, owner, track_node, attr_map, budget)?;
        patches.push((owner.range(), String::new()));
        patches.push((definition.range(), String::new()));
        patches.extend(entries.iter().map(|entry| (entry.range(), String::new())));
        map_entries += entries.len();
    }
    // Cubism keeps some files' attribute maps listing every curve; a new curve joins such a map.
    let map_complete = attr_map.is_some_and(|map| raw.iter().all(|member| {
        map.children().filter(Node::is_element)
            .any(|entry| raw_field(entry, "value").ok().and_then(|value| value.attribute("xs.ref")) == member.attribute("xs.ref"))
    }));
    let mut map_inserts = Vec::new();
    let mut new_slots = Vec::with_capacity(plan.ordered.len());
    let mut inserts = Vec::new();
    for slot in &plan.ordered {
        match *slot {
            CurveSlot::Kept { original, candidate } => {
                let raw_index =
                    owner_of[original].ok_or("animation curve lacks owning storage")?;
                let definition = graph.resolve(raw[raw_index])?;
                curve_patches(
                    text, graph, definition, &old_curves[original], &new_curves[candidate],
                    patches, budget,
                )?;
                new_slots.push(Some(raw_index));
            }
            CurveSlot::Relocated { original, candidate } => {
                let raw_index =
                    owner_of[original].ok_or("animation curve lacks owning storage")?;
                let owner = raw[raw_index];
                let definition = curve_entry(graph, owner, track_node, shared)?;
                curve_patches(
                    text, graph, definition, &old_curves[original], &new_curves[candidate],
                    patches, budget,
                )?;
                inserts.push(text[owner.range()].to_owned());
                patches.push((owner.range(), String::new()));
                new_slots.push(None);
            }
            CurveSlot::Added { candidate } => {
                let curve = &new_curves[candidate];
                check_new_curve(curve)?;
                let storage = free_storage_id(
                    &mut additions.reserved,
                    &format!(
                        "soul2d-curve-{}-{}",
                        track["guid"].as_str().ok_or("animation track GUID missing")?,
                        curve["id"].as_str().unwrap()
                    ),
                );
                let track_ref = track_node
                    .attribute("xs.id")
                    .ok_or("animation track lacks storage identity")?;
                inserts.push(format!(r#"<CMvAttrF xs.ref="{}"/>"#, escape(&storage)));
                match animation_clone::curve(text, track_node, &animation_clone::PARAMETER, track_ref, map_complete, curve, &storage,
                    additions)? {
                    Some(clone) => {
                        additions.definitions.push_str(&clone.definition);
                        map_inserts.extend(clone.map_entry);
                    }
                    None => additions.definitions.push_str(&serialize_curve(&storage, track_ref, curve)?),
                }
                new_slots.push(None);
            }
        }
    }
    let end = list_end(attr_list, text)?;
    emit_inserts(
        patches,
        new_slots.into_iter(),
        &raw,
        &end,
        inserts.into_iter(),
    );
    if let Some(map) = attr_map.filter(|_| map_entries > 0 || !map_inserts.is_empty()) {
        let entries: Vec<Node> = map.children().filter(Node::is_element).collect();
        patches.push(count_patch(map, entries.len() - map_entries + map_inserts.len())?);
        if !map_inserts.is_empty() {
            let end = list_end(map, text)?;
            emit_inserts(patches, [None].into_iter(), &entries, &end, [map_inserts.concat()].into_iter());
        }
    }
    let added = plan
        .ordered
        .iter()
        .filter(|slot| matches!(slot, CurveSlot::Added { .. }))
        .count();
    let new_len = raw.len() + added - plan.removed.len();
    if new_len != raw.len() {
        patches.push(count_patch(attr_list, new_len)?);
    }
    Ok(())
}

pub(crate) fn free_storage_id(reserved: &mut HashSet<String>, base: &str) -> String {
    let mut id = base.to_owned();
    let mut suffix = 1;
    while !reserved.insert(id.clone()) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}


fn track_set_patches(
    text: &str,
    graph: &Graph<'_, '_>,
    scene_node: Node<'_, '_>,
    old_tracks: &[Value],
    new_tracks: &[Value],
    plan: &TrackSetPlan,
    removed_extras: &[&Value],
    additions: &mut NativeAdditions,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
) -> Result<(), String> {
    let set_node = inline_field(scene_node, "trackSourceSet")?;
    let source_list = inline_field(set_node, "_sources")?;
    let shared = scene_node
        .document()
        .root_element()
        .children()
        .find(|node| node.is_element() && node.has_tag_name("shared"))
        .ok_or("animation source lacks a shared section")?;
    let scene_def = graph.resolve(scene_node)?;
    let raw: Vec<Node> = source_list.children().filter(Node::is_element).collect();
    let mut member_track: Vec<Option<usize>> = Vec::with_capacity(raw.len());
    let mut seen = HashSet::new();
    for entry in &raw {
        let resolved = graph.resolve(*entry)?;
        if !resolved.has_tag_name("CMvTrack_Live2DModel_Source") || resolved.parent() != Some(shared)
        {
            member_track.push(None);
            continue;
        }
        let guid = graph.guid(resolved)?.ok_or("animation track definition lacks a GUID")?;
        match old_tracks.iter().position(|track| track["guid"] == guid) {
            Some(index) if seen.insert(index) => member_track.push(Some(index)),
            Some(_) => return Err("animation track list holds a duplicate member".into()),
            None => member_track.push(None),
        }
    }
    for member in member_track.iter().flatten() {
        if !plan.removed.contains(member)
            && !plan.ordered.iter().any(|slot| {
                matches!(slot, TrackSlot::Kept { original, .. } if original == member)
            })
        {
            return Err("animation track list order cannot be reconciled".into());
        }
    }
    let mut closure: Vec<Node> = Vec::new();
    let mut removed_ranges: Vec<Range<usize>> = Vec::new();
    let mut parent_counts = HashMap::new();
    for (raw_index, member) in member_track.iter().enumerate() {
        let mut removed = member.filter(|index| plan.removed.contains(index)).map(|index| &old_tracks[index]);
        if removed.is_none() {
            let definition = graph.resolve(raw[raw_index])?;
            if matches!(definition.tag_name().name(), "CMvTrack_Image_Source" | "CMvTrack_JavaSoundTrack_Source" | "CMvTrack_GuideImage_Source") {
                let guid = graph.guid(definition)?.ok_or("animation image track definition lacks a GUID")?;
                removed = removed_extras.iter().copied().find(|track| track["guid"] == guid);
            }
        }
        if let Some(track) = removed {
            let deletion = track_definition_ownership(
                graph, raw[raw_index], scene_def, shared, track, budget,
            )?;
            closure.extend(deletion.definitions);
            if let Some((list, member)) = deletion.parent_member {
                parent_counts.entry(list.id()).or_insert((list, 0_usize)).1 += 1;
                removed_ranges.push(member.range());
            }
            for (list, item) in deletion.items {
                parent_counts.entry(list.id()).or_insert((list, 0_usize)).1 += 1;
                removed_ranges.push(item.range());
            }
            removed_ranges.push(raw[raw_index].range());
        }
    }
    // A cloned track joins the scene's root group the way Cubism lists its tracks.
    let group = match graph.field(scene_def, "rootTrack")? {
        Some(group) if group.has_tag_name("CMvTrack_Group_Source") => Some(group),
        _ => None,
    };
    let group_guid = match group {
        Some(group) => graph.field(group, "guid")?.and_then(|guid| guid.attribute("xs.id")),
        None => None,
    };
    let mut slots: Vec<Option<usize>> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut members = String::new();
    for slot in &plan.ordered {
        match slot {
            TrackSlot::Kept { original, candidate } => {
                let raw_index = member_track
                    .iter()
                    .position(|member| *member == Some(*original))
                    .ok_or("animation track member is missing from the raw list")?;
                slots.push(Some(raw_index));
                let old_track = &old_tracks[*original];
                let new_track = &new_tracks[*candidate];
                if old_track != new_track {
                    let track_node = graph.resolve(raw[raw_index])?;
                    track_curve_changes(
                        text, graph, track_node, old_track, new_track, patches, budget,
                        additions,
                    )?;
                    let mut retained = new_track.clone();
                    retained["parameterCurves"] = old_track["parameterCurves"].clone();
                    retained["partOpacityCurves"] = old_track["partOpacityCurves"].clone();
                    retained["legacyPartOpacityCurves"] = old_track["legacyPartOpacityCurves"].clone();
                    retained["underscorePartOpacityCurves"] = old_track["underscorePartOpacityCurves"].clone();
                    retained["skippedLegacyPartOpacityCount"] = old_track["skippedLegacyPartOpacityCount"].clone();
                    if retained != *old_track {
                        return Err(ONLY_EDITS.into());
                    }
                }
            }
            TrackSlot::Added { candidate } => {
                let scene_id = scene_def
                    .attribute("xs.id")
                    .ok_or("animation scene lacks shared storage for a track backlink")?;
                let track = &new_tracks[*candidate];
                let guid = track["guid"].as_str().ok_or("a new animation track lacks a GUID")?;
                let storage = free_storage_id(&mut additions.reserved, &format!("soul2d-track-{guid}"));
                let place = animation_clone::TrackPlace { scene_id, parent_guid: group_guid };
                let cloned = match group {
                    Some(_) => animation_clone::track(text, scene_def, &place, track, &storage, additions)?,
                    None => None,
                };
                match cloned {
                    Some(clone) => {
                        additions.definitions.push_str(&clone.definitions);
                        members.push_str(&format!(r#"<CTrackGuid xs.ref="{}"/>"#, escape(&clone.guid_id)));
                        pending.push(format!(r#"<CMvTrack_Live2DModel_Source xs.ref="{}"/>"#, escape(&storage)));
                    }
                    None => {
                        additions.reserved.remove(&storage);
                        let serialized = serialize_track(additions, scene_id, track)?;
                        additions.definitions.push_str(&serialized.definitions);
                        pending.push(serialized.reference);
                    }
                }
                slots.push(None);
            }
        }
    }
    if !members.is_empty() {
        let group = group.ok_or("a cloned track has no group to join")?;
        let list = raw_inherited_field(group, "_childTrackGuids")?;
        if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
            return Err("animation group children use shared storage".into());
        }
        let added = members.matches("<CTrackGuid").count();
        let entry = parent_counts.entry(list.id()).or_insert((list, 0_usize));
        let children: Vec<Node> = list.children().filter(Node::is_element).collect();
        let end = list_end(list, text)?;
        emit_inserts(patches, [None].into_iter(), &children, &end, [members].into_iter());
        let count = values::list(graph, list, budget)?.len().checked_sub(entry.1)
            .ok_or("animation group child count underflow")? + added;
        patches.push(topology_count_patch(list, count)?);
        parent_counts.remove(&list.id());
    }
    for (list, removed) in parent_counts.into_values() {
        let count = values::list(graph, list, budget)?.len().checked_sub(removed)
            .ok_or("animation group child count underflow")?;
        patches.push(topology_count_patch(list, count)?);
    }
    for node in &closure {
        removed_ranges.push(node.range());
    }
    patches.extend(removed_ranges.into_iter().map(|range| (range, String::new())));
    let count = raw.len() + pending.len() - plan.removed.len() - removed_extras.len();
    if count != raw.len() {
        patches.push(topology_count_patch(source_list, count)?);
    }
    let end = list_end(source_list, text)?;
    emit_inserts(patches, slots.into_iter(), &raw, &end, pending.into_iter());
    Ok(())
}

fn track_curve_changes(
    text: &str,
    graph: &Graph<'_, '_>,
    track_node: Node<'_, '_>,
    old_track: &Value,
    new_track: &Value,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
    additions: &mut NativeAdditions,
) -> Result<(), String> {
    let mut mapped_count = 0;
    for field in ["legacyPartOpacityCurves", "underscorePartOpacityCurves"] {
        mapped_count += new_track[field].as_array().ok_or("animation mapped Part curves missing")?.len();
    }
    if new_track["skippedLegacyPartOpacityCount"].as_u64() != Some(mapped_count as u64) {
        return Err("animation mapped Part curve count differs from its channels".into());
    }
    let effect = required(graph, track_node, "keyParamEffect")?;
    let old_curves = old_track["parameterCurves"]
        .as_array()
        .ok_or("animation curves missing")?;
    let new_curves = new_track["parameterCurves"]
        .as_array()
        .ok_or("animation curves missing")?;
    let set = curve_set_plan(old_curves, new_curves)?;
    if set.ordered.iter().all(|slot| matches!(slot, CurveSlot::Kept { .. }))
        && set.removed.is_empty()
    {
        for slot in &set.ordered {
            let CurveSlot::Kept { original, candidate } = slot else { unreachable!() };
            let curve = &old_curves[*original];
            let target = &new_curves[*candidate];
            if curve == target {
                continue;
            }
            let attribute = curve_attribute(graph, effect, &curve["id"], budget)?;
            curve_patches(text, graph, attribute, curve, target, patches, budget)?;
        }
    } else {
        curve_set_patches(
            text, graph, track_node, old_track, old_curves, new_curves, &set,
            patches, budget, additions,
        )?;
    }
    let (mut map_delta, mut member_delta) = part_opacity_curve_changes(
        text, graph, track_node, old_track, new_track, patches, budget, additions,
    )?;
    for field in ["legacyPartOpacityCurves", "underscorePartOpacityCurves"] {
        let (map, members) = mapped_part_curve_changes(text, graph, track_node, old_track, new_track, field, patches, budget)?;
        map_delta += map;
        member_delta += members;
    }
    if map_delta != 0 || member_delta != 0 {
        let effect = part_effect(graph, track_node, budget)?.ok_or("animation part-opacity effect missing")?;
        for (list, delta) in [(Some(effect.map), map_delta), (effect.members, member_delta)] {
            if delta != 0 {
                let list = list.ok_or("animation part-opacity member list missing")?;
                let count = list.children().filter(Node::is_element).count().checked_add_signed(delta)
                    .ok_or("animation part-opacity count underflow")?;
                patches.push(topology_count_patch(list, count)?);
            }
        }
    }
    Ok(())
}

fn raw_inherited_field<'d, 'i>(owner: Node<'d, 'i>, name: &str) -> Result<Node<'d, 'i>, String> {
    let mut current = owner;
    for _ in 0..=64 {
        let mut fields = current.children().filter(|n| n.is_element() && n.attribute("xs.n") == Some(name));
        if let Some(field) = fields.next() {
            if fields.next().is_some() {
                return Err(format!("duplicate animation field {name}"));
            }
            return Ok(field);
        }
        let mut supers = current.children().filter(|n| n.is_element() && n.attribute("xs.n") == Some("super"));
        let parent = supers.next().ok_or_else(|| format!("animation source lacks {name}"))?;
        if supers.next().is_some() || parent.attribute("xs.id").is_some() || parent.attribute("xs.ref").is_some() {
            return Err("animation inherited storage is shared or ambiguous".into());
        }
        current = parent;
    }
    Err("animation inheritance exceeds 64 nodes".into())
}

fn exclusive_references(id: &str, allowed: &[Node<'_, '_>], label: &str, budget: &mut Budget) -> Result<(), String> {
    for node in allowed {
        if node.attribute("xs.ref") != Some(id) || node.has_children()
            || node.attributes().any(|a| !matches!(a.name(), "xs.n" | "xs.ref"))
        {
            return Err(format!("animation {label} storage has an aliased reference"));
        }
    }
    let mut count = 0;
    for node in allowed[0].document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if node.attribute("xs.ref") == Some(id) {
            if !allowed.iter().any(|expected| expected.id() == node.id()) {
                return Err(format!("animation {label} storage is referenced outside its owner"));
            }
            count += 1;
        }
    }
    if count != allowed.len() {
        return Err(format!("animation {label} storage references are ambiguous"));
    }
    Ok(())
}

struct PartEffect<'d, 'i> {
    definition: Node<'d, 'i>,
    map: Node<'d, 'i>,
    members: Option<Node<'d, 'i>>,
}

struct PartCurveStorage<'d, 'i> {
    entry: Node<'d, 'i>,
    member: Node<'d, 'i>,
    attribute: Node<'d, 'i>,
    references: [Node<'d, 'i>; 3],
}

struct MappedPartStorage<'d, 'i> {
    entry: Node<'d, 'i>,
    member: Option<Node<'d, 'i>>,
    attribute: Node<'d, 'i>,
    references: Vec<Node<'d, 'i>>,
}

struct UnmappedPartStorage<'d, 'i> {
    attribute: Node<'d, 'i>,
    member: Option<Node<'d, 'i>>,
}

fn unmapped_part_storage<'d, 'i>(
    graph: &Graph<'d, 'i>, member_list: Option<Node<'d, 'i>>, track: Node<'d, 'i>,
    curve: &Value, budget: &mut Budget,
) -> Result<Option<UnmappedPartStorage<'d, 'i>>, String> {
    let mut selected = None;
    for node in track.document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if !node.has_tag_name("CMvAttrF") || node.attribute("xs.ref").is_some() {
            continue;
        }
        let id = graph.field(node, "id")?;
        let guid = graph.field(node, "guid")?;
        let same_id = id.and_then(|n| n.attribute("idstr")) == curve["id"].as_str();
        let same_guid = guid.is_some_and(|n| n.has_tag_name("CPartGuid")
            && n.attribute("uuid") == curve["partGuid"].as_str());
        if (!same_id && !same_guid) || graph.field(node, "track")? != Some(track) {
            continue;
        }
        if !same_id || !same_guid || id.is_none_or(|n| !n.has_tag_name("CAttrId")) {
            return Err("animation unmapped Part identity conflicts with the new curve".into());
        }
        if selected.replace(node).is_some() {
            return Err("animation unmapped Part storage is ambiguous".into());
        }
    }
    let Some(attribute) = selected else { return Ok(None) };
    let shared = track.document().root_element().children().find(|n| n.has_tag_name("shared"))
        .ok_or("animation source lacks a shared section")?;
    track_owned_definition(graph, attribute, track, shared, budget)?;
    let (part, kind, value) = source_part_identity(graph, attribute, budget, false)?;
    if curve["partId"] != part || curve["sourcePart"] != json!({"type":kind,"value":value})
        || curve["range"] != json!([number(graph, attribute, "rangeMin")?, number(graph, attribute, "rangeMax")?])
    {
        return Err("animation unmapped Part metadata differs from the new curve".into());
    }
    let (points, types) = sequence_data(graph, attribute, budget)?;
    if !points.is_empty() || !types.is_empty() {
        return Err("animation unmapped Part storage already contains keys".into());
    }
    let mut members = Vec::new();
    for member in member_list.into_iter().flat_map(|list| list.children()).filter(Node::is_element) {
        budget.consume(1)?;
        if graph.resolve(member)?.id() == attribute.id() {
            if !member.has_tag_name("CMvAttrF") || member.attributes().any(|a| a.name() != "xs.ref") {
                return Err("animation unmapped Part member is aliased".into());
            }
            members.push(member);
        }
    }
    if members.len() > 1 {
        return Err("animation unmapped Part member is ambiguous".into());
    }
    let sequence = inline_field(attribute, "valueData")?;
    let mut references = vec![raw_inherited_field(sequence, "attr")?];
    references.extend(members.iter().copied());
    exclusive_references(attribute.attribute("xs.id").unwrap(), &references, "part-opacity", budget)?;
    Ok(Some(UnmappedPartStorage { attribute, member: members.first().copied() }))
}

/// The effect a track names in `field`, written inline or as a shared entry that only the track and its effect
/// manager refer to; `None` when the track has none.
fn track_effect<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, field: &str, tag: &str, label: &str, budget: &mut Budget,
) -> Result<Option<Node<'d, 'i>>, String> {
    if graph.field(track, field)?.is_none() {
        return Ok(None);
    }
    let field = raw_inherited_field(track, field)?;
    let effect = graph.resolve(field)?;
    if effect.has_tag_name("null") {
        return Ok(None);
    }
    if let Some(id) = effect.attribute("xs.id") {
        if effect.parent().is_none_or(|parent| !parent.has_tag_name("shared")) {
            return Err(format!("animation {label} effect is not a direct shared entry"));
        }
        let mut references = vec![field];
        if graph.field(track, "effectManager")?.is_some() {
            let manager = raw_inherited_field(track, "effectManager")?;
            if !manager.has_tag_name("CMvEffectManager") || manager.attribute("xs.id").is_some()
                || manager.attribute("xs.ref").is_some()
            {
                return Err("animation effect manager is shared or aliased".into());
            }
            {
                let list = raw_inherited_field(manager, "effectList")?;
                if !list.has_tag_name("array") || list.attribute("type") != Some("ICMvEffect")
                    || list.attribute("count").is_none() || list.attribute("xs.id").is_some()
                    || list.attribute("xs.ref").is_some()
                {
                    return Err("animation effect registry has an unsupported shape or is shared".into());
                }
                values::list(graph, list, budget)?;
                let mut members = list.children().filter(|n| n.is_element() && n.attribute("xs.ref") == Some(id));
                if let Some(member) = members.next() {
                    if !member.has_tag_name(tag) || member.attributes().count() != 1
                        || member.has_children() || members.next().is_some()
                    {
                        return Err(format!("animation {label} effect registry member is ambiguous or aliased"));
                    }
                    references.push(member);
                }
            }
        }
        exclusive_references(id, &references, label, budget)?;
    } else if effect.id() != field.id() {
        return Err(format!("animation {label} effect is aliased"));
    }
    Ok(Some(effect))
}

fn part_effect<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, budget: &mut Budget,
) -> Result<Option<PartEffect<'d, 'i>>, String> {
    let Some(effect) = track_effect(graph, track, "partsVisibleEffect", "CMvEffect_Live2DPartsVisible", "part-opacity",
        budget)? else {
        return Ok(None);
    };
    let map = raw_inherited_field(effect, "attrMap")?;
    let members = if graph.field(effect, "attrList")?.is_some() {
        Some(raw_inherited_field(effect, "attrList")?)
    } else { None };
    for list in std::iter::once(map).chain(members) {
        if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
            return Err("animation part-opacity list is shared or aliased".into());
        }
        values::list(graph, list, budget)?;
    }
    Ok(Some(PartEffect { definition: effect, map, members }))
}

fn part_map_attribute<'d, 'i>(
    graph: &Graph<'d, 'i>, effect: &PartEffect<'d, 'i>, key: &str, curve: &Value, budget: &mut Budget,
) -> Result<(Node<'d, 'i>, Node<'d, 'i>), String> {
    let mut selected = None;
    for entry in values::list(graph, effect.map, budget)? {
        if required(graph, entry, "key")?.attribute("idstr") == Some(key) {
            if entry.parent() != Some(effect.map) || entry.attribute("xs.id").is_some()
                || entry.attribute("xs.ref").is_some() || selected.replace(entry).is_some()
            {
                return Err("animation part-opacity map entry is shared or ambiguous".into());
            }
        }
    }
    let entry = selected.ok_or("animation part-opacity map entry missing")?;
    let value = raw_field(entry, "value")?;
    let attribute = graph.resolve(value)?;
    if required(graph, attribute, "id")?.attribute("idstr") != curve["id"].as_str() {
        return Err("animation part-opacity attribute identity differs".into());
    }
    attribute.attribute("xs.id").ok_or("animation part-opacity attribute lacks owned storage")?;
    if attribute.parent().is_none_or(|parent| !parent.has_tag_name("shared")) {
        return Err("animation part-opacity attribute is not a direct shared entry".into());
    }
    Ok((entry, attribute))
}

fn part_curve_storage<'d, 'i>(
    graph: &Graph<'d, 'i>, effect: &PartEffect<'d, 'i>, curve: &Value, budget: &mut Budget,
) -> Result<PartCurveStorage<'d, 'i>, String> {
    let part = curve["partId"].as_str().ok_or("animation part ID missing")?;
    let (entry, attribute) = part_map_attribute(graph, effect, &format!("live2DPartsOpacity:{part}"), curve, budget)?;
    let value = raw_field(entry, "value")?;
    let id = attribute.attribute("xs.id").unwrap();
    let members = effect.members.ok_or("animation part-opacity member list missing")?;
    let mut matches = members.children().filter(|n| n.is_element() && n.attribute("xs.ref") == Some(id));
    let member = matches.next().ok_or("animation part-opacity member missing")?;
    if matches.next().is_some() {
        return Err("animation part-opacity member is ambiguous".into());
    }
    let sequence = inline_field(attribute, "valueData")?;
    let backref = raw_inherited_field(sequence, "attr")?;
    let references = [value, member, backref];
    for reference in references {
        if reference.attribute("xs.ref") != Some(id) || reference.has_children()
            || reference.attributes().any(|a| !matches!(a.name(), "xs.n" | "xs.ref"))
        {
            return Err("animation part-opacity storage has an aliased reference".into());
        }
    }
    Ok(PartCurveStorage { entry, member, attribute, references })
}

fn mapped_part_storage<'d, 'i>(
    graph: &Graph<'d, 'i>, effect: &PartEffect<'d, 'i>, curve: &Value, field: &str, budget: &mut Budget,
) -> Result<MappedPartStorage<'d, 'i>, String> {
    let key = if field == "underscorePartOpacityCurves" {
        curve["mapKey"].as_str().ok_or("animation underscore map identity missing")?.to_owned()
    } else {
        let part = curve["legacyMapPartId"].as_str().ok_or("animation legacy map identity missing")?;
        format!("live2DPartsOpacity:{part}")
    };
    let (entry, attribute) = part_map_attribute(graph, effect, &key, curve, budget)?;
    let id = attribute.attribute("xs.id").unwrap();
    let sequence = inline_field(attribute, "valueData")?;
    let mut references = vec![raw_field(entry, "value")?, raw_inherited_field(sequence, "attr")?];
    let mut members = effect.members.into_iter().flat_map(|list| list.children())
        .filter(|n| n.is_element() && n.attribute("xs.ref") == Some(id));
    let member = members.next();
    if let Some(member) = member { references.push(member); }
    if members.next().is_some() {
        return Err("animation legacy part-opacity member is ambiguous".into());
    }
    exclusive_references(id, &references, "part-opacity", budget)?;
    Ok(MappedPartStorage { entry, member, attribute, references })
}

fn mapped_part_curve_changes(
    text: &str,
    graph: &Graph<'_, '_>,
    track: Node<'_, '_>,
    old: &Value,
    new: &Value,
    field: &str,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
) -> Result<(isize, isize), String> {
    let old_curves = old[field].as_array().ok_or("animation mapped Part curves missing")?;
    let new_curves = new[field].as_array().ok_or("animation mapped Part curves missing")?;
    if old_curves == new_curves {
        return Ok((0, 0));
    }
    let effect = part_effect(graph, track, budget)?.ok_or("animation part-opacity effect missing")?;
    let identity = if field == "underscorePartOpacityCurves" { "mapKey" } else { "id" };
    let mut survivors = new_curves.iter().peekable();
    let (mut map_delta, mut member_delta) = (0, 0);
    for curve in old_curves {
        if survivors.peek().is_some_and(|target| target[identity] == curve[identity]) {
            let target = survivors.next().unwrap();
            if curve != target {
                let owned = mapped_part_storage(graph, &effect, curve, field, budget)?;
                curve_patches(text, graph, owned.attribute, curve, target, patches, budget)?;
            }
        } else {
            let owned = mapped_part_storage(graph, &effect, curve, field, budget)?;
            reject_nested_shared_storage(owned.entry, budget)?;
            track_owned_definition(graph, owned.attribute, track, owned.attribute.parent().unwrap(), budget)?;
            owned_descendants(owned.entry, &[], budget)?;
            owned_descendants(owned.attribute, &owned.references, budget)?;
            for node in [Some(owned.entry), owned.member, Some(owned.attribute)].into_iter().flatten() {
                patches.push((node.range(), String::new()));
            }
            map_delta -= 1;
            member_delta -= isize::from(owned.member.is_some());
        }
    }
    if survivors.next().is_some() {
        return Err("mapped PartOpacity curve addition or reordering is unsupported".into());
    }
    Ok((map_delta, member_delta))
}

fn owned_descendants(node: Node<'_, '_>, allowed: &[Node<'_, '_>], budget: &mut Budget) -> Result<(), String> {
    let ids: HashSet<&str> = node.descendants().filter_map(|n| n.attribute("xs.id")).collect();
    for reference in node.document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if ids.contains(reference.attribute("xs.ref").unwrap_or(""))
            && !reference.ancestors().any(|n| n.id() == node.id())
            && !allowed.iter().any(|n| n.id() == reference.id())
        {
            return Err("animation removed storage is referenced outside its owner".into());
        }
    }
    Ok(())
}

fn part_opacity_curve_changes(
    text: &str,
    graph: &Graph<'_, '_>,
    track: Node<'_, '_>,
    old: &Value,
    new: &Value,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
    additions: &mut NativeAdditions,
) -> Result<(isize, isize), String> {
    let old_curves = old["partOpacityCurves"].as_array().ok_or("animation part curves missing")?;
    let new_curves = new["partOpacityCurves"].as_array().ok_or("animation part curves missing")?;
    if old_curves == new_curves {
        return Ok((0, 0));
    }
    let plan = curve_set_plan(old_curves, new_curves)?;
    let Some(effect) = part_effect(graph, track, budget)? else {
        if !old_curves.is_empty() || track.attribute("xs.id").is_none()
            || track.parent().is_none_or(|n| !n.has_tag_name("shared"))
        {
            return Err("animation track lacks owned part-opacity storage".into());
        }
        let track_ref = track.attribute("xs.id").unwrap();
        let mut storage = Vec::new();
        for curve in new_curves {
            check_new_part_curve(curve)?;
            let id = if let Some(owned) = unmapped_part_storage(graph, None, track, curve, budget)? {
                let mut original = curve.clone();
                original["points"] = json!([]);
                original["curveTypes"] = json!([]);
                curve_patches(text, graph, owned.attribute, &original, curve, patches, budget)?;
                owned.attribute.attribute("xs.id").unwrap().to_owned()
            } else {
                let id = free_storage_id(&mut additions.reserved,
                    &format!("soul2d-part-curve-{track_ref}-{}", curve["id"].as_str().unwrap()));
                additions.definitions.push_str(&serialize_part_curve(&id, track_ref, curve)?);
                id
            };
            storage.push((id, curve));
        }
        let field = serialize_parts_effect_refs(additions, track_ref, &storage)?;
        if graph.field(track, "partsVisibleEffect")?.is_some() {
            let raw = raw_inherited_field(track, "partsVisibleEffect")?;
            if !raw.has_tag_name("null") || raw.attributes().any(|a| a.name() != "xs.n") || raw.has_children() {
                return Err("animation part-opacity null field is shared or aliased".into());
            }
            patches.push((raw.range(), field));
        } else {
            let end = list_end(track, text)?;
            emit_inserts(patches, [None].into_iter(), &[], &end, [field].into_iter());
        }
        return Ok((0, 0));
    };
    if plan.removed.is_empty() && plan.ordered.iter().all(|slot| matches!(slot, CurveSlot::Kept { .. })) {
        for slot in &plan.ordered {
            let CurveSlot::Kept { original, candidate } = *slot else { unreachable!() };
            if old_curves[original] != new_curves[candidate] {
                let owned = part_curve_storage(graph, &effect, &old_curves[original], budget)?;
                exclusive_references(owned.attribute.attribute("xs.id").unwrap(), &owned.references, "part-opacity", budget)?;
                curve_patches(text, graph, owned.attribute, &old_curves[original], &new_curves[candidate], patches, budget)?;
            }
        }
        return Ok((0, 0));
    }
    let storage = old_curves.iter().map(|curve| part_curve_storage(graph, &effect, curve, budget))
        .collect::<Result<Vec<_>, _>>()?;
    let entries: Vec<_> = effect.map.children().filter(Node::is_element).collect();
    let members: Vec<_> = effect.members.into_iter().flat_map(|list| list.children()).filter(Node::is_element).collect();
    for removed in &plan.removed {
        let owned = &storage[*removed];
        exclusive_references(owned.attribute.attribute("xs.id").unwrap(), &owned.references, "part-opacity", budget)?;
        owned_descendants(owned.entry, &[], budget)?;
        owned_descendants(owned.attribute, &[owned.entry, owned.member,
            raw_field(owned.entry, "value")?], budget)?;
        for node in [owned.entry, owned.member, owned.attribute] {
            patches.push((node.range(), String::new()));
        }
    }
    let mut map_slots = Vec::new();
    let mut member_slots = Vec::new();
    let mut map_inserts = Vec::new();
    let mut member_inserts = Vec::new();
    let mut adopted_members = 0;
    for slot in &plan.ordered {
        match *slot {
            CurveSlot::Kept { original, candidate } | CurveSlot::Relocated { original, candidate } => {
                let owned = &storage[original];
                if old_curves[original] != new_curves[candidate] || matches!(slot, CurveSlot::Relocated { .. }) {
                    exclusive_references(owned.attribute.attribute("xs.id").unwrap(), &owned.references, "part-opacity", budget)?;
                }
                curve_patches(text, graph, owned.attribute, &old_curves[original], &new_curves[candidate], patches, budget)?;
                if matches!(slot, CurveSlot::Kept { .. }) {
                    map_slots.push(Some(entries.iter().position(|n| n.id() == owned.entry.id()).unwrap()));
                    member_slots.push(Some(members.iter().position(|n| n.id() == owned.member.id()).unwrap()));
                } else {
                    map_slots.push(None);
                    member_slots.push(None);
                    map_inserts.push(text[owned.entry.range()].to_owned());
                    member_inserts.push(text[owned.member.range()].to_owned());
                    patches.push((owned.entry.range(), String::new()));
                    patches.push((owned.member.range(), String::new()));
                }
            }
            CurveSlot::Added { candidate } => {
                let curve = &new_curves[candidate];
                check_new_part_curve(curve)?;
                let key = format!("live2DPartsOpacity:{}", curve["partId"].as_str().unwrap());
                for entry in &entries {
                    if required(graph, *entry, "key")?.attribute("idstr") == Some(key.as_str()) {
                        return Err("animation Part curve map key already exists".into());
                    }
                }
                let track_ref = track.attribute("xs.id").ok_or("animation track lacks storage identity")?;
                if let Some(owned) = unmapped_part_storage(graph, effect.members, track, curve, budget)? {
                    let id = owned.attribute.attribute("xs.id").unwrap();
                    let mut original = curve.clone();
                    original["points"] = json!([]);
                    original["curveTypes"] = json!([]);
                    curve_patches(text, graph, owned.attribute, &original, curve, patches, budget)?;
                    map_inserts.push(part_map_entry(id, curve)?);
                    if let Some(member) = owned.member {
                        member_slots.push(Some(members.iter().position(|n| n.id() == member.id()).unwrap()));
                        adopted_members += 1;
                    } else {
                        member_inserts.push(format!(r#"<CMvAttrF xs.ref="{}"/>"#, escape(id)));
                        member_slots.push(None);
                    }
                } else {
                    let id = free_storage_id(&mut additions.reserved,
                        &format!("soul2d-part-curve-{track_ref}-{}", curve["id"].as_str().unwrap()));
                    let clone = animation_clone::curve(text, track, &animation_clone::PART, track_ref, false, curve, &id,
                        additions)?;
                    match clone {
                        Some(clone) => additions.definitions.push_str(&clone.definition),
                        None => additions.definitions.push_str(&serialize_part_curve(&id, track_ref, curve)?),
                    }
                    map_inserts.push(part_map_entry(&id, curve)?);
                    member_inserts.push(format!(r#"<CMvAttrF xs.ref="{}"/>"#, escape(&id)));
                    member_slots.push(None);
                }
                map_slots.push(None);
            }
        }
    }
    let delta = new_curves.len() as isize - old_curves.len() as isize;
    emit_inserts(patches, map_slots.into_iter(), &entries, &list_end(effect.map, text)?, map_inserts.into_iter());
    if let Some(list) = effect.members {
        emit_inserts(patches, member_slots.into_iter(), &members, &list_end(list, text)?, member_inserts.into_iter());
        Ok((delta, delta - adopted_members))
    } else {
        reject_nested_shared_storage(effect.definition, budget)?;
        let backlink = raw_inherited_field(effect.definition, "track")?;
        if backlink.attribute("xs.ref") != track.attribute("xs.id") || backlink.has_children()
            || backlink.attributes().any(|a| !matches!(a.name(), "xs.n" | "xs.ref"))
            || graph.resolve(backlink)?.id() != track.id()
        {
            return Err("animation part-opacity track backlink differs from its owner".into());
        }
        let owner = backlink.parent().unwrap();
        if !effect.definition.has_tag_name("CMvEffect_Live2DPartsVisible")
            || (owner != effect.definition && !owner.has_tag_name("ICMvEffect"))
        {
            return Err("animation part-opacity member list owner is unsupported".into());
        }
        let list = format!(r#"<array xs.n="attrList" count="{}" type="ICMvAttr">{}</array>"#,
            member_inserts.len(), member_inserts.concat());
        emit_inserts(patches, [None].into_iter(), &[], &list_end(owner, text)?, [list].into_iter());
        Ok((delta, 0))
    }
}

fn direct_reference(graph: &Graph<'_, '_>, reference: Node<'_, '_>, target: Node<'_, '_>) -> Result<(), String> {
    if reference.attribute("xs.ref") != target.attribute("xs.id") || reference.has_children()
        || reference.attributes().any(|a| !matches!(a.name(), "xs.n" | "xs.ref"))
        || reference.tag_name() != target.tag_name() || graph.resolve(reference)?.id() != target.id()
    {
        return Err("animation reference is shared or aliased".into());
    }
    Ok(())
}

fn track_storage(
    graph: &Graph<'_, '_>,
    definition: Node<'_, '_>,
    scene: Node<'_, '_>,
    track: &Value,
    shared: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<(), String> {
    if !matches!(definition.tag_name().name(), "CMvTrack_Live2DModel_Source" | "CMvTrack_Image_Source" | "CMvTrack_JavaSoundTrack_Source" | "CMvTrack_GuideImage_Source") {
        return Err("animation track definition is not a supported track".into());
    }
    if definition.attribute("xs.id").is_none() || definition.attribute("xs.ref").is_some() {
        return Err("animation track definition lacks direct shared storage".into());
    }
    if definition.parent() != Some(shared) {
        return Err("animation track definition is not a direct shared entry".into());
    }
    reject_nested_shared_storage(definition, budget)?;
    for name in ["name", "start", "internalOffset", "duration"] {
        let field = raw_inherited_field(definition, name)?;
        if field.attribute("xs.ref").is_some() || field.attribute("xs.id").is_some() {
            return Err("animation track field uses shared or aliased storage".into());
        }
    }
    direct_reference(graph, raw_inherited_field(definition, "_sceneSource")?, scene)?;
    for (name, expected) in [("guid", &track["guid"]), ("_resourceGuid", &track["resourceGuid"])] {
        let field = raw_inherited_field(definition, name)?;
        let value = graph.resolve(field)?;
        if field.attribute("xs.ref").is_some() {
            direct_reference(graph, field, value)?;
            if value.parent() != Some(shared) { return Err("animation GUID is not a direct shared entry".into()); }
        } else if field.attribute("xs.id").is_some() {
            return Err("animation track GUID has nested shared storage".into());
        }
        if value.attribute("uuid") != expected.as_str() || value.has_children() {
            return Err("animation track GUID differs from its model".into());
        }
    }
    Ok(())
}

fn reject_nested_shared_storage(definition: Node<'_, '_>, budget: &mut Budget) -> Result<(), String> {
    for node in definition.descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if node.id() != definition.id() && (node.attribute("xs.id").is_some()
            || (node.attribute("xs.n") == Some("super") && node.attribute("xs.ref").is_some()))
        {
            return Err("animation part-opacity definition contains nested shared storage".into());
        }
    }
    Ok(())
}

fn track_owned_definition(
    graph: &Graph<'_, '_>, definition: Node<'_, '_>, track: Node<'_, '_>, shared: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<(), String> {
    if definition.parent() != Some(shared) || definition.attribute("xs.id").is_none()
        || definition.attribute("xs.ref").is_some()
    {
        return Err("animation part-opacity definition lacks direct shared storage".into());
    }
    reject_nested_shared_storage(definition, budget)?;
    let backlink = raw_inherited_field(definition, "track")?;
    if backlink.attribute("xs.ref") != track.attribute("xs.id") || backlink.has_children()
        || backlink.attributes().any(|a| !matches!(a.name(), "xs.n" | "xs.ref"))
        || graph.resolve(backlink)?.id() != track.id()
    {
        return Err("animation part-opacity track backlink differs from its owner".into());
    }
    Ok(())
}

struct TrackDeletion<'d, 'i> {
    definitions: Vec<Node<'d, 'i>>,
    parent_member: Option<(Node<'d, 'i>, Node<'d, 'i>)>,
    /// Entries of the scene's parameter-control calculation order that name the track, with their list.
    items: Vec<(Node<'d, 'i>, Node<'d, 'i>)>,
}

fn group_children<'d, 'i>(
    graph: &Graph<'d, 'i>, group: Node<'d, 'i>, source: Node<'d, 'i>,
    scene: Node<'d, 'i>, shared: Node<'d, 'i>, budget: &mut Budget,
) -> Result<Node<'d, 'i>, String> {
    if !group.has_tag_name("CMvTrack_Group_Source") || group.parent() != Some(shared)
        || group.attribute("xs.id").is_none() || group.attribute("xs.ref").is_some()
    {
        return Err("animation group is not a direct shared entry".into());
    }
    direct_reference(graph, source, group)?;
    let guid = required(graph, group, "guid")?;
    if !guid.has_tag_name("CTrackGuid") || guid.parent() != Some(shared)
        || guid.attribute("uuid").is_none() || guid.has_children()
    {
        return Err("animation group GUID storage is unsupported".into());
    }
    direct_reference(graph, raw_inherited_field(group, "guid")?, guid)?;
    direct_reference(graph, raw_inherited_field(group, "_sceneSource")?, scene)?;
    reject_nested_shared_storage(group, budget)?;
    let list = raw_inherited_field(group, "_childTrackGuids")?;
    if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
        return Err("animation group children use shared storage".into());
    }
    values::list(graph, list, budget)?;
    let mut seen = HashSet::new();
    for member in list.children().filter(Node::is_element) {
        let target = graph.resolve(member)?;
        direct_reference(graph, member, target)?;
        if !target.has_tag_name("CTrackGuid") || target.attribute("uuid").is_none()
            || member.attributes().count() != 1 || !seen.insert(target.attribute("uuid"))
        {
            return Err("animation group child membership is aliased or duplicated".into());
        }
    }
    let id = group.attribute("xs.id").unwrap();
    let root_member = if graph.field(scene, "rootTrack")? == Some(group) {
        let member = raw_inherited_field(scene, "rootTrack")?;
        direct_reference(graph, member, group)?;
        Some(member)
    } else { None };
    for reference in group.document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if reference.attribute("xs.ref") == Some(id) && reference != source
            && Some(reference) != root_member
            && !reference.ancestors().any(|owner| owner == group)
        {
            return Err("animation group storage is referenced outside its owner".into());
        }
    }
    Ok(list)
}

fn track_parent_member<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, scene: Node<'d, 'i>,
    guid: Node<'d, 'i>, budget: &mut Budget,
) -> Result<Option<(Node<'d, 'i>, Node<'d, 'i>)>, String> {
    let Some(parent) = graph.field(track, "parentGuid")? else { return Ok(None) };
    if parent.has_tag_name("null") { return Ok(None); }
    if !parent.has_tag_name("CTrackGuid") || !guid.has_tag_name("CTrackGuid") || parent.parent() != track.parent() {
        return Err("animation group GUID storage is unsupported".into());
    }
    direct_reference(graph, raw_inherited_field(track, "parentGuid")?, parent)?;
    let sources = required(graph, scene, "trackSourceSet")?;
    let source_list = raw_inherited_field(sources, "_sources")?;
    values::list(graph, source_list, budget)?;
    let mut selected = None;
    for source in source_list.children().filter(Node::is_element) {
        let group = graph.resolve(source)?;
        if !group.has_tag_name("CMvTrack_Group_Source") || graph.field(group, "guid")? != Some(parent) { continue; }
        let list = group_children(graph, group, source, scene, track.parent().unwrap(), budget)?;
        for member in list.children().filter(Node::is_element) {
            let target = graph.resolve(member)?;
            if target != guid && target.attribute("uuid") == guid.attribute("uuid") {
                return Err("animation group child membership is aliased or duplicated".into());
            }
            if target == guid && selected.replace((list, member)).is_some() {
                return Err("animation track has multiple parent memberships".into());
            }
        }
    }
    selected.map(Some).ok_or_else(|| "animation track lacks exactly one parent membership".into())
}

fn unowned_parameter_declaration(effect: Node<'_, '_>, attribute: Node<'_, '_>, shared: Node<'_, '_>) -> bool {
    if !matches!(effect.tag_name().name(), "CMvKeyParamEffect" | "CMvEffect_Live2DParameter")
        || !attribute.has_tag_name("CMvAttrF") || attribute.parent() != Some(shared)
        || attribute.attribute("xs.id").is_none() || attribute.attribute("xs.ref").is_some()
    { return false; }
    let mut fields = attribute.children().filter(Node::is_element);
    let Some(id) = fields.next() else { return false };
    fields.next().is_none() && id.has_tag_name("CAttrId") && id.attribute("xs.n") == Some("id")
        && id.attributes().count() == 2 && !id.has_children()
        && id.attribute("idstr").is_some_and(|id| !id.starts_with("live2dParam_") && !id.starts_with("live2DPartsOpacity"))
}

fn parent_attribute_storage(
    graph: &Graph<'_, '_>, attribute: Node<'_, '_>, track: Node<'_, '_>,
    shared: Node<'_, '_>, budget: &mut Budget,
) -> Result<(), String> {
    let (field, kind) = match attribute.tag_name().name() {
        "CMvAttrF" => ("valueData", "CMutableSequence"),
        "CMvAttrI" => ("valueData", "CIntSequence"),
        "CMvAttrPt" => ("valueDataXY", "CXY_TNSSequence"),
        _ => return Err("animation track contains an unsupported attribute type".into()),
    };
    track_owned_definition(graph, attribute, track, shared, budget)?;
    let sequence = raw_inherited_field(attribute, field)?;
    let fixed = attribute.has_tag_name("CMvAttrF") && sequence.has_tag_name("CFixedSequence");
    if (!sequence.has_tag_name(kind) && !fixed) || sequence.attribute("xs.id").is_some() || sequence.attribute("xs.ref").is_some() {
        return Err("animation track attribute has unsupported sequence storage".into());
    }
    // Cubism sometimes writes a sequence's backlink to its attribute as null.
    let backlink = raw_inherited_field(sequence, "attr")?;
    if !(backlink.has_tag_name("null") && backlink.attributes().count() == 1 && !backlink.has_children()) {
        direct_reference(graph, backlink, attribute)?;
    }
    if fixed {
        if graph.field(sequence, "points")?.is_some() || graph.field(sequence, "curveTypes")?.is_some() {
            return Err("animation fixed attribute has unsupported key storage".into());
        }
        return Ok(());
    }
    let points = raw_inherited_field(sequence, "points")?;
    if points.attribute("xs.id").is_some() || points.attribute("xs.ref").is_some() {
        return Err("animation track attribute points use shared storage".into());
    }
    let count = values::list(graph, points, budget)?.len();
    if attribute.has_tag_name("CMvAttrF") {
        let types = raw_inherited_field(sequence, "curveTypes")?;
        if types.attribute("xs.id").is_some() || types.attribute("xs.ref").is_some()
            || values::list(graph, types, budget)?.len() != count
        {
            return Err("animation track attribute curve types differ from its keys".into());
        }
    }
    Ok(())
}

fn sound_track_storage(graph: &Graph<'_, '_>, track: Node<'_, '_>, shared: Node<'_, '_>, budget: &mut Budget) -> Result<(), String> {
    let effect = track_effect(graph, track, "soundEffect", "CMvEffect_SoundDefault", "Sound", budget)?
        .ok_or("Sound track lacks its effect")?;
    if !effect.has_tag_name("CMvEffect_SoundDefault") || effect.parent() != Some(shared) {
        return Err("Sound effect must be a shared definition".into());
    }
    for field in ["keyParamEffect", "partsVisibleEffect", "visualEffect", "lipSyncEffect", "eyeBlinkEffect",
        "paramCtrlGroupEffect", "formEditEffect", "paramCtrlGroupSet", "formAnimationSet"] {
        if graph.field(track, field)?.is_some_and(|node| !node.has_tag_name("null")) {
            return Err("Sound track contains another effect or control family".into());
        }
    }
    let list = raw_inherited_field(effect, "attrList")?;
    let map = raw_inherited_field(effect, "attrMap")?;
    for node in [list, map] {
        if node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some()
            || values::list(graph, node, budget)?.len() != 1 {
            return Err("Sound effect requires one owned volume attribute".into());
        }
    }
    if !list.has_tag_name("array") || !map.has_tag_name("hash_map") { return Err("Sound attribute lists are unsupported".into()); }
    let member = list.children().find(Node::is_element).unwrap();
    let attribute = graph.resolve(member)?;
    let entry = map.children().find(Node::is_element).unwrap();
    if !entry.has_tag_name("entry") || entry.attributes().len() != 0 || entry.children().filter(Node::is_element).count() != 2 {
        return Err("Sound attribute map entry is unsupported".into());
    }
    let key = required(graph, entry, "key")?;
    let value = raw_field(entry, "value")?;
    let named = raw_inherited_field(effect, "attrVolume")?;
    if !attribute.has_tag_name("CMvAttrF") || !key.has_tag_name("CAttrId") || key.attribute("idstr") != Some("volume")
        || required(graph, attribute, "id")?.attribute("idstr") != Some("volume") {
        return Err("Sound effect requires the volume attribute".into());
    }
    for reference in [member, value, named] { direct_reference(graph, reference, attribute)?; }
    parent_attribute_storage(graph, attribute, track, shared, budget)?;
    let sequence = raw_inherited_field(attribute, "valueData")?;
    if !sequence.has_tag_name("CFixedSequence") { return Err("Sound volume requires a fixed sequence".into()); }
    let backlink = raw_inherited_field(sequence, "attr")?;
    direct_reference(graph, backlink, attribute)?;
    exclusive_references(attribute.attribute("xs.id").unwrap(), &[member, value, named, backlink], "Sound volume", budget)?;
    let id = required(graph, effect, "id")?;
    if !id.has_tag_name("CEffectId") || id.attribute("idstr") != Some("SoundDefault") {
        return Err("Sound effect identity differs from SoundDefault".into());
    }
    let mut attribute_refs = vec![raw_inherited_field(attribute, "track")?, backlink];
    for (field, kind) in [("id", "CAttrId"), ("adaptType", "AdaptType")] {
        if graph.field(attribute, field)?.is_some() {
            let reference = raw_inherited_field(attribute, field)?;
            if reference.attribute("xs.ref").is_some() {
                let target = graph.resolve(reference)?;
                if !target.has_tag_name(kind) || target.parent() != Some(shared) { return Err("Sound volume has unsupported shared metadata".into()); }
                direct_reference(graph, reference, target)?;
                attribute_refs.push(reference);
            }
        }
    }
    for reference in attribute.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !attribute_refs.contains(&reference) { return Err("Sound volume contains an unclassified reference".into()); }
    }
    let manager = raw_inherited_field(track, "effectManager")?;
    let registry = raw_field(manager, "effectList")?;
    let members = values::list(graph, registry, budget)?;
    if members != [effect] { return Err("Sound effect registry must contain exactly its SoundDefault".into()); }
    let registry_member = registry.children().find(Node::is_element).unwrap();
    let mut track_refs = vec![raw_inherited_field(track, "soundEffect")?, registry_member];
    for field in ["guid", "parentGuid", "_sceneSource", "_resourceGuid"] {
        if graph.field(track, field)?.is_some() { track_refs.push(raw_inherited_field(track, field)?); }
    }
    for (field, kind) in [("visualHandler", "CVisualHandler"), ("soundHandler", "CSoundHandler")] {
        let handler = raw_inherited_field(track, field)?;
        if !handler.has_tag_name(kind) || handler.attribute("xs.id").is_some() || handler.attribute("xs.ref").is_some() {
            return Err("Sound handler storage is unsupported".into());
        }
        let reference = raw_field(handler, "track")?;
        direct_reference(graph, reference, track)?;
        track_refs.push(reference);
    }
    for reference in track.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !track_refs.contains(&reference) { return Err("Sound track contains an unclassified reference".into()); }
    }
    Ok(())
}

fn guide_track_storage(graph: &Graph<'_, '_>, track: Node<'_, '_>, shared: Node<'_, '_>, budget: &mut Budget) -> Result<(), String> {
    for field in ["soundEffect", "keyParamEffect", "partsVisibleEffect", "lipSyncEffect", "eyeBlinkEffect",
        "paramCtrlGroupEffect", "formEditEffect", "paramCtrlGroupSet", "formAnimationSet"] {
        if graph.field(track, field)?.is_some_and(|node| !node.has_tag_name("null")) {
            return Err("Guide track contains another effect or control family".into());
        }
    }
    let visual = track_effect(graph, track, "visualEffect", "CMvEffect_VisualDefault", "Guide visual", budget)?
        .ok_or("Guide track lacks its visual effect")?;
    let guide = track_effect(graph, track, "guideImageEffect", "CMvEffect_GuideImageDefault", "Guide image", budget)?
        .ok_or("Guide track lacks its image effect")?;
    let mut lists = Vec::new();
    for (effect, kind, count) in [(visual, "VisualDefault", 9), (guide, "GuideImageDefault", 1)] {
        let identity = required(graph, effect, "id")?;
        if effect.parent() != Some(shared) || !identity.has_tag_name("CEffectId") || identity.attribute("idstr") != Some(kind) {
            return Err("Guide effect identity is unsupported".into());
        }
        let list = raw_inherited_field(effect, "attrList")?;
        let map = raw_inherited_field(effect, "attrMap")?;
        if !list.has_tag_name("array") || !map.has_tag_name("hash_map") || map.attribute("keyType") != Some("string")
            || list.attribute("count").and_then(|value| value.parse::<usize>().ok()) != Some(count)
            || map.attribute("count").and_then(|value| value.parse::<usize>().ok()) != Some(0)
            || [list, map].iter().any(|node| node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some())
            || values::list(graph, list, budget)?.len() != count || !values::list(graph, map, budget)?.is_empty() {
            return Err("Guide effect attribute storage is unsupported".into());
        }
        lists.push(list);
    }
    let visual_members: Vec<_> = lists[0].children().filter(Node::is_element).collect();
    let mut attributes = Vec::new();
    for (id, name, kind) in [
        ("xy", "attrXY", "CMvAttrPt"), ("scalex", "attrScaleX", "CMvAttrF"),
        ("scaley", "attrScaleY", "CMvAttrF"), ("rotate", "attrRotate", "CMvAttrF"),
        ("shear", "attrShear", "CMvAttrF"), ("anchor", "attrAnchorXY", "CMvAttrPt"),
        ("opacity", "attrOpacity", "CMvAttrF"), ("artPathWidth", "attrArtPathWidth", "CMvAttrF"),
        ("frameStep", "attrFrameStep", "CMvAttrI"),
    ] {
        let named = raw_inherited_field(visual, name)?;
        let attribute = graph.resolve(named)?;
        let identity = required(graph, attribute, "id")?;
        let members: Vec<_> = visual_members.iter().copied()
            .filter(|member| member.attribute("xs.ref") == attribute.attribute("xs.id")).collect();
        if members.len() != 1 || !attribute.has_tag_name(kind) || !identity.has_tag_name("CAttrId")
            || identity.attribute("idstr") != Some(id) {
            return Err("Guide visual attribute identity is unsupported".into());
        }
        attributes.push((attribute, vec![members[0], named]));
    }
    let member = lists[1].children().find(Node::is_element).unwrap();
    let listed = graph.resolve(member)?;
    let named = raw_inherited_field(guide, "attrImageNo")?;
    let image_no = graph.resolve(named)?;
    if listed == image_no { return Err("Guide image attributes must have distinct storage".into()); }
    for (attribute, reference) in [(listed, member), (image_no, named)] {
        let identity = required(graph, attribute, "id")?;
        if !attribute.has_tag_name("CMvAttrF") || !identity.has_tag_name("CAttrId") || identity.attribute("idstr") != Some("guideimageno") {
            return Err("Guide image attribute identity is unsupported".into());
        }
        attributes.push((attribute, vec![reference]));
    }
    let mut seen = HashSet::new();
    for (attribute, mut references) in attributes {
        if !seen.insert(attribute.id()) { return Err("Guide attributes share mutable storage".into()); }
        for reference in &references { direct_reference(graph, *reference, attribute)?; }
        parent_attribute_storage(graph, attribute, track, shared, budget)?;
        let (field, kind) = match attribute.tag_name().name() {
            "CMvAttrPt" => ("valueDataXY", "CXY_TNSSequence"),
            "CMvAttrI" => ("valueData", "CIntSequence"),
            _ => ("valueData", "CMutableSequence"),
        };
        let sequence = raw_inherited_field(attribute, field)?;
        if !sequence.has_tag_name(kind) { return Err("Guide attribute sequence is unsupported".into()); }
        let backlink = raw_inherited_field(sequence, "attr")?;
        direct_reference(graph, backlink, attribute)?;
        references.push(backlink);
        exclusive_references(attribute.attribute("xs.id").unwrap(), &references, "Guide attribute", budget)?;
        let mut outgoing = vec![raw_inherited_field(attribute, "track")?, backlink];
        for (field, kind) in [("id", "CAttrId"), ("adaptType", "AdaptType")] {
            if graph.field(attribute, field)?.is_some() {
                let reference = raw_inherited_field(attribute, field)?;
                if reference.attribute("xs.ref").is_some() {
                    let target = graph.resolve(reference)?;
                    if !target.has_tag_name(kind) || target.parent() != Some(shared) {
                        return Err("Guide attribute has unsupported shared metadata".into());
                    }
                    direct_reference(graph, reference, target)?;
                    outgoing.push(reference);
                }
            }
        }
        for reference in attribute.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
            if !outgoing.contains(&reference) { return Err("Guide attribute contains an unclassified reference".into()); }
        }
    }
    let manager = raw_inherited_field(track, "effectManager")?;
    let registry = raw_field(manager, "effectList")?;
    if values::list(graph, registry, budget)? != [guide, visual] {
        return Err("Guide effect registry is unsupported".into());
    }
    let mut track_refs: Vec<_> = registry.children().filter(Node::is_element).collect();
    for field in ["guid", "parentGuid", "_sceneSource", "_resourceGuid", "visualEffect", "guideImageEffect"] {
        track_refs.push(raw_inherited_field(track, field)?);
    }
    for (field, kind) in [("visualHandler", "CVisualHandler"), ("soundHandler", "CSoundHandler")] {
        let handler = raw_inherited_field(track, field)?;
        if !handler.has_tag_name(kind) || handler.attribute("xs.id").is_some() || handler.attribute("xs.ref").is_some() {
            return Err("Guide handler storage is unsupported".into());
        }
        let reference = raw_field(handler, "track")?;
        direct_reference(graph, reference, track)?;
        track_refs.push(reference);
    }
    for reference in track.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !track_refs.contains(&reference) { return Err("Guide track contains an unclassified reference".into()); }
    }
    Ok(())
}

fn form_inline_list<'d, 'i>(
    graph: &Graph<'d, 'i>, node: Node<'d, 'i>, kind: &str, budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    if !node.has_tag_name(kind) || node.attribute("xs.id").is_some() || node.attribute("xs.ref").is_some()
        || node.attribute("count").and_then(|value| value.parse::<usize>().ok()).is_none() {
        return Err("FormEdit requires an inline collection with an explicit count".into());
    }
    values::list(graph, node, budget)?;
    Ok(node.children().filter(Node::is_element).collect())
}

fn form_plain_null(node: Node<'_, '_>) -> bool {
    node.has_tag_name("null") && node.attributes().count() == 1 && !node.has_children()
}

fn form_dummy_storage(
    graph: &Graph<'_, '_>, dummy: Node<'_, '_>, attribute_refs: &[Node<'_, '_>],
    shared: Node<'_, '_>, budget: &mut Budget,
) -> Result<(), String> {
    if !dummy.has_tag_name("DummyTrackSource") || dummy.parent() != Some(shared)
        || dummy.attribute("xs.id").is_none() || dummy.attribute("xs.ref").is_some() {
        return Err("FormEdit inline keys require a private dummy track".into());
    }
    reject_nested_shared_storage(dummy, budget)?;
    let base = raw_field(dummy, "super")?;
    if !base.has_tag_name("ICMvTrack_Source") || dummy.children().filter(Node::is_element).count() != 1 {
        return Err("FormEdit dummy inheritance is unsupported".into());
    }
    for field in base.children().filter(Node::is_element) {
        let kind = match field.attribute("xs.n") {
            Some("name") => "s", Some("guid") => "CTrackGuid",
            Some("start" | "internalOffset" | "duration") => "i",
            Some("isUserRenamed" | "editable" | "visible" | "mute" | "isGuide" | "isRepeat" | "soloSwitch") => "b",
            Some("visualHandler") => "CVisualHandler", Some("soundHandler") => "CSoundHandler",
            Some("effectManager") => "CMvEffectManager", Some("userData") => "hash_map",
            Some("soundEffect" | "visualEffect" | "parentGuid" | "_sceneSource" | "keys") => "null",
            _ => return Err("FormEdit dummy contains an unknown field".into()),
        };
        if !field.has_tag_name(kind) || field.attribute("xs.ref").is_some() {
            return Err("FormEdit dummy field storage is unsupported".into());
        }
    }
    let guid = raw_field(base, "guid")?;
    if guid.attribute("uuid").is_none() || guid.has_children() {
        return Err("FormEdit dummy GUID is not inline".into());
    }
    for field in ["soundEffect", "visualEffect", "parentGuid", "_sceneSource", "keys"] {
        if !form_plain_null(raw_field(base, field)?) { return Err("FormEdit dummy has active track state".into()); }
    }
    let manager = raw_field(base, "effectManager")?;
    if manager.children().filter(Node::is_element).count() != 1
        || !form_inline_list(graph, raw_field(manager, "effectList")?, "array", budget)?.is_empty()
        || !form_inline_list(graph, raw_field(base, "userData")?, "hash_map", budget)?.is_empty() {
        return Err("FormEdit dummy has effects or user data".into());
    }
    let mut incoming = attribute_refs.to_vec();
    let mut handlers = Vec::new();
    for field in ["visualHandler", "soundHandler"] {
        let handler = raw_field(base, field)?;
        if handler.children().filter(Node::is_element).count() != 1 {
            return Err("FormEdit dummy handler is unsupported".into());
        }
        let reference = raw_field(handler, "track")?;
        direct_reference(graph, reference, dummy)?;
        handlers.push(reference);
        incoming.push(reference);
    }
    for reference in dummy.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !handlers.contains(&reference) { return Err("FormEdit dummy has an unclassified reference".into()); }
    }
    exclusive_references(dummy.attribute("xs.id").unwrap(), &incoming, "FormEdit dummy", budget)
}

fn form_borrowed_attribute(
    graph: &Graph<'_, '_>, attribute: Node<'_, '_>, track: Node<'_, '_>,
    shared: Node<'_, '_>, budget: &mut Budget,
) -> Result<(), String> {
    let owner_ref = raw_inherited_field(attribute, "track")?;
    let owner = graph.resolve(owner_ref)?;
    if !attribute.has_tag_name("CMvAttrI") || !owner.has_tag_name("CMvTrack_Live2DModel_Source")
        || owner == track || owner.parent() != Some(shared) {
        return Err("borrowed frame-step attribute has unsupported ownership".into());
    }
    parent_attribute_storage(graph, attribute, owner, shared, budget)?;
    let sequence = raw_field(attribute, "valueData")?;
    let backlink = raw_inherited_field(sequence, "attr")?;
    direct_reference(graph, backlink, attribute)?;
    let mut allowed = vec![owner_ref, backlink];
    for (field, kind) in [("id", "CAttrId"), ("adaptType", "AdaptType")] {
        if let Some(value) = graph.field(attribute, field)? {
            if !value.has_tag_name(kind) { return Err("borrowed frame-step metadata is unsupported".into()); }
            let reference = raw_inherited_field(attribute, field)?;
            if reference.attribute("xs.ref").is_some() {
                direct_reference(graph, reference, value)?;
                if value.parent() != Some(shared) { return Err("borrowed frame-step metadata is not shared".into()); }
                allowed.push(reference);
            }
        }
    }
    for reference in attribute.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
        if !allowed.contains(&reference) { return Err("borrowed frame-step has an unclassified reference".into()); }
    }
    Ok(())
}

fn form_track_storage<'d, 'i>(
    graph: &Graph<'d, 'i>, track: Node<'d, 'i>, effect: Node<'d, 'i>, shared: Node<'d, 'i>,
    budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    if !track.has_tag_name("CMvTrack_Live2DModel_Source") {
        return Err("FormEdit requires a Model track".into());
    }
    let identity = required(graph, effect, "id")?;
    if !identity.has_tag_name("CEffectId") || identity.attribute("idstr") != Some("live2DFormEdit_") {
        return Err("FormEdit effect identity is unsupported".into());
    }
    let registry = raw_field(raw_inherited_field(track, "effectManager")?, "effectList")?;
    if values::list(graph, registry, budget)?.iter().filter(|node| **node == effect).count() != 1 {
        return Err("FormEdit is missing from the effect registry".into());
    }
    let set = raw_inherited_field(track, "formAnimationSet")?;
    if !set.has_tag_name("FormAnimationSet") || set.attribute("xs.ref").is_some() || set.attribute("xs.id").is_some() {
        return Err("FormEdit animation set uses shared storage".into());
    }
    direct_reference(graph, raw_field(set, "trackSource")?, track)?;
    if !form_inline_list(graph, raw_field(set, "formMapOnGlobal")?, "hash_map", budget)?.is_empty() {
        return Err("FormEdit global animation maps are unsupported".into());
    }
    for child in set.children().filter(Node::is_element) {
        if !matches!(child.attribute("xs.n"), Some("trackSource" | "formMapOnGlobal" | "formMapOnLocal")) {
            return Err("FormEdit animation set contains an unknown field".into());
        }
    }
    let entries = form_inline_list(graph, raw_field(set, "formMapOnLocal")?, "hash_map", budget)?;
    let mut definitions = Vec::new();
    let mut roots = HashSet::new();
    let mut attributes = HashSet::new();
    let mut extra_attributes = HashSet::new();
    let mut dummies: HashMap<_, (Node, Vec<Node>)> = HashMap::new();
    let mut identities = HashSet::new();
    let mut frames = HashSet::new();
    for entry in entries {
        if !entry.has_tag_name("entry") || entry.attributes().len() != 0
            || entry.children().filter(Node::is_element).count() != 2 {
            return Err("FormEdit map entry is unsupported".into());
        }
        let key = raw_field(entry, "key")?;
        let guid = graph.resolve(key)?;
        direct_reference(graph, key, guid)?;
        if guid.parent() != Some(shared) || !identities.insert(guid.id())
            || !matches!(guid.tag_name().name(), "CDeformerGuid" | "CDrawableGuid" | "CPartGuid") {
            return Err("FormEdit map key is not a retained identity".into());
        }
        let reference = raw_field(entry, "value")?;
        let object = graph.resolve(reference)?;
        let inline = reference.attribute("xs.ref").is_none();
        if !inline { direct_reference(graph, reference, object)?; }
        if (inline && object.attribute("xs.id").is_some()) || (!inline && object.parent() != Some(shared))
            || !roots.insert(object.id())
            || !matches!(object.tag_name().name(), "KeyFormsForObject" | "KeyFormsForArtMesh" | "KeyFormsForWarpDeformer") {
            return Err("FormEdit keyforms require distinct inline or shared storage".into());
        }
        reject_nested_shared_storage(object, budget)?;
        let base = if object.has_tag_name("KeyFormsForObject") { object } else {
            let base = raw_field(object, "super")?;
            if !base.has_tag_name("KeyFormsForObject") || base.attribute("xs.ref").is_some() {
                return Err("FormEdit keyform inheritance is unsupported".into());
            }
            base
        };
        for child in object.children().filter(Node::is_element) {
            let allowed = object == base || matches!((object.tag_name().name(), child.attribute("xs.n"), child.tag_name().name()),
                (_, Some("super"), "KeyFormsForObject") | ("KeyFormsForArtMesh", Some("indices"), "int-array")
                | ("KeyFormsForWarpDeformer", Some("col" | "row"), "i"));
            if !allowed { return Err("FormEdit keyform contains an unknown field".into()); }
        }
        for child in base.children().filter(Node::is_element) {
            if !matches!(child.attribute("xs.n"), Some("framePosKeyformList" | "cached_framePosKeyformMap"
                | "attrTimeRemapping" | "parentDeformerGuid" | "parentDeformerTypeName" | "guid" | "nameForDebug")) {
                return Err("FormEdit keyform contains an unknown field".into());
            }
        }
        direct_reference(graph, raw_field(base, "guid")?, guid)?;
        let parent = raw_field(base, "parentDeformerGuid")?;
        if inline {
            if !form_plain_null(parent) || !form_plain_null(raw_field(base, "parentDeformerTypeName")?) {
                return Err("inline FormEdit keyforms have a parent".into());
            }
        } else if !parent.has_tag_name("CDeformerGuid") { return Err("FormEdit parent identity is unsupported".into()); }
        if parent.attribute("xs.ref").is_some() {
            let target = graph.resolve(parent)?;
            direct_reference(graph, parent, target)?;
            if target.parent() != Some(shared) { return Err("FormEdit parent identity is not shared".into()); }
        }
        let attr_ref = raw_field(base, "attrTimeRemapping")?;
        let attribute = graph.resolve(attr_ref)?;
        direct_reference(graph, attr_ref, attribute)?;
        if !attribute.has_tag_name("CMvAttrF") || attributes.contains(&attribute.id())
            || extra_attributes.contains(&attribute.id()) {
            return Err("FormEdit time remapping must have one attribute owner".into());
        }
        if inline {
            extra_attributes.insert(attribute.id());
            let owner_ref = raw_inherited_field(attribute, "track")?;
            let dummy = graph.resolve(owner_ref)?;
            if !dummy.has_tag_name("DummyTrackSource") {
                return Err("inline FormEdit attribute requires a dummy owner".into());
            }
            parent_attribute_storage(graph, attribute, dummy, shared, budget)?;
            if !form_plain_null(raw_field(attribute, "linked_keyFormsForObject")?) {
                return Err("inline FormEdit attribute has a linked root".into());
            }
            direct_reference(graph, raw_inherited_field(attribute, "guid")?, guid)?;
            let sequence = raw_field(attribute, "valueData")?;
            if !sequence.has_tag_name("CMutableSequence") {
                return Err("inline FormEdit attribute requires an empty mutable sequence".into());
            }
            for field in ["points", "curveTypes"] {
                let list = raw_field(sequence, field)?;
                if !matches!(list.tag_name().name(), "array" | "carray_list")
                    || !form_inline_list(graph, list, list.tag_name().name(), budget)?.is_empty() {
                    return Err("inline FormEdit attribute has keys".into());
                }
            }
            let backlink = raw_inherited_field(sequence, "attr")?;
            direct_reference(graph, backlink, attribute)?;
            exclusive_references(attribute.attribute("xs.id").unwrap(), &[attr_ref, backlink], "inline FormEdit attribute", budget)?;
            dummies.entry(dummy.id()).or_insert_with(|| (dummy, Vec::new())).1.push(owner_ref);
            definitions.push(attribute);
        } else {
            attributes.insert(attribute.id());
            parent_attribute_storage(graph, attribute, track, shared, budget)?;
            direct_reference(graph, raw_field(attribute, "linked_keyFormsForObject")?, object)?;
        }
        let mut allowed_attr = vec![raw_inherited_field(attribute, "track")?, raw_field(attribute, "linked_keyFormsForObject")?,
            raw_inherited_field(raw_field(attribute, "valueData")?, "attr")?];
        for (field, kinds) in [("id", &["CAttrId"][..]), ("adaptType", &["AdaptType"][..]),
            ("guid", &["CDeformerGuid", "CDrawableGuid", "CPartGuid"][..])] {
            if let Some(value) = graph.field(attribute, field)? {
                let raw = raw_inherited_field(attribute, field)?;
                if !kinds.contains(&value.tag_name().name()) { return Err("FormEdit attribute metadata type is unsupported".into()); }
                if raw.attribute("xs.ref").is_some() {
                    direct_reference(graph, raw, value)?;
                    if value.parent() != Some(shared) { return Err("FormEdit attribute metadata is not shared".into()); }
                    allowed_attr.push(raw);
                }
            }
        }
        for reference in attribute.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
            if !allowed_attr.contains(&reference) { return Err("FormEdit attribute contains an unclassified reference".into()); }
        }
        let members = form_inline_list(graph, raw_field(base, "framePosKeyformList")?, "array_list", budget)?;
        if inline && !members.is_empty() { return Err("inline FormEdit keyforms have frames".into()); }
        let mut positions = HashMap::new();
        for member in members {
            let frame = graph.resolve(member)?;
            direct_reference(graph, member, frame)?;
            if !frame.has_tag_name("FramePosAndForm") || frame.parent() != Some(shared) || !frames.insert(frame.id()) {
                return Err("FormEdit frames have shared or unsupported ownership".into());
            }
            reject_nested_shared_storage(frame, budget)?;
            let position = frame.attribute("framePos").and_then(|value| value.parse::<i64>().ok())
                .ok_or("FormEdit frame position is invalid")?;
            if positions.insert(position, frame).is_some() { return Err("FormEdit has duplicate frame positions".into()); }
            if frame.children().filter(Node::is_element).count() != 2 {
                return Err("FormEdit frame contains unknown fields".into());
            }
            for field in ["targetForm", "originalForm"] {
                let form = raw_field(frame, field)?;
                if form.attribute("xs.ref").is_some() || !matches!(form.tag_name().name(),
                    "CArtMeshForm" | "CArtPathForm" | "CPartForm" | "CRotationDeformerForm" | "CWarpDeformerForm") {
                    return Err("FormEdit frame payload is unsupported".into());
                }
                for reference in form.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
                    let target = graph.resolve(reference)?;
                    direct_reference(graph, reference, target)?;
                    let role = (reference.parent().map(|node| node.tag_name().name()).unwrap_or(""),
                        reference.attribute("xs.n"), target.tag_name().name());
                    if target.parent() != Some(shared) || !matches!(role,
                        ("ACForm", Some("guid"), "CFormGuid")
                        | ("ACDeformerForm" | "ACDrawableForm", Some("coordType"), "CoordType")
                        | ("CArtPathPoint", Some("ctrlPtId"), "CArtPathPointGuid")
                        | ("CArtPathPoint", Some("ownerGuid"), "CDrawableGuid")) {
                        return Err("FormEdit payload contains an unclassified reference".into());
                    }
                }
            }
            definitions.push(frame);
        }
        let cache_node = raw_field(base, "cached_framePosKeyformMap")?;
        let cache_kind = if !inline && cache_node.has_tag_name("linked_map") { "linked_map" } else { "hash_map" };
        let cache = form_inline_list(graph, cache_node, cache_kind, budget)?;
        if cache.len() != positions.len() { return Err("FormEdit frame cache differs from its list".into()); }
        let mut cached = HashSet::new();
        for entry in cache {
            if !entry.has_tag_name("entry") || entry.attributes().len() != 0
                || entry.children().filter(Node::is_element).count() != 2 {
                return Err("FormEdit frame cache entry is unsupported".into());
            }
            let key = raw_field(entry, "key")?;
            let position = key.text().and_then(|value| value.parse::<i64>().ok()).filter(|_| key.has_tag_name("i"))
                .ok_or("FormEdit cache frame position is invalid")?;
            let frame = positions.get(&position).ok_or("FormEdit cache references an unknown frame")?;
            if !cached.insert(position) { return Err("FormEdit cache duplicates a frame".into()); }
            direct_reference(graph, raw_field(entry, "value")?, *frame)?;
        }
        for reference in object.descendants().filter(|node| node.attribute("xs.ref").is_some()) {
            let target = graph.resolve(reference)?;
            let field = reference.attribute("xs.n");
            let allowed = match field {
                Some("guid") => reference == raw_field(base, "guid")?,
                Some("parentDeformerGuid") => reference == parent,
                Some("attrTimeRemapping") => reference == attr_ref,
                Some("value") => positions.values().any(|frame| *frame == target)
                    && reference.parent().and_then(|node| node.parent()) == Some(raw_field(base, "cached_framePosKeyformMap")?),
                None => positions.values().any(|frame| *frame == target)
                    && reference.parent() == Some(raw_field(base, "framePosKeyformList")?),
                _ => false,
            };
            if !allowed { return Err("FormEdit keyform contains an unclassified reference".into()); }
        }
        if !inline { definitions.push(object); }
    }
    let effect_attrs: HashSet<_> = values::list(graph, raw_inherited_field(effect, "attrList")?, budget)?
        .iter().map(Node::id).collect();
    if effect_attrs != attributes { return Err("FormEdit attributes differ from its keyform owners".into()); }
    let entries = form_inline_list(graph, raw_inherited_field(effect, "attrMap")?, "hash_map", budget)?;
    let mapped: HashSet<_> = entries.iter().map(|entry| required(graph, *entry, "value").map(|node| node.id()))
        .collect::<Result<_, _>>()?;
    if !mapped.is_subset(&attributes) || entries.len() != mapped.len()
        || (extra_attributes.is_empty() && mapped != attributes) {
        return Err("FormEdit attribute map differs from its keyform owners".into());
    }
    for entry in entries {
        let raw_key = raw_field(entry, "key")?;
        let key = required(graph, entry, "key")?;
        let attribute = required(graph, entry, "value")?;
        let id = required(graph, attribute, "id")?;
        if (raw_key.attribute("xs.ref").is_some() && key != id)
            || key.attribute("idstr") != id.attribute("idstr") {
            return Err("FormEdit attribute map key differs from its target identity".into());
        }
    }
    for (dummy, references) in dummies.values() {
        form_dummy_storage(graph, *dummy, references, shared, budget)?;
        definitions.push(*dummy);
    }
    Ok(definitions)
}

fn track_definition_ownership<'d, 'i>(
    graph: &Graph<'d, 'i>, owner: Node<'d, 'i>, scene: Node<'d, 'i>,
    shared: Node<'d, 'i>, track: &Value, budget: &mut Budget,
) -> Result<TrackDeletion<'d, 'i>, String> {
    let definition = graph.resolve(owner)?;
    direct_reference(graph, owner, definition)?;
    if owner.attributes().count() != 1 { return Err("animation track member is not a bare reference".into()); }
    track_storage(graph, definition, scene, track, shared, budget)?;
    let sound = definition.has_tag_name("CMvTrack_JavaSoundTrack_Source");
    let guide = definition.has_tag_name("CMvTrack_GuideImage_Source");
    if sound { sound_track_storage(graph, definition, shared, budget)?; }
    if guide { guide_track_storage(graph, definition, shared, budget)?; }
    let mut definitions = vec![definition];
    let guid_field = raw_inherited_field(definition, "guid")?;
    let guid = graph.resolve(guid_field)?;
    if (sound || guide) && (guid == guid_field || !guid.has_tag_name("CTrackGuid")) {
        return Err("Sound and Guide tracks require a shared track GUID".into());
    }
    if guid != guid_field { definitions.push(guid); }
    let parent_member = track_parent_member(graph, definition, scene, guid, budget)?;
    let mut effects = Vec::new();
    for field in ["soundEffect"] {
        if sound && field == "soundEffect" { continue; }
        if graph.field(definition, field)?.is_some_and(|effect| !effect.has_tag_name("null")) {
            return Err("animation track contains an unsupported effect type".into());
        }
    }
    for (field, kind) in [
        ("keyParamEffect", "CMvEffect_Live2DParameter"), ("partsVisibleEffect", "CMvEffect_Live2DPartsVisible"),
        ("visualEffect", "CMvEffect_VisualDefault"), ("lipSyncEffect", "CMvEffect_LipSync"),
        ("eyeBlinkEffect", "CMvEffect_EyeBlink"), ("paramCtrlGroupEffect", "CMvEffect_ParamCtrlGroups"),
        ("soundEffect", "CMvEffect_SoundDefault"),
        ("guideImageEffect", "CMvEffect_GuideImageDefault"),
        ("formEditEffect", "CMvEffect_FormEdit"),
    ] {
        let Some(effect) = graph.field(definition, field)? else { continue };
        if effect.has_tag_name("null") { continue; }
        if field == "guideImageEffect" && !guide {
            return Err("Guide effects require a Guide track".into());
        }
        if !effect.has_tag_name(kind) && !(field == "keyParamEffect" && effect.has_tag_name("CMvKeyParamEffect")) {
            return Err("animation track contains an unsupported effect type".into());
        }
        if effects.contains(&effect) { return Err("animation track effect storage is shared".into()); }
        let reference = raw_inherited_field(definition, field)?;
        if effect != reference {
            direct_reference(graph, reference, effect)?;
            track_owned_definition(graph, effect, definition, shared, budget)?;
            definitions.push(effect);
        } else {
            reject_nested_shared_storage(effect, budget)?;
            direct_reference(graph, raw_inherited_field(effect, "track")?, definition)?;
        }
        effects.push(effect);
    }
    if definition.has_tag_name("CMvTrack_Live2DModel_Source") && graph.field(definition, "keyParamEffect")?.is_none() {
        return Err("animation track lacks keyParamEffect".into());
    }
    if let Some(manager) = graph.field(definition, "effectManager")? {
        if manager != raw_inherited_field(definition, "effectManager")? || !manager.has_tag_name("CMvEffectManager") {
            return Err("animation effect manager is shared or aliased".into());
        }
        let list = raw_inherited_field(manager, "effectList")?;
        if list.attribute("xs.ref").is_some() || list.attribute("xs.id").is_some() {
            return Err("animation effect registry is shared or aliased".into());
        }
        values::list(graph, list, budget)?;
        let mut registered = HashSet::new();
        for member in list.children().filter(Node::is_element) {
            let effect = graph.resolve(member)?;
            direct_reference(graph, member, effect)?;
            if member.attributes().count() != 1 || !effects.contains(&effect) || !registered.insert(effect.id()) {
                return Err("animation effect registry contains an unknown or duplicate member".into());
            }
        }
    }
    let mut attributes = HashSet::new();
    // Parameter-control markers and points are shared GUID objects the track, its effect, its attributes and the
    // scene's calculation order name; they go with the track.
    let mut control_guids: Vec<Node> = Vec::new();
    // Group heads and drawable identities may be shared with another track's controls; one goes only with the
    // last track naming it.
    let mut optional_guids: Vec<Node> = Vec::new();
    let mut borrowed_attributes = Vec::new();
    for effect in &effects {
        let mut owned = HashMap::new();
        let mut shared_keys = HashSet::new();
        if graph.field(*effect, "attrList")?.is_some() {
            let list = raw_inherited_field(*effect, "attrList")?;
            if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
                return Err("animation attribute list is shared or aliased".into());
            }
            values::list(graph, list, budget)?;
            for member in list.children().filter(Node::is_element) {
                let attribute = graph.resolve(member)?;
                direct_reference(graph, member, attribute)?;
                if member.attributes().count() != 1 || owned.insert(attribute.id(), attribute).is_some() {
                    return Err("animation attribute membership is aliased or duplicated".into());
                }
            }
        }
        if graph.field(*effect, "attrMap")?.is_some() {
            let map = raw_inherited_field(*effect, "attrMap")?;
            if map.attribute("xs.id").is_some() || map.attribute("xs.ref").is_some() {
                return Err("animation attribute map is shared or aliased".into());
            }
            values::list(graph, map, budget)?;
            let (mut keys, mut targets) = (HashSet::new(), HashSet::new());
            for entry in map.children().filter(Node::is_element) {
                if entry.attribute("xs.id").is_some() || entry.attribute("xs.ref").is_some() {
                    return Err("animation attribute map entry is shared or aliased".into());
                }
                let field = raw_field(entry, "key")?;
                // Cubism may name a key with a shared CAttrId that other maps use as well; it stays.
                let key = if field.attribute("xs.ref").is_some() {
                    let key = graph.resolve(field)?;
                    direct_reference(graph, field, key)?;
                    if key.parent() != Some(shared) {
                        return Err("animation attribute map key is aliased or duplicated".into());
                    }
                    shared_keys.insert(field.id());
                    key
                } else { field };
                let id = key.attribute("idstr").ok_or("animation attribute map key lacks identity")?;
                if !key.has_tag_name("CAttrId") || field.attribute("xs.id").is_some() || !keys.insert(id) {
                    return Err("animation attribute map key is aliased or duplicated".into());
                }
                let value = raw_field(entry, "value")?;
                let attribute = graph.resolve(value)?;
                direct_reference(graph, value, attribute)?;
                if !targets.insert(attribute.id()) { return Err("animation attribute map shares storage".into()); }
                owned.insert(attribute.id(), attribute);
            }
        }
        if guide && effect.has_tag_name("CMvEffect_GuideImageDefault") {
            let named = raw_inherited_field(*effect, "attrImageNo")?;
            let attribute = graph.resolve(named)?;
            direct_reference(graph, named, attribute)?;
            owned.insert(attribute.id(), attribute);
        }
        for attribute in owned.values() {
            if unowned_parameter_declaration(*effect, *attribute, shared) { continue; }
            if !attributes.insert(attribute.id()) { return Err("animation attribute has multiple effect owners".into()); }
            parent_attribute_storage(graph, *attribute, definition, shared, budget)?;
            definitions.push(*attribute);
        }
        let mut group_guids = HashSet::new();
        if effect.has_tag_name("CMvEffect_Live2DParameter") && graph.field(*effect, "parameterGroupList")?.is_some() {
            let list = raw_inherited_field(*effect, "parameterGroupList")?;
            if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
                return Err("animation parameter groups use shared storage".into());
            }
            values::list(graph, list, budget)?;
            for group in list.children().filter(Node::is_element) {
                if !group.has_tag_name("CMvParameter_Group") || group.attribute("xs.id").is_some()
                    || group.attribute("xs.ref").is_some()
                { return Err("animation parameter group has unsupported storage".into()); }
                let field = raw_inherited_field(group, "guid")?;
                let guid = graph.resolve(field)?;
                if !guid.has_tag_name("CParameterGroupGuid") || guid.attribute("uuid").is_none() || guid.has_children() {
                    return Err("animation parameter group GUID has unsupported storage".into());
                }
                if field.attribute("xs.ref").is_some() {
                    direct_reference(graph, field, guid)?;
                    if guid.parent() != Some(shared) {
                        return Err("animation parameter group GUID has unsupported storage".into());
                    }
                    group_guids.insert(field.id());
                } else if field.attribute("xs.id").is_some() {
                    return Err("animation parameter group GUID has unsupported storage".into());
                }
                if let Some(attribute) = graph.field(group, "attrFrameStep")? {
                    let reference = raw_field(group, "attrFrameStep")?;
                    if !form_plain_null(reference) {
                        direct_reference(graph, reference, attribute)?;
                        if graph.field(attribute, "track")? != Some(definition) {
                            form_borrowed_attribute(graph, attribute, definition, shared, budget)?;
                            group_guids.insert(reference.id());
                            borrowed_attributes.push(attribute);
                        }
                    }
                }
            }
        }
        if effect.has_tag_name("CMvEffect_FormEdit") {
            let parts = raw_field(*effect, "partList")?;
            for part in form_inline_list(graph, parts, "carray_list", budget)? {
                if !part.has_tag_name("CMvPart_FormEdit") || part.attribute("xs.ref").is_some()
                    || part.attribute("xs.id").is_some() {
                    return Err("FormEdit part list contains unsupported storage".into());
                }
                for child in part.children().filter(Node::is_element) {
                    let kind = match child.attribute("xs.n") {
                        Some("id") => "CPartId", Some("guid") => "CPartGuid",
                        Some("isShyMode" | "isExpanded") => "b",
                        _ => return Err("FormEdit part contains an unknown field".into()),
                    };
                    let value = graph.resolve(child)?;
                    if !value.has_tag_name(kind) { return Err("FormEdit part identity is unsupported".into()); }
                    if child.attribute("xs.ref").is_some() {
                        direct_reference(graph, child, value)?;
                        if kind == "b" || value.parent() != Some(shared) {
                            return Err("FormEdit part metadata is not a shared identity".into());
                        }
                        group_guids.insert(child.id());
                    }
                }
                raw_field(part, "id")?;
                raw_field(part, "guid")?;
            }
            definitions.extend(form_track_storage(graph, definition, *effect, shared, budget)?);
        }
        let backlink = raw_inherited_field(*effect, "track")?;
        // Cubism names an effect's kind with a CEffectId that every effect of that kind shares; it stays.
        let kind = match graph.field(*effect, "id")? {
            Some(id) if id.has_tag_name("CEffectId") && id.parent() == Some(shared) => {
                let field = raw_inherited_field(*effect, "id")?;
                direct_reference(graph, field, id)?;
                Some(field)
            }
            _ => None,
        };
        for reference in effect.descendants().filter(|n| n.is_element() && n.attribute("xs.ref").is_some()) {
            budget.consume(1)?;
            if reference == backlink || Some(reference) == kind || group_guids.contains(&reference.id())
                || shared_keys.contains(&reference.id())
            { continue; }
            let target = graph.resolve(reference)?;
            if target.has_tag_name("ParamCtrlMarkerGuid") && target.parent() == Some(shared) {
                direct_reference(graph, reference, target)?;
                if !optional_guids.contains(&target) { optional_guids.push(target); }
                continue;
            }
            if !owned.contains_key(&target.id()) { return Err("animation effect contains an unclassified reference".into()); }
            direct_reference(graph, reference, target)?;
        }
    }
    let mut items = Vec::new();
    if let Some(set) = graph.field(definition, "paramCtrlGroupSet")? {
        if set != raw_field(definition, "paramCtrlGroupSet")? {
            return Err("animation track control points use shared storage".into());
        }
        for reference in set.descendants().filter(|n| n.is_element() && n.attribute("xs.ref").is_some()) {
            budget.consume(1)?;
            let target = graph.resolve(reference)?;
            direct_reference(graph, reference, target)?;
            match target.tag_name().name() {
                "ParamCtrlMarkerGuid" | "CParamCtrlPointGuid" if target.parent() == Some(shared) => {
                    if !control_guids.contains(&target) { control_guids.push(target); }
                }
                "CDrawableGuid" | "CDrawableId" if target.parent() == Some(shared) => {
                    if !optional_guids.contains(&target) { optional_guids.push(target); }
                }
                // A control's parameter is the shared CParameterId that the parameter's curves name too; it stays.
                "CParameterId" if target.parent() == Some(shared) => {}
                "CTrackGuid" if target == guid => {}
                _ => return Err("animation track control points reference unsupported storage".into()),
            }
        }
        let track_guid_id = guid.attribute("xs.id").ok_or("animation track GUID lacks shared storage")?;
        if let Some(order) = graph.field(scene, "paramCtrlCalculationOrderSet")? {
            if order != raw_field(scene, "paramCtrlCalculationOrderSet")? {
                return Err("animation calculation order uses shared storage".into());
            }
            let list = raw_field(order, "calcGroupInfoList")?;
            if list.attribute("xs.id").is_some() || list.attribute("xs.ref").is_some() {
                return Err("animation calculation order uses shared storage".into());
            }
            values::list(graph, list, budget)?;
            for entry in list.children().filter(Node::is_element) {
                if entry.descendants().any(|n| n.attribute("xs.id").is_some()) {
                    return Err("animation calculation order uses shared storage".into());
                }
                let track_field = raw_field(entry, "trackGuid")?;
                if track_field.attribute("xs.ref") != Some(track_guid_id) { continue; }
                direct_reference(graph, track_field, guid)?;
                items.push((list, entry));
            }
        }
    }
    for object in optional_guids {
        let id = object.attribute("xs.id").unwrap_or_default();
        let exclusive = owner.document().descendants().filter(|n| n.attribute("xs.ref") == Some(id)).all(|n| {
            n.ancestors().any(|a| definitions.contains(&a) || control_guids.contains(&a) || items.iter().any(|(_, item)| a == *item))
        });
        if exclusive && !control_guids.contains(&object) { control_guids.push(object); }
    }
    for object in control_guids {
        if object.has_children() || object.attribute("uuid").is_none() && !object.has_tag_name("CDrawableId") {
            return Err("animation control GUID storage is unsupported".into());
        }
        definitions.push(object);
    }
    if sound && definitions.len() != 4 { return Err("Sound deletion requires exactly four owned definitions".into()); }
    if guide && definitions.len() != 15 { return Err("Guide deletion requires exactly fifteen owned definitions".into()); }
    let roots: HashSet<_> = definitions.iter().map(Node::id).collect();
    let ids: HashSet<_> = definitions.iter().flat_map(|node| node.descendants()).filter_map(|n| n.attribute("xs.id")).collect();
    for attribute in borrowed_attributes {
        if ids.contains(attribute.attribute("xs.id").unwrap_or("")) || attribute.descendants().any(|node|
            node.attribute("xs.ref").is_some_and(|id| ids.contains(id))) {
            return Err("borrowed frame-step attribute references deleted storage".into());
        }
    }
    for reference in owner.document().descendants().filter(Node::is_element) {
        budget.consume(1)?;
        if ids.contains(reference.attribute("xs.ref").unwrap_or("")) && reference != owner
            && parent_member.is_none_or(|(_, member)| reference != member)
            && !reference.ancestors().any(|node| roots.contains(&node.id()))
            && !items.iter().any(|(_, item)| reference.ancestors().any(|node| node == *item))
        {
            return Err("animation track storage is referenced outside its owner".into());
        }
    }
    Ok(TrackDeletion { definitions, parent_member, items })
}

/// Checks a scene to delete; returns the shared CSceneGuid its `guid` names, if any, which goes with it.
fn scene_storage<'d, 'i>(
    graph: &Graph<'d, 'i>,
    definition: Node<'d, 'i>,
    animation: Node<'d, 'i>,
    shared: Node<'d, 'i>,
) -> Result<Option<Node<'d, 'i>>, String> {
    if !definition.has_tag_name("CSceneSource") {
        return Err("animation scene definition is not CSceneSource".into());
    }
    if definition.attribute("xs.id").is_none() || definition.attribute("xs.ref").is_some() {
        return Err("animation scene definition lacks direct shared storage".into());
    }
    if definition.parent() != Some(shared) {
        return Err("animation scene definition is not a direct shared entry".into());
    }
    let guid_field = raw_field(definition, "guid")?;
    let guid = if guid_field.attribute("xs.ref").is_some() {
        let guid = graph.resolve(guid_field)?;
        direct_reference(graph, guid_field, guid)?;
        if !guid.has_tag_name("CSceneGuid") || guid.parent() != Some(shared) || guid.has_children()
            || guid.attribute("uuid").is_none()
        {
            return Err("animation scene GUID storage is unsupported".into());
        }
        Some(guid)
    } else { None };
    for name in ["guid", "sceneName", "movieInfo", "trackSourceSet"] {
        let field = raw_field(definition, name)?;
        if (name != "guid" && field.attribute("xs.ref").is_some()) || field.attribute("xs.id").is_some() {
            return Err("animation scene field uses shared or aliased storage".into());
        }
    }
    for name in ["fps", "duration"] {
        let field = raw_field(raw_field(definition, "movieInfo")?, name)?;
        if field.attribute("xs.ref").is_some() || field.attribute("xs.id").is_some() {
            return Err("animation scene field uses shared or aliased storage".into());
        }
    }
    let animation_ref = raw_field(definition, "_animation")?;
    if animation_ref.attribute("xs.ref") != animation.attribute("xs.id")
        || animation_ref.attribute("xs.id").is_some()
        || animation_ref.has_children()
        || animation_ref.attributes().count() != 2
        || graph.resolve(animation_ref)?.id() != animation.id()
    {
        return Err("animation scene backlink differs from its owner".into());
    }
    if definition
        .descendants()
        .filter(|node| node.is_element())
        .any(|node| {
            node.id() != definition.id()
                && (node.attribute("xs.id").is_some() || node.attribute("xs.n") == Some("super"))
        })
    {
        return Err("animation scene storage contains nested shared or inherited data".into());
    }
    Ok(guid)
}

fn scene_set_patches(
    text: &str,
    graph: &Graph<'_, '_>,
    animation: Node<'_, '_>,
    initial_scenes: &[Value],
    candidate_scenes: &[Value],
    plan: &SceneSetPlan,
    additions: &mut NativeAdditions,
    patches: &mut Vec<(Range<usize>, String)>,
    budget: &mut Budget,
) -> Result<(), String> {
    if plan.removed.is_empty()
        && plan
            .ordered
            .iter()
            .all(|slot| matches!(slot, SceneSlot::Kept { .. }))
    {
        return Ok(());
    }
    let list_node = inline_field(animation, "_scenes")?;
    let shared = animation
        .document()
        .root_element()
        .children()
        .find(|node| node.is_element() && node.has_tag_name("shared"))
        .ok_or("animation XML lacks a shared section")?;
    let raw: Vec<Node> = list_node.children().filter(Node::is_element).collect();
    let added_values: Vec<&Value> = plan.ordered.iter().filter_map(|slot| match slot {
        SceneSlot::Added { candidate } => Some(&candidate_scenes[*candidate]),
        _ => None,
    }).collect();
    let added_refs = add_scene_refs(text, graph, animation, &added_values, budget, additions)?;
    // The animation's currentScene and the resource manager's _sceneSource each name one scene; a deleted one
    // is replaced by the next kept scene, else the previous, else the first added. Cubism keeps _sceneSource on
    // the last scene (every supplied file does), so an added scene takes it.
    let mut retargeted = Vec::new();
    let mut pointers = vec![(animation, "currentScene")];
    if let Some(manager) = graph.field(animation, "resourceManager")? {
        pointers.push((manager, "_sceneSource"));
    }
    for (holder, field) in pointers {
        let Some(current) = graph.field(holder, field)? else { continue };
        if current.has_tag_name("null") { continue; }
        let member = raw_field(holder, field)?;
        direct_reference(graph, member, current)?;
        if field == "_sceneSource" {
            if let Some(last) = added_refs.last() {
                let attribute = member.attributes().find(|a| a.name() == "xs.ref")
                    .ok_or_else(|| format!("animation {field} is not a direct reference"))?;
                patches.push((attribute.range_value(), escape(&last.storage_id)));
                retargeted.push(member);
                continue;
            }
        }
        let scenes = list(graph, animation, "_scenes", budget)?;
        let index = scenes.iter().position(|entry| *entry == current)
            .ok_or_else(|| format!("animation {field} is not a scene member"))?;
        if plan.removed.contains(&index) {
            let replacement = (index + 1..raw.len()).chain((0..index).rev())
                .find(|index| !plan.removed.contains(index));
            let id = if let Some(index) = replacement {
                let target = graph.resolve(raw[index])?;
                direct_reference(graph, raw[index], target)?;
                target.attribute("xs.id").ok_or_else(|| format!("animation {field} target lacks shared storage"))?
            } else {
                added_refs.first().map(|entry| entry.storage_id.as_str())
                    .ok_or_else(|| format!("animation {field} cannot be left without a scene"))?
            };
            let attribute = member.attributes().find(|a| a.name() == "xs.ref")
                .ok_or_else(|| format!("animation {field} is not a direct reference"))?;
            patches.push((attribute.range_value(), escape(id)));
            retargeted.push(member);
        }
    }
    let mut shrunk_playlists: Vec<(Node, usize)> = Vec::new();
    for &index in &plan.removed {
        let scene = &initial_scenes[index];
        let definition = select_guid(
            graph, list(graph, animation, "_scenes", budget)?, &scene["guid"],
        )?;
        let mut owner = None;
        for &member in &raw {
            if graph.resolve(member)?.id() == definition.id() && owner.replace(member).is_some() {
                return Err("animation scene lacks exactly one owned member".into());
            }
        }
        let owner = owner.ok_or("animation scene lacks exactly one owned member")?;
        if owner.attribute("xs.ref").is_none()
            || owner.attributes().count() != 1
            || owner.has_children()
        {
            return Err("animation scene member is not a bare reference".into());
        }
        if definition.attribute("xs.id") != owner.attribute("xs.ref") {
            return Err("animation scene reference is aliased".into());
        }
        let scene_guid = scene_storage(graph, definition, animation, shared)?;
        let source_list = inline_field(
            inline_field(definition, "trackSourceSet")?,
            "_sources",
        )?;
        values::list(graph, source_list, budget)?;
        let mut closure: Vec<Node> = vec![definition];
        let mut group_lists = Vec::new();
        let mut track_members = HashSet::new();
        let mut tracks = Vec::new();
        for field in TRACK_FIELDS {
            tracks.extend(scene[field].as_array().ok_or("animation tracks missing")?);
        }
        let mut positions = HashSet::new();
        for entry in source_list.children().filter(|node| node.is_element()) {
            let resolved = graph.resolve(entry)?;
            if resolved.has_tag_name("CMvTrack_Group_Source") {
                group_lists.push(group_children(graph, resolved, entry, definition, shared, budget)?);
                closure.push(resolved);
                continue;
            }
            let track_guid = graph.guid(resolved)?;
            let position = tracks.iter()
                .position(|track| track["guid"].as_str() == track_guid)
                .ok_or("animation scene member is not an owned track")?;
            if !positions.insert(position) { return Err("animation scene has duplicate track members".into()); }
            let deletion = track_definition_ownership(
                graph, entry, definition, shared, &tracks[position], budget,
            )?;
            if let Some((_, member)) = deletion.parent_member {
                if !track_members.insert(member.id()) {
                    return Err("animation scene has duplicate group membership".into());
                }
            }
            closure.extend(deletion.definitions);
        }
        if positions.len() != tracks.len() { return Err("animation scene track storage is incomplete".into()); }
        for list in group_lists {
            for member in list.children().filter(Node::is_element) {
                if !track_members.remove(&member.id()) {
                    return Err("animation scene group contains an unowned child".into());
                }
            }
        }
        if !track_members.is_empty() { return Err("animation scene lacks a track parent group".into()); }
        if let Some(root) = graph.field(definition, "rootTrack")? {
            if !root.has_tag_name("null") {
                direct_reference(graph, raw_field(definition, "rootTrack")?, root)?;
                if !closure.contains(&root) { return Err("animation scene rootTrack is not owned".into()); }
            }
        }
        // A playlist item names its scene through the shared CSceneGuid and goes with it.
        let mut items = Vec::new();
        if let Some(guid) = scene_guid {
            closure.push(guid);
            let id = guid.attribute("xs.id").unwrap_or_default();
            for item in animation.descendants().filter(|node| node.has_tag_name("PlaylistItemData")) {
                let field = raw_field(item, "guid")?;
                if field.attribute("xs.ref") != Some(id) { continue; }
                direct_reference(graph, field, guid)?;
                let list = item.parent().filter(|list| list.has_tag_name("carray_list") && list.attribute("xs.n") == Some("list"))
                    .ok_or("animation playlist item is not in an inline list")?;
                if item.descendants().any(|node| node.attribute("xs.id").is_some()) || list.attribute("xs.id").is_some() {
                    return Err("animation playlist item uses shared storage".into());
                }
                match shrunk_playlists.iter_mut().find(|(known, _)| *known == list) {
                    Some((_, count)) => *count += 1,
                    None => shrunk_playlists.push((list, 1)),
                }
                items.push(item);
            }
        }
        let removed_ids: HashSet<&str> = closure.iter().flat_map(|node| node.descendants())
            .filter_map(|node| node.attribute("xs.id")).collect();
        for node in owner.document().root_element().descendants().filter(|node| {
            node.is_element()
                && removed_ids.contains(node.attribute("xs.ref").unwrap_or(""))
        }) {
            let inside = node.id() == owner.id()
                || retargeted.contains(&node)
                || items.iter().any(|item| node.ancestors().any(|ancestor| ancestor == *item))
                || closure.iter().any(|def| {
                    def.id() == node.id()
                        || def.descendants().any(|d| d.id() == node.id())
                });
            if !inside {
                return Err(
                    "animation scene storage is referenced outside its owner".into(),
                );
            }
        }
        patches.push((owner.range(), String::new()));
        for node in closure.iter().chain(&items) {
            patches.push((node.range(), String::new()));
        }
    }
    for (list, removed) in shrunk_playlists {
        patches.push(count_patch(list, list.children().filter(Node::is_element).count() - removed)?);
    }
    let refs = added_refs.iter().map(|entry| entry.reference.as_str()).collect::<String>();
    let count = raw.len() + added_values.len() - plan.removed.len();
    let count_patch = topology_count_patch(list_node, count)?;
    if count != raw.len() {
        patches.push(count_patch);
    }
    if !refs.is_empty() {
        let end = list_end(list_node, text)?;
        emit_inserts(
            patches,
            [None].into_iter(),
            &raw,
            &end,
            [refs].into_iter(),
        );
    }
    Ok(())
}

enum TrackSlot {
    Kept { original: usize, candidate: usize },
    Added { candidate: usize },
}

struct TrackSetPlan {
    ordered: Vec<TrackSlot>,
    removed: Vec<usize>,
}

enum SceneSlot {
    Kept { original: usize, candidate: usize },
    Added { candidate: usize },
}

struct SceneSetPlan {
    ordered: Vec<SceneSlot>,
    removed: Vec<usize>,
}

fn scene_set_plan(initial_scenes: &[Value], candidate_scenes: &[Value]) -> Result<SceneSetPlan, String> {
    let mut scene_index = HashMap::new();
    let mut track_scene = HashMap::new();
    for (index, scene) in initial_scenes.iter().enumerate() {
        let guid = scene["guid"].as_str().ok_or("animation scene GUID missing")?;
        if scene_index.insert(guid, index).is_some() {
            return Err("duplicate animation scene GUID".into());
        }
        for field in TRACK_FIELDS {
            for track in scene[field].as_array().ok_or("animation tracks missing")? {
                let track_guid = track["guid"].as_str().ok_or("animation track GUID missing")?;
                if track_scene.insert(track_guid, (guid, field)).is_some() {
                    return Err("duplicate animation track GUID".into());
                }
            }
        }
    }
    let mut seen_scenes = HashSet::new();
    let mut seen_tracks = HashSet::new();
    let mut used = vec![false; initial_scenes.len()];
    let mut ordered = Vec::with_capacity(candidate_scenes.len());
    let mut last: Option<usize> = None;
    let mut seen_added = false;
    for (candidate, scene) in candidate_scenes.iter().enumerate() {
        let guid = scene["guid"].as_str().ok_or("animation scene GUID missing")?;
        if !seen_scenes.insert(guid) {
            return Err("duplicate animation scene GUID".into());
        }
        for field in TRACK_FIELDS {
            for track in scene[field].as_array().ok_or("animation tracks missing")? {
                let track_guid = track["guid"].as_str().ok_or("animation track GUID missing")?;
                if !seen_tracks.insert(track_guid) {
                    return Err("duplicate animation track GUID".into());
                }
                if let Some(&(origin, family)) = track_scene.get(track_guid) {
                    if origin != guid {
                        return Err("animation track GUID cannot move between scenes".into());
                    }
                    if family != field {
                        return Err("animation track GUID cannot move between families".into());
                    }
                }
            }
        }
        match scene_index.get(guid) {
            Some(&original) => {
                if last.is_some_and(|last| original <= last) || seen_added {
                    return Err("animation scenes cannot be reordered".into());
                }
                used[original] = true;
                last = Some(original);
                ordered.push(SceneSlot::Kept { original, candidate });
            }
            None => {
                seen_added = true;
                ordered.push(SceneSlot::Added { candidate });
            }
        }
    }
    let removed: Vec<usize> = (0..initial_scenes.len()).filter(|index| !used[*index]).collect();
    Ok(SceneSetPlan { ordered, removed })
}

pub(crate) struct NativeAdditions {
    pub(crate) reserved: HashSet<String>,
    pub(crate) definitions: String,
    track_guid_tag: String,
    /// The last `xs.idx` serial in use, or `None` in a file that stores none.
    pub(crate) next_idx: Option<u64>,
}

struct SerializedTrack {
    reference: String,
    definitions: String,
}

fn track_set_plan(old_tracks: &[Value], new_tracks: &[Value]) -> Result<TrackSetPlan, String> {
    let mut old_index = HashMap::new();
    for (index, track) in old_tracks.iter().enumerate() {
        let guid = track["guid"].as_str().ok_or("animation track GUID missing")?;
        if old_index.insert(guid, index).is_some() {
            return Err("duplicate animation track GUID".into());
        }
    }
    let mut candidate_index = HashSet::new();
    for track in new_tracks {
        let guid = track["guid"].as_str().ok_or("animation track GUID missing")?;
        if !candidate_index.insert(guid) {
            return Err("duplicate animation track GUID".into());
        }
    }
    let mut used = vec![false; old_tracks.len()];
    let mut ordered = Vec::with_capacity(new_tracks.len());
    let mut last: Option<usize> = None;
    let mut seen_added = false;
    for (candidate, track) in new_tracks.iter().enumerate() {
        match old_index.get(track["guid"].as_str().unwrap()) {
            Some(&original) => {
                if last.is_some_and(|last| original <= last) || seen_added {
                    return Err("animation tracks cannot be reordered".into());
                }
                used[original] = true;
                last = Some(original);
                ordered.push(TrackSlot::Kept { original, candidate });
            }
            None => {
                seen_added = true;
                ordered.push(TrackSlot::Added { candidate });
            }
        }
    }
    let removed: Vec<usize> = (0..old_tracks.len()).filter(|index| !used[*index]).collect();
    Ok(TrackSetPlan { ordered, removed })
}

fn check_new_track(track: &Value) -> Result<(), String> {
    let invalid = || "animation track record is invalid".to_owned();
    const TRACK_KEYS: [&str; 11] = [
        "guid", "name", "resourceGuid", "start", "internalOffset", "duration",
        "parameterCurves", "partOpacityCurves", "legacyPartOpacityCurves",
        "underscorePartOpacityCurves",
        "skippedLegacyPartOpacityCount",
    ];
    let object = track.as_object().ok_or_else(invalid)?;
    if object.len() != TRACK_KEYS.len() || TRACK_KEYS.iter().any(|key| !object.contains_key(*key)) {
        return Err("animation track record is invalid".into());
    }
    for key in ["guid", "resourceGuid"] {
        let value = track[key].as_str().ok_or_else(invalid)?;
        if value.is_empty() || value.trim() != value {
            return Err("animation track identity is invalid".into());
        }
    }
    scene_setting("name", &track["name"])?;
    if track["start"] != json!(0) || track["internalOffset"] != json!(0) {
        return Err("animation track start and offset must be zero".into());
    }
    track["duration"]
        .as_i64()
        .filter(|value| *value >= 0 && i32::try_from(*value).is_ok())
        .ok_or_else(invalid)?;
    if track["legacyPartOpacityCurves"] != json!([])
        || track["underscorePartOpacityCurves"] != json!([])
        || track["skippedLegacyPartOpacityCount"] != json!(0)
    {
        return Err("animation track carries unsupported part opacity data".into());
    }
    for curve in track["parameterCurves"]
        .as_array()
        .ok_or_else(invalid)?
    {
        check_new_curve(curve)?;
    }
    let mut ids = HashSet::new();
    let mut guids = HashSet::new();
    for curve in track["partOpacityCurves"].as_array().ok_or_else(invalid)? {
        check_new_part_curve(curve)?;
        if !ids.insert(curve["id"].as_str().unwrap()) || !guids.insert(curve["partGuid"].as_str().unwrap()) {
            return Err("duplicate animation Part curve identity".into());
        }
    }
    Ok(())
}

fn serialize_track(
    additions: &mut NativeAdditions,
    scene_ref: &str,
    track: &Value,
) -> Result<SerializedTrack, String> {
    check_new_track(track)?;
    let guid = track["guid"].as_str().unwrap();
    let storage = free_storage_id(&mut additions.reserved, &format!("soul2d-track-{guid}"));
    let curves = track["parameterCurves"].as_array().unwrap();
    let mut references = String::new();
    let mut definitions = String::new();
    for curve in curves {
        let curve_storage = free_storage_id(
            &mut additions.reserved,
            &format!("soul2d-curve-{}-{}", guid, curve["id"].as_str().unwrap()),
        );
        references.push_str(&format!(
            r#"<CMvAttrF xs.ref="{}"/>"#, escape(&curve_storage),
        ));
        definitions.push_str(&serialize_curve(&curve_storage, &storage, curve)?);
    }
    let parts = track["partOpacityCurves"].as_array().unwrap();
    let parts_effect = if parts.is_empty() { String::new() }
        else { serialize_parts_effect(additions, &storage, parts)? };
    let storage = escape(&storage);
    let name = escape(track["name"].as_str().unwrap());
    let resource = escape(track["resourceGuid"].as_str().unwrap());
    let guid_out = escape(guid);
    let scene_ref = escape(scene_ref);
    let (start, offset, duration) = (
        track["start"].as_i64().unwrap(),
        track["internalOffset"].as_i64().unwrap(),
        track["duration"].as_i64().unwrap(),
    );
    let tag = &additions.track_guid_tag;
    definitions.insert_str(
        0,
        &format!(
            r#"<CMvTrack_Live2DModel_Source xs.id="{storage}"><{tag} xs.n="guid" uuid="{guid_out}"/><s xs.n="name">{name}</s><CSceneSource xs.n="_sceneSource" xs.ref="{scene_ref}"/><CModelGuid xs.n="_resourceGuid" uuid="{resource}"/><CMvKeyParamEffect xs.n="keyParamEffect"><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="{storage}"/><carray_list xs.n="attrList" count="{count}">{references}</carray_list></CMvKeyParamEffect>{parts_effect}<i xs.n="start">{start}</i><i xs.n="internalOffset">{offset}</i><i xs.n="duration">{duration}</i></CMvTrack_Live2DModel_Source>"#,
            count = curves.len(),
        ),
    );
    Ok(SerializedTrack {
        reference: format!(r#"<CMvTrack_Live2DModel_Source xs.ref="{storage}"/>"#),
        definitions,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn visual_control_policy_protects_metadata_and_keeps_recreated_keys_editable() {
        use super::*;
        let mut point = new_key_point(0, 10.0);
        point["corner"] = json!(false);
        point["previous"]["integerFrame"] = json!(-2);
        point["previous"]["positionOptimized"] = json!(true);
        let old = json!({"id":"opacity","points":[point],"curveTypes":["LINEAR"]});
        let check = |new: &Value| {
            visual_key_changes(&old, new, "Guide")?;
            visual_key_changes(&old, new, "Image")
        };
        let mut retained = old.clone();
        retained["points"][0]["anchor"]["value"] = json!(11.0);
        retained["points"][0]["previous"]["frame"] = json!(-3.5);
        retained["points"][0]["next"]["value"] = json!(12.0);
        retained["curveTypes"][0] = json!("BEZIER");
        check(&retained).unwrap();
        let mut recreated = retained.clone();
        recreated["points"][0] = new_key_point(0, 11.0);
        recreated["points"][0]["previous"]["frame"] = json!(-3.5);
        recreated["points"][0]["next"]["value"] = json!(12.0);
        check(&recreated).unwrap();
        let mut inserted = old.clone();
        let mut added = new_key_point(10, 20.0);
        added["previous"]["frame"] = json!(8.5);
        added["next"]["value"] = json!(22.0);
        inserted["points"].as_array_mut().unwrap().push(added);
        inserted["curveTypes"].as_array_mut().unwrap().push(json!("STEP"));
        check(&inserted).unwrap();
        for (base, index) in [(&retained, 0), (&recreated, 0), (&inserted, 1)] {
            for (field, value) in [("integerFrame", json!(99)), ("positionOptimized", json!("invalid")),
                ("extra", json!(true))] {
                let mut bad = base.clone();
                bad["points"][index]["previous"][field] = value;
                assert!(check(&bad).is_err(), "{field} {bad}");
            }
            let mut bad = base.clone();
            bad["points"][index]["corner"] = json!("invalid");
            assert!(check(&bad).is_err());
            bad = base.clone();
            bad["points"][index]["previous"] = Value::Null;
            assert!(check(&bad).is_err());
            bad = base.clone();
            bad["curveTypes"][index] = json!("unsupported");
            assert!(check(&bad).is_err());
        }
        let mut mixed = retained.clone();
        mixed["points"][0]["corner"] = json!(true);
        assert!(check(&mixed).is_err());
        let mut future = old.clone();
        future["curveTypes"][0] = json!("FUTURE");
        let mut future_edit = future.clone();
        future_edit["points"][0]["next"]["value"] = json!(12.0);
        for family in ["Guide", "Image"] { visual_key_changes(&future, &future_edit, family).unwrap(); }
        let mut bad_count = retained.clone();
        bad_count["curveTypes"] = json!([]);
        assert_eq!(visual_key_changes(&old, &bad_count, "Image").unwrap_err(),
            "Image scalar point and curve-type counts differ");
    }

    #[test]
    fn point_selection_counts_every_family_and_preserves_f32_noops() {
        use super::*;
        let track = json!({"guid":"target","pointChannels":[{"id":"xy","points":[
            {"frame":-3,"x":0.10000000149011612,"y":-0.0,"corner":false}]}]});
        let request = json!({"sceneGuid":"scene","trackGuid":"target","curveId":"xy","anchorFrame":-3,"x":0.1,"y":0.0});
        for family in ["imageTracks", "guideTracks"] {
            let mut model = json!({"format":"live2d.animation.source.v1","scenes":[{"guid":"scene"}]});
            model["scenes"][0][family] = json!([track]);
            assert_eq!(set_point(&model, &request).unwrap(), model);
            for other in TRACK_FIELDS {
                let mut ambiguous = model.clone();
                if other == family { ambiguous["scenes"][0][other].as_array_mut().unwrap().push(track.clone()); }
                else { ambiguous["scenes"][0][other] = json!([track]); }
                assert!(set_point(&ambiguous, &request).is_err(), "{family}/{other}");
            }
            let mut changed = request.clone();
            changed["x"] = json!(1.23456789123);
            let result = set_point(&model, &changed).unwrap();
            let rounded: f64 = (1.23456789123_f64 as f32).to_string().parse().unwrap();
            assert_eq!(result["scenes"][0][family][0]["pointChannels"][0]["points"][0]["x"], rounded);
        }
    }

    #[test]
    fn image_point_channel_validation_still_precedes_native_resolution() {
        use super::*;
        let xml = include_str!("../tests/fixtures/can3-image-track.xml");
        let document = xml_document(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let wrong_track = document.root_element();
        let old = json!({"pointChannels":[{"id":"xy","points":[{"frame":-3,"x":10.0,"y":20.0,"corner":false}]}]});
        let mut new = old.clone();
        new["pointChannels"][0]["extra"] = json!(true);
        let error = |new: &Value| image_point_changes(xml, &graph, wrong_track, &old, new,
            &mut Vec::new(), &mut Budget::new(1000)).unwrap_err();
        assert_eq!(error(&new), "Image point channel metadata changes are unsupported");
        new = old.clone();
        new["pointChannels"][0]["points"].as_array_mut().unwrap().push(json!({"frame":9}));
        assert_eq!(error(&new), image_visual_attribute(&graph, wrong_track, "xy", &mut Budget::new(1000)).unwrap_err());
        new = old.clone();
        new["pointChannels"][0]["points"][0]["corner"] = json!(true);
        let native_error = image_visual_attribute(&graph, wrong_track, "xy", &mut Budget::new(1000)).unwrap_err();
        assert_eq!(error(&new), native_error);
    }

    #[test]
    fn visual_point_deletion_reconciliation_requires_a_protected_subsequence() {
        use super::*;
        let points = [9, -3, 2].map(|frame| json!({"frame":frame,"x":10.0,"y":20.0,"corner":false}));
        let point_xml = "<CPtTNS><GVector2 xs.n=\"super\"><f xs.n=\"x\">10</f><f xs.n=\"y\">20</f></GVector2><GVector2 xs.n=\"value\"><f xs.n=\"x\">10</f><f xs.n=\"y\">20</f></GVector2></CPtTNS>";
        let xml = format!("<root><CMvAttrPt xs.id=\"attr\"><CXY_TNSSequence xs.n=\"valueDataXY\"><array xs.n=\"points\" count=\"3\" type=\"CPtTNS\">{}</array><GVector2 xs.n=\"basePt\"><f xs.n=\"x\">0</f><f xs.n=\"y\">0</f></GVector2><CMvAttrPt xs.n=\"attr\" xs.ref=\"attr\"/></CXY_TNSSequence></CMvAttrPt></root>", point_xml.repeat(3));
        let document = xml_document(&xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let attribute = document.descendants().find(|n| n.attribute("xs.id") == Some("attr")).unwrap();
        let apply = |after: &[Value]| point_channel_patches(&xml, &graph, attribute, &points, after,
            "Image", &mut Vec::new(), &mut Budget::new(1000));
        assert!(apply(&[points[0].clone(), points[2].clone()]).is_ok());
        assert!(apply(&[]).is_ok());
        for after in [vec![points[2].clone(), points[0].clone()], vec![points[0].clone(), points[0].clone()],
            vec![json!({"frame":99,"x":10.0,"y":20.0,"corner":false})],
            vec![json!({"frame":9,"x":10.0,"y":20.0,"corner":true})],
            vec![points[1].clone(), points[0].clone(), points[2].clone()]] {
            assert!(apply(&after).is_err(), "{after:?}");
        }
    }

    #[test]
    fn image_point_projection_keeps_empty_channels_and_propagates_budgets() {
        use super::*;
        let xml = include_str!("../tests/fixtures/can3-image-track.xml");
        let document = xml_document(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|node| node.attribute("xs.id") == Some("image-track")).unwrap();
        assert_eq!(visual_point_channels(&graph, track, &mut Budget::new(1000)).unwrap(),
            vec![json!({"id":"xy","points":[]})]);
        for mut budget in [Budget::new(0), Budget::new(1000).with_text_limit(0)] {
            assert!(visual_point_channels(&graph, track, &mut budget).is_err());
            assert!(budget.exhausted());
        }
        let unsupported = xml.replace("<CXY_TNSSequence xs.n=\"valueDataXY\"><CMvAttrPt xs.n=\"attr\" xs.ref=\"image-xy\"/>",
            "<CXY_TNSSequence xs.n=\"valueDataXY\"><null xs.n=\"attr\"/>");
        let document = xml_document(&unsupported).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|node| node.attribute("xs.id") == Some("image-track")).unwrap();
        assert!(visual_point_channels(&graph, track, &mut Budget::new(1000)).unwrap().is_empty());
    }

    #[test]
    fn image_visual_projection_keeps_empty_curves_and_propagates_budgets() {
        use super::*;
        let xml = include_str!("../tests/fixtures/can3-image-track.xml");
        let document = xml_document(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|n| n.attribute("xs.id") == Some("image-track")).unwrap();
        let curves = visual_scalar_curves(&graph, track, &mut Budget::new(1000)).unwrap();
        assert_eq!(curves, vec![json!({"id":"opacity","points":[],"curveTypes":[]})]);
        for mut budget in [Budget::new(0), Budget::new(1000).with_text_limit(0)] {
            assert!(visual_scalar_curves(&graph, track, &mut budget).is_err());
            assert!(budget.exhausted());
        }
        let unsupported = xml.replace("<CMutableSequence xs.n=\"valueData\"><CMvAttrF xs.n=\"attr\" xs.ref=\"image-opacity\"/>",
            "<CMutableSequence xs.n=\"valueData\"><null xs.n=\"attr\"/>");
        let document = xml_document(&unsupported).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|n| n.attribute("xs.id") == Some("image-track")).unwrap();
        assert!(visual_scalar_curves(&graph, track, &mut Budget::new(1000)).unwrap().is_empty());
        let original = xml_document(xml).unwrap();
        let opacity = original.descendants().find(|n| n.attribute("xs.id") == Some("image-opacity")).unwrap();
        let scale = xml[opacity.range()].replace("image-opacity", "image-scale").replace("opacity", "scalex");
        let both = xml.replace("</shared>", &format!("{scale}</shared>"))
            .replace("<array xs.n=\"attrList\" count=\"3\">", "<array xs.n=\"attrList\" count=\"4\"><CMvAttrF xs.ref=\"image-scale\"/>");
        let document = xml_document(&both).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|n| n.attribute("xs.id") == Some("image-track")).unwrap();
        let curves = visual_scalar_curves(&graph, track, &mut Budget::new(1000)).unwrap();
        assert_eq!(curves.iter().map(|c| c["id"].as_str().unwrap()).collect::<Vec<_>>(), ["scalex", "opacity"]);
        let mut budget = Budget::new(1000).with_text_limit(6);
        assert!(visual_scalar_curves(&graph, track, &mut budget).is_err());
        assert!(budget.exhausted());
        let partial = both.replace("<CMutableSequence xs.n=\"valueData\"><CMvAttrF xs.n=\"attr\" xs.ref=\"image-opacity\"/>",
            "<CMutableSequence xs.n=\"valueData\"><null xs.n=\"attr\"/>");
        let document = xml_document(&partial).unwrap();
        let graph = Graph::new(&document).unwrap();
        let track = document.descendants().find(|n| n.attribute("xs.id") == Some("image-track")).unwrap();
        assert_eq!(visual_scalar_curves(&graph, track, &mut Budget::new(1000)).unwrap(), vec![curves[0].clone()]);
    }

    #[test]
    fn scalar_selection_keeps_operation_family_limits_and_rejects_ambiguity() {
        use super::*;
        let curve = json!({"id":"opacity","points":[],"curveTypes":[]});
        let request = json!({"trackGuid":"selected","curveId":"opacity"});
        for family in TRACK_FIELDS {
            let mut scene = json!({"tracks":[],"imageTracks":[],"soundTracks":[],"guideTracks":[]});
            scene[family] = json!([{"guid":"selected","visualCurves":[curve.clone()],"parameterCurves":[curve.clone()]}]);
            assert_eq!(track_key_curve(&mut scene.clone(), &request).is_ok(), family != "soundTracks", "{family}");
            for other in TRACK_FIELDS {
                let mut ambiguous = scene.clone();
                ambiguous[other].as_array_mut().unwrap().push(scene[family][0].clone());
                assert!(track_key_curve(&mut ambiguous.clone(), &request).is_err());
            }
            if matches!(family, "imageTracks" | "guideTracks") {
                let mut selected = request.clone();
                selected["partOpacityMapKey"] = Value::Null;
                assert!(track_key_curve(&mut scene.clone(), &selected).is_err());
                selected = request.clone();
                selected["curveId"] = json!("missing");
                assert!(track_key_curve(&mut scene.clone(), &selected).is_err());
                scene[family][0]["visualCurves"].as_array_mut().unwrap().push(curve.clone());
                assert!(track_key_curve(&mut scene.clone(), &request).is_err());
            }
        }
    }

    #[test]
    fn image_track_copy_preserves_native_data_and_is_repeatable() {
        use super::*;
        let xml = include_str!("../tests/fixtures/can3-image-track.xml")
            .replace(r#"<s xs.n="name">Background</s>"#, r#"<s xs.n="name" xs.idx="1" future="opaque">Background</s>"#);
        let xml = xml.as_str();
        let container = crate::authoring_edit::tests::test_container(xml);
        let initial = read(&container).unwrap();
        let request = json!({"sceneGuid":"scene-guid","trackGuid":"image-track-guid","name":"Copy & image"});
        let (model, copy) = duplicate_track(&initial, &request, &[], "image-copy").unwrap();
        let bytes = rewrite_with_copies(&container, &initial, &model, &[copy.clone()]).unwrap();
        assert_eq!(rewrite_with_copies(&container, &initial, &model, &[copy.clone()]).unwrap(), bytes);
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), model);
        let copied_xml = String::from_utf8(CaffDocument::parse(&bytes).unwrap().main_xml()).unwrap();
        assert!(copied_xml.contains(r#"<s xs.n="name" xs.idx="2" future="opaque">Copy &amp; image</s>"#));
        let original = xml_document(xml).unwrap();
        for id in ["image-track", "image-guid", "image-visual", "image-xy", "image-opacity", "image-frame"] {
            let node = original.descendants().find(|node| node.attribute("xs.id") == Some(id)).unwrap();
            assert!(copied_xml.contains(&xml[node.range()]), "{id}");
        }
        let (mut twice, second) = duplicate_track(&model,
            &json!({"sceneGuid":"scene-guid","trackGuid":"image-copy","name":"Second"}), &[copy.clone()], "image-copy-2").unwrap();
        twice["scenes"][0]["imageTracks"].as_array_mut().unwrap().remove(0);
        twice["scenes"][0]["imageTracks"].as_array_mut().unwrap().remove(0);
        let bytes = rewrite_with_copies(&container, &initial, &twice, &[copy, second]).unwrap();
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), twice);
    }

    #[test]
    fn linked_track_copy_keeps_family_and_avoids_identity_collisions() {
        let xml = include_str!("../tests/fixtures/can3-image-track.xml")
            .replace("</shared>", r#"<Opaque xs.id="soul2d-linked-copy-0"/></shared>"#);
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let request = json!({"sceneGuid":"scene-guid","trackGuid":"image-track-guid","name":"Copy"});
        assert!(duplicate_track(&initial, &request, &[], "image-track-guid").is_err());
        let (candidate, copy) = duplicate_track(&initial, &request, &[], "copy").unwrap();
        assert!(duplicate_track(&initial, &request, &[copy.clone()], "copy").is_err());
        let bytes = rewrite_with_copies(&container, &initial, &candidate, &[copy.clone()]).unwrap();
        let saved = CaffDocument::parse(&bytes).unwrap();
        let text = String::from_utf8(saved.main_xml()).unwrap();
        let parsed = xml_document(&text).unwrap();
        let ids: Vec<_> = parsed.descendants().filter_map(|node| node.attribute("xs.id")).collect();
        assert_eq!(ids.len(), ids.iter().copied().collect::<HashSet<_>>().len());
        assert!(text.contains(r#"<Opaque xs.id="soul2d-linked-copy-0"/>"#));
        let mut wrong_family = copy.clone();
        wrong_family.family = CopyFamily::Guide;
        assert!(rewrite_with_copies(&container, &initial, &candidate, &[wrong_family]).is_err());
        let mut wrong_family = copy.clone();
        wrong_family.family = CopyFamily::Sound;
        assert!(rewrite_with_copies(&container, &initial, &candidate, &[wrong_family]).is_err());
        let mut moved = candidate.clone();
        moved["scenes"][0]["imageTracks"].as_array_mut().unwrap().pop();
        moved["scenes"][1]["imageTracks"].as_array_mut().unwrap().push(copy.track.clone());
        assert!(rewrite_with_copies(&container, &initial, &moved, &[copy]).is_err());
    }

    #[test]
    fn image_track_copy_refuses_unknown_references_and_keeps_optional_resources() {
        let base = include_str!("../tests/fixtures/can3-image-track.xml");
        let container = crate::authoring_edit::tests::test_container(base);
        let initial = read(&container).unwrap();
        assert_eq!(initial["imageResources"][0]["guid"], "image-resource");
        let mut changed = initial.clone();
        changed["imageResources"][0]["name"] = json!("changed");
        assert!(rewrite(&container, &initial, &changed).is_err());
        let parsed = xml_document(base).unwrap();
        let resource = parsed.descendants().find(|node| node.has_tag_name("CResource_Linked_Image")).unwrap();
        let mut ranges: Vec<_> = resource.descendants().filter(|node| matches!(node.attribute("xs.n"), Some("name" | "srcFile")))
            .map(|node| node.range()).collect();
        ranges.sort_by_key(|range| range.start);
        let mut minimal = base.to_owned();
        for range in ranges.into_iter().rev() { minimal.replace_range(range, ""); }
        let minimal = crate::authoring_edit::tests::test_container(&minimal);
        assert_eq!(read(&minimal).unwrap()["imageResources"], json!([{"guid":"image-resource"}]));
        for xml in [
            base.replace("</shared>", "<Opaque><CMvAttrI xs.ref=\"image-frame\"/></Opaque></shared>"),
            base.replace("<null xs.n=\"soundEffect\"/>", "<Opaque xs.n=\"soundEffect\"/>"),
            base.replace("<d xs.n=\"lastValue\">NaN</d>", "<Opaque xs.ref=\"attr-opaque\"/><d xs.n=\"lastValue\">NaN</d>"),
        ] {
            let container = crate::authoring_edit::tests::test_container(&xml);
            let initial = read(&container).unwrap();
            let (candidate, copy) = duplicate_track(&initial,
                &json!({"sceneGuid":"scene-guid","trackGuid":"image-track-guid","name":"Copy"}), &[], "copy").unwrap();
            assert!(rewrite_with_copies(&container, &initial, &candidate, &[copy]).is_err());
        }
    }
    use super::*;

    #[test]
    fn editable_curve_ids_are_unique_across_supported_arrays() {
        let mut track = json!({"parameterCurves":[{"id":"Parameter"}],
            "partOpacityCurves":[{"id":"Opacity"}],"legacyPartOpacityCurves":[{"id":"Legacy"}]});
        assert_eq!(editable_curve_slot(&track, "Parameter", None).unwrap(), ("parameterCurves", 0));
        assert_eq!(editable_curve_slot(&track, "Opacity", None).unwrap(), ("partOpacityCurves", 0));
        assert_eq!(editable_curve_slot(&track, "Legacy", None).unwrap(), ("legacyPartOpacityCurves", 0));
        assert!(editable_curve_slot(&track, "", None).is_err());
        track["partOpacityCurves"][0]["id"] = json!("Parameter");
        assert_eq!(editable_curve_slot(&track, "Parameter", None).unwrap_err(), "animation curveId is ambiguous");
    }

    #[test]
    fn image_tracks_only_allow_deletion_and_require_explicit_projection() {
        let container = crate::authoring_edit::tests::test_container(include_str!("../tests/fixtures/can3-image-track.xml"));
        let initial = read(&container).unwrap();
        assert_eq!(initial["scenes"][0]["imageTracks"].as_array().unwrap().len(), 1);
        assert_eq!(rewrite(&container, &initial, &initial).unwrap(), container.encode());
        for field in ["name", "resourceGuid", "start", "internalOffset", "duration", "future"] {
            let mut candidate = initial.clone();
            candidate["scenes"][0]["imageTracks"][0][field] = json!("changed");
            assert!(rewrite(&container, &initial, &candidate).is_err(), "{field}");
        }
        let mut missing = initial.clone();
        missing["scenes"][0].as_object_mut().unwrap().remove("imageTracks");
        assert!(rewrite(&container, &initial, &missing).is_err());
        let mut moved = initial.clone();
        let image = moved["scenes"][0]["imageTracks"].as_array_mut().unwrap().pop().unwrap();
        moved["scenes"][1]["imageTracks"].as_array_mut().unwrap().push(image.clone());
        assert!(rewrite(&container, &initial, &moved).is_err());
        let mut converted = initial.clone();
        converted["scenes"][0]["imageTracks"] = json!([]);
        converted["scenes"][0]["tracks"].as_array_mut().unwrap().push(image);
        assert!(rewrite(&container, &initial, &converted).is_err());
        let mut deleted = initial.clone();
        deleted["scenes"][0]["imageTracks"] = json!([]);
        let bytes = rewrite(&container, &initial, &deleted).unwrap();
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), deleted);
        deleted["scenes"][0]["tracks"].as_array_mut().unwrap().remove(0);
        let bytes = rewrite(&container, &initial, &deleted).unwrap();
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), deleted);
    }

    #[test]
    fn part_opacity_key_edit_budget_does_not_scan_the_document_for_unchanged_curves() {
        let base = include_str!("../tests/fixtures/can3-part-opacity.xml");
        let doc = xml_document(base).unwrap();
        let attribute = doc.descendants().find(|n| n.attribute("xs.id") == Some("part-b")).unwrap();
        let mut entries = String::new();
        let mut members = String::new();
        let mut definitions = String::new();
        for index in 0..20 {
            let id = format!("extra-{index}");
            entries.push_str(&format!(r#"<entry><CAttrId xs.n="key" idstr="live2DPartsOpacity:Extra{index}"/><CMvAttrF xs.n="value" xs.ref="{id}"/></entry>"#));
            members.push_str(&format!(r#"<CMvAttrF xs.ref="{id}"/>"#));
            definitions.push_str(&base[attribute.range()].replace("part-b", &id)
                .replace("PartArmB", &format!("Extra{index}")).replace("part-guid-b", &format!("extra-guid-{index}")));
        }
        let xml = base.replace(r#"<hash_map xs.n="attrMap" count="3">"#,
            &format!(r#"<hash_map xs.n="attrMap" count="23">{entries}"#))
            .replace(r#"<array xs.n="attrList" count="2" type="ICMvAttr">"#,
                &format!(r#"<array xs.n="attrList" count="22" type="ICMvAttr">{members}"#))
            .replace("</shared>", &format!("{definitions}</shared>"));
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let doc = xml_document(&xml).unwrap();
        let graph = Graph::new(&doc).unwrap();
        let track = doc.descendants().find(|n| n.attribute("xs.id") == Some("track")).unwrap();
        let old = &initial["scenes"][0]["tracks"][0];
        assert_eq!(old["partOpacityCurves"].as_array().unwrap().len(), 22);
        let mut new = old.clone();
        let curve = new["partOpacityCurves"].as_array_mut().unwrap().iter_mut()
            .find(|c| c["partId"] == "PartArmA").unwrap();
        curve["points"][0]["anchor"]["value"] = json!(0.25);
        let mut patches = Vec::new();
        let mut additions = NativeAdditions { reserved: HashSet::new(), definitions: String::new(), track_guid_tag: String::new(), next_idx: None };
        let node_count = doc.descendants().count();
        part_opacity_curve_changes(&xml, &graph, track, old, &new, &mut patches,
            &mut Budget::new(node_count * 3 + 1000), &mut additions).unwrap();
        assert_eq!(patches.len(), 2);
        assert!(additions.definitions.is_empty());
    }

    #[test]
    fn part_opacity_rewrite_refuses_metadata_and_budget_changes() {
        let xml = include_str!("../tests/fixtures/can3-part-opacity.xml");
        let container = crate::authoring_edit::tests::test_container(xml);
        let initial = read(&container).unwrap();
        for change in ["identity", "range", "legacy", "unknown"] {
            let mut candidate = initial.clone();
            let track = &mut candidate["scenes"][0]["tracks"][0];
            match change {
                "identity" => track["partOpacityCurves"][0]["partGuid"] = json!("another-part"),
                "range" => track["partOpacityCurves"][0]["range"] = json!([0,2]),
                "legacy" => track["legacyPartOpacityCurves"][0]["legacyMapPartId"] = json!("another-map"),
                "unknown" => track["partOpacityCurves"][0]["future"] = json!(true),
                _ => unreachable!(),
            }
            assert!(rewrite(&container, &initial, &candidate).is_err(), "{change}");
            assert_eq!(read(&container).unwrap(), initial);
        }
        let doc = xml_document(xml).unwrap();
        let graph = Graph::new(&doc).unwrap();
        let track = doc.descendants().find(|n| n.attribute("xs.id") == Some("track")).unwrap();
        let old = &initial["scenes"][0]["tracks"][0];
        let mut new = old.clone();
        new["partOpacityCurves"][0]["points"][0]["anchor"]["value"] = json!(0.25);
        assert!(part_opacity_curve_changes(xml, &graph, track, old, &new,
            &mut Vec::new(), &mut Budget::new(0), &mut NativeAdditions {
                reserved: HashSet::new(), definitions: String::new(), track_guid_tag: String::new(), next_idx: None,
            }).unwrap_err().contains("budget"));
    }

    #[test]
    fn scene_set_plan_cases() {
        let scene = |guid: &str, tracks: &[&str]| {
            json!({"guid": guid, "tracks": tracks.iter().map(|t| json!({"guid": t})).collect::<Vec<_>>(), "imageTracks":[], "soundTracks":[], "guideTracks":[]})
        };
        let old = vec![scene("A", &["t1"]), scene("B", &["t2"]), scene("C", &["t3"])];
        let plan = scene_set_plan(&old, &[scene("B", &["t2"])]).unwrap();
        assert!(plan.ordered.len() == 1 && plan.removed == [0, 2]);
        assert!(matches!(plan.ordered[0], SceneSlot::Kept { original: 1, candidate: 0 }));
        let plan = scene_set_plan(&old, &[scene("A", &["t1"]), scene("X", &[])]).unwrap();
        assert!(matches!(plan.ordered[1], SceneSlot::Added { .. }));
        assert!(scene_set_plan(&old, &[scene("B", &["t1"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("A", &["t1"]), scene("X", &["t1"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("X", &["t1"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("X", &["fresh"]), scene("Y", &["fresh"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("B", &["t2"]), scene("A", &["t1"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("X", &[]), scene("A", &["t1"])]).is_err());
        assert!(scene_set_plan(&old, &[scene("A", &["t1"]), scene("A", &[])]).is_err());
        let dup_old = vec![scene("A", &[]), scene("A", &[])];
        assert!(scene_set_plan(&dup_old, &[scene("A", &[])]).is_err());
        assert!(scene_set_plan(&[scene("A", &["t1"]), scene("B", &["t1"])], &[]).is_err());
        let duplicate = json!({"format":"live2d.animation.source.v1", "scenes":dup_old});
        assert_eq!(delete_scene(&duplicate, &json!({"sceneGuid":"A"})),
            Err("animation sceneGuid is ambiguous".into()));
    }

    #[test]
    fn scene_factory_reserves_deleted_ids() {
        const SCENE_XML: &str = include_str!("../tests/fixtures/can3-scene-editor.xml");
        let xml_of = |bytes: &[u8]| String::from_utf8(CaffDocument::parse(bytes).unwrap().main_xml()).unwrap();
        let bases = [
            ("scene", "soul2d-scene-test&"),
            ("track", "soul2d-track-t&"),
            ("attr", "soul2d-curve-t&-live2dParam_ParamTest"),
        ];
        let mut xml = SCENE_XML.replace(r#"<CAnimation xs.id="animation">"#,
            r#"<CAnimation xs.id="animation"><CSceneSource xs.n="currentScene" xs.ref="scene"/>"#);
        for (old, new) in bases {
            for attribute in ["xs.id", "xs.ref"] {
                xml = xml.replace(
                    &format!(r#"{attribute}="{old}""#),
                    &format!(r#"{attribute}="{}""#, escape(new)),
                );
            }
        }
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let mut model = initial.clone();
        model["scenes"] = json!([{
            "guid": "test&", "name": "N", "fps": 24.0, "durationFrames": 30,
            "imageTracks": [], "soundTracks": [], "guideTracks": [], "tracks": [{
                "guid": "t&", "name": "T", "resourceGuid": "res-guid", "start": 0,
                "internalOffset": 0, "duration": 30,
                "parameterCurves": initial["scenes"][0]["tracks"][0]["parameterCurves"].clone(),
                "partOpacityCurves": [], "legacyPartOpacityCurves": [], "underscorePartOpacityCurves": [],
                "skippedLegacyPartOpacityCount": 0,
            }],
        }]);
        let bytes = rewrite(&container, &initial, &model).unwrap();
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), model);
        let saved = xml_of(&bytes);
        let mut positions = Vec::new();
        for (_, base) in bases {
            assert!(!saved.contains(&format!(r#"xs.id="{}""#, escape(base))), "{saved}");
            positions.push(saved.find(&format!(r#"xs.id="{}-1""#, escape(base))).unwrap());
        }
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{saved}");
        assert!(saved.contains(r#"uuid="test&amp;""#), "{saved}");
        assert!(saved.contains(r#"<CSceneSource xs.n="currentScene" xs.ref="soul2d-scene-test&amp;-1"/>"#), "{saved}");
        let container2 = crate::authoring_edit::tests::test_container(SCENE_XML);
        let initial2 = read(&container2).unwrap();
        let mut deleted = initial2.clone();
        deleted["scenes"] = json!([]);
        let bytes = rewrite(&container2, &initial2, &deleted).unwrap();
        let gone = xml_of(&bytes);
        assert!(!gone.contains(r#"xs.id="scene""#) && !gone.contains(r#"xs.id="scene-kept""#)
            && gone.contains("count='0000'"), "{gone}");
    }

    #[test]
    fn scene_normalization_refuses_metadata() {
        let scene_xml: &str = include_str!("../tests/fixtures/can3-scene-editor.xml");
        let container = crate::authoring_edit::tests::test_container(scene_xml);
        let initial = read(&container).unwrap();
        let with = |f: &dyn Fn(&mut Value)| {
            let mut model = delete_scene(&initial, &json!({"sceneGuid":"scene-guid"})).unwrap();
            f(&mut model["scenes"][0]);
            model
        };
        let tampered_top = {
            let mut model = initial.clone();
            model["resources"] = json!([]);
            model
        };
        for (model, message) in [
            (with(&|s| s["unknownField"] = json!(1)), ONLY_EDITS),
            (with(&|s| s["tracks"][0]["unknown"] = json!(1)), ONLY_EDITS),
            ({
                let mut model = initial.clone();
                model["unknownTopLevel"] = json!(true);
                model
            }, ONLY_EDITS),
            (tampered_top, ONLY_EDITS),
        ] {
            assert_eq!(rewrite(&container, &initial, &model), Err(message.to_owned()));
        }
        let scenes_tag = {
            let at = scene_xml.find(r#"xs.n="_scenes""#).unwrap();
            let start = scene_xml[..at].rfind('<').unwrap();
            let end = scene_xml[at..].find('>').unwrap() + at + 1;
            scene_xml[start..end].to_owned()
        };
        let uncounted_tag = {
            let mut tag = scenes_tag.clone();
            let at = tag.find(" count=").unwrap() + 1;
            let quote = tag.as_bytes()[at + 6] as char;
            let end = tag[at + 7..].find(quote).unwrap() + at + 8;
            tag.replace_range(at..end, "");
            tag
        };
        let container = crate::authoring_edit::tests::test_container(
            &scene_xml.replace(&scenes_tag, &uncounted_tag));
        let initial = read(&container).unwrap();
        let deleted = delete_scene(&initial, &json!({"sceneGuid":"scene-guid"})).unwrap();
        let deleted_then_added = add_scene(
            &deleted, &json!({"name":"Replacement","fps":30,"durationFrames":90}), "replacement",
        ).unwrap();
        assert_eq!(deleted_then_added["scenes"].as_array().unwrap().len(),
            initial["scenes"].as_array().unwrap().len());
        let error = rewrite(&container, &initial, &deleted_then_added).unwrap_err();
        assert!(error.contains("count"), "{error}");
    }

    #[test]
    fn track_new_scene_special_storage_ids_roundtrip() {
        let container = crate::authoring_edit::tests::test_container(
            include_str!("../tests/fixtures/can3-curve-editor.xml"));
        let initial = read(&container).unwrap();
        let candidate = add_scene(
            &initial,
            &json!({"name":"New & scene","fps":30,"durationFrames":120}),
            "scene&new",
        ).unwrap();
        let mut candidate = add_track(
            &candidate,
            &json!({"sceneGuid":"scene&new","resourceGuid":"res-guid","name":"New & track"}),
            "track&new",
        ).unwrap();
        candidate["scenes"][1]["tracks"][0]["parameterCurves"] =
            initial["scenes"][0]["tracks"][0]["parameterCurves"].clone();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let saved = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(read(&saved).unwrap(), candidate);
        let xml = String::from_utf8(saved.main_xml()).unwrap();
        let scene = xml.find(r#"xs.id="soul2d-scene-scene&amp;new""#).unwrap();
        let track = xml.find(r#"xs.id="soul2d-track-track&amp;new""#).unwrap();
        let curve = xml.find(r#"xs.id="soul2d-curve-track&amp;new-live2dParam_ParamTest""#).unwrap();
        assert!(scene < track && track < curve);
    }

    #[test]
    fn track_factory_suffixes_all_opaque_storage_collisions() {
        let opaque = concat!(
            r#"<Opaque xs.id="soul2d-scene-new-scene"/>"#,
            r#"<Opaque xs.id="soul2d-track-new-track"/>"#,
            r#"<Opaque xs.id="soul2d-curve-new-track-live2dParam_ParamTest"/>"#,
        );
        let xml = include_str!("../tests/fixtures/can3-curve-editor.xml")
            .replace("</shared>", &format!("{opaque}</shared>"));
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let candidate = add_scene(
            &initial, &json!({"name":"New","fps":30,"durationFrames":120}), "new-scene",
        ).unwrap();
        let mut candidate = add_track(
            &candidate,
            &json!({"sceneGuid":"new-scene","resourceGuid":"res-guid","name":"New"}),
            "new-track",
        ).unwrap();
        candidate["scenes"][1]["tracks"][0]["parameterCurves"] =
            initial["scenes"][0]["tracks"][0]["parameterCurves"].clone();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let saved = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(read(&saved).unwrap(), candidate);
        let xml = String::from_utf8(saved.main_xml()).unwrap();
        assert!(xml.contains(opaque));
        for id in [
            "soul2d-scene-new-scene-1",
            "soul2d-track-new-track-1",
            "soul2d-curve-new-track-live2dParam_ParamTest-1",
        ] {
            assert!(xml.contains(&format!(r#"xs.id="{id}""#)), "{id}");
        }
        assert!(xml.contains(r#"xs.n="_sceneSource" xs.ref="soul2d-scene-new-scene-1""#));
    }

    #[test]
    fn track_factory_keeps_deleted_storage_ids_reserved() {
        let xml = include_str!("../tests/fixtures/can3-curve-editor.xml")
            .replace(r#"xs.id="track""#, r#"xs.id="soul2d-track-new-track""#)
            .replace(r#"xs.ref="track""#, r#"xs.ref="soul2d-track-new-track""#)
            .replace(r#"xs.id="attr""#, r#"xs.id="soul2d-curve-new-track-live2dParam_ParamTest""#)
            .replace(r#"xs.ref="attr""#, r#"xs.ref="soul2d-curve-new-track-live2dParam_ParamTest""#);
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let candidate = delete_track(
            &initial, &json!({"sceneGuid":"scene-guid","trackGuid":"track-guid"}),
        ).unwrap();
        let mut candidate = add_track(
            &candidate,
            &json!({"sceneGuid":"scene-guid","resourceGuid":"res-guid","name":"New"}),
            "new-track",
        ).unwrap();
        candidate["scenes"][0]["tracks"][1]["parameterCurves"] =
            initial["scenes"][0]["tracks"][0]["parameterCurves"].clone();
        let bytes = rewrite(&container, &initial, &candidate).unwrap();
        let saved = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(read(&saved).unwrap(), candidate);
        let xml = String::from_utf8(saved.main_xml()).unwrap();
        for id in ["soul2d-track-new-track", "soul2d-curve-new-track-live2dParam_ParamTest"] {
            assert!(!xml.contains(&format!(r#"xs.id="{id}""#)));
            assert!(xml.contains(&format!(r#"xs.id="{id}-1""#)), "{id}");
        }
    }

    #[test]
    fn track_membership_refuses_retained_metadata_changes_and_global_guid_reuse() {
        let container = crate::authoring_edit::tests::test_container(
            include_str!("../tests/fixtures/can3-curve-editor.xml"));
        let initial = read(&container).unwrap();
        let candidate = add_track(
            &initial,
            &json!({"sceneGuid":"scene-guid","resourceGuid":"res-guid","name":"New"}),
            "new-track",
        ).unwrap();
        for (key, value) in [
            ("name", json!("Changed")), ("resourceGuid", json!("missing")),
            ("start", json!(1)), ("internalOffset", json!(1)), ("duration", json!(119)),
            ("partOpacityCurves", json!([{}])), ("legacyPartOpacityCurves", json!([{}])),
            ("underscorePartOpacityCurves", json!([{}])),
            ("skippedLegacyPartOpacityCount", json!(1)), ("futureField", json!(true)),
        ] {
            let mut invalid = candidate.clone();
            invalid["scenes"][0]["tracks"][0][key] = value;
            assert!(rewrite(&container, &initial, &invalid).is_err(), "{key}");
        }
        let mut moved = add_scene(
            &initial, &json!({"name":"New","fps":30,"durationFrames":120}), "new-scene",
        ).unwrap();
        let track = moved["scenes"][0]["tracks"].as_array_mut().unwrap().remove(0);
        moved["scenes"][1]["tracks"] = json!([track]);
        assert_eq!(rewrite(&container, &initial, &moved).unwrap_err(),
            "animation track GUID cannot move between scenes");
        let candidate = add_scene(
            &candidate, &json!({"name":"A","fps":30,"durationFrames":120}), "scene-a",
        ).unwrap();
        let mut duplicate = add_scene(
            &candidate, &json!({"name":"B","fps":30,"durationFrames":120}), "scene-b",
        ).unwrap();
        let track = duplicate["scenes"][0]["tracks"].as_array_mut().unwrap().pop().unwrap();
        duplicate["scenes"][1]["tracks"] = json!([track.clone()]);
        duplicate["scenes"][2]["tracks"] = json!([track]);
        assert_eq!(rewrite(&container, &initial, &duplicate).unwrap_err(),
            "duplicate animation track GUID");
    }

    #[test]
    fn base_values_are_read_where_stored_and_kept_on_readd() {
        let xml = include_str!("../tests/fixtures/can3-curve-editor.xml").replacen(
            r#"<d xs.n="curMax">1.0000</d>"#, r#"<d xs.n="curMax">1.0000</d><d xs.n="baseValue">0.5</d>"#, 1);
        assert!(xml.contains("baseValue"));
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let first = |model: &Value| model["scenes"][0]["tracks"][0]["parameterCurves"][0].clone();
        assert_eq!(first(&initial)["baseValue"], json!(0.5));
        assert!(initial["scenes"][0]["tracks"][1]["parameterCurves"][0].get("baseValue").is_none());
        // A stored baseValue is not an edit target.
        let mut edited = initial.clone();
        edited["scenes"][0]["tracks"][0]["parameterCurves"][0]["baseValue"] = json!(0.75);
        assert_eq!(rewrite(&container, &initial, &edited), Err(ONLY_EDITS.to_owned()));
        // A deleted curve added again keeps what its original stored; a curve never stored takes the default.
        let source = json!({"parameters":[
            {"id":"ParamTest","guid":"parameter-guid","minimum":-1.0,"maximum":1.0,"default":0.25},
            {"id":"ParamNew","guid":"new-guid","minimum":-2.0,"maximum":2.0,"default":0.125}]});
        let place = json!({"sceneGuid":"scene-guid","trackGuid":"track-guid"});
        let deleted = delete_curve(&initial, &json!({
            "sceneGuid":"scene-guid","trackGuid":"track-guid","curveId":"live2dParam_ParamTest"})).unwrap();
        let mut request = place.clone();
        request["parameterId"] = json!("ParamTest");
        let readded = add_curve(&deleted, &initial, &request, &source).unwrap();
        assert_eq!(first(&readded)["baseValue"], json!(0.5));
        let bytes = rewrite(&container, &initial, &readded).unwrap();
        assert_eq!(read(&CaffDocument::parse(&bytes).unwrap()).unwrap(), readded);
        request["parameterId"] = json!("ParamNew");
        let added = add_curve(&initial, &initial, &request, &source).unwrap();
        assert_eq!(added["scenes"][0]["tracks"][0]["parameterCurves"][1]["baseValue"], json!(0.125));
    }

    #[test]
    fn new_curves_accept_only_a_finite_base_value() {
        let curve = |base: Option<Value>| {
            let mut curve = json!({"id":"live2dParam_P","parameterId":"P","parameterGuid":"g",
                "sourceParameter":{"type":"CParameterId","value":"P"},"range":[0.0,1.0],"points":[],"curveTypes":[]});
            if let Some(base) = base {
                curve["baseValue"] = base;
            }
            curve
        };
        assert!(check_new_curve(&curve(None)).is_ok());
        assert!(check_new_curve(&curve(Some(json!(0.5)))).is_ok());
        for bad in [json!("0.5"), json!(null)] {
            assert_eq!(check_new_curve(&curve(Some(bad))).unwrap_err(), "animation curve baseValue must be finite");
        }
        for base in [None, Some(json!(0.5))] {
            let mut extra = curve(base);
            extra["future"] = json!(1);
            assert_eq!(check_new_curve(&extra).unwrap_err(), "animation curve carries unsupported keys");
        }
        let with = serialize_curve("s", "t", &curve(Some(json!(0.5)))).unwrap();
        assert!(with.contains(r#"<d xs.n="curMax">0</d><d xs.n="baseValue">0.5</d></CMutableSequence>"#), "{with}");
        assert!(!serialize_curve("s", "t", &curve(None)).unwrap().contains("baseValue"));
    }

    #[test]
    fn track_set_plan_rejects_reorder_and_missing() {
        let track = |guid: &str| json!({"guid": guid});
        let old = vec![track("A"), track("B"), track("C")];
        let plan = track_set_plan(&old, &[track("B"), track("D")]).unwrap();
        assert!(plan.ordered.len() == 2
            && matches!(plan.ordered[0], TrackSlot::Kept { original: 1, .. })
            && matches!(plan.ordered[1], TrackSlot::Added { .. })
            && plan.removed == [0, 2]);
        let plan = track_set_plan(&old, &[track("A"), track("X"), track("Y")]).unwrap();
        assert!(plan.ordered.len() == 3
            && matches!(plan.ordered[0], TrackSlot::Kept { original: 0, .. }));
        assert!(track_set_plan(&old, &[track("B"), track("A")]).is_err());
        assert!(track_set_plan(&old, &[track("X"), track("A")]).is_err());
        assert!(track_set_plan(&old, &[track("A"), track("X"), track("B")]).is_err());
        let dup_old = vec![track("A"), track("A")];
        assert!(track_set_plan(&dup_old, &[track("A")]).is_err());
        assert!(track_set_plan(&old, &[track("A"), track("A")]).is_err());
    }

    #[test]
    fn raw_key_edits_keep_negative_frames_controls_and_other_source_fields() {
        let model = json!({"format":"live2d.animation.source.v1", "future":"keep",
            "scenes":[{"guid":"scene", "tracks":[{"guid":"track", "parameterCurves":[{
                "id":"live2dParam_Test", "range":[-1,0], "points":[{
                    "anchor":{"frame":-352,"value":1.0}, "previous":{"value":-9.0},
                    "next":{"value":5.0}}]}]}]}]});
        let request = json!({"sceneGuid":"scene", "trackGuid":"track", "curveId":"live2dParam_Test",
            "anchorFrame":-352, "value":2.125});
        let edited = set_key(&model, &request).unwrap();
        let point = &edited["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0];
        assert_eq!(
            point,
            &json!({"anchor":{"frame":-352,"value":2.125},
            "previous":{"value":-9.0},"next":{"value":5.0}})
        );
        assert_eq!(edited["future"], "keep");
        for (field, value) in [
            ("anchorFrame", json!(1.5)),
            ("value", json!(true)),
            ("sceneGuid", json!("missing")),
            ("trackGuid", json!(null)),
            ("curveId", json!("")),
        ] {
            let mut invalid = request.clone();
            invalid[field] = value;
            assert!(set_key(&model, &invalid).is_err());
        }
        assert_eq!(
            model["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0]["anchor"]["value"],
            1.0
        );
    }

    #[test]
    fn scene_edits_check_their_settings_and_new_identity() {
        let model = json!({"format":"live2d.animation.source.v1", "scenes":[{"guid":"s", "name":"A", "fps":30.0,
            "durationFrames":10, "tracks":[{"guid":"t"}]}]});
        let set = set_scene(&model, &json!({"sceneGuid":"s", "durationFrames":2147483647})).unwrap();
        assert_eq!(set["scenes"][0], json!({"guid":"s", "name":"A", "fps":30.0, "durationFrames":2147483647,
            "tracks":[{"guid":"t"}]}));
        let request = json!({"name":"B", "fps":0.5, "durationFrames":0});
        assert_eq!(add_scene(&model, &request, "n").unwrap()["scenes"][1],
            json!({"guid":"n", "name":"B", "fps":0.5, "durationFrames":0, "tracks":[], "imageTracks":[], "soundTracks":[], "guideTracks":[]}));
        assert_eq!(add_scene(&model, &request, "s"), Err("animation sceneGuid is already used".to_owned()));
        let inx = json!({"scenes":[]});
        assert_eq!(set_scene(&inx, &json!({"sceneGuid":"s", "name":"x"})),
            Err("scene editing requires a CAN3 source model".to_owned()));
        assert_eq!(add_scene(&inx, &request, "n"), Err("scene editing requires a CAN3 source model".to_owned()));
        assert_ne!(new_guid().unwrap(), new_guid().unwrap());
        assert_eq!(guid_text([0; 16]), "00000000-0000-4000-8000-000000000000");
        assert_eq!(guid_text([0xff; 16]), "ffffffff-ffff-4fff-bfff-ffffffffffff");
    }

    #[test]
    fn scene_rewrites_keep_stored_types_and_refuse_what_they_cannot_write() {
        let xml_of = |bytes: &[u8]| String::from_utf8(CaffDocument::parse(bytes).unwrap().main_xml()).unwrap();
        let container = crate::authoring_edit::tests::test_container(&parts_visible_project());
        let initial = read(&container).unwrap();
        let mut retimed = initial.clone();
        retimed["scenes"][0]["fps"] = json!(12.5);
        let saved = xml_of(&rewrite(&container, &initial, &retimed).unwrap());
        assert_eq!(saved, parts_visible_project().replace(r#"<d xs.n="fps">30</d>"#, r#"<d xs.n="fps">12.5</d>"#));
        let scene = json!({"guid":"g", "name":"N", "fps":24.0, "durationFrames":1, "tracks":[], "imageTracks":[], "soundTracks":[], "guideTracks":[]});
        let with = |change: &dyn Fn(&mut Value)| {
            let mut model = initial.clone();
            let mut added = scene.clone();
            change(&mut added);
            model["scenes"].as_array_mut().unwrap().push(added);
            model
        };
        let saved = xml_of(&rewrite(&container, &initial, &with(&|_| {})).unwrap());
        assert!(saved.contains(concat!(r#"<CSceneSource xs.id="soul2d-scene-g"><CSceneGuid xs.n="guid" uuid="g"/>"#,
            r#"<CAnimation xs.n="_animation" xs.ref="animation"/><s xs.n="sceneName">N</s>"#,
            r#"<MovieInfo xs.n="movieInfo"><d xs.n="fps">24</d><i xs.n="duration">1</i></MovieInfo>"#,
            r#"<TrackSourceSet xs.n="trackSourceSet"><carray_list xs.n="_sources" count="0"/></TrackSourceSet>"#,
            "</CSceneSource>")), "{saved}");
        let mut removed = initial.clone();
        removed["scenes"] = json!([]);
        let deleted = rewrite(&container, &initial, &removed).unwrap();
        assert_eq!(read(&CaffDocument::parse(&deleted).unwrap()).unwrap(), removed);
        let source = parts_visible_project();
        let document = roxmltree::Document::parse(&source).unwrap();
        let mut ranges = document.descendants().filter(|node| {
            matches!(node.attribute("xs.id"), Some("scene" | "track" | "parameter-effect"
                | "parts-effect" | "part-arm-a" | "part-arm-b"))
                || (node.attribute("xs.ref") == Some("scene")
                    && node.parent().and_then(|parent| parent.attribute("xs.n")) == Some("_scenes"))
        }).map(|node| node.range()).collect::<Vec<_>>();
        ranges.sort_by_key(|range| range.start);
        let mut expected = source.clone();
        for range in ranges.into_iter().rev() { expected.replace_range(range, ""); }
        expected = expected.replace(r#"<carray_list xs.n="_scenes" count="1">"#,
            r#"<carray_list xs.n="_scenes" count="0">"#);
        assert_eq!(xml_of(&deleted), expected);
        let mut moved = initial.clone();
        moved["scenes"][0]["guid"] = json!("other");
        for (model, message) in [
            (moved, "animation track GUID cannot move between scenes"),
            (with(&|s| s["tracks"] = json!([{}])), "animation track GUID missing"),
            (with(&|s| s["guid"] = json!(" g")), "a new animation scene needs a GUID"),
            (with(&|s| s["guid"] = json!("")), "a new animation scene needs a GUID"),
            (with(&|s| s["name"] = json!(5)), "a new animation scene has an invalid setting"),
            (with(&|s| s["fps"] = json!("24")), "a new animation scene has an invalid setting"),
            (with(&|s| s["durationFrames"] = json!(1.5)), "a new animation scene has an invalid setting"),
        ] {
            assert_eq!(rewrite(&container, &initial, &model), Err(message.to_owned()));
        }
        let container = crate::authoring_edit::tests::test_container(
            &parts_visible_project().replace("</shared>", r#"<Opaque xs.id="soul2d-scene-g"/></shared>"#));
        let initial = read(&container).unwrap();
        let added = with(&|_| {});
        let saved = xml_of(&rewrite(&container, &initial, &added).unwrap());
        assert!(saved.contains(r#"<Opaque xs.id="soul2d-scene-g"/>"#)
            && saved.contains(r#"xs.id="soul2d-scene-g-1""#), "{saved}");
        for (from, to, change, message) in [
            (r#"<s xs.n="sceneName">Parts Scene</s>"#, r#"<s xs.n="sceneName" extra="1">Parts Scene</s>"#,
                Some(("name", json!("X"))), "animation scene sceneName requires plain inline storage"),
            (r#"<d xs.n="fps">30</d>"#, r#"<i xs.n="fps">30</i>"#, Some(("fps", json!(24.0))),
                "animation scene fps requires plain inline storage"),
            (r#"<s xs.n="sceneName">Parts Scene</s>"#, r#"<s xs.n="sceneName">Parts Scene<!-- c --></s>"#,
                Some(("name", json!("X"))), "animation scene sceneName requires plain inline storage"),
        ] {
            let container = crate::authoring_edit::tests::test_container(&parts_visible_project().replace(from, to));
            let initial = read(&container).unwrap();
            let model = match &change {
                None => with(&|_| {}),
                Some((key, value)) => {
                    let mut model = initial.clone();
                    model["scenes"][0][*key] = value.clone();
                    model
                }
            };
            assert_eq!(rewrite(&container, &initial, &model), Err(message.to_owned()), "{to}");
        }
        for variant in [
            r#"<carray_list xs.n="_scenes" count="1" extra="1">"#,
            r#"<carray_list xs.n="_scenes" count="1" kind="1">"#,
        ] {
            let container = crate::authoring_edit::tests::test_container(
                &parts_visible_project().replace(r#"<carray_list xs.n="_scenes" count="1">"#, variant));
            let initial = read(&container).unwrap();
            let model = with(&|_| {});
            let saved = xml_of(&rewrite(&container, &initial, &model).unwrap());
            let expected = variant.replace("count=\"1\"", "count=\"2\"");
            assert!(saved.contains(&expected), "{variant}: {saved}");
        }
        let uncounted = parts_visible_project().replace(r#"<carray_list xs.n="_scenes" count="1">"#,
            r#"<carray_list xs.n="_scenes">"#);
        let container = crate::authoring_edit::tests::test_container(&uncounted);
        let mut renamed = read(&container).unwrap();
        renamed["scenes"][0]["name"] = json!("Renamed");
        let saved = xml_of(&rewrite(&container, &read(&container).unwrap(), &renamed).unwrap());
        assert_eq!(saved, uncounted.replace("Parts Scene", "Renamed"), "a list without a count only matters to an add");
        let project = r#"<CAnimation ID><s xs.n="name">Inline</s>
            <ResourceManager xs.n="resourceManager"><carray_list xs.n="_resourceRefList" count="0"/></ResourceManager>
            <carray_list xs.n="_scenes" count="0"/></CAnimation>"#;
        for (id, shared, message) in [
            ("", "<shared></shared>", "the animation project has no shared identity a new scene can name"),
            (r#"xs.id="animation""#, "", "animation XML lacks a shared section"),
            (r#"xs.id="animation""#, "<shared/>", "animation shared section is malformed"),
        ] {
            let xml = format!(r#"<root fileFormatVersion="401000004"><main>{}</main>{shared}</root>"#,
                project.replace("ID", id));
            let container = crate::authoring_edit::tests::test_container(&xml);
            let initial = read(&container).unwrap();
            let mut model = initial.clone();
            model["scenes"] = json!([scene]);
            assert_eq!(rewrite(&container, &initial, &model), Err(message.to_owned()), "{xml}");
        }
    }

    #[test]
    fn reads_an_empty_project_without_treating_it_as_a_model() {
        let xml = r#"<root fileFormatVersion="401000004"><main><CAnimation xs.ref="animation"/></main>
            <shared><CAnimation xs.id="animation"><s xs.n="name">Empty</s>
            <ResourceManager xs.n="resourceManager"><carray_list xs.n="_resourceRefList" count="0"/></ResourceManager>
            <carray_list xs.n="_scenes" count="0"/></CAnimation></shared></root>"#;
        assert_eq!(
            read_xml(xml).unwrap(),
            json!({
                "format": "live2d.animation.source.v1", "fileFormatVersion": 401000004,
                "name": "Empty", "resources": [], "imageResources": [], "scenes": [],
            })
        );
    }

    fn part_attribute_xml(
        xml_id: &str,
        part_id: &str,
        part_guid: &str,
        value: &str,
    ) -> String {
        format!(
            r#"<CMvAttrF xs.id="{xml_id}">
                <CAttrId xs.n="id" idstr="live2DPartsOpacity_{part_id}"/>
                <CPartGuid xs.n="guid" uuid="{part_guid}"/>
                <d xs.n="rangeMin">0.0</d><d xs.n="rangeMax">1.0</d>
                <hash_map xs.n="optionParam" count="1" keyType="string">
                    <s xs.n="KEY_PARTS_VISIBLE_ID">{part_id}</s>
                </hash_map>
                <CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
                <CMutableSequence xs.n="valueData">
                    <array xs.n="points" count="1" type="CBezierPt"><CBezierPt>
                        <CSeqPt xs.n="anchor"><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">{value}</d><b xs.n="isCorner">true</b>
                        </CSeqPt>
                        <CBezierCtrlPt xs.n="prev"><d xs.n="posF">0</d><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">{value}</d><b xs.n="isPosOptimized">false</b>
                        </CBezierCtrlPt>
                        <CBezierCtrlPt xs.n="next"><d xs.n="posF">0</d><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">{value}</d><b xs.n="isPosOptimized">false</b>
                        </CBezierCtrlPt>
                    </CBezierPt></array>
                    <carray_list xs.n="curveTypes" count="1"><CCurveType v="SMOOTH"/></carray_list>
                    <CMvAttrF xs.n="attr" xs.ref="{xml_id}"/>
                </CMutableSequence>
            </CMvAttrF>"#
        )
    }

    fn parts_visible_project() -> String {
        let arm_a = part_attribute_xml("part-arm-a", "PartArmA", "part-guid-a", "0.25");
        let arm_b = part_attribute_xml("part-arm-b", "PartArmB", "part-guid-b", "0.75");
        format!(
            r#"<root fileFormatVersion="401000004">
                <main><CAnimation xs.ref="animation"/></main>
                <shared>
                    <CAnimation xs.id="animation">
                        <s xs.n="name">Parts</s>
                        <ResourceManager xs.n="resourceManager"><carray_list xs.n="_resourceRefList" count="1">
                            <CResourceData xs.ref="resource-data"/>
                        </carray_list></ResourceManager>
                        <carray_list xs.n="_scenes" count="1"><CSceneSource xs.ref="scene"/></carray_list>
                    </CAnimation>
                    <CResourceData xs.id="resource-data">
                        <CResource_Linked_Model xs.n="resourceRef">
                            <CResourceGuid xs.n="guid" uuid="resource-guid"/>
                            <s xs.n="name">Model</s><s xs.n="srcFile">model.cmo3</s>
                        </CResource_Linked_Model>
                    </CResourceData>
                    <CSceneSource xs.id="scene">
                        <CSceneGuid xs.n="guid" uuid="scene-guid"/>
                        <s xs.n="sceneName">Parts Scene</s>
                        <CAnimation xs.n="_animation" xs.ref="animation"/>
                        <MovieInfo xs.n="movieInfo"><d xs.n="fps">30</d><i xs.n="duration">1</i></MovieInfo>
                        <TrackSourceSet xs.n="trackSourceSet"><carray_list xs.n="_sources" count="1">
                            <CMvTrack_Live2DModel_Source xs.ref="track"/>
                        </carray_list></TrackSourceSet>
                    </CSceneSource>
                    <CMvTrack_Live2DModel_Source xs.id="track">
                        <CMvTrackGuid xs.n="guid" uuid="track-guid"/>
                        <s xs.n="name">Parts</s>
                        <CSceneSource xs.n="_sceneSource" xs.ref="scene"/>
                        <CResourceGuid xs.n="_resourceGuid" uuid="resource-guid"/>
                        <i xs.n="start">0</i><i xs.n="internalOffset">0</i><i xs.n="duration">1</i>
                        <CMvEffect_Live2DParameter xs.n="keyParamEffect" xs.ref="parameter-effect"/>
                        <CMvEffect_Live2DPartsVisible xs.n="partsVisibleEffect" xs.ref="parts-effect"/>
                    </CMvTrack_Live2DModel_Source>
                    <CMvEffect_Live2DParameter xs.id="parameter-effect">
                        <CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
                        <array xs.n="attrList" count="0" type="ICMvAttr"/>
                    </CMvEffect_Live2DParameter>
                    <CMvEffect_Live2DPartsVisible xs.id="parts-effect">
                        <CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
                        <hash_map xs.n="attrMap" count="2">
                            <entry><CAttrId xs.n="key" idstr="live2DPartsOpacity:PartArmA"/>
                                <CMvAttrF xs.n="value" xs.ref="part-arm-a"/></entry>
                            <entry><CAttrId xs.n="key" idstr="live2DPartsOpacity:PartArmB"/>
                                <CMvAttrF xs.n="value" xs.ref="part-arm-b"/></entry>
                        </hash_map>
                    </CMvEffect_Live2DPartsVisible>
                    {arm_a}{arm_b}
                </shared>
            </root>"#
        )
    }

    fn legacy_parts_visible_project() -> String {
        parts_visible_project()
            .replace(
                "live2DPartsOpacity:PartArmA",
                "live2DPartsOpacity:legacy-map-only",
            )
            .replace(
                "live2DPartsOpacity_PartArmA",
                "live2DPartsOpacity:legacy-attribute-only",
            )
            .replace(
                "<s xs.n=\"KEY_PARTS_VISIBLE_ID\">PartArmA</s>",
                "<s xs.n=\"KEY_PARTS_VISIBLE_ID\">legacy-source-only</s>",
            )
    }

    fn parameter_attribute_xml(option_param: &str) -> String {
        format!(
            r#"<CMvAttrF xs.id="parameter-attribute">
                <CAttrId xs.n="id" idstr="live2dParam_ParamAngleX"/>
                <CParamGuid xs.n="guid" uuid="parameter-guid"/>
                <d xs.n="rangeMin">-30.0</d><d xs.n="rangeMax">30.0</d>
                {option_param}
                <CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
                <CMutableSequence xs.n="valueData">
                    <array xs.n="points" count="1" type="CBezierPt"><CBezierPt>
                        <CSeqPt xs.n="anchor"><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">0.25</d><b xs.n="isCorner">true</b>
                        </CSeqPt>
                        <CBezierCtrlPt xs.n="prev"><d xs.n="posF">0</d><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">0.25</d><b xs.n="isPosOptimized">false</b>
                        </CBezierCtrlPt>
                        <CBezierCtrlPt xs.n="next"><d xs.n="posF">0</d><i xs.n="pos">0</i>
                            <d xs.n="doubleValue">0.25</d><b xs.n="isPosOptimized">false</b>
                        </CBezierCtrlPt>
                    </CBezierPt></array>
                    <carray_list xs.n="curveTypes" count="1"><CCurveType v="SMOOTH"/></carray_list>
                    <CMvAttrF xs.n="attr" xs.ref="parameter-attribute"/>
                </CMutableSequence>
            </CMvAttrF>"#
        )
    }

    fn parameter_project(option_param: &str) -> String {
        let project = parts_visible_project().replace(
            "<array xs.n=\"attrList\" count=\"0\" type=\"ICMvAttr\"/>",
            "<array xs.n=\"attrList\" count=\"1\" type=\"ICMvAttr\"><CMvAttrF xs.ref=\"parameter-attribute\"/></array>",
        );
        let attribute = parameter_attribute_xml(option_param);
        project.replace("</shared>", &format!("{attribute}</shared>"))
    }

    #[test]
    fn projects_present_empty_parameter_maps_as_null_metadata() {
        let project =
            parameter_project(r#"<hash_map xs.n="optionParam" count="0" keyType="string"/>"#);
        let source = read_xml(&project).unwrap();
        let curve = &source["scenes"][0]["tracks"][0]["parameterCurves"][0];
        assert_eq!(curve.get("sourceParameter"), Some(&Value::Null));
    }

    #[test]
    fn rejects_missing_malformed_and_unknown_parameter_maps() {
        for option_param in [
            "",
            r#"<hash_map xs.n="optionParam" keyType="string"/>"#,
            r#"<hash_map xs.n="optionParam" count="1" keyType="string"/>"#,
            r#"<hash_map xs.n="optionParam" count="1" keyType="string"><s xs.n="OTHER">ParamAngleX</s></hash_map>"#,
            r#"<array xs.n="optionParam" count="0"/>"#,
        ] {
            assert!(
                read_xml(&parameter_project(option_param)).is_err(),
                "{option_param}"
            );
        }
    }

    #[test]
    fn preserves_populated_legacy_parameter_metadata_container() {
        let project = parameter_project(
            r#"<COptionParam xs.n="optionParam"><CParameterId xs.n="KEY_PARAM_ID" idstr="ParamAngleX"/></COptionParam>"#,
        );
        let source = read_xml(&project).unwrap();
        assert_eq!(
            source["scenes"][0]["tracks"][0]["parameterCurves"][0]["sourceParameter"],
            json!({"type":"CParameterId","value":"ParamAngleX"})
        );
        assert!(read_xml(&parameter_project(
            r#"<COptionParam xs.n="optionParam" count="0"/>"#
        )).is_err());
    }

    #[test]
    fn reads_parts_visible_effect_curves_from_the_attribute_map() {
        let model = read_xml(&parts_visible_project()).unwrap();
        let curves = &model["scenes"][0]["tracks"][0]["partOpacityCurves"];
        assert_eq!(curves.as_array().unwrap().len(), 2);
        assert_eq!(curves[0], json!({
            "id":"live2DPartsOpacity_PartArmA",
            "partId":"PartArmA",
            "partGuid":"part-guid-a",
            "sourcePart":{"type":"s","value":"PartArmA"},
            "range":[0.0,1.0],
            "points":[{"anchor":{"frame":0,"value":0.25},
                "previous":{"frame":0.0,"integerFrame":0,"value":0.25,"positionOptimized":false},
                "next":{"frame":0.0,"integerFrame":0,"value":0.25,"positionOptimized":false},
                "corner":true}],
            "curveTypes":["SMOOTH"]
        }));
    }

    #[test]
    fn validates_part_map_identity_and_finite_range() {
        let original = parts_visible_project();
        for changed in [
            original.replace(
                "live2DPartsOpacity:PartArmA",
                "live2DPartsOpacity:PartWrong",
            ),
            original.replace(
                "live2DPartsOpacity:PartArmB",
                "live2DPartsOpacity:PartArmA",
            ),
            original.replace(
                "live2DPartsOpacity_PartArmA",
                "unsupportedPartOpacity_PartArmA",
            ),
            original.replace("<d xs.n=\"rangeMin\">0.0</d>", "<d xs.n=\"rangeMin\">NaN</d>"),
        ] {
            assert!(read_xml(&changed).is_err());
        }
        let part_id_variant = original.replace(
            "<s xs.n=\"KEY_PARTS_VISIBLE_ID\">PartArmA</s>",
            "<CPartId xs.n=\"KEY_PARTS_VISIBLE_ID\" xs.ref=\"part-id-arm-a\"/>",
        ).replace(
            "</shared>",
            "<CPartId xs.id=\"part-id-arm-a\" idstr=\"PartArmA\"/></shared>",
        );
        let model = read_xml(&part_id_variant).unwrap();
        assert_eq!(model["scenes"][0]["tracks"][0]["partOpacityCurves"][0]["sourcePart"],
            json!({"type":"CPartId","value":"PartArmA"}));

        let legacy_channel = original.replace(
            "live2DPartsOpacity_PartArmA",
            "live2DPartsOpacity:PartArmA",
        );
        let model = read_xml(&legacy_channel).unwrap();
        assert_eq!(model["scenes"][0]["tracks"][0]["partOpacityCurves"].as_array().unwrap().len(), 1);
        assert_eq!(
            model["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(model["scenes"][0]["tracks"][0]["skippedLegacyPartOpacityCount"], 1);
    }

    #[test]
    fn reads_legacy_part_curves_without_collapsing_their_raw_id_namespaces() {
        let model = read_xml(&legacy_parts_visible_project()).unwrap();
        let track = &model["scenes"][0]["tracks"][0];
        assert_eq!(track["partOpacityCurves"].as_array().unwrap().len(), 1);
        assert_eq!(
            track["legacyPartOpacityCurves"],
            json!([{
                "id":"live2DPartsOpacity:legacy-attribute-only",
                "legacyMapPartId":"legacy-map-only",
                "legacyAttributeId":"legacy-attribute-only",
                "partGuid":"part-guid-a",
                "sourcePart":{"type":"s","value":"legacy-source-only"},
                "range":[0.0,1.0],
                "points":[{"anchor":{"frame":0,"value":0.25},
                    "previous":{"frame":0.0,"integerFrame":0,"value":0.25,"positionOptimized":false},
                    "next":{"frame":0.0,"integerFrame":0,"value":0.25,"positionOptimized":false},
                    "corner":true}],
                "curveTypes":["SMOOTH"]
            }])
        );
        assert_eq!(track["skippedLegacyPartOpacityCount"], 1);
    }

    #[test]
    fn colon_part_source_options_keep_their_raw_identity_boundary() {
        let original = include_str!("../tests/fixtures/can3-part-opacity.xml");
        let modern = original.replace("<s xs.n=\"KEY_PARTS_VISIBLE_ID\">PartArmA</s>",
            "<Opaque xs.n=\"super\"><s xs.n=\"KEY_PARTS_VISIBLE_ID\">PartArmA</s><f xs.n=\"KEY_ATTR_FADE_IN\">1</f></Opaque>");
        let legacy = original.replace("<s xs.n=\"KEY_PARTS_VISIBLE_ID\">LegacySource</s>",
            "<Opaque xs.n=\"super\"><s xs.n=\"KEY_PARTS_VISIBLE_ID\">LegacySource</s><f xs.n=\"KEY_ATTR_FADE_IN\">1</f></Opaque>");
        for xml in [modern, legacy] {
            assert_eq!(read_xml(&xml).unwrap_err(), "animation part identity requires KEY_PARTS_VISIBLE_ID");
        }
    }

    #[test]
    fn underscore_part_key_metadata_and_invalid_membership_changes_are_refused() {
        let xml = include_str!("../tests/fixtures/can3-underscore-opacity.xml");
        let container = crate::authoring_edit::tests::test_container(xml);
        let initial = read(&container).unwrap();
        assert!(editable_curve_slot(&initial["scenes"][0]["tracks"][0], "live2DPartsOpacity_PartArmA", None).is_err());
        let mut unique = initial["scenes"][0]["tracks"][0].clone();
        unique["underscorePartOpacityCurves"].as_array_mut().unwrap().pop();
        assert_eq!(editable_curve_slot(&unique, "live2DPartsOpacity_PartArmA", None).unwrap(),
            ("underscorePartOpacityCurves", 0));
        for field in ["id", "mapKey", "partGuid", "sourcePart", "range", "future"] {
            let mut candidate = initial.clone();
            candidate["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][0][field] = json!("changed");
            assert!(rewrite(&container, &initial, &candidate).is_err(), "{field}");
        }
        for value in [json!(null), json!(0), json!(2), json!(4)] {
            let mut candidate = initial.clone();
            candidate["scenes"][0]["tracks"][0]["skippedLegacyPartOpacityCount"] = value;
            assert!(rewrite(&container, &initial, &candidate).is_err());
        }
        let mut added = initial.clone();
        let mut extra = added["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][0].clone();
        extra["mapKey"] = json!("live2DPartsOpacity_extra");
        added["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"].as_array_mut().unwrap().push(extra);
        added["scenes"][0]["tracks"][0]["skippedLegacyPartOpacityCount"] = json!(4);
        assert!(rewrite(&container, &initial, &added).is_err());
        for remove in [false, true] {
            let mut candidate = initial.clone();
            let curves = candidate["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"].as_array_mut().unwrap();
            if remove { curves.pop(); } else { curves.swap(0, 1); }
            assert!(rewrite(&container, &initial, &candidate).is_err());
        }
        for (key, storage) in [("live2DPartsOpacity_PartArmA", "part-a"), ("live2DPartsOpacity_PartArmB", "part-b")] {
            let request = json!({"sceneGuid":"scene-guid","trackGuid":"track-guid",
                "curveId":"live2DPartsOpacity_PartArmA","partOpacityMapKey":key,"anchorFrame":0,"value":0.4});
            let candidate = set_key(&initial, &request).unwrap();
            let encoded = rewrite(&container, &initial, &candidate).unwrap();
            let saved = CaffDocument::parse(&encoded).unwrap();
            assert_eq!(read(&saved).unwrap(), candidate);
            let doc = xml_document(xml).unwrap();
            let range = doc.descendants().find(|node| node.attribute("xs.id") == Some(storage)).unwrap().range();
            let edited = xml[range.clone()].replace("<d xs.n=\"doubleValue\">0</d>", "<d xs.n=\"doubleValue\">0.4</d>")
                .replace("<d xs.n=\"curMin\">0.0000</d>", "<d xs.n=\"curMin\">0.4</d>");
            let expected = format!("{}{}{}", &xml[..range.start], edited, &xml[range.end..]);
            assert_eq!(saved.main_xml(), expected.as_bytes());
        }
    }

    #[test]
    fn legacy_part_keys_with_absent_members_preserve_identity_and_refuse_metadata_or_order_changes() {
        let xml = legacy_parts_visible_project().replace(
            r#"<CMvAttrF xs.n="attr" xs.ref="part-arm-a"/>"#,
            r#"<CMvAttrF xs.n="attr" xs.ref="part-arm-a"/><d xs.n="curMin">0.25</d><d xs.n="curMax">0.25</d>"#,
        );
        let container = crate::authoring_edit::tests::test_container(&xml);
        let initial = read(&container).unwrap();
        let mut candidate = initial.clone();
        candidate["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"][0]["points"][0]["anchor"]["value"] = json!(0.5);
        let encoded = rewrite(&container, &initial, &candidate).unwrap();
        assert_eq!(read(&CaffDocument::parse(&encoded).unwrap()).unwrap(), candidate);
        for field in ["id", "legacyMapPartId", "legacyAttributeId", "partGuid", "sourcePart", "range", "future"] {
            let mut candidate = initial.clone();
            candidate["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"][0][field] = json!("changed");
            assert!(rewrite(&container, &initial, &candidate).is_err(), "{field}");
        }
        let two = include_str!("../tests/fixtures/can3-part-opacity.xml")
            .replace("live2DPartsOpacity_PartArmA", "live2DPartsOpacity:another-legacy");
        let container = crate::authoring_edit::tests::test_container(&two);
        let initial = read(&container).unwrap();
        for remove in [false, true] {
            let mut candidate = initial.clone();
            let curves = candidate["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"].as_array_mut().unwrap();
            assert_eq!(curves.len(), 2);
            if remove { curves.pop(); } else { curves.swap(0, 1); }
            assert!(rewrite(&container, &initial, &candidate).is_err());
        }
    }

    #[test]
    fn rejects_malformed_legacy_part_owners_sequences_and_guids() {
        let original = legacy_parts_visible_project();
        for changed in [
            original.replacen(
                "<CMvTrack_Live2DModel_Source xs.n=\"track\" xs.ref=\"track\"/>",
                "<CMvTrack_Live2DModel_Source xs.n=\"track\" xs.ref=\"other\"/>",
                1,
            ),
            original.replace(
                "<CMvAttrF xs.n=\"attr\" xs.ref=\"part-arm-a\"/>",
                "<CMvAttrF xs.n=\"attr\" xs.ref=\"part-arm-b\"/>",
            ),
            original.replace(
                "<CPartGuid xs.n=\"guid\" uuid=\"part-guid-a\"/>",
                "<CAttrGuid xs.n=\"guid\" uuid=\"part-guid-a\"/>",
            ),
            original.replace(
                "<CPartGuid xs.n=\"guid\" uuid=\"part-guid-a\"/>",
                "<CPartGuid xs.n=\"guid\" uuid=\"\"/>",
            ),
            original.replacen(
                "<d xs.n=\"rangeMin\">0.0</d>",
                "<d xs.n=\"rangeMin\">NaN</d>",
                1,
            ),
            original.replace(
                "<s xs.n=\"KEY_PARTS_VISIBLE_ID\">legacy-source-only</s>",
                "<s xs.n=\"unknown-option\">legacy-source-only</s>",
            ),
            original.replace(
                "<CAttrId xs.n=\"key\" idstr=\"live2DPartsOpacity:legacy-map-only\"/>",
                "<CAttrGuid xs.n=\"key\" uuid=\"live2DPartsOpacity:legacy-map-only\"/>",
            ),
            original.replace(
                "<carray_list xs.n=\"curveTypes\" count=\"1\">",
                "<carray_list xs.n=\"curveTypes\" count=\"0\">",
            ),
            original.replace("live2DPartsOpacity:legacy-map-only", "live2DPartsOpacity:"),
            original.replace(
                "live2DPartsOpacity:legacy-attribute-only",
                "live2DPartsOpacity:",
            ),
        ] {
            assert!(read_xml(&changed).is_err());
        }

        let duplicate_legacy_guid = original
            .replace(
                "live2DPartsOpacity_PartArmB",
                "live2DPartsOpacity:legacy-attribute-b",
            )
            .replace("part-guid-b", "part-guid-a");
        assert!(read_xml(&duplicate_legacy_guid).is_err());
    }

    #[test]
    fn rejects_non_animation_and_ambiguous_main() {
        assert!(read_xml("<root><main><CModelSource/></main></root>").is_err());
        assert!(read_xml("<root><main/><main/></root>").is_err());
        assert!(read_xml("<!DOCTYPE root [<!ENTITY v 'x'>]><root/>").is_err());
    }

    #[test]
    fn xml_document_enforces_the_injected_node_limit_and_rejects_invalid_xml() {
        let at_limit = "<root><child/></root>";
        let over_limit = "<root><child/><extra/></root>";
        assert_eq!(
            xml_document_with_limit(at_limit, 3)
                .unwrap()
                .descendants()
                .count(),
            3
        );
        assert!(xml_document_with_limit(over_limit, 3).is_err());
        assert!(xml_document_with_limit("<root>", 3).is_err());
        assert!(xml_document_with_limit("<!DOCTYPE root [<!ENTITY v 'x'>]><root/>", 3).is_err());
    }

    fn binding_fixture() -> (Value, Value) {
        let animation = json!({
            "format":"live2d.animation.source.v1",
            "resources":[
                {"guid":"res-a","name":"Model","sourceFile":"C:/models/model.cmo3"},
                {"guid":"res-b","name":"Other","sourceFile":"C:/other.cmo3"},
                {"guid":"res-c","name":"Idle","sourceFile":"C:/idle.cmo3"}],
            "scenes":[
                {"guid":"scene-1","tracks":[
                    {"guid":"track-1","resourceGuid":"res-b","parameterCurves":[
                        {"id":"live2dParam_Unknown","parameterId":"Unknown",
                         "parameterGuid":"guid-elsewhere",
                         "sourceParameter":{"type":"unknown","value":"x"},
                         "points":[]}],"partOpacityCurves":[]},
                    {"guid":"track-2","resourceGuid":"res-a","parameterCurves":[
                        {"id":"live2dParam_ParamAngleX","parameterId":"ParamAngleX",
                         "parameterGuid":"guid-x",
                         "sourceParameter":{"type":"CParameterId","value":"ParamAngleX"},
                         "points":[{"anchor":{"frame":0,"value":0.5}}]},
                        {"id":"live2dParam_ParamLegacy","parameterId":"ParamLegacy",
                         "parameterGuid":"guid-legacy",
                         "sourceParameter":{"type":"s","value":"legacy label"},
                         "points":[]},
                        {"id":"live2dParam_ParamRenamed","parameterId":"ParamRenamed",
                         "parameterGuid":"guid-renamed",
                         "sourceParameter":{"type":"CParameterId","value":"ParamOldName"},
                         "points":[]}],"partOpacityCurves":[]}]}]});
        let model = json!({
            "format":"live2d.authoring.source.v1","name":"Model",
            "sources":[],
            "parameters":[
                {"guid":"guid-x","id":"ParamAngleX","name":"Angle X"},
                {"guid":"guid-legacy","id":"ParamLegacy","name":"Legacy"},
                {"guid":"guid-renamed","id":"ParamRenamed","name":"Renamed"}]});
        (animation, model)
    }

    #[test]
    fn bindings_match_exact_guid_and_attribute_suffix_in_source_order() {
        let (animation, model) = binding_fixture();
        let result = bindings(&animation, &model, "res-a").unwrap();
        assert_eq!(result, json!({
            "format":"live2d.animation.bindings.v1",
            "resourceGuid":"res-a",
            "modelName":"Model",
            "bindingBasis":"parameterGuid+attributeId",
            "partCurveCount":0,
            "bindings":[
                {"sceneGuid":"scene-1","trackGuid":"track-2",
                 "curveId":"live2dParam_ParamAngleX","parameterId":"ParamAngleX",
                 "parameterGuid":"guid-x",
                 "sourceParameter":{"type":"CParameterId","value":"ParamAngleX"},
                 "sourceParameterStatus":"matchesId"},
                {"sceneGuid":"scene-1","trackGuid":"track-2",
                 "curveId":"live2dParam_ParamLegacy","parameterId":"ParamLegacy",
                 "parameterGuid":"guid-legacy",
                 "sourceParameter":{"type":"s","value":"legacy label"},
                 "sourceParameterStatus":"legacyText"},
                {"sceneGuid":"scene-1","trackGuid":"track-2",
                 "curveId":"live2dParam_ParamRenamed","parameterId":"ParamRenamed",
                 "parameterGuid":"guid-renamed",
                 "sourceParameter":{"type":"CParameterId","value":"ParamOldName"},
                 "sourceParameterStatus":"differentId"}],
            "partBindings":[],
            "legacyPartBindings":[],
            "unboundLegacyPartCurves":[],"underscorePartBindings":[],"unboundUnderscorePartCurves":[]}));
        let untouched = bindings(&animation, &model, "res-c").unwrap();
        assert_eq!(untouched["bindings"], json!([]));
        assert_eq!(animation, binding_fixture().0);
        assert_eq!(model, binding_fixture().1);
    }

    #[test]
    fn part_bindings_match_known_source_parts_by_guid_and_id() {
        let (mut animation, mut model) = binding_fixture();
        animation["scenes"][0]["tracks"][1]["partOpacityCurves"] = json!([
            {"id":"live2DPartsOpacity_PartArmA", "partId":"PartArmA",
             "partGuid":"part-guid-a", "sourcePart":{"type":"s","value":"PartArmA"}},
            {"id":"live2DPartsOpacity_PartArmB", "partId":"PartArmB",
             "partGuid":"part-guid-b", "sourcePart":{"type":"CPartId","value":"PartArmB"}}
        ]);
        model["sources"] = json!([
            {"guid":"part-guid-a","id":"PartArmA","data":{"type":"Part"}},
            {"guid":"part-guid-b","id":"PartArmB","data":{"type":"Part"}},
            {"guid":"mesh-guid","id":"PartArmA","data":{"type":"ArtMesh"}}
        ]);
        let result = bindings(&animation, &model, "res-a").unwrap();
        assert_eq!(result["partCurveCount"], 2);
        assert_eq!(result["partBindings"], json!([
            {"sceneGuid":"scene-1","trackGuid":"track-2",
             "curveId":"live2DPartsOpacity_PartArmA","partId":"PartArmA",
             "partGuid":"part-guid-a","sourcePart":{"type":"s","value":"PartArmA"}},
            {"sceneGuid":"scene-1","trackGuid":"track-2",
             "curveId":"live2DPartsOpacity_PartArmB","partId":"PartArmB",
             "partGuid":"part-guid-b","sourcePart":{"type":"CPartId","value":"PartArmB"}}
        ]));
        assert!(result.get("bindings").is_some());

        for (field, value) in [
            ("partGuid", json!("part-guid-b")),
            ("partId", json!("PartArmB")),
            ("sourcePart", json!({"type":"s","value":"other"})),
        ] {
            let mut changed = animation.clone();
            changed["scenes"][0]["tracks"][1]["partOpacityCurves"][0][field] = value;
            assert!(bindings(&changed, &model, "res-a").is_err());
        }
        for source in [
            json!({"guid":"part-guid-a","id":"PartOther","data":{"type":"Part"}}),
            json!({"guid":"other-guid","id":"PartArmA","data":{"type":"Part"}}),
        ] {
            let mut changed = model.clone();
            changed["sources"].as_array_mut().unwrap().push(source);
            assert!(bindings(&animation, &changed, "res-a").is_err());
        }
    }

    #[test]
    fn legacy_part_bindings_use_only_exact_guid_and_return_unmatched_curves() {
        let (mut animation, mut model) = binding_fixture();
        animation["scenes"][0]["tracks"][1]["legacyPartOpacityCurves"] = json!([
            {"id":"live2DPartsOpacity:attribute-only",
             "legacyMapPartId":"map-only", "legacyAttributeId":"attribute-only",
             "partGuid":"part-guid-a", "sourcePart":{"type":"s","value":"source-only"}},
            {"id":"live2DPartsOpacity:PartArmA",
             "legacyMapPartId":"PartArmA", "legacyAttributeId":"PartArmA",
             "partGuid":"PART-GUID-A", "sourcePart":{"type":"CPartId","value":"PartArmA"}}
        ]);
        model["sources"] = json!([
            {"guid":"part-guid-a","id":"CanonicalArmA","data":{"type":"Part"}},
            {"guid":"mesh-guid","id":"PartArmA","data":{"type":"ArtMesh"}}
        ]);

        let result = bindings(&animation, &model, "res-a").unwrap();
        assert_eq!(result["bindingBasis"], "parameterGuid+attributeId");
        assert_eq!(result["partCurveCount"], 0);
        assert_eq!(
            result["legacyPartBindings"],
            json!([{
                "sceneGuid":"scene-1", "trackGuid":"track-2",
                "curveId":"live2DPartsOpacity:attribute-only",
                "partId":"CanonicalArmA", "partGuid":"part-guid-a",
                "sourcePart":{"type":"s","value":"source-only"}
            }])
        );
        assert_eq!(
            result["unboundLegacyPartCurves"],
            json!([{
                "sceneGuid":"scene-1", "trackGuid":"track-2",
                "curveId":"live2DPartsOpacity:PartArmA",
                "partGuid":"PART-GUID-A", "reason":"partGuidNotFound"
            }])
        );

        let mut malformed = animation.clone();
        malformed["scenes"][0]["tracks"][1]["legacyPartOpacityCurves"][0]["id"] =
            json!("live2DPartsOpacity:other");
        assert!(bindings(&malformed, &model, "res-a").is_err());
        malformed["scenes"][0]["tracks"][1]["legacyPartOpacityCurves"][0]["partGuid"] = json!("");
        assert!(bindings(&malformed, &model, "res-a").is_err());
    }

    #[test]
    fn binding_identity_mismatches_reject_the_whole_query() {
        let (animation, model) = binding_fixture();
        for mutate in [
            |a: &mut Value| {
                a["scenes"][0]["tracks"][1]["parameterCurves"][0]["parameterGuid"] =
                    json!("missing-guid");
            },
            |a: &mut Value| {
                a["scenes"][0]["tracks"][1]["parameterCurves"][0]["parameterId"] =
                    json!("ParamWrong");
            },
            |a: &mut Value| {
                a["scenes"][0]["tracks"][1]["parameterCurves"][0]["sourceParameter"] =
                    json!({"type":"other","value":"x"});
            },
        ] {
            let mut changed = animation.clone();
            mutate(&mut changed);
            assert!(bindings(&changed, &model, "res-a").is_err());
        }
        for mutate in [
            |m: &mut Value| {
                m["parameters"].as_array_mut().unwrap()
                    .push(json!({"guid":"guid-x","id":"ParamOther"}));
            },
            |m: &mut Value| {
                m["parameters"].as_array_mut().unwrap()
                    .push(json!({"guid":"guid-other","id":"ParamAngleX"}));
            },
        ] {
            let mut changed = model.clone();
            mutate(&mut changed);
            assert!(bindings(&animation, &changed, "res-a").is_err());
        }
        assert!(bindings(&animation, &model, "missing").is_err());
        let mut duplicate = animation.clone();
        duplicate["resources"].as_array_mut().unwrap()
            .push(json!({"guid":"res-a","name":"Duplicate","sourceFile":"x"}));
        assert!(bindings(&duplicate, &model, "res-a").is_err());
        assert!(bindings(&model, &model, "res-a").is_err());
        assert!(bindings(&animation, &animation, "res-a").is_err());
    }

    #[test]
    fn null_parameter_metadata_is_missing_only_after_target_identity_matches() {
        let (mut animation, model) = binding_fixture();
        animation["scenes"][0]["tracks"][1]["parameterCurves"][0]["sourceParameter"] =
            Value::Null;
        let result = bindings(&animation, &model, "res-a").unwrap();
        assert_eq!(result["bindings"][0]["sourceParameter"], Value::Null);
        assert_eq!(result["bindings"][0]["sourceParameterStatus"], "missing");
        assert_eq!(
            result["bindings"][1]["sourceParameter"],
            json!({"type":"s","value":"legacy label"})
        );

        let mut absent = animation.clone();
        absent["scenes"][0]["tracks"][1]["parameterCurves"][0]
            .as_object_mut()
            .unwrap()
            .remove("sourceParameter");
        assert!(bindings(&absent, &model, "res-a").is_err());

        let mut mismatched = animation;
        mismatched["scenes"][0]["tracks"][1]["parameterCurves"][0]["parameterId"] =
            json!("ParamWrong");
        assert!(bindings(&mismatched, &model, "res-a").is_err());

        let mut missing_guid = absent;
        missing_guid["scenes"][0]["tracks"][1]["parameterCurves"][0]["parameterGuid"] =
            json!("missing-guid");
        missing_guid["scenes"][0]["tracks"][1]["parameterCurves"][0]["sourceParameter"] =
            Value::Null;
        assert!(bindings(&missing_guid, &model, "res-a").is_err());
    }
}
