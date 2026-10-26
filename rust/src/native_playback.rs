use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_EXPRESSION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AttachedAnimation {
    pub(crate) source: Value,
    pub(crate) path: String,
    pub(crate) resource_guid: String,
}

pub(crate) struct AttachedModel {
    pub(crate) scene: crate::native_scene::NativeScene,
    pub(crate) path: String,
    pub(crate) resource_guid: String,
}

#[derive(Clone)]
pub(crate) struct TimelinePreview {
    pub(crate) scene_guid: String,
    pub(crate) frame: f64,
    pub(crate) clock_frame: f64,
    pub(crate) fps: f64,
    pub(crate) duration_frames: i64,
    pub(crate) values: BTreeMap<String, f64>,
    pub(crate) pose_signals: BTreeMap<String, f64>,
    pub(crate) part_opacities: BTreeMap<String, f64>,
}

#[derive(Clone)]
struct ExpressionValue {
    id: String,
    value: f64,
    blend: Blend,
}

struct Expression {
    values: Vec<ExpressionValue>,
    /// `FadeInTime`; unset (absent or negative) is no fade, as for motions.
    fade_in: Option<f64>,
}

#[derive(Clone, Copy)]
enum Blend {
    Add,
    Multiply,
    Overwrite,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) animation: Option<AttachedAnimation>,
    pub(crate) model: Option<AttachedModel>,
    pub(crate) active: Option<TimelinePreview>,
    expressions: BTreeMap<String, Expression>,
    pub(crate) expression_name: Option<String>,
    /// Seconds played since `expression_name` was applied with fades; `None` applies it at full weight.
    pub(crate) expression_elapsed: Option<f64>,
}

impl State {
    pub(crate) fn reset_animation(&mut self) {
        self.active = None;
    }

    pub(crate) fn expression_list(&self) -> Value {
        let expressions = self
            .expressions
            .iter()
            .map(|(name, expression)| json!({"name": name, "fadeIn": expression.fade_in}))
            .collect::<Vec<_>>();
        let mut result = json!({"expressions": expressions});
        if let Some(name) = &self.expression_name {
            result["active"] = json!(name);
            result["weight"] = json!(self.expression_weight());
        }
        result
    }

    pub(crate) fn import_expression(
        &mut self,
        name: &str,
        path: &str,
        parameters: &[Value],
        bytes: &[u8],
    ) -> Result<Value, String> {
        validate_expression_name(name)?;
        if self.expressions.contains_key(name) {
            return Err(format!("expression {name:?} already exists"));
        }
        if bytes.len() as u64 > MAX_EXPRESSION_BYTES {
            return Err("expression source exceeds the 16 MiB limit".into());
        }
        if !path.to_ascii_lowercase().ends_with(".exp3.json") {
            return Err("expression path must end in .exp3.json".into());
        }
        let document: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid expression JSON: {error}"))?;
        if document["Type"] != "Live2D Expression" {
            return Err("expression Type must be Live2D Expression".into());
        }
        let known = parameters
            .iter()
            .map(|parameter| {
                let id = parameter["id"]
                    .as_str()
                    .ok_or("native parameter ID is missing")?;
                Ok((id, parameter))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        let records = document["Parameters"]
            .as_array()
            .ok_or("expression Parameters must be an array")?;
        let mut seen = HashSet::new();
        let mut parsed = Vec::with_capacity(records.len());
        for record in records {
            let id = record["Id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("expression parameter Id must be a nonempty string")?;
            if !seen.insert(id) {
                return Err(format!("expression parameter {id:?} is duplicated"));
            }
            let parameter = known
                .get(id)
                .ok_or_else(|| format!("expression references unknown native parameter {id:?}"))?;
            let value = record["Value"]
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or("expression Value must be finite")?;
            let blend = match record.get("Blend") {
                None => Blend::Add,
                Some(Value::String(blend)) => match blend.as_str() {
                    "Add" => Blend::Add,
                    "Multiply" => Blend::Multiply,
                    "Overwrite" => Blend::Overwrite,
                    _ => return Err("expression Blend must be Add, Multiply, or Overwrite".into()),
                },
                Some(_) => return Err("expression Blend must be Add, Multiply, or Overwrite".into()),
            };
            let _ = parameter;
            parsed.push(ExpressionValue {
                id: id.to_owned(),
                value,
                blend,
            });
        }
        let fade_in = match document.get("FadeInTime") {
            None => None,
            Some(seconds) => Some(seconds.as_f64().ok_or("expression FadeInTime must be a number")?),
        };
        // ponytail: FadeOutTime is not played; reset and switching expressions cut at once. Add a fading-out
        // expression list if crossfades matter.
        let fade_in = fade_in.filter(|seconds| *seconds >= 0.0);
        self.expressions.insert(name.to_owned(), Expression { values: parsed, fade_in });
        Ok(json!({"name": name, "path": path}))
    }

    /// With `fades`, the expression fades in as playback advances (`tick_expression`).
    pub(crate) fn apply_expression(&mut self, name: &str, fades: bool) -> Result<Value, String> {
        if !self.expressions.contains_key(name) {
            return Err(format!("expression {name:?} was not found"));
        }
        self.expression_name = Some(name.to_owned());
        self.expression_elapsed = fades.then_some(0.0);
        Ok(json!({"name": name}))
    }

    pub(crate) fn reset_expression(&mut self) {
        self.expression_name = None;
    }

    /// Advances the fade-in of an expression applied with fades by `dt` seconds of playback.
    pub(crate) fn tick_expression(&mut self, dt: f64) {
        if let Some(elapsed) = &mut self.expression_elapsed {
            *elapsed += dt;
        }
    }

    /// ponytail: a seek's physics and pose replay uses this weight for every step; rebuild the clock per step (as
    /// motion fades do) if warm-up during an expression fade must be exact.
    fn expression_weight(&self) -> f64 {
        let fade_in = self.expression_name.as_ref().and_then(|name| self.expressions.get(name)?.fade_in);
        self.expression_elapsed.map_or(1.0, |elapsed| crate::motion3::fade_weight(elapsed, fade_in))
    }

    pub(crate) fn clear_expressions(&mut self) {
        self.expressions.clear();
        self.expression_name = None;
    }

    pub(crate) fn retain_parameters(&mut self, parameters: &[Value]) {
        let ids = parameters
            .iter()
            .filter_map(|parameter| parameter["id"].as_str())
            .collect::<HashSet<_>>();
        self.expressions
            .retain(|_, expression| expression.values.iter().all(|value| ids.contains(value.id.as_str())));
        if self
            .expression_name
            .as_ref()
            .is_some_and(|name| !self.expressions.contains_key(name))
        {
            self.expression_name = None;
        }
    }

    pub(crate) fn effective_values(
        &self,
        base: &BTreeMap<String, f64>,
        parameters: &[Value],
    ) -> Result<BTreeMap<String, f64>, String> {
        let values = self
            .active
            .as_ref()
            .map(|preview| preview.values.clone())
            .unwrap_or_else(|| base.clone());
        self.effective_values_without_animation(&values, parameters)
    }

    pub(crate) fn effective_values_without_animation(
        &self, base: &BTreeMap<String, f64>, parameters: &[Value],
    ) -> Result<BTreeMap<String, f64>, String> {
        let mut values = base.clone();
        let ranges = parameters
            .iter()
            .map(|parameter| {
                let id = parameter["id"]
                    .as_str()
                    .ok_or("native parameter ID is missing")?;
                let min = finite_field(parameter, "minimum")?;
                let max = finite_field(parameter, "maximum")?;
                if min > max {
                    return Err("native parameter range is reversed".into());
                }
                Ok((id, (min, max, parameter["repeat"] == true)))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        if let Some(name) = &self.expression_name {
            let expression = self
                .expressions
                .get(name)
                .ok_or("active expression was not found")?;
            let weight = self.expression_weight();
            for item in &expression.values {
                let current = *values
                    .get(&item.id)
                    .ok_or_else(|| format!("expression parameter {:?} is unavailable", item.id))?;
                let blended = match item.blend {
                    // Full weight keeps the exact values an expression without fades gives.
                    Blend::Add => current + item.value * weight,
                    Blend::Multiply if weight < 1.0 => current * (1.0 + (item.value - 1.0) * weight),
                    Blend::Multiply => current * item.value,
                    Blend::Overwrite if weight < 1.0 => current + (item.value - current) * weight,
                    Blend::Overwrite => item.value,
                };
                let (min, max, repeat) = ranges
                    .get(item.id.as_str())
                    .ok_or_else(|| format!("native parameter {:?} is unavailable", item.id))?;
                let value = if blended.is_finite() {
                    crate::native_range_value(blended, *min, *max, *repeat)
                } else if blended.is_sign_positive() {
                    *max
                } else {
                    *min
                };
                values.insert(item.id.clone(), value);
            }
        }
        for (id, value) in &mut values {
            let (min, max, repeat) = ranges
                .get(id.as_str())
                .ok_or_else(|| format!("native parameter {id:?} is unavailable"))?;
            *value = crate::native_range_value(*value, *min, *max, *repeat);
        }
        Ok(values)
    }
}

pub(crate) fn select_resource(
    animation: &Value,
    requested: Option<&str>,
) -> Result<String, String> {
    let resources = animation["resources"]
        .as_array()
        .ok_or("animation source lacks resources")?;
    if let Some(requested) = requested {
        if requested.is_empty() {
            return Err("resourceGuid must be a nonempty string".into());
        }
        if resources
            .iter()
            .filter(|resource| resource["guid"].as_str() == Some(requested))
            .count()
            != 1
        {
            return Err("animation model resource was not found or is ambiguous".into());
        }
        return Ok(requested.to_owned());
    }
    if resources.len() != 1 {
        return Err(
            "resourceGuid is required unless the CAN3 has exactly one model resource".into(),
        );
    }
    resources[0]["guid"]
        .as_str()
        .map(str::to_owned)
        .ok_or("animation model resource GUID is missing".into())
}

pub(crate) fn sample(
    animation: &Value,
    model: &Value,
    resource_guid: &str,
    scene_guid: &str,
    frame: f64,
    base: &BTreeMap<String, f64>,
) -> Result<TimelinePreview, String> {
    if !frame.is_finite() {
        return Err("frame must be finite".into());
    }
    let binding_report = crate::animation_source::bindings(animation, model, resource_guid)?;
    let legacy_part_bindings = part_bindings(&binding_report, "legacyPartBindings", "curveId")?;
    let underscore_part_bindings = part_bindings(&binding_report, "underscorePartBindings", "mapKey")?;
    let mut scenes = animation["scenes"]
        .as_array()
        .ok_or("animation scenes are missing")?
        .iter()
        .filter(|scene| scene["guid"].as_str() == Some(scene_guid));
    let scene = scenes.next().ok_or("animation scene was not found")?;
    if scenes.next().is_some() {
        return Err("animation scene GUID is ambiguous".into());
    }
    let fps = finite_field(scene, "fps")?;
    if fps <= 0.0 {
        return Err("animation FPS must be positive".into());
    }
    let duration_frames = scene["durationFrames"]
        .as_i64()
        .ok_or("animation durationFrames must be an integer")?;
    if duration_frames < 0 {
        return Err("animation durationFrames must be nonnegative".into());
    }
    let clock_frame = frame.clamp(0.0, duration_frames as f64);
    let frame = clock_frame.min(duration_frames.saturating_sub(1).max(0) as f64);
    let tracks = scene["tracks"]
        .as_array()
        .ok_or("animation tracks are missing")?;
    let mut selected = Vec::new();
    for track in tracks
        .iter()
        .filter(|track| track["resourceGuid"].as_str() == Some(resource_guid))
    {
        let start = integer_field(track, "start")?;
        let duration = integer_field(track, "duration")?;
        if duration < 0 {
            return Err("animation track duration must be nonnegative".into());
        }
        let end = start
            .checked_add(duration)
            .ok_or("animation track interval overflows")?;
        if frame >= start as f64 && frame < end as f64 {
            selected.push(track);
        }
    }
    reject_overlapping_parameter_writers(&selected)?;
    let mut values = base.clone();
    let mut pose_signals = BTreeMap::new();
    let mut part_opacities = BTreeMap::new();
    let mut part_writers = HashMap::new();
    for (track_index, track) in selected.into_iter().enumerate() {
        let start = integer_field(track, "start")?;
        let offset = integer_field(track, "internalOffset")?;
        let local_frame = frame - start as f64 + offset as f64;
        let track_guid = track["guid"].as_str().ok_or("animation track lacks a GUID")?;
        for curve in track["parameterCurves"]
            .as_array()
            .ok_or("animation curves are missing")?
        {
            let id = curve["parameterId"]
                .as_str()
                .ok_or("animation curve parameter ID is missing")?;
            let points = curve["points"]
                .as_array()
                .ok_or("animation curve points are missing")?;
            let types = curve["curveTypes"]
                .as_array()
                .ok_or("animation curve types are missing")?;
            if points.len() != types.len() {
                return Err("animation point and curve-type counts differ".into());
            }
            if let Some(value) = sample_curve(points, types, local_frame)? {
                if !values.contains_key(id) {
                    return Err(format!("animation parameter {id:?} is unavailable"));
                }
                values.insert(id.to_owned(), value);
            }
        }
        for (field, bindings, identity) in [
            ("partOpacityCurves", None, "id"),
            ("legacyPartOpacityCurves", Some(&legacy_part_bindings), "id"),
            ("underscorePartOpacityCurves", Some(&underscore_part_bindings), "mapKey"),
        ] {
            let Some(curves) = track.get(field) else { continue };
            for curve in curves.as_array().ok_or("animation PartOpacity curves must be an array")? {
                let (part_id, part_guid) = if let Some(bindings) = bindings {
                    let id = curve[identity].as_str().ok_or("animation PartOpacity curve identity is missing")?;
                    let key = (scene_guid.to_owned(), track_guid.to_owned(), id.to_owned());
                    let Some((id, guid)) = bindings.get(&key) else { continue };
                    (id.as_str(), guid.as_str())
                } else {
                    (curve["partId"].as_str().ok_or("animation Part ID is missing")?,
                        curve["partGuid"].as_str().ok_or("animation Part GUID is missing")?)
                };
                let points = curve["points"].as_array().ok_or("animation PartOpacity points are missing")?;
                let types = curve["curveTypes"].as_array().ok_or("animation PartOpacity curve types are missing")?;
                if points.len() != types.len() { return Err("animation PartOpacity point and type counts differ".into()); }
                if let Some(value) = sample_curve(points, types, local_frame)? {
                    if let Some(previous) = part_writers.insert(part_id.to_owned(), track_index) {
                        return Err(if previous == track_index {
                            "multiple active animation curves write the same Part"
                        } else {
                            "overlapping animation tracks write the same channel"
                        }.into());
                    }
                    let opacity = value.clamp(0.0, 1.0);
                    pose_signals.insert(part_id.to_owned(), opacity);
                    part_opacities.insert(part_guid.to_owned(), opacity);
                }
            }
        }
    }
    Ok(TimelinePreview {
        scene_guid: scene_guid.to_owned(),
        frame,
        clock_frame,
        fps,
        duration_frames,
        values,
        pose_signals,
        part_opacities,
    })
}

pub(crate) fn advance_frame(
    active: &TimelinePreview,
    dt: f64,
    looped: bool,
) -> Result<f64, String> {
    if !dt.is_finite() || dt < 0.0 {
        return Err("dt must be a finite nonnegative number".into());
    }
    let next = active.clock_frame + dt * active.fps;
    if !next.is_finite() {
        return Err("advanced animation frame is not finite".into());
    }
    Ok(if looped && active.duration_frames > 0 {
        next.rem_euclid(active.duration_frames as f64)
    } else {
        next.clamp(0.0, active.duration_frames as f64)
    })
}

type PartBindings = HashMap<(String, String, String), (String, String)>;

fn part_bindings(report: &Value, field: &str, identity: &str) -> Result<PartBindings, String> {
    let mut bindings = HashMap::new();
    for binding in report[field]
        .as_array()
        .ok_or_else(|| format!("animation binding report lacks {field}"))?
    {
        let key = (
            binding["sceneGuid"].as_str().ok_or("Part binding scene GUID is missing")?.to_owned(),
            binding["trackGuid"].as_str().ok_or("Part binding track GUID is missing")?.to_owned(),
            binding[identity].as_str().ok_or("Part binding curve identity is missing")?.to_owned(),
        );
        let part = (
            binding["partId"].as_str().ok_or("Part binding canonical Part ID is missing")?.to_owned(),
            binding["partGuid"].as_str().ok_or("Part binding Part GUID is missing")?.to_owned(),
        );
        if bindings.insert(key, part).is_some() {
            return Err(format!("{field} contains an ambiguous Part binding"));
        }
    }
    Ok(bindings)
}

fn reject_overlapping_parameter_writers(tracks: &[&Value]) -> Result<(), String> {
    let mut writers = Vec::with_capacity(tracks.len());
    for track in tracks {
        let curves = track["parameterCurves"]
            .as_array()
            .ok_or("animation curves are missing")?;
        let ids = curves
            .iter()
            .filter_map(|curve| curve["parameterId"].as_str().map(|id| format!("parameter:{id}")))
            .collect::<HashSet<_>>();
        if ids.len() != curves.len() {
            return Err("multiple active animation curves write the same parameter".into());
        }
        writers.push(ids);
    }
    for (index, left) in writers.iter().enumerate() {
        for right in writers.iter().skip(index + 1) {
            if left.iter().any(|id| right.contains(id)) {
                return Err("overlapping animation tracks write the same channel".into());
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum CurveKind {
    Smooth,
    Linear,
    Step,
}

fn sample_curve(points: &[Value], types: &[Value], frame: f64) -> Result<Option<f64>, String> {
    if points.is_empty() {
        return Ok(None);
    }
    let mut keys = points
        .iter()
        .zip(types)
        .map(|(point, curve_type)| {
            let anchor_frame = finite_field(&point["anchor"], "frame")?;
            let value = finite_field(&point["anchor"], "value")?;
            Ok((
                anchor_frame,
                value,
                point,
                curve_kind(curve_type.as_str().ok_or("curve type must be a string")?)?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    if keys.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err("animation key frames must be unique and increasing".into());
    }
    if let Some((_, value, _, _)) = keys.iter().find(|key| key.0 == frame) {
        return Ok(Some(*value));
    }
    if frame <= keys[0].0 {
        return Ok(Some(keys[0].1));
    }
    if frame >= keys[keys.len() - 1].0 {
        return Ok(Some(keys[keys.len() - 1].1));
    }
    let index = keys
        .windows(2)
        .position(|pair| pair[0].0 <= frame && frame <= pair[1].0)
        .ok_or("animation frame has no curve interval")?;
    let (x0, y0, left, kind) = keys[index];
    let (x3, y3, right, _) = keys[index + 1];
    match kind {
        CurveKind::Step => Ok(Some(y0)),
        CurveKind::Linear => Ok(Some(y0 + (frame - x0) / (x3 - x0) * (y3 - y0))),
        CurveKind::Smooth => {
            let x1 = finite_field(&left["next"], "frame")?;
            let y1 = finite_field(&left["next"], "value")?;
            let x2 = finite_field(&right["previous"], "frame")?;
            let y2 = finite_field(&right["previous"], "value")?;
            if !(x0 <= x1 && x1 <= x2 && x2 <= x3) {
                return Err("animation smooth curve has nonmonotonic time controls".into());
            }
            let mut low = 0.0;
            let mut high = 1.0;
            for _ in 0..64 {
                let t = (low + high) * 0.5;
                if cubic(x0, x1, x2, x3, t) < frame {
                    low = t;
                } else {
                    high = t;
                }
            }
            let t = (low + high) * 0.5;
            Ok(Some(cubic(y0, y1, y2, y3, t)))
        }
    }
}

fn curve_kind(token: &str) -> Result<CurveKind, String> {
    match token {
        "Smooth" | "SMOOTH" | "Bezier" | "BEZIER" => Ok(CurveKind::Smooth),
        "Linear" | "LINEAR" => Ok(CurveKind::Linear),
        "Step" | "STEP" | "Stepped" | "STEPPED" => Ok(CurveKind::Step),
        _ => Err(format!("unsupported CAN3 curve type {token:?}")),
    }
}

fn cubic(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

fn validate_expression_name(name: &str) -> Result<(), String> {
    if name.trim() != name
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
    {
        return Err(
            "expression name must be a nonempty printable string of at most 128 bytes".into(),
        );
    }
    Ok(())
}

fn finite_field(value: &Value, field: &str) -> Result<f64, String> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{field} must be finite"))
}

fn integer_field(value: &Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{field} must be an integer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Value {
        json!({
            "format":"live2d.authoring.source.v1","name":"Model",
            "parameters":[{"guid":"pg","id":"Param","minimum":0.0,"maximum":1.0,"default":0.25}],
            "sources":[]
        })
    }

    fn animation(curve_type: &str) -> Value {
        json!({
            "format":"live2d.animation.source.v1",
            "resources":[{"guid":"rg","name":"Model","sourceFile":"not-followed.cmo3"}],
            "scenes":[{
                "guid":"sg","name":"Scene","fps":10.0,"durationFrames":20,
                "tracks":[{
                    "guid":"tg","resourceGuid":"rg","start":5,"internalOffset":-2,"duration":10,
                    "parameterCurves":[{
                        "id":"live2dParam_Param","parameterId":"Param","parameterGuid":"pg",
                        "sourceParameter":{"type":"CParameterId","value":"Param"},
                        "range":[0.0,1.0],
                        "points":[
                            {"anchor":{"frame":0,"value":0.0},"previous":{"frame":0.0,"value":0.0},
                             "next":{"frame":0.0,"value":0.0}},
                            {"anchor":{"frame":10,"value":1.0},"previous":{"frame":10.0,"value":1.0},
                             "next":{"frame":10.0,"value":1.0}}
                        ],
                        "curveTypes":[curve_type,curve_type]
                    }]
                }]
            }]
        })
    }

    fn legacy_part_curve(animation: &Value, part_guid: &str) -> Value {
        let mut curve = animation["scenes"][0]["tracks"][0]["parameterCurves"][0].clone();
        curve["id"] = json!("live2DPartsOpacity:legacy-attribute");
        curve["legacyMapPartId"] = json!("legacy-map-part");
        curve["legacyAttributeId"] = json!("legacy-attribute");
        curve["partGuid"] = json!(part_guid);
        curve["sourcePart"] = json!({"type":"CPartId","value":"PartArmA"});
        for field in ["parameterId", "parameterGuid", "sourceParameter"] {
            curve.as_object_mut().unwrap().remove(field);
        }
        curve
    }

    #[test]
    fn part_opacity_samples_are_separate_from_parameters_and_reject_overlap() {
        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid","id":"Part","data":{"type":"Part"}}]);
        let mut animation = animation("Linear");
        let track = &mut animation["scenes"][0]["tracks"][0];
        let mut curve = track["parameterCurves"][0].clone();
        curve["id"] = json!("live2DPartsOpacity_Part");
        curve["partId"] = json!("Part");
        curve["partGuid"] = json!("part-guid");
        curve["sourcePart"] = json!({"type":"CPartId","value":"Part"});
        curve.as_object_mut().unwrap().remove("parameterId");
        curve.as_object_mut().unwrap().remove("parameterGuid");
        curve.as_object_mut().unwrap().remove("sourceParameter");
        track["partOpacityCurves"] = json!([curve]);
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let sampled = sample(&animation, &model, "rg", "sg", 8.0, &base).unwrap();
        assert_eq!(sampled.pose_signals["Part"], 0.1);
        assert_eq!(sampled.part_opacities["part-guid"], 0.1);
        assert!(!sampled.values.contains_key("Part"));
        let outside = sample(&animation, &model, "rg", "sg", 15.0, &base).unwrap();
        assert!(outside.pose_signals.is_empty() && outside.part_opacities.is_empty());
        let mut bad = animation.clone();
        bad["scenes"][0]["tracks"][0]["partOpacityCurves"][0]["partGuid"] = json!("missing");
        assert!(sample(&bad, &model, "rg", "sg", 8.0, &base).is_err());
        let second = animation["scenes"][0]["tracks"][0].clone();
        animation["scenes"][0]["tracks"].as_array_mut().unwrap().push(second);
        assert!(sample(&animation, &model, "rg", "sg", 8.0, &base).is_err());
    }

    #[test]
    fn legacy_part_opacity_binds_by_guid_and_uses_track_timing_for_pose_and_render() {
        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid","id":"PartArmA","data":{"type":"Part"}}]);
        let mut animation = animation("Linear");
        let legacy = legacy_part_curve(&animation, "part-guid");
        animation["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"] =
            json!([legacy]);
        let base = BTreeMap::from([("Param".into(), 0.25)]);

        let seek = sample(&animation, &model, "rg", "sg", 8.0, &base).unwrap();
        assert_eq!(seek.pose_signals["PartArmA"], 0.1);
        assert_eq!(seek.part_opacities["part-guid"], 0.1);
        assert_eq!(seek.values["Param"], 0.1);
        let before_track = sample(&animation, &model, "rg", "sg", 4.0, &base).unwrap();
        assert!(before_track.pose_signals.is_empty() && before_track.part_opacities.is_empty());
        let after_track = sample(&animation, &model, "rg", "sg", 15.0, &base).unwrap();
        assert!(after_track.pose_signals.is_empty() && after_track.part_opacities.is_empty());
    }

    #[test]
    fn unbound_legacy_part_opacity_never_writes_and_bound_legacy_overlap_is_rejected() {
        let mut animation = animation("Linear");
        let legacy = legacy_part_curve(&animation, "unmatched-guid");
        animation["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"] =
            json!([legacy]);
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let unbound = sample(&animation, &model(), "rg", "sg", 8.0, &base).unwrap();
        assert!(unbound.pose_signals.is_empty() && unbound.part_opacities.is_empty());

        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid","id":"PartArmA","data":{"type":"Part"}}]);
        animation["scenes"][0]["tracks"][0]["legacyPartOpacityCurves"][0]["partGuid"] = json!("part-guid");
        let mut modern = animation["scenes"][0]["tracks"][0]["parameterCurves"][0].clone();
        modern["id"] = json!("live2DPartsOpacity_PartArmA");
        modern["partId"] = json!("PartArmA");
        modern["partGuid"] = json!("part-guid");
        modern["sourcePart"] = json!({"type":"CPartId","value":"PartArmA"});
        for field in ["parameterId", "parameterGuid", "sourceParameter"] {
            modern.as_object_mut().unwrap().remove(field);
        }
        animation["scenes"][0]["tracks"][0]["partOpacityCurves"] = json!([modern]);
        assert!(sample(&animation, &model, "rg", "sg", 8.0, &base).is_err());
    }

    fn part_curve(animation: &Value, family: &str, identity: &str) -> Value {
        let mut curve = legacy_part_curve(animation, "part-guid");
        if family == "legacyPartOpacityCurves" {
            curve["id"] = json!(format!("live2DPartsOpacity:{identity}"));
            curve["legacyAttributeId"] = json!(identity);
        } else {
            for field in ["legacyMapPartId", "legacyAttributeId"] {
                curve.as_object_mut().unwrap().remove(field);
            }
            if family == "partOpacityCurves" {
                curve["id"] = json!("live2DPartsOpacity_PartArmA");
                curve["partId"] = json!("PartArmA");
            } else {
                curve["id"] = json!("shared-attribute-id");
                curve["mapKey"] = json!(format!("live2DPartsOpacity_{identity}"));
            }
        }
        curve
    }

    #[test]
    fn underscore_part_opacity_uses_map_keys_and_exact_guids_with_track_timing() {
        let mut model = model();
        model["sources"] = json!([
            {"guid":"part-guid", "id":"PartArmA", "data":{"type":"Part"}},
            {"guid":"second-guid", "id":"PartArmB", "data":{"type":"Part"}}]);
        let mut animation = animation("Linear");
        let first = part_curve(&animation, "underscorePartOpacityCurves", "first");
        let mut second = part_curve(&animation, "underscorePartOpacityCurves", "second");
        second["partGuid"] = json!("second-guid");
        second["sourcePart"] = json!({"type":"s", "value":"retained display text"});
        for point in second["points"].as_array_mut().unwrap() {
            point["anchor"]["value"] = json!(0.8);
        }
        animation["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"] = json!([first, second]);
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let preview = sample(&animation, &model, "rg", "sg", 8.0, &base).unwrap();
        assert_eq!(preview.pose_signals, BTreeMap::from([("PartArmA".into(), 0.1), ("PartArmB".into(), 0.8)]));
        assert_eq!(preview.part_opacities, BTreeMap::from([("part-guid".into(), 0.1), ("second-guid".into(), 0.8)]));
        assert_eq!(preview.values["Param"], 0.1);
        for frame in [4.0, 15.0] {
            let preview = sample(&animation, &model, "rg", "sg", frame, &base).unwrap();
            assert!(preview.pose_signals.is_empty() && preview.part_opacities.is_empty());
        }
        let report = crate::animation_source::bindings(&animation, &model, "rg").unwrap();
        assert_eq!(report["underscorePartBindings"][0]["mapKey"], "live2DPartsOpacity_first");
        assert_eq!(report["underscorePartBindings"][1]["partId"], "PartArmB");
        assert_eq!(report["underscorePartBindings"][1]["sourcePart"]["value"], "retained display text");
        animation["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][1]["mapKey"] =
            json!("live2DPartsOpacity_first");
        assert!(sample(&animation, &model, "rg", "sg", 8.0, &base).is_err());
    }

    #[test]
    fn empty_part_opacity_channels_do_not_conflict_with_actual_writers() {
        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid", "id":"PartArmA", "data":{"type":"Part"}}]);
        for (left, right) in [("partOpacityCurves", "legacyPartOpacityCurves"),
            ("partOpacityCurves", "underscorePartOpacityCurves"),
            ("legacyPartOpacityCurves", "underscorePartOpacityCurves"),
            ("underscorePartOpacityCurves", "underscorePartOpacityCurves")] {
            for (left_empty, right_empty) in [(true, true), (true, false), (false, true), (false, false)] {
                let mut animation = animation("Linear");
                let mut a = part_curve(&animation, left, "a");
                let mut b = part_curve(&animation, right, "b");
                for (curve, empty) in [(&mut a, left_empty), (&mut b, right_empty)] {
                    if empty { curve["points"] = json!([]); curve["curveTypes"] = json!([]); }
                }
                let track = &mut animation["scenes"][0]["tracks"][0];
                track[left] = json!([a]);
                if left == right { track[right].as_array_mut().unwrap().push(b); }
                else { track[right] = json!([b]); }
                let result = sample(&animation, &model, "rg", "sg", 8.0, &BTreeMap::from([("Param".into(), 0.25)]));
                if !left_empty && !right_empty {
                    assert_eq!(result.err().unwrap(), "multiple active animation curves write the same Part", "{left}/{right}");
                } else {
                    let preview = result.unwrap_or_else(|error| panic!("{left}/{right} empty={left_empty}/{right_empty}: {error}"));
                    let expected = if left_empty && right_empty { BTreeMap::new() }
                        else { BTreeMap::from([("PartArmA".into(), 0.1)]) };
                    assert_eq!(preview.pose_signals, expected, "{left}/{right} empty={left_empty}/{right_empty}");
                }
            }
        }
    }

    #[test]
    fn underscore_part_opacity_never_guesses_an_unmatched_guid_and_validates_bound_sequences() {
        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid", "id":"PartArmA", "data":{"type":"Part"}}]);
        let mut animation = animation("Linear");
        let mut curve = part_curve(&animation, "underscorePartOpacityCurves", "a");
        curve["partGuid"] = json!("unmatched-guid");
        animation["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"] = json!([curve]);
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let preview = sample(&animation, &model, "rg", "sg", 8.0, &base).unwrap();
        assert!(preview.pose_signals.is_empty() && preview.part_opacities.is_empty());
        let report = crate::animation_source::bindings(&animation, &model, "rg").unwrap();
        assert_eq!(report["underscorePartBindings"], json!([]));
        assert_eq!(report["unboundUnderscorePartCurves"], json!([{"sceneGuid":"sg", "trackGuid":"tg",
            "curveId":"shared-attribute-id", "mapKey":"live2DPartsOpacity_a", "partGuid":"unmatched-guid",
            "reason":"partGuidNotFound", "sourcePart":{"type":"CPartId", "value":"PartArmA"}}]));
        animation["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][0]["partGuid"] = json!("part-guid");
        for (field, value) in [("points", json!([])), ("points", Value::Null),
            ("curveTypes", json!([])), ("curveTypes", json!(["BAD", "BAD"]))] {
            let mut bad = animation.clone();
            bad["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][0][field] = value;
            assert!(sample(&bad, &model, "rg", "sg", 8.0, &base).is_err(), "{field}");
        }
        for (value, opacity) in [(-2.0, 0.0), (2.0, 1.0)] {
            for point in animation["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][0]["points"].as_array_mut().unwrap() {
                point["anchor"]["value"] = json!(value);
            }
            let preview = sample(&animation, &model, "rg", "sg", 8.0, &base).unwrap();
            assert_eq!(preview.part_opacities["part-guid"], opacity);
            assert_eq!(preview.pose_signals["PartArmA"], opacity);
        }
    }

    #[test]
    fn only_sampled_part_opacity_values_conflict_across_active_tracks() {
        let mut model = model();
        model["sources"] = json!([{"guid":"part-guid", "id":"PartArmA", "data":{"type":"Part"}}]);
        for left in ["partOpacityCurves", "legacyPartOpacityCurves", "underscorePartOpacityCurves"] {
            for (left_empty, right_empty) in [(true, true), (true, false), (false, true), (false, false)] {
                let mut animation = animation("Linear");
                let mut a = part_curve(&animation, left, "a");
                let mut b = part_curve(&animation, "underscorePartOpacityCurves", "b");
                for (curve, empty) in [(&mut a, left_empty), (&mut b, right_empty)] {
                    if empty { curve["points"] = json!([]); curve["curveTypes"] = json!([]); }
                }
                let mut second = animation["scenes"][0]["tracks"][0].clone();
                second["guid"] = json!("other-track");
                second["parameterCurves"] = json!([]);
                second["underscorePartOpacityCurves"] = json!([b]);
                animation["scenes"][0]["tracks"][0][left] = json!([a]);
                animation["scenes"][0]["tracks"].as_array_mut().unwrap().push(second);
                let result = sample(&animation, &model, "rg", "sg", 8.0, &BTreeMap::from([("Param".into(), 0.25)]));
                if !left_empty && !right_empty {
                    assert_eq!(result.err().unwrap(), "overlapping animation tracks write the same channel");
                } else {
                    let preview = result.unwrap();
                    if left_empty && right_empty { assert!(preview.part_opacities.is_empty()); }
                    else { assert_eq!(preview.part_opacities["part-guid"], 0.1); }
                }
            }
        }
    }

    #[test]
    fn samples_curve_types_offsets_boundaries_and_loop_period() {
        let model = model();
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let mut linear = sample(&animation("Linear"), &model, "rg", "sg", 8.0, &base).unwrap();
        assert!((linear.values["Param"] - 0.1).abs() < 1e-12);
        assert_eq!(
            sample(&animation("Bezier"), &model, "rg", "sg", 10.0, &base)
                .unwrap()
                .values["Param"],
            0.3
        );
        assert_eq!(
            sample(&animation("Step"), &model, "rg", "sg", 8.0, &base)
                .unwrap()
                .values["Param"],
            0.0
        );
        assert_eq!(
            sample(&animation("Linear"), &model, "rg", "sg", 15.0, &base)
                .unwrap()
                .values["Param"],
            0.25
        );
        assert_eq!(
            sample(&animation("Linear"), &model, "rg", "sg", 200.0, &base)
                .unwrap()
                .frame,
            19.0
        );
        assert_eq!(
            sample(&animation("Linear"), &model, "rg", "sg", 200.0, &base)
                .unwrap()
                .clock_frame,
            20.0
        );
        linear.frame = 19.5;
        linear.clock_frame = 19.5;
        assert_eq!(advance_frame(&linear, 0.2, true).unwrap(), 1.5);
        assert_eq!(advance_frame(&linear, 1.0, false).unwrap(), 20.0);
    }

    #[test]
    fn empty_source_parameter_metadata_keeps_native_sample_values() {
        let model = model();
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let expected = BTreeMap::from([("Param".into(), 0.1)]);
        let populated = sample(&animation("Linear"), &model, "rg", "sg", 8.0, &base).unwrap();
        let mut empty_metadata = animation("Linear");
        empty_metadata["scenes"][0]["tracks"][0]["parameterCurves"][0]["sourceParameter"] =
            Value::Null;
        let empty = sample(&empty_metadata, &model, "rg", "sg", 8.0, &base).unwrap();

        assert_eq!(populated.values, expected);
        assert_eq!(empty.values, expected);
    }

    #[test]
    fn zero_and_one_frame_timelines_keep_valid_clock_bounds() {
        let model = model();
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        for duration in [0, 1] {
            let mut animation = animation("Linear");
            animation["scenes"][0]["durationFrames"] = json!(duration);
            let active = sample(&animation, &model, "rg", "sg", 10.0, &base).unwrap();
            assert_eq!(active.frame, 0.0);
            assert_eq!(active.clock_frame, duration as f64);
            assert_eq!(advance_frame(&active, 1.0, false).unwrap(), duration as f64);
            assert_eq!(advance_frame(&active, 1.0, true).unwrap(), 0.0);
        }
    }

    #[test]
    fn binding_curve_and_track_conflicts_are_errors_without_preview_mutation() {
        let model = model();
        let base = BTreeMap::from([("Param".into(), 0.25)]);
        let mut state = State::default();
        state.active = Some(sample(&animation("Linear"), &model, "rg", "sg", 8.0, &base).unwrap());
        let previous = state.active.as_ref().unwrap().values.clone();
        let mut unbound = animation("Linear");
        unbound["scenes"][0]["tracks"][0]["parameterCurves"][0]["parameterGuid"] = json!("missing");
        assert!(sample(&unbound, &model, "rg", "sg", 9.0, &base).is_err());
        let mut unsafe_curve = animation("Bezier");
        unsafe_curve["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0]["next"]
            ["frame"] = json!(12.0);
        assert!(sample(&unsafe_curve, &model, "rg", "sg", 9.0, &base).is_err());
        assert!(sample(&animation("UNKNOWN"), &model, "rg", "sg", 8.0, &base).is_err());
        let mut overlap = animation("Linear");
        let second_track = overlap["scenes"][0]["tracks"][0].clone();
        overlap["scenes"][0]["tracks"]
            .as_array_mut()
            .unwrap()
            .push(second_track);
        assert!(sample(&overlap, &model, "rg", "sg", 8.0, &base).is_err());
        let mut duplicate_curve = animation("Linear");
        let curve = duplicate_curve["scenes"][0]["tracks"][0]["parameterCurves"][0].clone();
        duplicate_curve["scenes"][0]["tracks"][0]["parameterCurves"]
            .as_array_mut()
            .unwrap()
            .push(curve);
        assert!(sample(&duplicate_curve, &model, "rg", "sg", 8.0, &base).is_err());
        duplicate_curve["scenes"][0]["tracks"][0]["parameterCurves"][1]["curveTypes"][0] = json!(2);
        assert!(sample(&duplicate_curve, &model, "rg", "sg", 8.0, &base).is_err());
        assert_eq!(state.active.as_ref().unwrap().values, previous);
    }

    #[test]
    fn expressions_apply_once_after_runtime_values_and_clamp() {
        let parameters = vec![
            json!({"id":"Add","minimum":0.0,"maximum":10.0}),
            json!({"id":"Multiply","minimum":0.0,"maximum":10.0}),
            json!({"id":"Overwrite","minimum":0.0,"maximum":10.0}),
        ];
        let source = br#"{"Type":"Live2D Expression","Parameters":[
            {"Id":"Add","Value":3,"Blend":"Add"},
            {"Id":"Multiply","Value":4,"Blend":"Multiply"},
            {"Id":"Overwrite","Value":99,"Blend":"Overwrite"}]}"#;
        let mut state = State::default();
        state
            .import_expression("Test", "test.exp3.json", &parameters, source)
            .unwrap();
        state.apply_expression("Test", false).unwrap();
        let base = BTreeMap::from([
            ("Add".into(), 2.0),
            ("Multiply".into(), 2.0),
            ("Overwrite".into(), 2.0),
        ]);
        let first = state.effective_values(&base, &parameters).unwrap();
        assert_eq!(state.effective_values(&base, &parameters).unwrap(), first);
        assert_eq!(
            first,
            BTreeMap::from([
                ("Add".into(), 5.0),
                ("Multiply".into(), 8.0),
                ("Overwrite".into(), 10.0)
            ])
        );
        state
            .import_expression(
                "OmittedBlend",
                "omitted.exp3.json",
                &parameters,
                br#"{"Type":"Live2D Expression","Parameters":[
                    {"Id":"Add","Value":3}]}"#,
            )
            .unwrap();
        state.apply_expression("OmittedBlend", false).unwrap();
        assert_eq!(
            state.effective_values(&base, &parameters).unwrap()["Add"],
            5.0
        );
        state.apply_expression("Test", false).unwrap();
        assert!(state
            .import_expression("Test", "test.exp3.json", &parameters, source)
            .is_err());
        let invalid = br#"{"Type":"Live2D Expression","Parameters":[
            {"Id":"missing","Value":1,"Blend":"Add"}]}"#;
        assert!(state
            .import_expression("Other", "other.exp3.json", &parameters, invalid)
            .is_err());
        let expressions_before_malformed = state.expression_list()["expressions"].clone();
        for (name, blend) in [
            ("NullBlend", "null"),
            ("NumberBlend", "3"),
            ("UnknownBlend", "\"Unknown\""),
        ] {
            let malformed = format!(
                r#"{{"Type":"Live2D Expression","Parameters":[
                    {{"Id":"Add","Value":1,"Blend":{blend}}}]}}"#
            );
            assert!(state
                .import_expression(name, &format!("{name}.exp3.json"), &parameters, malformed.as_bytes())
                .is_err());
        }
        assert_eq!(
            state.expression_list()["expressions"]
                .as_array()
                .unwrap()
                .len(),
            expressions_before_malformed.as_array().unwrap().len()
        );
        assert_eq!(state.expression_name.as_deref(), Some("Test"));
        state
            .import_expression(
                "Neutral",
                "neutral.exp3.json",
                &parameters,
                br#"{"Type":"Live2D Expression","Parameters":[]}"#,
            )
            .unwrap();
        state.apply_expression("Neutral", false).unwrap();
        assert_eq!(state.effective_values(&base, &parameters).unwrap(), base);
    }

    #[test]
    fn finite_extreme_expression_blends_saturate_to_authored_bounds() {
        let parameters = vec![
            json!({"id":"Add","minimum":-10.0,"maximum":10.0}),
            json!({"id":"Multiply","minimum":-10.0,"maximum":10.0}),
        ];
        let source = br#"{"Type":"Live2D Expression","Parameters":[
            {"Id":"Add","Value":1e308,"Blend":"Add"},
            {"Id":"Multiply","Value":1e308,"Blend":"Multiply"}]}"#;
        let mut state = State::default();
        state
            .import_expression("Extreme", "extreme.exp3.json", &parameters, source)
            .unwrap();
        state.apply_expression("Extreme", false).unwrap();
        let base = BTreeMap::from([("Add".into(), 2.0), ("Multiply".into(), 2.0)]);
        assert_eq!(
            state.effective_values(&base, &parameters).unwrap(),
            BTreeMap::from([("Add".into(), 10.0), ("Multiply".into(), 10.0)])
        );
    }
}
