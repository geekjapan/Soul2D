use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Axis {
    X,
    Y,
    Angle,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Range {
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) default: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct Input {
    pub(crate) id: String,
    pub(crate) axis: Axis,
    pub(crate) weight: f32,
    pub(crate) reflect: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Output {
    pub(crate) id: String,
    pub(crate) axis: Axis,
    pub(crate) weight: f32,
    pub(crate) reflect: bool,
    pub(crate) scale: f32,
    pub(crate) vertex: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Particle {
    pub(crate) mobility: f32,
    pub(crate) delay: f32,
    pub(crate) acceleration: f32,
    pub(crate) radius: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub(crate) id: String,
    pub(crate) inputs: Vec<Input>,
    pub(crate) outputs: Vec<Output>,
    pub(crate) particles: Vec<Particle>,
    pub(crate) position: Range,
    pub(crate) angle: Range,
}

#[derive(Clone, Debug)]
pub(crate) struct Rig {
    pub(crate) groups: Vec<Group>,
    pub(crate) fps: f32,
    pub(crate) parameters: BTreeMap<String, Range>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ForceOptions {
    pub(crate) gravity: [f32; 2],
    pub(crate) wind: [f32; 2],
}

impl Default for ForceOptions {
    fn default() -> Self {
        Self {
            gravity: [0.0, -1.0],
            wind: [0.0, 0.0],
        }
    }
}

impl ForceOptions {
    fn parse(value: &Value) -> Result<Self, String> {
        fn vector(value: &Value, key: &str) -> Result<[f32; 2], String> {
            let object = value
                .get(key)
                .and_then(Value::as_object)
                .ok_or_else(|| format!("physics options {key} must be an object"))?;
            let component = |axis| {
                object
                    .get(axis)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && (*value as f32).is_finite())
                    .map(|value| value as f32)
                    .ok_or_else(|| format!("physics options {key}.{axis} must be a finite f32"))
            };
            Ok([component("x")?, component("y")?])
        }

        Ok(Self {
            gravity: vector(value, "gravity")?,
            wind: vector(value, "wind")?,
        })
    }

    fn snapshot(self) -> Value {
        serde_json::json!({
            "gravity": {"x": self.gravity[0], "y": self.gravity[1]},
            "wind": {"x": self.wind[0], "y": self.wind[1]},
        })
    }
}

fn number(value: &Value, key: &str) -> Result<f32, String> {
    value[key]
        .as_f64()
        .filter(|v| v.is_finite() && (*v as f32).is_finite())
        .map(|v| v as f32)
        .ok_or_else(|| format!("physics {key} must be a finite f32"))
}

fn range(value: &Value, keys: [&str; 3]) -> Result<Range, String> {
    let r = Range {
        min: number(value, keys[0])?,
        max: number(value, keys[1])?,
        default: number(value, keys[2])?,
    };
    if r.min > r.default || r.default > r.max {
        return Err("physics range is reversed or excludes default".into());
    }
    Ok(r)
}

fn axis(value: &Value) -> Result<Axis, String> {
    match value["Type"].as_str() {
        Some("X") => Ok(Axis::X),
        Some("Y") => Ok(Axis::Y),
        Some("Angle") => Ok(Axis::Angle),
        _ => Err("physics Type must be X, Y or Angle".into()),
    }
}

fn weight(value: &Value) -> Result<f32, String> {
    let weight = number(value, "Weight")?;
    if !(0.0..=100.0).contains(&weight) {
        return Err("physics Weight must be between 0 and 100".into());
    }
    Ok(weight / 100.0)
}

fn reflect(value: &Value) -> Result<bool, String> {
    value["Reflect"]
        .as_bool()
        .ok_or_else(|| "physics Reflect must be a boolean".into())
}

fn parameter_id(
    value: &Value,
    key: &str,
    parameters: &BTreeMap<String, Range>,
) -> Result<String, String> {
    let target = &value[key];
    if target["Target"] != "Parameter" {
        return Err("physics target must be Parameter".into());
    }
    let id = target["Id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 1024)
        .ok_or("physics parameter Id must be nonempty and at most 1024 bytes")?;
    if !parameters.contains_key(id) {
        return Err(format!("physics references unknown parameter {id:?}"));
    }
    Ok(id.to_owned())
}

fn array<'a>(
    value: &'a Value,
    key: &str,
    min: usize,
    max: usize,
) -> Result<&'a Vec<Value>, String> {
    value[key]
        .as_array()
        .filter(|a| a.len() >= min && a.len() <= max)
        .ok_or_else(|| format!("physics {key} must contain {min} to {max} entries"))
}

impl Rig {
    pub(crate) fn parse(document: &Value, source: &Value) -> Result<Self, String> {
        if document["Version"] != 3 {
            return Err("physics Version must be 3".into());
        }
        let mut parameters = BTreeMap::new();
        for parameter in array(source, "parameters", 1, 4096)? {
            let id = parameter["id"]
                .as_str()
                .ok_or("native parameter id is missing")?;
            if parameters
                .insert(
                    id.to_owned(),
                    range(parameter, ["minimum", "maximum", "default"])?,
                )
                .is_some()
            {
                return Err("native parameter id is duplicated".into());
            }
        }
        let meta = &document["Meta"];
        let fps = if meta.get("Fps").is_some() {
            number(meta, "Fps")?
        } else {
            0.0
        };
        if !(0.0..=240.0).contains(&fps) || (fps > 0.0 && fps < 1.0) {
            return Err("physics Fps must be zero or between 1 and 240".into());
        }
        for force in ["Gravity", "Wind"] {
            number(&meta["EffectiveForces"][force], "X")?;
            number(&meta["EffectiveForces"][force], "Y")?;
        }
        let mut groups = Vec::new();
        let mut ids = BTreeSet::new();
        let mut counts = [0usize; 3];
        for value in array(document, "PhysicsSettings", 1, 256)? {
            let id = value["Id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 1024)
                .ok_or("physics setting Id must be nonempty and at most 1024 bytes")?;
            if !ids.insert(id) {
                return Err("physics setting Id is duplicated".into());
            }
            let mut particles = Vec::new();
            for particle in array(value, "Vertices", 2, 1024)? {
                number(&particle["Position"], "X")?;
                number(&particle["Position"], "Y")?;
                let p = Particle {
                    mobility: number(particle, "Mobility")?,
                    delay: number(particle, "Delay")?,
                    acceleration: number(particle, "Acceleration")?,
                    radius: number(particle, "Radius")?,
                };
                if !(0.0..=1.0).contains(&p.mobility)
                    || p.delay < 0.0
                    || p.acceleration < 0.0
                    || p.radius < 0.0
                    || (!particles.is_empty() && p.radius == 0.0)
                {
                    return Err(
                        "physics particle has invalid mobility, delay, acceleration or radius"
                            .into(),
                    );
                }
                particles.push(p);
            }
            let inputs = array(value, "Input", 1, 1024)?
                .iter()
                .map(|input| {
                    Ok(Input {
                        id: parameter_id(input, "Source", &parameters)?,
                        axis: axis(input)?,
                        weight: weight(input)?,
                        reflect: reflect(input)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let outputs = array(value, "Output", 1, 1024)?
                .iter()
                .map(|output| {
                    let vertex = output["VertexIndex"]
                        .as_u64()
                        .filter(|v| *v >= 1 && *v < particles.len() as u64)
                        .ok_or("physics output VertexIndex is outside the strand")?
                        as usize;
                    Ok(Output {
                        id: parameter_id(output, "Destination", &parameters)?,
                        axis: axis(output)?,
                        weight: weight(output)?,
                        reflect: reflect(output)?,
                        scale: number(output, "Scale")?,
                        vertex,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            counts[0] += inputs.len();
            counts[1] += outputs.len();
            counts[2] += particles.len();
            if counts.iter().any(|count| *count > 16384) {
                return Err("physics rig exceeds entry budget".into());
            }
            groups.push(Group {
                id: id.to_owned(),
                inputs,
                outputs,
                particles,
                position: range(
                    &value["Normalization"]["Position"],
                    ["Minimum", "Maximum", "Default"],
                )?,
                angle: range(
                    &value["Normalization"]["Angle"],
                    ["Minimum", "Maximum", "Default"],
                )?,
            });
        }
        for (key, actual) in [
            "PhysicsSettingCount",
            "TotalInputCount",
            "TotalOutputCount",
            "VertexCount",
        ]
        .into_iter()
        .zip([groups.len(), counts[0], counts[1], counts[2]])
        {
            if meta[key].as_u64() != Some(actual as u64) {
                return Err(format!("physics Meta.{key} disagrees with entries"));
            }
        }
        Ok(Self {
            groups,
            fps,
            parameters,
        })
    }
}

// The pinned framework uses the range midpoint, not the authored default.
pub(crate) fn normalize(value: f32, parameter: Range, target: Range, reflect: bool) -> f32 {
    let midpoint = parameter.min + (parameter.max - parameter.min) / 2.0;
    let delta = value.clamp(parameter.min, parameter.max) - midpoint;
    let mut result = target.default;
    if delta > 0.0 && parameter.max != midpoint {
        result += delta * (target.max - target.default) / (parameter.max - midpoint);
    } else if delta < 0.0 && parameter.min != midpoint {
        result += delta * (target.min - target.default) / (parameter.min - midpoint);
    }
    if reflect {
        result
    } else {
        -result
    }
}

#[derive(Clone, Debug)]
struct Strand {
    positions: Vec<[f32; 2]>,
    velocities: Vec<[f32; 2]>,
    gravity: Vec<[f32; 2]>,
    previous: Vec<f32>,
    current: Vec<f32>,
}

#[derive(Clone, Debug)]
pub(crate) struct Runtime {
    rig: Rig,
    strands: Vec<Strand>,
    input_cache: Option<BTreeMap<String, f32>>,
    remain: f32,
    options: ForceOptions,
    pub(crate) values: BTreeMap<String, f64>,
}

fn direction_angle(from: [f32; 2], to: [f32; 2]) -> f32 {
    let mut angle = to[1].atan2(to[0]) - from[1].atan2(from[0]);
    while angle < -std::f32::consts::PI {
        angle += std::f32::consts::TAU;
    }
    while angle > std::f32::consts::PI {
        angle -= std::f32::consts::TAU;
    }
    angle
}

fn unit(value: [f32; 2]) -> Result<[f32; 2], String> {
    let length = value[0].hypot(value[1]);
    if !length.is_finite() || length == 0.0 {
        return Err("physics strand direction is degenerate".into());
    }
    Ok([value[0] / length, value[1] / length])
}

fn output_value(strand: &Strand, output: &Output, gravity: [f32; 2]) -> f32 {
    let index = output.vertex;
    let translation = [
        strand.positions[index][0] - strand.positions[index - 1][0],
        strand.positions[index][1] - strand.positions[index - 1][1],
    ];
    let value = match output.axis {
        Axis::X => translation[0],
        Axis::Y => translation[1],
        Axis::Angle => {
            let parent = if index >= 2 {
                [
                    strand.positions[index - 1][0] - strand.positions[index - 2][0],
                    strand.positions[index - 1][1] - strand.positions[index - 2][1],
                ]
            } else {
                [-gravity[0], -gravity[1]]
            };
            direction_angle(parent, translation)
        }
    };
    if output.reflect {
        -value
    } else {
        value
    }
}

fn apply_output(
    values: &mut BTreeMap<String, f32>,
    parameters: &BTreeMap<String, Range>,
    output: &Output,
    raw: f32,
) {
    let range = parameters[&output.id];
    // The pinned parser initializes only AngleScale; translation scales stay zero.
    let scale = if output.axis == Axis::Angle {
        output.scale
    } else {
        0.0
    };
    let target = (raw * scale).clamp(range.min, range.max);
    let old = values[&output.id];
    values.insert(
        output.id.clone(),
        old * (1.0 - output.weight) + target * output.weight,
    );
}

impl Runtime {
    pub(crate) fn snapshot(&self) -> Value {
        serde_json::json!({"groups": self.rig.groups.iter().map(|g| &g.id).collect::<Vec<_>>(),
            "fps": self.rig.fps, "parameterValues": self.values, "remainTime": self.remain,
            "options": self.options.snapshot()})
    }
    pub(crate) fn new(rig: Rig) -> Self {
        let strands = rig
            .groups
            .iter()
            .map(|group| {
                let mut y = 0.0;
                let positions = group
                    .particles
                    .iter()
                    .enumerate()
                    .map(|(index, particle)| {
                        if index != 0 {
                            y += particle.radius;
                        }
                        [0.0, y]
                    })
                    .collect();
                Strand {
                    positions,
                    velocities: vec![[0.0; 2]; group.particles.len()],
                    gravity: vec![[0.0, 1.0]; group.particles.len()],
                    previous: vec![0.0; group.outputs.len()],
                    current: vec![0.0; group.outputs.len()],
                }
            })
            .collect();
        Self {
            rig,
            strands,
            input_cache: None,
            remain: 0.0,
            options: ForceOptions::default(),
            values: BTreeMap::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.rig.clone());
    }

    pub(crate) fn rewind(&mut self) {
        let rig = self.rig.clone();
        let options = self.options;
        *self = Self::new(rig);
        self.options = options;
    }

    pub(crate) fn set_options(&mut self, options: &Value) -> Result<(), String> {
        self.options = ForceOptions::parse(options)?;
        Ok(())
    }

    pub(crate) fn stabilize(&mut self, base: &BTreeMap<String, f64>) -> Result<(), String> {
        let inputs = self
            .rig
            .parameters
            .keys()
            .map(|id| {
                let value = base
                    .get(id)
                    .ok_or_else(|| format!("physics input parameter {id:?} is missing"))?;
                if !value.is_finite() || !(*value as f32).is_finite() {
                    return Err("physics input must be finite f32".into());
                }
                Ok((id.clone(), *value as f32))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut candidate = self.clone();
        candidate.input_cache = Some(inputs.clone());
        let mut values = inputs;
        for (group, strand) in candidate.rig.groups.iter().zip(&mut candidate.strands) {
            let mut translation = [0.0, 0.0];
            let mut angle = 0.0;
            for input in &group.inputs {
                let range = if input.axis == Axis::Angle {
                    group.angle
                } else {
                    group.position
                };
                let value = normalize(
                    values[&input.id],
                    candidate.rig.parameters[&input.id],
                    range,
                    input.reflect,
                ) * input.weight;
                match input.axis {
                    Axis::X => translation[0] += value,
                    Axis::Y => translation[1] += value,
                    Axis::Angle => angle += value,
                }
            }
            let rotation = (-angle).to_radians();
            translation[0] = translation[0] * rotation.cos() - translation[1] * rotation.sin();
            translation[1] = translation[0] * rotation.sin() + translation[1] * rotation.cos();
            strand.positions[0] = translation;
            strand.velocities.fill([0.0; 2]);
            let radians = angle.to_radians();
            let current_gravity = unit([radians.sin(), radians.cos()])?;
            for index in 1..group.particles.len() {
                let particle = &group.particles[index];
                let direction = unit([
                    current_gravity[0] * particle.acceleration + candidate.options.wind[0],
                    current_gravity[1] * particle.acceleration + candidate.options.wind[1],
                ])?;
                let mut position = [
                    strand.positions[index - 1][0] + direction[0] * particle.radius,
                    strand.positions[index - 1][1] + direction[1] * particle.radius,
                ];
                if position[0].abs() < 0.001 * group.position.max {
                    position[0] = 0.0;
                }
                strand.positions[index] = position;
                strand.gravity[index] = current_gravity;
            }
            for (index, output) in group.outputs.iter().enumerate() {
                let raw = output_value(strand, output, candidate.options.gravity);
                if !raw.is_finite() {
                    return Err("native physics output is non-finite".into());
                }
                strand.previous[index] = raw;
                strand.current[index] = raw;
                apply_output(&mut values, &candidate.rig.parameters, output, raw);
            }
        }
        if values.values().any(|value| !value.is_finite()) {
            return Err("native physics output is non-finite".into());
        }
        candidate.values = candidate
            .rig
            .groups
            .iter()
            .flat_map(|group| &group.outputs)
            .map(|output| (output.id.clone(), f64::from(values[&output.id])))
            .collect();
        *self = candidate;
        Ok(())
    }

    pub(crate) fn step(&mut self, values: &BTreeMap<String, f64>, dt: f64) -> Result<(), String> {
        if !dt.is_finite() || dt < 0.0 || !(dt as f32).is_finite() {
            return Err("native physics dt must be finite, nonnegative and fit in f32".into());
        }
        let inputs = self
            .rig
            .parameters
            .iter()
            .map(|(id, _)| {
                let value = values
                    .get(id)
                    .ok_or_else(|| format!("physics input parameter {id:?} is missing"))?;
                if !value.is_finite() || !(*value as f32).is_finite() {
                    return Err("physics input must be finite f32".into());
                }
                Ok((id.clone(), *value as f32))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut candidate = self.clone();
        candidate.evaluate(inputs, dt as f32)?;
        *self = candidate;
        Ok(())
    }

    fn evaluate(&mut self, inputs: BTreeMap<String, f32>, dt: f32) -> Result<(), String> {
        if dt == 0.0 {
            return Ok(());
        }
        self.remain += dt;
        if self.remain > 5.0 {
            self.remain = 0.0;
        }
        let delta = if self.rig.fps > 0.0 {
            1.0 / self.rig.fps
        } else {
            dt
        };
        let cache = self.input_cache.get_or_insert_with(|| inputs.clone());
        while self.remain >= delta {
            let weight = delta / self.remain;
            let mut values = inputs
                .iter()
                .map(|(id, value)| (id.clone(), cache[id] * (1.0 - weight) + value * weight))
                .collect::<BTreeMap<_, _>>();
            *cache = values.clone();
            for (group, strand) in self.rig.groups.iter().zip(&mut self.strands) {
                strand.previous.clone_from(&strand.current);
                let mut translation = [0.0, 0.0];
                let mut angle = 0.0;
                for input in &group.inputs {
                    let range = if input.axis == Axis::Angle {
                        group.angle
                    } else {
                        group.position
                    };
                    let value = normalize(
                        values[&input.id],
                        self.rig.parameters[&input.id],
                        range,
                        input.reflect,
                    ) * input.weight;
                    match input.axis {
                        Axis::X => translation[0] += value,
                        Axis::Y => translation[1] += value,
                        Axis::Angle => angle += value,
                    }
                }
                let rotation = (-angle).to_radians();
                // Preserve the pinned framework's sequential X then Y assignment.
                translation[0] = translation[0] * rotation.cos() - translation[1] * rotation.sin();
                translation[1] = translation[0] * rotation.sin() + translation[1] * rotation.cos();
                strand.positions[0] = translation;
                let radians = angle.to_radians();
                let gravity = unit([radians.sin(), radians.cos()])?;
                for index in 1..group.particles.len() {
                    let particle = &group.particles[index];
                    let old = strand.positions[index];
                    let parent = strand.positions[index - 1];
                    let mut offset = [old[0] - parent[0], old[1] - parent[1]];
                    let turn = direction_angle(strand.gravity[index], gravity) / 5.0;
                    offset[0] = turn.cos() * offset[0] - offset[1] * turn.sin();
                    offset[1] = turn.sin() * offset[0] + offset[1] * turn.cos();
                    let delay = particle.delay * delta * 30.0;
                    let force = [
                        gravity[0] * particle.acceleration + self.options.wind[0],
                        gravity[1] * particle.acceleration + self.options.wind[1],
                    ];
                    let direction = unit([
                        offset[0]
                            + strand.velocities[index][0] * delay
                            + force[0] * delay * delay,
                        offset[1]
                            + strand.velocities[index][1] * delay
                            + force[1] * delay * delay,
                    ])?;
                    let mut next = [
                        parent[0] + direction[0] * particle.radius,
                        parent[1] + direction[1] * particle.radius,
                    ];
                    if next[0].abs() < 0.001 * group.position.max {
                        next[0] = 0.0;
                    }
                    if delay != 0.0 {
                        strand.velocities[index] = [
                            (next[0] - old[0]) / delay * particle.mobility,
                            (next[1] - old[1]) / delay * particle.mobility,
                        ];
                    }
                    strand.positions[index] = next;
                    strand.gravity[index] = gravity;
                }
                for (index, output) in group.outputs.iter().enumerate() {
                    let value = output_value(strand, output, self.options.gravity);
                    strand.current[index] = value;
                    apply_output(&mut values, &self.rig.parameters, output, value);
                }
            }
            self.remain -= delta;
        }
        let alpha = self.remain / delta;
        let mut values = inputs;
        for (group, strand) in self.rig.groups.iter().zip(&self.strands) {
            for (index, output) in group.outputs.iter().enumerate() {
                apply_output(
                    &mut values,
                    &self.rig.parameters,
                    output,
                    strand.previous[index] * (1.0 - alpha) + strand.current[index] * alpha,
                );
            }
        }
        if values.values().any(|value| !value.is_finite()) {
            return Err("native physics output is non-finite".into());
        }
        self.values = self
            .rig
            .groups
            .iter()
            .flat_map(|g| &g.outputs)
            .map(|output| (output.id.clone(), f64::from(values[&output.id])))
            .collect();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Value, Value) {
        (
            json!({"Version":3,"Meta":{"PhysicsSettingCount":1,"TotalInputCount":1,"TotalOutputCount":1,
            "VertexCount":2,"EffectiveForces":{"Gravity":{"X":0,"Y":-1},"Wind":{"X":0,"Y":0}}},
            "PhysicsSettings":[{"Id":"hair","Input":[{"Source":{"Target":"Parameter","Id":"in"},
                "Weight":60,"Type":"X","Reflect":false}],
                "Output":[{"Destination":{"Target":"Parameter","Id":"out"},"VertexIndex":1,
                    "Scale":2,"Weight":100,"Type":"Angle","Reflect":false}],
                "Vertices":[{"Position":{"X":5,"Y":7},"Mobility":1,"Delay":1,"Acceleration":1,"Radius":0},
                    {"Position":{"X":3,"Y":9},"Mobility":0.9,"Delay":0.8,"Acceleration":1.5,"Radius":3}],
                "Normalization":{"Position":{"Minimum":-10,"Default":0,"Maximum":10},
                    "Angle":{"Minimum":-10,"Default":0,"Maximum":10}}}]}),
            json!({"parameters":[{"id":"in","minimum":-30,"maximum":30,"default":0},
             {"id":"out","minimum":-1,"maximum":1,"default":0}]}),
        )
    }

    #[test]
    fn validates_bindings_counts_and_particle_boundaries() {
        let (document, source) = fixture();
        let rig = Rig::parse(&document, &source).unwrap();
        assert_eq!(rig.groups[0].inputs[0].weight, 0.6);
        assert_eq!(rig.groups[0].outputs[0].axis, Axis::Angle);
        assert_eq!(rig.fps, 0.0);
        for (pointer, invalid) in [
            ("/Meta/VertexCount", json!(1)),
            ("/Meta/Fps", json!(0.5)),
            ("/PhysicsSettings/0/Output/0/VertexIndex", json!(2)),
            ("/PhysicsSettings/0/Input/0/Source/Id", json!("missing")),
            ("/PhysicsSettings/0/Vertices/1/Radius", json!(0)),
            ("/PhysicsSettings/0/Vertices/1/Delay", json!(-0.1)),
            ("/PhysicsSettings/0/Input/0/Weight", json!(101)),
            ("/PhysicsSettings/0/Input/0/Type", json!("unknown")),
        ] {
            let mut bad = document.clone();
            if pointer == "/Meta/Fps" {
                bad["Meta"]["Fps"] = invalid;
            } else {
                *bad.pointer_mut(pointer).unwrap() = invalid;
            }
            assert!(Rig::parse(&bad, &source).is_err(), "{pointer}");
        }
    }

    #[test]
    fn accepts_a_particle_delay_above_one() {
        // Official samples ship Delay 1.3 and 1.5; the Framework reads it without a bound.
        let (mut document, source) = fixture();
        document["PhysicsSettings"][0]["Vertices"][1]["Delay"] = json!(1.5);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.0)]);
        runtime.step(&inputs, 1.0 / 30.0).unwrap();
        assert!(runtime.values["out"].is_finite());
    }

    #[test]
    fn normalization_uses_midpoint_reflection_and_clamping() {
        let parameter = Range {
            min: -10.0,
            max: 30.0,
            default: 25.0,
        };
        let target = Range {
            min: -4.0,
            max: 8.0,
            default: 2.0,
        };
        assert_eq!(normalize(10.0, parameter, target, true), 2.0);
        assert_eq!(normalize(20.0, parameter, target, true), 5.0);
        assert_eq!(normalize(0.0, parameter, target, false), 1.0);
        assert_eq!(normalize(100.0, parameter, target, false), -8.0);
    }

    #[test]
    fn force_options_are_complete_finite_pairs_and_preserve_runtime_history() {
        let (mut document, source) = fixture();
        document["Meta"]["EffectiveForces"]["Gravity"]["X"] = json!(20);
        document["Meta"]["EffectiveForces"]["Wind"]["Y"] = json!(30);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        assert_eq!(
            runtime.snapshot()["options"],
            json!({"gravity":{"x":0.0,"y":-1.0},"wind":{"x":0.0,"y":0.0}})
        );
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.step(&inputs, 1.0 / 60.0).unwrap();
        let positions = runtime.strands[0].positions.clone();
        let cache = runtime.input_cache.clone();
        let remain = runtime.remain;
        let values = runtime.values.clone();
        for invalid in [
            json!({"gravity":{"x":1,"y":2}}),
            json!({"gravity":{"x":1e40,"y":2},"wind":{"x":0,"y":0}}),
        ] {
            let before = format!("{runtime:?}");
            assert!(runtime.set_options(&invalid).is_err());
            assert_eq!(format!("{runtime:?}"), before);
        }
        runtime
            .set_options(&json!({"gravity":{"x":3,"y":4},"wind":{"x":5,"y":6}}))
            .unwrap();
        assert_eq!(runtime.strands[0].positions, positions);
        assert_eq!(runtime.input_cache, cache);
        assert_eq!(runtime.remain, remain);
        assert_eq!(runtime.values, values);
        runtime.reset();
        assert_eq!(
            runtime.snapshot()["options"],
            json!({"gravity":{"x":0.0,"y":-1.0},"wind":{"x":0.0,"y":0.0}})
        );
    }

    #[test]
    fn stabilization_builds_literal_geometry_and_matching_output_history() {
        let (document, source) = fixture();
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.stabilize(&inputs).unwrap();
        assert_eq!(runtime.strands[0].positions, vec![[-6.0, 0.0], [-6.0, 3.0]]);
        assert_eq!(runtime.values["out"], 0.0);
        assert_eq!(runtime.strands[0].previous, vec![0.0]);
        assert_eq!(runtime.strands[0].current, vec![0.0]);
        assert_eq!(runtime.input_cache, Some(BTreeMap::from([
            ("in".to_owned(), 30.0),
            ("out".to_owned(), 0.4),
        ])));
        assert_eq!(runtime.remain, 0.0);
    }

    #[test]
    fn physics_y_inputs_use_position_normalization_weight_and_reflection() {
        let (mut document, source) = fixture();
        document["PhysicsSettings"][0]["Input"][0]["Type"] = json!("Y");
        document["PhysicsSettings"][0]["Normalization"]["Angle"] =
            json!({"Minimum":-30,"Default":0,"Maximum":30});
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        for (weight, displacement) in [(60.0, 6.0), (12.5, 1.25)] {
            for reflect in [false, true] {
                document["PhysicsSettings"][0]["Input"][0]["Weight"] = json!(weight);
                document["PhysicsSettings"][0]["Input"][0]["Reflect"] = json!(reflect);
                let expected = [0.0, if reflect { displacement } else { -displacement }];
                let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
                runtime.stabilize(&inputs).unwrap();
                assert_eq!(runtime.strands[0].positions[0], expected);
                runtime.step(&inputs, 1.0 / 60.0).unwrap();
                assert_eq!(runtime.strands[0].positions[0], expected);
            }
        }
    }

    #[test]
    fn stabilization_uses_configured_gravity_for_first_segment_angle() {
        let (document, source) = fixture();
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        runtime
            .set_options(&json!({"gravity":{"x":1.0,"y":0.0},"wind":{"x":0.0,"y":0.0}}))
            .unwrap();
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.stabilize(&inputs).unwrap();
        assert!((runtime.strands[0].current[0] + std::f32::consts::FRAC_PI_2).abs() < 0.00001);
        assert_eq!(runtime.values["out"], -1.0);
    }

    #[test]
    fn stabilization_passes_earlier_group_outputs_to_later_inputs() {
        let (mut document, source) = fixture();
        document["PhysicsSettings"][0]["Output"][0]["Scale"] = json!(1.0);
        let mut feedback = document["PhysicsSettings"][0].clone();
        feedback["Id"] = json!("feedback");
        feedback["Input"][0]["Source"]["Id"] = json!("out");
        feedback["Input"][0]["Weight"] = json!(100.0);
        feedback["Output"][0]["Destination"]["Id"] = json!("in");
        document["PhysicsSettings"].as_array_mut().unwrap().push(feedback);
        document["Meta"]["PhysicsSettingCount"] = json!(2);
        document["Meta"]["TotalInputCount"] = json!(2);
        document["Meta"]["TotalOutputCount"] = json!(2);
        document["Meta"]["VertexCount"] = json!(4);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        runtime
            .set_options(&json!({"gravity":{"x":0.0,"y":-1.0},"wind":{"x":1.0,"y":0.0}}))
            .unwrap();
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.stabilize(&inputs).unwrap();
        assert!((runtime.strands[0].current[0] + 0.5880026).abs() < 0.00001);
        assert!((runtime.strands[1].positions[0][0] - 5.880026).abs() < 0.0001);
    }

    #[test]
    fn stabilization_preserves_nonzero_remaining_time() {
        let (mut document, source) = fixture();
        document["Meta"]["Fps"] = json!(60.0);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.step(&inputs, 1.0 / 120.0).unwrap();
        let remain = runtime.remain;
        assert!(remain > 0.0);
        runtime.stabilize(&inputs).unwrap();
        assert_eq!(runtime.remain, remain);
    }

    #[test]
    fn stabilization_uses_wind_and_rejects_degenerate_force_transactionally() {
        let (mut document, source) = fixture();
        document["PhysicsSettings"][0]["Vertices"][1]["Acceleration"] = json!(0);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        runtime
            .set_options(&json!({"gravity":{"x":0,"y":-1},"wind":{"x":1,"y":0}}))
            .unwrap();
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.stabilize(&inputs).unwrap();
        assert_eq!(runtime.strands[0].positions, vec![[-6.0, 0.0], [-3.0, 0.0]]);
        assert_eq!(runtime.values["out"], -1.0);

        let (document, source) = fixture();
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        runtime
            .set_options(&json!({"gravity":{"x":0,"y":-1},"wind":{"x":0,"y":-1.5}}))
            .unwrap();
        let before = format!("{runtime:?}");
        assert!(runtime.stabilize(&inputs).is_err());
        assert_eq!(format!("{runtime:?}"), before);
    }

    #[test]
    fn evaluate_adds_wind_to_particle_force() {
        let (mut document, source) = fixture();
        document["PhysicsSettings"][0]["Vertices"][1]["Acceleration"] = json!(0);
        document["PhysicsSettings"][0]["Vertices"][1]["Delay"] = json!(1);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        runtime
            .set_options(&json!({"gravity":{"x":0,"y":-1},"wind":{"x":1,"y":0}}))
            .unwrap();
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.step(&inputs, 1.0 / 30.0).unwrap();
        assert!((runtime.strands[0].positions[1][0] - (-6.0 + 21.0 / 58.0_f32.sqrt())).abs() < 0.00001);
        assert!((runtime.strands[0].positions[1][1] - 9.0 / 58.0_f32.sqrt()).abs() < 0.00001);
    }

    #[test]
    fn particle_motion_has_delayed_output_and_failures_preserve_state() {
        let (mut document, source) = fixture();
        document["Meta"]["Fps"] = json!(60);
        document["PhysicsSettings"][0]["Vertices"][1]["Delay"] = json!(0);
        let mut runtime = Runtime::new(Rig::parse(&document, &source).unwrap());
        let inputs = BTreeMap::from([("in".to_owned(), 30.0), ("out".to_owned(), 0.4)]);
        runtime.step(&inputs, 1.0 / 60.0).unwrap();
        assert!((runtime.strands[0].positions[1][0] + 3.3167184).abs() < 0.00001);
        assert!((runtime.strands[0].positions[1][1] - 1.3416408).abs() < 0.00001);
        assert_eq!(runtime.values["out"], 0.0);
        runtime.step(&inputs, 1.0 / 60.0).unwrap();
        assert_eq!(runtime.values["out"], -1.0);
        let before = format!("{runtime:?}");
        assert!(runtime.step(&inputs, -1.0).is_err());
        let mut invalid = inputs.clone();
        invalid.insert("in".into(), f64::NAN);
        assert!(runtime.step(&invalid, 0.01).is_err());
        assert_eq!(format!("{runtime:?}"), before);
        runtime.reset();
        assert_eq!(runtime.strands[0].positions, vec![[0.0, 0.0], [0.0, 3.0]]);
        assert!(runtime.values.is_empty());
    }

    #[test]
    #[ignore = "requires the licensed local sample corpus via VSOUL_NATIVE_PHYSICS_INPUTS"]
    fn binds_all_local_physics_sidecars_to_authored_parameters() {
        let root = std::path::PathBuf::from(std::env::var("VSOUL_NATIVE_PHYSICS_INPUTS").unwrap());
        for (folder, basename, modelname, groups, fps) in [
            ("ren_ja", "ren", "ren_t01", 16, 60.0),
            ("zundamon_ja", "zundamon", "zundamon_t01", 12, 60.0),
            (
                "hiyori_ja/hiyori_free",
                "hiyori_free_t08",
                "hiyori_free_t08",
                7,
                0.0,
            ),
            (
                "hiyori_ja/hiyori_pro",
                "hiyori_pro_t11",
                "hiyori_pro_t11",
                11,
                0.0,
            ),
        ] {
            let folder = root.join(folder);
            let bytes = std::fs::read(folder.join(format!("{modelname}.cmo3"))).unwrap();
            let container = crate::caff::CaffDocument::parse(&bytes).unwrap();
            let source = crate::authoring::inspect(&container).unwrap();
            let sidecar =
                std::fs::read(folder.join(format!("runtime/{basename}.physics3.json"))).unwrap();
            let document = serde_json::from_slice(&sidecar).unwrap();
            let rig = Rig::parse(&document, &source["model"]).unwrap();
            assert_eq!(rig.groups.len(), groups, "{basename}");
            assert_eq!(rig.fps, fps, "{basename}");
            let mut inputs = rig
                .parameters
                .iter()
                .map(|(id, range)| (id.clone(), f64::from(range.default)))
                .collect::<BTreeMap<_, _>>();
            let input_id = rig.groups[0].inputs[0].id.clone();
            inputs.insert(input_id.clone(), f64::from(rig.parameters[&input_id].max));
            let mut runtime = Runtime::new(rig);
            for _ in 0..30 {
                runtime.step(&inputs, 1.0 / 60.0).unwrap();
            }
            assert!(
                runtime.values.values().any(|v| v.abs() > 0.00001),
                "{basename} outputs stayed zero"
            );
            for (id, value) in &runtime.values {
                let range = runtime.rig.parameters[id];
                assert!(
                    value.is_finite()
                        && *value >= f64::from(range.min)
                        && *value <= f64::from(range.max)
                );
            }
        }
    }
}
