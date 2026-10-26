use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub(crate) const MAX_ANIMATION_LENGTH: i64 = 1_000_000;
const MAX_TIMESTEP: f32 = 60.0;
/// Most characters of a marker's text.
const MAX_MARKER_TEXT: usize = 256;
/// Most image tracks a clip takes; an export decodes each one whole.
pub const MAX_IMAGE_TRACKS: usize = 8;
/// Most numbered images one sequence can link.
pub const MAX_SEQUENCE_FILES: usize = 4096;
/// Most group tracks a clip takes.
pub const MAX_TRACK_GROUPS: usize = 32;

pub(crate) fn apply(model: &Value, request: &Value, op: &str) -> Result<Value, String> {
    let mut candidate = model.clone();
    match op {
        "animation.create" => {
            let name = animation_name(request, "name")?.to_owned();
            let animations = animations_mut(&mut candidate, true)?;
            if animations.contains_key(&name) {
                return Err(format!("animation {name:?} already exists"));
            }
            animations.insert(
                name,
                json!({
                    "length": 60,
                    "timestep": 1.0f32 / 60.0f32,
                    "additive": false,
                    "animationWeight": 1.0,
                    "leadIn": -1,
                    "leadOut": -1,
                    "lanes": [],
                }),
            );
        }
        "animation.rename" => {
            let old_name = animation_name(request, "name")?.to_owned();
            let new_name = animation_name(request, "new_name")?.to_owned();
            let animations = animations_mut(&mut candidate, false)?;
            let animation = animations
                .get(&old_name)
                .cloned()
                .ok_or_else(|| format!("animation {old_name:?} was not found"))?;
            if old_name != new_name && animations.contains_key(&new_name) {
                return Err(format!("animation {new_name:?} already exists"));
            }
            if old_name != new_name {
                animations.remove(&old_name);
                animations.insert(new_name.clone(), animation);
                crate::playlist::rename_clip(&mut candidate, &old_name, &new_name);
            }
        }
        "animation.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            if let Some(user) = crate::playlist::clip_user(&candidate, &name) {
                return Err(format!("animation {name:?} is in {user}"));
            }
            let animations = animations_mut(&mut candidate, false)?;
            if animations.remove(&name).is_none() {
                return Err(format!("animation {name:?} was not found"));
            }
        }
        "animation.set" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let object = animation
                .as_object_mut()
                .ok_or("animation must be an object")?;
            let mut changed = false;
            for key in [
                "length",
                "timestep",
                "leadIn",
                "leadOut",
                "additive",
                "animationWeight",
                "loopEditing",
            ] {
                let Some(value) = request.get(key) else {
                    continue;
                };
                changed = true;
                if key == "loopEditing" {
                    let flag = value
                        .as_bool()
                        .ok_or_else(|| format!("{key} must be a boolean"))?;
                    if flag {
                        object.insert(key.to_owned(), json!(true));
                    } else {
                        object.remove(key);
                    }
                    continue;
                }
                let value = match key {
                    "length" => json!(parse_length(value)?),
                    "timestep" => json!(parse_timestep(value)?),
                    "leadIn" | "leadOut" => json!(integer(value, key)?),
                    "additive" => json!(value
                        .as_bool()
                        .ok_or_else(|| format!("{key} must be a boolean"))?),
                    "animationWeight" => json!(finite_f32(value, key)?),
                    _ => unreachable!(),
                };
                object.insert(key.to_owned(), value);
            }
            if !changed {
                return Err("animation.set requires a timing, length, weight, or loopEditing field".into());
            }
            let length = validate_animation_header(animation)?;
            validate_animation_keyframes(animation, length)?;
            crate::frame_step::validate(model, animation, length)?;
            // A marker is a note on a frame of this clip, so the clip cannot end before it, as with a key.
            for (index, marker) in animation.get("markers").and_then(Value::as_array).into_iter().flatten().enumerate() {
                if marker.get("frame").and_then(Value::as_i64).is_some_and(|frame| frame >= length as i64) {
                    return Err(format!("animation.markers[{index}].frame is outside animation length"));
                }
            }
        }
        "animation.marker.set" | "animation.marker.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            let frame = key_frame(request, length);
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            let markers = object.entry("markers").or_insert_with(|| json!([]));
            let markers = markers.as_array_mut().ok_or("animation.markers must be an array")?;
            let frame_of = |marker: &Value| marker.get("frame").and_then(Value::as_i64);
            // A list another tool wrote may be out of order. It is put in order here, and one that would leave two
            // markers on a frame, or a marker without a frame, is refused as a key list of that shape is.
            if let Some(index) = markers.iter().position(|marker| frame_of(marker).is_none()) {
                return Err(format!("animation.markers[{index}].frame must be an integer"));
            }
            markers.sort_by_key(frame_of);
            if markers.windows(2).any(|pair| frame_of(&pair[0]) == frame_of(&pair[1])) {
                return Err("animation.markers must have unique frames".into());
            }
            // A marker a file brought in may lie outside the clip. None is set there, but that one can be deleted.
            let asked = request.get("frame").and_then(Value::as_i64);
            let at = asked.and_then(|asked| markers.iter().position(|marker| frame_of(marker) == Some(asked)));
            let locked_at = |markers: &[Value], at: usize| markers[at].get("locked") == Some(&json!(true));
            if op == "animation.marker.set" {
                // A marker a file brought in outside the clip takes no new text there, but it can be unlocked and
                // moved into the clip.
                let outside = frame.is_err() && (request.get("text").is_none() || request.get("to").is_some());
                let frame = match at.zip(asked).filter(|_| outside) {
                    Some((_, asked)) => i32::try_from(asked).map_err(|_| "frame must be an integer")?,
                    None => frame?,
                };
                let text = match request.get("text") {
                    None => None,
                    Some(text) => Some(text.as_str().ok_or("text must be a string")?),
                };
                if text.is_some_and(|text| text.chars().count() > MAX_MARKER_TEXT) {
                    return Err(format!("text must be at most {MAX_MARKER_TEXT} characters"));
                }
                let lock = match request.get("locked") {
                    None => None,
                    Some(lock) => Some(lock.as_bool().ok_or("locked must be a boolean")?),
                };
                let to = match request.get("to") {
                    None => None,
                    Some(to) => Some(key_frame(&json!({"frame": to}), length).map_err(|message| message.replacen("frame", "to", 1))?),
                };
                if let Some(at) = at {
                    if text.is_none() && lock.is_none() && to.is_none() {
                        return Err("text must be a string".into());
                    }
                    // A locked marker only changes in a request that also unlocks it.
                    if locked_at(markers, at) && lock != Some(false) && (text.is_some() || to.is_some()) {
                        return Err(format!("the marker at frame {frame} is locked"));
                    }
                    // Only the named fields are replaced, so a field another tool put on the marker stays.
                    if let Some(text) = text {
                        markers[at]["text"] = json!(text);
                    }
                    match lock {
                        Some(true) => markers[at]["locked"] = json!(true),
                        Some(false) => { markers[at].as_object_mut().map(|marker| marker.remove("locked")); }
                        None => {}
                    }
                    if let Some(to) = to.filter(|to| *to != frame) {
                        if markers.iter().any(|marker| frame_of(marker) == Some(i64::from(to))) {
                            return Err(format!("animation {name:?} already has a marker at frame {to}"));
                        }
                        let mut marker = markers.remove(at);
                        marker["frame"] = json!(to);
                        let after = markers.iter().position(|marker| frame_of(marker) > Some(i64::from(to)));
                        markers.insert(after.unwrap_or(markers.len()), marker);
                    }
                } else {
                        // A new marker needs its text; there is none here to lock or to move.
                        let text = text.ok_or(if lock.is_some() || to.is_some() {
                            format!("animation {name:?} has no marker at frame {frame}")
                        } else {
                            "text must be a string".to_owned()
                        })?;
                        if to.is_some() {
                            return Err(format!("animation {name:?} has no marker at frame {frame}"));
                        }
                        let mut marker = json!({"frame": frame, "text": text});
                        if lock == Some(true) {
                            marker["locked"] = json!(true);
                        }
                        let after = markers.iter().position(|marker| frame_of(marker) > Some(i64::from(frame)));
                        markers.insert(after.unwrap_or(markers.len()), marker);
                    }
            } else {
                let Some(at) = at else {
                    return Err(format!("animation {name:?} has no marker at frame {}", frame?));
                };
                if locked_at(markers, at) {
                    return Err(format!("the marker at frame {} is locked", asked.unwrap_or_default()));
                }
                markers.remove(at);
                if markers.is_empty() {
                    object.remove("markers");
                }
            }
        }
        "animation.key.delete_all" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            let frame = key_frame(request, length)?;
            let listed = listed_parameters(model, request)?;
            let mut removed = 0;
            let held = freeze_frame(animation).is_some();
            for lane in animation.get_mut("lanes").and_then(Value::as_array_mut).into_iter().flatten() {
                let of_lane = lane.get("uuid").and_then(Value::as_u64);
                if listed.as_ref().is_some_and(|ids| !ids.iter().any(|id| of_lane == Some(u64::from(*id)))) {
                    continue;
                }
                if held && lane.get("frozen") == Some(&json!(true)) {
                    continue;
                }
                if let Some(keys) = lane.get_mut("keyframes").and_then(Value::as_array_mut) {
                    let frames = validate_keyframes(keys, length, "animation lane keyframes", true)?;
                    if let Some(index) = frames.iter().position(|existing| *existing == frame) {
                        keys.remove(index);
                        removed += 1;
                    }
                }
            }
            if removed == 0 {
                return Err(format!("animation {name:?} has no key at frame {frame}"));
            }
        }
        "animation.key.set" | "animation.key.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let parameter_id = required_parameter_id(request)?;
            let axis = required_axis(request, parameter_id, model)?;
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            let frame = key_frame(request, length)?;
            if op == "animation.key.set" {
                let value = finite_f32(
                    request.get("value").ok_or("value is required")?,
                    "value",
                )?;
                let tension = request
                    .get("tension")
                    .map(|value| finite_f32(value, "tension"))
                    .transpose()?;
                let interpolation = request
                    .get("interpolation")
                    .map(parse_interpolation)
                    .transpose()?;
                set_key(
                    animation,
                    parameter_id,
                    axis,
                    frame,
                    value,
                    tension,
                    interpolation,
                    length,
                )?;
            } else {
                delete_key(animation, parameter_id, axis, frame, length)?;
            }
        }
        "animation.lane.set" | "animation.lane.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let parameter_id = required_parameter_id(request)?;
            let axis = required_axis(request, parameter_id, model)?;
            let animation = animation_mut(&mut candidate, &name)?;
            let lanes = animation
                .get_mut("lanes")
                .and_then(Value::as_array_mut)
                .ok_or("animation lane was not found")?;
            let index = find_lane(lanes, parameter_id, axis)?.ok_or("animation lane was not found")?;
            if op == "animation.lane.delete" {
                lanes.remove(index);
            } else {
                let interpolation = request.get("interpolation").map(parse_interpolation).transpose()?;
                let merge_mode = request.get("merge_mode").map(parse_merge_mode).transpose()?;
                let shy = request.get("shy").map(|shy| shy.as_bool().ok_or("shy must be a boolean")).transpose()?;
                let frozen = request.get("frozen").map(|flag| flag.as_bool().ok_or("frozen must be a boolean")).transpose()?;
                if interpolation.is_none() && merge_mode.is_none() && shy.is_none() && frozen.is_none() {
                    return Err("animation.lane.set requires interpolation, merge_mode, shy or frozen".into());
                }
                let lane = lanes[index].as_object_mut().ok_or("animation lane must be an object")?;
                set_flag(lane, "shy", shy);
                set_flag(lane, "frozen", frozen);
                if let Some(interpolation) = interpolation {
                    lane.insert("interpolation".into(), json!(interpolation.as_str()));
                }
                if let Some(merge_mode) = merge_mode {
                    lane.insert("merge_mode".into(), json!(merge_mode.as_str()));
                }
            }
        }
        "animation.blink.parameters.set" => {
            let invalid = "parameters must be an array of parameter UUIDs";
            let mut eyes: Vec<u32> = Vec::new();
            for id in request.get("parameters").and_then(Value::as_array).ok_or(invalid)? {
                let id = id.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or(invalid)?;
                let parameter = crate::find_parameter(model, id).ok_or_else(|| format!("parameter {id} was not found"))?;
                if parameter.get("groupUUID").is_some() {
                    return Err(format!("parameter {id} is a parameter group"));
                }
                if parameter_is_vec2(parameter, "parameter")? {
                    return Err(format!("parameter {id} is 2D; eye blinking drives 1D parameters"));
                }
                if eyes.contains(&id) {
                    return Err(format!("parameter {id} is listed twice"));
                }
                eyes.push(id);
            }
            let object = candidate.as_object_mut().ok_or("model must be an object")?;
            if eyes.is_empty() {
                if let Some(live2d) = object.get_mut("live2d").and_then(Value::as_object_mut) {
                    live2d.remove("eyeBlink");
                }
            } else {
                object
                    .entry("live2d")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or("model.live2d must be an object")?
                    .insert("eyeBlink".into(), json!(eyes));
            }
        }
        "animation.blink.set" | "animation.blink.key.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            // The track is checked after the edit, not before, so a bad key a file brought in can be replaced or
            // deleted.
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            let blink = object.entry("eyeBlink").or_insert_with(|| json!({}));
            let blink = blink.as_object_mut().ok_or("animation.eyeBlink must be an object")?;
            let keys = blink.entry("keyframes").or_insert_with(|| json!([]));
            let keys = keys.as_array_mut().ok_or("animation.eyeBlink.keyframes must be an array")?;
            let at = |keys: &[Value], frame: i32| {
                let frame_of = |key: &Value| key["frame"].as_i64();
                keys.iter().position(|key| frame_of(key) == Some(i64::from(frame)))
                    .ok_or_else(|| {
                        keys.iter().position(|key| frame_of(key) > Some(i64::from(frame))).unwrap_or(keys.len())
                    })
            };
            if op == "animation.blink.set" {
                let key = match (request.get("frame"), request.get("value")) {
                    (None, None) => None,
                    (Some(_), Some(value)) => Some((key_frame(request, length)?, bounded(value, "value", 1.0)?)),
                    _ => return Err("frame and value go together".into()),
                };
                let effect = request.get("effect").map(|effect| bounded(effect, "effect", 100.0)).transpose()?;
                if key.is_none() && effect.is_none() {
                    return Err("animation.blink.set requires effect, or frame and value".into());
                }
                match key.map(|(frame, value)| (at(keys, frame), frame, value)) {
                    Some((Ok(index), _, value)) => keys[index]["value"] = json!(value),
                    Some((Err(index), frame, value)) => keys.insert(index, json!({"frame": frame, "value": value})),
                    None => {}
                }
                if let Some(effect) = effect {
                    blink.insert("effect".into(), json!(effect));
                }
            } else {
                let frame = key_frame(request, length)?;
                let index = at(keys, frame).map_err(|_| format!("animation {name:?} has no blink key at frame {frame}"))?;
                keys.remove(index);
            }
            // An empty track leaves no trace in the clip.
            if blink.get("keyframes").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                blink.remove("keyframes");
            }
            if blink.is_empty() {
                object.remove("eyeBlink");
            }
            blink_track(animation, length)?;
        }
        "animation.audio.add" | "animation.audio.set" | "animation.audio.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            // The tracks are checked after the edit, so a bad one a file brought in can be changed or deleted.
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            // `audio: null` reads as no tracks, as a lane's null does.
            if object.get("audio").is_some_and(Value::is_null) {
                object.remove("audio");
            }
            // A track in a locked group (ANI04) does not change, nor does a track join a locked group.
            if op != "animation.audio.add" {
                let index = request.get("index").and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok());
                let track = index.and_then(|index| object.get("audio")?.get(index));
                let group = track.and_then(|track| track.get("group")).and_then(Value::as_str);
                check_group_lock(object, &[group, request.get("group").and_then(Value::as_str)])?;
            }
            let tracks = object.entry("audio").or_insert_with(|| json!([]));
            let tracks = tracks.as_array_mut().ok_or("animation.audio must be an array")?;
            if op == "animation.audio.add" {
                // The session has resolved `path` to a WAV file.
                let path = request.get("path").and_then(Value::as_str).ok_or("path must be a string")?;
                let frame = match request.get("frame") {
                    None => 0,
                    Some(_) => key_frame(request, length)?,
                };
                tracks.push(json!({"path": path, "frame": frame}));
            } else {
                let index = request
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < tracks.len())
                    .ok_or_else(|| format!("index must name one of the {} audio tracks of {name:?}", tracks.len()))?;
                if op == "animation.audio.delete" {
                    tracks.remove(index);
                } else {
                    let frame = request.get("frame").map(|_| key_frame(request, length)).transpose()?;
                    let mute = request.get("mute").map(|mute| mute.as_bool().ok_or("mute must be a boolean"));
                    let mute = mute.transpose()?;
                    let group = request.get("group");
                    if frame.is_none() && mute.is_none() && group.is_none() {
                        return Err("animation.audio.set requires frame, mute or group".into());
                    }
                    let track = tracks[index].as_object_mut().ok_or("animation audio track must be an object")?;
                    if let Some(group) = group {
                        join_group(track, group)?;
                    }
                    if let Some(frame) = frame {
                        track.insert("frame".into(), json!(frame));
                    }
                    match mute {
                        Some(true) => {
                            track.insert("mute".into(), json!(true));
                        }
                        Some(false) => {
                            track.remove("mute");
                        }
                        None => {}
                    }
                }
            }
            if tracks.is_empty() {
                object.remove("audio");
            }
            validate_audio(animation, length)?;
            validate_layers(animation)?;
        }
        "animation.image.add" | "animation.image.set" | "animation.image.delete" | "animation.group.add"
        | "animation.group.set" | "animation.group.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            validate_animation_header(animation)?;
            // As with audio, the tracks are checked after the edit, so a bad one a file brought in can be repaired.
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            if op.starts_with("animation.image.") {
                edit_image(object, op, request, &name)?;
            } else {
                edit_group(object, op, request, &name)?;
            }
            for key in ["images", "groups"] {
                if object.get(key).and_then(Value::as_array).is_some_and(Vec::is_empty) {
                    object.remove(key);
                }
            }
            validate_layers(animation)?;
        }
        "animation.image.sequence" | "animation.image.time" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            match op {
                "animation.image.sequence" => edit_image_sequence(object, request, &name)?,
                "animation.image.time" => edit_image_time(object, request, &name, length)?,
                _ => unreachable!(),
            }
            validate_layers(animation)?;
        }
        "animation.shy.set" => {
            let name = animation_name(request, "name")?.to_owned();
            let listed = listed_parameters(model, request)?;
            let mark = request.get("mark").map(|mark| mark.as_str().ok_or(SHY_MARKS)).transpose()?;
            let active = request.get("active").map(|active| active.as_bool().ok_or("active must be a boolean")).transpose()?;
            let animation = animation_mut(&mut candidate, &name)?;
            validate_animation_header(animation)?;
            if let Some(mark) = mark {
                // As in Cubism, marking a subset turns shy on and clearing turns it off; marking all leaves it.
                let (shy, turns_on): (fn(&Value, Option<&[u32]>) -> bool, Option<bool>) = match mark {
                    "all" => (|_, _| true, None),
                    "unkeyed" => (|lane, _| lane["keyframes"].as_array().is_none_or(Vec::is_empty), Some(true)),
                    "listed" => (|lane, ids| lane_listed(lane, ids), Some(true)),
                    "unlisted" => (|lane, ids| !lane_listed(lane, ids), Some(true)),
                    "none" => (|_, _| false, Some(false)),
                    _ => return Err(SHY_MARKS.into()),
                };
                if matches!(mark, "listed" | "unlisted") != listed.is_some() {
                    return Err("parameters is required with mark listed or unlisted, and only with them".into());
                }
                for lane in animation.get_mut("lanes").and_then(Value::as_array_mut).into_iter().flatten() {
                    let flag = shy(lane, listed.as_deref());
                    set_flag(lane.as_object_mut().ok_or("animation lane must be an object")?, "shy", Some(flag));
                }
                let object = animation.as_object_mut().ok_or("animation must be an object")?;
                set_flag(object, "shyActive", active.or(turns_on));
            } else if listed.is_some() {
                return Err("parameters is required with mark listed or unlisted, and only with them".into());
            } else {
                let object = animation.as_object_mut().ok_or("animation must be an object")?;
                set_flag(object, "shyActive", Some(active.ok_or("animation.shy.set requires mark or active")?));
            }
        }
        "animation.freeze.set" => {
            let name = animation_name(request, "name")?.to_owned();
            let listed = listed_parameters(model, request)?;
            let mark = request.get("mark").map(|mark| mark.as_str().ok_or(FREEZE_MARKS)).transpose()?;
            let animation = animation_mut(&mut candidate, &name)?;
            let length = validate_animation_header(animation)?;
            let frame = match request.get("frame") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(_) => Some(Some(key_frame(request, length)?)),
            };
            if mark.is_none() && frame.is_none() {
                return Err("animation.freeze.set requires mark or frame".into());
            }
            if matches!(mark, Some("listed" | "unlisted")) != listed.is_some() {
                return Err("parameters is required with mark listed or unlisted, and only with them".into());
            }
            if let Some(mark) = mark {
                let frozen: fn(&Value, Option<&[u32]>) -> bool = match mark {
                    "all" => |_, _| true,
                    "listed" => |lane, ids| lane_listed(lane, ids),
                    "unlisted" => |lane, ids| !lane_listed(lane, ids),
                    "none" => |_, _| false,
                    _ => return Err(FREEZE_MARKS.into()),
                };
                for lane in animation.get_mut("lanes").and_then(Value::as_array_mut).into_iter().flatten() {
                    let flag = frozen(lane, listed.as_deref());
                    set_flag(lane.as_object_mut().ok_or("animation lane must be an object")?, "frozen", Some(flag));
                }
            }
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            // Cancelling every freeze also stops it, unless the request names a frame.
            match frame.or((mark == Some("none")).then_some(None)) {
                Some(Some(frame)) => {
                    object.insert("freezeFrame".into(), json!(frame));
                }
                Some(None) => {
                    object.remove("freezeFrame");
                }
                None => {}
            }
        }
        "animation.bookmark.set" => {
            let name = animation_name(request, "name")?.to_owned();
            let label = bookmark_label(model, request)?;
            let animation = animation_mut(&mut candidate, &name)?;
            validate_animation_header(animation)?;
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            let labels = object.entry("bookmarks").or_insert_with(|| json!([]));
            if labels.is_null() {
                *labels = json!([]);
            }
            let labels = labels.as_array_mut().ok_or("animation.bookmarks must be an array")?;
            match request.get("index") {
                None => labels.push(label),
                Some(index) => {
                    let index = bookmark_index(labels, index)?;
                    labels[index] = label;
                }
            }
        }
        "animation.bookmark.delete" => {
            let name = animation_name(request, "name")?.to_owned();
            let animation = animation_mut(&mut candidate, &name)?;
            validate_animation_header(animation)?;
            let object = animation.as_object_mut().ok_or("animation must be an object")?;
            let labels = object.get_mut("bookmarks").and_then(Value::as_array_mut);
            let labels = labels.ok_or_else(|| format!("animation {name:?} has no bookmarks"))?;
            match request.get("index") {
                None => labels.clear(),
                Some(index) => {
                    labels.remove(bookmark_index(labels, index)?);
                }
            }
            if labels.is_empty() {
                object.remove("bookmarks");
            }
        }
        "animation.bookmark.copy" => {
            let name = animation_name(request, "name")?.to_owned();
            let source = animation_mut(&mut candidate, &name)?;
            validate_animation_header(source)?;
            let labels = source["bookmarks"].as_array().filter(|labels| !labels.is_empty()).cloned();
            let labels = labels.ok_or_else(|| format!("animation {name:?} has no bookmarks"))?;
            let targets: Vec<String> = match request.get("to") {
                None => {
                    let others = animations_mut(&mut candidate, false)?.keys().filter(|target| **target != name);
                    others.cloned().collect()
                }
                Some(list) => {
                    let invalid = "to must be a non-empty array of other animation names";
                    let list = list.as_array().filter(|list| !list.is_empty()).ok_or(invalid)?;
                    let mut targets = Vec::new();
                    for target in list {
                        let target = target.as_str().filter(|target| *target != name).ok_or(invalid)?.to_owned();
                        if targets.contains(&target) {
                            return Err(format!("animation {target:?} is listed twice"));
                        }
                        targets.push(target);
                    }
                    targets
                }
            };
            if targets.is_empty() {
                return Err("there is no other animation to copy the bookmarks to".into());
            }
            for target in targets {
                let animation = animation_mut(&mut candidate, &target)?;
                validate_animation_header(animation)?;
                let object = animation.as_object_mut().ok_or("animation must be an object")?;
                let kept = object.entry("bookmarks").or_insert_with(|| json!([]));
                if kept.is_null() {
                    *kept = json!([]);
                }
                let kept = kept.as_array_mut().ok_or("animation.bookmarks must be an array")?;
                // A label the target already has is not added twice, so copying again changes nothing.
                for label in &labels {
                    if !kept.contains(label) {
                        kept.push(label.clone());
                    }
                }
            }
        }
        _ => return Err(format!("unsupported animation operation: {op}")),
    }
    Ok(candidate)
}

/// A parameter bookmark from a request: its label, the canvas point it is shown at and the parameters it lists.
fn bookmark_label(model: &Value, request: &Value) -> Result<Value, String> {
    let label = request.get("label").and_then(Value::as_str).filter(|label| !label.trim().is_empty());
    let label = label.filter(|label| label.chars().count() <= 256);
    let label = label.ok_or("label must be a non-empty string of at most 256 characters")?;
    let x = crate::finite_number(request.get("x").ok_or("x is required")?, "x")?;
    let y = crate::finite_number(request.get("y").ok_or("y is required")?, "y")?;
    let parameters = listed_parameters(model, request)?.ok_or("parameters is required")?;
    Ok(json!({"label": label, "x": x, "y": y, "parameters": parameters}))
}

fn bookmark_index(labels: &[Value], index: &Value) -> Result<usize, String> {
    index
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < labels.len())
        .ok_or_else(|| format!("index must be below {}, the number of bookmarks", labels.len()))
}

/// The parameters that bind the node `id` or a node above it, in the order of the parameter list: what a parameter
/// bookmark of that node lists.
pub(crate) fn bookmark_parameters(model: &Value, id: u32) -> Result<Value, String> {
    fn path(node: &Value, id: u64, out: &mut Vec<u64>) -> bool {
        let own = node["uuid"].as_u64();
        out.extend(own);
        if own == Some(id) || node["children"].as_array().into_iter().flatten().any(|child| path(child, id, out)) {
            return true;
        }
        if own.is_some() {
            out.pop();
        }
        false
    }
    fn walk(list: &Value, chain: &[u64], out: &mut Vec<Value>) {
        for item in list.as_array().into_iter().flatten() {
            if item.get("groupUUID").is_some() {
                walk(&item["children"], chain, out);
            } else if item["bindings"].as_array().into_iter().flatten().any(|binding| {
                binding["node"].as_u64().is_some_and(|node| chain.contains(&node))
            }) {
                out.push(json!({"uuid": item["uuid"], "name": item["name"]}));
            }
        }
    }
    let mut chain = Vec::new();
    if !path(&model["nodes"], u64::from(id), &mut chain) {
        return Err(format!("node {id} was not found"));
    }
    let mut parameters = Vec::new();
    walk(&model["param"], &chain, &mut parameters);
    let node = crate::nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    Ok(json!({"node": id, "name": node["name"], "parameters": parameters}))
}

/// Animation templates of the user (ANI15): keys of a frame range, kept by template name in
/// `animation-templates.json` of the settings directory as `{length, lanes: [{parameter, axis, keyframes}]}`. A lane
/// names its parameter, since a template goes from one model to another; the frames count from the range's start.
pub(crate) const TEMPLATES: crate::preset_store::Kind = crate::preset_store::Kind {
    file: "animation-templates.json",
    noun: "template",
    entry: template_entry,
    builtin: |_| false,
};

const TEMPLATE_LENGTH: usize = 100_000;

/// What a template keeps, checked. It checks every template read from the file as well.
fn template_entry(request: &Value) -> Result<Value, String> {
    let length = request.get("length").and_then(Value::as_u64).and_then(|length| usize::try_from(length).ok());
    let length = length.filter(|length| (1..=TEMPLATE_LENGTH).contains(length));
    let length = length.ok_or_else(|| format!("length must be an integer from 1 to {TEMPLATE_LENGTH}"))?;
    let lanes = request.get("lanes").and_then(Value::as_array).filter(|lanes| !lanes.is_empty());
    let lanes = lanes.ok_or("lanes must be a non-empty array")?;
    let mut kept = Vec::new();
    for (index, lane) in lanes.iter().enumerate() {
        let path = format!("lanes[{index}]");
        let parameter = lane["parameter"].as_str().filter(|name| !name.trim().is_empty());
        let parameter = parameter.ok_or_else(|| format!("{path}.parameter must be a parameter name"))?;
        let axis = lane["axis"].as_u64().filter(|axis| *axis < 2);
        let axis = axis.ok_or_else(|| format!("{path}.axis must be 0 or 1"))?;
        if kept.iter().any(|other: &Value| other["parameter"] == parameter && other["axis"] == axis) {
            return Err(format!("{path} repeats parameter {parameter:?} axis {axis}"));
        }
        let keys = lane["keyframes"].as_array().filter(|keys| !keys.is_empty());
        let keys = keys.ok_or_else(|| format!("{path}.keyframes must be a non-empty array"))?;
        validate_keyframes(keys, length, &format!("{path}.keyframes"), true)?;
        let keys: Vec<Value> = keys
            .iter()
            .map(|key| {
                let mut kept = json!({"frame": key["frame"], "value": key["value"]});
                if let Some(tension) = key.get("tension") {
                    kept["tension"] = tension.clone();
                }
                kept
            })
            .collect();
        kept.push(json!({"parameter": parameter, "axis": axis, "keyframes": keys}));
    }
    Ok(json!({"length": length, "lanes": kept}))
}

/// An integer frame field of a request, inside an animation of `length` frames.
pub(crate) fn frame_field(request: &Value, key: &str, length: usize) -> Result<i32, String> {
    let frame = request.get(key).and_then(Value::as_i64).and_then(|frame| i32::try_from(frame).ok());
    frame
        .filter(|frame| *frame >= 0 && (*frame as usize) < length)
        .ok_or_else(|| format!("{key} must be an integer frame from 0 to {}", length - 1))
}

/// The one plain parameter named `name`, if any. Two with the name are refused, since a template could not tell them
/// apart.
fn named<'a>(parameters: &[&'a Value], name: &str) -> Result<Option<&'a Value>, String> {
    let mut named = parameters.iter().filter(|parameter| parameter["name"] == name);
    let first = named.next().copied();
    let more = named.count();
    if more > 0 {
        return Err(format!("{} parameters are named {name:?}; rename all but one first", more + 1));
    }
    Ok(first)
}

/// The plain parameters of the model, groups opened, in list order.
fn plain_parameters(model: &Value) -> Vec<&Value> {
    fn walk<'a>(list: &'a Value, out: &mut Vec<&'a Value>) {
        for item in list.as_array().into_iter().flatten() {
            if item.get("groupUUID").is_some() {
                walk(&item["children"], out);
            } else {
                out.push(item);
            }
        }
    }
    let mut out = Vec::new();
    walk(&model["param"], &mut out);
    out
}

/// The request that keeps the keys of the animation `name` from frame `from` to `to` (inclusive) as the template
/// `template`: every lane with a key in the range, or only the lanes of the listed parameters.
pub(crate) fn template_request(model: &Value, request: &Value) -> Result<Value, String> {
    let name = animation_name(request, "name")?;
    let template = request.get("template").cloned().unwrap_or(Value::Null);
    let listed = listed_parameters(model, request)?;
    let parameters = plain_parameters(model);
    let animation = model["animations"].get(name).ok_or_else(|| format!("animation {name:?} was not found"))?;
    let length = validate_animation_header(animation)?;
    let (from, to) = (frame_field(request, "from", length)?, frame_field(request, "to", length)?);
    if from > to {
        return Err("from must not be after to".into());
    }
    if (to - from) as usize >= TEMPLATE_LENGTH {
        return Err(format!("a template holds at most {TEMPLATE_LENGTH} frames; from {from} to {to} is more"));
    }
    let range = i64::from(from)..=i64::from(to);
    let mut lanes = Vec::new();
    for (index, lane) in animation["lanes"].as_array().into_iter().flatten().enumerate() {
        let path = format!("animation.lanes[{index}]");
        let id = lane_parameter_id(lane, &path)?;
        if listed.as_ref().is_some_and(|ids| !ids.contains(&id)) {
            continue;
        }
        let keys: Vec<Value> = lane["keyframes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|key| key["frame"].as_i64().is_some_and(|frame| range.contains(&frame)))
            .map(|key| {
                let mut key = key.clone();
                key["frame"] = json!(key["frame"].as_i64().unwrap_or_default() - i64::from(from));
                key
            })
            .collect();
        if keys.is_empty() {
            continue;
        }
        let parameter = crate::find_parameter(model, id).and_then(|parameter| parameter["name"].as_str());
        let parameter = parameter.ok_or_else(|| format!("{path} names parameter {id}, which has no name"))?;
        named(&parameters, parameter)?;
        lanes.push(json!({"parameter": parameter, "axis": lane["target"], "keyframes": keys}));
    }
    if lanes.is_empty() {
        return Err(format!("animation {name:?} has no key from frame {from} to {to}"));
    }
    // The store reads a template's name from `name`.
    let mut kept = json!({"name": template, "length": to - from + 1, "lanes": lanes});
    if let Some(overwrite) = request.get("overwrite") {
        kept["overwrite"] = overwrite.clone();
    }
    Ok(kept)
}

/// Inserts the keys of `template` into the animation `name` from the integer `frame` on, each lane into the plain
/// parameter of the model with the lane's parameter name. Answers the candidate and what was applied, left out and
/// dropped. A key past the end of the animation is dropped (a guess: the manual does not say).
pub(crate) fn apply_template(model: &Value, request: &Value, template: &Value) -> Result<(Value, Value), String> {
    let name = animation_name(request, "name")?.to_owned();
    let template_name = request["template"].as_str().unwrap_or_default();
    let mut candidate = model.clone();
    let animation = animation_mut(&mut candidate, &name)?;
    let length = validate_animation_header(animation)?;
    let frame = frame_field(request, "frame", length)?;
    let parameters = plain_parameters(model);
    let (mut applied, mut lanes, mut unmatched, mut dropped) = (0, Vec::new(), Vec::new(), 0);
    for lane in template["lanes"].as_array().into_iter().flatten() {
        let parameter = lane["parameter"].as_str().unwrap_or_default();
        let axis = lane["axis"].as_u64().unwrap_or_default() as usize;
        let axes = |target: &&Value| if target["is_vec2"] == true { 2 } else { 1 };
        let Some(target) = named(&parameters, parameter)?.filter(|target| axis < axes(target)) else {
            unmatched.push(json!({"parameter": parameter, "axis": axis}));
            continue;
        };
        let id = target["uuid"].as_u64().and_then(crate::valid_id).ok_or("a parameter has no valid UUID")?;
        let mut keyed = false;
        // ponytail: one set_key per key checks the whole lane each time, so a lane of n keys costs n^2. Merge the
        // keys of a lane in one pass if templates of many thousand keys are seen.
        for key in lane["keyframes"].as_array().into_iter().flatten() {
            let at = i64::from(frame) + key["frame"].as_i64().unwrap_or_default();
            if at >= length as i64 {
                dropped += 1;
                continue;
            }
            let tension = key.get("tension").map(|tension| finite_f32(tension, "tension")).transpose()?;
            set_key(animation, id, axis, at as i32, finite_f32(&key["value"], "value")?, tension, None, length)?;
            (applied, keyed) = (applied + 1, true);
        }
        if keyed {
            lanes.push(json!({"parameter": id, "axis": axis}));
        }
    }
    if applied == 0 {
        return Err(format!("no key of template {template_name:?} fits animation {name:?} from frame {frame}"));
    }
    Ok((candidate, json!({"applied": applied, "lanes": lanes, "unmatched": unmatched, "dropped": dropped})))
}

const FREEZE_MARKS: &str = "mark must be all, listed, unlisted or none";

/// The first change `candidate` makes to a lane that is frozen in `model`, as a refusal. While an animation's freeze
/// is on, its frozen lanes keep their keys, interpolation and merge mode, and stay in the animation.
pub(crate) fn frozen_change(model: &Value, candidate: &Value) -> Option<String> {
    let animations = model.get("animations")?.as_object()?;
    for (name, animation) in animations {
        let Some(held) = freeze_frame(animation) else { continue };
        // A deleted or renamed animation takes its lanes with it.
        let Some(after) = candidate["animations"].get(name) else {
            continue;
        };
        let identity = |lane: &Value| (lane.get("uuid").cloned(), lane.get("target").cloned());
        for lane in animation["lanes"].as_array().into_iter().flatten() {
            // Deleting the parameter is an edit of the model, which takes its lanes with it.
            let id = lane["uuid"].as_u64().and_then(crate::valid_id);
            let gone = id.is_none_or(|id| crate::find_parameter(candidate, id).is_none());
            if lane.get("frozen") != Some(&json!(true)) || gone {
                continue;
            }
            let kept = after["lanes"].as_array().into_iter().flatten().find(|other| identity(other) == identity(lane));
            let same = kept.is_some_and(|other| {
                ["keyframes", "interpolation", "merge_mode"].iter().all(|key| other.get(key) == lane.get(key))
            });
            let same_frame = id.is_none_or(|id| {
                let sampled = |model: &Value, clip: &Value| {
                    validate_animation_header(clip).and_then(|length| {
                        crate::frame_step::parameter_frame(model, clip, id, length, held as f64)
                    })
                };
                sampled(model, animation) == sampled(candidate, after)
            });
            if !same || !same_frame {
                return Some(format!(
                    "the lane of parameter {} axis {} in animation {name:?} is frozen; turn freeze off first",
                    lane["uuid"], lane["target"]
                ));
            }
        }
    }
    None
}

/// The frame frozen lanes hold, while the freeze of `animation` is on.
pub(crate) fn freeze_frame(animation: &Value) -> Option<u64> {
    animation.get("freezeFrame").and_then(Value::as_u64)
}

/// Whether the lane of `id` and `axis` holds its frame now; key all and unkey all pass it by.
fn held_lane(animation: &Value, id: u64, axis: usize) -> bool {
    freeze_frame(animation).is_some()
        && animation["lanes"].as_array().into_iter().flatten().any(|lane| {
            lane["uuid"].as_u64() == Some(id) && lane["target"].as_u64() == Some(axis as u64) && lane.get("frozen") == Some(&json!(true))
        })
}

const SHY_MARKS: &str = "mark must be all, unkeyed, listed, unlisted or none";

/// Stores a flag only while it is true, so a file without shy reads and writes as before.
fn set_flag(object: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    match value {
        Some(true) => {
            object.insert(key.into(), json!(true));
        }
        Some(false) => {
            object.remove(key);
        }
        None => {}
    }
}

fn lane_listed(lane: &Value, ids: Option<&[u32]>) -> bool {
    let of_lane = lane.get("uuid").and_then(Value::as_u64);
    ids.into_iter().flatten().any(|id| of_lane == Some(u64::from(*id)))
}

/// `group` in a request: a group name joins it, null leaves the group.
fn join_group(track: &mut Map<String, Value>, group: &Value) -> Result<(), String> {
    match group {
        Value::String(group) => {
            track.insert("group".into(), json!(group));
        }
        Value::Null => {
            track.remove("group");
        }
        _ => return Err("group must be a string or null".into()),
    }
    Ok(())
}

/// The `x`, `y`, `scale`, `opacity` and flag fields a request names, checked for type only; the ranges are checked
/// with the clip after the edit.
fn track_fields(request: &Value, flags: &[&str]) -> Result<Map<String, Value>, String> {
    let mut fields = Map::new();
    for key in ["x", "y", "scale", "opacity"] {
        if let Some(value) = request.get(key) {
            let value = value.as_f64().filter(|value| value.is_finite());
            fields.insert(key.into(), json!(value.ok_or_else(|| format!("{key} must be a finite number"))?));
        }
    }
    for key in flags {
        if let Some(value) = request.get(*key) {
            fields.insert((*key).into(), json!(value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?));
        }
    }
    Ok(fields)
}

/// Writes `fields` into `track`; a flag is kept only while it is true.
fn set_fields(track: &mut Map<String, Value>, fields: Map<String, Value>) {
    for (key, value) in fields {
        if value == json!(false) {
            track.remove(&key);
        } else {
            track.insert(key, value);
        }
    }
}

fn is_locked(track: &Value) -> bool {
    track.get("locked") == Some(&json!(true))
}

fn group_named<'a>(object: &'a Map<String, Value>, group: &str) -> Option<&'a Value> {
    object.get("groups")?.as_array()?.iter().find(|item| item.get("name").and_then(Value::as_str) == Some(group))
}

/// Refuses a change to a track in a locked group, or one that moves it into a locked group.
fn check_group_lock(object: &Map<String, Value>, groups: &[Option<&str>]) -> Result<(), String> {
    for group in groups.iter().flatten() {
        if group_named(object, group).is_some_and(is_locked) {
            return Err(format!("the group {group:?} is locked"));
        }
    }
    Ok(())
}

/// `animation.image.*` (ANI04): image tracks such as a background, `images: [{path, x, y, scale, opacity, front?,
/// hidden?, locked?, group?}]`, top track first. A locked track only changes in a request that unlocks it, as a
/// locked marker does; a track in a locked group does not change until the group is unlocked.
fn edit_image(object: &mut Map<String, Value>, op: &str, request: &Value, name: &str) -> Result<(), String> {
    if object.get("images").is_some_and(Value::is_null) {
        object.remove("images");
    }
    let count = object.get("images").and_then(Value::as_array).map_or(0, Vec::len);
    if op == "animation.image.add" {
        if count >= MAX_IMAGE_TRACKS {
            return Err(format!("animation {name:?} already has the most image tracks, {MAX_IMAGE_TRACKS}"));
        }
        // The session has resolved `path` to an image file it can decode.
        let path = request.get("path").and_then(Value::as_str).ok_or("path must be a string")?;
        let mut track = json!({"path": path, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0});
        let track_map = track.as_object_mut().expect("a track is an object");
        set_fields(track_map, track_fields(request, &["front", "guide"])?);
        if let Some(group) = request.get("group") {
            join_group(track_map, group)?;
        }
        check_group_lock(object, &[track_map.get("group").and_then(Value::as_str)])?;
        object.entry("images").or_insert_with(|| json!([])).as_array_mut().ok_or("animation.images must be an array")?.push(track);
        return Ok(());
    }
    let index = request
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < count)
        .ok_or_else(|| format!("index must name one of the {count} image tracks of {name:?}"))?;
    let track = object["images"][index].clone();
    let group = track.get("group").and_then(Value::as_str);
    if op == "animation.image.delete" {
        if is_locked(&track) {
            return Err(format!("image track {index} of {name:?} is locked"));
        }
        check_group_lock(object, &[group])?;
        object["images"].as_array_mut().expect("images is an array").remove(index);
        return Ok(());
    }
    let fields = track_fields(request, &["front", "hidden", "locked", "guide"])?;
    let to = match request.get("to") {
        None => None,
        Some(to) => Some(to.as_u64().and_then(|to| usize::try_from(to).ok()).filter(|to| *to < count).ok_or_else(
            || format!("to must name one of the {count} image tracks of {name:?}"),
        )?),
    };
    let joins = request.get("group");
    let changes = fields.keys().any(|key| key != "locked") || to.is_some() || joins.is_some();
    if fields.is_empty() && !changes {
        return Err("animation.image.set requires a field to change".into());
    }
    let unlocks = fields.get("locked") == Some(&json!(false));
    if is_locked(&track) && changes && !unlocks {
        return Err(format!("image track {index} of {name:?} is locked"));
    }
    check_group_lock(object, &[group, joins.and_then(Value::as_str)])?;
    let images = object["images"].as_array_mut().expect("images is an array");
    let track = images[index].as_object_mut().ok_or("animation image track must be an object")?;
    set_fields(track, fields);
    if let Some(group) = joins {
        join_group(track, group)?;
    }
    if let Some(to) = to {
        let moved = images.remove(index);
        images.insert(to, moved);
    }
    Ok(())
}

/// Add a numbered image sequence as an image track, or refresh the links of an existing sequence track.
fn edit_image_sequence(object: &mut Map<String, Value>, request: &Value, name: &str) -> Result<(), String> {
    let sequence = request.get("sequence").ok_or("sequence metadata is missing")?;
    validate_sequence(sequence, "animation.image.sequence")?;
    let first_path = sequence["files"][0]["path"].as_str().ok_or("sequence frame path is missing")?;
    if object.get("images").is_some_and(Value::is_null) {
        object.remove("images");
    }
    let count = object.get("images").and_then(Value::as_array).map_or(0, Vec::len);
    match request.get("index") {
        None => {
            if count >= MAX_IMAGE_TRACKS {
                return Err(format!("animation {name:?} already has the most image tracks, {MAX_IMAGE_TRACKS}"));
            }
            let mut track = json!({"path": first_path, "sequence": sequence, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0});
            let track = track.as_object_mut().expect("an image track is an object");
            set_fields(track, track_fields(request, &["front", "guide"])?);
            if let Some(group) = request.get("group") {
                join_group(track, group)?;
            }
            check_group_lock(object, &[track.get("group").and_then(Value::as_str)])?;
            object.entry("images").or_insert_with(|| json!([])).as_array_mut()
                .ok_or("animation.images must be an array")?.push(Value::Object(track.clone()));
        }
        Some(index) => {
            let index = index.as_u64().and_then(|index| usize::try_from(index).ok()).filter(|index| *index < count)
                .ok_or_else(|| format!("index must name one of the {count} image tracks of {name:?}"))?;
            let current = object["images"][index].clone();
            if !current.get("sequence").is_some_and(Value::is_object) {
                return Err(format!("image track {index} of {name:?} has no sequence to reload"));
            }
            if is_locked(&current) {
                return Err(format!("image track {index} of {name:?} is locked"));
            }
            check_group_lock(object, &[current.get("group").and_then(Value::as_str)])?;
            let images = object["images"].as_array_mut().expect("images is an array");
            images[index]["sequence"] = sequence.clone();
        }
    }
    Ok(())
}

fn validate_sequence(sequence: &Value, path: &str) -> Result<(), String> {
    if !sequence.is_object() {
        return Err(format!("{path} must be an object"));
    }
    if !sequence.get("directory").and_then(Value::as_str).is_some_and(|path| std::path::Path::new(path).is_absolute()) {
        return Err(format!("{path}.directory must be an absolute path"));
    }
    if sequence.get("prefix").and_then(Value::as_str).is_none() {
        return Err(format!("{path}.prefix must be a string"));
    }
    sequence.get("firstNumber").and_then(Value::as_u64).filter(|number| *number <= (1u64 << 53))
        .ok_or_else(|| format!("{path}.firstNumber must be an exact unsigned frame number"))?;
    let files = sequence.get("files").and_then(Value::as_array).filter(|files| !files.is_empty())
        .ok_or_else(|| format!("{path}.files must be a nonempty array"))?;
    if files.len() > MAX_SEQUENCE_FILES {
        return Err(format!("{path}.files has more than {MAX_SEQUENCE_FILES} entries"));
    }
    let mut previous = None;
    for (index, file) in files.iter().enumerate() {
        let number = file.get("number").and_then(Value::as_u64).filter(|number| *number <= (1u64 << 53))
            .ok_or_else(|| format!("{path}.files[{index}].number must be an exact unsigned frame number"))?;
        if previous.is_some_and(|previous| number <= previous) {
            return Err(format!("{path}.files must be sorted by unique numeric suffix"));
        }
        if !file.get("path").and_then(Value::as_str).is_some_and(|path| std::path::Path::new(path).is_absolute()) {
            return Err(format!("{path}.files[{index}].path must be an absolute path"));
        }
        previous = Some(number);
    }
    Ok(())
}

/// Select the image used by a track at a clip frame and return its numeric source suffix.
/// A missing or out-of-range sequence frame is a blank selection (`None`).
pub(crate) fn image_path_at(image: &Value, animation: &Value, frame: f64) -> Result<(Option<String>, f64), String> {
    if !frame.is_finite() {
        return Err("frame must be finite".into());
    }
    let Some(sequence) = image.get("sequence") else {
        return Ok((Some(image.get("path").and_then(Value::as_str).ok_or("image path is missing")?.to_owned()), 0.0));
    };
    validate_sequence(sequence, "animation.image.sequence")?;
    let first = sequence["firstNumber"].as_u64().ok_or("sequence firstNumber is missing")?;
    if frame < 0.0 {
        return Ok((None, first as f64));
    }
    let length = validate_animation_header(animation)?;
    let stepped = crate::frame_step::frame(animation, length, frame)?;
    let remap = image.get("timeRemap").filter(|remap| !remap.is_null());
    let source_offset = remap.map_or(Ok(stepped), |track| image_time(track, length, stepped))?;
    if !source_offset.is_finite() {
        return Err("animation.image.timeRemap value must be finite".into());
    }
    if source_offset < 0.0 {
        return Ok((None, first as f64));
    }
    let offset = source_offset.floor();
    if offset > (1u64 << 53) as f64 {
        return Ok((None, (first as f64) + offset));
    }
    let Some(number) = first.checked_add(offset as u64).filter(|number| *number <= (1u64 << 53)) else {
        return Ok((None, (first as f64) + offset));
    };
    let file = sequence["files"].as_array().and_then(|files| files.iter().find(|file| file["number"].as_u64() == Some(number)));
    let path = file.and_then(|file| file["path"].as_str()).map(str::to_owned);
    Ok((path, number as f64))
}

// Image indices need f64 interpolation: rounding an f32 error can change which whole image is selected.
fn image_time(track: &Value, length: usize, frame: f64) -> Result<f64, String> {
    let keys = track["keyframes"].as_array().ok_or("image time remap needs keyframes")?;
    let frames = validate_keyframes(keys, length, "image time remap", true)?;
    let mode = track.get("interpolation").filter(|mode| !mode.is_null())
        .map(parse_interpolation).transpose()?.unwrap_or(Interpolation::Linear);
    if keys.is_empty() { return Ok(frame); }
    let value = |index: usize| keys[index]["value"].as_f64().unwrap();
    let Some(index) = frames.iter().position(|at| f64::from(*at) >= frame) else {
        return Ok(value(keys.len() - 1));
    };
    if index == 0 || f64::from(frames[index]) == frame { return Ok(value(index)); }
    let (previous, current) = (value(index - 1), value(index));
    let elapsed = frame - f64::from(frames[index - 1]);
    let interval = f64::from(frames[index] - frames[index - 1]);
    let t = elapsed / interval;
    Ok(match mode {
        Interpolation::Nearest => if t > 0.5 { current } else { previous },
        Interpolation::Stepped => previous,
        Interpolation::Linear => previous + (current - previous) * elapsed / interval,
        Interpolation::Cubic => {
            let p0 = value(index.saturating_sub(2));
            let p3 = value((index + 1).min(keys.len() - 1));
            let a = -0.5 * p0 + 1.5 * previous - 1.5 * current + 0.5 * p3;
            let b = p0 - 2.5 * previous + 2.0 * current - 0.5 * p3;
            ((a * t + b) * t + (-0.5 * p0 + 0.5 * current)) * t + previous
        }
        Interpolation::Bezier => {
            let tension = keys[index].get("tension").and_then(Value::as_f64).unwrap_or(0.5);
            let t2 = t * t;
            let t3 = t2 * t;
            let weight = ((t3 - 2.0 * t2 + t) * 2.0 * tension
                + (-2.0 * t3 + 3.0 * t2) + (t3 - t2) * 2.0 * tension).clamp(0.0, 1.0);
            previous + (current - previous) * weight
        }
    })
}

/// Set or clear the image-order curve of a sequence track. New keys at exact frame boundaries take effect there.
fn edit_image_time(object: &mut Map<String, Value>, request: &Value, name: &str, length: usize) -> Result<(), String> {
    let count = object.get("images").and_then(Value::as_array).map_or(0, Vec::len);
    let index = request.get("index").and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok()).filter(|index| *index < count)
        .ok_or_else(|| format!("index must name one of the {count} image tracks of {name:?}"))?;
    let track = object["images"][index].clone();
    if !track.get("sequence").is_some_and(Value::is_object) {
        return Err(format!("image track {index} of {name:?} has no sequence"));
    }
    if is_locked(&track) {
        return Err(format!("image track {index} of {name:?} is locked"));
    }
    check_group_lock(object, &[track.get("group").and_then(Value::as_str)])?;
    let values = request.get("keyframes").ok_or("keyframes not provided; give null to clear the time remap")?;
    let keys = match values {
        Value::Null => Vec::new(),
        Value::Array(keys) => {
            let mut keys = keys.clone();
            keys.sort_by_key(|key| key.get("frame").and_then(Value::as_i64));
            keys
        }
        _ => return Err("keyframes must be an array or null".into()),
    };
    if keys.is_empty() {
        object["images"].as_array_mut().expect("images is an array")[index].as_object_mut()
            .expect("image track is an object").remove("timeRemap");
        return Ok(());
    }
    let mut remap = Map::new();
    if let Some(interpolation) = request.get("interpolation").filter(|value| !value.is_null()) {
        remap.insert("interpolation".into(), interpolation.clone());
    }
    remap.insert("keyframes".into(), Value::Array(keys));
    let remap = Value::Object(remap);
    sample_track(&remap, length, 0.0, "animation.image.time")?;
    object["images"].as_array_mut().expect("images is an array")[index]["timeRemap"] = remap;
    Ok(())
}

/// `animation.group.*` (ANI04): group tracks, `groups: [{name, x, y, scale, opacity, hidden?, locked?}]`. A member
/// (an image or audio track with `group`, or the model through `modelGroup`) is placed by the group: its position is
/// scaled by the group's scale and moved by the group's position, and the opacities multiply. A hidden group's
/// members are not exported.
fn edit_group(object: &mut Map<String, Value>, op: &str, request: &Value, name: &str) -> Result<(), String> {
    if object.get("groups").is_some_and(Value::is_null) {
        object.remove("groups");
    }
    let group = request.get("group").and_then(Value::as_str).filter(|group| !group.is_empty());
    let group = group.ok_or("group must be a nonempty string")?.to_owned();
    let existing = group_named(object, &group).cloned();
    if op == "animation.group.add" {
        if existing.is_some() {
            return Err(format!("animation {name:?} already has a group {group:?}"));
        }
        let count = object.get("groups").and_then(Value::as_array).map_or(0, Vec::len);
        if count >= MAX_TRACK_GROUPS {
            return Err(format!("animation {name:?} already has the most groups, {MAX_TRACK_GROUPS}"));
        }
        let mut track = json!({"name": group, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0});
        set_fields(track.as_object_mut().expect("a group is an object"), track_fields(request, &["hidden", "locked"])?);
        object.entry("groups").or_insert_with(|| json!([])).as_array_mut().ok_or("animation.groups must be an array")?.push(track);
        return Ok(());
    }
    let existing = existing.ok_or_else(|| format!("animation {name:?} has no group {group:?}"))?;
    let at = object["groups"].as_array().and_then(|groups| groups.iter().position(|item| item == &existing));
    let at = at.expect("the group was found");
    if op == "animation.group.delete" {
        if is_locked(&existing) {
            return Err(format!("the group {group:?} is locked"));
        }
        let images = object.get("images").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let member = |image: &Value| image.get("group").and_then(Value::as_str) == Some(group.as_str());
        if let Some(index) = images.iter().position(|image| member(image) && is_locked(image)) {
            return Err(format!("image track {index} of {name:?} in the group {group:?} is locked"));
        }
        // Its members stay, out of the group.
        object["groups"].as_array_mut().expect("groups is an array").remove(at);
        for key in ["images", "audio"] {
            for track in object.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten() {
                if track.get("group").and_then(Value::as_str) == Some(group.as_str()) {
                    track.as_object_mut().map(|track| track.remove("group"));
                }
            }
        }
        if object.get("modelGroup").and_then(Value::as_str) == Some(group.as_str()) {
            object.remove("modelGroup");
        }
        return Ok(());
    }
    let fields = track_fields(request, &["hidden", "locked"])?;
    let model = request.get("model").map(|model| model.as_bool().ok_or("model must be a boolean")).transpose()?;
    let changes = fields.keys().any(|key| key != "locked") || model.is_some();
    if fields.is_empty() && !changes {
        return Err("animation.group.set requires a field to change".into());
    }
    if is_locked(&existing) && changes && fields.get("locked") != Some(&json!(false)) {
        return Err(format!("the group {group:?} is locked"));
    }
    // The model does not leave a locked group either.
    if model == Some(true) {
        check_group_lock(object, &[object.get("modelGroup").and_then(Value::as_str).filter(|current| *current != group)])?;
    }
    let track = object["groups"][at].as_object_mut().ok_or("animation group track must be an object")?;
    set_fields(track, fields);
    match model {
        Some(true) => {
            object.insert("modelGroup".into(), json!(group));
        }
        Some(false) if object.get("modelGroup").and_then(Value::as_str) == Some(group.as_str()) => {
            object.remove("modelGroup");
        }
        _ => {}
    }
    Ok(())
}

/// The image and group tracks of a clip (ANI04) and the groups that image tracks, audio tracks and the model join.
fn validate_layers(animation: &Value) -> Result<(), String> {
    let length = validate_animation_header(animation)?;
    let list = |key: &str| match animation.get(key) {
        None | Some(Value::Null) => Ok(&[][..]),
        Some(list) => list.as_array().map(Vec::as_slice).ok_or_else(|| format!("animation.{key} must be an array")),
    };
    let placed = |track: &Value, at: &str, flags: &[&str]| -> Result<(), String> {
        let number = |key: &str| track.get(key).and_then(Value::as_f64).filter(|value| value.is_finite());
        for key in ["x", "y"] {
            number(key).ok_or_else(|| format!("{at}.{key} must be a finite number"))?;
        }
        number("scale").filter(|scale| *scale > 0.0).ok_or_else(|| format!("{at}.scale must be above 0"))?;
        number("opacity")
            .filter(|opacity| (0.0..=1.0).contains(opacity))
            .ok_or_else(|| format!("{at}.opacity must be between 0 and 1"))?;
        for key in flags {
            if track.get(*key).is_some_and(|flag| !flag.is_boolean()) {
                return Err(format!("{at}.{key} must be a boolean"));
            }
        }
        Ok(())
    };
    let mut names = Vec::new();
    for (index, group) in list("groups")?.iter().enumerate() {
        let at = format!("animation.groups[{index}]");
        let name = group.get("name").and_then(Value::as_str).filter(|name| !name.is_empty());
        let name = name.ok_or_else(|| format!("{at}.name must be a nonempty string"))?;
        if names.contains(&name) {
            return Err(format!("{at}.name {name:?} is used twice"));
        }
        names.push(name);
        placed(group, &at, &["hidden", "locked"])?;
    }
    let member = |track: &Value, at: &str| match track.get("group") {
        None => Ok(()),
        Some(Value::String(group)) if names.contains(&group.as_str()) => Ok(()),
        Some(_) => Err(format!("{at}.group must name one of the clip's groups")),
    };
    for (index, image) in list("images")?.iter().enumerate() {
        let at = format!("animation.images[{index}]");
        if !image.get("path").is_some_and(Value::is_string) {
            return Err(format!("{at}.path must be a string"));
        }
        placed(image, &at, &["front", "hidden", "locked", "guide"])?;
        if let Some(sequence) = image.get("sequence") {
            validate_sequence(sequence, &format!("{at}.sequence"))?;
        }
        if let Some(remap) = image.get("timeRemap").filter(|remap| !remap.is_null()) {
            if !image.get("sequence").is_some_and(Value::is_object) {
                return Err(format!("{at}.timeRemap requires a sequence"));
            }
            sample_track(remap, length, 0.0, &format!("{at}.timeRemap"))?;
        }
        member(image, &at)?;
    }
    for (index, track) in list("audio")?.iter().enumerate() {
        member(track, &format!("animation.audio[{index}]"))?;
    }
    match animation.get("modelGroup") {
        None | Some(Value::Null) => {}
        Some(Value::String(group)) if names.contains(&group.as_str()) => {}
        Some(_) => return Err("animation.modelGroup must name one of the clip's groups".into()),
    }
    Ok(())
}

/// The audio tracks of a clip (ANI03), top track first: `audio: [{path, frame, mute?}]`, where `path` is a WAV file
/// and the sound starts at `frame`.
fn validate_audio(animation: &Value, length: usize) -> Result<(), String> {
    let Some(tracks) = animation.get("audio").filter(|tracks| !tracks.is_null()) else {
        return Ok(());
    };
    for (index, track) in tracks.as_array().ok_or("animation.audio must be an array")?.iter().enumerate() {
        let path = format!("animation.audio[{index}]");
        if !track.get("path").is_some_and(Value::is_string) {
            return Err(format!("{path}.path must be a string"));
        }
        if !track.get("frame").and_then(Value::as_u64).is_some_and(|frame| frame < length as u64) {
            return Err(format!("{path}.frame is outside animation length"));
        }
        if track.get("mute").is_some_and(|mute| !mute.is_boolean()) {
            return Err(format!("{path}.mute must be a boolean"));
        }
    }
    Ok(())
}

/// The eye blinking track of a clip (ANI02): `eyeBlink: {effect?, keyframes?: [{frame, value}]}`, values 0 (closed) to 1
/// (open) at linear interpolation, effect 0 to 100 (100 when absent). Returns the keys, their frames and the effect.
fn blink_track(animation: &Value, length: usize) -> Result<Option<(&[Value], Vec<i32>, f64)>, String> {
    let Some(blink) = animation.get("eyeBlink") else {
        return Ok(None);
    };
    let blink = blink.as_object().ok_or("animation.eyeBlink must be an object")?;
    let effect = blink.get("effect").map_or(Ok(100.0), |effect| bounded(effect, "animation.eyeBlink.effect", 100.0))?;
    let keys = match blink.get("keyframes") {
        None => &[][..],
        Some(keys) => keys.as_array().ok_or("animation.eyeBlink.keyframes must be an array")?.as_slice(),
    };
    let frames = validate_keyframes(keys, length, "animation.eyeBlink.keyframes", true)?;
    for (index, key) in keys.iter().enumerate() {
        bounded(&key["value"], &format!("animation.eyeBlink.keyframes[{index}].value"), 1.0)?;
    }
    Ok(Some((keys, frames, effect)))
}

/// The ids of `model.live2d.eyeBlink`, each once.
pub(crate) fn eye_parameters(model: &Value) -> impl Iterator<Item = u32> + '_ {
    let eyes = model.get("live2d").and_then(|live2d| live2d.get("eyeBlink")).and_then(Value::as_array);
    let mut seen = std::collections::BTreeSet::new();
    eyes.into_iter()
        .flatten()
        .filter_map(|id| id.as_u64().and_then(|id| u32::try_from(id).ok()))
        .filter(move |id| seen.insert(*id))
}

/// A finite number from 0 to `max`.
fn bounded(value: &Value, name: &str, max: f64) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| (0.0..=max).contains(value))
        .ok_or_else(|| format!("{name} must be a number from 0 to {max}"))
}

/// Creates or replaces animation `request.animation` with one Forced, linear lane per entry of `lanes`: (parameter,
/// axis, keys as (frame, value)). The animation lasts until the last key of the first lane and is stepped at
/// `timestep` seconds per frame. Returns its `{name, length, timestep}`.
pub(crate) fn replace_with_lanes(
    model: &mut Value,
    request: &Value,
    timestep: f32,
    lanes: &[(u32, usize, Vec<(i32, f32)>)],
) -> Result<Value, String> {
    let name = animation_name(request, "animation")?.to_owned();
    let length = lanes.first().and_then(|lane| lane.2.last()).map_or(1, |(frame, _)| *frame as i64 + 1);
    let lanes: Vec<Value> = lanes
        .iter()
        .map(|(parameter_id, axis, keys)| {
            let keyframes: Vec<Value> =
                keys.iter().map(|(frame, value)| json!({ "frame": frame, "value": value, "tension": 0.5 })).collect();
            json!({
                "uuid": parameter_id, "target": axis, "interpolation": "Linear", "merge_mode": "Forced",
                "keyframes": keyframes,
            })
        })
        .collect();
    let mut animation = json!({
        "length": length,
        "timestep": timestep,
        "additive": false,
        "animationWeight": 1.0,
        "leadIn": -1,
        "leadOut": -1,
        "lanes": lanes,
    });
    // A clip made again keeps the eye blinking of the old one, the keys that still fit in it.
    let old = model.get("animations").and_then(|animations| animations.get(&name));
    if let Some(mut blink) = old.and_then(|old| old.get("eyeBlink")).filter(|blink| blink.is_object()).cloned() {
        if let Some(keys) = blink.get_mut("keyframes").and_then(Value::as_array_mut) {
            keys.retain(|key| key["frame"].as_i64().is_some_and(|frame| frame < length));
            if keys.is_empty() {
                blink.as_object_mut().map(|blink| blink.remove("keyframes"));
            }
        }
        if blink.as_object().is_some_and(|blink| !blink.is_empty()) {
            animation["eyeBlink"] = blink;
        }
    }
    // And its audio tracks that still start inside it.
    if let Some(mut tracks) = old.and_then(|old| old.get("audio")).and_then(Value::as_array).cloned() {
        tracks.retain(|track| track["frame"].as_i64().is_some_and(|frame| frame < length));
        if !tracks.is_empty() {
            animation["audio"] = json!(tracks);
        }
    }
    // And its image and group tracks (ANI04), which have no frames.
    for key in ["images", "groups", "modelGroup"] {
        if let Some(value) = old.and_then(|old| old.get(key)).filter(|value| !value.is_null()) {
            animation[key] = value.clone();
        }
    }
    let length = validate_animation_header(&animation)?;
    validate_animation_keyframes(&animation, length)?;
    animations_mut(model, true)?.insert(name.clone(), animation);
    Ok(json!({ "name": name, "length": length, "timestep": timestep }))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClipboardEntry {
    pub axis: usize,
    pub value: f32,
}

pub(crate) fn clipboard_summaries(entries: &[ClipboardEntry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| json!({ "axis": entry.axis, "value": entry.value }))
            .collect(),
    )
}

struct KeyTarget<'a, 'b> {
    animation: &'a Value,
    name: &'b str,
    parameter_id: u32,
    is_vec2: bool,
    frame: i32,
    length: usize,
}

pub(crate) fn copy_keys(model: &Value, request: &Value) -> Result<Vec<ClipboardEntry>, String> {
    let target = key_target(model, request)?;
    let lanes = match target.animation.get("lanes") {
        None | Some(Value::Null) => &[][..],
        Some(value) => value
            .as_array()
            .ok_or("animation.lanes must be an array")?
            .as_slice(),
    };
    let mut copied = Vec::new();
    for (lane_index, lane) in lanes.iter().enumerate() {
        let path = format!("animation.lanes[{lane_index}]");
        if !lane.is_object() {
            return Err(format!("{path} must be an object"));
        }
        if lane_parameter_id(lane, &path)? != target.parameter_id {
            continue;
        }
        let axis = lane_axis(lane, target.is_vec2, &path)?;
        let keys = match lane.get("keyframes") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value
                .as_array()
                .ok_or_else(|| format!("{path}.keyframes must be an array"))?
                .as_slice(),
        };
        for (key_index, key) in keys.iter().enumerate() {
            let key_path = format!("{path}.keyframes[{key_index}]");
            if !key.is_object() {
                return Err(format!("{key_path} must be an object"));
            }
            let key_frame = key
                .get("frame")
                .and_then(Value::as_i64)
                .and_then(|frame| i32::try_from(frame).ok())
                .ok_or_else(|| format!("{key_path}.frame must be an integer"))?;
            if key_frame != target.frame {
                continue;
            }
            copied.push(ClipboardEntry {
                axis,
                value: finite_f32(
                    key.get("value")
                        .ok_or_else(|| format!("{key_path}.value is required"))?,
                    &format!("{key_path}.value"),
                )?,
            });
            break;
        }
    }
    if copied.is_empty() {
        return Err(format!("no keyframe at frame {}", target.frame));
    }
    Ok(copied)
}

pub(crate) fn paste_keys(
    model: &Value,
    request: &Value,
    entries: &[ClipboardEntry],
) -> Result<Value, String> {
    if entries.is_empty() {
        return Err("Clipboard empty".into());
    }
    let target = key_target(model, request)?;
    let axis_count = if target.is_vec2 { 2 } else { 1 };
    let compatible: Vec<ClipboardEntry> = entries
        .iter()
        .copied()
        .filter(|entry| entry.axis < axis_count)
        .collect();
    if compatible.is_empty() {
        return Err("no compatible copied keyframes".into());
    }
    write_keys(model, &target, &compatible)
}

pub(crate) fn add_keys(
    model: &Value,
    request: &Value,
    values: [f64; 2],
) -> Result<Value, String> {
    let target = key_target(model, request)?;
    let axis_count = if target.is_vec2 { 2 } else { 1 };
    let mut entries = Vec::with_capacity(axis_count);
    for (axis, value) in values.iter().enumerate().take(axis_count) {
        entries.push(ClipboardEntry {
            axis,
            value: finite_f32(
                &json!(*value),
                &format!("parameter {} value", target.parameter_id),
            )?,
        });
    }
    write_keys(model, &target, &entries)
}

/// "Key all" of the D timeline: a key on one frame for every listed parameter, or for every parameter of the model,
/// each axis taking its value from `values`. A key already on the frame takes the new value.
pub(crate) fn add_all_keys(model: &Value, request: &Value, values: &BTreeMap<u32, [f64; 2]>) -> Result<Value, String> {
    let name = animation_name(request, "name")?;
    let mut candidate = model.clone();
    let animation = animation_mut(&mut candidate, name)?;
    let length = validate_animation_header(animation)?;
    let frame = key_frame(request, length)?;
    let ids = listed_parameters(model, request)?.unwrap_or_else(|| {
        let mut ids: Vec<u32> = crate::parameter_edit::parameter_defaults(model).into_keys().collect();
        ids.sort_unstable();
        ids
    });
    if ids.is_empty() {
        return Err("the model has no parameter to key".into());
    }
    for id in ids {
        let pair = values.get(&id).ok_or_else(|| format!("parameter {id} has no runtime value"))?;
        for (axis, value) in pair.iter().enumerate().take(parameter_axes(model, id)?) {
            if held_lane(animation, u64::from(id), axis) {
                continue;
            }
            let value = finite_f32(&json!(*value), &format!("parameter {id} value"))?;
            set_key(animation, id, axis, frame, value, None, None, length)?;
        }
    }
    Ok(candidate)
}

/// The parameters a "key all" or "unkey all" request lists, or `None` when it has no list and means all of them.
fn listed_parameters(model: &Value, request: &Value) -> Result<Option<Vec<u32>>, String> {
    let Some(list) = request.get("parameters") else {
        return Ok(None);
    };
    let invalid = "parameters must be a non-empty array of parameter UUIDs";
    let list = list.as_array().filter(|list| !list.is_empty()).ok_or(invalid)?;
    // Each id is found in the model and listed once, so the list is never longer than the model has parameters.
    let mut ids = Vec::new();
    for item in list {
        let id = item.as_u64().and_then(crate::valid_id).ok_or(invalid)?;
        if ids.contains(&id) {
            return Err(format!("parameter {id} is listed twice"));
        }
        parameter_axes(model, id)?;
        ids.push(id);
    }
    Ok(Some(ids))
}

/// How many axes the plain parameter `id` has. A group or an unknown id is refused.
/// Writes recorded parameter values as keys of animation `name` (issue #177): each `(parameter, frame)` sets every
/// axis of the parameter. Frames past the end are dropped. Answers the keys written and the samples dropped.
pub(crate) fn record_keys(
    model: &Value,
    name: &str,
    samples: &BTreeMap<(u32, i64), [f64; 2]>,
) -> Result<(Value, usize, usize), String> {
    let mut candidate = model.clone();
    let axes = samples.keys().map(|(id, _)| parameter_axes(model, *id)).collect::<Result<Vec<_>, _>>()?;
    let animation = animation_mut(&mut candidate, name)?;
    let length = validate_animation_header(animation)?;
    let (mut written, mut dropped) = (0, 0);
    for (((id, frame), value), axes) in samples.iter().zip(axes) {
        let Some(frame) = i32::try_from(*frame).ok().filter(|frame| *frame >= 0 && (*frame as usize) < length) else {
            dropped += 1;
            continue;
        };
        for axis in 0..axes {
            set_key(animation, *id, axis, frame, value[axis] as f32, None, None, length)?;
            written += 1;
        }
    }
    Ok((candidate, written, dropped))
}

fn parameter_axes(model: &Value, id: u32) -> Result<usize, String> {
    let parameter = crate::find_parameter(model, id).ok_or_else(|| format!("parameter {id} was not found"))?;
    if parameter.get("groupUUID").is_some() {
        return Err(format!("parameter {id} refers to a parameter group"));
    }
    Ok(if parameter_is_vec2(parameter, "parameter")? { 2 } else { 1 })
}

fn write_keys(
    model: &Value,
    target: &KeyTarget,
    entries: &[ClipboardEntry],
) -> Result<Value, String> {
    let mut candidate = model.clone();
    let animation = animation_mut(&mut candidate, target.name)?;
    for entry in entries {
        set_key(
            animation,
            target.parameter_id,
            entry.axis,
            target.frame,
            entry.value,
            None,
            None,
            target.length,
        )?;
    }
    Ok(candidate)
}

fn key_target<'a, 'b>(
    model: &'a Value,
    request: &'b Value,
) -> Result<KeyTarget<'a, 'b>, String> {
    let name = animation_name(request, "name")?;
    let parameter_id = required_parameter_id(request)?;
    let is_vec2 = parameter_axes(model, parameter_id)? == 2;
    let animation = model
        .get("animations")
        .and_then(Value::as_object)
        .ok_or("model.animations must be an object")?
        .get(name)
        .ok_or_else(|| format!("animation {name:?} was not found"))?;
    let length = validate_animation_header(animation)?;
    let frame = key_frame(request, length)?;
    Ok(KeyTarget {
        animation,
        name,
        parameter_id,
        is_vec2,
        frame,
        length,
    })
}

/// The parameter values of clip `name` at `frame` over `base`; with `blink`, after the clip's eye blinking track.
pub(crate) fn sample(
    model: &Value,
    name: &str,
    frame: f64,
    base: &BTreeMap<u32, [f64; 2]>,
    blink: bool,
) -> Result<BTreeMap<u32, [f64; 2]>, String> {
    let animation = model
        .get("animations")
        .and_then(Value::as_object)
        .ok_or("model.animations must be an object")?
        .get(name)
        .ok_or_else(|| format!("animation {name:?} was not found"))?;
    let length = validate_animation_header(animation)?;
    crate::frame_step::validate(model, animation, length)?;
    finite_f32(&json!(frame), "frame")?;
    let raw_frame = frame.clamp(0.0, length as f64);
    let lanes = match animation.get("lanes") {
        None | Some(Value::Null) => &[][..],
        Some(value) => value
            .as_array()
            .ok_or("animation.lanes must be an array")?
            .as_slice(),
    };
    let mut values = base.clone();
    let mut states: BTreeMap<u32, MergeState> = BTreeMap::new();
    // While freeze is on, a frozen lane holds its value at the freeze frame.
    let held = freeze_frame(animation).map(|held| (held as f32).min(length as f32));
    let loop_editing = animation.get("loopEditing") == Some(&json!(true));
    let interpolate = |keys: &[Value], frames: &[i32], at: f32, mode: Interpolation| {
        if loop_editing {
            interpolate_periodic_keys(keys, frames, at, mode, length)
        } else {
            interpolate_keys(keys, frames, at, mode)
        }
    };

    for (lane_index, lane) in lanes.iter().enumerate() {
        let path = format!("animation.lanes[{lane_index}]");
        if !lane.is_object() {
            return Err(format!("{path} must be an object"));
        }
        let parameter_id = lane_parameter_id(lane, &path)?;
        let parameter = crate::find_parameter(model, parameter_id)
            .ok_or_else(|| format!("{path} refers to missing parameter {parameter_id}"))?;
        if parameter.get("groupUUID").is_some() {
            return Err(format!("{path} refers to parameter group {parameter_id}"));
        }
        let is_vec2 = parameter_is_vec2(parameter, &path)?;
        let axis = lane_axis(lane, is_vec2, &path)?;
        let interpolation = lane
            .get("interpolation")
            .filter(|value| !value.is_null())
            .map(parse_interpolation)
            .transpose()?
            .unwrap_or(Interpolation::Nearest);
        let merge_mode = lane_merge_mode(lane, parameter)?;
        let keys = match lane.get("keyframes") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value
                .as_array()
                .ok_or_else(|| format!("{path}.keyframes must be an array"))?
                .as_slice(),
        };
        let key_frames = validate_keyframes(keys, length, &format!("{path}.keyframes"), true)?;
        let raw_at = held.map(f64::from).filter(|_| lane.get("frozen") == Some(&json!(true))).unwrap_or(raw_frame);
        let at = crate::frame_step::parameter_frame(model, animation, parameter_id, length, raw_at)? as f32;
        let sampled = interpolate(keys, &key_frames, at, interpolation)?;
        let state = if let Some(state) = states.get_mut(&parameter_id) {
            state
        } else {
            let initial = match values.get(&parameter_id) {
                Some(pair) => pair_f32(*pair, "base parameter value")?,
                None => parameter_defaults(parameter)?,
            };
            values.insert(
                parameter_id,
                [f64::from(initial[0]), f64::from(initial[1])],
            );
            states.entry(parameter_id).or_insert(MergeState::new(initial))
        };
        state.push(axis, sampled, merge_mode);
    }

    for (id, state) in states {
        values.insert(id, state.finish(id)?);
    }
    // Eye blinking scales each eye parameter the model lists toward its minimum, which is the manual's multiplication
    // for a 0 (closed) to 1 (open) parameter and keeps any other range inside the parameter. An eye parameter without
    // a lane blinks from its runtime value.
    let track = if blink { blink_track(animation, length)? } else { None };
    if let Some((keys, frames, effect)) = track.filter(|(keys, ..)| !keys.is_empty()) {
        for id in eye_parameters(model) {
            // ponytail: an id another tool left behind is skipped, and only axis 0 of a 2D one blinks.
            let parameter = crate::find_parameter(model, id);
            let Some(parameter) = parameter.filter(|parameter| parameter.get("groupUUID").is_none()) else {
                continue;
            };
            let blink_raw = if lanes.iter().any(|lane| {
                lane.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
                    && lane.get("frozen") == Some(&json!(true))
            }) {
                held.map(f64::from).unwrap_or(raw_frame)
            } else {
                raw_frame
            };
            let blink_frame = crate::frame_step::parameter_frame(model, animation, id, length, blink_raw)? as f32;
            let open = f64::from(interpolate(keys, &frames, blink_frame, Interpolation::Linear)?);
            let scale = 1.0 + (open - 1.0) * effect / 100.0;
            let value = match values.get(&id) {
                Some(pair) => *pair,
                None => parameter_defaults(parameter)?.map(f64::from),
            };
            let min = parameter.get("min").and_then(|min| min.get(0)).and_then(Value::as_f64).unwrap_or(0.0);
            values.insert(id, [min + (value[0] - min) * scale, value[1]]);
        }
    }
    Ok(values)
}

#[derive(Clone, Copy)]
enum Interpolation {
    Nearest,
    Stepped,
    Linear,
    Cubic,
    Bezier,
}

#[derive(Clone, Copy)]
enum MergeMode {
    Forced,
    Additive,
    Multiplicative,
    Weighted,
    Passthrough,
}

struct MergeState {
    value: [f32; 2],
    additive: [f32; 2],
    multiplier_sum: [f32; 2],
    multiplier_count: usize,
}

impl MergeState {
    fn new(value: [f32; 2]) -> Self {
        Self {
            value,
            additive: [0.0; 2],
            multiplier_sum: [0.0; 2],
            multiplier_count: 0,
        }
    }

    fn push(&mut self, axis: usize, value: f32, mode: MergeMode) {
        match mode {
            MergeMode::Forced => self.value[axis] = value,
            MergeMode::Additive => {
                self.additive[axis] += value;
                // D's axis combinator stores 1 in the other component, including additive lanes.
                self.additive[1 - axis] += 1.0;
            }
            MergeMode::Multiplicative | MergeMode::Weighted => {
                self.multiplier_sum[axis] += value;
                self.multiplier_sum[1 - axis] += 1.0;
                self.multiplier_count += 1;
            }
            MergeMode::Passthrough => {}
        }
    }

    fn finish(self, id: u32) -> Result<[f64; 2], String> {
        let value = [
            (self.value[0] + self.additive[0])
                * (if self.multiplier_count == 0 {
                    1.0
                } else {
                    self.multiplier_sum[0] / self.multiplier_count as f32
                }),
            (self.value[1] + self.additive[1])
                * (if self.multiplier_count == 0 {
                    1.0
                } else {
                    self.multiplier_sum[1] / self.multiplier_count as f32
                }),
        ];
        if value.iter().any(|value| !value.is_finite()) {
            return Err(format!("animation sample for parameter {id} is not finite"));
        }
        Ok([f64::from(value[0]), f64::from(value[1])])
    }
}

fn animations_mut(
    model: &mut Value,
    create_missing: bool,
) -> Result<&mut Map<String, Value>, String> {
    let object = model.as_object_mut().ok_or("model must be an object")?;
    if create_missing {
        let animations = object
            .entry("animations".to_owned())
            .or_insert_with(|| json!({}));
        if animations.is_null() {
            *animations = json!({});
        }
        return animations
            .as_object_mut()
            .ok_or("model.animations must be an object".into());
    }
    object
        .get_mut("animations")
        .and_then(Value::as_object_mut)
        .ok_or("model.animations must be an object".into())
}

pub(crate) fn animation_mut<'a>(model: &'a mut Value, name: &str) -> Result<&'a mut Value, String> {
    animations_mut(model, false)?
        .get_mut(name)
        .ok_or_else(|| format!("animation {name:?} was not found"))
}

fn animation_name<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    let name = request
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    if name.trim().is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(name)
}

pub(crate) fn validate_animation_header(animation: &Value) -> Result<usize, String> {
    if !animation.is_object() {
        return Err("animation must be an object".into());
    }
    let length = parse_length(
        animation
            .get("length")
            .ok_or("animation.length is required")?,
    )? as usize;
    parse_timestep(
        animation
            .get("timestep")
            .ok_or("animation.timestep is required")?,
    )?;
    if let Some(additive) = animation.get("additive") {
        if !additive.is_boolean() {
            return Err("animation.additive must be a boolean".into());
        }
    }
    if let Some(weight) = animation.get("animationWeight") {
        finite_f32(weight, "animation.animationWeight")?;
    }
    for key in ["leadIn", "leadOut"] {
        if let Some(value) = animation.get(key) {
            let marker = integer(value, &format!("animation.{key}"))?;
            if marker < -1 || marker > length as i64 {
                return Err(format!("animation.{key} must be between -1 and length"));
            }
        }
    }
    if let Some(flag) = animation.get("loopEditing") {
        let flag = flag.as_bool().ok_or("animation.loopEditing must be a boolean")?;
        if flag {
            let lead_in = animation["leadIn"].as_i64().unwrap_or(-1);
            let lead_out = animation["leadOut"].as_i64().unwrap_or(-1);
            if !(lead_in == -1 || lead_in == 0) || !(lead_out == -1 || lead_out == length as i64) {
                return Err("animation.loopEditing requires leadIn at the clip start and leadOut at the clip end".into());
            }
        }
    }
    Ok(length)
}

fn parse_length(value: &Value) -> Result<i64, String> {
    let length = integer(value, "length")?;
    if !(1..=MAX_ANIMATION_LENGTH).contains(&length) {
        return Err(format!("length must be between 1 and {MAX_ANIMATION_LENGTH}"));
    }
    Ok(length)
}

fn parse_timestep(value: &Value) -> Result<f32, String> {
    let timestep = finite_f32(value, "timestep")?;
    if timestep <= 0.0 || timestep > MAX_TIMESTEP {
        return Err(format!("timestep must be positive and at most {MAX_TIMESTEP}"));
    }
    Ok(timestep)
}

fn integer(value: &Value, name: &str) -> Result<i64, String> {
    value
        .as_i64()
        .ok_or_else(|| format!("{name} must be an integer"))
}

fn finite_f32(value: &Value, name: &str) -> Result<f32, String> {
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{name} must be a finite number"))?;
    let value = number as f32;
    if !value.is_finite() {
        return Err(format!("{name} must fit in a finite f32"));
    }
    Ok(value)
}

fn pair_f32(pair: [f64; 2], name: &str) -> Result<[f32; 2], String> {
    let convert = |value: f64| {
        let converted = value as f32;
        if value.is_finite() && converted.is_finite() {
            Ok(converted)
        } else {
            Err(format!("{name} must fit in finite f32 values"))
        }
    };
    Ok([convert(pair[0])?, convert(pair[1])?])
}

fn parameter_defaults(parameter: &Value) -> Result<[f32; 2], String> {
    match parameter.get("defaults") {
        None | Some(Value::Null) => Ok([0.0, 0.0]),
        Some(value) => {
            let values = value
                .as_array()
                .filter(|values| values.len() == 2)
                .ok_or("parameter.defaults must contain two numbers")?;
            Ok([
                finite_f32(&values[0], "parameter.defaults")?,
                finite_f32(&values[1], "parameter.defaults")?,
            ])
        }
    }
}

fn required_parameter_id(request: &Value) -> Result<u32, String> {
    let id = request
        .get("parameter")
        .and_then(Value::as_u64)
        .and_then(crate::valid_id)
        .ok_or("parameter must be a nonzero u32 parameter UUID")?;
    Ok(id)
}

fn required_axis(request: &Value, parameter_id: u32, model: &Value) -> Result<usize, String> {
    let axis = request
        .get("axis")
        .and_then(Value::as_i64)
        .and_then(|axis| usize::try_from(axis).ok())
        .ok_or("axis must be 0 or 1")?;
    let axis_count = parameter_axes(model, parameter_id)?;
    if axis >= axis_count {
        return Err(if axis_count == 2 {
            "axis must be 0 or 1".into()
        } else {
            "1D parameters only support axis 0".into()
        });
    }
    Ok(axis)
}

fn parameter_is_vec2(parameter: &Value, path: &str) -> Result<bool, String> {
    match parameter.get("is_vec2") {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{path}.is_vec2 must be a boolean")),
    }
}

fn key_frame(request: &Value, length: usize) -> Result<i32, String> {
    let frame = request
        .get("frame")
        .and_then(Value::as_i64)
        .and_then(|frame| i32::try_from(frame).ok())
        .ok_or("frame must be an integer")?;
    if frame < 0 || frame as usize >= length {
        return Err(format!("frame must be between 0 and {}", length - 1));
    }
    Ok(frame)
}

fn lane_parameter_id(lane: &Value, path: &str) -> Result<u32, String> {
    lane.get("uuid")
        .and_then(Value::as_u64)
        .and_then(crate::valid_id)
        .ok_or_else(|| format!("{path}.uuid must be a nonzero u32 parameter UUID"))
}

fn lane_axis(lane: &Value, is_vec2: bool, path: &str) -> Result<usize, String> {
    let axis = lane
        .get("target")
        .and_then(Value::as_i64)
        .and_then(|axis| usize::try_from(axis).ok())
        .ok_or_else(|| format!("{path}.target must be 0 or 1"))?;
    let axis_count = if is_vec2 { 2 } else { 1 };
    if axis >= axis_count {
        return Err(if is_vec2 {
            format!("{path}.target must be 0 or 1")
        } else {
            format!("{path} targets axis 1 of a 1D parameter")
        });
    }
    Ok(axis)
}

fn validate_animation_keyframes(animation: &Value, length: usize) -> Result<(), String> {
    blink_track(animation, length)?;
    validate_audio(animation, length)?;
    validate_layers(animation)?;
    if animation.get("shyActive").is_some_and(|flag| !flag.is_boolean()) {
        return Err("animation.shyActive must be a boolean".into());
    }
    if animation.get("freezeFrame").is_some_and(|frame| frame.as_u64().is_none_or(|frame| frame >= length as u64)) {
        return Err("animation.freezeFrame must be a frame inside the animation".into());
    }
    crate::controller::validate(animation)?;
    crate::form::validate(animation, length)?;
    let labels = animation.get("bookmarks").filter(|labels| !labels.is_null());
    for (index, label) in labels.map(|labels| labels.as_array().ok_or("animation.bookmarks must be an array"))
        .transpose()?.into_iter().flatten().enumerate()
    {
        let ids = label["parameters"].as_array();
        if !(label["label"].as_str().is_some_and(|label| !label.trim().is_empty())
            && ["x", "y"].iter().all(|key| label[key].as_f64().is_some_and(f64::is_finite))
            && ids.is_some_and(|ids| ids.iter().all(|id| id.as_u64().and_then(crate::valid_id).is_some())))
        {
            return Err(format!("animation.bookmarks[{index}] needs a label, a finite x and y, and parameter UUIDs"));
        }
    }
    let lanes = match animation.get("lanes") {
        None | Some(Value::Null) => return Ok(()),
        Some(value) => value
            .as_array()
            .ok_or("animation.lanes must be an array")?,
    };
    for (index, lane) in lanes.iter().enumerate() {
        if !lane.is_object() {
            return Err(format!("animation.lanes[{index}] must be an object"));
        }
        for flag in ["shy", "frozen"] {
            if lane.get(flag).is_some_and(|value| !value.is_boolean()) {
                return Err(format!("animation.lanes[{index}].{flag} must be a boolean"));
            }
        }
        let keys = match lane.get("keyframes") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value
                .as_array()
                .ok_or_else(|| format!("animation.lanes[{index}].keyframes must be an array"))?
                .as_slice(),
        };
        validate_keyframes(
            keys,
            length,
            &format!("animation.lanes[{index}].keyframes"),
            true,
        )?;
    }
    Ok(())
}

fn validate_keyframes(
    keys: &[Value],
    length: usize,
    path: &str,
    require_sorted: bool,
) -> Result<Vec<i32>, String> {
    let mut frames = Vec::with_capacity(keys.len());
    for (index, key) in keys.iter().enumerate() {
        let key_path = format!("{path}[{index}]");
        if !key.is_object() {
            return Err(format!("{key_path} must be an object"));
        }
        let frame = key
            .get("frame")
            .and_then(Value::as_i64)
            .and_then(|frame| i32::try_from(frame).ok())
            .ok_or_else(|| format!("{key_path}.frame must be an integer"))?;
        if frame < 0 || frame as usize >= length {
            return Err(format!("{key_path}.frame is outside animation length"));
        }
        finite_f32(
            key.get("value")
                .ok_or_else(|| format!("{key_path}.value is required"))?,
            &format!("{key_path}.value"),
        )?;
        if let Some(tension) = key.get("tension") {
            finite_f32(tension, &format!("{key_path}.tension"))?;
        }
        frames.push(frame);
    }
    if frames.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(format!("{path} must have unique frames"));
    }
    if require_sorted && frames.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(format!("{path} must be sorted by frame"));
    }
    Ok(frames)
}

/// Sets one key of a lane, as `animation.key.set` does without a tension or an interpolation.
pub(crate) fn put_key(animation: &mut Value, id: u32, axis: usize, frame: i32, value: f32, length: usize)
    -> Result<(), String> {
    set_key(animation, id, axis, frame, value, None, None, length)
}

fn set_key(
    animation: &mut Value,
    parameter_id: u32,
    axis: usize,
    frame: i32,
    value: f32,
    tension: Option<f32>,
    interpolation: Option<Interpolation>,
    length: usize,
) -> Result<(), String> {
    let object = animation
        .as_object_mut()
        .ok_or("animation must be an object")?;
    let lanes = object
        .entry("lanes".to_owned())
        .or_insert_with(|| json!([]));
    if lanes.is_null() {
        *lanes = json!([]);
    }
    let lanes = lanes
        .as_array_mut()
        .ok_or("animation.lanes must be an array")?;
    let lane_index = find_lane(lanes, parameter_id, axis)?;
    if let Some(index) = lane_index {
        let lane = lanes[index]
            .as_object_mut()
            .ok_or("animation lane must be an object")?;
        if let Some(interpolation) = interpolation {
            lane.insert("interpolation".into(), json!(interpolation.as_str()));
        }
        let keys = lane
            .entry("keyframes".to_owned())
            .or_insert_with(|| json!([]));
        if keys.is_null() {
            *keys = json!([]);
        }
        let keys = keys
            .as_array_mut()
            .ok_or("animation lane keyframes must be an array")?;
        validate_keyframes(keys, length, "animation lane keyframes", false)?;
        keys.sort_by_key(|key| key["frame"].as_i64().unwrap_or_default());
        let frames = validate_keyframes(keys, length, "animation lane keyframes", true)?;
        if let Some(key_index) = frames.iter().position(|existing| *existing == frame) {
            let key = keys[key_index]
                .as_object_mut()
                .ok_or("animation keyframe must be an object")?;
            key.insert("value".into(), json!(value));
            if let Some(tension) = tension {
                key.insert("tension".into(), json!(tension));
            }
        } else {
            let mut key = json!({ "frame": frame, "value": value });
            if let Some(tension) = tension {
                key["tension"] = json!(tension);
            } else {
                key["tension"] = json!(0.5);
            }
            keys.push(key);
            keys.sort_by_key(|key| key["frame"].as_i64().unwrap_or_default());
        }
    } else {
        let interpolation = interpolation.unwrap_or(Interpolation::Linear);
        lanes.push(json!({
            "uuid": parameter_id,
            "target": axis,
            "interpolation": interpolation.as_str(),
            "merge_mode": "Forced",
            "keyframes": [{
                "frame": frame,
                "value": value,
                "tension": tension.unwrap_or(0.5),
            }],
        }));
    }
    Ok(())
}

fn delete_key(
    animation: &mut Value,
    parameter_id: u32,
    axis: usize,
    frame: i32,
    length: usize,
) -> Result<(), String> {
    let lanes = animation
        .get_mut("lanes")
        .and_then(Value::as_array_mut)
        .ok_or("animation keyframe lane was not found")?;
    let lane_index = find_lane(lanes, parameter_id, axis)?
        .ok_or("animation keyframe lane was not found")?;
    let keys = lanes[lane_index]
        .get_mut("keyframes")
        .and_then(Value::as_array_mut)
        .ok_or("animation keyframe was not found")?;
    let frames = validate_keyframes(keys, length, "animation lane keyframes", true)?;
    let key_index = frames
        .iter()
        .position(|existing| *existing == frame)
        .ok_or("animation keyframe was not found")?;
    keys.remove(key_index);
    Ok(())
}

fn find_lane(lanes: &[Value], parameter_id: u32, axis: usize) -> Result<Option<usize>, String> {
    for (index, lane) in lanes.iter().enumerate() {
        if lane.get("uuid").and_then(Value::as_u64) != Some(u64::from(parameter_id)) {
            continue;
        }
        let lane_axis = lane
            .get("target")
            .and_then(Value::as_i64)
            .and_then(|axis| usize::try_from(axis).ok())
            .ok_or("matching animation lane has an invalid target")?;
        if lane_axis == axis {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn parse_interpolation(value: &Value) -> Result<Interpolation, String> {
    match value.as_str() {
        Some("Nearest" | "nearest") => Ok(Interpolation::Nearest),
        Some("Stepped" | "stepped") => Ok(Interpolation::Stepped),
        Some("Linear" | "linear") => Ok(Interpolation::Linear),
        Some("Cubic" | "cubic") => Ok(Interpolation::Cubic),
        Some("Bezier" | "bezier") => Ok(Interpolation::Bezier),
        _ => Err("interpolation must be Nearest, Stepped, Linear, Cubic, or Bezier".into()),
    }
}

impl Interpolation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Nearest => "Nearest",
            Self::Stepped => "Stepped",
            Self::Linear => "Linear",
            Self::Cubic => "Cubic",
            Self::Bezier => "Bezier",
        }
    }
}

impl MergeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Forced => "Forced",
            Self::Additive => "Additive",
            Self::Multiplicative => "Multiplicative",
            Self::Weighted => "Weighted",
            Self::Passthrough => "Passthrough",
        }
    }
}

fn lane_merge_mode(lane: &Value, parameter: &Value) -> Result<MergeMode, String> {
    let lane_mode = lane
        .get("merge_mode")
        .filter(|value| !value.is_null())
        .map(parse_merge_mode)
        .transpose()?
        .unwrap_or(MergeMode::Forced);
    if !matches!(lane_mode, MergeMode::Passthrough) {
        return Ok(lane_mode);
    }
    parameter
        .get("merge_mode")
        .filter(|value| !value.is_null())
        .map(parse_merge_mode)
        .transpose()
        .map(|mode| mode.unwrap_or(MergeMode::Additive))
}

fn parse_merge_mode(value: &Value) -> Result<MergeMode, String> {
    match value.as_str() {
        Some("Forced" | "forced") => Ok(MergeMode::Forced),
        Some("Additive" | "additive") => Ok(MergeMode::Additive),
        Some("Multiplicative" | "multiplicative") => Ok(MergeMode::Multiplicative),
        Some("Weighted" | "weighted") => Ok(MergeMode::Weighted),
        Some("Passthrough" | "passthrough") => Ok(MergeMode::Passthrough),
        _ => Err("unsupported animation merge mode".into()),
    }
}

/// The value a keyed track `{interpolation?, keyframes: [{frame, value, tension?}]}` takes on `frame`, as a lane
/// samples it (Linear by default), or None without keys.
pub(crate) fn sample_track(track: &Value, length: usize, frame: f32, path: &str) -> Result<Option<f32>, String> {
    if !track.is_object() {
        return Err(format!("{path} must be an object"));
    }
    let interpolation = track.get("interpolation").filter(|value| !value.is_null()).map(parse_interpolation);
    let interpolation = interpolation.transpose()?.unwrap_or(Interpolation::Linear);
    let keys = match track.get("keyframes") {
        None | Some(Value::Null) => &[][..],
        Some(value) => value.as_array().ok_or_else(|| format!("{path}.keyframes must be an array"))?.as_slice(),
    };
    let frames = validate_keyframes(keys, length, &format!("{path}.keyframes"), true)?;
    if keys.is_empty() {
        return Ok(None);
    }
    interpolate_keys(keys, &frames, frame, interpolation).map(Some)
}

fn interpolate_keys(
    keys: &[Value],
    frames: &[i32],
    frame: f32,
    mode: Interpolation,
) -> Result<f32, String> {
    if keys.is_empty() {
        return Ok(0.0);
    }
    if keys.len() == 1 {
        return finite_f32(&keys[0]["value"], "animation key value");
    }
    for index in 0..keys.len() {
        if (frames[index] as f32) < frame {
            continue;
        }
        if index == 0 {
            return finite_f32(&keys[0]["value"], "animation key value");
        }
        let previous = finite_f32(&keys[index - 1]["value"], "animation key value")?;
        let current = finite_f32(&keys[index]["value"], "animation key value")?;
        let tonext = frames[index] as f32 - frame;
        let interval = frames[index] as f32 - frames[index - 1] as f32;
        let t = 1.0 - tonext / interval;
        let value = match mode {
            Interpolation::Nearest => {
                if t > 0.5 {
                    current
                } else {
                    previous
                }
            }
            Interpolation::Stepped => previous,
            Interpolation::Linear => previous * (1.0 - t) + current * t,
            Interpolation::Cubic => {
                let p0 = key_value(keys, index.saturating_sub(2))?;
                let p1 = previous;
                let p2 = current;
                let p3 = key_value(keys, (index + 1).min(keys.len() - 1))?;
                let t2 = t * t;
                let t3 = t2 * t;
                let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
                let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
                let c = -0.5 * p0 + 0.5 * p2;
                a * t3 + b * t2 + c * t + p1
            }
            Interpolation::Bezier => {
                let tension = keys[index]
                    .get("tension")
                    .map(|value| finite_f32(value, "animation key tension"))
                    .transpose()?
                    .unwrap_or(0.5);
                let t2 = t * t;
                let t3 = t2 * t;
                let h1 = 2.0 * t3 - 3.0 * t2 + 1.0;
                let h2 = -2.0 * t3 + 3.0 * t2;
                let h3 = t3 - 2.0 * t2 + t;
                let h4 = t3 - t2;
                let weight = (h1 * 0.0 + h3 * (2.0 * tension) + h2 * 1.0 + h4 * (2.0 * tension))
                    .clamp(0.0, 1.0);
                previous * (1.0 - weight) + current * weight
            }
        };
        if !value.is_finite() {
            return Err("animation interpolation produced a non-finite value".into());
        }
        return Ok(value);
    }
    finite_f32(
        keys.last().ok_or("animation keyframe list is empty")?.get("value").ok_or("key value is required")?,
        "animation key value",
    )
}

fn key_value(keys: &[Value], index: usize) -> Result<f32, String> {
    finite_f32(
        keys[index]
            .get("value")
            .ok_or("animation key value is required")?,
        "animation key value",
    )
}

fn interpolate_periodic_keys(
    keys: &[Value],
    frames: &[i32],
    frame: f32,
    mode: Interpolation,
    length: usize,
) -> Result<f32, String> {
    if keys.len() <= 1 {
        return interpolate_keys(keys, frames, frame, mode);
    }
    let at = frame.rem_euclid(length as f32);
    if let Some(index) = frames.iter().position(|f| (*f as f32) == at) {
        return finite_f32(&keys[index]["value"], "animation key value");
    }
    let count = keys.len();
    let (i1, s1) = match frames.iter().rposition(|f| (*f as f32) < at) {
        Some(index) => (index, 0i64),
        None => (count - 1, -1i64),
    };
    let (i0, s0) = if i1 == 0 { (count - 1, s1 - 1) } else { (i1 - 1, s1) };
    let (i2, s2) = if i1 == count - 1 { (0, s1 + 1) } else { (i1 + 1, s1) };
    let (i3, s3) = if i2 == count - 1 { (0, s2 + 1) } else { (i2 + 1, s2) };
    let length = length as i64;
    let neighbors = [
        keys[i0].clone(),
        keys[i1].clone(),
        keys[i2].clone(),
        keys[i3].clone(),
    ];
    let virtual_frames = [
        i32::try_from(frames[i0] as i64 + s0 * length).map_err(|_| "frame is out of range")?,
        i32::try_from(frames[i1] as i64 + s1 * length).map_err(|_| "frame is out of range")?,
        i32::try_from(frames[i2] as i64 + s2 * length).map_err(|_| "frame is out of range")?,
        i32::try_from(frames[i3] as i64 + s3 * length).map_err(|_| "frame is out of range")?,
    ];
    interpolate_keys(&neighbors, &virtual_frames, at, mode)
}

#[cfg(test)]
mod tests {
    use super::{apply, sample};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn model() -> Value {
        json!({
            "param": [{
                "groupUUID": 10,
                "name": "Face",
                "children": [{
                    "uuid": 17,
                    "name": "Move",
                    "is_vec2": true,
                    "defaults": [0.0, 0.0],
                    "merge_mode": "Additive",
                    "custom_parameter": "keep",
                }],
            }],
            "animations": {},
            "custom_model": {"keep": true},
        })
    }

    fn animation(lanes: Value) -> Value {
        json!({
            "length": 4,
            "timestep": 1.0 / 60.0,
            "additive": false,
            "animationWeight": 1.0,
            "leadIn": -1,
            "leadOut": -1,
            "lanes": lanes,
            "custom_animation": "keep",
        })
    }

    fn lane(axis: usize, mode: &str, interpolation: &str, keys: Value) -> Value {
        json!({
            "uuid": 17,
            "target": axis,
            "merge_mode": mode,
            "interpolation": interpolation,
            "keyframes": keys,
        })
    }

    fn sampled(model: &Value, frame: f64, base: [f64; 2]) -> [f64; 2] {
        sample(model, "Idle", frame, &BTreeMap::from([(17, base)]), true).unwrap()[&17]
    }

    #[test]
    fn imported_animation_names_are_exact_identifiers() {
        let mut source = model();
        source["animations"]["Idle"] = animation(json!([]));
        source["animations"][" Idle "] = animation(json!([]));
        for (op, request) in [
            ("animation.delete", json!({"name":" Idle "})),
            ("animation.rename", json!({"name":" Idle ","new_name":" Renamed "})),
            ("animation.set", json!({"name":" Idle ","animationWeight":0.5})),
            ("animation.key.set", json!({"name":" Idle ","parameter":17,"axis":0,"frame":0,"value":0.5})),
        ] {
            let edited = apply(&source, &request, op).unwrap();
            assert_eq!(edited["animations"]["Idle"], source["animations"]["Idle"]);
            assert_ne!(edited["animations"][" Idle "], source["animations"][" Idle "]);
            if op == "animation.rename" {
                assert_eq!(edited["animations"][" Renamed "], source["animations"][" Idle "]);
            }
        }
        assert!(apply(&source, &json!({"name":"   "}), "animation.create").is_err());
    }

    #[test]
    fn crud_keeps_unknown_animation_and_model_fields() {
        let mut model = model();
        model["animations"]["Imported"] = animation(json!([lane(
            0,
            "Forced",
            "Linear",
            json!([{"frame": 0, "value": 1.0, "future_key": 42}]),
        )]));
        let created = apply(&model, &json!({"name":"New"}), "animation.create").unwrap();
        assert_eq!(created["animations"]["New"]["length"], 60);
        assert_eq!(
            created["animations"]["New"]["timestep"],
            json!(1.0f32 / 60.0f32)
        );
        let renamed = apply(
            &created,
            &json!({"name":"Imported", "new_name":"Renamed"}),
            "animation.rename",
        )
        .unwrap();
        assert_eq!(renamed["animations"]["Renamed"]["custom_animation"], "keep");
        assert_eq!(renamed["animations"]["Renamed"]["lanes"][0]["keyframes"][0]["future_key"], 42);
        assert_eq!(renamed["custom_model"]["keep"], true);
        let updated = apply(
            &renamed,
            &json!({"name":"Renamed", "length":8, "leadOut":8}),
            "animation.set",
        )
        .unwrap();
        assert_eq!(updated["animations"]["Renamed"]["length"], 8);
        let deleted = apply(&updated, &json!({"name":"New"}), "animation.delete").unwrap();
        assert!(deleted["animations"].get("New").is_none());
    }

    #[test]
    fn key_set_replaces_and_sorts_without_losing_unknown_fields_then_deletes() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([{
            "uuid": 17,
            "target": 0,
            "interpolation": "Linear",
            "merge_mode": "Forced",
            "custom_lane": "keep",
            "keyframes": [{"frame": 2, "value": 3.0, "tension": 0.25, "custom_key": true}],
        }]));
        let updated = apply(
            &model,
            &json!({"name":"Idle", "parameter":17, "axis":0, "frame":2, "value":4.0, "interpolation":"Bezier"}),
            "animation.key.set",
        )
        .unwrap();
        assert_eq!(updated["animations"]["Idle"]["lanes"][0]["interpolation"], "Bezier");
        assert_eq!(updated["animations"]["Idle"]["lanes"][0]["custom_lane"], "keep");
        assert_eq!(updated["animations"]["Idle"]["lanes"][0]["keyframes"][0]["value"], 4.0);
        assert_eq!(updated["animations"]["Idle"]["lanes"][0]["keyframes"][0]["tension"], 0.25);
        assert_eq!(updated["animations"]["Idle"]["lanes"][0]["keyframes"][0]["custom_key"], true);
        let inserted = apply(
            &updated,
            &json!({"name":"Idle", "parameter":17, "axis":0, "frame":0, "value":1.0}),
            "animation.key.set",
        )
        .unwrap();
        assert_eq!(inserted["animations"]["Idle"]["lanes"][0]["keyframes"][0]["frame"], 0);
        let deleted = apply(
            &inserted,
            &json!({"name":"Idle", "parameter":17, "axis":0, "frame":0}),
            "animation.key.delete",
        )
        .unwrap();
        assert_eq!(deleted["animations"]["Idle"]["lanes"][0]["keyframes"].as_array().unwrap().len(), 1);
        assert_eq!(deleted["animations"]["Idle"]["lanes"][0]["custom_lane"], "keep");
    }

    #[test]
    fn rejected_edits_are_atomic_and_name_collisions_fail() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([]));
        let original = model.clone();
        assert!(apply(&model, &json!({"name":"Idle", "length":0}), "animation.set").is_err());
        assert_eq!(model, original);
        assert!(apply(&model, &json!({"name":"Idle"}), "animation.create").is_err());
        assert!(apply(
            &model,
            &json!({"name":"Idle", "new_name":"Idle"}),
            "animation.rename"
        )
        .is_ok());
    }

    #[test]
    fn interpolation_matches_d_endpoints_stepped_exact_key_and_bezier_tension() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([lane(
            0,
            "Forced",
            "Linear",
            json!([{"frame":0,"value":0.0},{"frame":2,"value":10.0}]),
        )]));
        assert_eq!(sampled(&model, -4.0, [9.0, 0.0])[0], 0.0);
        assert_eq!(sampled(&model, 99.0, [9.0, 0.0])[0], 10.0);
        assert_eq!(sampled(&model, 1.0, [9.0, 0.0])[0], 5.0);

        model["animations"]["Idle"]["lanes"][0]["interpolation"] = json!("Nearest");
        assert_eq!(sampled(&model, 1.0, [9.0, 0.0])[0], 0.0);
        assert_eq!(sampled(&model, 1.5, [9.0, 0.0])[0], 10.0);

        model["animations"]["Idle"]["lanes"][0]["interpolation"] = json!("Stepped");
        assert_eq!(sampled(&model, 2.0, [9.0, 0.0])[0], 0.0);
        assert_eq!(sampled(&model, 2.25, [9.0, 0.0])[0], 10.0);

        model["animations"]["Idle"]["lanes"][0]["interpolation"] = json!("Bezier");
        model["animations"]["Idle"]["lanes"][0]["keyframes"][1]["frame"] = json!(1);
        model["animations"]["Idle"]["lanes"][0]["keyframes"][1]["tension"] = json!(0.0);
        assert_eq!(sampled(&model, 0.25, [9.0, 0.0])[0], 1.5625);
        model["animations"]["Idle"]["lanes"][0]["keyframes"][1]["tension"] = json!(1.0);
        assert_eq!(sampled(&model, 0.25, [9.0, 0.0])[0], 3.4375);
    }

    #[test]
    fn cubic_and_lane_merge_modes_match_d_combinators() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([
            lane(
                0,
                "Passthrough",
                "Cubic",
                json!([
                    {"frame":0,"value":0.0},
                    {"frame":1,"value":10.0},
                    {"frame":2,"value":0.0},
                    {"frame":3,"value":0.0},
                ]),
            ),
            lane(1, "Forced", "Linear", json!([{"frame":0,"value":7.0}])),
            lane(0, "Multiplicative", "Linear", json!([{"frame":0,"value":2.0}])),
            lane(1, "Weighted", "Linear", json!([{"frame":0,"value":3.0}])),
        ]));
        let value = sampled(&model, 1.5, [10.0, 20.0]);
        assert_eq!(value, [23.4375, 16.0]);
    }

    #[test]
    fn missing_parameter_and_invalid_key_ranges_are_reported() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([{
            "uuid": 999,
            "target": 0,
            "keyframes": [],
        }]));
        assert!(sample(&model, "Idle", 0.0, &BTreeMap::new(), true)
            .unwrap_err()
            .contains("missing parameter"));
        assert!(apply(
            &model,
            &json!({"name":"Idle", "parameter":17, "axis":0, "frame":4, "value":1.0}),
            "animation.key.set"
        )
        .is_err());
    }

    #[test]
    fn unsupported_lane_modes_fail_closed() {
        let mut model = model();
        model["animations"]["Idle"] = animation(json!([lane(
            0,
            "Unknown",
            "Linear",
            json!([{"frame":0,"value":1.0}]),
        )]));
        assert!(sample(&model, "Idle", 0.0, &BTreeMap::new(), true)
            .unwrap_err()
            .contains("unsupported animation merge mode"));
    }
}
