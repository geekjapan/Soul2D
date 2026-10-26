//! Form animation of an INX clip (ANI19-ANI21), after the Cubism Editor manual pages "About Form Animation (FA)",
//! "Create a form animation (FA)", "Adjustment Mode" and "Adjusting the Display Timing of Form Animations (FA)". A
//! form edit moves the vertices of one object on the clip's keyed frames, without parameters, and the motion between
//! keys is interpolated. It changes no model data and is not exported.
//!
//! A clip keeps its form edits as `forms: [{node, mode, keys: [{frame, offsets: [[dx, dy], ...]}]}]`, one entry per
//! node and adjustment mode, with one offset per vertex (mesh vertex of a Part, grid point of a GridDeformer, control
//! point of a PathDeformer). The manual gives no formula, so the reading of the two modes is inferred:
//! - mode 1 adds the offsets to the object's own vertices before its parents deform it, as a deform binding does, so
//!   the edit follows the parent's angle, and glue applies after it;
//! - mode 2 adds them in model coordinates to the drawn vertices, after the whole deformation and glue, so the edit
//!   ignores the parent's angle. Only a drawn Part or Mask takes mode 2.
//!
//! Key frames are form frames: without `formTime` they are the clip's frames.
//!
//! `formTime: {interpolation?, keyframes: [{frame, value}]}` remaps the timing: on a frame of the clip the forms show
//! the form frame `value` the curve gives there (the manual's time remapping). Before the first key and after the last
//! a form holds that key's offsets, and between keys every offset moves linearly (inferred).

use crate::scene_deform::SceneDeform;
use crate::{animation, nodes, required_string, Session};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const MAX_FORMS: usize = 256;
const MAX_OFFSETS: usize = crate::mesh_edit::MAX_VERTICES;

#[derive(Clone, Debug, PartialEq)]
struct Form {
    node: u32,
    mode: u8,
    keys: Vec<(i32, Vec<[f64; 2]>)>,
}

/// The offsets the forms of a clip add on one frame: `local` before the deformation (mode 1), `world` after it in
/// model coordinates (mode 2).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Sample {
    pub(crate) local: BTreeMap<u32, Vec<[f64; 2]>>,
    pub(crate) world: BTreeMap<u32, Vec<[f64; 2]>>,
}

impl Sample {
    /// Adds the mode 1 offsets to the deformations a scene is evaluated with.
    pub(crate) fn add_local(&self, deformations: &mut BTreeMap<u32, Vec<[f64; 2]>>) {
        for (id, offsets) in &self.local {
            let summed = match deformations.get(id) {
                Some(base) if base.len() == offsets.len() => {
                    base.iter().zip(offsets).map(|(a, b)| [a[0] + b[0], a[1] + b[1]]).collect()
                }
                _ => offsets.clone(),
            };
            deformations.insert(*id, summed);
        }
    }

    /// Hands the mode 2 offsets to an evaluated scene, whose drawing adds them after glue.
    pub(crate) fn add_world(&self, scene: &mut SceneDeform) {
        scene.world_offsets.clone_from(&self.world);
    }
}

fn pair(value: &Value) -> Option<[f64; 2]> {
    let pair = value.as_array().filter(|pair| pair.len() == 2)?;
    let x = pair[0].as_f64().filter(|x| x.is_finite())?;
    Some([x, pair[1].as_f64().filter(|y| y.is_finite())?])
}

// ponytail: drawing a frame parses every stored key again; keep the parsed forms on the preview if long clips with
// big meshes play slowly.
fn offsets(value: &Value, path: &str) -> Result<Vec<[f64; 2]>, String> {
    let list = value.as_array().ok_or_else(|| format!("{path} must be an array"))?;
    if list.len() > MAX_OFFSETS {
        return Err(format!("{path} holds more than {MAX_OFFSETS} offsets"));
    }
    let parsed = list.iter().enumerate().map(|(index, item)| pair(item).ok_or(index));
    parsed.collect::<Result<_, _>>().map_err(|index| format!("{path}[{index}] must be [x, y] of two finite numbers"))
}

fn mode(value: Option<&Value>, path: &str) -> Result<u8, String> {
    match value {
        None | Some(Value::Null) => Ok(1),
        Some(value) => match value.as_u64() {
            Some(1) => Ok(1),
            Some(2) => Ok(2),
            _ => Err(format!("{path} must be 1 or 2")),
        },
    }
}

/// The stored form edits of a clip, checked: an opened file is external input.
fn forms(animation: &Value, length: usize) -> Result<Vec<Form>, String> {
    let list = match animation.get("forms") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(list) => list.as_array().ok_or("animation.forms must be an array")?,
    };
    if list.len() > MAX_FORMS {
        return Err(format!("an animation holds at most {MAX_FORMS} form edits"));
    }
    let mut out: Vec<Form> = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let path = format!("animation.forms[{index}]");
        let node = item["node"].as_u64().and_then(crate::valid_id).ok_or_else(|| format!("{path}.node must be a node UUID"))?;
        let mode = mode(item.get("mode"), &format!("{path}.mode"))?;
        if out.iter().any(|form| form.node == node && form.mode == mode) {
            return Err(format!("{path} repeats the form edit of node {node} in mode {mode}"));
        }
        let keys = item["keys"].as_array().filter(|keys| !keys.is_empty());
        let keys = keys.ok_or_else(|| format!("{path}.keys must be a nonempty array"))?;
        let mut parsed: Vec<(i32, Vec<[f64; 2]>)> = Vec::with_capacity(keys.len());
        for (k, key) in keys.iter().enumerate() {
            let key_path = format!("{path}.keys[{k}]");
            let frame = key["frame"].as_i64().and_then(|frame| i32::try_from(frame).ok());
            let frame = frame.filter(|frame| *frame >= 0 && (*frame as usize) < length);
            let frame = frame.ok_or_else(|| format!("{key_path}.frame must be a frame inside the animation"))?;
            if parsed.last().is_some_and(|(last, _)| *last >= frame) {
                return Err(format!("{path}.keys must be sorted by frame, each frame once"));
            }
            let offsets = offsets(&key["offsets"], &format!("{key_path}.offsets"))?;
            if parsed.first().is_some_and(|(_, first)| first.len() != offsets.len()) {
                return Err(format!("{path}.keys must all hold the same number of offsets"));
            }
            parsed.push((frame, offsets));
        }
        out.push(Form { node, mode, keys: parsed });
    }
    Ok(out)
}

pub(crate) fn validate(animation: &Value, length: usize) -> Result<(), String> {
    forms(animation, length)?;
    form_frame(animation, length, 0.0).map(|_| ())
}

/// The form frame shown on `frame` of the clip: the `formTime` curve, or the frame itself.
fn form_frame(animation: &Value, length: usize, frame: f64) -> Result<f64, String> {
    let frame = crate::frame_step::frame(animation, length, frame)?;
    match animation.get("formTime") {
        None | Some(Value::Null) => Ok(frame),
        Some(track) => {
            let sampled = animation::sample_track(track, length, frame as f32, "animation.formTime")?;
            // The curve is sampled in f32, whose error grows with the values it blends, so a frame that maps onto a
            // whole form frame can come back a little off it.
            // ponytail: a form frame this near a whole one is taken as whole; sample in f64 if that ever shows.
            let keys = track["keyframes"].as_array().into_iter().flatten();
            let span = keys.filter_map(|key| key["value"].as_f64()).fold(1.0_f64, |span, value| span.max(value.abs()));
            let near = 16.0 * f64::from(f32::EPSILON) * span;
            Ok(sampled.map_or(frame, |at| {
                let at = f64::from(at);
                if (at - at.round()).abs() <= near { at.round() } else { at }
            }))
        }
    }
}

fn form_json(form: &Form) -> Value {
    let keys: Vec<Value> = form.keys.iter().map(|(frame, offsets)| json!({"frame": frame, "offsets": offsets})).collect();
    json!({"node": form.node, "mode": form.mode, "keys": keys})
}

fn store(model: &mut Value, name: &str, list: &[Form]) -> Result<(), String> {
    let animation = animation::animation_mut(model, name)?;
    let object = animation.as_object_mut().ok_or("animation must be an object")?;
    if list.is_empty() {
        object.remove("forms");
    } else {
        object.insert("forms".into(), Value::Array(list.iter().map(form_json).collect()));
    }
    Ok(())
}

/// The number of vertices a form edit of `node` moves, counted as the deformation counts them, or why it has none.
fn vertex_count(node: &Value, id: u32) -> Result<usize, String> {
    let kind = node["type"].as_str().unwrap_or_default();
    let none = || format!("a {kind} node has no vertices to edit");
    if !matches!(kind, "Part" | "Mask" | "GridDeformer" | "PathDeformer") {
        return Err(none());
    }
    let count = crate::scene_deform::geometry_points(node, id)?.len();
    if count == 0 {
        return Err(none());
    }
    Ok(count)
}

/// Why a form edit cannot be drawn, if it cannot.
fn broken(model: &Value, form: &Form) -> Option<String> {
    let Some(node) = nodes::find_node(model, form.node) else {
        return Some(format!("node {} was not found", form.node));
    };
    if form.mode == 2 && !matches!(node["type"].as_str(), Some("Part" | "Mask")) {
        return Some("Adjustment Mode 2 moves a drawn Part or Mask only".into());
    }
    // ponytail: a composite draws its children into a surface sized and warped before glue, so a mode 2 edit there
    // would be cut off and warped; drawing it would need the glued points in the composite's bounds.
    if form.mode == 2 && inside_composite(&model["nodes"], form.node, false) == Some(true) {
        return Some("Adjustment Mode 2 does not move a Part inside a composite".into());
    }
    let count = match vertex_count(node, form.node) {
        Ok(count) => count,
        Err(error) => return Some(error),
    };
    let held = form.keys[0].1.len();
    (held != count).then(|| format!("node {} has {count} vertices and the form edit {held}", form.node))
}

/// Whether node `id` sits below a Composite or DynamicComposite, if it is found.
fn inside_composite(node: &Value, id: u32, inside: bool) -> Option<bool> {
    if node["uuid"].as_u64() == Some(u64::from(id)) {
        return Some(inside);
    }
    let here = inside || matches!(node["type"].as_str(), Some("Composite" | "DynamicComposite"));
    node["children"].as_array()?.iter().find_map(|child| inside_composite(child, id, here))
}

fn interpolate(keys: &[(i32, Vec<[f64; 2]>)], at: f64) -> Vec<[f64; 2]> {
    let next = keys.iter().position(|(frame, _)| f64::from(*frame) >= at);
    match next {
        None => keys[keys.len() - 1].1.clone(),
        Some(0) => keys[0].1.clone(),
        Some(index) => {
            let ((f0, a), (f1, b)) = (&keys[index - 1], &keys[index]);
            let t = (at - f64::from(*f0)) / f64::from(f1 - f0);
            a.iter().zip(b).map(|(a, b)| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]).collect()
        }
    }
}

/// The form offsets of clip `name` on `frame`. A broken form edit is left out.
pub(crate) fn sample(model: &Value, name: &str, frame: f64) -> Result<Sample, String> {
    let animation = &model["animations"][name];
    let length = animation::validate_animation_header(animation)?;
    let list = forms(animation, length)?;
    let mut out = Sample::default();
    if list.is_empty() {
        return Ok(out);
    }
    let at = form_frame(animation, length, frame)?;
    for form in list.iter().filter(|form| broken(model, form).is_none()) {
        let target = if form.mode == 2 { &mut out.world } else { &mut out.local };
        target.insert(form.node, interpolate(&form.keys, at));
    }
    Ok(out)
}

impl Session {
    fn form_clip(&self, op: &str, request: &Value) -> Result<(String, Vec<Form>, usize), String> {
        self.require_inx(op)?;
        let name = required_string(request, "name")?;
        let animation = self.document.model["animations"].get(name);
        let animation = animation.ok_or_else(|| format!("animation {name:?} was not found"))?;
        let length = animation::validate_animation_header(animation)?;
        Ok((name.to_owned(), forms(animation, length)?, length))
    }

    fn commit_forms(&mut self, name: &str, list: &[Form]) -> Result<(), String> {
        let mut candidate = self.document.model.clone();
        store(&mut candidate, name, list)?;
        self.commit_model(candidate)
    }

    /// `animation.form.key {name, node, mode?, frame, offsets? | reset? | delta?, vertices?, space?}`: keys the form
    /// edit of `node` (mode 1 by default) on `frame`, adding the form edit if the node has none in that mode. The key
    /// holds `offsets`, or zeros with `reset` (the manual's Reset Form Edit), or else the offsets the form edit shows on
    /// that frame, moved by `delta` [dx, dy] at the listed `vertices` (all by default). With `space` "model" the delta
    /// is how far the vertices move in the picture the session shows, as a drag in the viewport gives it. One undo
    /// step.
    pub(crate) fn op_form_key(&mut self, request: &Value) -> Result<Value, String> {
        let (name, mut list, length) = self.form_clip("animation.form.key", request)?;
        let node_id = crate::required_id(request, "node")?;
        let mode = mode(request.get("mode"), "mode")?;
        let frame = animation::frame_field(request, "frame", length)?;
        let node = nodes::find_node(&self.document.model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
        let count = vertex_count(node, node_id)?;
        let index = list.iter().position(|form| form.node == node_id && form.mode == mode);
        let probe = Form { node: node_id, mode, keys: vec![(frame, vec![[0.0, 0.0]; count])] };
        if let Some(error) = broken(&self.document.model, index.map_or(&probe, |index| &list[index])) {
            return Err(format!("the form edit of node {node_id} cannot be keyed: {error}"));
        }
        let reset = match request.get("reset") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or("reset must be a boolean")?,
        };
        let mut keyed = match (request.get("offsets").filter(|value| !value.is_null()), reset) {
            (Some(_), true) => return Err("give offsets or reset, not both".into()),
            (Some(value), false) => {
                let keyed = offsets(value, "offsets")?;
                if keyed.len() != count {
                    return Err(format!("offsets must hold {count} pairs, one per vertex of node {node_id}"));
                }
                keyed
            }
            (None, true) => vec![[0.0, 0.0]; count],
            (None, false) => index.map_or_else(|| vec![[0.0, 0.0]; count], |index| interpolate(&list[index].keys, f64::from(frame))),
        };
        let given = |key: &str| request.get(key).is_some_and(|value| !value.is_null());
        if given("delta") && (given("offsets") || reset) {
            return Err("delta moves what the frame shows; give it without offsets or reset".into());
        }
        if given("vertices") && !given("delta") {
            return Err("vertices picks the vertices delta moves; give delta too".into());
        }
        if let Some(delta) = request.get("delta").filter(|value| !value.is_null()) {
            let delta = pair(delta).ok_or("delta must be [x, y] of two finite numbers")?;
            let mut picked: Vec<usize> = match request.get("vertices").filter(|value| !value.is_null()) {
                None => (0..count).collect(),
                Some(list) => {
                    let list = list.as_array().ok_or("vertices must be an array")?;
                    let picked = list.iter().map(|index| index.as_u64().map(|index| index as usize).filter(|index| *index < count));
                    picked.collect::<Option<_>>().ok_or_else(|| format!("vertices must be indices below {count}"))?
                }
            };
            picked.sort_unstable();
            picked.dedup();
            let moved = match request.get("space").filter(|value| !value.is_null()).map(Value::as_str) {
                None | Some(Some("form")) => vec![delta; picked.len()],
                Some(Some("model")) if mode == 2 => vec![delta; picked.len()],
                Some(Some("model")) => self.model_delta(&name, frame, node_id, count, &picked, delta)?,
                Some(_) => return Err("space must be \"form\" or \"model\"".into()),
            };
            for (index, moved) in picked.into_iter().zip(moved) {
                keyed[index] = [keyed[index][0] + moved[0], keyed[index][1] + moved[1]];
            }
        } else if given("space") {
            return Err("space says how delta is measured; give delta too".into());
        }
        let index = match index {
            Some(index) => index,
            None if list.len() >= MAX_FORMS => return Err(format!("an animation holds at most {MAX_FORMS} form edits")),
            None => {
                list.push(Form { node: node_id, mode, keys: Vec::new() });
                list.len() - 1
            }
        };
        let keys = &mut list[index].keys;
        match keys.binary_search_by_key(&frame, |(at, _)| *at) {
            Ok(at) => keys[at].1 = keyed,
            Err(at) => keys.insert(at, (frame, keyed)),
        }
        let count = keys.len();
        self.commit_forms(&name, &list)?;
        Ok(json!({"node": node_id, "mode": mode, "frame": frame, "keys": count}))
    }

    /// The mode 1 offsets that move the `picked` vertices of `node` by `delta` in the picture the session shows of clip
    /// `name` on form frame `frame`, measured by moving those vertices one unit along each axis of their own offsets.
    // ponytail: one finite step, exact under parents that move, turn and scale; a deformer that bends the mesh gets a
    // first-order answer, so a long drag through a bend lands near the pointer, not on it. The picked vertices are
    // moved together, so glue between them is in the measure too; probe each on its own if that ever matters.
    fn model_delta(&mut self, name: &str, frame: i32, node: u32, count: usize, picked: &[usize], delta: [f64; 2])
        -> Result<Vec<[f64; 2]>, String> {
        let shown = match self.animation_preview.as_ref().map(|preview| &preview.timeline) {
            Some(crate::PreviewTimeline::Animation { name: shown, frame, .. }) if shown == name => *frame,
            _ => return Err(format!("seek animation {name:?} first; a model-space delta is measured in the picture shown")),
        };
        let animation = &self.document.model["animations"][name];
        let length = animation::validate_animation_header(animation)?;
        if form_frame(animation, length, shown)? != f64::from(frame) {
            return Err(format!("seek a frame that shows form frame {frame}; a model-space delta is measured there"));
        }
        // ponytail: a composite draws its children into a surface of its own, which this measure has not been checked
        // against, so their model-space deltas are refused.
        if inside_composite(&self.document.model["nodes"], node, false) == Some(true) {
            return Err(format!("node {node} is inside a composite; give delta in form space"));
        }
        let drawn = |session: &mut Session| -> Result<Vec<[f64; 2]>, String> {
            let geometry = session.op_scene_geometry(&json!({}))?;
            let part = geometry["parts"].as_array().into_iter().flatten().find(|part| part["id"] == node);
            let part = part.ok_or_else(|| format!("node {node} is not drawn on its own; give delta in form space"))?;
            let vertices = part["vertices"].as_array().into_iter().flatten().map(pair);
            vertices.collect::<Option<Vec<_>>>().filter(|list| list.len() == count)
                .ok_or_else(|| format!("node {node} is not drawn with its {count} vertices"))
        };
        let base = drawn(self)?;
        let mut axes = Vec::new();
        for step in [[1.0, 0.0], [0.0, 1.0]] {
            let forms = &mut self.animation_preview.as_mut().ok_or("no animation is shown")?.forms;
            let saved = forms.local.get(&node).cloned();
            let probe = forms.local.entry(node).or_insert_with(|| vec![[0.0, 0.0]; count]);
            for index in picked {
                probe[*index] = [probe[*index][0] + step[0], probe[*index][1] + step[1]];
            }
            let probed = drawn(self);
            let forms = &mut self.animation_preview.as_mut().ok_or("no animation is shown")?.forms;
            match saved {
                Some(saved) => forms.local.insert(node, saved),
                None => forms.local.remove(&node),
            };
            axes.push(probed?);
        }
        picked.iter().map(|index| {
            let a = [axes[0][*index][0] - base[*index][0], axes[0][*index][1] - base[*index][1]];
            let b = [axes[1][*index][0] - base[*index][0], axes[1][*index][1] - base[*index][1]];
            let det = a[0] * b[1] - b[0] * a[1];
            if det.abs() <= 1e-9 * a[0].hypot(a[1]) * b[0].hypot(b[1]) {
                return Err(format!("vertex {index} of node {node} does not follow its offset in the picture"));
            }
            Ok([(b[1] * delta[0] - b[0] * delta[1]) / det, (a[0] * delta[1] - a[1] * delta[0]) / det])
        }).collect()
    }

    /// `animation.form.delete {name, node?, mode?, frame?}`: deletes the form edits of `node` (in `mode` only, if
    /// given), or every form edit of the clip without `node`; with `frame`, only their key on that frame. A form edit
    /// left without keys goes. One undo step.
    pub(crate) fn op_form_delete(&mut self, request: &Value) -> Result<Value, String> {
        let (name, mut list, length) = self.form_clip("animation.form.delete", request)?;
        let node = match request.get("node") {
            None | Some(Value::Null) => None,
            Some(_) => Some(crate::required_id(request, "node")?),
        };
        let mode = match request.get("mode") {
            None | Some(Value::Null) => None,
            value => Some(mode(value, "mode")?),
        };
        let frame = match request.get("frame") {
            None | Some(Value::Null) => None,
            Some(_) => Some(animation::frame_field(request, "frame", length)?),
        };
        let picked = |form: &Form| node.is_none_or(|node| form.node == node) && mode.is_none_or(|mode| form.mode == mode);
        let before: usize = list.iter().filter(|form| picked(form)).map(|form| form.keys.len()).sum();
        for form in list.iter_mut().filter(|form| picked(form)) {
            form.keys.retain(|(at, _)| frame.is_some_and(|frame| *at != frame));
        }
        list.retain(|form| !form.keys.is_empty());
        let after: usize = list.iter().filter(|form| picked(form)).map(|form| form.keys.len()).sum();
        if before == after {
            return Err(match frame {
                Some(frame) => format!("animation {name:?} has no form key on frame {frame} to delete"),
                None => format!("animation {name:?} has no form edit to delete"),
            });
        }
        self.commit_forms(&name, &list)?;
        Ok(json!({"deleted": before - after}))
    }

    /// `animation.form.time {name, keyframes, interpolation?}`: sets the clip's form timing curve, each key mapping a
    /// frame of the clip to the form frame shown there; null or no keys clears it. One undo step.
    pub(crate) fn op_form_time(&mut self, request: &Value) -> Result<Value, String> {
        let (name, _, length) = self.form_clip("animation.form.time", request)?;
        let mut keys: Vec<Value> = match request.get("keyframes") {
            None => return Err("keyframes not provided; give null to clear the timing".into()),
            Some(Value::Null) => Vec::new(),
            Some(list) => list.as_array().ok_or("keyframes must be an array")?.clone(),
        };
        keys.sort_by_key(|key| key["frame"].as_i64());
        let mut candidate = self.document.model.clone();
        let object = animation::animation_mut(&mut candidate, &name)?.as_object_mut().ok_or("animation must be an object")?;
        if keys.is_empty() {
            object.remove("formTime");
        } else {
            let mut track = Map::new();
            if let Some(interpolation) = request.get("interpolation").filter(|value| !value.is_null()) {
                track.insert("interpolation".into(), interpolation.clone());
            }
            track.insert("keyframes".into(), Value::Array(keys));
            let track = Value::Object(track);
            animation::sample_track(&track, length, 0.0, "formTime")?;
            object.insert("formTime".into(), track);
        }
        self.commit_model(candidate)?;
        Ok(json!({"name": name}))
    }

    /// `animation.form.read {name, frame}`: the form edits of a clip and the offsets each shows on `frame`.
    pub(crate) fn op_form_read(&mut self, request: &Value) -> Result<Value, String> {
        let (name, list, length) = self.form_clip("animation.form.read", request)?;
        let frame = animation::frame_field(request, "frame", length)?;
        let at = form_frame(&self.document.model["animations"][&name], length, f64::from(frame))?;
        let model = &self.document.model;
        let read: Vec<Value> = list
            .iter()
            .map(|form| {
                let frames: Vec<i32> = form.keys.iter().map(|(frame, _)| *frame).collect();
                let mut out = json!({"node": form.node, "mode": form.mode, "keys": frames});
                match broken(model, form) {
                    Some(error) => out["broken"] = json!(error),
                    None => out["offsets"] = json!(interpolate(&form.keys, at)),
                }
                out
            })
            .collect();
        Ok(json!({"name": name, "frame": frame, "formFrame": at, "forms": read}))
    }
}
