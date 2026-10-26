//! Physics playlists (PHY03, issue #238): INX clips played one after another, and scene groups that play clips at
//! once, each overwriting or adding to the values by a weight. Stored in the model as `playlists` (name to entries
//! `{animation}` or `{group}`) and `sceneGroups` (name to `[{animation, type, weight}]`).

use crate::animation;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const MAX_ENTRIES: usize = 256;

/// `animation.playlist.set|delete` and `animation.scene_group.set|delete` on a copy of `model`.
pub(crate) fn apply(model: &Value, request: &Value, op: &str) -> Result<Value, String> {
    let name = request
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or("name must be a non-empty string")?;
    let mut candidate = model.clone();
    match op {
        "animation.playlist.set" => {
            let entries = list(request, "entries")?
                .iter()
                .map(|entry| playlist_entry(model, entry))
                .collect::<Result<Vec<_>, _>>()?;
            table(&mut candidate, "playlists")?.insert(name.to_owned(), json!(entries));
        }
        "animation.scene_group.set" => {
            let scenes = list(request, "scenes")?
                .iter()
                .map(|scene| group_scene(model, scene))
                .collect::<Result<Vec<_>, _>>()?;
            table(&mut candidate, "sceneGroups")?.insert(name.to_owned(), json!(scenes));
        }
        "animation.playlist.delete" | "animation.scene_group.delete" => {
            let key = if op == "animation.playlist.delete" { "playlists" } else { "sceneGroups" };
            if key == "sceneGroups" {
                let playlists = model.get("playlists").and_then(Value::as_object).into_iter().flatten();
                if let Some((playlist, _)) = playlists.into_iter().find(|(_, entries)| {
                    entries.as_array().into_iter().flatten().any(|entry| entry["group"] == name)
                }) {
                    return Err(format!("scene group {name:?} is in playlist {playlist:?}"));
                }
            }
            let entries = table(&mut candidate, key)?;
            entries.remove(name).ok_or_else(|| format!("{} {name:?} was not found", &key[..key.len() - 1]))?;
            if entries.is_empty() {
                candidate.as_object_mut().ok_or("model must be an object")?.remove(key);
            }
        }
        other => return Err(format!("unknown playlist operation {other:?}")),
    }
    Ok(candidate)
}

/// Where clip `name` is used: `playlist "P"` or `scene group "G"`.
pub(crate) fn clip_user(model: &Value, name: &str) -> Option<String> {
    for (key, label) in [("playlists", "playlist"), ("sceneGroups", "scene group")] {
        for (user, entries) in model.get(key).and_then(Value::as_object).into_iter().flatten() {
            if entries.as_array().into_iter().flatten().any(|entry| entry["animation"] == name) {
                return Some(format!("{label} {user:?}"));
            }
        }
    }
    None
}

/// Points every playlist entry and scene of clip `old` at `new`.
pub(crate) fn rename_clip(model: &mut Value, old: &str, new: &str) {
    for key in ["playlists", "sceneGroups"] {
        let tables = model.get_mut(key).and_then(Value::as_object_mut).into_iter().flat_map(|table| table.values_mut());
        for entry in tables.filter_map(Value::as_array_mut).flatten() {
            if entry["animation"] == old {
                entry["animation"] = json!(new);
            }
        }
    }
}

fn list<'a>(request: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    request
        .get(key)
        .and_then(Value::as_array)
        .filter(|items| (1..=MAX_ENTRIES).contains(&items.len()))
        .ok_or_else(|| format!("{key} must hold 1 to {MAX_ENTRIES} items"))
}

fn table<'a>(model: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>, String> {
    let model = model.as_object_mut().ok_or("model must be an object")?;
    let entry = model.entry(key.to_owned()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().ok_or_else(|| format!("model.{key} must be an object"))
}

fn clip_name<'a>(model: &Value, value: Option<&'a Value>) -> Result<&'a str, String> {
    let name = value.and_then(Value::as_str).ok_or("animation must be a string")?;
    if model["animations"].get(name).is_none() {
        return Err(format!("animation {name:?} was not found"));
    }
    Ok(name)
}

fn playlist_entry(model: &Value, entry: &Value) -> Result<Value, String> {
    match (entry.get("animation"), entry.get("group")) {
        (Some(animation), None) => Ok(json!({"animation": clip_name(model, Some(animation))?})),
        (None, Some(group)) => {
            let group = group.as_str().ok_or("group must be a string")?;
            if model["sceneGroups"].get(group).is_none() {
                return Err(format!("scene group {group:?} was not found"));
            }
            Ok(json!({"group": group}))
        }
        _ => Err("a playlist entry holds either animation or group".into()),
    }
}

fn group_scene(model: &Value, scene: &Value) -> Result<Value, String> {
    let animation = clip_name(model, scene.get("animation"))?;
    let kind = match scene.get("type").map(|kind| kind.as_str()) {
        None | Some(Some("overwrite")) => "overwrite",
        Some(Some("add")) => "add",
        _ => return Err("type must be overwrite or add".into()),
    };
    let weight = match scene.get("weight") {
        None => 100.0,
        Some(weight) => weight.as_f64().filter(|weight| (0.0..=100.0).contains(weight))
            .ok_or("weight must be a number from 0 to 100")?,
    };
    Ok(json!({"animation": animation, "type": kind, "weight": weight}))
}

/// The clips an entry plays at once, with their type and weight (0 to 1).
fn entry_clips(model: &Value, entry: &Value) -> Result<Vec<(String, bool, f64)>, String> {
    if let Some(animation) = entry["animation"].as_str() {
        return Ok(vec![(animation.to_owned(), false, 1.0)]);
    }
    let group = entry["group"].as_str().ok_or("a playlist entry holds either animation or group")?;
    let scenes = model["sceneGroups"][group].as_array().ok_or_else(|| format!("scene group {group:?} was not found"))?;
    Ok(scenes
        .iter()
        .map(|scene| {
            let weight = scene["weight"].as_f64().unwrap_or(100.0) / 100.0;
            (scene["animation"].as_str().unwrap_or_default().to_owned(), scene["type"] == "add", weight)
        })
        .collect())
}

fn clip_seconds(model: &Value, name: &str) -> Result<f64, String> {
    let animation = &model["animations"][name];
    let length = animation["length"].as_f64().ok_or_else(|| format!("animation {name:?} was not found"))?;
    Ok(length * animation["timestep"].as_f64().ok_or("animation timestep is missing")?)
}

/// Each entry's clips and its seconds, the longest of its clips.
fn timeline(model: &Value, name: &str) -> Result<Vec<(Vec<(String, bool, f64)>, f64)>, String> {
    let entries = model["playlists"][name].as_array().ok_or_else(|| format!("playlist {name:?} was not found"))?;
    entries
        .iter()
        .map(|entry| {
            let clips = entry_clips(model, entry)?;
            let seconds = clips.iter().map(|(clip, ..)| clip_seconds(model, clip)).try_fold(0.0f64, |longest, seconds| {
                seconds.map(|seconds| longest.max(seconds))
            })?;
            Ok((clips, seconds))
        })
        .collect()
}

/// Seconds the whole playlist plays.
pub(crate) fn duration(model: &Value, name: &str) -> Result<f64, String> {
    Ok(timeline(model, name)?.iter().map(|(_, seconds)| seconds).sum())
}

/// The values of playlist `name` at `time` seconds (clamped to the playlist) over `base`, and which entry plays.
/// In a group the clips apply in order: overwrite moves a parameter the clip animates toward the clip's value by
/// the weight, add adds the clip's offset from the parameter default times the weight.
pub(crate) fn sample(
    model: &Value,
    name: &str,
    time: f64,
    base: &BTreeMap<u32, [f64; 2]>,
) -> Result<(BTreeMap<u32, [f64; 2]>, f64, usize), String> {
    let entries = timeline(model, name)?;
    let total: f64 = entries.iter().map(|(_, seconds)| seconds).sum();
    if !time.is_finite() || total <= 0.0 {
        return Err("the playlist has no length to play".into());
    }
    let time = time.clamp(0.0, total);
    let (mut start, mut index) = (0.0, entries.len() - 1);
    for (at, (_, seconds)) in entries.iter().enumerate() {
        if time < start + seconds || at + 1 == entries.len() {
            index = at;
            break;
        }
        start += seconds;
    }
    let local = time - start;
    let mut values = base.clone();
    for (clip, add, weight) in &entries[index].0 {
        let animation = &model["animations"][clip];
        let frame = local / animation["timestep"].as_f64().ok_or("animation timestep is missing")?;
        let mut defaults = BTreeMap::new();
        for lane in animation["lanes"].as_array().into_iter().flatten() {
            if let Some(id) = lane["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()) {
                let parameter = crate::find_parameter(model, id).ok_or_else(|| format!("parameter {id} was not found"))?;
                defaults.insert(id, crate::parse_pair(&parameter["defaults"], "defaults")?);
            }
        }
        let sampled = animation::sample(model, clip, frame, if *add { &defaults } else { &values }, false)?;
        for (&id, default) in &defaults {
            let Some(value) = sampled.get(&id).copied() else { continue };
            let current = values.get(&id).copied().unwrap_or(*default);
            let next = std::array::from_fn(|axis| {
                if *add {
                    current[axis] + (value[axis] - default[axis]) * weight
                } else {
                    current[axis] + (value[axis] - current[axis]) * weight
                }
            });
            values.insert(id, next);
        }
    }
    Ok((values, time, index))
}
