//! Deterministic runtime simulation for D SimplePhysics and PathDeformer drivers.
//!
//! Values passed to `step` are current external parameter inputs. Deformations
//! are preview overrides only; parameter-bound deformation inputs are evaluated
//! after SimplePhysics outputs. Both must exclude previous simulation output.

use crate::deformers::node_geometry_points;
use crate::nodes::find_node;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

type Vec2 = [f32; 2];
type Mat4 = crate::bone_deform::Mat4;

const DT_CAP: f32 = 10.0;
const SUBSTEP: f32 = 0.01;
const EPSILON: f32 = f32::EPSILON;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Runtime {
    states: BTreeMap<u32, State>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    pub(crate) parameter_values: BTreeMap<u32, [f64; 2]>,
    pub(crate) path_deformations: BTreeMap<u32, Vec<[f64; 2]>>,
    pub(crate) path_preprocess: BTreeMap<u32, crate::scene_deform::PathUpdate>,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    SimplePendulum(SimplePendulum),
    SimpleSpring(SimpleSpring),
    PathPendulum(PathPendulum),
    PathSpring(PathSpring),
}

#[derive(Clone, Debug, PartialEq)]
struct SimplePendulum {
    bob: Vec2,
    angle: f32,
    angular_velocity: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct SimpleSpring {
    bob: Vec2,
    velocity: Vec2,
    last_valid_direction: Vec2,
}

#[derive(Clone, Debug, PartialEq)]
struct PathPendulum {
    shape: Vec<Vec2>,
    angles: Vec<f32>,
    initial_angles: Vec<f32>,
    angular_velocities: Vec<f32>,
    lengths: Vec<f32>,
    base: Vec2,
    damping: f32,
    restore_constant: f32,
    gravity: f32,
    input_scale: f32,
    propagate_scale: f32,
    previous_root: Option<Vec2>,
}

#[derive(Clone, Debug, PartialEq)]
struct PathSpring {
    shape: Vec<Vec2>,
    positions: Vec<Vec2>,
    initial_positions: Vec<Vec2>,
    velocities: Vec<Vec2>,
    lengths: Vec<f32>,
    damping: f32,
    spring_constant: f32,
    restoration_constant: f32,
    gravity: Vec2,
    previous_root: Option<Vec2>,
}

#[derive(Clone, Copy)]
struct SimpleSettings {
    model_type: SimpleModel,
    param: Option<u32>,
    local_only: bool,
    gravity: f32,
    length: f32,
    frequency: f32,
    angle_damping: f32,
    length_damping: f32,
    output_scale: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SimpleModel {
    Pendulum,
    SpringPendulum,
}

#[derive(Clone, Copy)]
struct PathDriverKind {
    spring: bool,
}

impl Runtime {
    pub(crate) fn step(
        &mut self,
        model: &Value,
        values: &BTreeMap<u32, [f64; 2]>,
        deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
        dt: f64,
    ) -> Result<Step, String> {
        if !dt.is_finite() || dt < 0.0 {
            return Err("physics dt must be finite and nonnegative".into());
        }

        let mut staged = self.clone();
        let result = staged.step_staged(
            model,
            values,
            deformations,
            dt.min(f64::from(DT_CAP)) as f32,
        )?;
        *self = staged;
        Ok(result)
    }

    fn step_staged(
        &mut self,
        model: &Value,
        values: &BTreeMap<u32, [f64; 2]>,
        deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
        dt: f32,
    ) -> Result<Step, String> {
        let mut parameter_values = effective_values(model, values)?;
        let drivers = driver_nodes(model)?;
        let active: BTreeSet<u32> = drivers.iter().map(|(id, _)| *id).collect();
        self.states.retain(|id, _| active.contains(id));
        let mut path_preprocess = BTreeMap::new();

        for (id, kind) in drivers.iter().copied() {
            if !matches!(kind, DriverKind::Simple) {
                continue;
            }
            let evaluated = crate::evaluation::evaluate(model, &parameter_values)?;
            let node = find_node(&evaluated, id)
                .ok_or_else(|| format!("physics node {id} was not found"))?;
            if let Some((param, value)) = self.step_simple(id, model, &evaluated, node, dt)? {
                if parameter_values.contains_key(&param) {
                    parameter_values.insert(param, value);
                }
            }
        }

        // D updates all parameter-driving SimplePhysics drivers before path
        // drivers. Evaluate their final parameter outputs together for Paths.
        let evaluated = crate::evaluation::evaluate(model, &parameter_values)?;
        let mut path_inputs = crate::evaluation::raster_deformations(model, &parameter_values)?;
        crate::evaluation::overlay_preview(&mut path_inputs, deformations);
        let path_drivers: BTreeMap<_, _> = drivers.into_iter().filter_map(|(id, kind)| {
            match kind {
                DriverKind::Path(kind) => Some((id, kind)),
                DriverKind::Simple => None,
            }
        }).collect();
        let scene = crate::scene_deform::evaluate_with_path_update(
            &evaluated,
            model.get("nodes").ok_or("model.nodes is missing")?,
            &path_inputs,
            &mut |id, node, world, rotation, input| {
                let Some(kind) = path_drivers.get(&id) else { return Ok(None); };
                let offsets = self.step_path(id, node, world, rotation, input, *kind, dt)?;
                if let Some(offsets) = &offsets {
                    path_preprocess.insert(id, offsets.clone());
                }
                Ok(offsets)
            },
        )?;
        let path_deformations = path_preprocess.keys().map(|id| {
            (*id, scene.local_offsets[id].clone())
        }).collect();

        Ok(Step {
            parameter_values,
            path_deformations,
            path_preprocess,
        })
    }

    fn step_simple(
        &mut self,
        id: u32,
        model: &Value,
        evaluated: &Value,
        node: &Value,
        dt: f32,
    ) -> Result<Option<(u32, [f64; 2])>, String> {
        let settings = simple_settings(model, node, id)?;
        let original_root = model.get("nodes").ok_or("model.nodes is missing")?;
        let transform = crate::bone_deform::world_matrix_with_root(evaluated, id, original_root)
            .map_err(|error| format!("SimplePhysics node {id}: {error}"))?;
        let anchor = simple_anchor(node, transform, settings.local_only, id)?;
        let length = settings.length;
        let state = self
            .states
            .entry(id)
            .or_insert_with(|| match settings.model_type {
                SimpleModel::Pendulum => State::SimplePendulum(SimplePendulum {
                    bob: [anchor[0], anchor[1] + length],
                    angle: 0.0,
                    angular_velocity: 0.0,
                }),
                SimpleModel::SpringPendulum => State::SimpleSpring(SimpleSpring {
                    bob: [anchor[0], anchor[1] + length],
                    velocity: [0.0, 0.0],
                    last_valid_direction: [0.0, 1.0],
                }),
            });

        let output = match settings.model_type {
            SimpleModel::Pendulum => {
                if !matches!(&*state, State::SimplePendulum(_)) {
                    *state = State::SimplePendulum(SimplePendulum {
                        bob: [anchor[0], anchor[1] + length],
                        angle: 0.0,
                        angular_velocity: 0.0,
                    });
                }
                if let State::SimplePendulum(state) = state {
                    tick_simple_pendulum(state, anchor, settings, dt)?
                } else {
                    unreachable!()
                }
            }
            SimpleModel::SpringPendulum => {
                if !matches!(&*state, State::SimpleSpring(_)) {
                    *state = State::SimpleSpring(SimpleSpring {
                        bob: [anchor[0], anchor[1] + length],
                        velocity: [0.0, 0.0],
                        last_valid_direction: [0.0, 1.0],
                    });
                }
                if let State::SimpleSpring(state) = state {
                    tick_simple_spring(state, anchor, settings, dt)?
                } else {
                    unreachable!()
                }
            }
        };

        let Some(param) = settings.param else {
            return Ok(None);
        };
        let Some(output) = output else {
            return Ok(None);
        };
        let anchor_distance = length_of(sub(output, anchor));
        if !anchor_distance.is_finite() || !length.is_finite() || length.abs() <= EPSILON {
            return Ok(None);
        }
        let mut local_position = output;
        if !settings.local_only {
            let inverse = crate::bone_deform::invert(transform)
                .map_err(|error| format!("SimplePhysics node {id} transform: {error}"))?;
            let local = crate::bone_deform::transform_point(
                inverse,
                [f64::from(output[0]), f64::from(output[1]), 0.0],
            )
            .map_err(|error| format!("SimplePhysics node {id} output transform: {error}"))?;
            local_position = [local[0] as f32, local[1] as f32];
        }
        let local_length = length_of(local_position);
        if !local_length.is_finite() || local_length <= EPSILON {
            return Ok(None);
        }
        let local_direction = [
            local_position[0] / local_length,
            local_position[1] / local_length,
        ];
        let relative_length = anchor_distance / length;
        if !relative_length.is_finite() {
            return Ok(None);
        }
        let mapped = match map_mode(node, id)? {
            MapMode::XY => [
                local_direction[0] * relative_length,
                1.0 - local_direction[1] * relative_length,
            ],
            MapMode::YX => [
                1.0 - local_direction[1] * relative_length,
                local_direction[0] * relative_length,
            ],
            MapMode::AngleLength => [
                (-local_direction[0]).atan2(local_direction[1]) / std::f32::consts::PI,
                relative_length,
            ],
            MapMode::LengthAngle => [
                relative_length,
                (-local_direction[0]).atan2(local_direction[1]) / std::f32::consts::PI,
            ],
        };
        let mapped = [
            mapped[0] * settings.output_scale[0],
            mapped[1] * settings.output_scale[1],
        ];
        if mapped.iter().any(|value| !value.is_finite()) {
            return Err(format!("SimplePhysics node {id} output is not finite"));
        }
        Ok(Some((param, [f64::from(mapped[0]), f64::from(mapped[1])])))
    }

    fn step_path(
        &mut self,
        id: u32,
        node: &Value,
        world: Mat4,
        rotation: f64,
        input_deformation: &[[f64; 2]],
        kind: PathDriverKind,
        dt: f32,
    ) -> Result<Option<crate::scene_deform::PathUpdate>, String> {
        let source_points = node_geometry_points(node);
        if source_points.len() < 2 {
            return Ok(None);
        }
        let points: Vec<Vec2> = source_points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                finite_vec2(
                    [point[0] as f32, point[1] as f32],
                    &format!("PathDeformer node {id} vertices[{index}]"),
                )
            })
            .collect::<Result<_, _>>()?;
        let input = offsets_to_f32(input_deformation, id)?;
        if input.len() != points.len() {
            return Err(format!(
                "PathDeformer node {id} deformation length does not match vertices"
            ));
        }
        for (index, segment) in points.windows(2).enumerate() {
            let difference = sub(segment[1], segment[0]);
            let length = length_of(difference);
            if !difference.iter().all(|component| component.is_finite())
                || !length.is_finite()
                || length <= 1e-6
            {
                return Err(format!(
                    "PathDeformer node {id} has a degenerate segment at {index}"
                ));
            }
        }
        let local_root = [points[0][0] + input[0][0], points[0][1] + input[0][1]];
        finite_vec2(local_root, &format!("PathDeformer node {id} local root"))?;
        let world_root = crate::bone_deform::transform_point(
            world,
            [f64::from(local_root[0]), f64::from(local_root[1]), 0.0],
        )
        .map_err(|error| format!("PathDeformer node {id} root transform: {error}"))?;
        let world_root = finite_vec2(
            [world_root[0] as f32, world_root[1] as f32],
            &format!("PathDeformer node {id} world root"),
        )?;
        let rotation = finite_f32(rotation as f32, &format!("PathDeformer node {id} world rotation"))?;
        let physics = node
            .get("physics")
            .ok_or_else(|| format!("PathDeformer node {id} physics is missing"))?;
        let physics_only = match node.get("physics_only") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(format!(
                    "PathDeformer node {id}.physics_only must be a boolean"
                ))
            }
        };

        let fresh_state = if kind.spring {
            State::PathSpring(make_path_spring(&points, id)?)
        } else {
            State::PathPendulum(make_path_pendulum(physics, &points)?)
        };
        let state_matches = match (self.states.get(&id), &fresh_state) {
            (Some(State::PathPendulum(current)), State::PathPendulum(fresh)) => {
                current.shape == fresh.shape
                    && current.initial_angles == fresh.initial_angles
                    && current.lengths == fresh.lengths
                    && current.base == fresh.base
                    && current.damping == fresh.damping
                    && current.restore_constant == fresh.restore_constant
                    && current.gravity == fresh.gravity
                    && current.input_scale == fresh.input_scale
                    && current.propagate_scale == fresh.propagate_scale
            }
            (Some(State::PathSpring(current)), State::PathSpring(fresh)) => {
                current.shape == fresh.shape
            }
            _ => false,
        };
        if !state_matches {
            self.states.insert(id, fresh_state);
        }
        let state = self.states.get_mut(&id).expect("path state inserted");
        let first_frame = match state {
            State::PathPendulum(state) => state.previous_root.is_none(),
            State::PathSpring(state) => state.previous_root.is_none(),
            _ => unreachable!(),
        };
        let physics_offsets = match state {
            State::PathPendulum(state) => {
                tick_path_pendulum(state, &points, world_root, rotation, dt, id)?
            }
            State::PathSpring(state) => tick_path_spring(state, &points, world_root, dt, id)?,
            _ => unreachable!(),
        };

        let effective = physics_offsets
            .iter()
            .zip(input)
            .enumerate()
            .map(|(index, (physics_offset, input_offset))| {
                if physics_only {
                    if first_frame {
                        Ok([f64::from(input_offset[0]), f64::from(input_offset[1])])
                    } else {
                        finite_vec2(
                            *physics_offset,
                            &format!("PathDeformer node {id} physics deformation[{index}]"),
                        )?;
                        Ok([f64::from(physics_offset[0]), f64::from(physics_offset[1])])
                    }
                } else {
                    let combined = add(input_offset, *physics_offset);
                    finite_vec2(
                        combined,
                        &format!("PathDeformer node {id} effective deformation[{index}]"),
                    )?;
                    Ok([f64::from(combined[0]), f64::from(combined[1])])
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(crate::scene_deform::PathUpdate {
            offsets: effective,
            reset_reference: physics_only && !first_frame,
        }))
    }
}

#[derive(Clone, Copy)]
enum DriverKind {
    Simple,
    Path(PathDriverKind),
}

#[derive(Clone, Copy)]
enum MapMode {
    AngleLength,
    XY,
    LengthAngle,
    YX,
}

fn driver_nodes(model: &Value) -> Result<Vec<(u32, DriverKind)>, String> {
    fn walk(
        node: &Value,
        ancestors_enabled: bool,
        output: &mut Vec<(u32, DriverKind)>,
    ) -> Result<(), String> {
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(crate::valid_id)
            .ok_or_else(|| "physics node UUID must be a nonzero u32".to_string())?;
        let enabled =
            ancestors_enabled && node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if enabled {
            match node.get("type").and_then(Value::as_str).unwrap_or("Node") {
                "SimplePhysics" => output.push((id, DriverKind::Simple)),
                "PathDeformer" => {
                    if let Some(physics) = node.get("physics") {
                        let spring = match physics.get("type").and_then(Value::as_str) {
                            Some("Pendulum") => false,
                            Some("SpringPendulum") => true,
                            Some(other) => {
                                return Err(format!(
                                    "PathDeformer node {id} physics type {other:?} is unsupported"
                                ));
                            }
                            None => {
                                return Err(format!(
                                    "PathDeformer node {id} physics.type is missing"
                                ))
                            }
                        };
                        output.push((id, DriverKind::Path(PathDriverKind { spring })));
                    }
                }
                _ => {}
            }
        }
        if let Some(children) = node.get("children") {
            let children = children
                .as_array()
                .ok_or_else(|| format!("node {id}.children must be an array"))?;
            for child in children {
                walk(child, enabled, output)?;
            }
        }
        Ok(())
    }

    let root = model
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    let mut output = Vec::new();
    walk(root, true, &mut output)?;
    Ok(output)
}

pub(crate) fn simple_outputs(model: &Value) -> Result<(Vec<(u32, u32)>, bool), String> {
    let mut outputs = Vec::new();
    let mut seen = BTreeSet::new();
    let mut path = false;
    for (id, kind) in driver_nodes(model)? {
        match kind {
            DriverKind::Path(_) => path = true,
            DriverKind::Simple => {
                let node = find_node(model, id)
                    .ok_or_else(|| format!("physics node {id} was not found"))?;
                if let Some(param) = simple_settings(model, node, id)?.param {
                    if seen.insert(param) {
                        outputs.push((id, param));
                    }
                }
            }
        }
    }
    Ok((outputs, path))
}

fn effective_values(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
) -> Result<BTreeMap<u32, [f64; 2]>, String> {
    crate::evaluation::evaluate(model, values)?;
    fn collect(
        value: &Value,
        values: &BTreeMap<u32, [f64; 2]>,
        output: &mut BTreeMap<u32, [f64; 2]>,
    ) -> Result<(), String> {
        if value.get("groupUUID").is_some() {
            let children = value
                .get("children")
                .and_then(Value::as_array)
                .ok_or_else(|| "parameter group children must be an array".to_string())?;
            for child in children {
                collect(child, values, output)?;
            }
            return Ok(());
        }
        let id = value
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(crate::valid_id)
            .ok_or_else(|| "parameter UUID must be a nonzero u32".to_string())?;
        let defaults = json_vec2(
            value.get("defaults"),
            [0.0, 0.0],
            &format!("parameter {id}.defaults"),
        )?;
        let value = values.get(&id).copied().unwrap_or(defaults.map(f64::from));
        if !value[0].is_finite() || !value[1].is_finite() {
            return Err(format!("parameter {id} runtime value is not finite"));
        }
        output.insert(id, value);
        Ok(())
    }

    let groups = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or_else(|| "model.param must be an array".to_string())?;
    let mut output = BTreeMap::new();
    for parameter in groups {
        collect(parameter, values, &mut output)?;
    }
    Ok(output)
}

fn simple_settings(model: &Value, node: &Value, id: u32) -> Result<SimpleSettings, String> {
    let model_type = match node
        .get("model_type")
        .and_then(Value::as_str)
        .unwrap_or("Pendulum")
    {
        "Pendulum" => SimpleModel::Pendulum,
        "SpringPendulum" => SimpleModel::SpringPendulum,
        other => {
            return Err(format!(
                "SimplePhysics node {id} model_type {other:?} is unsupported"
            ))
        }
    };
    let param = match node.get("param").and_then(Value::as_u64) {
        Some(value) if value <= u64::from(u32::MAX) && value != u64::from(u32::MAX) => {
            Some(value as u32)
        }
        Some(value) if value == u64::from(u32::MAX) => None,
        None => None,
        Some(_) => return Err(format!("SimplePhysics node {id} param UUID is invalid")),
    };
    let global_physics = model.get("physics");
    let global_gravity = field_f32(
        global_physics,
        "gravity",
        9.8,
        &format!("model.physics.gravity"),
    )?;
    let pixels_per_meter = field_f32(
        global_physics,
        "pixelsPerMeter",
        1000.0,
        "model.physics.pixelsPerMeter",
    )?;
    let gravity = field_f32(Some(node), "gravity", 1.0, &format!("node {id}.gravity"))?
        * global_gravity
        * pixels_per_meter;
    Ok(SimpleSettings {
        model_type,
        param,
        local_only: match node.get("local_only") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(format!(
                    "SimplePhysics node {id}.local_only must be a boolean"
                ))
            }
        },
        gravity: finite_f32(gravity, &format!("SimplePhysics node {id} gravity"))?,
        length: field_f32(Some(node), "length", 100.0, &format!("node {id}.length"))?,
        frequency: field_f32(
            Some(node),
            "frequency",
            1.0,
            &format!("node {id}.frequency"),
        )?,
        angle_damping: field_f32(
            Some(node),
            "angle_damping",
            0.5,
            &format!("node {id}.angle_damping"),
        )?,
        length_damping: field_f32(
            Some(node),
            "length_damping",
            0.5,
            &format!("node {id}.length_damping"),
        )?,
        output_scale: json_vec2(
            node.get("output_scale"),
            [1.0, 1.0],
            &format!("node {id}.output_scale"),
        )?,
    })
}

fn simple_anchor(node: &Value, transform: Mat4, local_only: bool, id: u32) -> Result<Vec2, String> {
    if local_only {
        return Ok([
            transform_component(node, "trans", 0, 0.0, id)?,
            transform_component(node, "trans", 1, 0.0, id)?,
        ]);
    }
    let point = crate::bone_deform::transform_point(transform, [0.0, 0.0, 0.0])
        .map_err(|error| format!("SimplePhysics node {id} anchor: {error}"))?;
    finite_vec2(
        [point[0] as f32, point[1] as f32],
        &format!("SimplePhysics node {id} anchor"),
    )
}

fn tick_simple_pendulum(
    state: &mut SimplePendulum,
    anchor: Vec2,
    settings: SimpleSettings,
    dt: f32,
) -> Result<Option<Vec2>, String> {
    let delta = sub(state.bob, anchor);
    finite_vec2(delta, "pendulum bob-anchor vector")?;
    state.angle = (-delta[0]).atan2(delta[1]);
    finite_f32(state.angle, "pendulum angle")?;
    for_substeps(dt, |h| {
        let mut values = [state.angle, state.angular_velocity];
        rk4(&mut values, h, |sample| {
            let ratio = settings.gravity / settings.length;
            let acceleration = if !ratio.is_finite() || ratio < 0.0 {
                0.0
            } else {
                let critical_damping = 2.0 * ratio.sqrt();
                -ratio * sample[0].sin() - sample[1] * settings.angle_damping * critical_damping
            };
            [sample[1], acceleration]
        })?;
        state.angle = values[0];
        state.angular_velocity = values[1];
        Ok(())
    })?;
    if settings.length.abs() <= EPSILON {
        return Ok(None);
    }
    if state.angle.is_finite() && state.angular_velocity.is_finite() {
        let direction = [-state.angle.sin(), state.angle.cos()];
        let bob = add(anchor, scale(direction, settings.length));
        finite_vec2(bob, "pendulum bob")?;
        state.bob = bob;
        Ok(Some(bob))
    } else {
        Err("pendulum state is not finite".into())
    }
}

fn tick_simple_spring(
    state: &mut SimpleSpring,
    anchor: Vec2,
    settings: SimpleSettings,
    dt: f32,
) -> Result<Option<Vec2>, String> {
    for_substeps(dt, |h| {
        let mut values = [
            state.bob[0],
            state.bob[1],
            state.velocity[0],
            state.velocity[1],
        ];
        let last_valid_direction = &mut state.last_valid_direction;
        rk4(&mut values, h, |sample| {
            let bob = [sample[0], sample[1]];
            let velocity = [sample[2], sample[3]];
            spring_derivative(bob, velocity, anchor, settings, last_valid_direction)
        })?;
        state.bob = [values[0], values[1]];
        state.velocity = [values[2], values[3]];
        Ok(())
    })?;
    if finite_vec2(state.bob, "spring pendulum bob").is_ok()
        && finite_vec2(state.velocity, "spring pendulum velocity").is_ok()
    {
        Ok(Some(state.bob))
    } else {
        Err("spring pendulum state is not finite".into())
    }
}

fn spring_derivative(
    bob: Vec2,
    velocity: Vec2,
    anchor: Vec2,
    settings: SimpleSettings,
    last_valid_direction: &mut Vec2,
) -> [f32; 4] {
    let frequency = settings.frequency;
    let spring_k_sqrt = frequency * (2.0 * std::f32::consts::PI);
    let spring_k = spring_k_sqrt * spring_k_sqrt;
    let length = settings.length;
    if !frequency.is_finite()
        || frequency.abs() <= EPSILON
        || !spring_k_sqrt.is_finite()
        || !spring_k.is_finite()
        || spring_k.abs() <= EPSILON
        || !length.is_finite()
        || length.abs() <= EPSILON
    {
        return [velocity[0], velocity[1], 0.0, 0.0];
    }
    let rest_length = length - settings.gravity / spring_k;
    let offset = sub(bob, anchor);
    let offset_length = length_of(offset);
    let direction = if offset_length <= EPSILON {
        *last_valid_direction
    } else {
        let direction = [offset[0] / offset_length, offset[1] / offset_length];
        *last_valid_direction = direction;
        direction
    };
    let length_ratio = settings.gravity / length;
    if !length_ratio.is_finite() || length_ratio < 0.0 {
        return [velocity[0], velocity[1], 0.0, 0.0];
    }
    let critical_angle = 2.0 * length_ratio.sqrt();
    let critical_length = 2.0 * spring_k_sqrt;
    let force = sub(
        [0.0, settings.gravity],
        scale(direction, (offset_length - rest_length) * spring_k),
    );
    let velocity_rotated = [
        velocity[0] * direction[1] + velocity[1] * direction[0],
        velocity[1] * direction[1] - velocity[0] * direction[0],
    ];
    let damping_rotated = [
        -velocity_rotated[0] * settings.angle_damping * critical_angle,
        -velocity_rotated[1] * settings.length_damping * critical_length,
    ];
    let damping = [
        damping_rotated[0] * direction[1] - velocity_rotated[1] * direction[0],
        damping_rotated[1] * direction[1] + velocity_rotated[0] * direction[0],
    ];
    let acceleration = add(force, damping);
    [velocity[0], velocity[1], acceleration[0], acceleration[1]]
}

fn make_path_pendulum(physics: &Value, points: &[Vec2]) -> Result<PathPendulum, String> {
    let physics_points: Vec<Vec2> = points.iter().map(|point| [point[0], -point[1]]).collect();
    let (initial_angles, lengths) = extract_angles_and_lengths(&physics_points);
    Ok(PathPendulum {
        shape: points.to_vec(),
        angles: initial_angles.clone(),
        initial_angles: initial_angles.clone(),
        angular_velocities: vec![0.0; initial_angles.len()],
        lengths,
        base: physics_points[0],
        damping: field_f32(Some(physics), "damping", 1.0, "path physics damping")?,
        restore_constant: field_f32(
            Some(physics),
            "restore_constant",
            300.0,
            "path physics restore_constant",
        )?,
        gravity: field_f32(Some(physics), "gravity", 9.8, "path physics gravity")?,
        input_scale: field_f32(
            Some(physics),
            "input_scale",
            0.01,
            "path physics input_scale",
        )?,
        propagate_scale: field_f32(
            Some(physics),
            "propagate_scale",
            0.2,
            "path physics propagate_scale",
        )?,
        previous_root: None,
    })
}

fn tick_path_pendulum(
    state: &mut PathPendulum,
    points: &[Vec2],
    world_root: Vec2,
    rotation: f32,
    dt: f32,
    id: u32,
) -> Result<Vec<Vec2>, String> {
    let mut force = [0.0, 0.0];
    let update = if let Some(previous_root) = state.previous_root {
        let difference = sub(world_root, previous_root);
        force = scale(difference, state.input_scale);
        true
    } else {
        false
    };
    state.previous_root = Some(world_root);

    if update {
        let world_angle = -rotation;
        for_substeps(dt, |h| {
            update_path_pendulum(state, force, world_angle, h, id)
        })?;
    }
    let positions = calculate_positions(state.base, &state.angles, &state.lengths);
    if positions.len() != points.len() {
        return Err(format!(
            "PathDeformer node {id} physics shape is inconsistent"
        ));
    }
    positions
        .iter()
        .enumerate()
        .try_for_each(|(index, position)| {
            finite_vec2(
                *position,
                &format!("PathDeformer node {id} reconstructed point[{index}]"),
            )
            .map(|_| ())
        })?;
    positions
        .iter()
        .zip(points)
        .enumerate()
        .map(|(index, (position, original))| {
            finite_vec2(
                [position[0] - original[0], -position[1] - original[1]],
                &format!("PathDeformer node {id} physics deformation[{index}]"),
            )
        })
        .collect()
}

fn update_path_pendulum(
    state: &mut PathPendulum,
    force: Vec2,
    world_angle: f32,
    dt: f32,
    id: u32,
) -> Result<(), String> {
    if state.lengths.is_empty() {
        return Ok(());
    }
    let mut external_torque = force[0] * dt * state.lengths[0];
    for index in 0..state.angles.len() {
        let length = state.lengths[index];
        if !length.is_finite() || length <= 1e-6 {
            return Err(format!(
                "PathDeformer node {id} has a degenerate pendulum segment"
            ));
        }
        let angle = state.angles[index];
        let velocity = state.angular_velocities[index];
        let restoring_torque =
            -(1.0 / dt).min(state.restore_constant) * (angle - state.initial_angles[index]);
        let damping_torque = -state.damping * velocity;
        let gravitational_torque = -state.gravity * (angle + world_angle).sin();
        let acceleration =
            restoring_torque + damping_torque + gravitational_torque + external_torque;
        let new_velocity = velocity + acceleration * dt;
        let new_angle = angle + new_velocity * dt;
        if !acceleration.is_finite() || !new_velocity.is_finite() || !new_angle.is_finite() {
            return Err(format!(
                "PathDeformer node {id} pendulum state is not finite"
            ));
        }
        state.angular_velocities[index] = new_velocity;
        state.angles[index] = new_angle;
        external_torque += (-acceleration * new_angle.sin()) * dt * length * state.propagate_scale;
    }
    Ok(())
}

fn make_path_spring(points: &[Vec2], id: u32) -> Result<PathSpring, String> {
    let initial_positions: Vec<Vec2> = points.iter().map(|point| [point[0], -point[1]]).collect();
    let lengths = initial_positions
        .windows(2)
        .map(|segment| length_of(sub(segment[1], segment[0])))
        .collect::<Vec<_>>();
    if let Some((index, _)) = lengths
        .iter()
        .enumerate()
        .find(|(_, length)| !length.is_finite() || **length <= 1e-6)
    {
        return Err(format!(
            "PathDeformer node {id} has an invalid spring rest length at {index}"
        ));
    }
    Ok(PathSpring {
        shape: points.to_vec(),
        positions: initial_positions.clone(),
        initial_positions: initial_positions.clone(),
        velocities: vec![[0.0, 0.0]; initial_positions.len()],
        lengths,
        damping: 0.3,
        spring_constant: 10.0,
        restoration_constant: 0.0,
        gravity: [0.0, 9.8],
        previous_root: None,
    })
}

fn tick_path_spring(
    state: &mut PathSpring,
    points: &[Vec2],
    world_root: Vec2,
    dt: f32,
    id: u32,
) -> Result<Vec<Vec2>, String> {
    let force = state
        .previous_root
        .map(|previous| sub(world_root, previous));
    state.previous_root = Some(world_root);
    if let Some(force) = force {
        for_substeps(dt, |h| update_path_spring(state, force, h, id))?;
    }
    if state.positions.len() != points.len() {
        return Err(format!(
            "PathDeformer node {id} spring shape is inconsistent"
        ));
    }
    state
        .positions
        .iter()
        .zip(points)
        .enumerate()
        .map(|(index, (position, original))| {
            finite_vec2(
                [position[0] - original[0], -position[1] - original[1]],
                &format!("PathDeformer node {id} spring deformation[{index}]"),
            )
        })
        .collect()
}

fn update_path_spring(
    state: &mut PathSpring,
    external_force: Vec2,
    dt: f32,
    id: u32,
) -> Result<(), String> {
    for index in 1..state.positions.len() {
        let mut spring_force = [0.0, 0.0];
        let previous = if index == 1 {
            add(state.positions[index - 1], scale(external_force, dt))
        } else {
            state.positions[index - 1]
        };
        let difference = sub(state.positions[index], previous);
        let difference_length = length_of(difference);
        if !difference_length.is_finite() || difference_length <= 1e-6 {
            return Err(format!(
                "PathDeformer node {id} has a degenerate spring segment"
            ));
        }
        spring_force = sub(
            spring_force,
            scale(
                difference,
                state.spring_constant * (difference_length - state.lengths[index - 1])
                    / difference_length,
            ),
        );
        if index < state.positions.len() - 1 {
            let next_origin = if index == 0 {
                add(state.positions[index], scale(external_force, dt))
            } else {
                state.positions[index]
            };
            let difference = sub(next_origin, state.positions[index + 1]);
            let difference_length = length_of(difference);
            if !difference_length.is_finite() || difference_length <= 1e-6 {
                return Err(format!(
                    "PathDeformer node {id} has a degenerate spring segment"
                ));
            }
            spring_force = sub(
                spring_force,
                scale(
                    difference,
                    state.spring_constant * (difference_length - state.lengths[index])
                        / difference_length,
                ),
            );
        }
        let restoration = scale(
            sub(state.positions[index], state.initial_positions[index]),
            -state.restoration_constant,
        );
        let damping = scale(state.velocities[index], -state.damping);
        let acceleration = add(add(add(spring_force, damping), restoration), state.gravity);
        let velocity = add(state.velocities[index], scale(acceleration, dt));
        let position = add(state.positions[index], scale(velocity, dt));
        finite_vec2(velocity, "path spring velocity")?;
        finite_vec2(position, "path spring position")?;
        state.velocities[index] = velocity;
        state.positions[index] = position;
    }
    Ok(())
}

fn extract_angles_and_lengths(points: &[Vec2]) -> (Vec<f32>, Vec<f32>) {
    let mut angles = Vec::with_capacity(points.len().saturating_sub(1));
    let mut lengths = Vec::with_capacity(points.len().saturating_sub(1));
    for segment in points.windows(2) {
        let vector = sub(segment[1], segment[0]);
        angles.push(vector[0].atan2(-vector[1]));
        lengths.push(length_of(vector));
    }
    (angles, lengths)
}

fn calculate_positions(base: Vec2, angles: &[f32], lengths: &[f32]) -> Vec<Vec2> {
    let mut positions = vec![base];
    let mut x = base[0];
    let mut y = base[1];
    for (angle, length) in angles.iter().zip(lengths) {
        x += *length * angle.sin();
        y -= *length * angle.cos();
        positions.push([x, y]);
    }
    positions
}

fn for_substeps(dt: f32, mut step: impl FnMut(f32) -> Result<(), String>) -> Result<(), String> {
    let mut remaining = dt.min(DT_CAP);
    while remaining > SUBSTEP {
        step(SUBSTEP)?;
        remaining -= SUBSTEP;
    }
    step(remaining)
}

fn rk4<const N: usize>(
    state: &mut [f32; N],
    dt: f32,
    mut derivative: impl FnMut(&[f32; N]) -> [f32; N],
) -> Result<(), String> {
    let current = *state;
    let k1 = derivative(&current);
    let second = std::array::from_fn(|index| current[index] + dt * k1[index] / 2.0);
    let k2 = derivative(&second);
    let third = std::array::from_fn(|index| current[index] + dt * k2[index] / 2.0);
    let k3 = derivative(&third);
    let fourth = std::array::from_fn(|index| current[index] + dt * k3[index]);
    let k4 = derivative(&fourth);
    let result = std::array::from_fn(|index| {
        current[index] + dt * (k1[index] + 2.0 * k2[index] + 2.0 * k3[index] + k4[index]) / 6.0
    });
    if result.iter().any(|value| !value.is_finite()) {
        return Err("physics RK4 state is not finite".into());
    }
    *state = result;
    Ok(())
}

fn map_mode(node: &Value, id: u32) -> Result<MapMode, String> {
    match node
        .get("map_mode")
        .and_then(Value::as_str)
        .unwrap_or("AngleLength")
    {
        "AngleLength" => Ok(MapMode::AngleLength),
        "XY" => Ok(MapMode::XY),
        "LengthAngle" => Ok(MapMode::LengthAngle),
        "YX" => Ok(MapMode::YX),
        other => Err(format!(
            "SimplePhysics node {id} map_mode {other:?} is unsupported"
        )),
    }
}

fn transform_component(
    node: &Value,
    key: &str,
    index: usize,
    fallback: f32,
    id: u32,
) -> Result<f32, String> {
    let Some(transform) = node.get("transform") else {
        return Ok(fallback);
    };
    let array = transform
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("node {id}.transform.{key} must be an array"))?;
    match array.get(index) {
        None => Ok(fallback),
        Some(value) => value_f32(value, &format!("node {id}.transform.{key}[{index}]")),
    }
}

fn field_f32(object: Option<&Value>, key: &str, fallback: f32, path: &str) -> Result<f32, String> {
    match object.and_then(|value| value.get(key)) {
        None => Ok(fallback),
        Some(value) => value_f32(value, path),
    }
}

fn json_vec2(value: Option<&Value>, fallback: Vec2, path: &str) -> Result<Vec2, String> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{path} must be a two-number array"))?;
    if array.len() != 2 {
        return Err(format!("{path} must be a two-number array"));
    }
    Ok([value_f32(&array[0], path)?, value_f32(&array[1], path)?])
}

fn value_f32(value: &Value, path: &str) -> Result<f32, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{path} must be a finite number"))?;
    finite_f32(number as f32, path)
}

fn finite_f32(value: f32, path: &str) -> Result<f32, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{path} must fit in a finite f32"))
    }
}

fn finite_vec2(value: Vec2, path: &str) -> Result<Vec2, String> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(value)
    } else {
        Err(format!("{path} is not finite"))
    }
}

fn offsets_to_f32(offsets: &[[f64; 2]], id: u32) -> Result<Vec<Vec2>, String> {
    offsets
        .iter()
        .enumerate()
        .map(|(index, offset)| {
            finite_vec2(
                [offset[0] as f32, offset[1] as f32],
                &format!("PathDeformer node {id} deformation[{index}]"),
            )
        })
        .collect()
}

fn length_of(vector: Vec2) -> f32 {
    (vector[0] * vector[0] + vector[1] * vector[1]).sqrt()
}

fn add(left: Vec2, right: Vec2) -> Vec2 {
    [left[0] + right[0], left[1] + right[1]]
}

fn sub(left: Vec2, right: Vec2) -> Vec2 {
    [left[0] - right[0], left[1] - right[1]]
}

fn scale(vector: Vec2, amount: f32) -> Vec2 {
    [vector[0] * amount, vector[1] * amount]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> Value {
        json!({
            "physics": {"gravity": 1.0, "pixelsPerMeter": 1.0},
            "param": [{"uuid": 7, "defaults": [0.0, 0.0], "is_vec2": false, "min": [0.0,0.0], "max": [1.0,1.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": []}],
            "nodes": {"uuid": 1, "type": "Node", "transform": {"trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0]}, "children": [
                {"uuid": 2, "type": "SimplePhysics", "param": 7, "model_type": "Pendulum", "map_mode": "AngleLength", "gravity": 1.0, "length": 10.0, "frequency": 1.0, "angle_damping": 0.5, "length_damping": 0.5, "output_scale": [1.0,1.0], "local_only": false, "transform": {"trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0]}, "children": []}
            ]}
        })
    }

    fn root_transform_parameter_model(
        channel: &str,
        amount: f64,
        lock_to_root: bool,
        locked_parent: bool,
    ) -> Value {
        let mut model = model();
        model["param"][0]["bindings"] = json!([{
            "node": 1,
            "param_name": channel,
            "values": [[0.0], [amount]],
            "interpolate_mode": "Linear"
        }]);
        let driver = &mut model["nodes"]["children"][0];
        driver["lockToRoot"] = json!(lock_to_root);
        if locked_parent {
            let driver = driver.take();
            model["nodes"]["children"] = json!([{
                "uuid": 3,
                "type": "Node",
                "lockToRoot": true,
                "transform": {
                    "trans": [0.0, 0.0, 0.0],
                    "rot": [0.0, 0.0, 0.0],
                    "scale": [1.0, 1.0]
                },
                "children": [driver]
            }]);
        }
        model
    }

    fn assert_parameter_output(actual: [f64; 2], expected: [f64; 2]) {
        assert!((actual[0] - expected[0]).abs() < 1e-6, "{actual:?} != {expected:?}");
        assert!((actual[1] - expected[1]).abs() < 1e-6, "{actual:?} != {expected:?}");
    }

    fn moved_model() -> Value {
        let mut model = model();
        model["nodes"]["children"][0]["transform"]["trans"] = json!([1.0, 0.0, 0.0]);
        model
    }

    fn path_pendulum_model(parent_rotation: f64, physics_only: bool) -> Value {
        json!({
            "physics": {},
            "param": [],
            "nodes": {"uuid":1,"type":"Node","enabled":true,"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[
                {"uuid":2,"type":"Node","enabled":true,"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,parent_rotation],"scale":[1.0,1.0]},"children":[
                    {"uuid":3,"type":"PathDeformer","enabled":true,"vertices":[0.0,0.0,0.0,10.0],"physics_only":physics_only,"physics":{"type":"Pendulum","damping":0.0,"restore_constant":0.0,"gravity":1.0,"input_scale":0.0,"propagate_scale":0.0},"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]}
                ]}
            ]}
        })
    }

    #[test]
    fn simple_pendulum_matches_fixed_rk4_step() {
        let mut runtime = Runtime::default();
        let _ = runtime
            .step(&model(), &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let result = runtime
            .step(&moved_model(), &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        let angle = result.parameter_values[&7][0] * std::f64::consts::PI;
        // Independent RK4 evaluation for theta=atan(0.1), g/L=0.1, damping=.5, h=.01.
        assert!((angle - 0.09966815).abs() < 2e-6, "{angle}");
        assert!((result.parameter_values[&7][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn simple_physics_lock_to_root_uses_unparameterized_root_rotation() {
        let rotation = std::f64::consts::FRAC_PI_2;
        for (lock_to_root, locked_parent, expected) in [
            (true, false, [0.0, 1.0]),
            (false, true, [0.0, 1.0]),
            (false, false, [-0.5, 1.0]),
        ] {
            let model = root_transform_parameter_model(
                "transform.r.z",
                rotation,
                lock_to_root,
                locked_parent,
            );
            let initial_values = BTreeMap::from([(7, [0.0, 0.0])]);
            let rotated_values = BTreeMap::from([(7, [1.0, 0.0])]);
            let evaluated = crate::evaluation::evaluate(&model, &rotated_values).unwrap();
            assert!(
                (evaluated["nodes"]["transform"]["rot"][2]
                    .as_f64()
                    .unwrap()
                    - rotation)
                    .abs()
                    < 1e-9
            );

            let mut runtime = Runtime::default();
            let initialized = runtime
                .step(&model, &initial_values, &BTreeMap::new(), 0.0)
                .unwrap();
            assert_parameter_output(initialized.parameter_values[&7], [0.0, 1.0]);
            let rotated = runtime
                .step(&model, &rotated_values, &BTreeMap::new(), 0.0)
                .unwrap();
            assert_parameter_output(rotated.parameter_values[&7], expected);
        }
    }

    #[test]
    fn simple_physics_lock_to_root_ignores_parameterized_root_translation() {
        let model = root_transform_parameter_model("transform.t.x", 3.0, true, false);
        let initial_values = BTreeMap::from([(7, [0.0, 0.0])]);
        let translated_values = BTreeMap::from([(7, [1.0, 0.0])]);
        let evaluated = crate::evaluation::evaluate(&model, &translated_values).unwrap();
        assert_eq!(evaluated["nodes"]["transform"]["trans"][0], json!(3.0));

        let mut runtime = Runtime::default();
        let initialized = runtime
            .step(&model, &initial_values, &BTreeMap::new(), 0.0)
            .unwrap();
        let translated = runtime
            .step(&model, &translated_values, &BTreeMap::new(), 0.0)
            .unwrap();
        assert_parameter_output(initialized.parameter_values[&7], [0.0, 1.0]);
        assert_parameter_output(translated.parameter_values[&7], [0.0, 1.0]);
    }

    #[test]
    fn dt_partition_and_damping_are_stable() {
        let mut whole = Runtime::default();
        let mut partitioned = Runtime::default();
        let model = model();
        let moved = moved_model();
        let _ = whole
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let _ = partitioned
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let whole = whole
            .step(&moved, &BTreeMap::new(), &BTreeMap::new(), 0.02)
            .unwrap();
        let _ = partitioned
            .step(&moved, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        let partitioned = partitioned
            .step(&moved, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        assert!((whole.parameter_values[&7][0] - partitioned.parameter_values[&7][0]).abs() < 2e-5);

        let mut base = Runtime::default();
        let _ = base
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        if let Some(State::SimplePendulum(state)) = base.states.get_mut(&2) {
            let angle = 0.2f32;
            state.bob = [-angle.sin() * 10.0, angle.cos() * 10.0];
            state.angular_velocity = 1.0;
        } else {
            unreachable!();
        }
        let mut damped_model = model.clone();
        damped_model["nodes"]["children"][0]["angle_damping"] = json!(1.0);
        let mut undamped_model = model.clone();
        undamped_model["nodes"]["children"][0]["angle_damping"] = json!(0.0);
        let mut damped = base.clone();
        let mut undamped = base;
        let _ = damped
            .step(&damped_model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        let _ = undamped
            .step(&undamped_model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        let velocity = |runtime: &Runtime| match runtime.states.get(&2) {
            Some(State::SimplePendulum(state)) => state.angular_velocity,
            _ => unreachable!(),
        };
        assert!(velocity(&damped) < velocity(&undamped));
    }

    #[test]
    fn equilibrium_and_invalid_steps_are_transactional() {
        let mut equilibrium_model = model();
        equilibrium_model["nodes"]["children"][0]["transform"]["trans"] = json!([0.0, 0.0, 0.0]);
        let mut runtime = Runtime::default();
        let equilibrium = runtime
            .step(&equilibrium_model, &BTreeMap::new(), &BTreeMap::new(), 0.5)
            .unwrap();
        assert!(equilibrium.parameter_values[&7][0].abs() < 1e-7);
        assert!((equilibrium.parameter_values[&7][1] - 1.0).abs() < 1e-7);

        let before = runtime.clone();
        assert!(runtime
            .step(&equilibrium_model, &BTreeMap::new(), &BTreeMap::new(), -0.1)
            .is_err());
        assert_eq!(runtime, before);
        let mut invalid_values = BTreeMap::new();
        invalid_values.insert(7, [f64::NAN, 0.0]);
        assert!(runtime
            .step(&equilibrium_model, &invalid_values, &BTreeMap::new(), 0.01)
            .is_err());
        assert_eq!(runtime, before);
    }

    #[test]
    fn path_spring_default_is_stable_at_rest_and_includes_input_offsets() {
        let path_model = json!({
            "physics": {},
            "param": [],
            "nodes": {"uuid":1,"type":"Node","transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[
                {"uuid":3,"type":"PathDeformer","vertices":[0.0,0.0,0.0,10.0],"physics":{"type":"SpringPendulum"},"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]}
            ]}
        });
        let input = BTreeMap::from([(3, vec![[2.0, 3.0], [0.0, 0.0]])]);
        let mut runtime = Runtime::default();
        let result = runtime
            .step(&path_model, &BTreeMap::new(), &input, 0.02)
            .unwrap();
        assert_eq!(result.path_deformations[&3], input[&3]);
    }

    #[test]
    fn path_pendulum_uses_composed_parent_rotation() {
        let model = path_pendulum_model(1.0, false);
        let mut rotated = Runtime::default();
        let mut unrotated = Runtime::default();
        let _ = rotated
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let _ = unrotated
            .step(
                &path_pendulum_model(0.0, false),
                &BTreeMap::new(),
                &BTreeMap::new(),
                0.0,
            )
            .unwrap();
        let rotated = rotated
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        let unrotated = unrotated
            .step(
                &path_pendulum_model(0.0, false),
                &BTreeMap::new(),
                &BTreeMap::new(),
                0.01,
            )
            .unwrap();
        assert!(rotated.path_deformations[&3][1][0] > 0.0005);
        assert!(unrotated.path_deformations[&3][1][0].abs() < 1e-8);
    }

    #[test]
    fn simple_outputs_are_evaluated_before_path_drivers() {
        let mut model = path_pendulum_model(0.0, false);
        model["param"] = json!([{
            "uuid":7,"defaults":[0.0,0.0],"is_vec2":false,"min":[0.0,0.0],"max":[1.0,1.0],"axis_points":[[0.0,1.0],[0.0]],
            "bindings":[{"node":3,"param_name":"transform.r.z","values":[[0.0],[1.0]],"interpolate_mode":"Linear"}]
        }]);
        let simple = json!({
            "uuid":4,"type":"SimplePhysics","enabled":true,"param":7,"model_type":"Pendulum","map_mode":"AngleLength","gravity":1.0,"length":10.0,"frequency":1.0,"angle_damping":0.5,"length_damping":0.5,"output_scale":[1.0,1.0],"local_only":false,
            "transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]
        });
        // The path is first in traversal order; SimplePhysics must still run first.
        model["nodes"]["children"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(simple);
        let mut runtime = Runtime::default();
        let _ = runtime
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        model["nodes"]["children"][0]["children"][1]["transform"]["trans"] = json!([1.0, 0.0, 0.0]);
        let result = runtime
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        assert!(result.parameter_values[&7][0] > 0.0);
        assert!(result.path_deformations[&3][1][0] > 1e-5);
    }

    #[test]
    fn disabled_ancestor_disables_descendant_physics() {
        let mut model = path_pendulum_model(0.0, false);
        model["nodes"]["children"][0]["enabled"] = json!(false);
        let mut runtime = Runtime::default();
        let result = runtime
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .unwrap();
        assert!(result.path_deformations.is_empty());
        assert!(runtime.states.is_empty());
    }

    #[test]
    fn path_physics_only_preserves_external_offsets_on_initialization() {
        let model = path_pendulum_model(0.0, true);
        let input = BTreeMap::from([(3, vec![[2.0, 3.0], [4.0, 5.0]])]);
        let mut runtime = Runtime::default();
        let result = runtime
            .step(&model, &BTreeMap::new(), &input, 0.01)
            .unwrap();
        assert_eq!(result.path_deformations[&3], input[&3]);
    }

    #[test]
    fn path_physics_consumes_ancestor_grid_once_before_filtering_children() {
        let mut model = path_pendulum_model(0.0, false);
        let grid = &mut model["nodes"]["children"][0];
        grid["type"] = json!("GridDeformer");
        grid["grid_axis_x"] = json!([-10.0, 10.0]);
        grid["grid_axis_y"] = json!([-10.0, 20.0]);
        grid["dynamic"] = json!(false);
        grid["children"][0]["curve_type"] = json!("Bezier");
        grid["children"][0]["children"] = json!([{
            "uuid": 4, "type": "Part", "transform": {
                "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]
            }, "mesh": {
                "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "indices": [0, 1, 2]
            }, "children": []
        }]);
        let inputs = BTreeMap::from([(2, vec![[3.0, 1.0]; 4])]);
        let mut runtime = Runtime::default();
        let step = runtime.step(&model, &BTreeMap::new(), &inputs, 0.0).unwrap();
        let scene = crate::scene_deform::evaluate_with_path_update(
            &model, &model["nodes"], &inputs,
            &mut |id, _, _, _, _| Ok(step.path_preprocess.get(&id).cloned()),
        ).unwrap();
        for offsets in [&step.path_deformations[&3], &scene.local_offsets[&4]] {
            for offset in offsets {
                assert!((offset[0] - 3.0).abs() < 1e-4, "{offset:?}");
                assert!((offset[1] - 1.0).abs() < 1e-4, "{offset:?}");
            }
        }
    }

    #[test]
    fn path_spring_moves_after_root_motion() {
        let path_model = json!({
            "physics": {},
            "param": [],
            "nodes": {"uuid":1,"type":"Node","transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[
                {"uuid":3,"type":"PathDeformer","vertices":[0.0,0.0,0.0,10.0],"physics":{"type":"SpringPendulum"},"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]}
            ]}
        });
        let mut runtime = Runtime::default();
        let _ = runtime
            .step(&path_model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let input = BTreeMap::from([(3, vec![[100.0, 0.0], [0.0, 0.0]])]);
        let result = runtime
            .step(&path_model, &BTreeMap::new(), &input, 0.01)
            .unwrap();
        assert!(result.path_deformations[&3][1][0] > 1e-6);
    }

    #[test]
    fn path_geometry_or_settings_changes_reinitialize_cached_state() {
        let mut model = path_pendulum_model(0.0, false);
        let mut runtime = Runtime::default();
        let _ = runtime
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        if let Some(State::PathPendulum(state)) = runtime.states.get_mut(&3) {
            state.angular_velocities[0] = 2.0;
        } else {
            unreachable!();
        }
        model["nodes"]["children"][0]["children"][0]["vertices"] = json!([0.0, 0.0, 10.0, 10.0]);
        model["nodes"]["children"][0]["children"][0]["physics"]["gravity"] = json!(2.0);
        let _ = runtime
            .step(&model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        let Some(State::PathPendulum(state)) = runtime.states.get(&3) else {
            unreachable!();
        };
        assert_eq!(state.shape, vec![[0.0, 0.0], [10.0, 10.0]]);
        assert_eq!(state.gravity, 2.0);
        assert_eq!(state.angular_velocities, vec![0.0]);
        assert_ne!(state.initial_angles, vec![0.0]);
    }

    #[test]
    fn overflowed_path_lengths_and_combined_offsets_fail_transactionally() {
        let overflow_model = json!({
            "physics": {},
            "param": [],
            "nodes": {"uuid":1,"type":"Node","transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[
                {"uuid":3,"type":"PathDeformer","vertices":[0.0,0.0,1e20,0.0],"physics":{"type":"SpringPendulum"},"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]}
            ]}
        });
        let mut runtime = Runtime::default();
        let before = runtime.clone();
        assert!(runtime
            .step(&overflow_model, &BTreeMap::new(), &BTreeMap::new(), 0.01)
            .is_err());
        assert_eq!(runtime, before);

        let spring_model = json!({
            "physics": {},
            "param": [],
            "nodes": {"uuid":1,"type":"Node","transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[
                {"uuid":3,"type":"PathDeformer","vertices":[0.0,0.0,0.0,10.0],"physics":{"type":"SpringPendulum"},"transform":{"trans":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"scale":[1.0,1.0]},"children":[]}
            ]}
        });
        let _ = runtime
            .step(&spring_model, &BTreeMap::new(), &BTreeMap::new(), 0.0)
            .unwrap();
        if let Some(State::PathSpring(state)) = runtime.states.get_mut(&3) {
            state.velocities[1][0] = f32::MAX * 0.999;
        } else {
            unreachable!();
        }
        let before = runtime.clone();
        let large = f64::from(f32::MAX * 0.999);
        let input = BTreeMap::from([(3, vec![[0.0, 0.0], [large, 0.0]])]);
        assert!(runtime
            .step(&spring_model, &BTreeMap::new(), &input, 0.01)
            .is_err());
        assert_eq!(runtime, before);
    }
}
