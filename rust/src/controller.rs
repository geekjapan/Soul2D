//! Parameter controllers of an INX animation (ANI18), after the Cubism Editor manual pages "About the parameter
//! controller" and "Controller target settings". A controller is a point pinned to a Part's mesh (single-point), or
//! a direction from one such point to another, with the parameters that move it. Moving it solves those parameters so
//! that the point reaches the place it is moved to, and keys them. A controller with a target follows the target,
//! and baking turns that into keys. The manual gives no formula: the solve is a damped Gauss-Newton search over the
//! listed parameters, inside their ranges (inferred).
//!
//! A clip keeps its controllers as `controllers: [{name, start, end?, parameters, target?, extend?, weight?}]`, where
//! `start` and `end` are `{node, vertices: [i, j, k], weights: [a, b, c]}`: a point of one triangle of the Part's mesh.

use crate::document::TextureBlob;
use crate::{animation, required_string, Session};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

const MAX_CONTROLLERS: usize = 64;
const MAX_UNKNOWNS: usize = 16;
const MAX_BAKE_FRAMES: i64 = 2000;
const ITERATIONS: usize = 40;
/// A position closer than this, in world units, or an angle closer than this, in radians, is reached.
const REACHED: f64 = 1e-3;

/// A point of a triangle of a Part's mesh, by barycentric weights, so that it moves with the mesh.
#[derive(Clone, Debug, PartialEq)]
struct Anchor {
    node: u32,
    vertices: [usize; 3],
    weights: [f64; 3],
}

#[derive(Clone, Debug)]
struct Controller {
    name: String,
    start: Anchor,
    end: Option<Anchor>,
    parameters: Vec<u32>,
    target: Option<String>,
    extend: bool,
    weight: f64,
}

/// A drawn Part: its world vertices and triangles, in painter order.
struct Drawn {
    id: u32,
    vertices: Vec<[f64; 2]>,
    indices: Vec<usize>,
}

fn anchor(value: &Value, path: &str) -> Result<Anchor, String> {
    let invalid = || format!("{path} needs a node, three vertex indices and three weights");
    let node = value["node"].as_u64().and_then(crate::valid_id).ok_or_else(invalid)?;
    let numbers = |key: &str| value[key].as_array().filter(|list| list.len() == 3).cloned().ok_or_else(invalid);
    let mut vertices = [0; 3];
    let mut weights = [0.0; 3];
    for (index, vertex) in numbers("vertices")?.iter().enumerate() {
        vertices[index] = vertex.as_u64().and_then(|vertex| usize::try_from(vertex).ok()).ok_or_else(invalid)?;
    }
    for (index, weight) in numbers("weights")?.iter().enumerate() {
        weights[index] = weight.as_f64().ok_or_else(invalid)?;
    }
    Ok(Anchor { node, vertices, weights })
}

fn anchor_json(anchor: &Anchor) -> Value {
    json!({"node": anchor.node, "vertices": anchor.vertices, "weights": anchor.weights})
}

/// The controllers of a clip, checked. A clip without the field, or with null, has none.
fn controllers(animation: &Value) -> Result<Vec<Controller>, String> {
    let list = match animation.get("controllers") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(list) => list.as_array().ok_or("animation.controllers must be an array")?,
    };
    if list.len() > MAX_CONTROLLERS {
        return Err(format!("an animation holds at most {MAX_CONTROLLERS} controllers"));
    }
    let mut out: Vec<Controller> = Vec::new();
    for (index, item) in list.iter().enumerate() {
        let path = format!("animation.controllers[{index}]");
        let name = item["name"].as_str().filter(|name| !name.trim().is_empty());
        let name = name.ok_or_else(|| format!("{path}.name must be a non-empty string"))?;
        if out.iter().any(|other| other.name == name) {
            return Err(format!("{path} repeats the controller name {name:?}"));
        }
        let end = match item.get("end") {
            None | Some(Value::Null) => None,
            Some(end) => Some(anchor(end, &format!("{path}.end"))?),
        };
        let ids = item["parameters"].as_array().ok_or_else(|| format!("{path}.parameters must be an array"))?;
        let parameters = ids
            .iter()
            .map(|id| id.as_u64().and_then(crate::valid_id))
            .collect::<Option<Vec<u32>>>()
            .ok_or_else(|| format!("{path}.parameters must hold parameter UUIDs"))?;
        let target = match item.get("target") {
            None | Some(Value::Null) => None,
            Some(target) => Some(target.as_str().filter(|target| *target != name).map(str::to_owned)
                .ok_or_else(|| format!("{path}.target must name another controller"))?),
        };
        let extend = match item.get("extend") {
            None => true,
            Some(extend) => extend.as_bool().ok_or_else(|| format!("{path}.extend must be a boolean"))?,
        };
        let weight = match item.get("weight") {
            None => 100.0,
            Some(weight) => weight.as_f64().filter(|weight| (0.0..=100.0).contains(weight))
                .ok_or_else(|| format!("{path}.weight must be a number from 0 to 100"))?,
        };
        let start = anchor(&item["start"], &format!("{path}.start"))?;
        out.push(Controller { name: name.to_owned(), start, end, parameters, target, extend, weight });
    }
    Ok(out)
}

fn controller_json(controller: &Controller) -> Value {
    let mut out = json!({
        "name": controller.name, "start": anchor_json(&controller.start), "parameters": controller.parameters,
    });
    if let Some(end) = &controller.end {
        out["end"] = anchor_json(end);
    }
    if let Some(target) = &controller.target {
        out["target"] = json!(target);
    }
    if !controller.extend {
        out["extend"] = json!(false);
    }
    if controller.weight != 100.0 {
        out["weight"] = json!(controller.weight);
    }
    out
}

/// Checks the `controllers` field an `animation.set` would store.
pub(crate) fn validate(animation: &Value) -> Result<(), String> {
    controllers(animation).map(|_| ())
}

/// The drawn Parts for `values`, as a frame of the animation draws them (without physics).
fn draw(model: &Value, values: &BTreeMap<u32, [f64; 2]>, textures: &[TextureBlob]) -> Result<Vec<Drawn>, String> {
    let evaluated = crate::evaluation::evaluate(model, values)?;
    let mut deformations = crate::evaluation::raster_deformations(model, values)?;
    let mut pose = BTreeMap::new();
    for parameter in values.keys() {
        pose.extend(crate::dirty::preview_pose(model, values, *parameter)?);
    }
    crate::evaluation::overlay_preview(&mut deformations, &pose);
    let nodes = model.get("nodes").ok_or("model.nodes is missing")?;
    let scene = crate::scene_deform::evaluate_with_path_update(&evaluated, nodes, &deformations, &mut |_, _, _, _, _| {
        Ok(None)
    })?;
    let geometry = crate::render::scene_geometry_for_scene(&evaluated, &scene, textures)?;
    let parts = geometry["parts"].as_array().into_iter().flatten();
    Ok(parts
        .filter_map(|part| {
            let id = part["id"].as_u64().and_then(|id| u32::try_from(id).ok())?;
            let vertices = part["vertices"].as_array()?.iter();
            let vertices = vertices.map(|vertex| Some([vertex[0].as_f64()?, vertex[1].as_f64()?]));
            let indices = part["indices"].as_array()?.iter().map(|index| index.as_u64().map(|index| index as usize));
            Some(Drawn { id, vertices: vertices.collect::<Option<_>>()?, indices: indices.collect::<Option<_>>()? })
        })
        .collect())
}

/// The anchor of the frontmost drawn Part whose mesh covers `point`.
fn pin(drawn: &[Drawn], point: [f64; 2]) -> Option<Anchor> {
    for part in drawn.iter().rev() {
        for triangle in part.indices.chunks_exact(3) {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
            let (Some(pa), Some(pb), Some(pc)) = (part.vertices.get(a), part.vertices.get(b), part.vertices.get(c)) else {
                continue;
            };
            let det = (pb[1] - pc[1]) * (pa[0] - pc[0]) + (pc[0] - pb[0]) * (pa[1] - pc[1]);
            if det.abs() < 1e-12 {
                continue;
            }
            let u = ((pb[1] - pc[1]) * (point[0] - pc[0]) + (pc[0] - pb[0]) * (point[1] - pc[1])) / det;
            let v = ((pc[1] - pa[1]) * (point[0] - pc[0]) + (pa[0] - pc[0]) * (point[1] - pc[1])) / det;
            let w = 1.0 - u - v;
            if [u, v, w].iter().all(|weight| *weight >= -1e-9) {
                return Some(Anchor { node: part.id, vertices: [a, b, c], weights: [u, v, w] });
            }
        }
    }
    None
}

/// Where the anchor is, or why it cannot be found: its Part is not drawn or no longer has the vertex.
fn place(drawn: &[Drawn], anchor: &Anchor) -> Result<[f64; 2], String> {
    let part = drawn.iter().find(|part| part.id == anchor.node);
    let part = part.ok_or_else(|| format!("node {} draws no mesh", anchor.node))?;
    let mut point = [0.0; 2];
    for (vertex, weight) in anchor.vertices.iter().zip(anchor.weights) {
        let at = part.vertices.get(*vertex).ok_or_else(|| format!("node {} has no vertex {vertex}", anchor.node))?;
        point = [point[0] + at[0] * weight, point[1] + at[1] * weight];
    }
    Ok(point)
}

/// The start and, for a direction controller, the end of a controller.
fn ends(drawn: &[Drawn], controller: &Controller) -> Result<([f64; 2], Option<[f64; 2]>), String> {
    Ok((place(drawn, &controller.start)?, controller.end.as_ref().map(|end| place(drawn, end)).transpose()?))
}

fn angle(from: [f64; 2], to: [f64; 2]) -> f64 {
    (to[1] - from[1]).atan2(to[0] - from[0])
}

/// The angle from `b` to `a`, between -pi and pi.
fn turn(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(std::f64::consts::TAU);
    if d > std::f64::consts::PI { d - std::f64::consts::TAU } else { d }
}

/// The point of the line through `a` and `b` nearest to `p`; of the segment only, without `extend`.
fn nearest(a: [f64; 2], b: [f64; 2], p: [f64; 2], extend: bool) -> [f64; 2] {
    let d = [b[0] - a[0], b[1] - a[1]];
    let length = d[0] * d[0] + d[1] * d[1];
    if length < 1e-24 {
        return a;
    }
    let mut t = ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / length;
    if !extend {
        t = t.clamp(0.0, 1.0);
    }
    [a[0] + t * d[0], a[1] + t * d[1]]
}

/// What a controller is moved toward.
enum Goal<'a> {
    /// A point dragged to: a single-point controller reaches it, a direction controller turns its end toward it.
    Point([f64; 2]),
    /// Another controller, followed as the manual's target settings describe.
    Follow(&'a Controller),
}

/// How far `controller` is from `goal` in `drawn`: a position (two numbers) or an angle (one).
fn residual(drawn: &[Drawn], controller: &Controller, goal: &Goal) -> Result<Vec<f64>, String> {
    let (start, end) = ends(drawn, controller)?;
    let gap = |a: [f64; 2], b: [f64; 2]| vec![a[0] - b[0], a[1] - b[1]];
    Ok(match (goal, end) {
        (Goal::Point(point), None) => gap(start, *point),
        (Goal::Point(point), Some(end)) => vec![turn(angle(start, end), angle(start, *point))],
        (Goal::Follow(target), end) => {
            let (target_start, target_end) = ends(drawn, target)?;
            match (end, target_end) {
                (None, None) => gap(start, target_start),
                // A point comes to the nearest point of the target's line, or of its two ends without extend.
                (None, Some(target_end)) => gap(start, nearest(target_start, target_end, start, controller.extend)),
                // A direction turns so that its line passes through the target point.
                (Some(end), None) => gap(target_start, nearest(start, end, target_start, controller.extend)),
                (Some(end), Some(target_end)) => {
                    vec![turn(angle(start, end), angle(target_start, target_end))]
                }
            }
        }
    })
}

/// One unknown of the solve: a parameter axis and its range.
struct Unknown {
    parameter: u32,
    axis: usize,
    min: f64,
    max: f64,
}

fn unknowns(model: &Value, parameters: &[u32]) -> Result<Vec<Unknown>, String> {
    let mut out = Vec::new();
    for id in parameters {
        let parameter = crate::find_parameter(model, *id).ok_or_else(|| format!("parameter {id} was not found"))?;
        let axes = if parameter["is_vec2"] == true { 2 } else { 1 };
        for axis in 0..axes {
            let bound = |key: &str| parameter[key][axis].as_f64().ok_or_else(|| format!("parameter {id} has no {key}"));
            out.push(Unknown { parameter: *id, axis, min: bound("min")?, max: bound("max")? });
        }
    }
    if out.len() > MAX_UNKNOWNS {
        return Err(format!("a controller moves at most {MAX_UNKNOWNS} parameter axes; list fewer parameters"));
    }
    Ok(out)
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting. A singular system answers None.
fn linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for column in 0..n {
        let pivot = (column..n).max_by(|i, j| a[*i][column].abs().total_cmp(&a[*j][column].abs()))?;
        if a[pivot][column].abs() < 1e-15 {
            return None;
        }
        a.swap(column, pivot);
        b.swap(column, pivot);
        for row in column + 1..n {
            let factor = a[row][column] / a[column][column];
            for k in column..n {
                a[row][k] -= factor * a[column][k];
            }
            b[row] -= factor * b[column];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let sum: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - sum) / a[row][row];
    }
    Some(x)
}

/// Damped Gauss-Newton from `start` inside the bounds of `unknowns`: the values that bring `residual` nearest to
/// zero, and how far from zero they leave it.
fn solve(
    unknowns: &[Unknown],
    start: Vec<f64>,
    residual: &mut dyn FnMut(&[f64]) -> Result<Vec<f64>, String>,
) -> Result<(Vec<f64>, f64), String> {
    let norm = |r: &[f64]| r.iter().map(|value| value * value).sum::<f64>();
    let clamp = |x: &mut Vec<f64>| {
        for (value, unknown) in x.iter_mut().zip(unknowns) {
            *value = value.clamp(unknown.min, unknown.max);
        }
    };
    let mut x = start;
    clamp(&mut x);
    let mut r = residual(&x)?;
    let mut cost = norm(&r);
    let mut damping = 1e-3;
    for _ in 0..ITERATIONS {
        if cost.sqrt() < REACHED || unknowns.is_empty() {
            break;
        }
        let mut jacobian = vec![vec![0.0; x.len()]; r.len()];
        for (column, unknown) in unknowns.iter().enumerate() {
            let mut h = (unknown.max - unknown.min).abs().max(1e-6) * 1e-4;
            if x[column] + h > unknown.max {
                h = -h;
            }
            let mut moved = x.clone();
            moved[column] += h;
            let shifted = residual(&moved)?;
            for row in 0..r.len() {
                jacobian[row][column] = (shifted[row] - r[row]) / h;
            }
        }
        let n = x.len();
        let mut normal = vec![vec![0.0; n]; n];
        let mut gradient = vec![0.0; n];
        for i in 0..n {
            for j in 0..n {
                normal[i][j] = (0..r.len()).map(|row| jacobian[row][i] * jacobian[row][j]).sum();
            }
            gradient[i] = -(0..r.len()).map(|row| jacobian[row][i] * r[row]).sum::<f64>();
        }
        let mut improved = false;
        while damping < 1e9 {
            let mut damped = normal.clone();
            for (i, row) in damped.iter_mut().enumerate() {
                row[i] += damping * (normal[i][i] + 1e-9);
            }
            let Some(step) = linear(damped, gradient.clone()) else {
                damping *= 10.0;
                continue;
            };
            let mut next: Vec<f64> = x.iter().zip(&step).map(|(value, step)| value + step).collect();
            clamp(&mut next);
            let next_r = residual(&next)?;
            let next_cost = norm(&next_r);
            if next_cost < cost {
                (x, r, cost) = (next, next_r, next_cost);
                damping = (damping * 0.3).max(1e-9);
                improved = true;
                break;
            }
            damping *= 10.0;
        }
        if !improved {
            break;
        }
    }
    Ok((x, cost.sqrt()))
}

/// The values with the unknowns set to `x`.
fn with(values: &BTreeMap<u32, [f64; 2]>, unknowns: &[Unknown], x: &[f64]) -> BTreeMap<u32, [f64; 2]> {
    let mut values = values.clone();
    for (unknown, value) in unknowns.iter().zip(x) {
        values.entry(unknown.parameter).or_insert([0.0, 0.0])[unknown.axis] = *value;
    }
    values
}

/// Solves the parameters of `controller` toward `goal` from `values`: the solved values and the distance left.
fn reach(
    model: &Value,
    textures: &[TextureBlob],
    values: &BTreeMap<u32, [f64; 2]>,
    controller: &Controller,
    goal: &Goal,
) -> Result<(BTreeMap<u32, [f64; 2]>, f64), String> {
    let unknowns = unknowns(model, &controller.parameters)?;
    let start = unknowns.iter().map(|unknown| values.get(&unknown.parameter).map_or(0.0, |v| v[unknown.axis]));
    let start: Vec<f64> = start.collect();
    let mut residual = |x: &[f64]| residual(&draw(model, &with(values, &unknowns, x), textures)?, controller, goal);
    let (x, distance) = solve(&unknowns, start, &mut residual)?;
    Ok((with(values, &unknowns, &x), distance))
}

/// Keys the parameters of `controller` at `frame` of the clip with `solved` (each axis of a 2D parameter).
/// The frame must then draw the solved values from the current parameter values `runtime`; it does not, for example,
/// on a later key of a Stepped lane, or with a Multiplicative lane on a value other than 1.
fn key(
    model: &mut Value,
    name: &str,
    controller: &Controller,
    frame: i32,
    solved: &BTreeMap<u32, [f64; 2]>,
    runtime: &BTreeMap<u32, [f64; 2]>,
) -> Result<Vec<Value>, String> {
    let unknowns = unknowns(model, &controller.parameters)?;
    let animation = animation::animation_mut(model, name)?;
    let length = animation::validate_animation_header(animation)?;
    let mut keys = Vec::new();
    for unknown in &unknowns {
        let value = solved.get(&unknown.parameter).map_or(0.0, |value| value[unknown.axis]) as f32;
        animation::put_key(animation, unknown.parameter, unknown.axis, frame, value, length)?;
        keys.push((unknown, value));
    }
    let drawn = animation::sample(model, name, f64::from(frame), runtime, false)?;
    for (unknown, value) in &keys {
        let now = drawn.get(&unknown.parameter).map_or(0.0, |drawn| drawn[unknown.axis]);
        if (now - f64::from(*value)).abs() > 1e-4 * f64::from(value.abs()).max(1.0) {
            return Err(format!(
                "the lane of parameter {} axis {} does not draw its key on frame {frame}, so a controller cannot key \
                 it",
                unknown.parameter, unknown.axis
            ));
        }
    }
    let keys = keys.iter().map(|(unknown, value)| json!({"parameter": unknown.parameter, "axis": unknown.axis,
        "value": value}));
    Ok(keys.collect())
}

/// The parameters that can move a Part: those bound to it or a node above it, as a parameter bookmark lists them.
fn movers(model: &Value, node: u32) -> Result<Vec<u32>, String> {
    let listed = animation::bookmark_parameters(model, node)?;
    let listed = listed["parameters"].as_array().into_iter().flatten();
    Ok(listed.filter_map(|p| p["uuid"].as_u64().and_then(crate::valid_id)).collect())
}

fn point(value: &Value, key: &str) -> Result<[f64; 2], String> {
    let pair = value.as_array().filter(|pair| pair.len() == 2);
    let pair = pair.and_then(|pair| Some([pair[0].as_f64()?, pair[1].as_f64()?]));
    pair.filter(|pair| pair.iter().all(|value| value.is_finite())).ok_or_else(|| format!("{key} must be [x, y], finite"))
}

fn store(model: &mut Value, name: &str, list: &[Controller]) -> Result<(), String> {
    let animation = animation::animation_mut(model, name)?;
    let object = animation.as_object_mut().ok_or("animation must be an object")?;
    if list.is_empty() {
        object.remove("controllers");
    } else {
        object.insert("controllers".into(), Value::Array(list.iter().map(controller_json).collect()));
    }
    Ok(())
}

fn find(list: &[Controller], name: &str, controller: &str) -> Result<usize, String> {
    list.iter().position(|item| item.name == controller)
        .ok_or_else(|| format!("animation {name:?} has no controller named {controller:?}"))
}

impl Session {
    fn clip(&self, name: &str) -> Result<(&Value, usize), String> {
        let animation = self.document.model["animations"].get(name);
        let animation = animation.ok_or_else(|| format!("animation {name:?} was not found"))?;
        Ok((animation, animation::validate_animation_header(animation)?))
    }

    /// The parameter values a frame of the clip draws with, before eye blinking, which plays on top of the keys.
    fn frame_values(&self, name: &str, frame: i32) -> Result<BTreeMap<u32, [f64; 2]>, String> {
        animation::sample(&self.document.model, name, f64::from(frame), &self.runtime_values, false)
    }

    /// `animation.controller.add {name, controller, x, y, end?, frame?, parameters?}`: a controller pinned to the
    /// frontmost Part drawn at (x, y) on `frame` (default 0), a direction controller with `end`. Its parameters are
    /// those listed, or every parameter that can move its Part (or Parts).
    pub(crate) fn op_controller_add(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.add")?;
        let name = required_string(request, "name")?;
        let label = required_string(request, "controller")?;
        if label.trim().is_empty() {
            return Err("controller must not be empty".into());
        }
        let (animation, length) = self.clip(name)?;
        let mut list = controllers(animation)?;
        if list.iter().any(|item| item.name == label) {
            return Err(format!("animation {name:?} has a controller named {label:?} already"));
        }
        if list.len() >= MAX_CONTROLLERS {
            return Err(format!("an animation holds at most {MAX_CONTROLLERS} controllers"));
        }
        let frame = match request.get("frame") {
            None | Some(Value::Null) => 0,
            Some(_) => animation::frame_field(request, "frame", length)?,
        };
        let at = point(&json!([request.get("x"), request.get("y")]), "x and y")?;
        let model = &self.document.model;
        let drawn = draw(model, &self.frame_values(name, frame)?, self.document.textures())?;
        let missing = |at: [f64; 2]| format!("no Part is drawn at ({}, {}) on frame {frame}", at[0], at[1]);
        let start = pin(&drawn, at).ok_or_else(|| missing(at))?;
        let end = match request.get("end") {
            None | Some(Value::Null) => None,
            Some(end) => {
                let end = point(end, "end")?;
                Some(pin(&drawn, end).ok_or_else(|| missing(end))?)
            }
        };
        let mut allowed = movers(model, start.node)?;
        if let Some(end) = &end {
            for id in movers(model, end.node)? {
                if !allowed.contains(&id) {
                    allowed.push(id);
                }
            }
        }
        let parameters = match request.get("parameters") {
            None | Some(Value::Null) => allowed,
            Some(list) => {
                let list = list.as_array().ok_or("parameters must be an array of parameter UUIDs")?;
                let mut ids = Vec::new();
                for id in list {
                    let id = id.as_u64().and_then(crate::valid_id);
                    let id = id.ok_or("parameters must be an array of parameter UUIDs")?;
                    if !allowed.contains(&id) {
                        return Err(format!("parameter {id} cannot move the Part of controller {label:?}"));
                    }
                    if ids.contains(&id) {
                        return Err(format!("parameter {id} is listed twice"));
                    }
                    ids.push(id);
                }
                if ids.is_empty() {
                    return Err("parameters must not be empty".into());
                }
                ids
            }
        };
        if parameters.is_empty() {
            return Err(format!("no parameter can move the Part of controller {label:?}"));
        }
        unknowns(model, &parameters)?;
        let controller =
            Controller { name: label.to_owned(), start, end, parameters, target: None, extend: true, weight: 100.0 };
        let added = controller_json(&controller);
        list.push(controller);
        let mut candidate = model.clone();
        store(&mut candidate, name, &list)?;
        self.commit_model(candidate)?;
        Ok(added)
    }

    /// `animation.controller.set {name, controller, parameters?, target?, extend?, weight?}`: changes what a controller
    /// moves and follows. A null target follows nothing.
    pub(crate) fn op_controller_set(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.set")?;
        let name = required_string(request, "name")?;
        let label = required_string(request, "controller")?;
        let (animation, _) = self.clip(name)?;
        let mut list = controllers(animation)?;
        let index = find(&list, name, label)?;
        let model = &self.document.model;
        if let Some(ids) = request.get("parameters").filter(|ids| !ids.is_null()) {
            let mut allowed = movers(model, list[index].start.node)?;
            if let Some(end) = &list[index].end {
                allowed.extend(movers(model, end.node)?);
            }
            let ids = ids.as_array().ok_or("parameters must be an array of parameter UUIDs")?;
            let mut parameters = Vec::new();
            for id in ids {
                let id = id.as_u64().and_then(crate::valid_id).ok_or("parameters must be an array of parameter UUIDs")?;
                if !allowed.contains(&id) {
                    return Err(format!("parameter {id} cannot move the Part of controller {label:?}"));
                }
                if parameters.contains(&id) {
                    return Err(format!("parameter {id} is listed twice"));
                }
                parameters.push(id);
            }
            if parameters.is_empty() {
                return Err("parameters must not be empty".into());
            }
            unknowns(model, &parameters)?;
            list[index].parameters = parameters;
        }
        match request.get("target") {
            None => {}
            Some(Value::Null) => list[index].target = None,
            Some(target) => {
                let target = target.as_str().ok_or("target must be a controller name or null")?;
                if target == label {
                    return Err("a controller cannot follow itself".into());
                }
                find(&list, name, target)?;
                list[index].target = Some(target.to_owned());
            }
        }
        if let Some(extend) = request.get("extend") {
            list[index].extend = extend.as_bool().ok_or("extend must be a boolean")?;
        }
        if let Some(weight) = request.get("weight") {
            list[index].weight = weight.as_f64().filter(|weight| (0.0..=100.0).contains(weight))
                .ok_or("weight must be a number from 0 to 100")?;
        }
        let changed = controller_json(&list[index]);
        let mut candidate = model.clone();
        store(&mut candidate, name, &list)?;
        self.commit_model(candidate)?;
        Ok(changed)
    }

    /// `animation.controller.delete {name, controller?}`: one controller, or every one of the clip. A controller that
    /// followed a deleted one follows nothing.
    pub(crate) fn op_controller_delete(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.delete")?;
        let name = required_string(request, "name")?;
        let (animation, _) = self.clip(name)?;
        let mut list = controllers(animation)?;
        if list.is_empty() {
            return Err(format!("animation {name:?} has no controllers"));
        }
        let deleted: Vec<String> = match request.get("controller") {
            None | Some(Value::Null) => list.drain(..).map(|item| item.name).collect(),
            Some(label) => {
                let label = label.as_str().ok_or("controller must be a string")?;
                vec![list.remove(find(&list, name, label)?).name]
            }
        };
        for item in &mut list {
            if item.target.as_ref().is_some_and(|target| deleted.contains(target)) {
                item.target = None;
            }
        }
        let mut candidate = self.document.model.clone();
        store(&mut candidate, name, &list)?;
        self.commit_model(candidate)?;
        Ok(json!({"deleted": deleted}))
    }

    /// `animation.controller.read {name, frame}`: where each controller is on `frame`, read only. A controller whose
    /// Part is not drawn, or has lost the vertex it is pinned to, or whose target is gone, says so in `broken`.
    pub(crate) fn op_controller_read(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.read")?;
        let name = required_string(request, "name")?;
        let (animation, length) = self.clip(name)?;
        let frame = animation::frame_field(request, "frame", length)?;
        let list = controllers(animation)?;
        let model = &self.document.model;
        let drawn = draw(model, &self.frame_values(name, frame)?, self.document.textures())?;
        let names: HashSet<&str> = list.iter().map(|item| item.name.as_str()).collect();
        let read: Vec<Value> = list
            .iter()
            .map(|item| {
                let mut out = json!({"name": item.name, "kind": if item.end.is_some() { "direction" } else { "point" },
                    "parameters": item.parameters, "target": item.target, "extend": item.extend, "weight": item.weight});
                match ends(&drawn, item) {
                    Ok((start, end)) => {
                        out["start"] = json!(start);
                        if let Some(end) = end {
                            out["end"] = json!(end);
                        }
                    }
                    Err(error) => out["broken"] = json!(error),
                }
                if let Some(target) = item.target.as_deref().filter(|target| !names.contains(target)) {
                    out["broken"] = json!(format!("target {target:?} is not a controller of the animation"));
                }
                out
            })
            .collect();
        Ok(json!({"name": name, "frame": frame, "controllers": read}))
    }

    /// `animation.controller.move {name, controller, frame, x, y}`: solves the controller's parameters so that a
    /// single-point controller reaches (x, y), or a direction controller turns its end toward it, and keys them on the
    /// integer `frame`, in one undo step. Answers how near it came.
    pub(crate) fn op_controller_move(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.move")?;
        let name = required_string(request, "name")?;
        let label = required_string(request, "controller")?;
        let (animation, length) = self.clip(name)?;
        let frame = animation::frame_field(request, "frame", length)?;
        let to = point(&json!([request.get("x"), request.get("y")]), "x and y")?;
        let list = controllers(animation)?;
        let controller = &list[find(&list, name, label)?];
        let model = &self.document.model;
        let values = self.frame_values(name, frame)?;
        let (solved, distance) = reach(model, self.document.textures(), &values, controller, &Goal::Point(to))?;
        let mut candidate = model.clone();
        let keys = key(&mut candidate, name, controller, frame, &solved, &self.runtime_values)?;
        self.commit_model(candidate)?;
        Ok(json!({"reached": distance < REACHED, "distance": distance, "keys": keys}))
    }

    /// `animation.controller.bake {name, from?, to?}`: for every integer frame from `from` to `to` (default the whole
    /// clip), each controller with a target, in list order, follows it; its parameters are keyed with the solved
    /// values, blended by its weight. Then the clip's controllers are removed, as the manual's baking does. One undo.
    pub(crate) fn op_controller_bake(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.controller.bake")?;
        let name = required_string(request, "name")?;
        let (animation, length) = self.clip(name)?;
        if animation::freeze_frame(animation).is_some() {
            return Err("target tracking cannot run while freeze is on; turn freeze off first".into());
        }
        let bound = |key: &str, default: i32| match request.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(_) => animation::frame_field(request, key, length),
        };
        let (from, to) = (bound("from", 0)?, bound("to", length as i32 - 1)?);
        if from > to {
            return Err("from must not be after to".into());
        }
        if i64::from(to - from) >= MAX_BAKE_FRAMES {
            return Err(format!("bake at most {MAX_BAKE_FRAMES} frames at a time"));
        }
        let list = controllers(animation)?;
        let following: Vec<(&Controller, &Controller)> = list
            .iter()
            .filter_map(|item| Some((item, list.iter().find(|other| Some(&other.name) == item.target.as_ref())?)))
            .collect();
        if following.is_empty() {
            return Err(format!("no controller of animation {name:?} follows another of its controllers"));
        }
        let mut candidate = self.document.model.clone();
        let textures = self.document.textures();
        let mut keyed = 0;
        let mut unreached = Vec::new();
        for frame in from..=to {
            // Each frame starts from the clip as it was, not from the keys baked on the frames before it.
            let mut values = self.frame_values(name, frame)?;
            for (controller, target) in &following {
                let (solved, distance) = reach(&candidate, textures, &values, controller, &Goal::Follow(target))?;
                if distance >= REACHED {
                    unreached.push(json!({"controller": controller.name, "frame": frame, "distance": distance}));
                }
                // A weight below 100 blends the solved values with those the frame had.
                for id in &controller.parameters {
                    let (Some(before), Some(after)) = (values.get(id).copied(), solved.get(id).copied()) else {
                        continue;
                    };
                    let mix = |a: f64, b: f64| a + (b - a) * controller.weight / 100.0;
                    values.insert(*id, [mix(before[0], after[0]), mix(before[1], after[1])]);
                }
                keyed += key(&mut candidate, name, controller, frame, &values, &self.runtime_values)?.len();
            }
        }
        store(&mut candidate, name, &[])?;
        self.commit_model(candidate)?;
        Ok(json!({"from": from, "to": to, "keys": keyed, "unreached": unreached}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_linear_solver_and_the_angle_and_line_helpers_answer_known_values() {
        let x = linear(vec![vec![2.0, 1.0], vec![1.0, 3.0]], vec![3.0, 5.0]).unwrap();
        assert!((x[0] - 0.8).abs() < 1e-12 && (x[1] - 1.4).abs() < 1e-12, "{x:?}");
        assert_eq!(linear(vec![vec![1.0, 2.0], vec![2.0, 4.0]], vec![1.0, 2.0]), None);
        assert!((turn(3.0, -3.0) - (6.0 - std::f64::consts::TAU)).abs() < 1e-12);
        assert_eq!(nearest([0.0, 0.0], [2.0, 0.0], [5.0, 1.0], true), [5.0, 0.0]);
        assert_eq!(nearest([0.0, 0.0], [2.0, 0.0], [5.0, 1.0], false), [2.0, 0.0]);
        let unknowns = [Unknown { parameter: 1, axis: 0, min: -1.0, max: 1.0 }];
        let (x, left) = solve(&unknowns, vec![0.0], &mut |x| Ok(vec![x[0] * 4.0 - 2.0])).unwrap();
        assert!((x[0] - 0.5).abs() < 1e-3 && left < REACHED, "{x:?} {left}");
        let (x, left) = solve(&unknowns, vec![0.0], &mut |x| Ok(vec![x[0] - 3.0])).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-9 && (left - 2.0).abs() < 1e-6, "clamped to the range: {x:?} {left}");
    }

    #[test]
    fn each_pair_of_controller_kinds_has_its_own_residual() {
        let drawn = [
            Drawn { id: 1, vertices: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], indices: vec![0, 1, 2] },
            Drawn { id: 2, vertices: vec![[3.0, 5.0], [3.0, 6.0], [4.0, 5.0]], indices: vec![0, 1, 2] },
        ];
        let at = |node: u32, vertex: usize| {
            let mut weights = [0.0; 3];
            weights[vertex] = 1.0;
            Anchor { node, vertices: [0, 1, 2], weights }
        };
        let controller = |start: Anchor, end: Option<Anchor>| Controller {
            name: "C".into(), start, end, parameters: vec![], target: None, extend: true, weight: 100.0,
        };
        let point = controller(at(1, 0), None);
        let direction = controller(at(1, 0), Some(at(1, 1)));
        let (target_point, target_direction) = (controller(at(2, 0), None), controller(at(2, 0), Some(at(2, 1))));
        let follow = |c: &Controller, t: &Controller| residual(&drawn, c, &Goal::Follow(t)).unwrap();
        assert_eq!(follow(&point, &target_point), vec![-3.0, -5.0]);
        assert_eq!(follow(&point, &target_direction), vec![-3.0, 0.0], "to the line x = 3");
        assert_eq!(follow(&direction, &target_point), vec![0.0, 5.0], "the line y = 0 should pass (3, 5)");
        let turned = follow(&direction, &target_direction);
        assert!((turned[0] + std::f64::consts::FRAC_PI_2).abs() < 1e-12, "{turned:?}");
        assert_eq!(residual(&drawn, &point, &Goal::Point([2.0, 2.0])).unwrap(), vec![-2.0, -2.0]);
        let aimed = residual(&drawn, &direction, &Goal::Point([0.0, 2.0])).unwrap();
        assert!((aimed[0] + std::f64::consts::FRAC_PI_2).abs() < 1e-12, "{aimed:?}");
        let mut lost = point.clone();
        lost.start.vertices = [0, 1, 9];
        assert_eq!(residual(&drawn, &lost, &Goal::Point([0.0, 0.0])), Err("node 1 has no vertex 9".into()));
        assert_eq!(pin(&drawn, [3.2, 5.2]).map(|anchor| anchor.node), Some(2));
        assert_eq!(pin(&drawn, [9.0, 9.0]), None);
    }
}
