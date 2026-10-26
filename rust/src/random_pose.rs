//! Random pose preview values. The formulas are Soul2D's own: the Cubism Editor manual names the parameters
//! and ranges of its patterns but gives no speed, period or interpolation. The values are a pure function of
//! (seed, pattern, time, model), so a pose can be recomputed at any time without keeping generator state.

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::{finite_number, parse_pair, valid_id};

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
/// Latest time of a pose, in seconds. Far beyond it an `f64` holds no fraction of a period and the pose would stand
/// still while time moves on.
const MAX_TIME: f64 = 1e9;
/// Seconds between two random targets of pattern B.
const PERIOD: f64 = 1.5;
/// Back-and-forth parameters of patterns A and Posing: (name, low, high, period in seconds).
const WAVES: [(&str, f64, f64, f64); 7] = [
    ("ParamAngleX", -30.0, 30.0, 4.0),
    ("ParamAngleZ", -30.0, 30.0, 5.0),
    ("ParamEyeLOpen", 0.0, 2.0, 3.0),
    ("ParamEyeROpen", 0.0, 2.0, 3.0),
    ("ParamBodyAngleX", -10.0, 10.0, 6.0),
    ("ParamMouthOpenY", -1.0, 1.0, 2.0),
    ("ParamBreath", 0.0, 1.0, 3.5),
];

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Pattern {
    A,
    B,
    Posing,
}

#[derive(Clone)]
pub(crate) struct RandomPose {
    pattern: Pattern,
    seed: u64,
    time: f64,
    /// Leaf parameters that move; `None` moves every leaf parameter.
    parameters: Option<Vec<u32>>,
}

impl RandomPose {
    pub(crate) fn parse(request: &Value) -> Result<Self, String> {
        let time = finite_number(request.get("time").ok_or("time is required")?, "time")?;
        if time < 0.0 {
            return Err("time must not be negative".into());
        }
        if time > MAX_TIME {
            return Err(format!("time must be at most {MAX_TIME} seconds"));
        }
        let given = |key: &str| request.get(key).filter(|value| !value.is_null());
        let seed = match given("seed") {
            None => 0,
            Some(value) => value.as_u64().ok_or("seed must be a non-negative integer")?,
        };
        let pattern = match given("pattern").map(|value| value.as_str()) {
            None => Pattern::B,
            Some(Some("A")) => Pattern::A,
            Some(Some("B")) => Pattern::B,
            Some(Some("Posing")) => Pattern::Posing,
            Some(_) => return Err("pattern must be \"A\", \"B\" or \"Posing\"".into()),
        };
        let parameters = match given("parameters") {
            None => None,
            Some(value) => {
                let list = value.as_array().filter(|list| !list.is_empty())
                    .ok_or("parameters must be a non-empty array of parameter UUIDs")?;
                let ids = list.iter().map(|id| id.as_u64().and_then(valid_id)).collect::<Option<Vec<_>>>();
                Some(ids.ok_or("parameters must contain only parameter UUIDs")?)
            }
        };
        Ok(Self { pattern, seed, time, parameters })
    }

    /// The same pose `dt` seconds later.
    pub(crate) fn advanced(&self, dt: f64) -> Result<Self, String> {
        let time = self.time + dt;
        if !time.is_finite() || time > MAX_TIME {
            return Err(format!("a random pose ends at {MAX_TIME} seconds"));
        }
        Ok(Self { time, ..self.clone() })
    }

    pub(crate) fn summary(&self) -> Value {
        json!({"pattern": match self.pattern { Pattern::A => "A", Pattern::B => "B", Pattern::Posing => "Posing" },
            "seed": self.seed, "time": self.time, "parameters": self.parameters})
    }
}

/// A number in [0, 1) that depends only on its four inputs.
fn noise(seed: u64, id: u32, axis: usize, k: u64) -> f64 {
    let mut x = seed;
    for part in [u64::from(id), axis as u64, k] {
        // splitmix64 finaliser, chained over the inputs
        x = (x ^ part).wrapping_add(GOLDEN);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
    }
    (x >> 11) as f64 / (1u64 << 53) as f64
}

struct Leaf {
    id: u32,
    name: String,
    is_vec2: bool,
    min: [f64; 2],
    max: [f64; 2],
}

fn collect(list: &[Value], leaves: &mut Vec<Leaf>) {
    for item in list {
        let range = |key: &str, fallback: f64| {
            item.get(key).and_then(|pair| parse_pair(pair, key).ok()).unwrap_or([fallback; 2])
        };
        if let (None, Some(id)) = (item.get("groupUUID"), item.get("uuid").and_then(Value::as_u64).and_then(valid_id)) {
            leaves.push(Leaf { id, name: item.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
                is_vec2: item.get("is_vec2").and_then(Value::as_bool).unwrap_or(false),
                min: range("min", 0.0), max: range("max", 1.0) });
        }
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            collect(children, leaves);
        }
    }
}

/// `base` with the parameters the pose moves replaced. Every produced value is clamped into its parameter's range.
pub(crate) fn values(model: &Value, base: &BTreeMap<u32, [f64; 2]>, pose: &RandomPose)
    -> Result<BTreeMap<u32, [f64; 2]>, String> {
    let mut leaves = Vec::new();
    collect(model.get("param").and_then(Value::as_array).map_or(&[][..], Vec::as_slice), &mut leaves);
    if let Some(ids) = &pose.parameters {
        for id in ids {
            if !leaves.iter().any(|leaf| leaf.id == *id) {
                return Err(format!("parameter {id} is not a leaf parameter"));
            }
        }
        leaves.retain(|leaf| ids.contains(&leaf.id));
    }
    let (seed, time) = (pose.seed, pose.time);
    let mut result = base.clone();
    for leaf in &leaves {
        let wave = WAVES.iter().find(|wave| wave.0 == leaf.name && match pose.pattern {
            Pattern::A => wave.0 != "ParamBreath",
            Pattern::Posing => matches!(wave.0, "ParamEyeLOpen" | "ParamEyeROpen" | "ParamMouthOpenY" | "ParamBreath"),
            Pattern::B => false,
        });
        if pose.pattern == Pattern::A && wave.is_none() {
            continue;
        }
        let mut value = base.get(&leaf.id).copied().unwrap_or([0.0; 2]);
        if let Some((_, low, high, period)) = wave {
            let phase = std::f64::consts::TAU * (time / period + noise(seed, leaf.id, 0, 0));
            value[0] = ((low + high) / 2.0 + (high - low) / 2.0 * phase.sin()).max(leaf.min[0]).min(leaf.max[0]);
        } else {
            let k = (time / PERIOD).floor() as u64;
            let f = time / PERIOD - (time / PERIOD).floor();
            for axis in 0..if leaf.is_vec2 { 2 } else { 1 } {
                let target = |k: u64| leaf.min[axis] + noise(seed, leaf.id, axis, k) * (leaf.max[axis] - leaf.min[axis]);
                let blend = target(k) * (1.0 - f) + target(k.wrapping_add(1)) * f;
                value[axis] = blend.max(leaf.min[axis]).min(leaf.max[axis]);
            }
        }
        result.insert(leaf.id, value);
    }
    Ok(result)
}
