//! `animation.physics.bake` (ANI25): bake the outputs of the enabled SimplePhysics drivers of an INX
//! animation into parameter keys, the way the Cubism Editor manual's physics-operation bake does
//! (https://docs.live2d.com/en/cubism-editor-manual/physics-operation-bake/). With `warmup` on, the whole clip
//! plays once to settle the physics state, then the clip replays from frame 0 through `to` and the outputs on
//! `from..=to` are keyed; every step advances by the clip's timestep, so the replay continues the loop
//! cyclically. With `warmup` off the runtime starts fresh on frame 0 with `dt` 0 and advances by the timestep
//! from there. A PathDeformer driver is not baked in this slice and refuses the call, as does anything that
//! could stop the written keys from drawing the simulated values: frozen or merged lanes, frame stepping that
//! remaps an output parameter, an eye-blink track over an output, form edits or form timing, and an
//! `animationWeight` other than 1 or an additive clip. The simulation runs on a private runtime and a model
//! clone, so a refusal leaves the document, its history and the live physics state untouched.

use crate::{animation, evaluation, frame_step, physics_runtime, required_string, valid_id, Session};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BAKE_STEPS: u64 = 4000;
const MAX_BAKE_KEYS: u64 = 200_000;
const MAX_BAKE_TIMESTEP: f64 = 10.0;

struct Output {
    id: u32,
    axes: usize,
    min: [f64; 2],
    max: [f64; 2],
}

impl Session {
    /// `animation.physics.bake {name, parameters?, from?, to?, warmup?}`: keys the simulated values of the
    /// enabled SimplePhysics outputs on the whole frames `from..=to` of clip `name`, in one undo step.
    pub(crate) fn op_animation_physics_bake(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.physics.bake")?;
        let name = required_string(request, "name")?;
        let model = &self.document.model;
        let animation = model["animations"]
            .get(name)
            .ok_or_else(|| format!("animation {name:?} was not found"))?;
        let length = animation::validate_animation_header(animation)?;
        frame_step::validate(model, animation, length)?;
        if animation::freeze_frame(animation).is_some() {
            return Err("physics cannot be baked while freeze is on; turn freeze off first".into());
        }
        if animation.get("additive") == Some(&json!(true)) {
            return Err("physics cannot be baked into an additive animation".into());
        }
        let weight = animation.get("animationWeight").and_then(Value::as_f64).unwrap_or(1.0);
        if weight != 1.0 {
            return Err("physics cannot be baked into an animation whose animationWeight is not 1".into());
        }
        if animation.get("forms").and_then(Value::as_array).is_some_and(|forms| !forms.is_empty())
            || animation.get("formTime").is_some_and(|time| !time.is_null())
        {
            return Err("physics cannot be baked into an animation with form edits or form timing".into());
        }
        let timestep = animation["timestep"].as_f64().ok_or("animation timestep is missing")?;
        if timestep > MAX_BAKE_TIMESTEP {
            return Err(format!(
                "animation timestep {timestep} exceeds the physics step limit of {MAX_BAKE_TIMESTEP} seconds"
            ));
        }
        let warmup = match request.get("warmup") {
            None | Some(Value::Null) => true,
            Some(value) => value.as_bool().ok_or("warmup must be a boolean")?,
        };
        let from = if request.get("from").is_some_and(|value| !value.is_null()) {
            animation::frame_field(request, "from", length)?
        } else {
            0
        };
        let to = if request.get("to").is_some_and(|value| !value.is_null()) {
            animation::frame_field(request, "to", length)?
        } else {
            (length - 1) as i32
        };
        if from > to {
            return Err("from must not be after to".into());
        }

        let (outputs, has_path) = physics_runtime::simple_outputs(model)?;
        if has_path {
            return Err("baking a PathDeformer driver is not supported; the clip keeps live physics".into());
        }
        if outputs.is_empty() {
            return Err("the model has no enabled SimplePhysics node with an assigned parameter".into());
        }
        let driven: BTreeSet<u32> = outputs.iter().map(|(_, id)| *id).collect();
        let ids: Vec<u32> = match request.get("parameters").filter(|value| !value.is_null()) {
            None => outputs.iter().map(|(_, id)| *id).collect(),
            Some(value) => {
                let list = value.as_array().ok_or("parameters must be an array of parameter UUIDs")?;
                if list.is_empty() {
                    return Err("parameters must not be empty".into());
                }
                let mut ids = Vec::with_capacity(list.len());
                for entry in list {
                    let id = entry
                        .as_u64()
                        .and_then(valid_id)
                        .ok_or("parameters must be parameter UUIDs")?;
                    if ids.contains(&id) {
                        return Err(format!("parameter {id} is listed twice"));
                    }
                    ids.push(id);
                }
                ids
            }
        };
        let mut selected = Vec::with_capacity(ids.len());
        for id in ids {
            let parameter = crate::find_parameter(model, id)
                .ok_or_else(|| format!("parameter {id} was not found"))?;
            if parameter.get("groupUUID").is_some() {
                return Err(format!("parameter {id} is a parameter group"));
            }
            if !driven.contains(&id) {
                return Err(format!(
                    "parameter {id} is not the output of an enabled SimplePhysics node"
                ));
            }
            let is_vec2 = match parameter.get("is_vec2") {
                None => false,
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| format!("parameter {id}.is_vec2 must be a boolean"))?,
            };
            let axes = if is_vec2 { 2 } else { 1 };
            let bound = |key: &str| -> Result<[f64; 2], String> {
                let mut bounds = [0.0; 2];
                for (axis, bound) in bounds.iter_mut().enumerate().take(axes) {
                    *bound = parameter
                        .get(key)
                        .and_then(|list| list.get(axis))
                        .and_then(Value::as_f64)
                        .ok_or_else(|| format!("parameter {id}.{key} axis {axis} is missing"))?;
                }
                Ok(bounds)
            };
            selected.push(Output { id, axes, min: bound("min")?, max: bound("max")? });
        }

        let steps = (u64::from(warmup as u8) * length as u64)
            .checked_add(u64::from(to as u32) + 1)
            .ok_or("the bake would take too many physics steps")?;
        if steps > MAX_BAKE_STEPS {
            return Err(format!(
                "baking would take {steps} physics steps; the limit is {MAX_BAKE_STEPS}"
            ));
        }
        let frames = u64::from((to - from) as u32) + 1;
        let axes: u64 = selected.iter().map(|output| output.axes as u64).sum();
        let keys = frames.checked_mul(axes).ok_or("the bake would write too many keys")?;
        if keys > MAX_BAKE_KEYS {
            return Err(format!("baking would write {keys} keys; the limit is {MAX_BAKE_KEYS}"));
        }

        let lanes = match animation.get("lanes") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value.as_array().ok_or("animation.lanes must be an array")?,
        };
        for output in &selected {
            let mut seen = BTreeSet::new();
            for lane in lanes
                .iter()
                .filter(|lane| lane["uuid"].as_u64() == Some(u64::from(output.id)))
            {
                let axis = lane
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(|axis| usize::try_from(axis).ok())
                    .filter(|axis| *axis < output.axes)
                    .ok_or_else(|| {
                        format!("a lane of parameter {} has an axis the parameter does not have", output.id)
                    })?;
                if !seen.insert(axis) {
                    return Err(format!(
                        "parameter {} has two lanes on axis {axis}; the bake would overwrite one", output.id
                    ));
                }
                let linear = lane
                    .get("interpolation")
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("linear"));
                let forced = lane
                    .get("merge_mode")
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("forced"));
                if !linear || !forced {
                    return Err(format!(
                        "the lane of parameter {} axis {axis} must be Linear and Forced so baked keys draw",
                        output.id
                    ));
                }
            }
        }
        let blink = animation
            .get("eyeBlink")
            .and_then(|track| track.get("keyframes"))
            .and_then(Value::as_array)
            .is_some_and(|keys| !keys.is_empty());
        if blink {
            let eyes: BTreeSet<u32> = animation::eye_parameters(model).collect();
            for output in &selected {
                if eyes.contains(&output.id) {
                    return Err(format!(
                        "parameter {} is an eye parameter and the clip has blink keys, so its baked keys \
                         would not draw as simulated",
                        output.id
                    ));
                }
            }
        }
        for output in &selected {
            for frame in from..=to {
                let remapped =
                    frame_step::parameter_frame(model, animation, output.id, length, f64::from(frame))?;
                if remapped != f64::from(frame) {
                    return Err(format!(
                        "frame stepping shows frame {remapped} for parameter {} on frame {frame}, so its \
                         baked keys would not draw as simulated",
                        output.id
                    ));
                }
            }
        }

        let base = self.runtime_values.clone();
        let mut runtime = physics_runtime::Runtime::default();
        let mut keyed: BTreeMap<(u32, usize), Vec<(i32, f32)>> = BTreeMap::new();
        let mut step = |frame: i32, dt: f64, record: bool| -> Result<(), String> {
            let inputs = animation::sample(model, name, f64::from(frame), &base, true)?;
            let deformations = evaluation::raster_deformations(model, &inputs)?;
            let stepped = runtime.step(model, &inputs, &deformations, dt)?;
            if !record {
                return Ok(());
            }
            for output in &selected {
                let value = stepped.parameter_values.get(&output.id).ok_or_else(|| {
                    format!("parameter {} has no value in the simulation", output.id)
                })?;
                for axis in 0..output.axes {
                    let value = value[axis];
                    if !value.is_finite() || !(value as f32).is_finite() {
                        return Err(format!(
                            "the physics output for parameter {} axis {axis} is not a finite f32 on \
                             frame {frame}",
                            output.id
                        ));
                    }
                    if value < output.min[axis] || value > output.max[axis] {
                        return Err(format!(
                            "the physics output {value} for parameter {} axis {axis} is outside its range \
                             [{}, {}] on frame {frame}",
                            output.id, output.min[axis], output.max[axis]
                        ));
                    }
                    keyed.entry((output.id, axis)).or_default().push((frame, value as f32));
                }
            }
            Ok(())
        };
        if warmup {
            for frame in 0..length as i32 {
                step(frame, timestep, false)?;
            }
        }
        for frame in 0..=to {
            let dt = if warmup || frame != 0 { timestep } else { 0.0 };
            step(frame, dt, frame >= from)?;
        }

        let mut candidate = model.clone();
        let mut written = 0_u64;
        {
            let animation = animation::animation_mut(&mut candidate, name)?;
            for ((id, axis), keys) in &keyed {
                for (frame, value) in keys {
                    animation::put_key(animation, *id, *axis, *frame, *value, length)?;
                    written += 1;
                }
            }
        }
        for frame in from..=to {
            let sampled = animation::sample(&candidate, name, f64::from(frame), &base, true)?;
            for output in &selected {
                let drawn = sampled.get(&output.id).ok_or_else(|| {
                    format!("the baked animation has no value for parameter {}", output.id)
                })?;
                for axis in 0..output.axes {
                    let expected = f64::from(keyed[&(output.id, axis)][(frame - from) as usize].1);
                    if (drawn[axis] - expected).abs() > 1e-6 * expected.abs().max(1.0) {
                        return Err(format!(
                            "the baked lane of parameter {} axis {axis} draws {} on frame {frame} instead \
                             of {expected}; held frames, a blink track or another lane may rescale it",
                            output.id,
                            drawn[axis]
                        ));
                    }
                }
            }
        }
        self.commit_model(candidate)?;
        Ok(json!({
            "name": name,
            "from": from,
            "to": to,
            "frames": frames,
            "keys": written,
            "parameters": selected.iter().map(|output| output.id).collect::<Vec<_>>(),
            "warmup": warmup,
        }))
    }
}
