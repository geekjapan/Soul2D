use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Clone, Copy)]
struct Point {
    time: f64,
    value: f64,
}

struct Motion {
    duration: f64,
    curves: Vec<Curve>,
    fade_in: Option<f64>,
    fade_out: Option<f64>,
}

struct Curve {
    target: String,
    id: String,
    /// The curve's own fades, used in place of the motion's (zero is an instant change, not the motion's fade).
    fade_in: Option<f64>,
    fade_out: Option<f64>,
    points: Vec<Point>,
    segments: Vec<Segment>,
}

enum Segment {
    Linear {
        start: Point,
        end: Point,
    },
    Bezier {
        start: Point,
        control1: Point,
        control2: Point,
        end: Point,
        restricted: bool,
    },
    Stepped {
        start: Point,
        end: Point,
    },
    InverseStepped {
        end: Point,
    },
}

pub(crate) fn validate(motion: &Value) -> Result<(), String> {
    parse_motion(motion).map(|_| ())
}

pub(crate) fn end_time(motion: &Value) -> Result<f64, String> {
    let parsed = parse_motion(motion)?;
    let mut end_time = parsed
        .curves
        .iter()
        .filter_map(|curve| curve.points.last())
        .fold(parsed.duration, |maximum, point| maximum.max(point.time));
    if let Some(events) = motion.get("UserData").and_then(Value::as_array) {
        for event in events {
            let time = event
                .get("Time")
                .ok_or("motion3.UserData event time is required")?;
            end_time = end_time.max(finite_number(time, "motion3.UserData.Time")?);
        }
    }
    Ok(end_time)
}

pub(crate) fn sample(motion: &Value, time: f64) -> Result<Value, String> {
    if !time.is_finite() {
        return Err("motion sample time must be finite".into());
    }
    let parsed = parse_motion(motion)?;
    let curves = parsed
        .curves
        .iter()
        .map(|curve| {
            let value = curve.sample(time);
            if !value.is_finite() {
                return Err(format!(
                    "motion curve {:?}/{:?} sampled to a non-finite value",
                    curve.target, curve.id
                ));
            }
            Ok(json!({
                "target": &curve.target,
                "id": &curve.id,
                "value": value,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({
        "time": time,
        "duration": parsed.duration,
        "curves": curves,
    }))
}

/// Playback with fades (`fades: true` on `motion3.seek`): the seconds played since the seek, and whether the clip
/// loops (a looping clip never fades out).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FadeClock {
    pub(crate) elapsed: f64,
    pub(crate) looping: bool,
}

/// Each curve's weight (in curve order) at clip `time`: the sine ease Cubism's runtimes use, over the curve's fade
/// or else the motion's, in from the clock's start and out towards `clip_end`.
pub(crate) fn weights(motion: &Value, time: f64, clip_end: f64, clock: FadeClock) -> Result<Vec<f64>, String> {
    let parsed = parse_motion(motion)?;
    let ease = fade_weight;
    Ok(parsed
        .curves
        .iter()
        .map(|curve| {
            let fade_in = ease(clock.elapsed, curve.fade_in.or(parsed.fade_in));
            let fade_out = if clock.looping { 1.0 } else { ease(clip_end - time, curve.fade_out.or(parsed.fade_out)) };
            fade_in * fade_out
        })
        .collect())
}

/// The sine ease Cubism's runtimes use, `seconds` into a fade of `fade` seconds; no fade (or zero) is full weight.
pub(crate) fn fade_weight(seconds: f64, fade: Option<f64>) -> f64 {
    match fade {
        Some(fade) if fade > 0.0 => 0.5 - 0.5 * (std::f64::consts::PI * (seconds / fade).clamp(0.0, 1.0)).cos(),
        _ => 1.0,
    }
}

/// `motion3.fade.set`: the motion's fades (`Meta`) or, with `curve`, one curve's. A number sets the time in seconds
/// and `null` removes it; an absent key is left as it is.
pub(crate) fn set_fade(motion: &Value, request: &Value) -> Result<Value, String> {
    let parsed = parse_motion(motion)?;
    let mut edited = motion.clone();
    let target = match request.get("curve") {
        None | Some(Value::Null) => &mut edited["Meta"],
        Some(index) => {
            let index = index.as_u64().map(|index| index as usize).filter(|index| *index < parsed.curves.len());
            let index = index.ok_or("curve must be the index of a curve of the motion")?;
            &mut edited["Curves"][index]
        }
    };
    let target = target.as_object_mut().ok_or("motion3 fade target must be an object")?;
    let mut changed = false;
    for (field, key) in [("fadeIn", "FadeInTime"), ("fadeOut", "FadeOutTime")] {
        match request.get(field) {
            None => {}
            Some(Value::Null) => {
                target.remove(key);
                changed = true;
            }
            Some(value) => {
                let seconds = value.as_f64().filter(|seconds| seconds.is_finite() && *seconds >= 0.0);
                let seconds =
                    seconds.ok_or_else(|| format!("{field} must be a nonnegative number of seconds or null"))?;
                target.insert(key.to_owned(), json!(seconds));
                changed = true;
            }
        }
    }
    if !changed {
        return Err("give fadeIn or fadeOut".into());
    }
    parse_motion(&edited)?;
    Ok(edited)
}

pub(crate) fn set_point(motion: &Value, request: &Value) -> Result<Value, String> {
    let parsed = parse_motion(motion)?;
    let request = request
        .as_object()
        .ok_or("motion point request must be an object")?;
    let curve_index = request_index(request, "curve")?;
    let point_index = request_index(request, "point")?;
    let curve = parsed
        .curves
        .get(curve_index)
        .ok_or_else(|| format!("motion curve index {curve_index} is out of range"))?;
    let point = curve
        .points
        .get(point_index)
        .ok_or_else(|| format!("motion point index {point_index} is out of range"))?;

    let new_time = request
        .get("time")
        .map(|value| finite_number(value, "time"))
        .transpose()?;
    let new_value = request
        .get("value")
        .map(|value| finite_number(value, "value"))
        .transpose()?;
    if new_time.is_none() && new_value.is_none() {
        return Err("motion point request requires time or value".into());
    }
    if new_time.is_some_and(|time| time < 0.0) {
        return Err("motion point time must be nonnegative".into());
    }

    let time_changed = new_time.is_some_and(|time| time != point.time);
    let mut candidate = motion.clone();
    let has_beziers = parsed.curves.iter().any(|curve| {
        curve
            .segments
            .iter()
            .any(|segment| matches!(segment, Segment::Bezier { .. }))
    });
    let normalize_restricted = time_changed
        && has_beziers
        && motion["Meta"]["AreBeziersRestricted"]
            .as_bool()
            .unwrap_or(false);
    if normalize_restricted {
        canonicalize_bezier_control_times(&mut candidate)?;
        candidate["Meta"]["AreBeziersRestricted"] = json!(false);
    }

    {
        let flat = candidate["Curves"][curve_index]["Segments"]
            .as_array_mut()
            .ok_or("motion curve segments must be an array")?;
        let (time_offset, value_offset) = logical_point_offsets(flat)?
            .get(point_index)
            .copied()
            .ok_or_else(|| format!("motion point index {point_index} is out of range"))?;
        if let Some(time) = new_time {
            flat[time_offset] = json!(time);
        }
        if let Some(value) = new_value {
            flat[value_offset] = json!(value);
        }
    }

    if new_time.is_some_and(|time| time > parsed.duration) {
        let edited = parse_motion(&candidate)?;
        let mut duration = edited
            .curves
            .iter()
            .filter_map(|curve| curve.points.last())
            .fold(parsed.duration, |maximum, point| maximum.max(point.time));
        if let Some(events) = candidate.get("UserData").and_then(Value::as_array) {
            for event in events {
                let event_time = event
                    .get("Time")
                    .and_then(Value::as_f64)
                    .ok_or("motion3.UserData event time must be a finite number")?;
                duration = duration.max(event_time);
            }
        }
        if duration > parsed.duration {
            candidate["Meta"]["Duration"] = json!(duration);
        }
    }
    validate(&candidate)?;
    Ok(candidate)
}

/// Keys the PartOpacity curve of part `id` to `value` at `time` (seconds) with stepped segments, as a pose
/// switch is keyed. A missing curve is added holding `value` from 0, with a stepped segment to `time` so it never
/// has a single point. A key within half a frame of an anchor sets that anchor. Every segment the key touches must
/// be stepped: changing or splitting another kind would fade the switch.
pub(crate) fn key_part(motion: &Value, id: &str, time: f64, value: f64) -> Result<Value, String> {
    let parsed = parse_motion(motion)?;
    let near = 0.5 / motion["Meta"]["Fps"].as_f64().ok_or("motion3.Meta.Fps must be a number")?;
    let mut edited = motion.clone();
    let curves = edited["Curves"].as_array_mut().ok_or("motion3.Curves must be an array")?;
    let (curve_count, segments, points) =
        match curves.iter_mut().find(|curve| curve["Target"] == "PartOpacity" && curve["Id"] == id) {
            None if time == 0.0 => {
                curves.push(json!({"Target": "PartOpacity", "Id": id, "Segments": [0.0, value]}));
                (1, 0, 1)
            }
            None => {
                curves.push(json!({"Target": "PartOpacity", "Id": id, "Segments": [0.0, value, 2, time, value]}));
                (1, 1, 2)
            }
            Some(curve) => {
                let flat = curve["Segments"].as_array_mut().ok_or("motion curve segments must be an array")?;
                let added = key_stepped(flat, time, value, near).map_err(|error| format!("PartOpacity curve {id:?} {error}"))?;
                (0, added, added)
            }
        };
    let meta = &mut edited["Meta"];
    for (key, more) in [("CurveCount", curve_count), ("TotalSegmentCount", segments), ("TotalPointCount", points)] {
        if more > 0 {
            let count = meta[key].as_f64().ok_or_else(|| format!("motion3.Meta.{key} must be a number"))? as u64;
            meta[key] = json!(count + more);
        }
    }
    if points > 0 && time > parsed.duration {
        meta["Duration"] = json!(time);
    }
    validate(&edited)?;
    Ok(edited)
}

/// Sets the anchor within `near` of `time` or adds one there joined by stepped segments; answers how many points
/// were added. Each anchor is listed with the type of the segment that ends at it (stepped for the first).
fn key_stepped(flat: &mut Vec<Value>, time: f64, value: f64, near: f64) -> Result<u64, String> {
    let mut anchors = vec![(0, 2.0)];
    let mut index = 2;
    while index < flat.len() {
        let tag = flat[index].as_f64().ok_or("has a segment type that is not a number")?;
        let end = index + if tag == 1.0 { 5 } else { 1 };
        anchors.push((end, tag));
        index = end + 2;
    }
    let time_at = |offset: usize| flat[offset].as_f64().unwrap_or(f64::NAN);
    let not_stepped = || Err(format!("has a segment that is not stepped at time {time}"));
    if let Some(index) = anchors.iter().position(|(offset, _)| (time_at(*offset) - time).abs() < near) {
        if anchors[index].1 != 2.0 || anchors.get(index + 1).is_some_and(|(_, tag)| *tag != 2.0) {
            return not_stepped();
        }
        flat[anchors[index].0 + 1] = json!(value);
        return Ok(0);
    }
    match anchors.iter().find(|(offset, _)| time_at(*offset) > time) {
        None => flat.extend([json!(2), json!(time), json!(value)]),
        Some((0, _)) => {
            let first = [json!(2), flat[0].clone(), flat[1].clone()];
            flat.splice(0..2, [json!(time), json!(value)].into_iter().chain(first));
        }
        Some(&(offset, tag)) if tag == 2.0 => {
            flat.splice(offset - 1..offset - 1, [json!(2), json!(time), json!(value)]);
        }
        Some(_) => return not_stepped(),
    }
    Ok(1)
}

fn parse_motion(motion: &Value) -> Result<Motion, String> {
    let object = motion
        .as_object()
        .ok_or("motion3 document must be an object")?;
    let version = finite_number(
        object.get("Version").ok_or("motion3.Version is required")?,
        "motion3.Version",
    )?;
    if version != 3.0 {
        return Err("motion3.Version must be 3".into());
    }

    let meta = object
        .get("Meta")
        .and_then(Value::as_object)
        .ok_or("motion3.Meta must be an object")?;
    let duration = finite_number(
        meta.get("Duration")
            .ok_or("motion3.Meta.Duration is required")?,
        "motion3.Meta.Duration",
    )?;
    if duration < 0.0 {
        return Err("motion3.Meta.Duration must be nonnegative".into());
    }
    let fps = finite_number(
        meta.get("Fps").ok_or("motion3.Meta.Fps is required")?,
        "motion3.Meta.Fps",
    )?;
    if fps <= 0.0 {
        return Err("motion3.Meta.Fps must be positive".into());
    }
    for key in ["Loop", "AreBeziersRestricted"] {
        if meta.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("motion3.Meta.{key} must be a boolean"));
        }
    }
    validate_optional_finite(meta, "FadeInTime", "motion3.Meta")?;
    validate_optional_finite(meta, "FadeOutTime", "motion3.Meta")?;
    let restricted = meta
        .get("AreBeziersRestricted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let raw_curves = object
        .get("Curves")
        .and_then(Value::as_array)
        .ok_or("motion3.Curves must be an array")?;
    let mut curves = Vec::with_capacity(raw_curves.len());
    let mut curve_ids = HashSet::with_capacity(raw_curves.len());
    let mut total_segments = 0usize;
    let mut total_points = 0usize;
    for (index, raw_curve) in raw_curves.iter().enumerate() {
        let curve = parse_curve(raw_curve, index, restricted)?;
        if !curve_ids.insert((curve.target.clone(), curve.id.clone())) {
            return Err(format!(
                "motion3.Curves[{index}] duplicates target/id {:?}/{:?}",
                curve.target, curve.id
            ));
        }
        total_segments = total_segments
            .checked_add(curve.segments.len())
            .ok_or("motion3 segment count overflow")?;
        total_points = total_points
            .checked_add(curve.points.len())
            .ok_or("motion3 point count overflow")?;
        curves.push(curve);
    }
    validate_count(meta, "CurveCount", curves.len())?;
    validate_count(meta, "TotalSegmentCount", total_segments)?;
    validate_count(meta, "TotalPointCount", total_points)?;

    let user_data = match object.get("UserData") {
        None => &[][..],
        Some(value) => value
            .as_array()
            .ok_or("motion3.UserData must be an array")?
            .as_slice(),
    };
    let mut user_data_size = 0usize;
    for (index, event) in user_data.iter().enumerate() {
        let event = event
            .as_object()
            .ok_or_else(|| format!("motion3.UserData[{index}] must be an object"))?;
        let time = finite_number(
            event
                .get("Time")
                .ok_or_else(|| format!("motion3.UserData[{index}].Time is required"))?,
            &format!("motion3.UserData[{index}].Time"),
        )?;
        if time < 0.0 {
            return Err(format!(
                "motion3.UserData[{index}].Time must be nonnegative"
            ));
        }
        let value = event
            .get("Value")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("motion3.UserData[{index}].Value must be a string"))?;
        user_data_size = user_data_size
            .checked_add(value.len())
            .ok_or("motion3 user data size overflow")?;
    }
    if meta.contains_key("UserDataCount") {
        validate_count(meta, "UserDataCount", user_data.len())?;
    }
    if meta.contains_key("TotalUserDataSize") {
        validate_count(meta, "TotalUserDataSize", user_data_size)?;
    }

    Ok(Motion { duration, curves, fade_in: fade_time(meta, "FadeInTime"), fade_out: fade_time(meta, "FadeOutTime") })
}

fn parse_curve(raw: &Value, curve_index: usize, restricted: bool) -> Result<Curve, String> {
    let curve = raw
        .as_object()
        .ok_or_else(|| format!("motion3.Curves[{curve_index}] must be an object"))?;
    let target = curve
        .get("Target")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("motion3.Curves[{curve_index}].Target must be a string"))?;
    if !matches!(target, "Model" | "Parameter" | "PartOpacity") {
        return Err(format!(
            "motion3.Curves[{curve_index}].Target is unsupported: {target:?}"
        ));
    }
    let id = curve
        .get("Id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("motion3.Curves[{curve_index}].Id must be a nonempty string"))?;
    validate_optional_finite(
        curve,
        "FadeInTime",
        &format!("motion3.Curves[{curve_index}]"),
    )?;
    validate_optional_finite(
        curve,
        "FadeOutTime",
        &format!("motion3.Curves[{curve_index}]"),
    )?;

    let flat = curve
        .get("Segments")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("motion3.Curves[{curve_index}].Segments must be an array"))?;
    if flat.len() < 2 {
        return Err(format!(
            "motion3.Curves[{curve_index}].Segments must contain an initial point"
        ));
    }

    let path = format!("motion3.Curves[{curve_index}].Segments");
    let initial = parse_point(flat, 0, &path)?;
    let mut points = vec![initial];
    let mut segments = Vec::new();
    let mut index = 2usize;
    let mut start = initial;
    while index < flat.len() {
        let tag = finite_number(&flat[index], &format!("{path}[{index}]"))?;
        match tag {
            0.0 => {
                let end = parse_point(flat, index + 1, &path)?;
                validate_segment_end(start, end, &path, index)?;
                points.push(end);
                segments.push(Segment::Linear { start, end });
                start = end;
                index += 3;
                continue;
            }
            1.0 => {
                let control1 = parse_point(flat, index + 1, &path)?;
                let control2 = parse_point(flat, index + 3, &path)?;
                let end = parse_point(flat, index + 5, &path)?;
                validate_segment_end(start, end, &path, index)?;
                if !restricted && !bezier_x_is_monotonic(start, control1, control2, end) {
                    return Err(format!(
                        "{path}[{index}] has Bezier time handles that are not monotonic"
                    ));
                }
                points.extend([control1, control2, end]);
                segments.push(Segment::Bezier {
                    start,
                    control1,
                    control2,
                    end,
                    restricted,
                });
                start = end;
                index += 7;
                continue;
            }
            2.0 | 3.0 => {
                let end = parse_point(flat, index + 1, &path)?;
                validate_segment_end(start, end, &path, index)?;
                points.push(end);
                let segment = if tag == 2.0 {
                    Segment::Stepped { start, end }
                } else {
                    Segment::InverseStepped { end }
                };
                segments.push(segment);
                start = end;
                index += 3;
                continue;
            }
            _ => {
                return Err(format!(
                    "{path}[{index}] has unsupported segment type {tag}"
                ));
            }
        }
    }

    Ok(Curve {
        target: target.to_owned(),
        id: id.to_owned(),
        fade_in: fade_time(curve, "FadeInTime"),
        fade_out: fade_time(curve, "FadeOutTime"),
        points,
        segments,
    })
}

impl Curve {
    fn sample(&self, time: f64) -> f64 {
        let Some(first) = self.points.first() else {
            return 0.0;
        };
        let time = time.max(first.time);
        if self.segments.is_empty() {
            return first.value;
        }
        for segment in &self.segments {
            if time < segment.end().time {
                return segment.sample(time);
            }
        }
        self.points
            .last()
            .map(|point| point.value)
            .unwrap_or(first.value)
    }
}

impl Segment {
    fn end(&self) -> Point {
        match self {
            Segment::Linear { end, .. }
            | Segment::Bezier { end, .. }
            | Segment::Stepped { end, .. }
            | Segment::InverseStepped { end, .. } => *end,
        }
    }

    fn sample(&self, time: f64) -> f64 {
        match self {
            Segment::Linear { start, end } => {
                let amount = (time - start.time) / (end.time - start.time);
                lerp(start.value, end.value, amount)
            }
            Segment::Bezier {
                start,
                control1,
                control2,
                end,
                restricted,
            } => {
                let span = end.time - start.time;
                let amount = if *restricted {
                    (time - start.time) / span
                } else {
                    invert_bezier_time(start.time, control1.time, control2.time, end.time, time)
                };
                cubic(
                    start.value,
                    control1.value,
                    control2.value,
                    end.value,
                    amount,
                )
            }
            Segment::Stepped { start, .. } => start.value,
            Segment::InverseStepped { end, .. } => end.value,
        }
    }
}

fn parse_point(flat: &[Value], index: usize, path: &str) -> Result<Point, String> {
    let time = finite_number(
        flat.get(index)
            .ok_or_else(|| format!("{path} is missing a point time at index {index}"))?,
        &format!("{path}[{index}]"),
    )?;
    let value_index = index
        .checked_add(1)
        .ok_or_else(|| format!("{path} point index overflow"))?;
    let value = finite_number(
        flat.get(value_index)
            .ok_or_else(|| format!("{path} is missing a point value at index {value_index}"))?,
        &format!("{path}[{value_index}]"),
    )?;
    if time < 0.0 {
        return Err(format!("{path}[{index}] point time must be nonnegative"));
    }
    Ok(Point { time, value })
}

fn validate_segment_end(start: Point, end: Point, path: &str, index: usize) -> Result<(), String> {
    if end.time <= start.time {
        return Err(format!(
            "{path}[{index}] segment end time must be strictly greater than its start"
        ));
    }
    Ok(())
}

fn bezier_x_is_monotonic(start: Point, control1: Point, control2: Point, end: Point) -> bool {
    let span = end.time - start.time;
    if !span.is_finite() || span <= 0.0 {
        return false;
    }
    let x1 = (control1.time - start.time) / span;
    let x2 = (control2.time - start.time) / span;
    if !x1.is_finite() || !x2.is_finite() {
        return false;
    }

    // The cubic's derivative is quadratic. Check its endpoints and any interior minimum.
    let constant = x1;
    let linear = 2.0 * (x2 - 2.0 * x1);
    let quadratic = 1.0 - 3.0 * x2 + 3.0 * x1;
    if !linear.is_finite() || !quadratic.is_finite() {
        return false;
    }
    let mut minimum = constant.min(constant + linear + quadratic);
    if quadratic > 0.0 {
        let vertex = -linear / (2.0 * quadratic);
        if vertex > 0.0 && vertex < 1.0 {
            minimum = minimum.min(constant + linear * vertex + quadratic * vertex * vertex);
        }
    }
    minimum >= 0.0
}

fn invert_bezier_time(start: f64, control1: f64, control2: f64, end: f64, time: f64) -> f64 {
    let span = end - start;
    let x1 = (control1 - start) / span;
    let x2 = (control2 - start) / span;
    let target = (time - start) / span;
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..64 {
        let middle = (low + high) * 0.5;
        if cubic(0.0, x1, x2, 1.0, middle) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) * 0.5
}

fn cubic(p0: f64, p1: f64, p2: f64, p3: f64, amount: f64) -> f64 {
    let p01 = lerp(p0, p1, amount);
    let p12 = lerp(p1, p2, amount);
    let p23 = lerp(p2, p3, amount);
    let p012 = lerp(p01, p12, amount);
    let p123 = lerp(p12, p23, amount);
    lerp(p012, p123, amount)
}

fn lerp(start: f64, end: f64, amount: f64) -> f64 {
    if start.is_sign_negative() == end.is_sign_negative() {
        start + (end - start) * amount
    } else {
        start * (1.0 - amount) + end * amount
    }
}

fn logical_point_offsets(flat: &[Value]) -> Result<Vec<(usize, usize)>, String> {
    let mut offsets = vec![(0, 1)];
    let mut index = 2usize;
    while index < flat.len() {
        let tag = finite_number(&flat[index], "motion segment type")?;
        match tag {
            0.0 | 2.0 | 3.0 => {
                offsets.push((index + 1, index + 2));
                index += 3;
            }
            1.0 => {
                offsets.extend([
                    (index + 1, index + 2),
                    (index + 3, index + 4),
                    (index + 5, index + 6),
                ]);
                index += 7;
            }
            _ => return Err(format!("unsupported motion segment type {tag}")),
        }
    }
    Ok(offsets)
}

fn canonicalize_bezier_control_times(motion: &mut Value) -> Result<(), String> {
    let curves = motion
        .get_mut("Curves")
        .and_then(Value::as_array_mut)
        .ok_or("motion3.Curves must be an array")?;
    for (curve_index, curve) in curves.iter_mut().enumerate() {
        let flat = curve
            .get_mut("Segments")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| format!("motion3.Curves[{curve_index}].Segments must be an array"))?;
        let path = format!("motion3.Curves[{curve_index}].Segments");
        let mut start_time = finite_number(
            flat.first()
                .ok_or_else(|| format!("{path} is missing its initial point"))?,
            &format!("{path}[0]"),
        )?;
        let mut index = 2usize;
        while index < flat.len() {
            let tag = finite_number(&flat[index], &format!("{path}[{index}]"))?;
            match tag {
                0.0 | 2.0 | 3.0 => {
                    start_time = finite_number(
                        flat.get(index + 1)
                            .ok_or_else(|| format!("{path} is missing a segment endpoint"))?,
                        &format!("{path}[{}]", index + 1),
                    )?;
                    index += 3;
                }
                1.0 => {
                    let end_time = finite_number(
                        flat.get(index + 5)
                            .ok_or_else(|| format!("{path} is missing a Bezier endpoint"))?,
                        &format!("{path}[{}]", index + 5),
                    )?;
                    let span = end_time - start_time;
                    if !span.is_finite() || span <= 0.0 {
                        return Err(format!(
                            "{path}[{index}] Bezier endpoint time must be greater than its start"
                        ));
                    }
                    flat[index + 1] = json!(start_time + span / 3.0);
                    flat[index + 3] = json!(start_time + span * (2.0 / 3.0));
                    start_time = end_time;
                    index += 7;
                }
                _ => {
                    return Err(format!(
                        "{path}[{index}] has unsupported segment type {tag}"
                    ))
                }
            }
        }
    }
    Ok(())
}

fn request_index(request: &serde_json::Map<String, Value>, key: &str) -> Result<usize, String> {
    request
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("motion point request requires a nonnegative integer {key}"))
}

fn finite_number(value: &Value, name: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{name} must be a finite number"))
}

/// A fade the file sets: the spec gives no default, so an absent or negative time is unset (a curve then uses the
/// motion's fade, and a motion has none).
fn fade_time(object: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64).filter(|seconds| *seconds >= 0.0)
}

fn validate_optional_finite(
    object: &serde_json::Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<(), String> {
    if let Some(value) = object.get(key) {
        finite_number(value, &format!("{path}.{key}"))?;
    }
    Ok(())
}

fn validate_count(
    object: &serde_json::Map<String, Value>,
    key: &str,
    expected: usize,
) -> Result<(), String> {
    let number = finite_number(
        object
            .get(key)
            .ok_or_else(|| format!("motion3.Meta.{key} is required"))?,
        &format!("motion3.Meta.{key}"),
    )?;
    if number < 0.0 || number.fract() != 0.0 || number != expected as f64 {
        return Err(format!(
            "motion3.Meta.{key} must equal the computed value {expected}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(curves: Value, segments: u64, points: u64) -> Value {
        let count = curves.as_array().unwrap().len();
        json!({"Version":3, "Meta":{"Duration":1, "Fps":30, "CurveCount":count, "TotalSegmentCount":segments,
            "TotalPointCount":points}, "Curves":curves})
    }

    #[test]
    fn part_keys_split_stepped_segments_and_add_curves_with_counts() {
        let base = motion(json!([{"Target":"Parameter","Id":"A","Segments":[0,5,0,1,6]},
            {"Target":"PartOpacity","Id":"A","Segments":[0,1,2,1,0]}]), 2, 4);
        let split = key_part(&base, "A", 0.5, 0.0).unwrap();
        assert_eq!(split["Curves"][1]["Segments"], json!([0,1,2,0.5,0.0,2,1,0]));
        assert_eq!(split["Curves"][0], base["Curves"][0]);
        assert_eq!(split["Meta"], json!({"Duration":1, "Fps":30, "CurveCount":2, "TotalSegmentCount":3,
            "TotalPointCount":5}));
        let near = key_part(&split, "A", 1.01, 1.0).unwrap();
        assert_eq!(near["Curves"][1]["Segments"], json!([0,1,2,0.5,0.0,2,1,1.0]), "within half a frame of 1");
        assert_eq!(near["Meta"], split["Meta"]);
        assert_eq!(key_part(&split, "A", 1.02, 1.0).unwrap()["Curves"][1]["Segments"],
            json!([0,1,2,0.5,0.0,2,1,0,2,1.02,1.0]), "more than half a frame after 1");
        let first = key_part(&base, "A", 0.0, 0.0).unwrap();
        assert_eq!(first["Curves"][1]["Segments"], json!([0,0.0,2,1,0]));
        assert_eq!(first["Meta"], base["Meta"]);
        let mut floats = base.clone();
        floats["Meta"]["TotalPointCount"] = json!(4.0);
        assert_eq!(key_part(&floats, "A", 0.0, 0.0).unwrap()["Meta"], floats["Meta"], "unchanged counts keep their form");
        let later = key_part(&base, "A", 2.0, 1.0).unwrap();
        assert_eq!(later["Curves"][1]["Segments"], json!([0,1,2,1,0,2,2.0,1.0]));
        assert_eq!((&later["Meta"]["Duration"], &later["Meta"]["TotalPointCount"]), (&json!(2.0), &json!(5)));
        let added = key_part(&base, "B", 0.25, 1.0).unwrap();
        assert_eq!(added["Curves"][2], json!({"Target":"PartOpacity","Id":"B","Segments":[0.0,1.0,2,0.25,1.0]}));
        assert_eq!(added["Meta"], json!({"Duration":1, "Fps":30, "CurveCount":3, "TotalSegmentCount":3,
            "TotalPointCount":6}));
        let at_zero = key_part(&base, "C", 0.0, 0.0).unwrap();
        assert_eq!(at_zero["Curves"][2]["Segments"], json!([0.0,0.0]));
        assert_eq!(at_zero["Meta"], json!({"Duration":1, "Fps":30, "CurveCount":3, "TotalSegmentCount":2,
            "TotalPointCount":5}));
        let late = motion(json!([{"Target":"PartOpacity","Id":"D","Segments":[0.5,1]}]), 0, 1);
        let earlier = key_part(&late, "D", 0.0, 0.0).unwrap();
        assert_eq!(earlier["Curves"][0]["Segments"], json!([0.0,0.0,2,0.5,1]));
        assert_eq!(earlier["Meta"], json!({"Duration":1, "Fps":30, "CurveCount":1, "TotalSegmentCount":1,
            "TotalPointCount":2}));
    }

    #[test]
    fn part_keys_refuse_segments_that_are_not_stepped() {
        let base = motion(json!([{"Target":"PartOpacity","Id":"L","Segments":[0,0,0,1,1]},
            {"Target":"PartOpacity","Id":"B","Segments":[0,0,1,0.3,0,0.6,1,1,1,2,2,0]},
            {"Target":"PartOpacity","Id":"I","Segments":[0,0,3,1,1]}]), 4, 9);
        for (id, time) in [("L", 0.5), ("L", 0.0), ("L", 1.0), ("B", 0.5), ("B", 1.0), ("I", 0.5), ("I", 1.0)] {
            assert_eq!(key_part(&base, id, time, 1.0),
                Err(format!("PartOpacity curve {id:?} has a segment that is not stepped at time {time}")));
        }
        assert_eq!(key_part(&base, "B", 1.5, 1.0).unwrap()["Curves"][1]["Segments"],
            json!([0,0,1,0.3,0,0.6,1,1,1,2,1.5,1.0,2,2,0]));
        assert_eq!(key_part(&base, "B", 2.0, 1.0).unwrap()["Curves"][1]["Segments"],
            json!([0,0,1,0.3,0,0.6,1,1,1,2,2,1.0]));
    }
}
