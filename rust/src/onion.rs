//! Cubism UI08: `animation.onion` (the puppet at neighbouring frames drawn faintly behind the current one) and
//! `animation.trajectory` (the paths vertices take through an animation).
//!
//! Both preview other frames of an animation for the length of one call. The previous preview and physics
//! output are put back afterwards, so the session is unchanged.

use crate::{finite_number, model_import, normalized_export_target, required_id, required_quad, required_string, write_atomic, Session};
use serde_json::{json, Value};
use std::path::Path;

/// Frames drawn on each side of the current one.
const MAX_GHOSTS: u64 = 8;
const MAX_TRAJECTORY_FRAMES: f64 = 2000.0;
const MAX_TRAJECTORY_VERTICES: usize = 256;
/// The renderer refuses more than 16 megapixels, which is 4096 by 4096.
const MAX_SIZE: f64 = 4096.0;

impl Session {
    /// Runs `read` with `frame` of the animation previewed (without physics), then restores the previous state.
    fn at_frame<T>(&mut self, name: &str, frame: f64, read: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        let next = self.animation_frame(name, frame)?;
        let saved = (self.animation_preview.replace(next), self.physics_output.take());
        let result = read(self);
        (self.animation_preview, self.physics_output) = saved;
        result
    }

    /// The last frame of the clip: `length` counts frames, and a key cannot sit on `length` itself.
    fn animation_last_frame(&self, name: &str) -> Result<f64, String> {
        let length = self.document.model["animations"][name]["length"].as_f64().filter(|length| *length >= 1.0);
        length.map(|length| length - 1.0).ok_or_else(|| format!("animation {name} was not found"))
    }

    /// `animation.onion`: a PNG of the puppet at `frame` over the frames `step` apart before it (red) and after
    /// it (blue), each fainter the further it is from `frame`. A ghost keeps only the picture's brightness. Frames outside the animation are left out.
    pub(crate) fn op_animation_onion(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.onion")?;
        let name = required_string(request, "name")?;
        let last = self.animation_last_frame(name)?;
        let frame = finite_number(request.get("frame").ok_or("frame not provided")?, "frame")?.clamp(0.0, last);
        let ghosts = |key: &str| match request.get(key) {
            None | Some(Value::Null) => Ok(2),
            Some(value) => value.as_u64().filter(|count| *count <= MAX_GHOSTS).ok_or(format!("{key} must be 0 to {MAX_GHOSTS}")),
        };
        let (before, after) = (ghosts("before")?, ghosts("after")?);
        let number = |key: &str, default: f64, valid: fn(f64) -> bool, message: &str| match request.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(value) => value.as_f64().filter(|number| valid(*number)).ok_or(message.to_owned()),
        };
        let step = number("step", 1.0, |step| step.is_finite() && step > 0.0, "step must be above 0")?;
        let opacity = number("opacity", 0.3, |opacity| (0.0..=1.0).contains(&opacity), "opacity must be 0 to 1")?;
        let size = number("size", 1024.0, |size| (16.0..=MAX_SIZE).contains(&size.floor()), "size must be 16 to 4096")?.floor();

        let target = match request.get("path") {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) => Some(normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "animation.onion")?),
            Some(_) => return Err("path must be a string".into()),
        };

        // Farthest first, the current frame last. Each is (frame, opacity, ghost colour).
        let mut drawn: Vec<(f64, f64, Option<[f64; 3]>)> = Vec::new();
        for distance in (1..=before.max(after)).rev() {
            let fade = opacity / distance as f64;
            let past = frame - distance as f64 * step;
            let future = frame + distance as f64 * step;
            if distance <= before && past >= 0.0 {
                drawn.push((past, fade, Some([1.0, 0.25, 0.25])));
            }
            if distance <= after && future <= last {
                drawn.push((future, fade, Some([0.25, 0.25, 1.0])));
            }
        }
        drawn.push((frame, 1.0, None));

        // A view given as `bounds`, `width` and `height` (the editor's camera) is drawn as it is.
        let view = match (request.get("bounds"), request.get("width"), request.get("height")) {
            (None | Some(Value::Null), None | Some(Value::Null), None | Some(Value::Null)) => None,
            (Some(_), Some(width), Some(height)) => {
                let side = |value: &Value| value.as_u64().filter(|side| (1..=u64::from(MAX_SIZE as u32)).contains(side)).map(|side| side as u32);
                let (width, height) = side(width).zip(side(height)).ok_or("width and height must be 1 to 4096")?;
                let bounds = required_quad(request, "bounds")?;
                if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
                    return Err("bounds must have increasing min and max coordinates".into());
                }
                Some((bounds, width, height))
            }
            _ => return Err("bounds, width and height go together".into()),
        };
        let mut union = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (at, _, _) in drawn.iter().filter(|_| view.is_none()) {
            let bounds = self.at_frame(name, *at, |session| session.op_bounds(&json!({})))?["bounds"].clone();
            let found: Vec<f64> = bounds.as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
            let [a, b, c, d] = found[..] else {
                return Err("Puppet bounds are empty".into());
            };
            union = [union[0].min(a), union[1].min(b), union[2].max(c), union[3].max(d)];
        }
        let ([x0, y0, x1, y1], width, height) = match view {
            Some(view) => view,
            None => {
                let [x0, y0, x1, y1] = union;
                let (w, h) = (x1 - x0, y1 - y0);
                if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
                    return Err("Puppet bounds are empty".into());
                }
                let scale = size / w.max(h);
                (union, ((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
            }
        };

        // Premultiplied colour, source over.
        let mut canvas = vec![[0.0f32; 4]; width as usize * height as usize];
        for (at, fade, tint) in &drawn {
            let rendered = self.at_frame(name, *at, |session| {
                session.op_render(&json!({ "width": width, "height": height, "bounds": [x0, y0, x1, y1] }))
            })?;
            let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, rendered["png"].as_str().unwrap_or_default())
                .map_err(|error| format!("the onion skin could not be read: {error}"))?;
            let picture = model_import::decode_rgba("Onion skin", Some(image::ImageFormat::Png), &png)?;
            for (pixel, below) in picture.pixels().zip(&mut canvas) {
                let alpha = f32::from(pixel[3]) / 255.0 * *fade as f32;
                let luma = (0.299 * f32::from(pixel[0]) + 0.587 * f32::from(pixel[1]) + 0.114 * f32::from(pixel[2])) / 255.0;
                for channel in 0..3 {
                    // A ghost is its colour shaded by the picture's brightness, so any texture reads red or blue.
                    let colour = tint.map_or(f32::from(pixel[channel]) / 255.0, |tint| tint[channel] as f32 * (0.5 + 0.5 * luma));
                    below[channel] = colour * alpha + below[channel] * (1.0 - alpha);
                }
                below[3] = alpha + below[3] * (1.0 - alpha);
            }
        }
        let bytes: Vec<u8> = canvas
            .iter()
            .flat_map(|[r, g, b, a]| {
                let straight = |c: f32| if *a > 0.0 { ((c / a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
                [straight(*r), straight(*g), straight(*b), (a.clamp(0.0, 1.0) * 255.0).round() as u8]
            })
            .collect();
        let picture = image::RgbaImage::from_raw(width, height, bytes).ok_or("the onion skin has the wrong size")?;
        let mut png = Vec::new();
        picture
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|error| format!("the onion skin could not be encoded: {error}"))?;
        let mut result = json!({
            "width": width, "height": height, "frames": drawn.iter().map(|(at, _, _)| *at).collect::<Vec<_>>(),
            "png": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png),
        });
        if let Some(target) = &target {
            write_atomic(target, &png, true)?;
            result["path"] = request["path"].clone();
        }
        Ok(result)
    }

    /// `animation.trajectory`: where vertices of one Part are, in world space, at each frame from `from` to `to`
    /// (default the whole animation) `step` frames apart (default 1, or more for a clip over 2000 frames). Default
    /// vertices: every one, or every few-th of a mesh over 256.
    pub(crate) fn op_animation_trajectory(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.trajectory")?;
        let name = required_string(request, "name")?;
        let node = required_id(request, "node")?;
        let last = self.animation_last_frame(name)?;
        let bound = |key: &str, default: f64| match request.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(value) => finite_number(value, key).and_then(|number| {
                (0.0..=last).contains(&number).then_some(number).ok_or(format!("{key} must be 0 to {last}"))
            }),
        };
        let (from, to) = (bound("from", 0.0)?, bound("to", last)?);
        let step = match request.get("step") {
            // One frame, or the fewest whole frames that keep a long clip within the frame cap.
            None | Some(Value::Null) => ((to - from) / (MAX_TRAJECTORY_FRAMES - 1.0)).ceil().max(1.0),
            Some(value) => value.as_f64().filter(|step| step.is_finite() && *step > 0.0).ok_or("step must be above 0")?,
        };
        if to < from {
            return Err("to must not be before from".into());
        }
        let count = ((to - from) / step + 1e-9).floor() + 1.0;
        if count > MAX_TRAJECTORY_FRAMES {
            return Err(format!("{count} frames is more than {MAX_TRAJECTORY_FRAMES}; raise step"));
        }
        let frames: Vec<f64> = (0..count as u64).map(|index| from + index as f64 * step).collect();
        let sample = |session: &mut Session, at: f64| -> Result<Vec<[f64; 2]>, String> {
            let geometry = session.at_frame(name, at, |session| session.op_scene_geometry(&json!({})))?;
            let part = geometry["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|part| part["id"].as_u64() == Some(u64::from(node)))
                .ok_or_else(|| format!("node {node} draws no mesh"))?;
            Ok(part["vertices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|vertex| Some([vertex[0].as_f64()?, vertex[1].as_f64()?]))
                .collect())
        };
        let first = sample(self, frames[0])?;
        let total = first.len();
        let chosen: Vec<usize> = match request.get("vertices") {
            None | Some(Value::Null) => {
                // A bigger mesh gives every few-th vertex.
                (0..total).step_by(total.div_ceil(MAX_TRAJECTORY_VERTICES).max(1)).collect()
            }
            Some(list) => {
                let list = list.as_array().ok_or("vertices must be an array of indices")?;
                if list.len() > MAX_TRAJECTORY_VERTICES {
                    return Err(format!("name at most {MAX_TRAJECTORY_VERTICES} vertices"));
                }
                list.iter()
                    .map(|index| index.as_u64().map(|index| index as usize).filter(|index| *index < total))
                    .collect::<Option<_>>()
                    .ok_or(format!("vertices must be indices below {total}"))?
            }
        };
        let mut positions = vec![first];
        for at in &frames[1..] {
            let drawn = sample(self, *at)?;
            // A vertex that has no finite position is left out, which would shift every index after it.
            if drawn.len() != total {
                return Err(format!("node {node} has {} drawn vertices at frame {at}, not {total}", drawn.len()));
            }
            positions.push(drawn);
        }
        let paths: Vec<Value> = chosen
            .iter()
            .map(|vertex| json!({ "vertex": vertex, "points": positions.iter().map(|frame| frame[*vertex]).collect::<Vec<_>>() }))
            .collect();
        Ok(json!({ "node": node, "frames": frames, "paths": paths }))
    }
}
