use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::Session;

pub(crate) fn handle_mcp(
    session: &mut Session,
    request: &Value,
    mut guard: impl FnMut(&mut Session, &Value) -> Result<Option<crate::export_ops::PreparedExport>, String>,
) -> Option<Value> {
    if !valid_envelope(request) {
        return Some(rpc_error(Value::Null, -32600, "invalid JSON-RPC request"));
    }
    if let Some(call) = editor_request(request) {
        // No window here: the Session-held part of the state is all there is, and the tools have nothing to set.
        let outcome = match call {
            Ok(call) if call["op"] == "editor.state" => Ok(editor_state(session, None)),
            _ => Err(format!(
                "{} needs the editor: this host has no window",
                request["params"]["name"].as_str().unwrap_or_default()
            )),
        };
        return editor_reply(request, outcome);
    }
    let id = request.get("id").cloned();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return id.map(|id| rpc_error(id, -32600, "request requires a string method"));
    };
    let has_id = request.get("id").is_some();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
            "serverInfo": { "name": "soul2d-core", "version": env!("CARGO_PKG_VERSION") },
        })),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(session, &params, &mut guard),
        "resources/list" => Ok(json!({ "resources": resources(session) })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": resource_templates() })),
        // The native render model is built on demand, which the shared `&Session` reader cannot do.
        "resources/read" if params["uri"] == STATISTICS_URI => {
            let reply = session.handle_request(&json!({"op": "model.statistics"}));
            if reply["succeeded"] == true {
                let text = reply["result"].to_string();
                Ok(json!({ "contents": [{ "uri": STATISTICS_URI, "mimeType": "application/json", "text": text }] }))
            } else {
                Err((-32002, reply["message"].as_str().unwrap_or("model statistics failed").to_owned()))
            }
        }
        "resources/read" if params["uri"] == VIDEO_CODECS_URI => {
            let content = session.op_export_video_probe_status();
            Ok(json!({"contents":[{"uri":VIDEO_CODECS_URI,"mimeType":"application/json","text":content.to_string()}]}))
        }
        "resources/read" => read_resource(session, &params),
        "prompts/list" => Ok(json!({ "prompts": prompts_list() })),
        "prompts/get" => prompt_get(&params),
        _ => return id.map(|id| rpc_error(id, -32601, "method not found")),
    };
    if !has_id {
        return None;
    }
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => rpc_error(id.unwrap_or(Value::Null), code, &message),
    })
}

fn valid_envelope(request: &Value) -> bool {
    let valid_id = request.get("id").is_none_or(|id| {
        id.is_null() || id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()
    });
    request.is_object()
        && request.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && request.get("method").and_then(Value::as_str).is_some()
        && valid_id
}

// ------------------------------------------------------------------------------------ editor state

/// What the user has selected and is looking at. A host without a window serves the Session-held part only.
pub(crate) const EDITOR_STATE_URI: &str = "resource://soul2d/editor/state";
const STATISTICS_URI: &str = "resource://soul2d/document/statistics";
const RIG_IMPORT_REPORT_URI: &str = "resource://soul2d/rig/import-report";
const GLTF_EXPORT_REPORT_URI: &str = "resource://soul2d/export/gltf-report";
const VIDEO_CODECS_URI: &str = "resource://soul2d/export/video-codecs";
const VIDEO_COMMAND_URI: &str = "resource://soul2d/export/video-command";
const MESH_IMPORT_REPORT_URI: &str = "resource://soul2d/mesh/import-report";
const MMD_REPORT_URI: &str = "resource://soul2d/rig/mmd-report";
const DEFORMER_VALIDATION_URI: &str = "resource://soul2d/deformers/validation";
const AUTOMESH_PRESETS_URI: &str = "resource://soul2d/automesh/presets";
const PHYSICS_PRESETS_URI: &str = "resource://soul2d/physics/presets";
const ANIMATION_TEMPLATES_URI: &str = "resource://soul2d/animation/templates";
const PHYSICS_SOURCE_URI: &str = "resource://soul2d/document/physics-source";
const PHYSICS3_PRESETS_URI: &str = "resource://soul2d/physics3/presets";
const PART_SPLIT_PLAN_URI: &str = "resource://soul2d/part-split/plan";
const PART_SPLIT_SOURCE_URI: &str = "resource://soul2d/part-split/source";
const PART_SPLIT_MASK_PREFIX: &str = "resource://soul2d/part-split/masks/";
const OBJECT_NAMES_CSV_URI: &str = "resource://soul2d/object/names-csv";
/// Tools that set state only the editor window holds. Every other host refuses them, and the manifest
/// lists them with `headless_ok: false`.
pub(crate) const EDITOR_TOOLS: &[&str] = &[
    "editor_quit",
    "editor_selection_set", "editor_armed_set", "editor_camera_set", "editor_camera_fit", "editor_camera_sync", "editor_tool_set", "editor_grid_set",
    "editor_guides_set", "editor_physics_preview_set", "editor_solo_set", "editor_onion_set", "editor_playback_set", "editor_timeline_zoom_set", "editor_viewport_set", "editor_address_dialog", "editor_about_set", "editor_welcome_set", "editor_command_browser_set", "editor_texture_viewer_set", "editor_parameters_set", "editor_panel_set", "editor_layout_reset", "editor_workspace_save",
    "editor_workspace_apply", "editor_native_parameters_set",
    "editor_workspace_delete", "editor_recovery_list", "editor_recovery_keep", "editor_recovery_restore",
    "editor_settings_get", "editor_settings_set", "editor_shortcut_set",
];
/// Longest workspace name, in characters.
const EDITOR_MAX_WORKSPACE_NAME: usize = 64;
const EDITOR_MAX_NODES: usize = 4096;
/// Most vertex indices one selection call may name.
const EDITOR_MAX_VERTICES: usize = 65536;
const EDITOR_MAX_GUIDES: usize = 256;
const EDITOR_MAX_MEMO: usize = 256;
/// Largest camera coordinate accepted: far beyond any puppet, small enough that view maths stays finite.
const EDITOR_MAX_COORDINATE: f64 = 1e9;

/// The `editor.*` request a JSON-RPC message carries: a read of the editor state resource, or a call of an
/// editor tool with its arguments checked. `None` for every other message, malformed envelopes included.
pub(crate) fn editor_request(request: &Value) -> Option<Result<Value, String>> {
    if !valid_envelope(request) {
        return None;
    }
    let params = &request["params"];
    match request["method"].as_str()? {
        "resources/read" if params["uri"] == EDITOR_STATE_URI => Some(Ok(json!({"op": "editor.state"}))),
        "tools/call" => {
            let name = params["name"].as_str().filter(|name| EDITOR_TOOLS.contains(name))?;
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Some(check_declared_arguments(name, &arguments).and_then(|()| editor_tool_request(name, &arguments)))
        }
        _ => None,
    }
}

/// `[r, g, b, a]`, each an integer from 0 to 255.
fn rgba(value: &Value, what: &str) -> Result<Vec<u8>, String> {
    let channels: Vec<u8> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|channel| channel.as_u64().and_then(|channel| u8::try_from(channel).ok()))
        .collect();
    // Both lengths: a bad entry in a list of five would otherwise leave four good ones.
    if value.as_array().map(Vec::len) != Some(4) || channels.len() != 4 {
        return Err(format!("{what} must be [r, g, b, a], each an integer from 0 to 255"));
    }
    Ok(channels)
}

/// The `lines` of `editor_guides_set`, checked and written out in full.
fn guide_lines(value: &Value) -> Result<Value, String> {
    let lines = value.as_array().ok_or("lines must be an array")?;
    if lines.len() > EDITOR_MAX_GUIDES {
        return Err(format!("lines holds more than {EDITOR_MAX_GUIDES} guides"));
    }
    let mut checked = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let line = line.as_object().ok_or_else(|| format!("lines[{index}] must be an object"))?;
        if let Some(key) = line.keys().find(|key| !["axis", "position", "colour", "memo"].contains(&key.as_str())) {
            return Err(format!("unknown key in lines[{index}]: {}", echoed([key.as_str()])));
        }
        let axis = line.get("axis").and_then(Value::as_str).filter(|axis| ["horizontal", "vertical"].contains(axis));
        let axis = axis.ok_or_else(|| format!("lines[{index}].axis must be horizontal or vertical"))?;
        let position = line.get("position").and_then(Value::as_f64);
        let position = position
            .filter(|position| position.abs() <= EDITOR_MAX_COORDINATE)
            .ok_or_else(|| format!("lines[{index}].position must be a number within +/-{EDITOR_MAX_COORDINATE:e}"))?;
        let mut guide = json!({"axis": axis, "position": position});
        if let Some(colour) = line.get("colour") {
            guide["colour"] = json!(rgba(colour, &format!("lines[{index}].colour"))?);
        }
        if let Some(memo) = line.get("memo") {
            let memo = memo.as_str().filter(|memo| memo.chars().count() <= EDITOR_MAX_MEMO);
            let memo =
                memo.ok_or_else(|| format!("lines[{index}].memo must be a string of at most {EDITOR_MAX_MEMO} characters"))?;
            guide["memo"] = json!(memo);
        }
        checked.push(guide);
    }
    Ok(Value::Array(checked))
}

fn editor_point(value: &Value, what: &str) -> Result<Value, String> {
    let numbers: Vec<f64> = value.as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
    let bounded = numbers.iter().all(|n| n.is_finite() && n.abs() <= EDITOR_MAX_COORDINATE);
    if value.as_array().map(Vec::len) != Some(2) || numbers.len() != 2 || !bounded {
        return Err(format!("{what} must be [x, y], finite and within +/-{EDITOR_MAX_COORDINATE:e}"));
    }
    Ok(json!(numbers))
}

fn editor_tool_request(name: &str, arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("tool arguments must be an object")?;
    let id = |value: &Value, what: &str| {
        value.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or_else(|| format!("{what} must be an integer UUID"))
    };
    let nodes = |value: &Value| -> Result<Vec<u32>, String> {
        let list = value.as_array().ok_or("nodes must be an array of node UUIDs")?;
        if list.len() > EDITOR_MAX_NODES {
            return Err(format!("nodes holds more than {EDITOR_MAX_NODES} entries"));
        }
        let mut ids = Vec::new();
        for item in list {
            let node = id(item, "nodes entries")?;
            if !ids.contains(&node) {
                ids.push(node);
            }
        }
        Ok(ids)
    };
    Ok(match name {
        "editor_quit" => json!({"op":"editor.quit"}),
        "editor_selection_set" => {
            let mut call = json!({"op": "editor.selection.set"});
            if let Some(list) = args.get("nodes") {
                call["nodes"] = json!(nodes(list)?);
            }
            if let Some(list) = args.get("bindings") {
                let list = list.as_array().ok_or("bindings must be an array of {node, channel}")?;
                if list.len() > EDITOR_MAX_NODES {
                    return Err(format!("bindings holds more than {EDITOR_MAX_NODES} entries"));
                }
                let mut bindings = Vec::new();
                for item in list {
                    let (node, channel) = (item.get("node").and_then(Value::as_u64), item.get("channel").and_then(Value::as_str));
                    let (Some(node), Some(channel)) = (node, channel) else {
                        return Err("bindings entries need node and channel".into());
                    };
                    bindings.push(json!({"node": id(&json!(node), "bindings entries' node")?, "channel": channel}));
                }
                call["bindings"] = json!(bindings);
            }
            if let Some(list) = args.get("vertices") {
                let list = list.as_array().ok_or("vertices must be an array of vertex indices")?;
                if list.len() > EDITOR_MAX_VERTICES {
                    return Err(format!("vertices holds more than {EDITOR_MAX_VERTICES} entries"));
                }
                if !list.iter().all(Value::is_u64) {
                    return Err("vertices entries must be non-negative integers".into());
                }
                call["vertices"] = json!(list);
            }
            if call.as_object().is_some_and(|fields| fields.len() == 1) {
                return Err("give at least one of nodes, bindings or vertices".into());
            }
            call
        }
        "editor_armed_set" => match args.get("parameter").ok_or("parameter is required (null disarms)")? {
            Value::Null => json!({"op": "editor.armed.set", "parameter": null}),
            value => json!({"op": "editor.armed.set", "parameter": id(value, "parameter")?}),
        },
        "editor_camera_set" => {
            let mut call = json!({"op": "editor.camera.set"});
            if let Some(centre) = args.get("centre") {
                call["centre"] = editor_point(centre, "centre")?;
            }
            if let Some(zoom) = args.get("zoom") {
                call["zoom"] = json!(zoom.as_f64().filter(|zoom| zoom.is_finite()).ok_or("zoom must be a finite number")?);
            }
            if let Some(mirror) = args.get("mirror") {
                call["mirror"] = json!(mirror.as_bool().ok_or("mirror must be a boolean")?);
            }
            if call.get("centre").is_none() && call.get("zoom").is_none() && call.get("mirror").is_none() {
                return Err("give centre, zoom or mirror".into());
            }
            call
        }
        "editor_camera_sync" => json!({"op": "editor.camera.sync"}),
        "editor_native_parameters_set" => json!({"op":"editor.native_parameters.set",
            "filter":args["filter"].as_str().ok_or("filter must be a string")?}),
        "editor_parameters_set" => {
            let mut call = json!({"op":"editor.parameters.set"});
            for key in ["filter", "animation"] {
                if let Some(value) = args.get(key) {
                    call[key] = json!(value.as_str().ok_or_else(|| format!("{key} must be a string"))?);
                }
            }
            if let Some(bookmark) = args.get("bookmark") {
                let shape = "bookmark must be null, {labelIndex}, or {part, point}";
                call["bookmark"] = if bookmark.is_null() {
                    Value::Null
                } else {
                    let selector = bookmark.as_object().ok_or(shape)?;
                    if selector.len() == 1 && selector.contains_key("labelIndex") {
                        let index = selector["labelIndex"].as_u64().ok_or("bookmark.labelIndex must be a non-negative integer")?;
                        json!({"labelIndex":index})
                    } else if selector.len() == 2 && selector.contains_key("part") && selector.contains_key("point") {
                        json!({"part":id(&selector["part"], "bookmark.part")?,
                            "point":editor_point(&selector["point"], "bookmark.point")?})
                    } else {
                        return Err(shape.into());
                    }
                };
            }
            if call.as_object().unwrap().len() == 1 {
                return Err("give filter, animation or bookmark".into());
            }
            call
        }
        "editor_camera_fit" => match args.get("nodes") {
            Some(value) => {
                let ids = nodes(value)?;
                if ids.is_empty() {
                    return Err("nodes must not be empty; leave it out to fit the whole model".into());
                }
                json!({"op": "editor.camera.fit", "nodes": ids})
            }
            None => json!({"op": "editor.camera.fit"}),
        },
        "editor_recovery_list" => json!({"op": "editor.recovery.list"}),
        "editor_recovery_keep" => json!({"op": "editor.recovery.keep"}),
        "editor_recovery_restore" => {
            let id = args.get("id").and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= 255);
            let id = id.ok_or("id must be a recovery snapshot id (editor_recovery_list)")?;
            json!({"op": "editor.recovery.restore", "id": id})
        }
        "editor_settings_get" => json!({"op": "editor.settings.get"}),
        "editor_settings_set" => {
            let key = args.get("key").and_then(Value::as_str).filter(|key| !key.is_empty() && key.len() <= 64);
            let key = key.ok_or("key must be a setting key (editor_settings_get)")?;
            json!({"op": "editor.settings.set", "key": key, "value": args["value"]})
        }
        "editor_shortcut_set" => {
            let command = args.get("command").and_then(Value::as_str).filter(|command| !command.is_empty() && command.len() <= 64);
            let command = command.ok_or("command must be a command id (editor_settings_get lists them under shortcuts)")?;
            let shortcut = &args["shortcut"]; // required: the declared-argument check refuses a call without it
            if !(shortcut.is_null() || shortcut.as_str().is_some_and(|text| !text.is_empty() && text.len() <= 64)) {
                return Err("shortcut must be a chord string such as Ctrl+Shift+S, or null to unassign".into());
            }
            let mut call = json!({"op": "editor.shortcut.set", "command": command, "shortcut": shortcut});
            if let Some(repeat) = args.get("repeat") {
                call["repeat"] = json!(repeat.as_bool().ok_or("repeat must be a boolean")?);
            }
            call
        }
        "editor_about_set" => json!({"op":"editor.about.set", "open":args["open"].as_bool().ok_or("open must be a boolean")?}),
        "editor_welcome_set" => json!({"op":"editor.welcome.set", "open":args["open"].as_bool().ok_or("open must be a boolean")?}),
        "editor_command_browser_set" => json!({"op":"editor.command.browser.set", "open":args["open"].as_bool().ok_or("open must be a boolean")?}),
        "editor_texture_viewer_set" => json!({"op":"editor.texture.viewer.set", "open":args["open"].as_bool().ok_or("open must be a boolean")?}),
        "editor_address_dialog" => {
            let action = args["action"].as_str().ok_or("action must be open, copy or close")?;
            let target = args.get("target").map(|value| value.as_str().ok_or("target must be a string")).transpose()?;
            crate::window_shell::AddressDialogOperation::from_fields(action, target)?;
            let mut call = json!(args);
            call["op"] = json!("editor.address.dialog");
            call
        }
        "editor_viewport_set" => {
            let mut call = json!({"op": "editor.viewport.set"});
            if let Some(sort) = args.get("partSort") {
                call["partSortBySize"] = json!(match sort.as_str() {
                    Some("z_sort") => false,
                    Some("size_sort") => true,
                    _ => return Err("partSort must be z_sort or size_sort".into()),
                });
            }
            for key in ["vertices", "bounds", "orientation"] {
                if let Some(value) = args.get(key) {
                    call[key] = json!(value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?);
                }
            }
            if let Some(background) = args.get("background") {
                let channels: Option<Vec<u8>> = background.as_array().filter(|v| v.len() == 3).and_then(|v| {
                    v.iter().map(|channel| channel.as_u64().and_then(|v| u8::try_from(v).ok())).collect()
                });
                call["background"] = json!(channels.ok_or("background must be [r, g, b], each an integer from 0 to 255")?);
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give vertices, bounds, orientation, background or partSort".into());
            }
            call
        }
        "editor_grid_set" => {
            let mut call = json!({"op": "editor.grid.set"});
            if let Some(show) = args.get("show") {
                call["show"] = json!(show.as_bool().ok_or("show must be a boolean")?);
            }
            if let Some(spacing) = args.get("spacing") {
                // `as_f64` has no answer for a literal too large for an f64.
                call["spacing"] = json!(spacing.as_f64().ok_or("spacing must be a finite number")?);
            }
            if let Some(colour) = args.get("colour") {
                call["colour"] = json!(rgba(colour, "colour")?);
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give show, spacing or colour".into());
            }
            call
        }
        "editor_layout_reset" => json!({"op":"editor.layout.reset"}),
        "editor_panel_set" => {
            let panel = args["panel"].as_str().filter(|panel| matches!(*panel, "outline" | "timeline"))
                .ok_or("panel must be outline or timeline")?;
            let visible = args["visible"].as_bool().ok_or("visible must be a boolean")?;
            json!({"op":"editor.panel.set", "panel":panel, "visible":visible})
        }
        "editor_workspace_save" | "editor_workspace_apply" | "editor_workspace_delete" => {
            let workspace = args.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
            if workspace.is_empty()
                || workspace.chars().count() > EDITOR_MAX_WORKSPACE_NAME
                || workspace.chars().any(char::is_control)
            {
                return Err(format!("name must be a non-empty string of at most {EDITOR_MAX_WORKSPACE_NAME} characters \
                    without control characters"));
            }
            let op = format!("editor.workspace.{}", name.trim_start_matches("editor_workspace_"));
            json!({"op": op, "name": workspace})
        }
        "editor_solo_set" => {
            let mut call = json!({"op": "editor.solo.set"});
            if let Some(show) = args.get("show") {
                call["show"] = json!(show.as_bool().ok_or("show must be a boolean")?);
            }
            if let Some(colour) = args.get("colour") {
                call["colour"] = json!(rgba(colour, "colour")?);
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give show or colour".into());
            }
            call
        }
        "editor_onion_set" => {
            let mut call = json!({"op": "editor.onion.set"});
            for key in ["onion", "trajectories"] {
                if let Some(value) = args.get(key) {
                    call[key] = json!(value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?);
                }
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give onion or trajectories".into());
            }
            call
        }
        "editor_playback_set" => {
            let mut call = json!({"op": "editor.playback.set"});
            for key in ["lightweight", "snapToFramerate"] {
                if let Some(value) = args.get(key) {
                    call[key] = json!(value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?);
                }
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give lightweight or snapToFramerate".into());
            }
            call
        }
        "editor_timeline_zoom_set" => {
            let zoom = number_arg(args, "zoom")?;
            json!({"op":"editor.timeline.zoom.set","zoom":zoom})
        }
        "editor_physics_preview_set" => {
            let enabled = args["enabled"].as_bool().ok_or("enabled must be a boolean")?;
            json!({"op":"editor.physics.preview.set","enabled":enabled})
        }
        "editor_guides_set" => {
            let mut call = json!({"op": "editor.guides.set"});
            for key in ["open", "show", "snap"] {
                if let Some(value) = args.get(key) {
                    call[key] = json!(value.as_bool().ok_or_else(|| format!("{key} must be a boolean"))?);
                }
            }
            if let Some(colour) = args.get("colour") {
                call["colour"] = json!(rgba(colour, "colour")?);
            }
            if let Some(lines) = args.get("lines") {
                call["lines"] = guide_lines(lines)?;
            }
            if call.as_object().is_some_and(|call| call.len() == 1) {
                return Err("give open, show, snap, colour or lines".into());
            }
            call
        }
        _ => {
            let tool = args.get("tool").and_then(Value::as_str).filter(|tool| !tool.is_empty() && tool.len() <= 64);
            json!({"op": "editor.tool.set", "tool": tool.ok_or("tool must be a tool id (tool.offered in the editor state)")?})
        }
    })
}

/// The JSON-RPC answer to a message `editor_request` recognised, in the shapes `handle_mcp` gives a
/// resource read and a tool call. `None` for a notification.
pub(crate) fn editor_reply(request: &Value, outcome: Result<Value, String>) -> Option<Value> {
    let id = request.get("id").filter(|id| !id.is_null())?.clone();
    let result = if request["method"] == "resources/read" {
        match outcome {
            Ok(state) => json!({ "contents": [{ "uri": EDITOR_STATE_URI, "mimeType": "application/json", "text": state.to_string() }] }),
            Err(message) => return Some(rpc_error(id, -32603, &message)),
        }
    } else {
        match outcome {
            Ok(result) => tool_reply(result),
            Err(message) => tool_error(&message),
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// The editor state resource: the document facts the Session holds, plus `window` (the fields only an editor
/// window has) when the host is one. `editor` says which, so a missing key never reads as an empty value.
pub(crate) fn editor_state(session: &Session, window: Option<Value>) -> Value {
    let mut state = json!({
        "editor": window.is_some(),
        "document": {
            "path": session.file_path.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "format": session.document.format(),
            "modified": session.is_modified(),
            "revision": session.revision,
        },
    });
    if let (Some(state), Some(Value::Object(window))) = (state.as_object_mut(), window) {
        state.extend(window);
    }
    state
}

fn call_tool(
    session: &mut Session,
    params: &Value,
    guard: &mut impl FnMut(&mut Session, &Value) -> Result<Option<crate::export_ops::PreparedExport>, String>,
) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "tools/call requires a tool name".into()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if name == crate::mcp_aliases::SCOPE_TOOL {
        return Ok(tool_reply(crate::mcp_aliases::scope_response(&tools())));
    }
    if name == "ViewCommand_CaptureLiveScreenshot" {
        return capture_live_screenshot(session, &arguments, guard);
    }
    if let Err(message) = check_declared_arguments(name, &arguments) {
        return Ok(tool_error(&message));
    }
    let request = match tool_request(name, &arguments) {
        Ok(request) => request,
        Err(message) => return Ok(tool_error(&message)),
    };
    let prepared = match guard(session, &request) {
        Ok(prepared) => prepared,
        Err(message) => return Ok(tool_error(&message)),
    };
    let response = session.handle_prepared_request(&request, prepared);
    let succeeded = response
        .get("succeeded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !succeeded {
        let message = response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("tool failed");
        return Ok(tool_error(message));
    }
    let result = response.get("result").cloned().unwrap_or(Value::Null);
    if let Some(encoded) = result.get("png").and_then(Value::as_str) {
        let image = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| (-32603, format!("invalid PNG result: {error}")))?;
        let mut content = vec![json!({
            "type": "image",
            "mimeType": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(image),
        })];
        let metadata = result
            .as_object()
            .map(|object| {
                let mut metadata = object.clone();
                metadata.remove("png");
                Value::Object(metadata)
            })
            .unwrap_or(Value::Null);
        content.push(json!({ "type": "text", "text": metadata.to_string() }));
        return Ok(with_structured(json!({ "isError": false, "content": content }), metadata));
    }
    Ok(tool_reply(result))
}

/// A successful `tools/call` answer: the result as JSON text, and as `structuredContent` when it is an object.
/// MCP allows only an object there, and a client that validates (Claude Code does) refuses the whole answer
/// for an array or `null`, so a list answer such as `nodes_list` is read from `content[0].text`.
pub(crate) fn tool_reply(result: Value) -> Value {
    let reply = json!({ "isError": false, "content": [{ "type": "text", "text": result.to_string() }] });
    with_structured(reply, result)
}

fn with_structured(mut reply: Value, result: Value) -> Value {
    if result.is_object() {
        reply["structuredContent"] = result;
    }
    reply
}

const CAPTURE_WIDTH: u64 = 640;
const CAPTURE_HEIGHT: u64 = 480;

/// World-space capture metadata of a render of `bounds` (`[min x, min y, max x, max y]`) into `width` by
/// `height` pixels, in the shape D's `buildScreenshotWorldCaptureMeta` returned. The renderer does not flip
/// y, so pixel row 0 is the bounds' minimum y; matrices are row-major 4x4 with the translation in column 3.
pub(crate) fn world_capture(width: u32, height: u32, bounds: [f64; 4]) -> Value {
    let (w, h) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
    let (pw, ph) = (f64::from(width), f64::from(height));
    let matrix = |rows: [[f64; 4]; 3]| json!([rows[0], rows[1], rows[2], [0.0, 0.0, 0.0, 1.0]]);
    json!({
        "coordinateSystem": "soul2d-world",
        "pixelOrigin": "top-left",
        "width": width,
        "height": height,
        "corners": {
            "topLeft": [bounds[0], bounds[1]],
            "topRight": [bounds[2], bounds[1]],
            "bottomRight": [bounds[2], bounds[3]],
            "bottomLeft": [bounds[0], bounds[3]],
        },
        "aabb": {"min": [bounds[0], bounds[1]], "max": [bounds[2], bounds[3]]},
        "worldToClipMatrix": matrix([
            [2.0 / w, 0.0, 0.0, -2.0 * bounds[0] / w - 1.0],
            [0.0, -2.0 / h, 0.0, 1.0 + 2.0 * bounds[1] / h],
            [0.0, 0.0, 1.0, 0.0],
        ]),
        "clipToWorldMatrix": matrix([
            [w / 2.0, 0.0, 0.0, bounds[0] + w / 2.0],
            [0.0, -h / 2.0, 0.0, bounds[1] + h / 2.0],
            [0.0, 0.0, 1.0, 0.0],
        ]),
        "worldToImageMatrix": matrix([
            [pw / w, 0.0, 0.0, -bounds[0] * pw / w],
            [0.0, ph / h, 0.0, -bounds[1] * ph / h],
            [0.0, 0.0, 1.0, 0.0],
        ]),
    })
}

/// `ViewCommand_CaptureLiveScreenshot` as D's Rust-core mode served it: 640x480 and the scene bounds when
/// the caller gives none, `context.parameterValue` for one `context.parameters` UUID, and an image result
/// whose `_meta` carries `worldCapture`. The parameter value is a transient override of the render (nothing
/// in the session is set), so there is no value to restore afterwards. `overlayObjects` needs a rasteriser
/// for outlines and is refused, as D's Rust-core mode refused it.
fn capture_live_screenshot(
    session: &mut Session,
    arguments: &Value,
    guard: &mut impl FnMut(&mut Session, &Value) -> Result<Option<crate::export_ops::PreparedExport>, String>,
) -> Result<Value, (i64, String)> {
    let outcome = (|| -> Result<Value, String> {
        if arguments.get("overlayObjects").and_then(Value::as_array).is_some_and(|list| !list.is_empty()) {
            return Err("overlayObjects are not supported in Rust Core mode".into());
        }
        let size = |key: &str, default: u64| match arguments.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(value) => value.as_u64().filter(|v| *v > 0).ok_or_else(|| format!("{key} must be a positive integer")),
        };
        let (width, height) = (size("width", CAPTURE_WIDTH)?, size("height", CAPTURE_HEIGHT)?);
        let mut values = arguments.get("values").cloned();
        if let Some(context) = arguments.get("context").filter(|context| context.get("parameterValue").is_some()) {
            let ids = context
                .get("parameters")
                .and_then(Value::as_array)
                .filter(|ids| ids.len() == 1)
                .ok_or("screenshot requires one parameter UUID")?;
            let id = ids[0].as_u64().ok_or("context.parameters must contain integer UUIDs")?;
            let pair = context["parameterValue"]
                .as_array()
                .filter(|pair| pair.len() == 2 && pair.iter().all(|n| n.as_f64().is_some_and(f64::is_finite)))
                .ok_or("context.parameterValue must be two finite numbers")?;
            values = Some(json!({ id.to_string(): pair }));
        }
        let mut request = json!({"op": "render", "width": width, "height": height});
        if let Some(values) = &values {
            request["values"] = values.clone();
        }
        if let Some(path) = arguments.get("path") {
            request["path"] = path.clone();
        }
        request["bounds"] = match arguments.get("bounds") {
            Some(bounds) => bounds.clone(),
            None => {
                let mut ask = json!({"op": "bounds"});
                if let Some(values) = &values {
                    ask["values"] = values.clone();
                }
                let found = session.handle_request(&ask);
                if found["succeeded"] != true {
                    return Err(found["message"].as_str().unwrap_or("bounds failed").to_owned());
                }
                found["result"]["bounds"].clone()
            }
        };
        let _ = guard(session, &request)?;
        let response = session.handle_request(&request);
        if response["succeeded"] != true {
            return Err(response["message"].as_str().unwrap_or("tool failed").to_owned());
        }
        let mut result = response["result"].clone();
        let bounds: [f64; 4] = serde_json::from_value(request["bounds"].clone()).map_err(|_| "bounds must be four numbers")?;
        result["bounds"] = request["bounds"].clone();
        result["worldCapture"] = world_capture(result["width"].as_u64().unwrap_or(width) as u32, result["height"].as_u64().unwrap_or(height) as u32, bounds);
        Ok(result)
    })();
    let result = match outcome {
        Ok(result) => result,
        Err(message) => return Ok(tool_error(&message)),
    };
    let encoded = result.get("png").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut metadata = result.as_object().cloned().unwrap_or_default();
    metadata.remove("png");
    let meta = json!({
        "width": result["width"], "height": result["height"],
        "bounds": result["bounds"], "worldCapture": result["worldCapture"],
    });
    let metadata = Value::Object(metadata);
    let reply = json!({
        "isError": false,
        "content": [
            {"type": "image", "mimeType": "image/png", "data": encoded},
            {"type": "text", "text": metadata.to_string()},
        ],
        "_meta": meta,
    });
    Ok(with_structured(reply, metadata))
}

fn take_context_nodes(args: &mut Map<String, Value>, request: &mut Map<String, Value>) {
    if request.contains_key("parents") || request.contains_key("parent") {
        return;
    }
    let Some(context) = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
    else {
        return;
    };
    if let Some(nodes) = context.get("nodes") {
        request.insert("parents".into(), nodes.clone());
    }
}

fn take_context_rename(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if request.contains_key("id") {
        return Ok(());
    }
    let context = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "context.nodes is required".to_string())?;
    let nodes = context
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| nodes.len() == 1)
        .ok_or_else(|| "SetNodeName requires exactly one context.nodes UUID".to_string())?;
    let names = args
        .remove("newNames")
        .and_then(|value| value.as_array().cloned())
        .filter(|names| names.len() == 1)
        .ok_or_else(|| "newNames must contain one name".to_string())?;
    request.insert("id".into(), nodes[0].clone());
    request.insert("name".into(), names[0].clone());
    Ok(())
}

fn take_context_node_ids(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if request.contains_key("id") || request.contains_key("ids") || request.contains_key("nodes") {
        return Ok(());
    }
    let context = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "context.nodes is required".to_string())?;
    let nodes = context
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
        .ok_or_else(|| "context.nodes must contain at least one UUID".to_string())?;
    request.insert("nodes".into(), Value::Array(nodes.clone()));
    Ok(())
}

fn take_context_node_first(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    let context = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "context.nodes is required".to_string())?;
    let nodes = context
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
        .ok_or_else(|| "context.nodes must contain at least one UUID".to_string())?;
    request.insert("id".into(), nodes[0].clone());
    Ok(())
}

fn take_binding_endpoint(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
    require_bone: bool,
) -> Result<(), String> {
    for key in ["root", "target", "bone"] {
        if let Some(value) = args.remove(key) {
            request.insert(key.into(), value);
        }
    }
    args.remove("context");
    if !request.contains_key("root") || !request.contains_key("target") {
        return Err("root and target are required".into());
    }
    if require_bone && !request.contains_key("bone") {
        return Err("bone is required".into());
    }
    Ok(())
}

fn take_deform_command(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    for key in ["root", "targets", "parameter"] {
        if let Some(value) = args.remove(key) {
            request.insert(key.into(), value);
        }
    }
    if let Some(context) = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
    {
        if !request.contains_key("parameter") {
            let armed = context.get("armedParameters").and_then(Value::as_array);
            let parameters = context.get("parameters").and_then(Value::as_array);
            let chosen = armed.filter(|ids| !ids.is_empty()).or(parameters);
            if let Some(id) = chosen.and_then(|ids| ids.first()) {
                request.insert("parameter".into(), id.clone());
            }
        }
    }
    if !request.contains_key("root") {
        return Err("root is required".into());
    }
    Ok(())
}

fn take_rig_root(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if let Some(root) = args.remove("root") {
        request.insert("root".into(), root);
    } else if let Some(first) = args
        .get("context")
        .and_then(|value| value.as_object())
        .and_then(|context| context.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .cloned()
    {
        request.insert("root".into(), first);
    }
    args.remove("context");
    if !request.contains_key("root") {
        return Err("root is required".into());
    }
    Ok(())
}

fn take_depth_target(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if request.contains_key("target") || request.contains_key("id") || request.contains_key("nodes")
    {
        return Ok(());
    }
    if let Some(target) = args.remove("target") {
        request.insert("target".into(), target);
        return Ok(());
    }
    if args.get("context").is_some() {
        take_context_node_ids(args, request)?;
    }
    Ok(())
}

fn take_context_inspector(
    args: &mut Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), String> {
    if request.contains_key("id") {
        return Ok(());
    }
    let context = args
        .remove("context")
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "context.nodes is required".to_string())?;
    let nodes = context
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| nodes.len() == 1)
        .ok_or_else(|| "Inspector apply requires exactly one context.nodes UUID".to_string())?;
    request.insert("id".into(), nodes[0].clone());
    Ok(())
}

/// Refuse a name the tool list does not declare, whatever `tool_request` would make of it; a call that leaves
/// out an argument the tool's schema lists in `required` (a `null` value counts as given); and arguments the
/// schema does not declare when it says `additionalProperties: false`.
/// Tools whose schema allows additional properties (the default of `schema()`) are otherwise forwarded unchanged,
/// except an undeclared key starting with `_`, the prefix of request keys such as `_suffix` that only the tools
/// declaring them may pass.
fn check_declared_arguments(name: &str, arguments: &Value) -> Result<(), String> {
    static TOOLS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    let tools = TOOLS.get_or_init(tools);
    let Some(input_schema) = tools.iter().find(|tool| tool["name"] == name).map(|tool| &tool["inputSchema"]) else {
        return Err(format!("unknown tool: {}", echoed([name])));
    };
    let missing: Vec<&str> = input_schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| arguments.get(key).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing required argument(s) for {name}: {}", echoed(missing)));
    }
    let Some(arguments) = arguments.as_object() else {
        return Ok(());
    };
    let closed = input_schema["additionalProperties"] == false;
    let unknown: Vec<&str> = arguments
        .keys()
        .map(String::as_str)
        .filter(|key| input_schema["properties"].get(key).is_none() && (closed || key.starts_with('_')))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!("unknown argument(s) for {name}: {}", echoed(unknown)))
}

/// Names or keys as a refusal repeats them: the caller wrote them, so at most 8, of at most 64 characters each.
pub(crate) fn echoed<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names: Vec<String> = names.into_iter().take(8).map(|name| name.chars().take(64).collect()).collect();
    names.join(", ")
}

fn tool_request(name: &str, arguments: &Value) -> Result<Value, String> {
    let mut args = arguments
        .as_object()
        .cloned()
        .ok_or("tool arguments must be an object")?;
    args.remove("op");
    if let Some(aliased) = crate::mcp_aliases::request(name, &args) {
        return aliased;
    }
    let mut request = Map::new();
    let op = match name {
        "ParamCommand_Add1DParameter" => {
            let min = number_arg(&args, "min")?;
            let max = number_arg(&args, "max")?;
            args.remove("min");
            args.remove("max");
            request.insert("dimensions".into(), json!(1));
            request.insert("min".into(), json!([min, 0.0]));
            request.insert("max".into(), json!([max, 1.0]));
            "parameter.create"
        }
        "ParamCommand_Add2DParameter" => {
            let min = number_arg(&args, "min")?;
            let max = number_arg(&args, "max")?;
            args.remove("min");
            args.remove("max");
            request.insert("dimensions".into(), json!(2));
            request.insert("min".into(), json!([min, min]));
            request.insert("max".into(), json!([max, max]));
            "parameter.create"
        }
        "ParamPropCommand_SetParameterName" => "parameter.rename",
        "EditCommand_Undo" => "undo",
        "EditCommand_Redo" => "redo",
        "ModelCommand_SetTRSBinding" => {
            if let Some(value) = args.remove("rotationDegrees") {
                let apply = args
                    .remove("applyRotation")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if apply || value.as_f64().is_some_and(|value| value != 0.0) {
                    request.insert("rotation_degrees".into(), value);
                }
            } else if args
                .remove("applyRotation")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                request.insert("rotation_degrees".into(), json!(0.0));
            }
            for key in [
                "parameter",
                "nodes",
                "key",
                "translation",
                "scale",
                "translation_z",
            ] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            "binding.set_trs"
        }
        "ModelCommand_SetRotationXBinding" | "ModelCommand_SetRotationYBinding" => {
            let field = if name.ends_with("XBinding") {
                "rotation_x"
            } else {
                "rotation_y"
            };
            if let Some(value) = args.remove("rotationRadians") {
                request.insert(field.into(), value);
            }
            for key in ["parameter", "nodes", "key"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            "binding.set_trs"
        }
        "ViewCommand_CaptureLiveScreenshot" => "render",
        "NodeCommand_AddNode" => {
            for key in ["className", "_suffix", "parents", "parent", "name"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            take_context_nodes(&mut args, &mut request);
            "node.create"
        }
        "DepthBoneCommand_CreateDepthRigRoot" => {
            request.insert("className".into(), json!("DepthRigRoot"));
            let name = args
                .remove("name")
                .and_then(|value| value.as_str().map(str::to_owned))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "DepthRig".to_owned());
            request.insert("name".into(), Value::String(name));
            if let Some(parent) = args.remove("parent") {
                request.insert("parents".into(), json!([parent]));
            } else if let Some(first) = args
                .remove("context")
                .and_then(|value| value.as_object().cloned())
                .as_ref()
                .and_then(|context| context.get("nodes"))
                .and_then(Value::as_array)
                .and_then(|nodes| nodes.first())
                .cloned()
            {
                request.insert("parents".into(), json!([first]));
            }
            "node.create"
        }
        "DepthBoneCommand_AddDepthBone" => {
            request.insert("className".into(), json!("DepthBone"));
            let bone_id = args.remove("boneId").unwrap_or_else(|| json!(""));
            request.insert("name".into(), args.remove("name").unwrap_or_else(|| bone_id.clone()));
            request.insert("boneId".into(), bone_id);
            for key in ["restHead", "restTail", "restRoll"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            if let Some(parent) = args.remove("parent") {
                request.insert("parents".into(), json!([parent]));
            }
            take_context_nodes(&mut args, &mut request);
            if !request.contains_key("parents") && !request.contains_key("parent") {
                return Err("parent is required".into());
            }
            if !request.contains_key("restHead") || !request.contains_key("restTail") {
                return Err("restHead and restTail must be [x, y, z]".into());
            }
            "node.create"
        }
        "DepthBoneCommand_AddStandardDepthParameters" => {
            take_rig_root(&mut args, &mut request)?;
            "rig.add_standard_parameters"
        }
        "DepthBoneCommand_AddStandardDepthSkeleton" => {
            take_rig_root(&mut args, &mut request)?;
            if let Some(scale) = args.remove("scale") {
                request.insert("scale".into(), scale);
            }
            "rig.add_skeleton"
        }
        "DepthBoneCommand_ListDepthBones" => {
            take_rig_root(&mut args, &mut request)?;
            "rig.list_bones"
        }
        "DepthBoneCommand_AddDepthBoneSource"
        | "DepthBoneCommand_RemoveDepthBoneSource"
        | "DepthBoneCommand_SetDepthBoneSourceSettings"
        | "DepthBoneCommand_PreviewDepthBoneInfluence" => {
            take_binding_endpoint(&mut args, &mut request, true)?;
            if let Some(value) = args.remove("settings") {
                request.insert("settings".into(), value);
            }
            match name {
                "DepthBoneCommand_AddDepthBoneSource" => "rig.add_source",
                "DepthBoneCommand_RemoveDepthBoneSource" => "rig.remove_source",
                "DepthBoneCommand_SetDepthBoneSourceSettings" => "rig.set_source_settings",
                _ => "rig.preview_influence",
            }
        }
        "DepthBoneCommand_ListDepthBoneSources" | "DepthBoneCommand_GetDepthBoneInfluenceRule" => {
            take_binding_endpoint(&mut args, &mut request, false)?;
            if name.ends_with("ListDepthBoneSources") {
                "rig.list_sources"
            } else {
                "rig.get_influence_rule"
            }
        }
        "DepthBoneCommand_FitDepthRigRootZToDepth" => {
            take_rig_root(&mut args, &mut request)?;
            "rig.fit_root_z"
        }
        "DepthBoneCommand_SetDepthBoneRest" => {
            if let Some(bone) = args.remove("bone") {
                request.insert("bone".into(), bone);
            }
            for key in ["restHead", "restTail", "restRoll"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            args.remove("context");
            "rig.set_rest"
        }
        "DepthBoneCommand_SetDepthBoneConstraint" => {
            if let Some(bone) = args.remove("bone") {
                request.insert("bone".into(), bone);
            }
            if let Some(value) = args.remove("constraint") {
                request.insert("constraint".into(), value);
            }
            args.remove("context");
            "rig.set_constraint"
        }
        "DepthBoneCommand_FitDepthBoneZToDepth" => {
            if let Some(bone) = args.remove("bone") {
                request.insert("bone".into(), bone);
            } else if let Some(first) = args
                .get("context")
                .and_then(|value| value.as_object())
                .and_then(|context| context.get("nodes"))
                .and_then(Value::as_array)
                .and_then(|nodes| nodes.first())
                .cloned()
            {
                request.insert("bone".into(), first);
            }
            args.remove("context");
            if !request.contains_key("bone") {
                return Err("bone is required".into());
            }
            "rig.fit_bone_z"
        }
        "DepthBoneCommand_PreviewDepthBoneDeform" | "DepthBoneCommand_ApplyDepthBoneDeform" => {
            take_deform_command(&mut args, &mut request)?;
            if name.ends_with("ApplyDepthBoneDeform") {
                "rig.apply_deform"
            } else {
                "rig.preview_deform"
            }
        }
        "DepthBoneCommand_SetDepthBoneInfluenceRule" => {
            take_binding_endpoint(&mut args, &mut request, false)?;
            if let Some(value) = args.remove("rule") {
                request.insert("rule".into(), value);
            }
            "rig.set_influence_rule"
        }
        "NodeCommand_SetNodeName" => {
            if let Some(value) = args.remove("id") {
                request.insert("id".into(), value);
            }
            if let Some(value) = args.remove("name") {
                request.insert("name".into(), value);
            }
            take_context_rename(&mut args, &mut request)?;
            "node.rename"
        }
        "NodeCommand_MoveNode" => {
            for key in ["parent", "newParent", "index"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            take_context_node_ids(&mut args, &mut request)?;
            "node.move"
        }
        "NodeCommand_DeleteNode" => {
            take_context_node_ids(&mut args, &mut request)?;
            "node.delete"
        }
        "NodeCommand_CopyNode" => {
            take_context_node_ids(&mut args, &mut request)?;
            "node.copy"
        }
        "NodeCommand_CutNode" => {
            take_context_node_ids(&mut args, &mut request)?;
            "node.cut"
        }
        "NodeCommand_PasteNode" => {
            for key in ["parent", "newParent", "index"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            if !request.contains_key("parent") && !request.contains_key("newParent") {
                take_context_node_ids(&mut args, &mut request)?;
            }
            "node.paste"
        }
        "NodeCommand_ConvertTo" => {
            if let Some(value) = args.remove("className").or_else(|| args.remove("toType")) {
                request.insert("className".into(), value);
            }
            take_context_node_ids(&mut args, &mut request)?;
            "node.convert"
        }
        "NodeCommand_ToggleVisibility" => {
            if args.contains_key("id") {
                return Err("ToggleVisibility rejects an id argument".into());
            }
            take_context_node_first(&mut args, &mut request)?;
            "node.enabled.toggle"
        }
        other if other.starts_with("Node_ConvertTo_") => {
            let to_type = other.trim_start_matches("Node_ConvertTo_");
            request.insert("className".into(), Value::String(to_type.to_owned()));
            take_context_node_ids(&mut args, &mut request)?;
            "node.convert"
        }
        other if other.starts_with("Node_Add_") => {
            let class_name = other.trim_start_matches("Node_Add_");
            request.insert("className".into(), Value::String(class_name.to_owned()));
            if let Some(value) = args.remove("_suffix") {
                request.insert("_suffix".into(), value);
            }
            // `parents` or `parent` given by the caller goes to the core as it is and wins over `context`.
            if !args.contains_key("parents") && !args.contains_key("parent") {
                if args.get("context").is_some() {
                    take_context_node_ids(&mut args, &mut request)?;
                    if let Some(nodes) = request.remove("nodes") {
                        request.insert("parents".into(), nodes);
                    }
                }
            }
            "node.create"
        }
        other if other.starts_with("Node_Insert_") => {
            let class_name = other.trim_start_matches("Node_Insert_");
            request.insert("className".into(), Value::String(class_name.to_owned()));
            if let Some(value) = args.remove("_suffix") {
                request.insert("_suffix".into(), value);
            }
            take_context_node_ids(&mut args, &mut request)?;
            "node.insert"
        }
        "VertexCommand_DefineVertices" => {
            if let Some(value) = args.remove("vertices") {
                request.insert("vertices".into(), value);
            }
            take_context_node_ids(&mut args, &mut request)?;
            "deformer.define_vertices"
        }
        "VertexCommand_DefineGrid" => {
            for key in ["axisX", "axisY"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            take_context_node_ids(&mut args, &mut request)?;
            "deformer.define_grid"
        }
        "VertexCommand_DefineMesh" => {
            for key in ["vertices", "indices", "uvs", "origin"] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            take_context_node_ids(&mut args, &mut request)?;
            "deformer.define_mesh"
        }
        "Inspector_Apply_GridDeformDynamic"
        | "Inspector_Apply_MeshGroupDynamic"
        | "Inspector_Apply_MeshGroupTranslateChildren"
        | "Inspector_Apply_PathDeformDynamic"
        | "Inspector_Apply_PathDeformPhysicsEnabled"
        | "Inspector_Apply_PathDeformCurveType"
        | "Inspector_Apply_PathDeformGravity"
        | "Inspector_Apply_PathDeformRestoreConstant"
        | "Inspector_Apply_PathDeformDamping"
        | "Inspector_Apply_PathDeformInputScale"
        | "Inspector_Apply_PathDeformPropagateScale" => {
            let property = name.trim_start_matches("Inspector_Apply_");
            request.insert("property".into(), Value::String(property.to_owned()));
            if let Some(value) = args.remove("value") {
                request.insert("value".into(), value);
            }
            take_context_node_ids(&mut args, &mut request)?;
            "deformer.set"
        }
        "DepthMapCommand_ListDepths" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.list"
        }
        "DepthMapCommand_SetDepths" => {
            if let Some(value) = args.remove("depths") {
                request.insert("depths".into(), value);
            }
            take_depth_target(&mut args, &mut request)?;
            "depth.set"
        }
        "DepthMapCommand_ClearDepths" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.clear"
        }
        "DepthMapCommand_ListDepthOps" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.list"
        }
        "DepthMapCommand_SetDepthOps" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.set"
        }
        "DepthMapCommand_AddDepthOp" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.add"
        }
        "DepthMapCommand_UpdateDepthOp" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.update"
        }
        "DepthMapCommand_RemoveDepthOp" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.remove"
        }
        "DepthMapCommand_MoveDepthOp" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.move"
        }
        "DepthMapCommand_ClearDepthOps" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.clear"
        }
        "DepthMapCommand_ApplyDepthOps" => {
            take_depth_target(&mut args, &mut request)?;
            "depth.ops.apply"
        }
        "DepthMapCommand_ImportPSDDepths" => {
            if let Some(value) = args.remove("path") {
                request.insert("path".into(), value);
            }
            for key in [
                "invert",
                "backDepth",
                "frontDepth",
                "depthScale",
                "convolution",
                "channel",
                "customRadius",
                "alphaThreshold",
                "missingPolicy",
                "matchDirectGridName",
                "layers",
                "disabledGrids",
            ] {
                if let Some(value) = args.remove(key) {
                    request.insert(key.into(), value);
                }
            }
            "depth.import_psd"
        }
        other if inspector_transform_component(other).is_some() => {
            let component = inspector_transform_component(other).unwrap_or_default();
            map_inspector_component(&mut request, &mut args, component)?;
            take_context_inspector(&mut args, &mut request)?;
            "node.transform"
        }
        other if inspector_node_property(other).is_some() => {
            let property = inspector_node_property(other).unwrap_or_default();
            request.insert("property".into(), json!(property));
            if let Some(value) = args.remove("value") {
                request.insert("value".into(), value);
            }
            take_context_inspector(&mut args, &mut request)?;
            "node.property.set"
        }
        other if inspector_physics_key(other).is_some() => {
            let key = inspector_physics_key(other).unwrap_or_default();
            if let Some(value) = args.remove("value") {
                request.insert(key.into(), value);
            }
            take_context_node_ids(&mut args, &mut request)?;
            "physics.set"
        }
        _ => direct_operation(name).ok_or_else(|| format!("unknown tool: {}", echoed([name])))?,
    };
    // The match above has set what the tool fixes. A leftover argument under the same key would replace it, and
    // so would `id` or `ids` beside `nodes`, which the core reads first.
    for key in args.keys() {
        let set = if matches!(key.as_str(), "id" | "ids") && request.contains_key("nodes") { "nodes" } else { key };
        if request.contains_key(set) {
            return Err(format!("{name} sets {} itself: do not pass the argument {}", echoed([set]), echoed([key.as_str()])));
        }
    }
    request.extend(args);
    request.insert("op".into(), Value::String(op.to_owned()));
    Ok(Value::Object(request))
}

/// D `InspectorNodeApplyCommand` ids that `node.property.set` serves.
const INSPECTOR_NODE_PROPERTIES: &[(&str, &str)] = &[
    ("LockToRoot", "lockToRoot"),
    ("PinToMesh", "pinToMesh"),
    ("PartTint", "tint"),
    ("PartScreenTint", "screenTint"),
    ("PartEmissionStrength", "emissionStrength"),
    ("PartBlendingMode", "blend_mode"),
    ("PartOpacity", "opacity"),
    ("PartMaskAlphaThreshold", "mask_threshold"),
    ("PartAutoResizedMesh", "auto_resized"),
    ("CompositeTint", "tint"),
    ("CompositeScreenTint", "screenTint"),
    ("CompositeBlendingMode", "blend_mode"),
    ("CompositeOpacity", "opacity"),
    ("CompositeThreshold", "mask_threshold"),
    ("ViewportOrigin", "viewport"),
];

/// D `SimplePhysics*` inspector ids and the `physics.set` key each one writes.
const INSPECTOR_PHYSICS_KEYS: &[(&str, &str)] = &[
    ("SimplePhysicsModelType", "model_type"),
    ("SimplePhysicsMapMode", "map_mode"),
    ("SimplePhysicsLocalOnly", "local_only"),
    ("SimplePhysicsGravity", "gravity"),
    ("SimplePhysicsLength", "length"),
    ("SimplePhysicsFrequency", "frequency"),
    ("SimplePhysicsAngleDamping", "angle_damping"),
    ("SimplePhysicsLengthDamping", "length_damping"),
    ("SimplePhysicsOutputScaleX", "output_scale_x"),
    ("SimplePhysicsOutputScaleY", "output_scale_y"),
];

/// D transform inspector ids and the `node.transform` component each one writes.
const INSPECTOR_TRANSFORM_COMPONENTS: &[(&str, &str)] = &[
    ("TranslationX", "translationX"),
    ("TranslationY", "translationY"),
    ("TranslationZ", "translationZ"),
    ("RotationX", "rotationX"),
    ("RotationY", "rotationY"),
    ("RotationZ", "rotationZ"),
    ("ScaleX", "scaleX"),
    ("ScaleY", "scaleY"),
    ("ZSort", "zSort"),
];

/// snake_case tools that share an operation with a D-named tool: name, D sibling, required arguments. The
/// description and properties are the sibling's; `required` is what the operation needs when the call has
/// no `context` to fall back on.
const SIBLING_TOOLS: &[(&str, &str, &[&str])] = &[
    ("depth_ops_list", "DepthMapCommand_ListDepthOps", &[]),
    ("depth_ops_set", "DepthMapCommand_SetDepthOps", &["operations"]),
    ("depth_ops_add", "DepthMapCommand_AddDepthOp", &["operation", "index"]),
    ("depth_ops_update", "DepthMapCommand_UpdateDepthOp", &["index", "operation"]),
    ("depth_ops_remove", "DepthMapCommand_RemoveDepthOp", &["index"]),
    ("depth_ops_move", "DepthMapCommand_MoveDepthOp", &["fromIndex", "toIndex"]),
    ("depth_ops_clear", "DepthMapCommand_ClearDepthOps", &[]),
    ("depth_ops_apply", "DepthMapCommand_ApplyDepthOps", &[]),
    ("rig_add_standard_parameters", "DepthBoneCommand_AddStandardDepthParameters", &["root"]),
    ("rig_fit_root_z", "DepthBoneCommand_FitDepthRigRootZToDepth", &["root"]),
    ("rig_fit_bone_z", "DepthBoneCommand_FitDepthBoneZToDepth", &["bone"]),
    ("rig_set_rest", "DepthBoneCommand_SetDepthBoneRest", &["bone", "restHead", "restTail", "restRoll"]),
    ("rig_set_constraint", "DepthBoneCommand_SetDepthBoneConstraint", &["bone", "constraint"]),
    ("rig_add_source", "DepthBoneCommand_AddDepthBoneSource", &["root", "target", "bone"]),
    ("rig_remove_source", "DepthBoneCommand_RemoveDepthBoneSource", &["root", "target", "bone"]),
    ("rig_list_sources", "DepthBoneCommand_ListDepthBoneSources", &["root", "target"]),
    ("rig_set_source_settings", "DepthBoneCommand_SetDepthBoneSourceSettings", &["root", "target", "bone", "settings"]),
    ("rig_set_influence_rule", "DepthBoneCommand_SetDepthBoneInfluenceRule", &["root", "target", "rule"]),
    ("rig_get_influence_rule", "DepthBoneCommand_GetDepthBoneInfluenceRule", &["root", "target"]),
    ("rig_preview_influence", "DepthBoneCommand_PreviewDepthBoneInfluence", &["root", "target", "bone"]),
    ("rig_preview_deform", "DepthBoneCommand_PreviewDepthBoneDeform", &["root"]),
    ("rig_apply_deform", "DepthBoneCommand_ApplyDepthBoneDeform", &["root", "parameter"]),
];

fn inspector_lookup(table: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    let suffix = name.strip_prefix("Inspector_Apply_")?;
    table.iter().find(|(id, _)| *id == suffix).map(|(_, key)| *key)
}

fn inspector_node_property(name: &str) -> Option<&'static str> {
    inspector_lookup(INSPECTOR_NODE_PROPERTIES, name)
}

fn inspector_physics_key(name: &str) -> Option<&'static str> {
    inspector_lookup(INSPECTOR_PHYSICS_KEYS, name)
}

fn inspector_transform_component(name: &str) -> Option<&'static str> {
    inspector_lookup(INSPECTOR_TRANSFORM_COMPONENTS, name)
}

fn map_inspector_component(
    request: &mut Map<String, Value>,
    args: &mut Map<String, Value>,
    component: &str,
) -> Result<&'static str, String> {
    request.insert("component".into(), json!(component));
    if let Some(value) = args.remove("value") {
        request.insert("value".into(), value);
    }
    Ok("node.transform")
}

fn direct_operation(name: &str) -> Option<&'static str> {
    Some(match name {
        "new" => "new",
        "open" => "open",
        "import_psd" => "import.psd",
        "merge_psd_plan" => "merge.psd.plan",
        "merge_psd_apply" => "merge.psd.apply",
        "import_kra" => "import.kra",
        "merge_kra_plan" => "merge.kra.plan",
        "merge_kra_apply" => "merge.kra.apply",
        "export_inp" => "export.inp",
        "export_image" => "export.image",
        "export_frames" => "export.frames",
        "export_gif" => "export.gif",
        "export_collect" => "export.collect",
        "export_video_start" => "export.video.start",
        "export_video_status" => "export.video.status",
        "export_video_probe" => "export.video.probe",
        "export_video_probe_status" => "export.video.probe.status",
        "export_video_command" => "export.video.command",
        "export_video_cancel" => "export.video.cancel",
        "import_inp" => "import.inp",
        "merge_inp" => "merge.inp",
        "import_folder" => "import.folder",
        "merge_images" => "merge.images",
        "tool_regenerate_node_ids" => "tool.regenerate_node_ids",
        "tool_repair" => "tool.repair",
        "tool_generate_fake_layer_names" => "tool.generate_fake_layer_names",
        "tool_premultiply_textures" => "tool.premultiply_textures",
        "tool_rebleed_textures" => "tool.rebleed_textures",
        "tool_import_session_data" => "tool.import_session_data",
        "view_screenshot" => "view.screenshot",
        "sketch_snapshot" => "sketch.snapshot",
        "values_reset" => "values.reset",
        "extdata_get" => "extdata.get",
        "extdata_set" => "extdata.set",
        "texture_get" => "texture.get",
        "texture_set" => "texture.set",
        "texture_export" => "texture.export",
        "texture_replace" => "texture.replace",
        "scene_geometry" => "scene.geometry",
        "scene_overlap" => "scene.overlap",
        "model_statistics" => "model.statistics",
        "atlas_build" => "atlas.build",
        "atlas_layout_set" => "atlas.layout.set",
        "atlas_layout_clear" => "atlas.layout.clear",
        "snapshot" => "snapshot",
        "parameters" => "parameters",
        "atlas_pack" => "atlas.pack",
        "cv_analyze" => "cv.analyze",
        "image_process" => "image.process",
        "nodes_list" => "nodes.list",
        "node_get" => "node.get",
        "node_create" => "node.create",
        "node_move" => "node.move",
        "node_delete" => "node.delete",
        "node_copy" => "node.copy",
        "node_cut" => "node.cut",
        "node_paste" => "node.paste",
        "node_convert" => "node.convert",
        "node_insert" => "node.insert",
        "deformer_define_vertices" => "deformer.define_vertices",
        "deformer_define_grid" => "deformer.define_grid",
        "deformer_define_mesh" => "deformer.define_mesh",
        "deformer_set" => "deformer.set",
        "deformer_validate" => "deformer.validate",
        "deformer_extend_grid" => "deformer.extend_grid",
        "deformer_apply_to_children" => "deformer.apply_to_children",
        "deformer_create_rotation" => "deformer.create_rotation",
        "deformer_bezier_controls" => "deformer.bezier_controls",
        "deformer_bezier_warp" => "deformer.bezier_warp",
        "deformer_generate_sway" => "deformer.generate_sway",
        "deformer_generate_face_turn" => "deformer.generate_face_turn",
        "deformer_generate_humanoid" => "deformer.generate_humanoid",
        "physics_set" => "physics.set",
        "physics_preset_save" => "physics.preset.save",
        "physics_preset_delete" => "physics.preset.delete",
        "physics_preset_list" => "physics.preset.list",
        "physics_step" => "physics.step",
        "physics_reset" => "physics.reset",
        "animation_create" => "animation.create",
        "animation_rename" => "animation.rename",
        "animation_delete" => "animation.delete",
        "animation_set" => "animation.set",
        "animation_key_set" => "animation.key.set",
        "animation_key_delete" => "animation.key.delete",
        "animation_marker_set" => "animation.marker.set",
        "animation_marker_delete" => "animation.marker.delete",
        "animation_blink_parameters_set" => "animation.blink.parameters.set",
        "animation_blink_set" => "animation.blink.set",
        "animation_blink_key_delete" => "animation.blink.key.delete",
        "animation_audio_add" => "animation.audio.add",
        "animation_audio_set" => "animation.audio.set",
        "animation_audio_delete" => "animation.audio.delete",
        "animation_image_add" => "animation.image.add",
        "animation_image_set" => "animation.image.set",
        "animation_image_delete" => "animation.image.delete",
        "animation_image_sequence" => "animation.image.sequence",
        "animation_image_time" => "animation.image.time",
        "animation_image_read" => "animation.image.read",
        "animation_frame_step" => "animation.frame_step",
        "animation_record_start" => "animation.record.start",
        "animation_record_status" => "animation.record.status",
        "animation_record_stop" => "animation.record.stop",
        "animation_playlist_set" => "animation.playlist.set",
        "animation_playlist_delete" => "animation.playlist.delete",
        "animation_scene_group_set" => "animation.scene_group.set",
        "animation_scene_group_delete" => "animation.scene_group.delete",
        "animation_playlist_seek" => "animation.playlist.seek",
        "animation_group_add" => "animation.group.add",
        "animation_group_set" => "animation.group.set",
        "animation_group_delete" => "animation.group.delete",
        "animation_shy_set" => "animation.shy.set",
        "animation_freeze_set" => "animation.freeze.set",
        "animation_bookmark_parameters" => "animation.bookmark.parameters",
        "animation_controller_add" => "animation.controller.add",
        "animation_controller_set" => "animation.controller.set",
        "animation_controller_delete" => "animation.controller.delete",
        "animation_controller_read" => "animation.controller.read",
        "animation_controller_move" => "animation.controller.move",
        "animation_controller_bake" => "animation.controller.bake",
        "animation_physics_bake" => "animation.physics.bake",
        "animation_motionsync_start" => "animation.motionsync.start",
        "animation_motionsync_status" => "animation.motionsync.status",
        "animation_motionsync_cancel" => "animation.motionsync.cancel",
        "animation_motionsync_bake" => "animation.motionsync.bake",
        "animation_form_key" => "animation.form.key",
        "animation_form_delete" => "animation.form.delete",
        "animation_form_time" => "animation.form.time",
        "animation_form_read" => "animation.form.read",
        "animation_template_save" => "animation.template.save",
        "animation_template_apply" => "animation.template.apply",
        "animation_template_delete" => "animation.template.delete",
        "animation_template_list" => "animation.template.list",
        "animation_bookmark_set" => "animation.bookmark.set",
        "animation_bookmark_delete" => "animation.bookmark.delete",
        "animation_bookmark_copy" => "animation.bookmark.copy",
        "animation_lane_set" => "animation.lane.set",
        "animation_lane_delete" => "animation.lane.delete",
        "animation_key_add" => "animation.key.add",
        "animation_key_add_all" => "animation.key.add_all",
        "animation_key_delete_all" => "animation.key.delete_all",
        "animation_key_copy" => "animation.key.copy",
        "animation_key_paste" => "animation.key.paste",
        "animation_sample" => "animation.sample",
        "animation_onion" => "animation.onion",
        "animation_trajectory" => "animation.trajectory",
        "animation_seek" => "animation.seek",
        "animation_advance" => "animation.advance",
        "animation_reset" => "animation.reset",
        "random_pose_seek" => "random_pose.seek",
        "motion3_import" => "motion3.import",
        "live2d_authoring_inspect" => "live2d.authoring.inspect",
        "live2d_authoring_read" => "live2d.authoring.read",
        "live2d_cdi3_export" => "live2d.cdi3.export",
        "live2d_runtime_inspect" => "live2d.runtime.inspect",
        "live2d_physics3_export" => "live2d.physics3.export",
        "live2d_convert_inx" => "live2d.convert.inx",
        "live2d_physics3_read" => "live2d.physics3.read",
        "live2d_physics3_edit" => "live2d.physics3.edit",
        "live2d_physics3_preset_save" => "live2d.physics3.preset.save",
        "live2d_physics3_preset_delete" => "live2d.physics3.preset.delete",
        "live2d_physics3_preset_list" => "live2d.physics3.preset.list",
        "live2d_userdata3_export" => "live2d.userdata3.export",
        "live2d_user_data_set" => "live2d.user_data.set",
        "live2d_animation_read" => "live2d.animation.read",
        "live2d_animation_key_add" => "live2d.animation.key.add",
        "live2d_animation_curve_add" => "live2d.animation.curve.add",
        "live2d_animation_curve_delete" => "live2d.animation.curve.delete",
        "live2d_animation_track_add" => "live2d.animation.track.add",
        "live2d_animation_track_delete" => "live2d.animation.track.delete",
        "live2d_animation_track_duplicate" => "live2d.animation.track.duplicate",
        "live2d_animation_resource_add" => "live2d.animation.resource.add",
        "live2d_animation_image_resource_add" => "live2d.animation.image_resource.add",
        "live2d_animation_image_resource_relink" => "live2d.animation.image_resource.relink",
        "live2d_animation_key_delete" => "live2d.animation.key.delete",
        "live2d_animation_key_set" => "live2d.animation.key.set",
        "live2d_animation_point_set" => "live2d.animation.point.set",
        "live2d_animation_point_delete" => "live2d.animation.point.delete",
        "live2d_animation_point_add" => "live2d.animation.point.add",
        "live2d_animation_handle_set" => "live2d.animation.handle.set",
        "live2d_animation_curve_type_set" => "live2d.animation.curve_type.set",
        "live2d_animation_scene_set" => "live2d.animation.scene.set",
        "live2d_animation_scene_add" => "live2d.animation.scene.add",
        "live2d_animation_scene_delete" => "live2d.animation.scene.delete",
        "live2d_animation_bindings" => "live2d.animation.bindings",
        "live2d_animation_attach" => "live2d.animation.attach",
        "live2d_model_attach" => "live2d.model.attach",
        "live2d_animation_list" => "live2d.animation.list",
        "live2d_animation_seek" => "live2d.animation.seek",
        "live2d_animation_advance" => "live2d.animation.advance",
        "live2d_animation_reset" => "live2d.animation.reset",
        "live2d_expression_import" => "live2d.expression.import",
        "live2d_expression_apply" => "live2d.expression.apply",
        "live2d_expression_reset" => "live2d.expression.reset",
        "live2d_expression_list" => "live2d.expression.list",
        "live2d_pose_attach" => "live2d.pose.attach",
        "live2d_pose_reset" => "live2d.pose.reset",
        "live2d_pose_step" => "live2d.pose.step",
        "live2d_physics_attach" => "live2d.physics.attach",
        "live2d_physics_reset" => "live2d.physics.reset",
        "live2d_physics_step" => "live2d.physics.step",
        "live2d_physics_options_set" => "live2d.physics.options.set",
        "live2d_physics_stabilize" => "live2d.physics.stabilize",
        "live2d_authoring_linear_weights" => "live2d.authoring.linear_weights",
        "live2d_authoring_primary_forms" => "live2d.authoring.primary_forms",
        "live2d_authoring_morph_forms" => "live2d.authoring.morph_forms",
        "live2d_form_set_vertex" => "live2d.form.set_vertex",
        "live2d_form_set_opacity" => "live2d.form.set_opacity",
        "live2d_form_paste" => "live2d.form.paste",
        "live2d_form_set_opacities" => "live2d.form.set_opacities",
        "live2d_form_set" => "live2d.form.set",
        "live2d_source_set" => "live2d.source.set",
        "live2d_brush_set" => "live2d.brush.set",
        "live2d_brush_replace_image" => "live2d.brush.replace_image",
        "live2d_artpath_point_set" => "live2d.artpath.point.set",
        "live2d_artpath_point_add" => "live2d.artpath.point.add",
        "live2d_artpath_point_delete" => "live2d.artpath.point.delete",
        "live2d_reparent" => "live2d.reparent",
        "live2d_reparent_deformer" => "live2d.reparent_deformer",
        "live2d_delete" => "live2d.delete",
        "live2d_parameter_delete" => "live2d.parameter.delete",
        "live2d_rename" => "live2d.rename",
        "live2d_set_id" => "live2d.set_id",
        "live2d_duplicate" => "live2d.duplicate",
        "live2d_parameter_edit" => "live2d.parameter.edit",
        "live2d_keyform_add" => "live2d.keyform.add",
        "live2d_keyform_delete" => "live2d.keyform.delete",
        "live2d_parameter_set" => "live2d.parameter.set",
        "motion3_list" => "motion3.list",
        "motion3_read" => "motion3.read",
        "motion3_sample" => "motion3.sample",
        "motion3_point_set" => "motion3.point.set",
        "motion3_fade_set" => "motion3.fade.set",
        "motion3_pose_switch" => "motion3.pose.switch",
        "motion3_export" => "motion3.export",
        "motion3_bind" => "motion3.bind",
        "motion3_bindings" => "motion3.bindings",
        "motion3_pose" => "motion3.pose",
        "motion3_seek" => "motion3.seek",
        "motion3_advance" => "motion3.advance",
        "motion3_reset" => "motion3.reset",
        "mesh_mirror" => "mesh.mirror",
        "mesh_edit" => "mesh.edit",
        "mesh_copy" => "mesh.copy",
        "mesh_paste" => "mesh.paste",
        "mesh_select" => "mesh.select",
        "deform_edit" => "deform.edit",
        "automesh_generate" => "automesh.generate",
        "automesh_preset_save" => "automesh.preset.save",
        "automesh_preset_delete" => "automesh.preset.delete",
        "automesh_preset_list" => "automesh.preset.list",
        "deform_continue" => "deform.continue",
        "depth_list" => "depth.list",
        "depth_set" => "depth.set",
        "depth_clear" => "depth.clear",
        "depth_ops_list" => "depth.ops.list",
        "depth_ops_set" => "depth.ops.set",
        "depth_ops_add" => "depth.ops.add",
        "depth_ops_update" => "depth.ops.update",
        "depth_ops_remove" => "depth.ops.remove",
        "depth_ops_move" => "depth.ops.move",
        "depth_ops_clear" => "depth.ops.clear",
        "depth_ops_apply" => "depth.ops.apply",
        "depth_import_psd" => "depth.import_psd",
        "depth_import_psd_plan" => "depth.import_psd.plan",
        "depth_edit_apply" => "depth.edit.apply",
        "depth_draw_manifest_get" => "depth.draw.manifest.get",
        "depth_draw_manifest_set" => "depth.draw.manifest.set",
        "depth_draw_load_png" => "depth.draw.load_png",
        "depth_draw_layer_update" => "depth.draw.layer.update",
        "depth_draw_binding_update" => "depth.draw.binding.update",
        "depth_draw_auto_bind" => "depth.draw.auto_bind",
        "depth_draw_compose" => "depth.draw.compose",
        "depth_draw_apply" => "depth.draw.apply",
        "depth_draw_cleanup" => "depth.draw.cleanup",
        "depth_draw_fit_z" => "depth.draw.fit_z",
        "depth_draw_export_png" => "depth.draw.export_png",
        "depth_draw_diagnostics" => "depth.draw.diagnostics",
        "depth_draw_display" => "depth.draw.display",
        "depth_draw_select" => "depth.draw.select",
        "split_open" => "split.open",
        "split_part_set" => "split.part.set",
        "split_part_remove" => "split.part.remove",
        "split_part_order" => "split.part.order",
        "split_plan" => "split.plan",
        "split_components" => "split.components",
        "split_mask_inspect" => "split.mask.inspect",
        "split_overlay" => "split.overlay",
        "split_apply" => "split.apply",
        "split_close" => "split.close",
        "rig_add_skeleton" => "rig.add_skeleton",
        "rig_fit_human" => "rig.fit_human",
        "rig_add_standard_parameters" => "rig.add_standard_parameters",
        "rig_list_bones" => "rig.list_bones",
        "rig_overlay" => "rig.overlay",
        "rig_pick_bone" => "rig.pick_bone",
        "rig_drag_bone" => "rig.drag_bone",
        "rig_solve_ik" => "rig.solve_ik",
        "rig_apply_mmd_pose" => "rig.apply_mmd_pose",
        "rig_bake_mmd_motion" => "rig.bake_mmd_motion",
        "rig_import_gltf" => "rig.import_gltf",
        "mesh_import_gltf" => "mesh.import_gltf",
        "export_gltf" => "export.gltf",
        "rig_mirror" => "rig.mirror",
        "rig_set_vertex_weights" => "rig.set_vertex_weights",
        "rig_clear_vertex_weights" => "rig.clear_vertex_weights",
        "rig_add_source" => "rig.add_source",
        "rig_remove_source" => "rig.remove_source",
        "rig_list_sources" => "rig.list_sources",
        "rig_set_source_settings" => "rig.set_source_settings",
        "rig_set_influence_rule" => "rig.set_influence_rule",
        "rig_get_influence_rule" => "rig.get_influence_rule",
        "rig_preview_influence" => "rig.preview_influence",
        "rig_preview_deform" => "rig.preview_deform",
        "rig_gpu_offsets" => "rig.gpu_offsets",
        "rig_apply_deform" => "rig.apply_deform",
        "rig_fit_root_z" => "rig.fit_root_z",
        "rig_fit_bone_z" => "rig.fit_bone_z",
        "rig_set_rest" => "rig.set_rest",
        "rig_set_constraint" => "rig.set_constraint",
        "part_set_weld" => "part.set_weld",
        "part_clear_weld" => "part.clear_weld",
        "node_rename" => "node.rename",
        "object_find" => "object.find",
        "object_replace_name" => "object.replace_name",
        "object_export_csv" => "object.export_csv",
        "object_import_csv" => "object.import_csv",
        "model_template_export" => "model.template.export",
        "model_template_apply" => "model.template.apply",
        "node_transform" => "node.transform",
        "node_enabled_set" => "node.enabled.set",
        "node_enabled_toggle" => "node.enabled.toggle",
        "node_visibility_set" => "node.visibility.set",
        "node_lock_set" => "node.lock.set",
        "node_property_set" => "node.property.set",
        "node_mask_edit" => "node.mask.edit",
        "puppet_meta_set" => "puppet.meta.set",
        "parameter_get" => "parameter.get",
        "parameter_create" => "parameter.create",
        "parameter_rename" => "parameter.rename",
        "parameter_delete" => "parameter.delete",
        "parameter_set_axes" => "parameter.set_axes",
        "parameter_duplicate" => "parameter.duplicate",
        "parameter_create_mouth" => "parameter.create_mouth",
        "parameter_create_standard" => "parameter.create_standard",
        "parameter_group_create" => "parameter.group.create",
        "parameter_group_delete" => "parameter.group.delete",
        "parameter_group_set_color" => "parameter.group.set_color",
        "parameter_move" => "parameter.move",
        "parameter_set_repeat" => "parameter.set_repeat",
        "parameter_flip" => "parameter.flip",
        "parameter_mirror" => "parameter.mirror",
        "parameter_set_defaults" => "parameter.set_defaults",
        "parameter_set_all_defaults" => "parameter.set_all_defaults",
        "parameter_split" => "parameter.split",
        "binding_keypoint" => "binding.keypoint",
        "binding_remove" => "binding.remove",
        "binding_set_interpolation" => "binding.set_interpolation",
        "parameter_link" => "parameter.link",
        "parameter_unlink" => "parameter.unlink",
        "parameter_mirrored_autofill" => "parameter.mirrored_autofill",
        "parameter_synthesize_corners" => "parameter.synthesize_corners",
        "parameter_copy" => "parameter.copy",
        "parameter_paste" => "parameter.paste",
        "parameter_duplicate_flip" => "parameter.duplicate_flip",
        "parameter_convert_to_2d" => "parameter.convert_to_2d",
        "binding_copy" => "binding.copy",
        "binding_paste" => "binding.paste",
        "binding_multi_edit" => "binding.multi_edit",
        "binding_weight_limits_set" => "binding.weight_limits.set",
        "binding_copy_to" => "binding.copy_to",
        "binding_swap_with" => "binding.swap_with",
        "undo" => "undo",
        "redo" => "redo",
        "history_summary" => "history.summary",
        "history_goto" => "history.goto",
        "history_clear" => "history.clear",
        "history_limit_set" => "history.limit.set",
        "default_form_lock_set" => "default_form.lock.set",
        "view_list" => "view.list",
        "view_add" => "view.add",
        "view_select" => "view.select",
        "view_remove" => "view.remove",
        "view_sync_set" => "view.sync.set",
        "save" => "save",
        "export" => "export",
        "binding_set_trs" => "binding.set_trs",
        "binding_set_deform" => "binding.set_deform",
        "evaluate" => "evaluate",
        "scene_evaluate" => "scene.evaluate",
        "value_set" => "value.set",
        "scene_ambient_set" => "scene.ambient.set",
        "render" => "render",
        "render_packet" => "render.packet",
        "bounds" => "bounds",
        _ => return None,
    })
}

fn binding_list_schema() -> Value {
    json!({"type":"array", "items":{"type":"object", "properties":{
        "node": {"type":"integer"}, "param_name": {"type":"string"}},
        "required":["node", "param_name"]}})
}

fn number_arg(args: &Map<String, Value>, key: &str) -> Result<Value, String> {
    let value = args.get(key).ok_or_else(|| format!("{key} is required"))?;
    if value.as_f64().is_none_or(|value| !value.is_finite()) {
        return Err(format!("{key} must be a finite number"));
    }
    Ok(value.clone())
}

/// Every tool the endpoint lists, as `tools/list` returns them (name, description, inputSchema).
pub(crate) fn tool_list() -> Vec<Value> {
    tools()
}

fn tools() -> Vec<Value> {
    let mut tools = vec![
        tool(
            "bounds",
            "Frame the evaluated world geometry with a small margin.",
            schema(&[("values", values_schema())], &[]),
        ),
        tool(
            "new",
            "Create an empty model or install a supplied model.",
            schema(&[("model", json!({ "type": "object" }))], &[]),
        ),
        tool(
            "open",
            "Open an INP/INX or supported native CMO3 file into the authoritative session.",
            schema(&[("path", string_schema())], &["path"]),
        ),
        tool(
            "import_psd",
            "Replace the session with a new model built from a layered PSD file. The snapshot gains imported {nodes, textures}, \
             plus blendModesNotApplied (blend modes the engine lacks, imported as Normal) when there are any. \
             A layer clipped to the layer below it gets a Mask binding to that layer (or its group).",
            schema(&[
                ("path", string_schema()),
                ("keepStructure", json!({"type": "boolean"})),
                ("layerGroupNodeType", json!({"type": "string", "enum": ["Node", "Composite", "DynamicComposite"]})),
            ], &["path"]),
        ),
        tool(
            "merge_psd_plan",
            "Match the layers of a PSD file to the Parts of the open INX document without changing it. \
             Without path it reads the PSD the document is linked to (meta.psdSource, set by import_psd and \
             merge_psd_apply). The plan has source (the path read), missingLayers [{part, path}] (Parts linked to a \
             layer the file no longer has, by path or by name; empty unless path is the linked PSD or the document is \
             unlinked) and blendModesNotApplied when a layer's blend mode is not one the engine has.",
            schema(&[
                ("path", string_schema()),
                ("renameMapped", json!({"type": "boolean"})),
                ("retranslateMapped", json!({"type": "boolean"})),
            ], &[]),
        ),
        tool(
            "merge_psd_apply",
            "Merge PSD layers into the open INX document (models and textures) as one undo step. choices override the plan defaults per layer index. \
             Without path it relinks to the PSD the document is linked to (meta.psdSource; the absolute path is stored in the file). \
             A document without a link is linked to the PSD read; link true relinks a linked one to path, link false never links. removed says what happens to Parts whose linked layer the file no \
             longer has and no choice fills: keep (default), hide (enabled false) or remove (deleted like node_delete; a Part with children is hidden \
             instead). Only a merge of the linked PSD (or of a first PSD into an unlinked document) finds such Parts. \
             Result {created, replaced, textures, missingLayers, removed}.",
            schema(&[
                ("path", string_schema()),
                ("removed", json!({"type": "string", "enum": ["keep", "hide", "remove"]})),
                ("link", json!({"type": "boolean"})),
                ("renameMapped", json!({"type": "boolean"})),
                ("retranslateMapped", json!({"type": "boolean"})),
                ("choices", json!({"type": "array", "items": {"type": "object", "properties": {
                    "layer": {"type": "integer", "minimum": 0},
                    "action": {"type": "string", "enum": ["replace", "add", "ignore"]},
                    "part": integer_schema(),
                }, "required": ["layer", "action"]}})),
            ], &[]),
        ),
        tool(
            "import_kra",
            "Replace the session with a new model built from a Krita (.kra) file. \
             The snapshot gains imported {nodes, textures}, plus skippedLayers (layers that were not imported), \
             groupBlendNotApplied (groups whose blend mode no node carries) and blendModesNotApplied (blend modes \
             the engine lacks, imported as Normal) when there are any.",
            schema(&[
                ("path", string_schema()),
                ("keepStructure", json!({"type": "boolean"})),
                ("layerGroupNodeType", json!({"type": "string", "enum": ["Node", "Composite", "DynamicComposite"]})),
            ], &["path"]),
        ),
        tool(
            "merge_kra_plan",
            "Match the layers of a Krita file to the Parts of the open INX document without changing it. \
             The plan has skippedLayers when layers were not imported and blendModesNotApplied when a blend mode is not \
             one the engine has.",
            schema(&[
                ("path", string_schema()),
                ("renameMapped", json!({"type": "boolean"})),
                ("retranslateMapped", json!({"type": "boolean"})),
            ], &["path"]),
        ),
        tool(
            "merge_kra_apply",
            "Merge Krita layers into the open INX document (models and textures) as one undo step. \
             choices override the plan defaults per layer index. Result {created, replaced, textures, \
             missingLayers (always empty: a document links to a PSD only), removed}.",
            schema(&[
                ("path", string_schema()),
                ("renameMapped", json!({"type": "boolean"})),
                ("retranslateMapped", json!({"type": "boolean"})),
                ("choices", json!({"type": "array", "items": {"type": "object", "properties": {
                    "layer": {"type": "integer", "minimum": 0},
                    "action": {"type": "string", "enum": ["replace", "add", "ignore"]},
                    "part": integer_schema(),
                }, "required": ["layer", "action"]}})),
            ], &["path"]),
        ),
        tool(
            "export_gltf",
            "Export the open INX document as layered planes in glTF 2.0 without changing the session: one mesh for each drawn Part at its rest shape, placed along Z in draw order, with its texture as an embedded PNG. DepthBones become nodes and each key of a one-axis parameter becomes a morph target. `path` ends in .glb (binary) or .gltf (one file, buffer inside). unitsPerMetre is the number of canvas units for one metre of the file and is required. Bone weights, masks, blend modes, composites, physics, animations and two-axis parameters are listed in the returned loss report as dropped, so the call needs lossy=true. Rights data an import kept is written to `asset.extras` and shown under `rights` (`truncated` names `rights` when that view is not all of it), and the call then needs acknowledgeRights=true, which only the user may decide. While `needs` names an argument, or with dryRun=true, the report is returned and nothing is written (`applied` false). An existing file is only replaced with overwrite. The last applied report is readable at resource://soul2d/export/gltf-report.",
            schema(
                &[
                    ("path", string_schema()),
                    ("unitsPerMetre", json!({ "type": "number", "exclusiveMinimum": 0 })),
                    ("lossy", json!({ "type": "boolean" })),
                    ("acknowledgeRights", json!({ "type": "boolean" })),
                    ("overwrite", json!({ "type": "boolean" })),
                    ("dryRun", json!({ "type": "boolean" })),
                ],
                &["path"],
            ),
        ),
        tool(
            "export_inp",
            "Export the open INX document as an INP runtime file (atlas packing, pruning) without changing the \
             session. The .inp extension is forced; an existing file is only replaced with overwrite.",
            schema(&[
                ("path", string_schema()),
                ("atlasResolution", json!({"type": "integer", "minimum": 1, "maximum": 8192})),
                ("nonLinearScaling", json!({"type": "boolean"})),
                ("scale", json!({"type": "number", "exclusiveMinimum": 0, "maximum": 16})),
                ("padding", json!({"type": "integer", "minimum": 0})),
                ("optimizePruneUnused", json!({"type": "boolean"})),
                ("overwrite", json!({"type": "boolean"})),
            ], &["path"]),
        ),
        tool(
            "export_image",
            "Render one camera of the open INP/INX document to an image file without changing the session. The \
             extension is forced to the format's (png, tga or jpeg). An existing file is only replaced \
             with overwrite. Not available for native Live2D documents (use render).",
            schema(&[
                ("path", string_schema()),
                ("format", json!({"type": "string", "enum": ["png", "tga", "jpeg"]})),
                ("camera", string_schema()),
                ("transparency", json!({"type": "boolean"})),
                ("background", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                    "minItems": 3, "maxItems": 3})),
                ("overwrite", json!({"type": "boolean"})),
            ], &["path"]),
        ),
        tool(
            "export_frames",
            "Render the animation of the open INP/INX document as numbered stills: path must end in .png or .tga \
             and frame N is written to <stem>-N.<ext> beside it. Runs to completion before replying; returns the \
             frame count and the first and last file. An existing file is only replaced with overwrite. Limited to \
             1000000000 pixels in total (frames x width x height); use export_video_start with an image codec beyond that.\
              The clip's image tracks (animation_image_add) are drawn with the model; imagesLeftOut in the reply names the ones that cannot be drawn.",
            schema(&export_video_properties(), &["path"]),
        ),
        tool(
            "export_gif",
            "Render the animation of the open INP/INX document as one animated GIF: path gets the .gif extension. \
             colors (2 to 256, default 256) is the palette of each frame; with transparency one slot of it is the \
             transparent colour. No dithering. repeat (0 to 65535) is how many times the clip plays again after the \
             first time, absent loops forever and 0 plays once. A GIF shows a frame for whole hundredths of a \
             second, at least 2 (browsers slow shorter frames down): the rate is rounded (the reply's delay), and \
             a clip faster than 50 fps is rendered at 50 fps, so it keeps its length. Runs to completion before \
             replying; an existing file is only replaced with overwrite. Limited to 1000000000 pixels in total \
             (frames x width x height). The clip's image tracks (animation_image_add) are drawn with the model; imagesLeftOut in the reply names the ones that cannot be drawn.",
            schema(&gif_properties(), &["path"]),
        ),
        tool(
            "export_collect",
            "Copy the open INX document into the folder dir, with every image its DepthDraw layers and animation sequences name copied to \
             dir/assets and named there by a relative path, so the folder can move as one. The file is named after \
             the open document (model.inx when it has no file); the open document and its links are unchanged. A \
             source that is missing refuses the whole copy; existing files are only replaced with overwrite.",
            schema(&[("dir", string_schema()), ("overwrite", json!({"type": "boolean"}))], &["dir"]),
        ),
        tool(
            "export_video_start",
            "Start encoding the animation of the open INP/INX document with ffmpeg (arguments only, no shell) on a \
             background thread and return its job id; poll export_video_status. One encode runs at a time. \
             A failed or cancelled encode can leave a partial output file, and with overwrite an existing \
             file is already replaced when the encode starts. Extra ffmpeg options are a host setting, not a request \
             field. preset (instead of codec) picks the encoder and the extension: mp4 (H.264, no transparency), \
             webm (VP9, with alpha under transparency) or mov (ProRes 4444 with alpha, 422 HQ without). audio names \
             a sound file to mux in with the container's audio codec (AAC in mp4 and mov), padded with silence \
             or cut to the video's length. Without it the clip's unmuted audio tracks (animation_audio_add) are \
             mixed in, each starting where its frame first shows; audioLeftOut in the reply names the tracks \
             that cannot play. The clip's image tracks (animation_image_add) are drawn with the model; imagesLeftOut in the reply names the ones that cannot be drawn.",
            schema(&{
                let mut properties = export_video_properties();
                properties.push(("codec", string_schema()));
                properties.push(("preset", json!({"type": "string", "enum": ["mp4", "webm", "mov"]})));
                properties.push(("audio", string_schema()));
                properties
            }, &["path"]),
        ),
        tool("export_video_probe", "Start one background codec probe using the host-configured encoder and fixed -codecs arguments. No file or document edits. Refuses another running probe. Uses the existing five-second timeout and bounded output; poll export_video_probe_status or the video-codecs resource. Requests cannot choose a program or extra options.",
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false})),
        tool("export_video_probe_status", "Read the last codec probe: idle, running, finished with codecs or failed with error, including the program captured at start. Never waits for a running worker; available for every document format.",
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false})),
        tool("export_video_command", "Plan the actual host-configured ffmpeg command for an INP/INX video export without running a process, rendering or writing output. Accepts the same options as export_video_start, including preset/audio; respects overwrite and host extra options. Requires a valid output parent and protects the open document. Refuses Motion3 preview, but permits an existing video job. Returns program, argument vector, working directory, output and timing plus omission warnings. Caches the last successful preview as the video-command resource; new/open clears it. This is a preview, not executable-resolution or later-launch proof.", {
            let mut properties = export_video_properties();
            properties.push(("codec", string_schema()));
            properties.push(("preset", json!({"type":"string","enum":["mp4","webm","mov"]})));
            properties.push(("audio", string_schema()));
            let mut input = schema(&properties, &["path"]);
            input["additionalProperties"] = json!(false);
            input
        }),
        tool(
            "export_video_status",
            "Read a video export job: state running, finished, failed (with error) or cancelled, and the frames \
             done of the total. Does not wait.",
            schema(&[("job", integer_schema())], &["job"]),
        ),
        tool(
            "export_video_cancel",
            "Ask a running video export to stop. ffmpeg is closed and the partial file is kept; export_video_status \
             reports cancelled once it has stopped.",
            schema(&[("job", integer_schema())], &["job"]),
        ),
        tool(
            "extdata_get",
            "Read one named payload of the INX extension section as base64.",
            schema(&[("key", string_schema())], &["key"]),
        ),
        tool(
            "extdata_set",
            "Store (base64 string) or remove (base64 null) one named payload of the INX extension section as \
             one undo step.",
            schema(&[
                ("key", string_schema()),
                ("base64", json!({"type": ["string", "null"]})),
            ], &["key", "base64"]),
        ),
        tool(
            "texture_get",
            "Read the stored bytes (base64 PNG or TGA) and decoded size of one texture slot.",
            schema(&[("slot", json!({"type": "integer", "minimum": 0}))], &["slot"]),
        ),
        tool(
            "texture_set",
            "Replace the pixels of one existing texture slot, for every Part that uses it, with an image from path \
             (PNG, TGA or JPEG) or png_base64 (PNG, at most 7 MiB of text), as one undo step. Straight-alpha input is \
             premultiplied on store unless premultiplied is true. A different size than the old texture is refused \
             unless allowResize is true; the Part meshes and UVs are not rescaled. Returns slot, width, height and \
             changed (false and no undo step when the pixels are the same).",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "slot": {"type": "integer", "minimum": 0},
                    "path": {"type": "string", "minLength": 1},
                    "png_base64": {"type": "string", "maxLength": 7 * 1024 * 1024},
                    "premultiplied": {"type": "boolean"},
                    "allowResize": {"type": "boolean"}
                },
                "required": ["slot"]
            }),
        ),
        tool(
            "texture_export",
            "Write every INX texture slot as slot-N.png in an existing directory. The PNG holds the stored \
             premultiplied pixels. The document does not change. An existing slot-N.png is refused before any \
             write unless overwrite is true. A failed write can leave files already written. Confirm with \
             resource://soul2d/textures/{slot}, which returns the same JSON as texture_get.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "directory": {"type": "string", "minLength": 1},
                    "overwrite": {"type": "boolean"}
                },
                "required": ["directory"]
            }),
        ),
        tool(
            "texture_replace",
            "Read slot-N.png from an existing directory back into that INX texture slot, as one undo step for \
             every slot whose pixels differ. Missing files leave their slots unchanged and are listed in missing. \
             The PNG is stored as decoded, in the slot's existing PNG or TGA format, with no second premultiply: \
             texture_export wrote stored premultiplied pixels, so a straight-alpha file belongs in texture_set. \
             A different size is refused for the whole batch unless allowResize is true, and the meshes are not \
             rescaled. Identical pixels record no undo step.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "directory": {"type": "string", "minLength": 1},
                    "allowResize": {"type": "boolean"}
                },
                "required": ["directory"]
            }),
        ),
        tool(
            "model_statistics",
            "Count the tree that is rendered without changing the session: nodes, maskReferences and \
             maxOffscreenDepth, the deepest nesting of offscreen passes (Composite and DynamicComposite nodes; \
             CMO3 Parts with offscreen drawing). 0 means none; a tree deeper than offscreenDepthLimit does not render. \
             A hidden node is counted, as it is for that limit.",
            json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false }),
        ),
        tool(
            "scene_geometry",
            "List the drawn Parts in painter order with world-space vertices, indices, zsort and the evaluated opacity, \
             without texture data.",
            schema(&[("values", values_schema()), ("deformations", json!({
                "type": "object", "additionalProperties": {
                    "type": "array", "items": pair_schema()
                }
            }))], &[]),
        ),
        tool(
            "scene_overlap",
            "List the drawn ArtMeshes whose own triangles overlap in the current pose (REN10), which an advanced blend \
mode can draw twice and flicker: by default those with an advanced blend (a blend_mode other than Normal, Multiply and \
LinearDodge, a Cubism53 color other than NORMAL, ADD and MULTIPLY, or alpha other than over), every mesh with all. Triangles sharing \
only an edge or corner do not count. Result {meshes:[{id, name, blendMode, pairs, triangles}]}, where triangles lists up \
to 16 index pairs of triangles in the mesh's index order. INX documents only; a native document reports nothing \
without all.",
            schema(&[("values", values_schema()), ("all", json!({"type": "boolean"}))], &[]),
        ),
        tool(
            "atlas_build",
            "Plan the texture atlases D's INP export would build from the Part textures and meshes, without changing the session. \
Pages are up to 8192 pixels a side (larger than the 16M-pixel texture limit, so Soul2D itself does not draw such an exported \
file). Parts with a manual place (atlas_layout_set) keep it and the rest are packed around them. preview (an atlas index) \
adds preview, that page's albedo as a base64 PNG at most 512 pixels a side.",
            schema(&[
                ("resolution", json!({"type": "integer", "minimum": 1, "maximum": 8192})),
                ("preview", json!({"type": "integer", "minimum": 0})),
                ("padding", json!({"type": "integer", "minimum": 0})),
                ("scale", json!({"type": "number", "exclusiveMinimum": 0, "maximum": 16})),
                ("nonLinearScaling", json!({"type": "boolean"})),
                ("parts", json!({"type": "array", "items": integer_schema()})),
            ], &[]),
        ),
        tool(
            "atlas_layout_set",
            "Place Parts by hand in the atlas layout (IO04) in one undo: each {id, atlas, x, y, scale?} puts the Part's pixels \
(padding excluded) with their top left at x, y on page atlas (0 to 15), at scale texels per texture pixel (default: the \
packing scale). Stored as model atlasLayout.parts; atlas_build and export_inp keep the places and pack the other Parts \
around them, and refuse places that overlap or leave the page. Result {atlasLayout}.",
            schema(&[("parts", json!({"type": "array", "minItems": 1, "maxItems": 4096, "items": {"type": "object", "properties": {
                "id": integer_schema(), "atlas": {"type": "integer", "minimum": 0, "maximum": 15},
                "x": {"type": "integer", "minimum": 0}, "y": {"type": "integer", "minimum": 0},
                "scale": {"type": "number", "exclusiveMinimum": 0, "maximum": 16},
            }, "required": ["id", "atlas", "x", "y"]}}))], &["parts"]),
        ),
        tool(
            "atlas_layout_clear",
            "Return Parts to automatic packing in one undo: parts lists Part UUIDs, or none to clear every manual place. \
Result {atlasLayout}.",
            schema(&[("parts", json!({"type": "array", "items": integer_schema()}))], &[]),
        ),
        tool(
            "snapshot",
            "Read the model, runtime values, history state, and revision.",
            schema(&[], &[]),
        ),
        tool(
            "parameters",
            "List raw parameter records.",
            schema(&[], &[]),
        ),
        tool(
            "parameter_get",
            "Read a parameter or group by UUID.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_create",
            "Create a 1D or 2D parameter.",
            schema(
                &[
                    ("dimensions", json!({ "type": "integer", "enum": [1, 2] })),
                    ("name", string_schema()),
                    ("min", pair_schema()),
                    ("max", pair_schema()),
                ],
                &["dimensions"],
            ),
        ),
        tool(
            "parameter_rename",
            "Rename a parameter by UUID.",
            schema(
                &[("id", integer_schema()), ("name", string_schema())],
                &["id", "name"],
            ),
        ),
        tool(
            "parameter_delete",
            "Delete a parameter and clear its SimplePhysics assignments in one undo.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_set_axes",
            "Edit parameter ranges and normalized axes; remap keys in one undo.",
            schema(&[
                ("id", integer_schema()), ("min", pair_schema()), ("max", pair_schema()),
                ("axis_points", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items":{"type":"array", "items":{"type":"number"}, "maxItems":256}})),
            ], &["id", "min", "max", "axis_points"]),
        ),
        tool(
            "parameter_duplicate",
            "Copy a parameter (and its bindings) into the same group in one undo.",
            schema(&[("id", integer_schema()), ("name", string_schema())], &["id"]),
        ),
        tool(
            "parameter_create_mouth",
            "Create the 2D mouth-shape parameter with extra axis points in one undo.",
            schema(&[("name", string_schema())], &[]),
        ),
        tool(
            "parameter_create_standard",
            "Add the parameters of the Cubism standard parameter list (ParamAngleX and the rest) that no parameter is named after yet, in one undo. Each is 1D with the list's minimum, default and maximum. all=true also adds the rows the list marks as needed only sometimes. Answers the created and the existing names. INX documents only.",
            schema(&[("all", json!({"type": "boolean"}))], &[]),
        ),
        tool(
            "parameter_group_create",
            "Create a parameter group, optionally moving member parameters into it, in one undo.",
            schema(&[
                ("name", string_schema()),
                ("members", json!({"type":"array", "items": integer_schema()})),
            ], &[]),
        ),
        tool(
            "parameter_group_delete",
            "Delete a parameter group; its parameters return to the top level in one undo.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_group_set_color",
            "Set a parameter group's color as three numbers.",
            schema(&[
                ("id", integer_schema()),
                ("color", json!({"type":"array", "minItems":3, "maxItems":3, "items":{"type":"number"}})),
            ], &["id", "color"]),
        ),
        tool(
            "parameter_move",
            "Move or reorder a parameter: group is a group UUID or null for the top level; index is the position.",
            schema(&[
                ("id", integer_schema()),
                ("group", json!({"type":["integer", "null"]})),
                ("index", json!({"type":"integer", "minimum":0})),
            ], &["id"]),
        ),
        tool(
            "parameter_set_repeat",
            "Join the two ends of a 1D parameter into a loop, or open it again. Any finite value is then accepted \
             and wrapped into the range.",
            schema(&[("id", integer_schema()), ("repeat", json!({"type": "boolean"}))], &["id", "repeat"]),
        ),
        tool(
            "parameter_flip",
            "Reverse a parameter axis and the binding keys that follow it in one undo.",
            schema(&[("id", integer_schema()), ("axis", json!({"type":"integer", "enum":[0, 1]}))], &["id"]),
        ),
        tool(
            "parameter_mirror",
            "Negate every set binding key across an axis (0 horizontal, 1 vertical) in one undo.",
            schema(&[("id", integer_schema()), ("axis", json!({"type":"integer", "enum":[0, 1]}))], &["id"]),
        ),
        tool(
            "parameter_set_defaults",
            "Set the parameter's starting position to value, or to its current session value.",
            schema(&[("id", integer_schema()), ("value", pair_schema())], &["id"]),
        ),
        tool(
            "parameter_set_all_defaults",
            "Set every parameter's starting position to its current session value in one undo. Answers \
             {parameters, changed}; when no value differs nothing is committed.",
            // It edits the whole document, so a stray `id` must not read as "this parameter only".
            json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false }),
        ),
        tool(
            "parameter_split",
            "Move the bindings of the listed nodes into a new '(Split)' parameter in one undo.",
            schema(&[
                ("id", integer_schema()),
                ("nodes", json!({"type":"array", "items": integer_schema(), "minItems":1})),
            ], &["id", "nodes"]),
        ),
        tool(
            "binding_keypoint",
            "Edit one keypoint of bindings: unset, reset, set_current, invert, mirror_horizontal, mirror_vertical, \
flip_deform, symmetrize_deform or set_from_mirror_horizontal / _vertical / _diagonal / _1d (these use the \
document's flip pairs; _1d pulls from the partner without bindings and pushes to it with them).",
            schema(&[
                ("parameter", integer_schema()),
                ("key", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items": {"type":"integer", "minimum":0}})),
                ("action", json!({"type":"string", "enum":
                    ["unset", "reset", "set_current", "invert", "mirror_horizontal", "mirror_vertical",
                     "flip_deform", "symmetrize_deform", "set_from_mirror_horizontal",
                     "set_from_mirror_vertical", "set_from_mirror_diagonal", "set_from_mirror_1d"]})),
                ("bindings", binding_list_schema()),
            ], &["parameter", "key", "action"]),
        ),
        tool(
            "parameter_link",
            "Make an axis of parameter id drive an axis of another parameter (a parameter link) in one undo.",
            schema(&[
                ("id", integer_schema()),
                ("axis", json!({"type":"integer", "enum":[0, 1]})),
                ("target", integer_schema()),
                ("target_axis", json!({"type":"integer", "enum":[0, 1]})),
            ], &["id", "target"]),
        ),
        tool(
            "parameter_unlink",
            "Remove the link from parameter id to an axis of the target parameter in one undo.",
            schema(&[
                ("id", integer_schema()),
                ("target", integer_schema()),
                ("target_axis", json!({"type":"integer", "enum":[0, 1]})),
            ], &["id", "target"]),
        ),
        tool(
            "parameter_mirrored_autofill",
            "Fill unset keys of one half of the parameter from the other half through the flip pairs, in one undo: \
direction 1 low X, 2 high X, 3 high Y, 4 low Y.",
            schema(&[("id", integer_schema()), ("direction", json!({"type":"integer", "enum":[1, 2, 3, 4]}))],
                &["id", "direction"]),
        ),
        tool(
            "parameter_synthesize_corners",
            "Fill the corner keys of a 2D parameter from its edges in one undo: each key off the default's row and \
column becomes row edge + column edge - reference key, for bindings whose whole reference row and column are set.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_copy",
            "Copy a parameter with its bindings into the session clipboard without changing the document.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_paste",
            "Paste the copied parameter's bindings into parameter id in one undo; flip pastes mirrored onto the \
flip-pair partners. The clipboard is used up.",
            schema(&[("id", integer_schema()), ("flip", json!({"type":"boolean"}))], &["id"]),
        ),
        tool(
            "parameter_duplicate_flip",
            "Duplicate a parameter with its bindings mirrored onto the flip-pair partners in one undo.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "parameter_convert_to_2d",
            "Convert a 1D parameter into a 2D one with the same UUID, X range and X axis points in one undo.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "binding_copy",
            "Copy the keypoint of bindings (all of the parameter's when bindings is omitted) into the session clipboard.",
            schema(&[
                ("parameter", integer_schema()),
                ("key", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items": {"type":"integer", "minimum":0}})),
                ("bindings", binding_list_schema()),
            ], &["parameter", "key"]),
        ),
        tool(
            "binding_paste",
            "Paste the copied binding keypoint into the parameter at key in one undo; bindings select the targets \
             (Paste Form Special: only those channels). ratio in [0,1] blends instead (Blend Form): current + \
             ratio × (copied − current). invert {axis: vertical|horizontal, origin?} mirrors a pasted deform shape \
             about that line in node space (default origin: centre of the node's rest bounds), negates a pasted \
             transform.r.z and reflects a pasted transform.t.x (vertical) or t.y (horizontal) position about origin \
             in the parent's space (default 0). Copy a key and paste it back with invert to Reflect it.",
            schema(&[
                ("parameter", integer_schema()),
                ("key", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items": {"type":"integer", "minimum":0}})),
                ("bindings", binding_list_schema()),
                ("ratio", json!({"type":"number", "minimum":0, "maximum":1})),
                ("invert", json!({"type":"object", "properties": {
                    "axis": {"type":"string", "enum":["vertical", "horizontal"]}, "origin": number_schema()},
                    "required": ["axis"]})),
            ], &["parameter", "key"]),
        ),
        tool(
            "binding_copy_to",
            "Copy the keypoint of bindings on one node to a compatible node in one undo.",
            schema(&[
                ("parameter", integer_schema()),
                ("key", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items": {"type":"integer", "minimum":0}})),
                ("bindings", binding_list_schema()),
                ("target", integer_schema()),
            ], &["parameter", "key", "bindings", "target"]),
        ),
        tool(
            "binding_swap_with",
            "Swap the keypoint of bindings on one node with those of a compatible node in one undo.",
            schema(&[
                ("parameter", integer_schema()),
                ("key", json!({"type":"array", "minItems":2, "maxItems":2,
                    "items": {"type":"integer", "minimum":0}})),
                ("bindings", binding_list_schema()),
                ("target", integer_schema()),
            ], &["parameter", "key", "bindings", "target"]),
        ),
        tool(
            "binding_remove",
            "Remove bindings (node + param_name) from a parameter in one undo.",
            schema(&[("parameter", integer_schema()), ("bindings", binding_list_schema())],
                &["parameter", "bindings"]),
        ),
        tool(
            "binding_set_interpolation",
            "Set the interpolation mode (Nearest, Linear, Cubic) of bindings in one undo.",
            schema(&[
                ("parameter", integer_schema()),
                ("mode", json!({"type":"string", "enum":["Nearest", "Linear", "Cubic"]})),
                ("bindings", binding_list_schema()),
            ], &["parameter", "mode", "bindings"]),
        ),
        tool(
            "nodes_list",
            "List node UUIDs, names, types, and parent links.",
            schema(&[], &[]),
        ),
        tool(
            "node_create",
            "Create Node children under one or more parents.",
            schema(
                &[
                    ("className", string_schema()),
                    ("_suffix", string_schema()),
                    ("parents", json!({ "type": "array", "items": integer_schema() })),
                    ("name", string_schema()),
                ],
                &["className"],
            ),
        ),
        tool(
            "node_rename",
            "Rename a node by UUID.",
            schema(
                &[("id", integer_schema()), ("name", string_schema())],
                &["id", "name"],
            ),
        ),
        tool(
            "object_find",
            "List the nodes, parameters and parameter groups whose name contains text, nodes first in document \
             order. types narrows by typeId (a node's type; Parameter for parameters and groups). Changes nothing.",
            schema(
                &[
                    ("text", json!({ "type": "string", "minLength": 1, "maxLength": 256 })),
                    ("types", json!({ "type": "array", "items": string_schema(), "minItems": 1, "maxItems": 4096 })),
                    ("caseSensitive", json!({ "type": "boolean", "description": "default true" })),
                ],
                &["text"],
            ),
        ),
        tool(
            "object_replace_name",
            "Replace every occurrence of find with replace (exact case) in the names of the nodes, parameters and \
             parameter groups listed in ids, in one undo step. A name that would be empty, taken or longer than 4096 characters refuses the \
             whole call. Objects whose name lacks find are reported as unchanged.",
            schema(
                &[
                    ("ids", json!({ "type": "array", "items": integer_schema(), "minItems": 1, "maxItems": 4096 })),
                    ("find", json!({ "type": "string", "minLength": 1, "maxLength": 256 })),
                    ("replace", json!({ "type": "string", "maxLength": 256 })),
                ],
                &["ids", "find", "replace"],
            ),
        ),
        tool(
            "object_export_csv",
            "Write the UUID, type and name of every node, parameter and parameter group to a .csv file without \
             changing the session: the header uuid,type,name, then one row for each object in the order of \
             object_find, UTF-8, comma separated, a cell in double quotes when it holds a comma, a quote, a line break, \
             a semicolon or a tab. A type or a name that starts with =, +, -, @, a tab, a carriage \
             return or an apostrophe is written after an apostrophe, so that a spreadsheet does not take it for a \
             formula. The format is \
             Soul2D's own and was not checked against a CSV that Cubism Editor writes. The same text is the \
             resource resource://soul2d/object/names-csv.",
            schema(&[("path", string_schema()), ("overwrite", json!({ "type": "boolean" }))], &["path"]),
        ),
        tool(
            "object_import_csv",
            "Rename nodes, parameters and parameter groups from a CSV that object_export_csv wrote and somebody \
             edited, in one undo step. Give the text as the file path or as csv, at most 1 MiB and 4096 rows. A row \
             names its object by uuid and gives it the name of the row; one leading apostrophe of a type or a name \
             is taken off, and an object \
             without a row keeps its name. The whole call is refused when a uuid names nothing or has two rows, \
             when the type of a row is not the type of its object, or when a name would be empty, taken or longer \
             than 4096 characters. Every name is judged against the finished document, so two rows may swap \
             names. Only names are read: no object is created and no value is changed. With dryRun=true, or when \
             no name differs, nothing is written (`applied` false).",
            schema(
                &[("path", string_schema()), ("csv", string_schema()), ("dryRun", json!({ "type": "boolean" }))],
                &[],
            ),
        ),
        tool(
            "model_template_export",
            "Write the document's node tree, without textures, and its parameters to a model template .json file \
             (format soul2d.model-template.v1) without changing the session. name defaults to the file name.",
            schema(
                &[("path", string_schema()), ("name", string_schema()), ("description", string_schema()),
                    ("overwrite", json!({ "type": "boolean" }))],
                &["path"],
            ),
        ),
        tool(
            "model_template_apply",
            "Apply a model template that model_template_export wrote to the document, in one undo step. Template \
             Parts map to the document's Parts (or to the Parts listed in parts) by name, or with match=position to \
             the nearest centre once the template is fitted. The fit is one uniform scale from the areas of the two \
             sets of Parts plus a move that puts centre on centre; scale and offset override it. The template's \
             Nodes, GridDeformers, PathDeformers and Composites are copied, fitted, with new UUIDs; each mapped Part \
             moves under the copy of its template parent and stays where it is drawn. Parameters with a binding on \
             a copied node or a mapped Part come along: with parameters=join (the default) a document parameter of \
             the same name and axes takes the bindings, otherwise a parameter is added (named \"<name> (Template)\" \
             when the name is taken). Deform and translation keys scale with the fit; a Part's deform keys are \
             re-sampled at the document Part's own vertices. The template's parameter groups are not kept (a \
             document parameter inside a group can still be joined) and physics is not copied. Answers scale, offset, \
             created node UUIDs, mapped and unmapped Parts, the parameters and skippedBindings (bindings on nodes \
             that were neither copied nor mapped, such as parameter links).",
            schema(
                &[("path", string_schema()), ("parts", json!({ "type": "array", "items": integer_schema() })),
                    ("match", json!({ "type": "string", "enum": ["name", "position"] })),
                    ("parameters", json!({ "type": "string", "enum": ["join", "add"] })),
                    ("scale", json!({ "type": "number", "exclusiveMinimum": 0 })), ("offset", pair_schema())],
                &["path"],
            ),
        ),
        tool(
            "node_enabled_set",
            "Set a node's enabled flag by UUID.",
            schema(
                &[("id", integer_schema()), ("enabled", json!({ "type": "boolean" }))],
                &["id", "enabled"],
            ),
        ),
        tool(
            "node_enabled_toggle",
            "Toggle a node's enabled flag by UUID.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "node_visibility_set",
            "Hide or show several nodes in one undo step; omit enabled to toggle from the first node; recursive includes each subtree; siblings makes each listed node stand for every child of its parent (the root has none).",
            schema(
                &[
                    ("nodes", node_ids_schema()),
                    ("enabled", json!({ "type": "boolean" })),
                    ("recursive", json!({ "type": "boolean" })),
                    ("siblings", json!({ "type": "boolean" })),
                ],
                &[],
            ),
        ),
        tool(
            "node_lock_set",
            "Lock or unlock several nodes in one undo step; omit locked to toggle from the first node; recursive includes each subtree; siblings makes each listed node stand for every child of its parent. A locked node, and every node below it, cannot be picked in the editor viewport; every tool still edits it. scene_geometry marks such Parts with locked.",
            schema(
                &[
                    ("nodes", node_ids_schema()),
                    ("locked", json!({ "type": "boolean" })),
                    ("recursive", json!({ "type": "boolean" })),
                    ("siblings", json!({ "type": "boolean" })),
                ],
                &[],
            ),
        ),
        tool(
            "node_property_set",
            "Set one Inspector property of a node in one undo: lockToRoot, pinToMesh, zsort; tint, screenTint, \
             opacity, mask_threshold, blend_mode (Part, DynamicComposite, Composite); emissionStrength (Part, \
             DynamicComposite); auto_resized (DynamicComposite); propagate_meshgroup (Composite); origin (Mask); \
             viewport (Camera). blend_mode takes the INX names of its node type, and LinearBurn, LinearLight, Hue \
             and Color, which are not modes of INX: render draws them with their formula, render_packet refuses \
             them, and a program that knows the INX names only does not know them. alphaBlend (Part, \
             DynamicComposite, Composite) is the Cubism alpha composition Over, Atop, Out, Conjoint or Disjoint; \
             absent means Over. Any but Over draws the Cubism formula of the blend_mode of the same name, which \
             render draws and render_packet refuses; Difference, Exclusion, Subtract, Inverse, DestinationIn, \
             ClipToLower and SliceFromLower have none and take Over only. alphaBlend is not a key of INX. \
             MeshGroup, deformer and SimplePhysics settings use deformer_set and physics_set.",
            schema(
                &[("id", integer_schema()), ("property", string_schema()), ("value", json!({}))],
                &["id", "property", "value"],
            ),
        ),
        tool(
            "node_mask_edit",
            "Edit the mask list of a Part, DynamicComposite or Composite in one undo: add, remove, mode \
             (Mask or DodgeMask) or move (to index).",
            schema(
                &[
                    ("id", integer_schema()),
                    ("action", json!({"type": "string", "enum": ["add", "remove", "mode", "move"]})),
                    ("source", integer_schema()),
                    ("mode", json!({"type": "string", "enum": ["Mask", "DodgeMask"]})),
                    ("index", json!({"type": "integer", "minimum": 0})),
                ],
                &["id", "action", "source"],
            ),
        ),
        tool(
            "puppet_meta_set",
            "Set one puppet metadata value in one undo: name, artist, rigger, contact, licenseURL, copyright, \
             reference, preservePixels, or the physics globals pixelsPerMeter and gravity.",
            schema(&[("property", string_schema()), ("value", json!({}))], &["property", "value"]),
        ),
        tool(
            "node_copy",
            "Copy nodes into the session clipboard without changing the model.",
            schema(&[("nodes", node_ids_schema())], &[]),
        ),
        tool(
            "node_cut",
            "Copy nodes into the session clipboard and delete them.",
            schema(&[("nodes", node_ids_schema())], &[]),
        ),
        tool(
            "node_paste",
            "Paste the session clipboard under a parent and consume it.",
            schema(&[("parent", paste_parent_schema()), ("index", integer_schema())], &[]),
        ),
        tool(
            "node_insert",
            "Insert a node above each context node and reparent that node under it.",
            schema(
                &[
                    ("className", string_schema()),
                    ("_suffix", string_schema()),
                    ("nodes", node_ids_schema()),
                ],
                &["className"],
            ),
        ),
        tool(
            "deformer_define_vertices",
            "Set PathDeformer or GridDeformer control points.",
            schema(
                &[
                    ("nodes", node_ids_schema()),
                    ("vertices", json!({ "type": "array", "items": number_schema() })),
                ],
                &["vertices"],
            ),
        ),
        tool(
            "deformer_define_grid",
            "Set GridDeformer axes.",
            schema(
                &[
                    ("nodes", node_ids_schema()),
                    ("axisX", json!({ "type": "array", "items": number_schema() })),
                    ("axisY", json!({ "type": "array", "items": number_schema() })),
                ],
                &["axisX", "axisY"],
            ),
        ),
        tool(
            "physics_set",
            "Edit SimplePhysics settings in one undo. preset names a preset saved with physics_preset_save: its settings are written first and the settings of the call after them, so a setting of the call wins. Does not step the simulation or write the driven parameter.",
            physics_set_schema(),
        ),
        tool(
            "physics_preset_save",
            "Keep SimplePhysics settings under name, for physics_set to use as preset: the settings of the call, and for a setting the call leaves out the one that node holds, when node is given. param is not kept, it names a parameter of one document. The presets belong to the user of this machine, not to the document: they are a file in the settings directory, and the call changes no document and adds no undo step. A saved name is replaced only with overwrite:true, and the answer then holds replaced:true. Refused: a call without a setting, a setting physics_set refuses, nodes, output_scale_x, output_scale_y and preset, which a preset cannot keep, a name that is not 1 to 64 characters, has a control character anywhere or white space at either end, a 65th preset, presets that would make the file larger than 1 MiB, and a host without a settings directory. There are no built-in presets.",
            schema(
                &[
                    ("name", string_schema()),
                    ("node", integer_schema()),
                    ("model_type", json!({ "type": "string", "enum": ["Pendulum", "SpringPendulum"] })),
                    ("map_mode", json!({ "type": "string", "enum": ["AngleLength", "XY", "LengthAngle", "YX"] })),
                    ("gravity", number_schema()),
                    ("length", number_schema()),
                    ("frequency", number_schema()),
                    ("angle_damping", number_schema()),
                    ("length_damping", number_schema()),
                    ("output_scale", json!({ "type": "array", "items": number_schema(), "minItems": 2, "maxItems": 2 })),
                    ("local_only", json!({ "type": "boolean" })),
                    ("overwrite", json!({ "type": "boolean" })),
                ],
                &["name"],
            ),
        ),
        tool(
            "physics_preset_delete",
            "Delete a preset that physics_preset_save kept. A name that is not saved is refused.",
            schema(&[("name", string_schema())], &["name"]),
        ),
        tool(
            "physics_preset_list",
            "List the presets physics_preset_save kept, as {presets: {name: {settings}}}. The resource resource://soul2d/physics/presets returns the same. The three preset tools answer with any document open.",
            schema(&[], &[]),
        ),
        tool(
            "physics_step",
            "Advance runtime physics by dt seconds without changing the document or undo history.",
            schema(&[("dt", json!({"type":"number","minimum":0}))], &["dt"]),
        ),
        tool("physics_reset", "Reset runtime physics to the current authored pose.", schema(&[], &[])),
        tool("live2d_animation_attach", "Attach an explicit CAN3 to the active CMO3 for bounded native parameter and GUID-bound PartOpacity playback. Parameter GUID/attribute IDs and Part GUIDs are validated against the selected model resource; underscore PartOpacity channels retain their exact map keys. Linked paths are never followed automatically.",
            schema(&[("path", string_schema()), ("resourceGuid", string_schema())], &["path"])),
        tool("live2d_model_attach", "Attach an explicit CMO3, or a runtime `.model3.json` (its `.moc3` and textures read from its folder; a bare `.moc3` is refused), to the active CAN3 for bounded native parameter and GUID-bound PartOpacity playback. A runtime model takes the parameter and Part GUIDs of the CAN3's curves; Glue, blend shapes, keyform colours (versions 5 and 6) and offscreen Parts (version 6) are read; `warnings` lists blend shapes the file leaves without a default keyform or parameter. Parameter GUID/attribute IDs and Part GUIDs are validated against the selected model resource; underscore PartOpacity channels retain their exact map keys. Linked paths are never followed automatically.",
            schema(&[("path", string_schema()), ("resourceGuid", string_schema())], &["path"])),
        tool("live2d_animation_list", "List active CAN3 scenes and the explicit native model/resource attachment, if any.",
            schema(&[], &[])),
        tool("live2d_animation_seek", "Sample one CAN3 scene at a GUI frame from runtime/default parameter values. Unsupported curve tokens or conflicting writers are errors. Empty PartOpacity channels produce no write; multiple populated channels for one Part are refused, including modern, legacy-colon and underscore-map channels.",
            schema(&[("sceneGuid", string_schema()), ("frame", number_schema())], &["sceneGuid", "frame"])),
        tool("live2d_animation_advance", "Advance the selected CAN3 preview by seconds; looping uses the scene frame duration.",
            schema(&[("dt", json!({"type":"number","minimum":0})), ("loop", json!({"type":"boolean"}))], &["dt", "loop"])),
        tool("live2d_animation_reset", "Clear only the session CAN3 preview values.", schema(&[], &[])),
        tool("live2d_expression_import", "Import a bounded Live2D Expression JSON file into session-only native playback state.",
            schema(&[("name", string_schema()), ("path", string_schema())], &["name", "path"])),
        tool("live2d_expression_apply", "Apply an imported expression once after the active native motion/timeline preview. \
            With fades true it fades in over its FadeInTime as motion, timeline or physics playback advances.",
            schema(&[("name", string_schema()), ("fades", json!({"type":"boolean"}))], &["name"])),
        tool("live2d_expression_reset", "Clear the active session-only expression.", schema(&[], &[])),
        tool("live2d_expression_list", "List imported session-only expressions and the active expression.", schema(&[], &[])),
        tool("live2d_pose_attach", "Attach an explicit Pose3 file to the native model. Runtime only; linked files are not followed.",
            schema(&[("path", string_schema())], &["path"])),
        tool("live2d_pose_reset", "Reset native Pose groups and selection signals without editing the document.", schema(&[], &[])),
        tool("live2d_pose_step", "Advance native Pose by seconds, optionally overriding known selection signals.",
            schema(&[("dt", json!({"type":"number","minimum":0})),
                ("signals",json!({"type":"object","additionalProperties":{"type":"number"}}))], &["dt"])),
        tool("live2d_physics_attach", "Attach an explicit Physics3 file to the native model. Runtime only; linked files are not followed.",
            schema(&[("path", string_schema())], &["path"])),
        tool("live2d_physics_reset", "Reset native Physics3 runtime values without editing the document.", schema(&[], &[])),
        tool("live2d_physics_step", "Advance native Physics3 by seconds without editing the document.",
            schema(&[("dt", json!({"type":"number","minimum":0}))], &["dt"])),
        tool("live2d_physics_options_set", "Set the complete native Physics3 gravity and wind vectors for this session.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "gravity": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "x": number_schema(), "y": number_schema() },
                        "required": ["x", "y"]
                    },
                    "wind": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "x": number_schema(), "y": number_schema() },
                        "required": ["x", "y"]
                    }
                },
                "required": ["gravity", "wind"]
            })),
        tool("live2d_physics_stabilize", "Stabilize the native Physics3 runtime pose without editing the document.",
            json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })),
        tool("motion3_import", "Import Motion3 into INX history or the active CMO3 session-only clip library. CMO3 clips are lost on open/new and must be exported separately; this does not load or render a Live2D model.",
            schema(&[("name", string_schema()), ("path", string_schema())], &["name", "path"])),
        tool("live2d_authoring_inspect", "Read supplied-profile CMO3/CAN3 authoring container and model definitions without changing the active session; does not evaluate or render a Cubism model.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_authoring_read", "Read CMO3 source geometry, primary and generated forms, multi-axis grids, morph targets, blend-weight constraints, and texture metadata without changing the session. An ArtPath source lists its stroke settings and the control points of each keyform; it is not evaluated or drawn; its name, ID, parent, clipping masks and control points can be edited, and points added and deleted. The model's brushes list the stored ArtPath brush identifiers, drawing settings and textureInput metadata, including the current input's stored canvas transform. live2d_brush_set edits stored drawing settings and transform coefficients; live2d_brush_replace_image replaces an existing shared image with a same-size static PNG. Generated vertex-count and target-availability flags describe incomplete caches. Non-null morph edit base maps are not decoded. Positions retain source coordinate spaces; this is not an evaluated canvas pose.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_animation_read", "Read CAN3 scenes, model tracks and existing parameter keys without changing the active session. scene.imageTracks separately lists image-track identity, timing, visualCurves and pointChannels for stored xy/anchor coordinates. scene.soundTracks lists existing Sound track guid, name, resourceGuid, start, internalOffset and duration for deletion; it does not decode audio or expose volume. scene.guideTracks lists the same identity and timing fields plus six supported visualCurves and pointChannels for stored xy/anchor coordinates of existing GuideImage tracks; guide directories are never opened. Unsupported or malformed Image/Guide scalar channels are omitted; budget exhaustion fails the read. imageResources lists linked image GUIDs and names/sourceFile paths only where stored; paths are literal and never opened. Values and frames retain stored units; control positions and flags are not evaluated. scene.loop, scene.tag and a parameter curve's baseValue appear only where the file stores them. Does not load linked models, play animation, edit keys, or establish Cubism compatibility.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_authoring_linear_weights", "Compute in-range mathematical weights on primary LINEAR keyform grids without changing the session. Parameter IDs omitted from parameterValues use active runtime values, or model defaults for path queries; sourceGuid optionally selects one owner. Does not clamp, wrap, evaluate morphs or extended caches, transform coordinates, or produce a Cubism pose.",
            schema(&[("path", string_schema()), ("sourceGuid", string_schema()), ("parameterValues", json!({"type":"object","additionalProperties":{"type":"number"}}))], &[])),
        tool("live2d_authoring_primary_forms", "Interpolate primary LINEAR keyform geometry and numeric attributes in their recorded coordinate spaces without changing the session. Uses active runtime values, or model defaults for path queries, unless parameterValues overrides IDs; sourceGuid optionally selects one owner. Preserves separate coordinate representations and raw rotation angles and floating draw order. Does not evaluate morphs, extended caches, deformer transforms, runtime sorting or a Cubism pose.",
            schema(&[("path", string_schema()), ("sourceGuid", string_schema()), ("parameterValues", json!({"type":"object","additionalProperties":{"type":"number"}}))], &[])),
        tool("live2d_authoring_morph_forms", "Add morph geometry, raw Glue intensity, and opacity deltas to primary LINEAR forms in their recorded coordinate spaces without changing the session. Requires an exact normal default corner and non-repeating MORPH_TARGET parameters whose range includes neutral zero. Active SNS_CURVE settings consume validated stored intermediate morph keys; extendedMorphExtensions identifies selected extensions. Inactive caches are preserved; curve regeneration and normal extended caches remain unsupported. Unique full-range constraint curves interpolate linearly and use the smallest controller limit; opacity clamps after all deltas. Supports positions, raw Glue intensity, and raw rotation angle/origin; changed draw order/colors/scale/reflection, ambiguous or partial constraint graphs and ambiguous bases are rejected. Uses active runtime values, or model defaults for path queries, unless parameterValues overrides IDs; sourceGuid optionally selects one owner. Deformer transforms are not evaluated; this is not a Cubism pose.",
            schema(&[("path", string_schema()), ("sourceGuid", string_schema()), ("parameterValues", json!({"type":"object","additionalProperties":{"type":"number"}})),
                ("clampToKeys", json!({"type":"boolean","description":"On a keyform axis of a non-repeating parameter, hold the end keyform when a value lies outside the keyed span, take a value closer than 0.001 to a key as that key (Cubism Core 6.0.1 snaps this way), and report the resulting axisValues; morph-target keys and repeating parameters are not snapped; the default rejects samples outside the span and does not snap."}))], &[])),
        tool("live2d_animation_point_delete", "Delete one existing CAN3 Image or supported Guide xy or anchor point. Requires a matching expectedRevision and unique signed anchorFrame. Preserves the channel, base point, retained points and unrelated native bytes; an empty channel remains readable. Supports only verified inline point-array layouts; shared, aliased or unknown structural storage is refused. Uses Undo/Redo and native save/reopen. Does not establish Cubism playback equivalence.",
            schema(&[("sceneGuid", string_schema()), ("trackGuid", string_schema()), ("curveId", json!({"type":"string","enum":["xy","anchor"]})),
                ("anchorFrame", json!({"type":"integer"})), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","trackGuid","curveId","anchorFrame","expectedRevision"])),
        tool("live2d_animation_point_add", "Add one CAN3 Image or supported Guide xy or anchor key with explicit frame, coordinates and corner. Requires increasing existing frames, a new signed-i32 frame and finite f32 coordinates; values round to f32. Uses a verified private inline layout, preserves retained points and base point, and refuses unknown structural metadata for insertion or corner replacement. Recreating an original frame with its original corner reconciles as a coordinate edit or no-op, preserving its original record and unknown metadata. Supports revision checks, Undo/Redo and native save/reopen. Does not establish interpolation or Cubism playback equivalence.",
            json!({"type":"object","additionalProperties":false,"properties":{
                "sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"curveId":{"type":"string","enum":["xy","anchor"]},
                "anchorFrame":{"type":"integer","minimum":-2147483648_i64,"maximum":2147483647_i64},
                "x":{"type":"number"},"y":{"type":"number"},"corner":{"type":"boolean"},
                "expectedRevision":{"type":"integer","minimum":0}},
                "required":["sceneGuid","trackGuid","curveId","anchorFrame","x","y","corner","expectedRevision"]})),
        tool("live2d_animation_point_set", "Edit both coordinates of one existing CAN3 Image or supported Guide xy or anchor point, selected by sceneGuid, trackGuid, curveId and signed anchorFrame. x and y must fit finite f32 storage; values round to f32 and equivalent values are no-ops. Updates both stored coordinate copies while preserving frame, corner, base point, other channels and unrelated bytes. Shared or inconsistent storage is refused. Uses expectedRevision, Undo/Redo and native save/reopen; no point insertion, interpolation or Cubism playback claim.",
            schema(&[("sceneGuid", string_schema()), ("trackGuid", string_schema()), ("curveId", json!({"type":"string","enum":["xy","anchor"]})),
                ("anchorFrame", json!({"type":"integer"})), ("x", number_schema()), ("y", number_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","trackGuid","curveId","anchorFrame","x","y","expectedRevision"])),
        tool("live2d_animation_key_set", "Edit one existing CAN3 parameter, modern PartOpacity, legacy-colon or underscore-map PartOpacity, or Image/Guide visual scalar anchor value with shared revision, undo/redo and native save. Image/Guide curveId is scalex, scaley, rotate, shear, opacity or artPathWidth, as listed in visualCurves; these channels reject partOpacityMapKey. Guide requires its complete supported track ownership and empty-map layout. Supported Image/Guide scalar channels also accept handle and type edits. Signed anchorFrame identifies the key; controls, key frames and unknown data stay unchanged. Anchor extrema caches are verified and updated; stored evaluation caches are invalidated. Shared scalar storage is rejected; this raw source edit does not claim Cubism playback equivalence. For underscore-map channels, optional partOpacityMapKey selects the exact raw map key and is required when curveId is ambiguous; a supplied selector never falls back.",
            schema(&[("sceneGuid", string_schema()), ("trackGuid", string_schema()), ("curveId", string_schema()), ("partOpacityMapKey", json!({"type":"string","minLength":1})),
                ("anchorFrame", json!({"type":"integer"})), ("value", number_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","trackGuid","curveId","anchorFrame","value","expectedRevision"])),
        tool("live2d_animation_key_add", "Add one CAN3 parameter, modern PartOpacity, legacy-colon or underscore-map PartOpacity, or existing Image or supported Guide visual scalar key in one undo step. expectedRevision must match the current document revision. anchorFrame is an unused signed 32-bit integer and value is finite. Omitted type defaults to LINEAR. Both controls coincide with the anchor, corner is true and positionOptimized is false. Requires supported inline sequence storage, valid extrema caches and valid affected current segments. Shared or referenced keys are refused. Image/Guide channels accept the six raw IDs listed in visualCurves and reject partOpacityMapKey; XY and integer channels are unsupported. Guide requires complete supported track ownership and consistent empty effect maps. Native save preserves unrelated XML and CAFF data. Cubism Editor acceptance is not established. For underscore-map channels, optional partOpacityMapKey selects the exact raw map key and is required when curveId is ambiguous; a supplied selector never falls back.",
            json!({"type":"object","properties":{
                "sceneGuid": {"type":"string"},
                "trackGuid": {"type":"string"},
                "curveId": {"type":"string"},
                "partOpacityMapKey": {"type":"string","minLength":1},
                "anchorFrame": {"type":"integer","minimum":-2147483648,"maximum":2147483647},
                "value": {"type":"number"},
                "type": {"type":"string","enum":["SMOOTH","BEZIER","LINEAR","STEP"],"default":"LINEAR"},
                "expectedRevision": {"type":"integer","minimum":0}},
                "required": ["sceneGuid","trackGuid","curveId","anchorFrame","value","expectedRevision"],
                "additionalProperties": false})),
        tool("live2d_animation_key_delete", "Delete the existing CAN3 parameter, modern PartOpacity, legacy-colon or underscore-map PartOpacity, or Image or supported Guide visual scalar key at anchorFrame in one undo step. expectedRevision must match the current document revision. Its paired curve type is removed. Deleting the final key leaves empty arrays and keeps the original template extrema tokens. Requires supported inline sequence storage, valid extrema caches and valid affected current segments. Shared or referenced keys are refused. Image/Guide channels accept the six raw IDs listed in visualCurves and reject partOpacityMapKey; XY and integer channels are unsupported. Guide requires complete supported track ownership and consistent empty effect maps. Native save preserves unrelated XML and CAFF data. Cubism Editor acceptance is not established. For underscore-map channels, optional partOpacityMapKey selects the exact raw map key and is required when curveId is ambiguous; a supplied selector never falls back.",
            json!({"type":"object","properties":{
                "sceneGuid": {"type":"string"},
                "trackGuid": {"type":"string"},
                "curveId": {"type":"string"},
                "partOpacityMapKey": {"type":"string","minLength":1},
                "anchorFrame": {"type":"integer"},
                "expectedRevision": {"type":"integer","minimum":0}},
                "required": ["sceneGuid","trackGuid","curveId","anchorFrame","expectedRevision"],
                "additionalProperties": false})),
        tool("live2d_animation_curve_add", "Add an empty CAN3 parameter or modern PartOpacity curve in one undo step. Requires matching expectedRevision and a CMO3 modelPath. Give exactly one of parameterId or partId. Parameter curves copy the source GUID and range; they also store the parameter's default as baseValue, and a re-added original keeps the baseValue it stores; new Part curves use the source Part GUID, typed CPartId identity and range [0,1]. Re-adding an original Part curve preserves its source identity and range. Creates an owned parts-visible effect when absent. A new parameter or modern Part curve is saved as a copy of a curve of its kind the file already stores, with the new identity, range, key and fresh xs.idx, the display name another curve of the same target carries, and the file's shared typed ID and GUID objects where it names them by reference; the synthetic layout is used only when the file has no such curve, or the track has no Part effect yet, or the curve it would copy stores no baseValue; in a track or scene added in the same save, such a curve is refused. Duplicate targets and unsupported or shared mutable storage are refused. Native save preserves unrelated XML and CAFF data. Cubism Editor acceptance is not established.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"modelPath":{"type":"string"},"parameterId":{"type":"string"},"partId":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","modelPath","expectedRevision"],"oneOf":[{"required":["parameterId"]},{"required":["partId"]}],"additionalProperties":false})),
        tool("live2d_animation_curve_delete", "Delete one CAN3 parameter, modern PartOpacity, legacy-colon PartOpacity or underscore-map PartOpacity curve in one undo step. Requires matching expectedRevision. Optional partOpacityMapKey selects an exact underscore map key and is required when curveId is ambiguous; a supplied selector never falls back. Removes the owned map entry, optional mapped member and attribute definition together; modern members remain required. A parameter curve's attribute-map entries go with it, and the shared GUID, id and type objects it names stay. The effect, shared or inline, and its registry remain. Shared, aliased, nested shared or externally referenced removed storage is refused. Other curves, unknown XML outside removed nodes and unrelated CAFF data are preserved. Cubism Editor acceptance is not established.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"curveId":{"type":"string"},"partOpacityMapKey":{"type":"string","minLength":1},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","curveId","expectedRevision"],"additionalProperties":false})),
        tool("live2d_animation_track_add", "Add an empty CAN3 model track to an existing scene using an existing linked-model resource. Requires sceneGuid, resourceGuid, name and matching expectedRevision. Returns a session snapshot plus trackGuid. The track gets a new GUID, starts at frame 0 with offset 0, and uses the scene duration. Save copies a model track the file already stores, preferably from the same scene, with its effects, a new CTrackGuid, fresh xs.idx and empty curve lists, and adds it to the scene's root group; the synthetic layout is used only when the scene has no root group or no such track. Preserves unrelated native data and provides one undo step. Does not add model resources or claim Cubism vendor acceptance.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"resourceGuid":{"type":"string"},"name":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","resourceGuid","name","expectedRevision"],"additionalProperties":false})),
        tool("live2d_animation_track_duplicate", "Duplicate an existing CAN3 Image, supported Guide or supported Sound track in its own scene and parent group. Requires sceneGuid, trackGuid, name and matching expectedRevision; returns a snapshot plus trackGuid. Copies owned attributes, keys and caches without changing the linked resource. Guide copies preserve distinct image-number attributes and never open the directory. Sound copies require one SoundDefault effect with fixed volume, clone four private definitions and preserve timing, speed, default volume and volume caches without opening audio paths. Provides undo, redo and native save. Refuses unsupported effects, shared mutable storage and unknown references. Does not create blank tracks or replace resources. No Cubism Editor acceptance is claimed.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"name":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","name","expectedRevision"],"additionalProperties":false})),
        tool("live2d_animation_track_delete", "Delete one owned CAN3 Model, Image, supported Sound or supported GuideImage track. Model tracks include owned parameter and modern, legacy-colon and underscore-map PartOpacity definitions; image tracks include their owned VisualDefault attributes. Requires sceneGuid, unique trackGuid and matching expectedRevision. Part maps use exact stored identities; mapped members may be absent or one bare reference. Refuses unowned entries, shared or nested shared storage, referenced map keys and outside references; the effect-kind ids and attribute-map keys Cubism shares across tracks stay, and a null sequence backlink is accepted for older track families. Inline unshared Part inheritance is supported. A track's parameter controls go with it: its ParamCtrlGroups effect and attributes, its marker and point GUIDs, the scene's calculation-order entries naming it, and group heads and drawable identities no other track names. Local FormEdit data with distinct shared keyform and frame definitions is supported when time-remapping links, frame caches and ownership agree; retained form and object identities stay. Empty inline keyforms with private dummy-owned remapping attributes and sparse maps of shared-root attributes are supported; their lists, caches, identities and exclusive references must agree. Foreign parameter-group frame-step attributes remain unchanged and must not reference deleted storage; same-owner attributes use ordinary ownership checks. Global form maps and nonempty inline keyforms are refused. Preserves unrelated XML and linked resources and provides one undo step. Sound deletion requires exactly one SoundDefault effect and one fixed volume attribute; its four private definitions are removed while linked resources and shared identities remain. GuideImage deletion requires the evidenced VisualDefault and GuideImageDefault layout with eleven attributes, including distinct list and named image-number attributes; fifteen private definitions are removed, preserving the linked guide-directory resource. Sound and Guide creation and playback, and Sound key/volume editing remain unsupported. Cubism vendor acceptance is not run.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"trackGuid":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","trackGuid","expectedRevision"],"additionalProperties":false})),

        tool("live2d_animation_handle_set", "Move one Bezier handle of an existing CAN3 parameter, modern PartOpacity or legacy-colon or underscore-map PartOpacity key, or supported Image/Guide visual scalar key: side previous or next, \
            frame (fractional) and value, with shared revision, undo/redo and native save. Give at least one of frame and \
            value. A smooth or Bezier segment the handle shapes must stay in frame order if it was. Save rewrites only the \
            handle's posF and doubleValue; its integer pos and isPosOptimized and every other stored field stay as they are. Image/Guide use the six visualCurves IDs and reject partOpacityMapKey; Image requires owned consistent attribute references, and Guide requires complete supported ownership. New float scalar writes must fit finite 32-bit storage. Sound controls remain unsupported. This edits stored controls without proving Image/Guide interpolation or playback equivalence. For underscore-map channels, optional partOpacityMapKey selects the exact raw map key and is required when curveId is ambiguous; a supplied selector never falls back.",
            schema(&[("sceneGuid", string_schema()), ("trackGuid", string_schema()), ("curveId", string_schema()), ("partOpacityMapKey", json!({"type":"string","minLength":1})),
                ("anchorFrame", json!({"type":"integer"})), ("side", json!({"type":"string","enum":["previous","next"]})),
                ("frame", number_schema()), ("value", number_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","trackGuid","curveId","anchorFrame","side","expectedRevision"])),
        tool("live2d_animation_curve_type_set", "Set interpolation of the CAN3 parameter, modern PartOpacity or legacy-colon or underscore-map PartOpacity, or supported Image/Guide visual scalar segment starting at an \
            existing key: SMOOTH, BEZIER, LINEAR or STEP, with shared revision, undo/redo and native save. A segment \
            set to smooth or Bezier must have its handles in frame order. The last key starts no segment, but its stored \
            type can be set. Soul2D plays SMOOTH and BEZIER the same way and does not \
            recompute handles. Image/Guide use the six visualCurves IDs and reject partOpacityMapKey; Image requires owned consistent attribute references, and Guide requires complete supported ownership. Sound types remain unsupported. This edits stored types without proving Image/Guide interpolation or playback equivalence. For underscore-map channels, optional partOpacityMapKey selects the exact raw map key and is required when curveId is ambiguous; a supplied selector never falls back.",
            schema(&[("sceneGuid", string_schema()), ("trackGuid", string_schema()), ("curveId", string_schema()), ("partOpacityMapKey", json!({"type":"string","minLength":1})),
                ("anchorFrame", json!({"type":"integer"})),
                ("type", json!({"type":"string","enum":["SMOOTH","BEZIER","LINEAR","STEP"]})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","trackGuid","curveId","anchorFrame","type","expectedRevision"])),
        tool("live2d_animation_resource_add", "Link a CMO3 model to the active CAN3 document as a new resource, the first half of placing a model on the animation canvas; live2d_animation_track_add then places it in a scene. Requires modelPath (a readable CMO3) and matching expectedRevision; the reply is the snapshot with resourceGuid. Save copies the file's own resource entry with a new CResourceGuid, the file name and the absolute model path, and lists it in the resource manager's reference list, GUID map and root group. A file without a resource entry in Cubism's layout is refused. One undo step. Cubism Editor acceptance is not run.",
            schema(&[("modelPath", string_schema()), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["modelPath","expectedRevision"])),
        tool("live2d_animation_image_resource_add", "Link a static PNG or JPEG to the active CAN3 as a new Image resource. Requires imagePath and matching expectedRevision; returns snapshot plus resourceGuid. Validates bounded complete image data, then stores its canonical path and basename using an existing owned Image resource as donor. The image is not embedded and no track is created. Mixed pending Model and Image resources share one save. Unknown references, ambiguous memberships and unsupported custom names are refused. One undo step; no Cubism Editor acceptance is claimed.",
            json!({"type":"object","properties":{"imagePath":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["imagePath","expectedRevision"],"additionalProperties":false})),
        tool("live2d_animation_image_resource_relink", "Replace only an existing Image resource's stored link with the canonical path of a validated static PNG or JPEG. All tracks sharing resourceGuid use that link; names, transforms, keys and memberships remain unchanged. Requires an editable stored path and matching expectedRevision. One undo step; a matching path is a no-op. No resizing, rendering or Cubism Editor equivalence is claimed.",
            json!({"type":"object","properties":{"resourceGuid":{"type":"string"},"imagePath":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["resourceGuid","imagePath","expectedRevision"],"additionalProperties":false})),
        tool("live2d_animation_scene_set", "Change the name, frame rate, length in frames, loop setting or tag of one scene in the active CAN3 document, with shared revision, undo/redo and native save. Give at least one of name, fps, durationFrames, loop and tag. loop is the scene's stored isLoopMotion flag, shown as scene.loop only where the scene stores one; elsewhere it is refused. tag is the scene's stored tag text, shown as scene.tag only where the scene stores one; an empty tag clears it, and elsewhere it is refused. It does not change how Soul2D plays the scene. Save rewrites only those elements; settings stored shared or aliased are refused. Tracks and keys are not moved or cut to the new length.",
            schema(&[("sceneGuid", string_schema()), ("name", string_schema()), ("fps", number_schema()),
                ("durationFrames", json!({"type":"integer","minimum":0})), ("loop", json!({"type":"boolean"})),
                ("tag", string_schema()), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sceneGuid","expectedRevision"])),
        tool("live2d_animation_scene_add", "Add an empty scene with a new random GUID at the end of the active CAN3 document's scenes, with shared revision, undo/redo and native save. The reply is the snapshot with sceneGuid. Where the document's last scene shows scene.tag, the new scene takes a copy of it, shown as scene.tag; live2d_animation_scene_set clears it with an empty tag. Save copies the last scene the file stores in Cubism's layout, with its root group, a new CSceneGuid, fresh xs.idx, the new name, frame rate, length and tag, loop off, and the workspace over the whole scene as the copied scene sets it; tracks added to the new scene are copied like added tracks. A file without such a scene gets a scene holding only its GUID, name, frame rate, length, the tag where one is shown, and an empty track list. Cubism Editor 5.3.04 opened a copied scene with an added track, curve and keys on 2026-10-20; it has not opened the minimal scene.",
            schema(&[("name", string_schema()), ("fps", number_schema()),
                ("durationFrames", json!({"type":"integer","minimum":0})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["name","fps","durationFrames","expectedRevision"])),
        tool("live2d_animation_bindings", "Bind CAN3 parameter and PartOpacity channels of one linked-model resource to one explicit CMO3 without changing the active session. Parameters use exact parameter GUID and live2dParam_ attribute ID. Typed source parameter IDs are reported as matchesId/differentId, legacy s labels as legacyText, and empty source maps as null/missing after exact matching. Part GUIDs supply canonical Part IDs; raw source identity remains metadata. Underscore channels report exact mapKey with scene/track/curveId in underscorePartBindings or unboundUnderscorePartCurves; duplicate attribute IDs do not merge channels. Does not verify stored link provenance, play animation, or establish Cubism equivalence.",
            json!({"type":"object","properties":{
                "modelPath":{"type":"string"},
                "resourceGuid":{"type":"string"},
                "path":{"type":"string"}},
                "required":["modelPath","resourceGuid"],
                "additionalProperties":false})),
        tool("live2d_animation_scene_delete", "Delete one owned CAN3 scene and its supported Model, Image, Sound and Guide tracks in one undo step. Model tracks include parameter curves and modern, legacy-colon and underscore-map PartOpacity definitions; Image, Sound and Guide tracks use the same ownership checks as track deletion. Requires sceneGuid and matching expectedRevision. Refuses shared, aliased, externally referenced or unsupported track storage and unowned Part map/list entries. Inline unshared Part inheritance and tracks with parameter controls are supported; nested shared storage is refused. Local FormEdit data uses the same strict shared-keyform ownership checks as track deletion; unsupported form layouts are refused. A scene's shared GUID and the playlist items that name it go with it, and the animation's current scene and the resource manager's scene move to the next kept scene, else the previous. The last scene may be deleted only when no stored scene pointer needs a surviving target. Linked model, image, sound and guide-directory resources and shared identities remain. Guide tracks use the same strict layout checks as track deletion. Unsupported members are refused. Preserves unrelated native data. Cubism vendor acceptance is not run.",
            json!({"type":"object","properties":{"sceneGuid":{"type":"string"},"expectedRevision":{"type":"integer","minimum":0}},"required":["sceneGuid","expectedRevision"],"additionalProperties":false})),

        tool("live2d_form_set_vertex", "Edit one primary ArtMesh or Warp form vertex in its recorded coordinate space in the active CMO3 document, with shared revision and undo/redo. Requires the current expectedRevision and exact owner/form/coordinate identity. Unsupported shared storage or derived caches are rejected; this does not evaluate a canvas pose.",
            schema(&[("sourceGuid", string_schema()), ("formGuid", string_schema()),
                ("coordinateType", string_schema()),
                ("vertexIndex", json!({"type":"integer","minimum":0})),
                ("position", json!({"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","formGuid","coordinateType","vertexIndex","position","expectedRevision"])),
        tool("live2d_runtime_inspect", "Read a Live2D runtime model without changing the session: a .model3.json (its \
             file references, groups and hit areas, and the .moc3 it names, which must stay inside its folder) or a \
             .moc3 alone. Answers version, canvas {pixelsPerUnit, origin, size}, parts (IDs), deformers {ids, warp, \
             rotation}, parameters [{id, min, max, default}], drawables [{id, parentPart, texture, vertices, \
             triangles}] and, for a .model3.json, model3. Soul2D's own clean-room reader built from the structure of \
             sample files (ADR 0004): it reads .moc3 versions 1, 2, 3, 5 and 6 in the little-endian layout and refuses \
             anything else rather than guess. Keyforms and deformer geometry are not read.",
            schema(&[("path", string_schema())], &["path"])),
        tool("live2d_cdi3_export", "Return the display information of the active CMO3 document in the .cdi3.json layout: IDs, names and groups of parameters, parameter groups, Parts, and the parameter pairs shown as one two-axis control. With path, also create that JSON atomically at an unused destination. History is unchanged.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_physics3_export", "Return the physics settings of the active CMO3 document in the .physics3.json layout: inputs, outputs, pendulum vertices and normalization of each setting, numbered PhysicsSetting1..N in document order, with numbers rounded to three decimals. With path, also create that JSON atomically at an unused destination. History is unchanged.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_convert_inx", "Convert the active CMO3 document to an INX model: the default pose becomes the node tree with the CMO3 textures, and every parameter becomes a one-axis INX parameter whose keys hold the vertex offsets of the pose at that key, so each single-parameter key looks the same. Lossy: warp and rotation deformers, Glue and blend shapes are baked into those offsets, combined poses add in INX, and physics, blend-shape constraints and CAN3 animations are not converted unless animation names a CAN3 file; the answer lists these counts under baked and notConverted. Keyed ArtMesh and offscreen-Part draw order and opacity become zSort and opacity bindings of the same parameter (a node whose opacity is keyed by several parameters multiplies them). With animation, every scene of that CAN3 that drives the model (resourceGuid picks the model resource when the CAN3 has several) becomes an INX animation of frame-exact Linear keyframes sampled by the CAN3 player, with Bezier handles baked. Each Part with opacity curves becomes a 0..1 \"<Part> opacity\" parameter whose opacity bindings reach the nodes the Part fades, driven by one lane per scene; a Part that fades no drawn node is counted as not converted, and so is a Part when a node it fades already has an opacity binding and is drawn at opacity 0 at the default pose. Between keys the INX draw order interpolates linearly where the CMO3 rounds to whole draw orders, so ties between meshes can resolve differently mid-key. With path, also create the INX atomically at an unused destination. History is unchanged.",
            schema(&[("path", string_schema()), ("animation", string_schema()), ("resourceGuid", string_schema())], &[])),
        tool("live2d_physics3_read", "Return the typed full-precision view of the physics settings stored in the active CMO3 document: per-group name, normalization ranges, inputs, outputs and pendulum vertices in document order, with a shared/editable flag on each. Groups have no stable ID; indexes are positions under the current revision. History is unchanged.",
            schema(&[], &[])),
        tool("live2d_physics3_edit", "Apply one typed edit to the physics settings stored in the active CMO3 document, with shared revision and undo/redo. Requires the current expectedRevision. Address a group by its document-order index from live2d_physics3_read. Set a group field with field=name or normalization.position.min|default|max (or .angle.*) plus value; set the physics rate with field=fps and no group (value null clears it); edit one list item with group, list=inputs|outputs|vertices, index, field and value (fields: source/destination parameter ID, type X|Angle on inputs, weight 0-100, reflect, scale, vertexIndex, x, y, mobility 0-1, delay, acceleration, radius); remove with group+remove or group+list+index+remove; insert with group+list+insert+value (object of item fields; vertices need x,y,mobility,delay,acceleration,radius, inputs need source+type, outputs need destination+vertexIndex; inserting a vertex at index 0 of a nonempty strand is rejected, the strand root stays fixed). group+duplicate=true appends a copy with new GUIDs and the next PhysicsSetting<n> ID. To create the first group in an existing empty inline settings list, omit group/list/index/field and use insert:0 plus value:{name,normalization:{position:{min,default,max},angle:{min,default,max}},inputs:[],outputs:[],vertices:[]}; supply all fields and use the same item shapes for populated arrays. Creation gives the group a fresh GUID and selects it. Duplication preserves selection; removing the selected group selects the first survivor with a unique GUID, or clears selection when none remain. Unsupported selection storage, ambiguous/missing survivor identities and opaque outside group-GUID links refuse atomically. Missing/null/shared sets and nonempty lists refuse donor-free creation. Inserted items use the known inline layout, and parameter fields use the file's own form (a reference to its shared GUID object when it keeps one). Ordinary edits reject aliased or shared storage. With detach:true and only expectedRevision, explicitly copy the complete known physics ownership tree into private inline storage in one undo step; original shared definitions, parameter UUIDs and existing parameter-GUID references, unknown markup and numeric spelling are preserved. The selected-group GUID follows its copied group; ambiguous or missing selection and opaque physics-GUID links refuse atomically. Already private settings are unchanged. Mutable ownership uses direct known fields and one-hop aliases; parameter-GUID reference chains are kept unchanged; inherited fields, conflicting alias metadata, opaque cross-references into copied ownership, cycles and copies exceeding 16 MiB or 500000 nodes refuse atomically. A failed edit changes nothing. With group, list=inputs|vertices and preset (the name of a preset kept by live2d_physics3_preset_save) the list of that group is made the preset: matching items are set field by field, missing items are inserted at the end and surplus items are removed from the end, in one undo step; no other field is allowed with preset, and a step the edit would refuse (an unknown parameter, shared storage, an output that names a removed vertex) refuses the whole call.",
            schema(&[("group", json!({"type":"integer","minimum":0})),
                ("list", json!({"type":"string","enum":["inputs","outputs","vertices"]})),
                ("index", json!({"type":"integer","minimum":0})),
                ("insert", json!({"type":"integer","minimum":0})),
                ("remove", json!({"type":"boolean"})),
                ("duplicate", json!({"type":"boolean"})),
                ("detach", json!({"type":"boolean"})),
                ("field", string_schema()),
                ("value", json!({})),
                ("preset", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["expectedRevision"])),
        tool("live2d_physics3_preset_save", "Keep the inputs of a physics group, or its physical model (the pendulum vertices), under name, for live2d_physics3_edit to use as preset. list is inputs or vertices. Give group, the index from live2d_physics3_read, to keep the list that group holds now (the active CMO3 document is read, nothing else changes), or items, which work with any document open: for inputs [{source: parameter ID, type X|Angle, weight 0-100, reflect, scale?}], for vertices [{x, y, mobility 0-1, delay, acceleration, radius}] with at least two vertices and a radius of 0 only on the first. An input keeps the parameter by its ID, not its GUID, and an output is no part of either kind. The presets belong to the user of this machine, not to the document: they are the files physics3-input-presets.json and physics3-model-presets.json in the settings directory, and the call changes no document and adds no undo step. A saved name is replaced only with overwrite:true, and the answer then holds replaced:true. Refused: group and items together or neither, a key a preset cannot keep, a name that is not 1 to 64 characters, has a control character anywhere or white space at either end, a 65th preset of a kind, presets that would make a file larger than 1 MiB, and a host without a settings directory. There are no built-in presets.",
            schema(&[("list", json!({"type":"string","enum":["inputs","vertices"]})),
                ("name", string_schema()),
                ("group", json!({"type":"integer","minimum":0})),
                ("items", json!({"type":"array","items":{"type":"object"}})),
                ("overwrite", json!({"type":"boolean"}))],
                &["list", "name"])),
        tool("live2d_physics3_preset_delete", "Delete a preset that live2d_physics3_preset_save kept, of the kind list names (inputs or vertices). A name that is not saved is refused.",
            schema(&[("list", json!({"type":"string","enum":["inputs","vertices"]})), ("name", string_schema())],
                &["list", "name"])),
        tool("live2d_physics3_preset_list", "List the presets live2d_physics3_preset_save kept, as {inputs: {name: {items}}, vertices: {name: {items}}}. The resource resource://soul2d/physics3/presets returns the same. The three preset tools answer with any document open; only a save with group needs the active CMO3.",
            schema(&[], &[])),
        tool("live2d_userdata3_export", "Return the ArtMesh user data of the active CMO3 document in the .userdata3.json layout: one row {Target: ArtMesh, Id, Value} per ArtMesh with user data, in document order, and Meta with UserDataCount and TotalUserDataSize (UTF-8 bytes). With path, also create that JSON atomically at an unused destination. History is unchanged.",
            schema(&[("path", string_schema())], &[])),
        tool("live2d_user_data_set", "Set the user data text of one ArtMesh in the active CMO3 document, with shared revision and undo/redo; an empty value clears it. guid is the ArtMesh GUID from live2d_authoring_read, whose sources[].userData shows the text. Requires the current expectedRevision. At most 65536 bytes; tab and line feed are the only control characters allowed. An ArtMesh whose file has no inline userData field is rejected.",
            schema(&[("guid", string_schema()), ("value", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","value","expectedRevision"])),
        tool("live2d_rename", "Set the display name of one ArtMesh, ArtPath, Part, deformer or Glue (its localName), of one parameter or of one ArtPath brush in the active CMO3 document, with shared revision and undo/redo. guid is the source, parameter or brush GUID from live2d_authoring_read; the ID string is not changed. Requires the current expectedRevision. A name stored as a shared string, or an object without a name field, is rejected.",
            schema(&[("guid", string_schema()), ("name", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","name","expectedRevision"])),
        tool("live2d_form_set_opacity", "Edit the explicit opacity scalar of one primary ArtMesh or ArtPath form in the active CMO3 document, with shared revision and undo/redo. Requires the current expectedRevision and exact owner/form/coordinate identity. Values must fit the native finite float field; authored values are preserved without clamping. ArtPath requires one existing private literal float through private inline form inheritance; missing, aliased, shared, shadowed or nonliteral storage is refused. Unsupported derived forms are rejected. Stored editing does not establish ArtPath rendering or vendor acceptance.",
            schema(&[("sourceGuid", string_schema()), ("formGuid", string_schema()),
                ("coordinateType", string_schema()), ("opacity", number_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","formGuid","coordinateType","opacity","expectedRevision"])),
        tool("binding_multi_edit", "Multiple Keys Editing: every set key of every binding of nodes on channel \
             (opacity, zSort, tint or screenTint) in parameters (default all) becomes value (method overwrite, the \
             default), itself plus value (add) or, for tint and screenTint, itself multiplied or screened by value. \
             value is one number, or [r,g,b] for a colour. Opacity and colours are clamped to 0..1. Result \
             {changed}: the number of keys changed. One undo step; nothing changes when no key matches.",
            schema(&[("nodes", node_ids_schema()),
                ("channel", json!({"type":"string","enum":["opacity","zSort","tint","screenTint"]})),
                ("value", json!({"oneOf":[number_schema(), {"type":"array","items":number_schema(),"minItems":3,"maxItems":3}]})),
                ("method", json!({"type":"string","enum":["overwrite","add","multiply","screen"]})),
                ("parameters", json!({"type":"array","items":{"type":"integer","minimum":1}}))],
                &["nodes","channel","value"])),
        tool("binding_weight_limits_set", "Blend-shape weight limits: the deform binding of node on parameter is \
             scaled by the smallest weight its limits give. Each limit is {parameter, points: [[value, weight], ...]}, \
             2 to 5 points with rising values and weights in 0..1 over another parameter's first axis, linear between \
             points and flat outside them. An empty limits list removes them. One undo step.",
            schema(&[("parameter", integer_schema()), ("node", integer_schema()),
                ("limits", json!({"type":"array","items":{"type":"object","properties":{
                    "parameter": integer_schema(),
                    "points": {"type":"array","minItems":2,"maxItems":5,"items":pair_schema()}},
                    "required":["parameter","points"]}}))],
                &["parameter","node","limits"])),
        tool("live2d_form_paste", "Paste Form / Blend Form on one ArtMesh or Warp source of the active CMO3: form \
             formGuid becomes itself + ratio × (fromFormGuid − itself), vertex by vertex, and so does an ArtMesh \
             form's opacity. ratio defaults to 1 (a plain paste). invert vertical|horizontal mirrors the copied \
             shape about that line through the centre of its bounds first, and properties [vertices, opacity] \
             limits what is pasted (Paste Form Special; default both). Both forms are \
             primary forms of the source in coordinateType. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("formGuid", string_schema()),
                ("coordinateType", string_schema()), ("fromFormGuid", string_schema()),
                ("ratio", json!({"type":"number","minimum":0,"maximum":1})),
                ("invert", json!({"type":"string","enum":["vertical","horizontal"]})),
                ("properties", json!({"type":"array","items":{"type":"string","enum":["vertices","opacity"]}})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","formGuid","coordinateType","fromFormGuid","expectedRevision"])),
        tool("live2d_form_set_opacities", "Multiple key editing of ArtMesh or existing private primary ArtPath opacity in the active CMO3: every form in \
             forms [{sourceGuid, formGuid, coordinateType}] gets opacity (method overwrite, the default) or its own \
             opacity plus opacity clamped to 0..1 (method add). Repeated forms apply sequentially. Every form must \
             pass the single-opacity setter and storage checks; any refusal leaves the entire edit unchanged. \
             One undo step; needs the current expectedRevision. Stored editing does not establish ArtPath rendering or vendor acceptance.",
            schema(&[("forms", json!({"type":"array","minItems":1,"items":{"type":"object","properties":{
                    "sourceGuid": string_schema(), "formGuid": string_schema(), "coordinateType": string_schema()},
                    "required":["sourceGuid","formGuid","coordinateType"]}})),
                ("opacity", number_schema()), ("method", json!({"type":"string","enum":["overwrite","add"]})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["forms","opacity","expectedRevision"])),
        tool("live2d_form_set", "Edit the stored settings of one primary form in the active CMO3: drawOrder (integer \
             0..1000) of an ArtMesh or ArtPath form, multiplyColor or screenColor ([r,g,b] or [r,g,b,a] in 0..1) of \
             an ArtMesh or Part form, angle, origin [x,y], scale and reflectX/reflectY of a rotation deformer form, intensity \
             (0..1) of a Glue form. ArtPath drawOrder requires one existing private literal integer through private inline \
             form inheritance; missing, aliased, shared, shadowed or nonliteral storage is refused. It changes stored \
             order, not ArtPath rendering. Every key is optional; other fields the file does not store are copied from the same field \
             elsewhere in the file, and refused when the file stores it nowhere. \
             coordinateType is omitted for Part and Glue forms. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("formGuid", string_schema()), ("coordinateType", string_schema()),
                ("drawOrder", json!({"type":"integer","minimum":0,"maximum":1000})),
                ("multiplyColor", json!({"type":"array","items":number_schema(),"minItems":3,"maxItems":4})),
                ("screenColor", json!({"type":"array","items":number_schema(),"minItems":3,"maxItems":4})),
                ("angle", number_schema()), ("origin", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("scale", number_schema()), ("reflectX", json!({"type":"boolean"})), ("reflectY", json!({"type":"boolean"})),
                ("intensity", number_schema()), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","formGuid","expectedRevision"])),
        tool("live2d_source_set", "Edit the stored settings of one source in the active CMO3: an ArtMesh or Part's \
             blend mode as colorComposition (NORMAL, ADD, ADD_R2, ADD_GLOW, DARKEN, MULTIPLY_R2, COLOR_BURN, \
             LINEAR_BURN, LIGHTEN, SCREEN, COLOR_DODGE, OVERLAY, SOFTLIGHT, HARDLIGHT, LINEAR_LIGHT, HSL_HUE, HSL_COLOR) \
             and alphaComposition (OVER, ATOP, OUT, CONJOINT, DISJOINT). invertClippingMask applies to ArtMesh, Part \
             and ArtPath; an ArtPath requires one existing private plain boolean in its inline source inheritance, \
             without shared, referenced or shadowed storage. useOffscreen applies to a Part. clipGuids lists \
             the clipping mask ArtMeshes of an ArtMesh, Part or ArtPath (kept masks keep their \
             order, new ones go last). closed and divisionCount edit an ArtPath's existing private direct boolean \
             and unsigned 32-bit integer. brushGuid selects exactly one existing listed brush, changing only the \
             ArtPath's private GUID slot. Missing, shared or ambiguous storage is refused. These are stored settings; \
             ArtPath rendering and vendor acceptance are separate. \
             Other scalar fields the file does not store are copied from the same field elsewhere in the \
             file (refused when there is none); a mask list the source does not store is copied, emptied, from another \
             source of the same kind (refused when none stores one). One undo step; needs the current \
             expectedRevision.",
            schema(&[("guid", string_schema()), ("colorComposition", string_schema()), ("alphaComposition", string_schema()),
                ("invertClippingMask", json!({"type":"boolean"})), ("useOffscreen", json!({"type":"boolean"})),
                ("closed", json!({"type":"boolean"})),
                ("divisionCount", json!({"type":"integer","minimum":0,"maximum":4294967295_u64})),
                ("brushGuid", string_schema()),
                ("clipGuids", json!({"type":"array","items":string_schema()})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","expectedRevision"])),
        tool("live2d_brush_replace_image", "Replace the existing shared image of an ArtPath brush of the active CMO3 \
             with a static PNG file at path. The PNG dimensions must match the existing image and resource metadata. \
             Every brush and mesh referencing that image entry changes; the reply reports affectedBrushes and \
             affectedTextures. Image bytes are preserved without premultiplication. Identical bytes are a no-op; \
             otherwise one undo step. No resizing, image selection or new image entries. Requires expectedRevision.",
            schema(&[("guid", string_schema()), ("path", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))], &["guid", "path", "expectedRevision"])),
        tool("live2d_brush_set", "Edit the stored drawing settings of one ArtPath brush of the active CMO3 (guid \
             from the brushes of live2d_authoring_read): drawingCurveType (SPRITE or TIP_USE, the types the \
             supplied files store), widthCoefficient, and the numbers of textureUvData (up, upInter, center, \
             downInter, down, inStart, repeatStart, repeatEnd, outEnd), fixWidthData (upFixedWidth, downFixedWidth) \
             and spriteData (density, useTextureMagnification), plus the six stored affine coefficients under \
             textureInput.currentTextureInputData.optionalTransformOnCanvas. Use live2d_brush_replace_image for image bytes. \
             A group names only the numbers it changes. Each \
             change rewrites its one stored token; a setting the file does not store for the brush is refused. One \
             undo step; needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("drawingCurveType", json!({"type":"string","enum":["SPRITE","TIP_USE"]})),
                ("widthCoefficient", number_schema()),
                ("textureInput", json!({"type":"object","additionalProperties":false,"properties":{
                    "currentTextureInputData":{"type":"object","additionalProperties":false,"properties":{
                        "optionalTransformOnCanvas":{"type":"object","additionalProperties":false,"properties":{
                            "m00":number_schema(),"m01":number_schema(),"m02":number_schema(),
                            "m10":number_schema(),"m11":number_schema(),"m12":number_schema()}}}}}})),
                ("textureUvData", json!({"type":"object","additionalProperties":false,"properties":{
                    "up":number_schema(),"upInter":number_schema(),"center":number_schema(),"downInter":number_schema(),
                    "down":number_schema(),"inStart":number_schema(),"repeatStart":number_schema(),
                    "repeatEnd":number_schema(),"outEnd":number_schema()}})),
                ("fixWidthData", json!({"type":"object","additionalProperties":false,"properties":{
                    "upFixedWidth":number_schema(),"downFixedWidth":number_schema()}})),
                ("spriteData", json!({"type":"object","additionalProperties":false,"properties":{
                    "density":number_schema(),"useTextureMagnification":number_schema()}})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","expectedRevision"])),
        tool("live2d_artpath_point_set", "Edit the stored values of control point index of one ArtPath form in the \
             active CMO3 (stroke editing): position, startVelocity and endVelocity as [x,y], width (0 or more), \
             opacity (0..1), color ([r,g,b] or [r,g,b,a] in 0..1) and corner. Every key is optional. ArtPaths are \
             read and edited, not drawn. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("formGuid", string_schema()), ("coordinateType", string_schema()),
                ("index", json!({"type":"integer","minimum":0})),
                ("position", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("startVelocity", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("endVelocity", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("width", number_schema()), ("opacity", number_schema()),
                ("color", json!({"type":"array","items":number_schema(),"minItems":3,"maxItems":4})),
                ("corner", json!({"type":"boolean"})), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","formGuid","coordinateType","index","expectedRevision"])),
        tool("live2d_artpath_point_add", "Insert a control point at index (0 to the number of points) into every \
             keyform of one ArtPath of the active CMO3 (stroke editing). The new point starts as a copy of the point \
             it goes before (the last point when appended), and the values given here replace that copy's in every \
             form: position, startVelocity and endVelocity as [x,y], width (0 or more), opacity (0..1), color \
             ([r,g,b] or [r,g,b,a] in 0..1) and corner. Every value is optional. No midpoint is computed. An \
             ArtPath whose forms hold different numbers of points is refused. ArtPaths are read and edited, not \
             drawn. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("index", json!({"type":"integer","minimum":0})),
                ("position", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("startVelocity", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("endVelocity", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
                ("width", number_schema()), ("opacity", number_schema()),
                ("color", json!({"type":"array","items":number_schema(),"minItems":3,"maxItems":4})),
                ("corner", json!({"type":"boolean"})), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","index","expectedRevision"])),
        tool("live2d_artpath_point_delete", "Delete control point index from every keyform of one ArtPath of the \
             active CMO3. The last point of an ArtPath stays, a point the file names from elsewhere is refused, and \
             so is an ArtPath whose forms hold different numbers of points. One undo step; needs the current \
             expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("index", json!({"type":"integer","minimum":0})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","index","expectedRevision"])),
        tool("live2d_reparent", "Move one source of the active CMO3 (ArtMesh, deformer, Part, Glue or ArtPath, by \
             guid) under another Part (parent, a Part GUID), at the end of its children. Its deformer and vertices do \
             not change. The root Part, a move under itself or its own descendant, and a file whose GUID entries are \
             shared definitions are refused. One undo step; needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("parent", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","parent","expectedRevision"])),
        tool("live2d_reparent_deformer", "Reattach one static ArtMesh of the active CMO3 to an existing static Rotation \
             chain while preserving canvas coordinates within 1e-6 per coordinate. Source-local round-trip \
             precision loss above 1e-6 is refused. Requires sourceGuid, \
             targetDeformerGuid and the current expectedRevision. Changes only its target and sole DeformerLocal \
             form positions; Part membership, opacity, topology, UVs and material stay unchanged. Root rotations \
             must use Canvas forms, nested rotations DeformerLocal, with neutral opacity and positive individual \
             and complete-chain scale at least 1e-6. Keyed, morph, extended, reflected, Warp, Glue-bound, aliased \
             or numerically unsafe geometry is refused. Root/detach destinations are unsupported. One undo step; \
             the same target is a no-op after revision and identity checks. Read back with live2d_authoring_read \
             or the model in resource://vsoul/snapshot. This bounded source edit does not establish vendor parity.",
            closed_schema(&[("sourceGuid", string_schema()), ("targetDeformerGuid", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","targetDeformerGuid","expectedRevision"])),
        tool("live2d_parameter_edit", "Change the stored minimum, maximum, default and repeat flag of one parameter of \
             the active CMO3 (by guid); each key is optional. The default must lie in the range, and so must every \
             key a source has on the parameter. A field the file does not store is copied from another parameter. One \
             undo step; needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("minimum", json!({"type":"number"})), ("maximum", json!({"type":"number"})),
                ("default", json!({"type":"number"})), ("repeat", json!({"type":"boolean"})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))], &["guid","expectedRevision"])),
        tool("live2d_keyform_add", "Add a key to one ArtMesh or warp deformer of the active CMO3 (sourceGuid) on its \
             axis for one parameter (parameterGuid). key lies in the parameter's range and is not a key yet. Each new \
             form takes the vertices and opacity interpolated linearly between its neighbours on the axis (or those \
             of the end form) and is written as a copy of a form the file stores. Sources with blend shapes or extended \
             interpolation forms are refused. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("parameterGuid", string_schema()), ("key", json!({"type":"number"})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","parameterGuid","key","expectedRevision"])),
        tool("live2d_keyform_delete", "Delete one key, and the forms on it, from the axis of one ArtMesh or warp \
             deformer of the active CMO3 (sourceGuid) for one parameter (parameterGuid). The last key of an axis \
             stays. One undo step; needs the current expectedRevision.",
            schema(&[("sourceGuid", string_schema()), ("parameterGuid", string_schema()), ("key", json!({"type":"number"})),
                ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["sourceGuid","parameterGuid","key","expectedRevision"])),
        tool("live2d_duplicate", "Copy one ArtMesh, ArtPath, deformer, Glue or empty Part of the active CMO3 (by guid) \
             under the same parent, after the last source of its kind, or one parameter that no source keys and the \
             physics settings do not use, at the end and in its group. The copy is the original's own XML with new \
             identifiers. An ArtPath brush can also be copied at the end of the brush list, with its current \
             settings and texture metadata but without duplicating image bytes. A texture setting embedded in \
             another model object is refused. \
             It keeps the name and takes id (default <id>_copy). A source the file names from outside the \
             sources, for example from a registry, is refused. The reply carries the copy's guid. One undo step; \
             needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("id", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))], &["guid","expectedRevision"])),
        tool("live2d_set_id", "Change the ID string of one ArtMesh, Part, deformer, Glue or ArtPath, of one \
             parameter or of one ArtPath brush, in the active CMO3 (by guid). id is 1 to 63 ASCII letters, digits or \
             underscores and unused among the sources (or the parameters, or the brushes). A parameter's keyform axes and morph weights follow; motions, \
             expressions and physics files that name the old ID are not changed. One undo step; needs the current \
             expectedRevision.",
            schema(&[("guid", string_schema()), ("id", string_schema()),
                ("expectedRevision", json!({"type":"integer","minimum":0}))], &["guid","id","expectedRevision"])),
        tool("live2d_delete", "Delete one source of the active CMO3 (ArtMesh, deformer, empty Part, Glue or ArtPath, \
             by guid) that no other source names as parent, deformer, clipping mask or Glue target; its texture goes \
             too when no other ArtMesh uses it, and so do the objects only it uses (its GUID object, extensions, \
             keyform grid) and a Cubism 2.1 conversion record that names it. The root Part, a Part with children and \
             a source the file names from anywhere else are refused, and the message says where. \
             An unused ArtPath brush can also be deleted; any remaining source or XML reference refuses deletion. \
             One undo step; needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","expectedRevision"])),
        tool("live2d_parameter_delete", "Delete one parameter of the active CMO3 (by guid) that no source keys and the \
             physics settings do not use, with its places in parameter groups and the objects only it uses. A \
             parameter the file names from anywhere else is refused, and the message says where. Motions that use \
             its ID are not changed. One undo step; needs the current expectedRevision.",
            schema(&[("guid", string_schema()), ("expectedRevision", json!({"type":"integer","minimum":0}))],
                &["guid","expectedRevision"])),
        tool("live2d_parameter_set", "Set one runtime parameter of the active Live2D document within its authored range. A value outside the range is refused, or wraps into it when the parameter repeats. Changes subsequent active authoring queries, without changing saved authoring data, revision or undo/redo. This does not render a Cubism pose.",
            schema(&[("parameterId", string_schema()), ("value", number_schema())], &["parameterId", "value"])),
        tool("motion3_list", "List imported Motion3 data without changing runtime playback.", schema(&[], &[])),
        tool("motion3_read", "Read imported Motion3 JSON from the INX document or CMO3 session-only clip library.",
            schema(&[("name", string_schema())], &["name"])),
        tool("motion3_sample", "Evaluate raw Motion3 curves at seconds, without loop, fades, runtime blending or pose changes.",
            schema(&[("name", string_schema()), ("time", number_schema())], &["name", "time"])),
        tool("motion3_point_set", "Edit a zero-based curve/point in one undo. Time edits normalize restricted Bezier handle times to preserve other curves, and extend Duration when needed. Times are seconds.",
            schema(&[("name", string_schema()), ("curve", json!({"type":"integer","minimum":0})),
                ("point", json!({"type":"integer","minimum":0})), ("time", number_schema()),
                ("value", number_schema())], &["name", "curve", "point"])),
        tool("motion3_fade_set", "Set the fade-in and fade-out seconds of a Motion3 motion (Meta) or, with curve, of one \
            curve, which then overrides the motion's. A number sets the time, null removes it, an absent key stays. An \
            INX motion is edited in one undo; a motion on a CMO3 lives in this session. Playback uses them only when \
            motion3_seek has fades.",
            schema(&[("name", string_schema()), ("curve", json!({"type":["integer","null"],"minimum":0})),
                ("fadeIn", json!({"type":["number","null"],"minimum":0})),
                ("fadeOut", json!({"type":["number","null"],"minimum":0}))], &["name"])),
        tool("motion3_pose_switch", "Key a pose switch in a Motion3 motion on the active CMO3: at time (seconds), the \
            PartOpacity curve of part is keyed 1 and those of the other parts in its group of the attached Pose3 are keyed \
            0. A key within half a frame of an anchor sets it, any other key is joined by stepped segments, and a key \
            touching a segment that is not stepped is refused for the whole group. A missing curve holds its key from 0, so \
            key the starting part at 0 first. The motion lives in this session like the import (a playing preview shows \
            the edit at the next seek or advance); read it with motion3_read and export it with motion3_export.",
            schema(&[("name", string_schema()), ("part", string_schema()),
                ("time", json!({"type":"number","minimum":0}))], &["name", "part", "time"])),
        tool("motion3_export", "Atomically create edited Motion3 JSON at an unused destination, without changing history. \
             With runtimeModel (a .model3.json or .moc3), the export is refused unless every Parameter and \
             PartOpacity curve names an ID that runtime model has; the reply then carries checkedCurves.",
            schema(&[("name", string_schema()), ("path", string_schema()), ("runtimeModel", string_schema())],
                &["name", "path"])),
        tool("motion3_bind", "Persist explicit Motion3 Parameter ID to INX UUID/axis bindings in one undo. Empty bindings clear the mapping; raw motion data stays unchanged.",
            schema(&[("name", string_schema()), ("bindings", json!({"type":"array","items":{
                "type":"object","properties":{"id":{"type":"string"},"parameter":{"type":"integer","minimum":1},
                "axis":{"type":"integer","minimum":0,"maximum":1}},"required":["id","parameter","axis"]}})),
                ("preserve_unknown", json!({"type":"boolean","description":"Keep existing unknown fields for matching IDs; edit only id/parameter/axis. Defaults to false."}))], &["name", "bindings"])),
        tool("motion3_bindings", "Read persisted Motion3 parameter bindings.", schema(&[("name", string_schema())], &["name"])),
        tool("motion3_pose", "Read Motion3 runtime parameter values at seconds without applying them. INX uses explicit bindings; CMO3 matches exact native Parameter IDs and returns scalar values. Out-of-range values fail, except the X value of a repeating INX parameter, which evaluation wraps; every unmapped curve is reported. No fades, blending or Cubism pose evaluation.",
            schema(&[("name", string_schema()), ("time", number_schema())], &["name", "time"])),
        tool("motion3_seek", "Apply Motion3 runtime values. CMO3 matches native Parameter IDs and attached Pose selection signals, replaying Pose at 60 Hz for deterministic seeks up to one hour. Leaves document/history unchanged and reports unmapped curves. No blending between motions. \
            With fades true, playback starts here: each Parameter curve is weighted by its fade (the curve's, else the \
            motion's; Cubism's sine ease) from the value without the motion, fading in over the time played since this \
            seek and out towards the clip end unless motion3_advance loops. Without it the curves apply in full.",
            schema(&[("name", string_schema()), ("time", number_schema()), ("fades", json!({"type":"boolean"}))], &["name", "time"])),
        tool("motion3_advance", "Advance the active Motion3 preview by seconds, optionally looping at its effective clip end. Default clamps at the end; fades as the seek set them, no blending.",
            schema(&[("dt", number_schema()), ("loop", json!({"type":"boolean"}))], &["dt"])),
        tool("motion3_reset", "Clear the Motion3 runtime preview and physics without changing document/history. Rejects an active INX animation.", schema(&[], &[])),
        tool("animation_create", "Create an animation in the shared document history.",
            schema(&[("name", string_schema())], &["name"])),
        tool("animation_rename", "Rename an animation while preserving its lanes and keys.",
            schema(&[("name", string_schema()), ("new_name", string_schema())], &["name", "new_name"])),
        tool("animation_delete", "Delete an animation in one undo.",
            schema(&[("name", string_schema())], &["name"])),
        tool("animation_set", "Edit animation timing, playback properties and loop editing in one undo.", schema(&[
            ("name", string_schema()), ("length", integer_schema()), ("timestep", number_schema()),
            ("leadIn", integer_schema()), ("leadOut", integer_schema()), ("additive", json!({"type":"boolean"})),
            ("animationWeight", number_schema()), ("loopEditing", json!({"type":"boolean"})),
        ], &["name"])),
        tool("animation_key_set", "Set a parameter-axis animation key without changing other lanes.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("axis", json!({"type":"integer","enum":[0,1]})), ("frame", frame_schema()),
            ("value", number_schema()), ("tension", number_schema()),
            ("interpolation", json!({"type":"string","enum":["Nearest","Linear","Stepped","Cubic","Bezier"]})),
        ], &["name", "parameter", "axis", "frame", "value"])),
        tool("animation_lane_set", "Set a parameter-axis lane's interpolation, merge mode, shy mark or freeze mark in \
            one undo. A shy lane is hidden from the editor timeline while the animation's shy is on (animation_shy_set); \
            a frozen lane holds its value at the freeze frame while freeze is on (animation_freeze_set).", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("axis", json!({"type":"integer","enum":[0,1]})),
            ("interpolation", json!({"type":"string","enum":["Nearest","Linear","Stepped","Cubic","Bezier"]})),
            ("merge_mode", string_schema()), ("shy", json!({"type":"boolean"})), ("frozen", json!({"type":"boolean"})),
        ], &["name", "parameter", "axis"])),
        tool("animation_freeze_set", "Mark lanes of an INX animation frozen in a batch, or turn its freeze on at a frame \
            or off, in one undo. mark all, listed or unlisted marks exactly those lanes; none clears every mark and turns \
            freeze off unless frame is given. frame (an integer frame of the animation) turns freeze on there, null turns it off. While freeze \
            is on, each frozen lane holds its value at that frame in playback and every export, and an edit that changes \
            a frozen lane's keys, interpolation or merge mode, or deletes it, is refused; \
            key all and unkey all pass frozen lanes by.", schema(&[
            ("name", string_schema()),
            ("mark", json!({"type":"string","enum":["all","listed","unlisted","none"]})),
            ("parameters", json!({"type":"array","items":integer_schema()})),
            ("frame", json!({"type":["integer","null"],"minimum":0})),
        ], &["name"])),
        tool("animation_template_save", "Keep the keys of an INX animation from frame `from` to `to` (inclusive) as an \
            animation template named `template`, in animation-templates.json of the settings directory. Every lane with \
            a key in the range is kept, or only the lanes of `parameters`; a lane is kept under its parameter's name, \
            with frames counted from `from`, at most 100000 frames. A saved name is replaced only with overwrite. \
            Answers {template, entry, replaced}. The document is unchanged and there is no undo step.", schema(&[
            ("name", string_schema()), ("template", string_schema()), ("from", frame_schema()), ("to", frame_schema()),
            ("parameters", json!({"type":"array","items":integer_schema()})), ("overwrite", json!({"type":"boolean"})),
        ], &["name", "template", "from", "to"])),
        tool("animation_template_apply", "Insert the keys of a saved animation template into an INX animation from the \
            integer frame on, in one undo. Each template lane goes to the parameter with the same name and axis; a name \
            two parameters share is refused. Answers {applied, lanes, unmatched, dropped}: the keys written, the \
            parameter UUIDs and axes they went to, the template's parameter names and axes no parameter matches, and \
            the keys past the end of the animation, which are left out. A new lane is Linear and Forced. An apply that \
            changes nothing adds no undo step. A frozen lane refuses the whole apply.", schema(&[
            ("name", string_schema()), ("template", string_schema()), ("frame", frame_schema()),
        ], &["name", "template", "frame"])),
        tool("animation_template_delete", "Delete a saved animation template. A name that is not saved is refused. \
            There is no undo step.", schema(&[("template", string_schema())], &["template"])),
        tool("animation_template_list", "List the saved animation templates as {presets: {template: {length, lanes: \
            [{parameter, axis, keyframes}]}}}. The resource resource://soul2d/animation/templates returns the same.",
            schema(&[], &[])),
        tool("animation_controller_add", "Add a parameter controller to an INX animation, pinned to the frontmost Part \
            drawn at (x, y) on the integer `frame` (default 0, before eye blinking); with `end` [x, y], a direction \
            controller from (x, y) to `end`. It moves the listed `parameters`, each one that binds its Part (or the Part \
            of `end`) or a node above it, or all of those by default. One undo step. Read back as the clip's \
            `controllers`.", schema(&[
            ("name", string_schema()), ("controller", string_schema()), ("x", number_schema()), ("y", number_schema()),
            ("end", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})), ("frame", frame_schema()),
            ("parameters", json!({"type":"array","items":integer_schema()})),
        ], &["name", "controller", "x", "y"])),
        tool("animation_controller_set", "Change a parameter controller: the `parameters` it moves, the controller it \
            follows as `target` (null for none), `extend` (whether a line of a direction controller reaches past its \
            ends, default true) and `weight` (0 to 100, how much of the followed result a bake keys). Null \
            `parameters` changes nothing. One undo step.",
            schema(&[
            ("name", string_schema()), ("controller", string_schema()),
            ("parameters", json!({"type":["array","null"],"items":integer_schema()})),
            ("target", json!({"type":["string","null"]})),
            ("extend", json!({"type":"boolean"})), ("weight", number_schema()),
        ], &["name", "controller"])),
        tool("animation_controller_delete", "Delete one parameter controller, or every one of the animation without \
            `controller`. A controller that followed a deleted one follows nothing. One undo step.", schema(&[
            ("name", string_schema()), ("controller", string_schema()),
        ], &["name"])),
        tool("animation_controller_read", "Read only. Where each parameter controller of an INX animation is on an \
            integer frame, before eye blinking: {name, frame, controllers: [{name, kind, start, end?, parameters, \
            target, extend, weight, \
            broken?}]}. `broken` says why a controller cannot be placed.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
        ], &["name", "frame"])),
        tool("animation_controller_move", "Move a parameter controller to (x, y) on an integer frame: its parameters are \
            solved, inside their ranges, so that a single-point controller reaches the point or a direction controller \
            turns its end toward it, and keyed on the frame in one undo step. A lane that would not then draw the key \
            from the current parameter values (such as a Stepped lane on a later key) is refused. Answers {reached, distance, keys}.",
            schema(&[
            ("name", string_schema()), ("controller", string_schema()), ("frame", frame_schema()),
            ("x", number_schema()), ("y", number_schema()),
        ], &["name", "controller", "frame", "x", "y"])),
        tool("animation_controller_bake", "Bake target tracking: on every integer frame from `from` to `to` (default \
            the whole animation), each controller with a target, in list order, follows it, and its parameters are \
            keyed with the solved values blended by its weight with the frame's values so far, in list order. Then the animation's \
            controllers are removed. One undo step. Refused while freeze is on, and for a lane that would not draw the \
            key, as move is. Answers {from, to, keys, unreached}.", schema(&[
            ("name", string_schema()), ("from", frame_schema()), ("to", frame_schema()),
        ], &["name"])),
        tool("animation_physics_bake", "Bake physics (INX): simulate the enabled SimplePhysics drivers of the \
            animation `name` and key their output parameters on every integer frame `from` to `to` (default the \
            whole clip) as Linear/Forced keys, in one undo step. `parameters` limits the bake to those \
            SimplePhysics outputs (default all of them). With `warmup` (default true) the whole clip plays once \
            to settle the physics before the replay that the keys come from, as the Cubism manual's physics bake \
            does. An enabled PathDeformer driver, form edits or form timing, a freeze, an additive or weighted \
            clip, an output lane that is not Linear/Forced, or a simulated value outside the parameter's range \
            refuses the bake without changing anything. Answers {name, from, to, frames, keys, parameters, warmup}.",
            schema(&[
            ("name", string_schema()),
            ("parameters", json!({"type":"array","items":integer_schema()})),
            ("from", frame_schema()), ("to", frame_schema()), ("warmup", json!({"type":"boolean"})),
        ], &["name"])),
        tool("animation_motionsync_start", "Start an offline mouth-shape analysis (ANI26) of the local WAV file \
            `path` (RIFF/WAVE, resolved to a canonical name, at most 64 MiB) with the external Rhubarb Lip Sync \
            executable on a worker thread, and return its job id; poll animation_motionsync_status. One analysis \
            runs at a time. `recognizer` is phonetic (default, for non-English audio; documented as less precise) \
            or pocketSphinx (English). The program, its options and its timeout are host settings \
            (SOUL2D_RHUBARB or `rhubarb` on PATH), never request fields. The document, its history and the preview \
            do not change. Answers {job, state: 'running', backend: 'rhubarb', recognizer, path}.", schema(&[
            ("path", string_schema()),
            ("recognizer", json!({"type":"string","enum":["phonetic","pocketSphinx"]})),
        ], &["path"])),
        tool("animation_motionsync_status", "Report a speech-analysis job without blocking: running, finished \
            (with duration and cues [{start, end, value}] of half-open spans in seconds, values one of the Rhubarb \
            shapes A-H/X), failed (with error) or cancelled. Answers {job, state, backend, recognizer, path, ...}; \
            an unknown job is an error.", schema(&[("job", job_schema())], &["job"])),
        tool("animation_motionsync_cancel", "Ask a running speech analysis to stop; the analyzer is killed and \
            reaped. A finished, failed or already cancelled job is returned unchanged. Answers the same object as \
            animation_motionsync_status.", schema(&[("job", job_schema())], &["job"])),
        tool("animation_motionsync_bake", "Bake speech lip sync (INX): key the mouth shapes of the finished \
            analysis job `job` on every frame `from` to `to` of the animation `name` (default the whole recording \
            starting where its audio track `audio_index` begins, one frame past the end keyed to the rest shape X) \
            as Linear/Forced keys, in one undo step. `mappings` is a nonempty list of at most 32 entries \
            {parameter (a leaf parameter UUID), axis (0 or 1), shapes {A, B, C, D, E, F, G, H, X: value}}: every \
            shape is required and each value must be a finite f32 inside the parameter's axis range. Cues are \
            half-open [start, end) at audio time (frame - audio frame) * timestep. With `extend` true the clip may \
            be lengthened to reach `to`. The WAV the track names must still be the analyzed file with unchanged \
            bytes. A stale or running job, a muted/group-hidden/group-locked track, an additive or weighted clip, \
            a freeze, a lane that is not Linear/Forced, frame stepping or a blink track over a mapped parameter \
            refuses the bake without changing anything. Answers {name, job, audio_index, from, to, frames, keys, \
            parameters, length}.", schema(&[
            ("name", string_schema()),
            ("job", job_schema()),
            ("audio_index", json!({"type":"integer","minimum":0})),
            ("mappings", json!({"type":"array","minItems":1,"maxItems":32,"items":{
                "type":"object","required":["parameter","axis","shapes"],"additionalProperties":false,
                "properties":{"parameter":integer_schema(),"axis":{"type":"integer","minimum":0,"maximum":1},
                "shapes":motionsync_shapes_schema()}}})),
            ("from", frame_schema()), ("to", frame_schema()),
            ("extend", json!({"type":"boolean"})),
        ], &["name", "job", "audio_index", "mappings"])),
        tool("animation_form_key", "Form animation: key the form edit of `node` on an integer `frame` of an INX \
            animation, without parameters, adding the form edit if the node has none in that adjustment `mode`. Mode 1 \
            (default) offsets the node's own vertices before its parents deform it, so the edit follows the parent's \
            angle; mode 2 offsets a drawn Part or Mask in model coordinates after all deformation and glue. A Part, \
            Mask, GridDeformer or PathDeformer has one offset per vertex or grid point. The key holds `offsets`, or \
            zeros with `reset` (Reset Form Edit), or else what the form edit shows on that frame moved by `delta` \
            [dx, dy] at the listed `vertices` (all by default). With `space` \"model\" the delta is how far the \
            vertices move in the picture shown, as a viewport drag gives it; in mode 1 it is measured on a drawn Part \
            outside composites, on a frame of the clip that shows form `frame`, so seek one first. `frame` is a form \
            frame, which `animation_form_time` maps clip frames to. One undo step. Seek again to see it. Read back as \
            the clip's `forms`.", schema(&[
            ("name", string_schema()), ("node", integer_schema()), ("mode", json!({"type":"integer","enum":[1, 2]})),
            ("frame", frame_schema()),
            ("offsets", json!({"type":"array","items":{"type":"array","items":number_schema(),"minItems":2,"maxItems":2}})),
            ("reset", json!({"type":"boolean"})),
            ("delta", json!({"type":"array","items":number_schema(),"minItems":2,"maxItems":2})),
            ("vertices", json!({"type":"array","items":{"type":"integer","minimum":0}})),
            ("space", json!({"type":"string","enum":["form", "model"]})),
        ], &["name", "node", "frame"])),
        tool("animation_form_delete", "Delete form edits of an INX animation: those of `node` (in `mode` only, if \
            given), or all without `node`; with `frame`, only their key on that frame. A form edit left without keys \
            goes. One undo step.", schema(&[
            ("name", string_schema()), ("node", integer_schema()), ("mode", json!({"type":"integer","enum":[1, 2]})),
            ("frame", frame_schema()),
        ], &["name"])),
        tool("animation_form_time", "Set the form timing curve of an INX animation (time remapping): each key maps a \
            frame of the clip to the form frame its form edits show there, sampled with `interpolation` (Linear by \
            default, or Nearest, Stepped, Cubic, Bezier). Null or no keys clears it. One undo step. Read back as the \
            clip's `formTime`.", schema(&[
            ("name", string_schema()),
            ("keyframes", json!({"type":["array","null"],"items":{"type":"object","properties":{
                "frame": frame_schema(), "value": number_schema(), "tension": number_schema()},
                "required":["frame","value"]}})),
            ("interpolation", json!({"type":"string","enum":["Linear","Nearest","Stepped","Cubic","Bezier"]})),
        ], &["name", "keyframes"])),
        tool("animation_form_read", "Read only. The form edits of an INX animation on an integer frame: {name, frame, \
            formFrame, forms: [{node, mode, keys, offsets | broken}]}, where `formFrame` is the frame the timing curve \
            shows and `broken` says why a form edit is not drawn: a missing node, a node without vertices to edit, a vertex \
            count that changed, or mode 2 on a node that is no drawn Part or Mask or sits inside a composite.",
            schema(&[("name", string_schema()), ("frame", frame_schema())], &["name", "frame"])),
        tool("animation_bookmark_parameters", "The parameters related to a node, as a parameter bookmark lists them: \
            every parameter with a binding on the node or a node above it, in parameter list order, as [{uuid, name}]. \
            To bookmark what is drawn at a point, find the Part there with scene_geometry. The session is unchanged.",
            schema(&[("node", integer_schema())], &["node"])),
        tool("animation_bookmark_set", "Add a parameter bookmark label to an INX animation in one undo, or replace the \
            one at index (from 0). A label (at most 256 characters) names a list of parameters and sits at a canvas point (x, y in model \
            coordinates); the editor draws it on the viewport and lists its parameters in the Parameters panel. The clip \
            keeps them as bookmarks: [{label, x, y, parameters}]. Deleting a parameter takes it off every label; an INP \
            export drops them.", schema(&[
            ("name", string_schema()), ("label", json!({"type":"string","minLength":1,"maxLength":256})),
            ("x", number_schema()), ("y", number_schema()),
            ("parameters", json!({"type":"array","items":integer_schema()})),
            ("index", json!({"type":"integer","minimum":0})),
        ], &["name", "label", "x", "y", "parameters"])),
        tool("animation_bookmark_delete", "Delete the parameter bookmark label at index of an INX animation, or every \
            label of it when index is left out, in one undo.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})),
        ], &["name"])),
        tool("animation_bookmark_copy", "Copy every parameter bookmark label of an INX animation to the animations in \
            to, or to every other animation, in one undo. A label the target already has is not added again.", schema(&[
            ("name", string_schema()), ("to", json!({"type":"array","items":string_schema()})),
        ], &["name"])),
        tool("animation_shy_set", "Mark lanes of an INX animation shy in a batch, or turn its shy on or off, in one undo. \
            mark all marks every lane; unkeyed, listed and unlisted mark exactly the lanes without keys, of the listed \
            parameters, or of the others, and turn shy on; none clears every mark and turns shy off. active sets shy on or \
            off and overrides what the mark does. While shy is on, the editor timeline hides shy lanes; they still play, \
            export and take part in Key all and Unkey all.", schema(&[
            ("name", string_schema()),
            ("mark", json!({"type":"string","enum":["all","unkeyed","listed","unlisted","none"]})),
            ("parameters", json!({"type":"array","items":integer_schema()})), ("active", json!({"type":"boolean"})),
        ], &["name"])),
        tool("animation_lane_delete", "Delete a parameter-axis lane and its keys in one undo.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("axis", json!({"type":"integer","enum":[0,1]})),
        ], &["name", "parameter", "axis"])),
        tool("animation_key_delete", "Delete a parameter-axis key at a frame in one undo.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("axis", json!({"type":"integer","enum":[0,1]})), ("frame", frame_schema()),
        ], &["name", "parameter", "axis", "frame"])),
        tool("animation_marker_set", "Put a marker (a note of at most 256 characters, may be empty) on a frame of an INX animation in one undo, or replace the text of the marker already there. Markers are kept in the clip as markers: [{frame, text}] sorted by frame, one per frame. They travel with the clip, so a saved file and an INP export carry them. A new marker needs text. On an existing marker, to moves it to a free frame and locked: true or false sets its lock, each with or without text, all in the same undo. A locked marker (locked: true in the list) refuses a new text, a move and a delete until a request carries locked: false.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()), ("text", string_schema()),
            ("to", frame_schema()), ("locked", json!({"type":"boolean"})),
        ], &["name", "frame"])),
        tool("animation_marker_delete", "Delete the marker on a frame of an INX animation in one undo. A locked marker is refused.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
        ], &["name", "frame"])),
        tool("animation_blink_parameters_set", "Eye blinking settings: list the 1D parameters that open and close the eyes (0 closed, 1 open) in one undo, kept as model.live2d.eyeBlink. An empty list clears it. Deleting a parameter takes it off the list.", schema(&[
            ("parameters", json!({"type":"array","items":{"type":"integer"}})),
        ], &["parameters"])),
        tool("animation_blink_set", "Set the eye blinking track of an INX animation in one undo: a key (frame, value from 0 closed to 1 open, linear between keys) and/or the effect (0 to 100, 100 when unset). The clip keeps it as eyeBlink: {effect, keyframes: [{frame, value}]}. While the track has keys, playback, seek, sample and export scale every listed eye parameter toward its minimum by 1 + (value - 1) * effect / 100.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
            ("value", json!({"type":"number","minimum":0,"maximum":1})),
            ("effect", json!({"type":"number","minimum":0,"maximum":100})),
        ], &["name"])),
        tool("animation_blink_key_delete", "Delete the eye blinking key on a frame of an INX animation in one undo.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
        ], &["name", "frame"])),
        tool("animation_audio_add", "Add an audio track to an INX animation in one undo: a WAV file (RIFF/WAVE; RF64 and a name with '%' are refused) that starts at frame (default 0). The clip keeps it as audio: [{path, frame, mute?}], top track first, path resolved to an absolute name. Video export (export_video_start) mixes the tracks that are not muted at full volume, each starting where its frame first shows in the video and playing once, unless the request names its own audio file. An image sequence, frames, GIF, APNG, WebP and still-image exports leave them out, and an INP export drops them. Live playback in the editor plays no sound.", schema(&[
            ("name", string_schema()), ("path", string_schema()), ("frame", frame_schema()),
        ], &["name", "path"])),
        tool("animation_audio_set", "Move an audio track of an INX animation to another start frame, mute or unmute it, and/or put it in a group track (group names one, null leaves it; a hidden group's tracks are not exported), in one undo. index counts from 0 in the clip's audio list.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})), ("frame", frame_schema()),
            ("mute", json!({"type":"boolean"})), ("group", json!({"type":["string","null"]})),
        ], &["name", "index"])),
        tool("animation_audio_delete", "Remove an audio track of an INX animation in one undo. index counts from 0 in the clip's audio list.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})),
        ], &["name", "index"])),
        tool("animation_image_add", "Add an image track (a background, or a foreground with front) to an INX animation in one undo: a PNG, TGA or baseline JPEG file, up to 8 tracks a clip. The clip keeps it as images: [{path, x, y, scale, opacity, front?, hidden?, locked?, group?}], top track first, path resolved to an absolute name. x and y place the image's centre in world units (default 0), scale is world units per image pixel (default 1), opacity 0 to 1 (default 1). Video, frames and GIF exports of the clip draw the tracks behind the model (in front with front), the top track over the lower ones; a hidden track is not exported. Still image and INP exports leave them out. The editor draws image tracks while an image sequence is present in the active clip, including guides; Solo and onion-skin views omit these overlays.", schema(&[
            ("name", string_schema()), ("path", string_schema()), ("x", json!({"type":"number"})), ("y", json!({"type":"number"})),
            ("scale", json!({"type":"number","exclusiveMinimum":0})), ("opacity", json!({"type":"number","minimum":0,"maximum":1})),
            ("front", json!({"type":"boolean"})), ("group", json!({"type":["string","null"]})),
        ], &["name", "path"])),
        tool("animation_image_set", "Change an image track of an INX animation in one undo: placement (x, y, scale, opacity), front, hidden, locked, guide (preview only, omitted from exports), its group (a name, or null to leave it) and its place in the list (to, an index). A locked track only changes in a request that unlocks it (locked: false); a track in a locked group does not change until the group is unlocked. index counts from 0 in the clip's images list.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})), ("x", json!({"type":"number"})),
            ("y", json!({"type":"number"})), ("scale", json!({"type":"number","exclusiveMinimum":0})),
            ("opacity", json!({"type":"number","minimum":0,"maximum":1})), ("front", json!({"type":"boolean"})),
            ("guide", json!({"type":"boolean"})),
            ("hidden", json!({"type":"boolean"})), ("locked", json!({"type":"boolean"})),
            ("group", json!({"type":["string","null"]})), ("to", json!({"type":"integer","minimum":0})),
        ], &["name", "index"])),
        tool("animation_image_delete", "Remove an image track of an INX animation in one undo; a locked track, or one in a locked group, is refused. index counts from 0 in the clip's images list.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})),
        ], &["name", "index"])),
        tool("animation_image_sequence", "Add a numbered PNG/JPEG image sequence with name, directory and prefix, or reload an existing sequence with name and index. Numeric suffix gaps remain blank exposures. Reload preserves the source-frame origin and timing. Files remain external links; export_collect copies them. One undo; locked tracks and groups refuse reload.", schema(&[
            ("name", string_schema()), ("directory", string_schema()), ("prefix", string_schema()),
            ("index", json!({"type":"integer","minimum":0})),
        ], &["name"])),
        tool("animation_image_time", "Set a sequence track's timeRemap curve in one undo. Each key maps a clip frame to a zero-based source offset from firstNumber; fractional offsets hold the preceding image, and missing or out-of-range offsets are blank. Without a curve, source time follows the clip. Null or empty keyframes clears the curve. Exact keys take effect on their frame.", schema(&[
            ("name", string_schema()), ("index", json!({"type":"integer","minimum":0})),
            ("keyframes", json!({"type":["array","null"],"items":{"type":"object","properties":{"frame":{"type":"integer","minimum":0},"value":{"type":"number"}},"required":["frame","value"],"additionalProperties":false}})),
            ("interpolation", string_schema()),
        ], &["name", "index", "keyframes"])),
        tool("animation_image_read", "Read the source image selected by every sequence track at a clip frame without changing the session: images contains index, path (null for a gap), sourceFrame, gap and guide. Global frame stepping is applied before image remapping.", schema(&[
            ("name", string_schema()), ("frame", json!({"type":"number","minimum":0})),
        ], &["name", "frame"])),
        tool("animation_frame_step", "Set stepped exposure for an INX clip or parameterGroup in one undo. Keys contain frame and integer value (hold length 1 to 1000000). A key starts a new hold grid; time before the first key is continuous. Null or empty keyframes clears. The nearest ancestor group's override replaces global stepping for its parameters. Global stepping also affects forms and images, while audio keeps continuous clip time.", schema(&[
            ("name", string_schema()), ("parameterGroup", json!({"type":"integer","minimum":0})),
            ("keyframes", json!({"type":["array","null"],"items":{"type":"object","properties":{"frame":{"type":"integer","minimum":0},"value":{"type":"integer","minimum":1,"maximum":1000000}},"required":["frame","value"],"additionalProperties":false}})),
        ], &["name", "keyframes"])),
        tool("animation_record_start", "Start recording live parameter changes into an INX animation (PAR09): from \
             now until animation_record_stop, every value_set is sampled at the clip frame its wall-clock time reaches \
             (frame, default 0, plus elapsed seconds / timestep). Nothing changes in the document until the stop. \
             Refused while a recording runs. Result {name, frame}.",
            schema(&[("name", string_schema()), ("frame", json!({"type":"integer","minimum":0}))], &["name"])),
        tool("animation_record_status", "Read the live recording: {recording:false} or {recording:true, name, \
             startFrame, frame (the current one), samples}.", schema(&[], &[])),
        tool("animation_record_stop", "Stop the recording. Unless discard, every recorded parameter is keyed in one \
             undo on all its axes: its value before the recording on the start frame, then the last value set within \
             each later frame; frames past the clip end are dropped. Result {name, parameters, keys, dropped} or, with \
             discard, {name, discarded}.",
            schema(&[("discard", json!({"type":"boolean"}))], &[])),
        tool("animation_playlist_set", "Create or replace a physics playlist (PHY03) in one undo: entries play one after \
             another, each {animation} (an INX clip) or {group} (a scene group); 1 to 256 entries. Stored in the model as \
             playlists. Result {playlists, sceneGroups}.",
            schema(&[("name", string_schema()), ("entries", json!({"type":"array","minItems":1,"maxItems":256,
                "items":{"type":"object","properties":{"animation":string_schema(),"group":string_schema()}}}))],
                &["name", "entries"])),
        tool("animation_playlist_delete", "Delete a physics playlist in one undo. Result {playlists, sceneGroups}.",
            schema(&[("name", string_schema())], &["name"])),
        tool("animation_scene_group_set", "Create or replace a scene group in one undo: its clips play at once, applied \
             in order; type overwrite (default) moves each parameter the clip animates toward the clip's value by \
             weight/100, add adds the clip's offset from the parameter default times weight/100 (weight 0 to 100, \
             default 100). Stored in the model as sceneGroups. Result {playlists, sceneGroups}.",
            schema(&[("name", string_schema()), ("scenes", json!({"type":"array","minItems":1,"maxItems":256,
                "items":{"type":"object","properties":{"animation":string_schema(),
                    "type":{"type":"string","enum":["overwrite","add"]},
                    "weight":{"type":"number","minimum":0,"maximum":100}},"required":["animation"]}}))],
                &["name", "scenes"])),
        tool("animation_scene_group_delete", "Delete a scene group in one undo; refused while a playlist holds it. \
             Result {playlists, sceneGroups}.", schema(&[("name", string_schema())], &["name"])),
        tool("animation_playlist_seek", "Preview a physics playlist at time seconds (clamped to its length, the sum of \
             its entries, each as long as its longest clip) without changing the document; animation_advance then plays \
             it on, looping unless loop is false, and physics_step shows the physics. Clip forms are not previewed. \
             The snapshot's playlist field shows {name, time}.",
            schema(&[("name", string_schema()), ("time", number_schema())], &["name", "time"])),
        tool("animation_group_add", "Add a group track to an INX animation in one undo, up to 32, hidden or locked from the start if asked: groups: [{name, x, y, scale, opacity, hidden?, locked?}]. Image and audio tracks join it with their group field and the model with animation_group_set model: true. A member's position is scaled by the group's scale and moved by its x, y; the opacities multiply.", schema(&[
            ("name", string_schema()), ("group", string_schema()), ("x", json!({"type":"number"})), ("y", json!({"type":"number"})),
            ("scale", json!({"type":"number","exclusiveMinimum":0})), ("opacity", json!({"type":"number","minimum":0,"maximum":1})),
            ("hidden", json!({"type":"boolean"})), ("locked", json!({"type":"boolean"})),
        ], &["name", "group"])),
        tool("animation_group_set", "Change a group track of an INX animation in one undo: placement (x, y, scale, opacity), hidden (a hidden group's members, the model included, are not exported), locked (its members cannot be changed either), and model (true puts the model in the group as the clip's modelGroup, false takes it out). A locked group only changes in a request that unlocks it.", schema(&[
            ("name", string_schema()), ("group", string_schema()), ("x", json!({"type":"number"})), ("y", json!({"type":"number"})),
            ("scale", json!({"type":"number","exclusiveMinimum":0})), ("opacity", json!({"type":"number","minimum":0,"maximum":1})),
            ("hidden", json!({"type":"boolean"})), ("locked", json!({"type":"boolean"})), ("model", json!({"type":"boolean"})),
        ], &["name", "group"])),
        tool("animation_group_delete", "Remove a group track of an INX animation in one undo; its members stay, out of the group. A locked group is refused.", schema(&[
            ("name", string_schema()), ("group", string_schema()),
        ], &["name", "group"])),
        tool("animation_key_add", "Add a keyframe at a frame using the parameter's current value, in one undo.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("frame", frame_schema()),
        ], &["name", "parameter", "frame"])),
        tool("animation_key_add_all", "Key all: add a keyframe at a frame of an INX animation for every listed parameter (every parameter of the model without a list), each axis using the parameter's current value, in one undo. A key already on the frame takes the new value.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
            ("parameters", json!({"type":"array","items":{"type":"integer"},"minItems":1})),
        ], &["name", "frame"])),
        tool("animation_key_delete_all", "Unkey all: delete every key on a frame of an INX animation (only the keys of the listed parameters with a list) in one undo. The lanes stay. Refused when no key is on the frame.", schema(&[
            ("name", string_schema()), ("frame", frame_schema()),
            ("parameters", json!({"type":"array","items":{"type":"integer"},"minItems":1})),
        ], &["name", "frame"])),
        tool("animation_key_copy", "Copy one parameter's animation keys at a frame into the session key clipboard.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("frame", frame_schema()),
        ], &["name", "parameter", "frame"])),
        tool("animation_key_paste", "Paste the session key clipboard into a parameter at a frame in one undo.", schema(&[
            ("name", string_schema()), ("parameter", integer_schema()),
            ("frame", frame_schema()),
        ], &["name", "parameter", "frame"])),
        tool("animation_sample", "Sample animation values without changing the session.",
            schema(&[("name", string_schema()), ("frame", number_schema())], &["name", "frame"])),
        tool("animation_onion", "Onion skin: a PNG of the puppet at a frame over the frames before it (red) and after it \
             (blue), each fainter the further it is. before and after count the ghost frames (default 2 each, at most 8), \
             step is the frames between them (default 1), opacity is the nearest ghost's (default 0.3), size the longest \
             side (16 to 4096, default 1024). bounds, width and height together draw a given view instead (the editor's camera). Optional path also writes the PNG. The session is unchanged. INX documents.",
            schema(&[("name", string_schema()), ("frame", number_schema()),
                ("before", json!({"type": "integer", "minimum": 0, "maximum": 8})),
                ("after", json!({"type": "integer", "minimum": 0, "maximum": 8})), ("step", number_schema()),
                ("opacity", json!({"type": "number", "minimum": 0, "maximum": 1})),
                ("size", json!({"type": "integer", "minimum": 16, "maximum": 4096})),
                ("bounds", json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4})),
                ("width", json!({"type": "integer", "minimum": 1, "maximum": 4096})),
                ("height", json!({"type": "integer", "minimum": 1, "maximum": 4096})),
                ("path", string_schema())], &["name", "frame"])),
        tool("animation_trajectory", "Vertex trajectories: the world-space positions of vertices of one Part at each frame \
             from from to to (default the whole animation), step frames apart (default 1, or more for a clip over 2000 frames; at most 2000 frames). \
             vertices lists indices (default every vertex, every few-th one for a mesh over 256). Read only. INX documents.",
            schema(&[("name", string_schema()), ("node", integer_schema()), ("from", number_schema()),
                ("to", number_schema()), ("step", number_schema()),
                ("vertices", json!({"type": "array", "items": {"type": "integer", "minimum": 0}}))], &["name", "node"])),
        tool("animation_seek", "Preview an animation frame without changing document history.",
            schema(&[("name", string_schema()), ("frame", number_schema())], &["name", "frame"])),
        tool("animation_advance", "Advance the active animation preview using its timing and loop markers; also moves a random pose on by dt (loop has no effect there).",
            schema(&[("dt", json!({"type":"number","minimum":0})),
                ("loop", json!({"type":"boolean"}))], &["dt"])),
        tool("animation_reset", "Stop animation preview (or a random pose) and restore manual parameter values.",
            schema(&[], &[])),
        tool("random_pose_seek", "Preview a random pose at a time without changing the document, history or manual values. Soul2D's own formulas: pattern B moves every leaf parameter between random targets 1.5 s apart, A swings the default-ID head, eye, body and mouth parameters, Posing is B with the eyes, mouth and breath swinging. The same seed and time give the same pose. time is at most 1e9 seconds. animation_advance moves it on, animation_reset stops it.",
            schema(&[("time", json!({"type":"number","minimum":0})),
                ("seed", json!({"type":"integer","minimum":0})),
                ("pattern", json!({"type":"string","enum":["A","B","Posing"]})),
                ("parameters", json!({"type":"array","items":integer_schema(),"minItems":1}))], &["time"])),
        tool("atlas_pack", "Plan best-short-side-fit MaxRects placements for rectangles in a fixed atlas without changing the session.", schema(&[
            ("size", json!({"type":"array","items":{"type":"integer","minimum":1,"maximum":32768},"minItems":2,"maxItems":2})),
            ("rectangles", json!({"type":"array","items":{"type":"array",
                "items":{"type":"integer","minimum":1,"maximum":4294967295u64},"minItems":2,"maxItems":2},
                "minItems":1,"maxItems":4096})),
        ], &["size", "rectangles"])),
        tool("cv_analyze", "Run a read-only binary-mask query (distance transform, thinning, longest path, contours) without changing the session.", schema(&[
            ("method", json!({"type":"string","enum":["distance","thin","path","contours"]})),
            ("size", json!({"type":"array","items":{"type":"integer","minimum":1,"maximum":65536},"minItems":2,"maxItems":2})),
            ("pixels", json!({"type":"array","items":{"type":"integer","minimum":0,"maximum":255},"minItems":1,"maxItems":65536})),
            ("mode", json!({"type":"string","enum":["External","List","Ccomp","Tree"]})),
            ("approximation", json!({"type":"string","enum":["None","Simple","Tc89L1","Tc89Kcos"]})),
        ], &["method", "size", "pixels"])),
        tool("image_process", "Inspect an 8-bit image or run the RGBA color-bleed padding transform without changing the session.", schema(&[
            ("method", json!({"type":"string","enum":["inspect","bleed"]})),
            ("format", json!({"type":"string","enum":["Mono","Rgb","Rgba"]})),
            ("size", json!({"type":"array","items":{"type":"integer","minimum":0,"maximum":65536},"minItems":2,"maxItems":2})),
            ("pixels", json!({"type":"array","items":{"type":"integer","minimum":0,"maximum":255},"minItems":0,"maxItems":262144})),
            ("iterations", json!({"type":"integer","minimum":-2147483648i64,"maximum":256})),
        ], &["method", "format", "size", "pixels"])),
        tool(
            "deformer_set",
            "Set a persisted PathDeformer, GridDeformer, or MeshGroup deform property.",
            schema(
                &[
                    ("nodes", node_ids_schema()),
                    ("property", string_schema()),
                    ("value", json!({})),
                ],
                &["property", "value"],
            ),
        ),
        tool("deformer_validate", "Check deformers: overhanging lists each child of a GridDeformer whose rest mesh, or \
             rest mesh plus a keyed deform form, has vertices outside the deformer's grid (those vertices are \
             clamped to the grid edge when deformed); empty lists GridDeformer, PathDeformer and MeshGroup nodes \
             without children. values poses the parameters (default the current values). Read only.",
            schema(&[("values", values_schema())], &[])),
        tool("deformer_extend_grid", "Extend each GridDeformer in nodes so that no child leaves it: add a grid line \
             outside each side that deformer_validate reports a child past, at the farthest such point. Each added line \
             takes the keyed offsets of the old edge it continues, so the old area and the children outside it move \
             as before. A grid no child leaves is unchanged. One undo step.",
            schema(&[("nodes", node_ids_schema())], &["nodes"])),
        tool("deformer_create_rotation", "Create a rotation deformer (INX): a Node marked rotationDeformer \
             {handleLength} at pivot [x, y] in root space with angle radians (transform.r.z, default 0), under parent \
             (default the root), and move children into it keeping their world transforms. Children turn about the \
             pivot when its angle changes or is keyed (transform.r.z bindings). For continuous creation call it once \
             per deformer, passing the previous one as parent. Result {node, children}. One undo step.",
            schema(&[("pivot", pair_schema()), ("angle", number_schema()), ("handleLength", number_schema()),
                ("parent", integer_schema()), ("name", string_schema()), ("children", node_list_schema())],
                &["pivot"])),
        tool("deformer_bezier_controls", "Read a GridDeformer's Bezier control lattice: (3·columns+1) × (3·rows+1) \
             row-major [x, y] points in grid space for divisions [columns, rows] of cubic patches, sampled from the \
             grid at parameter's key (the rest grid without parameter). Edit them and write with deformer_bezier_warp. \
             Read only.",
            schema(&[("nodes", node_ids_schema()), ("divisions", pair_integer_schema()),
                ("parameter", integer_schema()), ("key", pair_integer_schema())], &["nodes", "divisions"])),
        tool("deformer_bezier_warp", "Warp a GridDeformer through Bezier handles: each grid point moves to the \
             surface of the controls lattice (as deformer_bezier_controls returns it) at its normalised axis \
             position, and the offsets are written to parameter's key. Handles are not stored; read them again \
             from the key. Divisions that divide the grid evenly reproduce an unedited lattice exactly. One undo step.",
            schema(&[("nodes", node_ids_schema()), ("divisions", pair_integer_schema()),
                ("controls", json!({"type":"array","items":pair_schema()})),
                ("parameter", integer_schema()), ("key", pair_integer_schema())],
                &["nodes", "divisions", "controls", "parameter", "key"])),
        tool("deformer_generate_sway", "Generate a sway (Cubism's automatic sway, PAR17) on each GridDeformer in nodes: \
             the 1D parameter gets three keys, the middle one the rest grid, the low and high ones swinging the grid \
             away from the fulcrum edge (top, bottom, left or right; default top). Inferred rule, not compared with \
             Cubism: a point at normalised distance d from the fulcrum has weight d^(1+softness) (softness default 1) \
             and moves -horizontal·w / +horizontal·w across the axis (default 15 % of the grid's length along it), \
             vertical·w toward the fulcrum (default 0; negative droops), and zoom·w times its distance from the centre \
             line (default 0). flip swaps the swing. A parameter without exactly three keys, the middle one at its \
             default, is refused unless replaceKeys, which sets them to min, default and max. Result {parameter, nodes:[{node, horizontal}]}. One undo step.",
            schema(&[("nodes", node_ids_schema()), ("parameter", integer_schema()),
                ("fulcrum", json!({"type":"string","enum":["top","bottom","left","right"]})),
                ("horizontal", number_schema()), ("vertical", number_schema()), ("softness", number_schema()),
                ("zoom", number_schema()), ("flip", json!({"type":"boolean"})), ("replaceKeys", json!({"type":"boolean"}))],
                &["nodes", "parameter"])),
        tool("deformer_generate_face_turn", "Generate a face turn (Cubism's automatic face movement, PAR16) on each \
             GridDeformer in nodes: the 1D parameter (for example ParamAngleX for axis x, ParamAngleY for axis y) gets \
             three keys, the middle one the rest grid, the high one turning right (x) or up (y), the low one the other \
             way. Inferred rule, not compared with Cubism: the grid outline stays and a point at normalised position \
             (along, across) in -1..1 moves amount·half·(1-along²)·√(1-across²) along the axis, where half is half the \
             grid's width or height and amount (default 0.3, below 0.5) is the centre's shift. flip reverses the turn. \
             Apply it to the face grid; nested grids follow. A parameter without exactly three keys, the middle one at its \
             default, is refused unless replaceKeys, which sets them to min, default and max. Result {parameter, axis, nodes:[{node, amount}]}. One undo step.",
            schema(&[("nodes", node_ids_schema()), ("parameter", integer_schema()),
                ("axis", json!({"type":"string","enum":["x","y"]})), ("amount", number_schema()),
                ("flip", json!({"type":"boolean"})), ("replaceKeys", json!({"type":"boolean"}))],
                &["nodes", "parameter", "axis"])),
        tool("deformer_generate_humanoid", "Generate the humanoid GridDeformer tree (Cubism's automatic deformer \
             generation, GEO13, without part recognition): roles maps role names to mesh UUIDs placed directly in that \
             role. Roles and nesting: Body > Arm L, Arm R, Neck, Head; Head > Face, Front Hair, Side Hair L, Side Hair \
             R, Back Hair; Face > Eye L, Eye R, Brow L, Brow R, Mouth, Nose (inferred, not compared). Each role becomes \
             GridDeformer 'Warp <role>' under parent (default the root) or its parent role, holding its meshes with \
             their world transforms kept, sized to the rest bounds of every mesh under it plus margin (fraction of the \
             larger extent, default 0.05, at least 1 unit) with divisions cells per axis (default 4). A role with no \
             mesh under it is not created and is listed in notGenerated. Result {created:[{role, node}], \
             notGenerated}. Running it again is refused while the 'Warp <role>' names exist. One undo step.",
            schema(&[("roles", json!({"type":"object","additionalProperties":node_list_schema()})),
                ("parent", integer_schema()), ("margin", number_schema()), ("divisions", integer_schema())],
                &["roles"])),
        tool("deformer_apply_to_children", "Apply one GridDeformer to its children and delete it: its keyed deform \
             offsets are added to each direct child's deform binding on the same parameter, key by key, and the \
             children take its place in the tree with their world transforms kept, so every pose looks the same. \
             Refused for a dynamic or rig-driven grid, a grid with a binding other than deform, a child that is not a \
             Part, Mask, MeshGroup, GridDeformer or PathDeformer, a child that passes the grid on to its own children, \
             a child with a transform binding, and a child that interpolates a shared parameter differently. One undo step.",
            schema(&[("nodes", node_ids_schema())], &["nodes"])),
        tool(
            "mesh_mirror",
            "Reflect Path, Grid, or Part vertices with the mesh-editor mirror. A Part needs vertices or mesh.verts. A grid mirror requires every vertex and does not call rig.mirror.",
            schema(
                &[
                    ("node", integer_schema()),
                    ("axis", string_schema()),
                    (
                        "origin",
                        json!({ "type": "array", "items": number_schema(), "minItems": 2, "maxItems": 2 }),
                    ),
                    (
                        "vertices",
                        json!({
                            "type": "array",
                            "items": { "type": "integer", "minimum": 0, "maximum": 4294967294u64 }
                        }),
                    ),
                ],
                &["node", "axis", "origin", "vertices"],
            ),
        ),
        tool(
            "mesh_edit",
            "Vertex-definition edits of a Part mesh (the mesh editor's Point, Connect, Edge cutter and Grid tools) as one undo step. Edits run in order: add {pos, connect?}, remove {indices | at}, move {indices, delta}, connect/disconnect/toggle {a, b}, cut {from, to}, grid_create {bounds, cuts?}, grid_axes {ys, xs}, grid_add_axis {pos}, grid_remove_axis {vertex}, grid_move {vertex, to}, triangulate (merges coincident vertices first), flip {axis}, path_deform {path, target, indices?, resolution?}, reset (the quad a default import gives the Part's texture, half sizes rounded down, as Cubism Reset mesh), merge {distance?} (vertices at most distance apart, 0 by default, become the lowest-numbered one). Triangles are rebuilt from the vertex connections, wound counter-clockwise, and UVs are position / texture size + 0.5. Deform bindings are carried over to the new vertices; welds, depth data and skinning weights are rejected when vertices change.",
            schema(
                &[
                    ("id", integer_schema()),
                    ("edits", json!({ "type": "array", "minItems": 1, "items": { "type": "object" } })),
                    ("mirror", json!({ "type": "object" })),
                    ("zoom", number_schema()),
                    ("autoConnect", json!({ "type": "boolean" })),
                ],
                &["id", "edits"],
            ),
        ),
        tool(
            "mesh_copy",
            "Copy one Part's rest triangles into the session mesh clipboard without changing the document. A later edit of the source does not change the clipboard. X or Y rotation, a nonlinear deformer ancestor, pinToMesh, or dynamic deformation is refused.",
            schema(&[("id", integer_schema())], &["id"]),
        ),
        tool(
            "mesh_paste",
            "Append the mesh clipboard onto Part id in that Part's rest space and consume the clipboard. reflect is none, horizontal, vertical, or both, across the target mesh origin. A failed paste keeps the clipboard. One undo step. The target's texture, transform, name, and UUID stay. Welds, depth data, and skinning weights are refused when the vertex count changes.",
            schema(
                &[("id", integer_schema()), ("reflect", string_schema())],
                &["id"],
            ),
        ),
        tool(
            "mesh_select",
            "Rectangle (rect [x0, y0, x1, y1]) or lasso (clicked points) selection over a Part's vertices, or over rest + keypoint offsets when parameter and key are given. mode is replace, add, remove or toggle against current. Read only.",
            schema(
                &[
                    ("id", integer_schema()),
                    ("parameter", integer_schema()),
                    ("key", pair_integer_schema()),
                    ("rect", json!({ "type": "array", "items": number_schema(), "minItems": 4, "maxItems": 4 })),
                    ("lasso", json!({ "type": "array", "items": pair_schema(), "minItems": 3 })),
                    ("mode", string_schema()),
                    ("current", json!({ "type": "array", "items": { "type": "integer", "minimum": 0 } })),
                ],
                &["id"],
            ),
        ),
        tool(
            "deform_edit",
            "Deform-mode tool gesture on the keypoint key of a parameter, written as one undo step through binding_set_deform. tool is translate {delta}, reset, brush {brush, stroke, flow?, snapAxis?}, inflate {brush, samples:[{center,amount}]}, path_deform {path, target, resolution?}, warp {grid: {columns, rows, points}}, perspective {corners}, bezier_translate {index, delta} or bezier_rotate {locked, index, from, to}. warp and perspective are the temporary deform tool (GEO21): warp lays a lattice of (columns+1)x(rows+1) points (1 to 64 cells a side), row-major from the top left, over the bounding box of the vertices and moves them with it, each cell bilinear (one cell zooms, rotates, transforms freely or distorts); perspective moves the box corners (top left, top right, bottom left, bottom right) to corners, which must make a convex quad, through a projective map. Inflate applies p += (p-center)*amount*brushWeight to current positions in sample order; finite amount in [-1,1], positive expands and negative contracts. At most 4096 samples. Brush kinds include line {radius, angle}: a strip with positive half-width radius, angle in radians, zero horizontal in node-local coordinates, linear falloff across the strip. The vertices a tool works on come from selected, rect or lasso (brush, inflate, path_deform, warp, perspective and reset default to every vertex; reset with none of them returns the whole key to the rest shape). Optional mirror {horizontal, vertical, origin} and zoom; brush, inflate, path_deform, warp and perspective do not apply mirror. space canvas (translate with selected only, PAR18) reads delta as a canvas move of the vertices as drawn at the current parameter values and solves each vertex's local move through the deformers above it (Newton steps); it answers {id, residual} and is refused, changing nothing, when a vertex does not follow the key at the current values or the solve does not converge. Set the parameter onto the edited key first: between keys the key moves by more than delta. Physics bending a PathDeformer and preview poses are not part of the solve.",
            schema(
                &[
                    ("id", integer_schema()),
                    ("parameter", integer_schema()),
                    ("key", pair_integer_schema()),
                    ("tool", string_schema()),
                    ("expectedRevision", json!({"type":"integer","minimum":0})),
                    ("selected", json!({ "type": "array", "items": { "type": "integer", "minimum": 0 } })),
                    ("rect", json!({ "type": "array", "items": number_schema(), "minItems": 4, "maxItems": 4 })),
                    ("lasso", json!({ "type": "array", "items": pair_schema(), "minItems": 3 })),
                    ("delta", pair_schema()),
                    ("brush", json!({ "type": "object" })),
                    ("stroke", json!({ "type": "array", "items": pair_schema(), "minItems": 2 })),
                    ("samples", json!({"type":"array","minItems":1,"maxItems":crate::mesh_edit::MAX_INFLATE_SAMPLES,
                        "items":{"type":"object","properties":{"center":pair_schema(),"amount":{"type":"number","minimum":-1,"maximum":1}},
                            "required":["center","amount"],"additionalProperties":false}})),
                    ("flow", json!({ "type": "boolean" })),
                    ("snapAxis", json!({ "type": "boolean" })),
                    ("path", json!({ "type": "array", "items": pair_schema(), "minItems": 2 })),
                    ("target", json!({ "type": "array", "items": pair_schema(), "minItems": 2 })),
                    ("resolution", number_schema()),
                    ("mirror", json!({ "type": "object" })),
                    ("zoom", number_schema()),
                    ("grid", json!({ "type": "object", "properties": {
                        "columns": {"type": "integer", "minimum": 1, "maximum": 64},
                        "rows": {"type": "integer", "minimum": 1, "maximum": 64},
                        "points": { "type": "array", "items": pair_schema() },
                    }, "required": ["columns", "rows", "points"] })),
                    ("corners", json!({ "type": "array", "items": pair_schema(), "minItems": 4, "maxItems": 4 })),
                    ("space", json!({ "type": "string", "enum": ["local", "canvas"] })),
                ],
                &["id", "parameter", "key", "tool"],
            ),
        ),
        tool(
            "automesh_generate",
            "Generate a Part mesh from its texture alpha with the grid, contour, optimum or skeleton automesher. Option fields are clamped to their documented ranges. preset (contour and optimum only) names a built-in preset (Normal parts, Detailed mesh, Large parts, Small parts, Thin and minimum parts, Preserve edges) or one saved with automesh_preset_save; an option field of the call replaces that field of the preset. Replaces mesh.verts, uvs and indices as one undo step; a Part with deform bindings, welds or depth data is rejected. When the algorithm finds nothing to mesh (skeleton on a shape that thins to a point, contour with too few vertices) the call is not an error: it answers {changed:false, message:\"no mesh generated\"} and the Part keeps its mesh. Check changed and try another algorithm.",
            schema(
                &[
                    ("id", integer_schema()),
                    (
                        "algorithm",
                        json!({ "type": "string", "enum": ["grid", "contour", "optimum", "skeleton"] }),
                    ),
                    ("preset", string_schema()),
                    ("mask_threshold", number_schema()),
                    ("x_segments", number_schema()),
                    ("y_segments", number_schema()),
                    ("margin", number_schema()),
                    ("scale_x", json!({ "type": "array", "items": number_schema() })),
                    ("scale_y", json!({ "type": "array", "items": number_schema() })),
                    ("sampling_step", number_schema()),
                    ("min_distance", number_schema()),
                    ("max_distance", number_schema()),
                    ("scales", json!({ "type": "array", "items": number_schema() })),
                    ("large_threshold", number_schema()),
                    ("length_threshold", number_schema()),
                    ("ratio_threshold", number_schema()),
                    ("sharp_expand", number_schema()),
                    ("unsharp_expand", number_schema()),
                    ("unsharp_contract", number_schema()),
                    ("div_per_part", number_schema()),
                    ("target_point_count", number_schema()),
                ],
                &["id", "algorithm"],
            ),
        ),
        tool(
            "automesh_preset_save",
            "Keep the option fields of a contour or optimum automesh call under name, for automesh_generate and the automesh of split_apply to use as preset. preset may name the built-in preset the fields start from. The presets belong to the user of this machine, not to the document: they are a file in the settings directory, and the call changes no document and adds no undo step. A saved name is replaced only with overwrite:true, and the answer then holds replaced:true. Refused: a built-in name, a name that is not 1 to 64 characters, has a control character anywhere or white space at either end, presets that would make the file larger than 1 MiB, the algorithms grid and skeleton, a field that automesh_generate refuses, a 65th preset, and a host without a settings directory.",
            schema(
                &[
                    ("name", string_schema()),
                    ("algorithm", json!({ "type": "string", "enum": ["contour", "optimum"] })),
                    ("preset", string_schema()),
                    ("mask_threshold", number_schema()),
                    ("sampling_step", number_schema()),
                    ("min_distance", number_schema()),
                    ("max_distance", number_schema()),
                    ("scales", json!({ "type": "array", "items": number_schema() })),
                    ("large_threshold", number_schema()),
                    ("length_threshold", number_schema()),
                    ("ratio_threshold", number_schema()),
                    ("sharp_expand", number_schema()),
                    ("unsharp_expand", number_schema()),
                    ("unsharp_contract", number_schema()),
                    ("div_per_part", number_schema()),
                    ("overwrite", json!({ "type": "boolean" })),
                ],
                &["name", "algorithm"],
            ),
        ),
        tool(
            "automesh_preset_delete",
            "Delete a preset that automesh_preset_save kept. A name that is not saved is refused, and so is a built-in name.",
            schema(&[("name", string_schema())], &["name"]),
        ),
        tool(
            "automesh_preset_list",
            "List the presets automesh_preset_save kept, as {presets: {name: {algorithm, preset?, option fields}}}. The built-in presets are not listed. The resource resource://soul2d/automesh/presets returns the same. The three preset tools answer with any document open.",
            schema(&[], &[]),
        ),
        tool(
            "deform_continue",
            "Write the next whole cells of a pending all-keypoints deform refresh.",
            schema(&[], &[]),
        ),
        tool(
            "part_set_weld",
            "Weld two Parts. Each index names the target vertex for that part vertex, or -1. Without indices each \
             part vertex takes the nearest target vertex within 4 canvas units at rest (a target vertex goes to the \
             nearest part vertex that claims it); weight defaults to 0.5. Answers {part, target, indices}.",
            schema(
                &[
                    ("part", integer_schema()),
                    ("target", integer_schema()),
                    ("indices", json!({ "type": "array", "items": { "type": "integer" } })),
                    ("weight", number_schema()),
                ],
                &["part", "target"],
            ),
        ),
        tool(
            "part_clear_weld",
            "Remove the reciprocal weld between two Parts.",
            schema(
                &[("part", integer_schema()), ("target", integer_schema())],
                &["part", "target"],
            ),
        ),
        tool(
            "node_convert",
            "Convert structural nodes to another supported type.",
            schema(
                &[
                    ("className", json!({ "type": "string", "description": "toType is read when className is absent" })),
                    ("nodes", node_ids_schema()),
                ],
                &[],
            ),
        ),
        tool(
            "node_transform",
            "Apply local transform components or a full transform payload.",
            schema(
                &[
                    ("id", integer_schema()),
                    ("component", string_schema()),
                    ("value", number_schema()),
                    ("translation", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("rotation", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("scale", pair_schema()),
                    ("zsort", number_schema()),
                ],
                &["id"],
            ),
        ),
        tool(
            "undo",
            "Undo the latest persistent edit.",
            schema(&[], &[]),
        ),
        tool("redo", "Redo the next persistent edit.", schema(&[], &[])),
        tool("history_summary", "Read retained undo history position, capacity and state flags without changing the session.", schema(&[], &[])),
        tool("history_limit_set", "Set how many undo states the history retains (2 to 1000, default 100). Shrinking drops the oldest states, never the current one; the document is untouched.",
            schema(&[("limit", json!({"type":"integer","minimum":2,"maximum":1000}))], &["limit"])),
        tool("default_form_lock_set", "Lock or unlock the default forms (Cubism \"Lock default forms\") of an INX \
            document. While locked, an edit that changes or unsets a keyform set at a parameter's default value \
            is refused. Not saved and not an undo step; a new document starts unlocked. Read back as \
            defaultFormLocked.",
            schema(&[("locked", json!({"type":"boolean"}))], &["locked"])),
        tool("view_list", "List the views of the document (Cubism multiview): [{id, active}], the active id and \
            synchronized. Each view keeps its own parameter values; value_set, render and the other tools use the \
            active view's. A document starts with view 1. Not saved and not an undo step.",
            schema(&[], &[])),
        tool("view_add", "Add a view that shows the default forms and make it active. At most 16 views. Not an \
            undo step. INX and native CMO3 documents.",
            schema(&[], &[])),
        tool("view_select", "Make a view active: the values of the view that was active are kept with it, and the \
            values of this view come back without removed parameters; a value outside its current range falls back \
            to the default. The view's Depth Bone pose preview comes back with it, and the random pose, motion and \
            physics previews end. While synchronized, only the active view changes.",
            schema(&[("id", json!({"type":"integer","minimum":1}))], &["id"])),
        tool("view_remove", "Remove a view. The last view cannot be removed; removing the active view makes the \
            next view active, or the previous one when it was the last.",
            schema(&[("id", json!({"type":"integer","minimum":1}))], &["id"])),
        tool("view_sync_set", "Synchronize parameter values across views (Cubism \"Synchronize parameter values \
            across tabs\"). While on, every view shows the active values. Turning it off leaves every view with \
            the values they shared and their pose preview; false while off changes nothing.",
            schema(&[("synchronized", json!({"type":"boolean"}))], &["synchronized"])),
        tool("history_clear", "Forget every retained undo state except the current one without changing the document.", schema(&[], &[])),
        tool("history_goto", "Restore a retained undo state by index when the expected revision matches.", schema(&[
            ("index", json!({"type":"integer","minimum":0})),
            ("expectedRevision", json!({"type":"integer","minimum":0})),
        ], &["index", "expectedRevision"])),
        tool(
            "save",
            "Atomically save the active INP/INX or native CMO3 document, preserving shared history and external-file conflict checks.",
            schema(
                &[
                    ("path", string_schema()),
                    ("mark_saved", json!({ "type": "boolean" })),
                ],
                &["path"],
            ),
        ),
        tool(
            "export",
            "Return the current INP container bytes.",
            schema(&[], &[]),
        ),
        tool(
            "binding_set_trs",
            "Set node transform bindings at a 2D parameter key.",
            schema(
                &[
                    ("parameter", integer_schema()),
                    (
                        "nodes",
                        json!({ "type": "array", "items": integer_schema(), "minItems": 1 }),
                    ),
                    ("key", pair_integer_schema()),
                    ("translation", pair_schema()),
                    ("translation_z", number_schema()),
                    ("scale", pair_schema()),
                    ("rotation_degrees", number_schema()),
                    ("rotation_x", json!({
                        "type": "number",
                        "description": "Pitch binding in radians, added to transform.r.x."
                    })),
                    ("rotation_y", json!({
                        "type": "number",
                        "description": "Yaw binding in radians, added to transform.r.y."
                    })),
                ],
                &["parameter", "nodes", "key"],
            ),
        ),
        tool(
            "binding_set_deform",
            "Set vertex offsets at a parameter key for undriven nodes.",
            schema(&[
                ("parameter", integer_schema()),
                ("nodes", json!({"type":"array","items":integer_schema(),"minItems":1})),
                ("key", pair_integer_schema()),
                ("values", json!({"type":"array","items":number_schema()})),
            ], &["parameter", "nodes", "key", "values"]),
        ),
        tool(
            "evaluate",
            "Evaluate a model at runtime parameter values.",
            schema(&[("values", values_schema())], &[]),
        ),
        tool(
            "scene_evaluate",
            "Read effective world matrices and local geometry offsets after runtime deformation, and `worldOffsets`: the \
                model-space offsets a drawn Part takes after glue (form edits in Adjustment Mode 2).",
            schema(&[("values", values_schema()), ("deformations", json!({
                "type": "object", "additionalProperties": {
                    "type": "array", "items": pair_schema()
                }
            }))], &[]),
        ),
        tool(
            "value_set",
            "Set one session-only parameter value.",
            schema(
                &[("id", integer_schema()), ("value", pair_schema())],
                &["id", "value"],
            ),
        ),
        tool(
            "scene_ambient_set",
            "Set the session-only scene ambient light (r, g, b each 0..1, default 1,1,1) applied by render. Not an undo step.",
            schema(
                &[("color", json!({ "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 1 }, "minItems": 3, "maxItems": 3 }))],
                &["color"],
            ),
        ),
        tool(
            "render",
            "Render an INX or bounded native CMO3 scene to PNG bytes and optionally save them. Native rendering reports \
             its approximation warnings. solo (INX only) draws just the given nodes and the nodes below them; masks \
             still clip. solo_cover [r, g, b, a] draws them over the rest of the picture covered by that colour, as \
             the editor's Solo view does.",
            schema(
                &[
                    ("path", string_schema()),
                    ("width", integer_schema()),
                    ("height", integer_schema()),
                    ("bounds", quad_schema()),
                    ("values", values_schema()),
                    ("solo", json!({"type": "array", "items": integer_schema()})),
                    ("solo_cover", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                        "minItems": 4, "maxItems": 4})),
                ],
                &["width", "height", "bounds"],
            ),
        ),
        tool(
            "render_packet",
            "Build an immutable render packet for the current INX or bounded native CMO3 scene; native packets include approximation warnings and reject manual deformations.",
            schema(
                &[
                    ("values", values_schema()),
                    ("deformations", json!({
                        "type": "object",
                        "additionalProperties": {
                            "type": "array", "items": pair_schema()
                        }
                    })),
                ],
                &[],
            ),
        ),
    ];
    tools.extend([
        tool(
            "ParamCommand_Add1DParameter",
            "Create a 1D parameter using D command arguments.",
            schema(
                &[
                    ("min", number_schema()),
                    ("max", number_schema()),
                    ("name", string_schema()),
                ],
                &["min", "max"],
            ),
        ),
        tool(
            "ParamCommand_Add2DParameter",
            "Create a 2D parameter using D command arguments.",
            schema(
                &[
                    ("min", number_schema()),
                    ("max", number_schema()),
                    ("name", string_schema()),
                ],
                &["min", "max"],
            ),
        ),
        tool(
            "ParamPropCommand_SetParameterName",
            "Rename an explicit parameter UUID.",
            schema(
                &[("id", integer_schema()), ("name", string_schema())],
                &["id", "name"],
            ),
        ),
        tool(
            "NodeCommand_AddNode",
            "Create Node children under context.nodes or the root.",
            schema(
                &[
                    ("className", string_schema()),
                    ("_suffix", string_schema()),
                    ("parents", json!({ "type": "array", "items": integer_schema() })),
                    ("context", context_nodes_schema()),
                ],
                &["className", "_suffix"],
            ),
        ),
        tool(
            "NodeCommand_SetNodeName",
            "Rename one node: id with name, or one context.nodes entry with one newNames entry.",
            schema(
                &[
                    ("id", integer_schema()),
                    ("name", string_schema()),
                    ("context", context_nodes_schema()),
                    ("newNames", json!({ "type": "array", "items": string_schema(), "minItems": 1, "maxItems": 1 })),
                ],
                &[],
            ),
        ),
        tool(
            "NodeCommand_MoveNode",
            "Reparent context.nodes under parent (newParent is read when parent is absent) at index.",
            context_schema(
                &[
                    ("newParent", integer_schema()),
                    ("parent", integer_schema()),
                    ("index", integer_schema()),
                ],
                &[],
            ),
        ),
        tool(
            "NodeCommand_DeleteNode",
            "Delete context.nodes and their subtrees.",
            context_schema(&[], &[]),
        ),
        tool(
            "NodeCommand_CopyNode",
            "Copy context.nodes into the session clipboard.",
            context_schema(&[], &[]),
        ),
        tool(
            "NodeCommand_CutNode",
            "Cut context.nodes into the session clipboard.",
            context_schema(&[], &[]),
        ),
        tool(
            "NodeCommand_PasteNode",
            "Paste the session clipboard under parent (newParent is read when parent is absent), or under the first of \
             context.nodes when neither is given.",
            schema(
                &[
                    ("parent", integer_schema()),
                    ("newParent", integer_schema()),
                    ("index", integer_schema()),
                    ("context", context_nodes_schema()),
                ],
                &[],
            ),
        ),
        tool(
            "NodeCommand_ConvertTo",
            "Convert context.nodes to className (toType is read when className is absent).",
            context_schema(&[("className", string_schema()), ("toType", string_schema())], &[]),
        ),
        tool(
            "NodeCommand_ToggleVisibility",
            "Toggle visibility of the first context.nodes entry.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_Node",
            "Convert context.nodes to Node.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_Composite",
            "Convert context.nodes to Composite.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_MeshGroup",
            "Convert context.nodes to MeshGroup.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_DynamicComposite",
            "Convert context.nodes to DynamicComposite.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_Camera",
            "Convert context.nodes to Camera.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_Mask",
            "Convert context.nodes to Mask.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_GridDeformer",
            "Convert context.nodes to GridDeformer.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_PathDeformer",
            "Convert context.nodes to PathDeformer.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_ConvertTo_Part",
            "Convert context.nodes to Part.",
            context_schema(&[], &[]),
        ),
        tool(
            "Node_Add_GridDeformer",
            "Add a GridDeformer under context.nodes or the root.",
            schema(&[("_suffix", string_schema()), ("context", context_nodes_schema())], &["_suffix"]),
        ),
        tool(
            "Node_Add_PathDeformer",
            "Add a PathDeformer under context.nodes or the root.",
            schema(&[("_suffix", string_schema()), ("context", context_nodes_schema())], &["_suffix"]),
        ),
        tool(
            "Node_Insert_GridDeformer",
            "Insert a GridDeformer above each context node.",
            context_schema(&[("_suffix", string_schema())], &["_suffix"]),
        ),
        tool(
            "Node_Insert_PathDeformer",
            "Insert a PathDeformer above each context node.",
            context_schema(&[("_suffix", string_schema())], &["_suffix"]),
        ),
        tool(
            "VertexCommand_DefineVertices",
            "Set control vertices on PathDeformer and GridDeformer nodes.",
            context_schema(&[("vertices", json!({ "type": "array", "items": number_schema() }))], &["vertices"]),
        ),
        tool(
            "VertexCommand_DefineGrid",
            "Define GridDeformer axes.",
            context_schema(
                &[
                    ("axisX", json!({ "type": "array", "items": number_schema() })),
                    ("axisY", json!({ "type": "array", "items": number_schema() })),
                ],
                &["axisX", "axisY"],
            ),
        ),
        tool(
            "VertexCommand_DefineMesh",
            "Define a MeshGroup mesh. Other drawables stay unsupported.",
            context_schema(
                &[
                    ("vertices", json!({ "type": "array", "items": number_schema() })),
                    ("indices", json!({ "type": "array", "items": integer_schema() })),
                ],
                &["vertices", "indices"],
            ),
        ),
        tool(
            "Inspector_Apply_GridDeformDynamic",
            "Set GridDeformer dynamic.",
            context_schema(&[("value", json!({ "type": "boolean" }))], &["value"]),
        ),
        tool(
            "Inspector_Apply_MeshGroupDynamic",
            "Set MeshGroup dynamic deformation.",
            context_schema(&[("value", json!({ "type": "boolean" }))], &["value"]),
        ),
        tool(
            "Inspector_Apply_MeshGroupTranslateChildren",
            "Set MeshGroup translate_children.",
            context_schema(&[("value", json!({ "type": "boolean" }))], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformDynamic",
            "Set PathDeformer dynamic deformation.",
            context_schema(&[("value", json!({ "type": "boolean" }))], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformPhysicsEnabled",
            "Enable or disable the PathDeformer pendulum driver.",
            context_schema(&[("value", json!({ "type": "boolean" }))], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformCurveType",
            "Set PathDeformer curve type to Spline or Bezier.",
            context_schema(&[("value", string_schema())], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformGravity",
            "Set PathDeformer pendulum gravity.",
            context_schema(&[("value", number_schema())], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformRestoreConstant",
            "Set PathDeformer pendulum restore constant.",
            context_schema(&[("value", number_schema())], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformDamping",
            "Set PathDeformer pendulum damping.",
            context_schema(&[("value", number_schema())], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformInputScale",
            "Set PathDeformer pendulum input scale.",
            context_schema(&[("value", number_schema())], &["value"]),
        ),
        tool(
            "Inspector_Apply_PathDeformPropagateScale",
            "Set PathDeformer pendulum propagate scale.",
            context_schema(&[("value", number_schema())], &["value"]),
        ),
        tool(
            "DepthBoneCommand_CreateDepthRigRoot",
            "Create a DepthRigRoot under parent, else under the first of context.nodes, else under the root. Save keeps \
             it; export drops it.",
            schema(
                &[("name", string_schema()), ("parent", integer_schema()), ("context", context_nodes_schema())],
                &[],
            ),
        ),
        tool(
            "DepthBoneCommand_AddStandardDepthParameters",
            "Create standard face and body parameters and every D standardDepthBoneBindingSpecs row.",
            schema(&[("root", integer_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthBoneCommand_FitDepthRigRootZToDepth",
            "Fit descendant DepthBone translation Z values to their current mapped depths.",
            schema(&[("root", integer_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthBoneCommand_FitDepthBoneZToDepth",
            "Fit one DepthBone translation Z value to its current mapped depth.",
            schema(&[("bone", integer_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthBoneCommand_SetDepthBoneRest",
            "Set a DepthBone rest head, tail, and roll. One undo restores the deform refresh.",
            schema(
                &[
                    ("bone", integer_schema()),
                    ("restHead", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("restTail", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("restRoll", number_schema()),
                ],
                &["bone", "restHead", "restTail", "restRoll"],
            ),
        ),
        tool(
            "DepthBoneCommand_SetDepthBoneConstraint",
            "Set DepthBone constraint fields. Locks affect pose. Hinge fields are stored only.",
            schema(
                &[
                    ("bone", integer_schema()),
                    ("constraint", json!({ "type": "object" })),
                ],
                &["bone", "constraint"],
            ),
        ),
        tool(
            "DepthBoneCommand_AddStandardDepthSkeleton",
            "Append the standard depth skeleton under a DepthRigRoot. Existing bones stay.",
            schema(
                &[("root", integer_schema()), ("scale", number_schema()), ("context", context_nodes_schema())],
                &[],
            ),
        ),
        tool(
            "DepthBoneCommand_ListDepthBones",
            "List depth bones under a DepthRigRoot.",
            schema(&[("root", integer_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "rig_fit_human",
            "Place named B2 bones on matching Part and Grid bounds, then store gaussian vertexWeights.",
            schema(&[("root", integer_schema())], &["root"]),
        ),
        tool(
            "rig_add_skeleton",
            "Append the standard depth skeleton under a DepthRigRoot.",
            schema(
                &[("root", integer_schema()), ("scale", number_schema())],
                &["root"],
            ),
        ),
        tool(
            "rig_list_bones",
            "List depth bones under a DepthRigRoot.",
            schema(&[("root", integer_schema())], &["root"]),
        ),
        tool(
            "rig_overlay",
            "Read DepthBone overlay points, links, and effective yaw pivots. Does not draw or edit.",
            schema(
                &[("root", integer_schema()), ("selected", integer_schema())],
                &["root"],
            ),
        ),
        tool(
            "rig_pick_bone",
            "Pick the closest DepthBone overlay point within a radius. Does not edit.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("point", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("radius", number_schema()),
                ],
                &["root", "point", "radius"],
            ),
        ),
        tool(
            "rig_gpu_offsets",
            "Compare the software deform kernel with the CPU offsets. Does not edit.",
            schema(
                &[("root", integer_schema()), ("parameter", integer_schema())],
                &["root"],
            ),
        ),
        tool(
            "rig_solve_ik",
            "Move a DepthBone chain toward a rig-local target with CCD, then refresh deform.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("endEffector", integer_schema()),
                    ("target", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                ],
                &["root", "endEffector", "target"],
            ),
        ),
        tool(
            "rig_apply_mmd_pose",
            "Write an MMD pose into one keypoint of a parameter on a standard-skeleton DepthRigRoot: a .vpd file, or a .vmd file sampled at `frame` (30 fps). Bones are mapped by name; the pose replaces the keypoint's whole pose, so a bone the file does not name is written at its MMD identity (what undoes its parent, or zero where the parameter already drives it), and the keypoint deform is refreshed in one undo. `key` defaults to the keypoint closest to the parameter's current value. Pelvis translation uses `translationScale` (rig units per MMD unit) or a scale derived from the rig. `boneTable` overrides the name table per MMD bone name: a rig boneId (such as \"Head\") reads a non-standard name as that bone's standard MMD counterpart, null ignores a name; each rig bone keeps one source, so the standard name it replaces must be mapped to null too. `storeBoneTable` saves that table with the document in the same undo step (an empty table removes it); a stored table is used when `boneTable` is absent, and the report's `boneTable` says which applied. `dryRun` returns the report without editing. The last report, dry run or not, is readable at resource://soul2d/rig/mmd-report.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("parameter", integer_schema()),
                    ("key", pair_integer_schema()),
                    ("path", string_schema()),
                    ("frame", json!({ "type": "number", "minimum": 0, "maximum": 1000000 })),
                    ("translationScale", json!({ "type": "number", "exclusiveMinimum": 0 })),
                    ("boneTable", mmd_bone_table_schema()),
                    ("storeBoneTable", json!({ "type": "boolean" })),
                    ("dryRun", json!({ "type": "boolean" })),
                ],
                &["root", "parameter", "path"],
            ),
        ),
        tool(
            "rig_import_gltf",
            "Add the standard 19-bone skeleton below an empty DepthRigRoot at the rest positions of the humanoid in a glTF 2.0 or VRM file (.gltf, .glb, .vrm). Bones come from the VRM humanoid table (0.x and 1.0), or from Mixamo-style node names in a plain glTF. Only the skeleton is read: meshes, textures, morphs, physics and animation are listed in the returned loss report as dropped, so the call needs lossy=true. unitsPerSourceUnit is the number of canvas units for one metre of the file and is required. A VRM states its terms in `meta`, and a plain glTF may state them in `asset.copyright`: the report shows them under `rights` (`truncated` names `rights` when that view is not all the file states), they are kept with the document, and the call needs acknowledgeRights=true, which only the user may decide. A file whose terms prohibit modification is refused. INX documents only. While `needs` names an argument, or with dryRun=true, the report is returned and nothing is written (`applied` false). The last applied report is readable at resource://soul2d/rig/import-report. One undo step.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("path", string_schema()),
                    ("unitsPerSourceUnit", json!({ "type": "number", "exclusiveMinimum": 0 })),
                    ("lossy", json!({ "type": "boolean" })),
                    ("acknowledgeRights", json!({ "type": "boolean" })),
                    ("dryRun", json!({ "type": "boolean" })),
                ],
                &["root", "path"],
            ),
        ),
        tool(
            "mesh_import_gltf",
            "Prototype (issue #244): import the meshes of a glTF 2.0 or VRM file (.gltf, .glb, .vrm) as Parts seen from the front. Each triangle primitive with a base colour texture becomes one Part below the document root, placed where the DepthRigRoot `root` puts its bones: x and y are kept, z becomes the depth of each vertex, triangles that turn away from the viewer are not drawn and the others are drawn far to near. A skinned mesh is bound to the standard bones below `root` with the skin weights of each vertex; the weight on a bone that is no standard bone goes to the standard bone above it. rig_import_gltf must have added the bones from the same file with the same unitsPerSourceUnit, and the call is refused when they are not where the file puts them, or when `root` is moved or turned out of the picture plane. Only buffers and PNG images inside the file are read (the GLB chunk or data URIs). Morph targets, normals and further attribute sets are listed in the returned loss report as dropped and shading as approximated, so the call needs lossy=true. unitsPerSourceUnit is the number of canvas units for one metre of the file and is required. When the file states rights, the document must already keep them (rig_import_gltf does) and the call needs acknowledgeRights=true, which only the user may decide. INX documents only. While `needs` names an argument, with dryRun=true, or when the file has no primitive to import, the report is returned and nothing is written (`applied` false). The last applied report is readable at resource://soul2d/mesh/import-report. One undo step.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("path", string_schema()),
                    ("unitsPerSourceUnit", json!({ "type": "number", "exclusiveMinimum": 0 })),
                    ("lossy", json!({ "type": "boolean" })),
                    ("acknowledgeRights", json!({ "type": "boolean" })),
                    ("dryRun", json!({ "type": "boolean" })),
                ],
                &["root", "path"],
            ),
        ),
        tool(
            "rig_bake_mmd_motion",
            "Bake a .vmd motion into one axis of a 1D or 2D parameter on a standard-skeleton DepthRigRoot: the motion is sampled at each VMD frame in `frames` (strictly increasing whole numbers, 30 fps) and sample i is written into keypoint i along `axis` (the other axis of a 2D parameter stays at keypoint `other`, default 0), with the same pose replacement as rig_apply_mmd_pose. The parameter must already have at least as many keypoints on that axis as there are frames. With `animation`, an animation of that name is created or replaced (its other lanes are dropped) with one linear lane that moves the parameter through those keypoints, one animation frame per VMD frame counted from the first sampled frame. `morphs` (needs `animation`) maps MMD morph names to parameter axes: each mapped morph the file holds becomes one more linear lane of that animation, its weight (linear between morph keys) sampled at the same frames and mapped from 0..1 onto `range` (default: the axis minimum to maximum); the report lists the values, and a mapped name the file lacks as `inFile: false`. `boneTable` and `storeBoneTable` work as in rig_apply_mmd_pose. One undo; `dryRun` returns the report without editing. The last report, dry run or not, is readable at resource://soul2d/rig/mmd-report.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("parameter", integer_schema()),
                    ("path", string_schema()),
                    ("frames", json!({ "type": "array", "items": { "type": "integer", "minimum": 0, "maximum": 1000000 }, "minItems": 1, "maxItems": 256 })),
                    ("axis", json!({ "type": "integer", "minimum": 0, "maximum": 1 })),
                    ("other", json!({ "type": "integer", "minimum": 0 })),
                    ("animation", string_schema()),
                    ("translationScale", json!({ "type": "number", "exclusiveMinimum": 0 })),
                    ("boneTable", mmd_bone_table_schema()),
                    ("storeBoneTable", json!({ "type": "boolean" })),
                    (
                        "morphs",
                        json!({
                            "type": "object",
                            "maxProperties": 64,
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "parameter": integer_schema(),
                                    "axis": { "type": "integer", "minimum": 0, "maximum": 1 },
                                    "range": { "type": "array", "items": number_schema(), "minItems": 2, "maxItems": 2 },
                                },
                                "required": ["parameter"],
                            },
                        }),
                    ),
                    ("dryRun", json!({ "type": "boolean" })),
                ],
                &["root", "parameter", "path", "frames"],
            ),
        ),
        tool(
            "rig_mirror",
            "Reflect closed DepthBone subtrees through the rig-local YZ plane and refresh deform.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("bones", json!({ "type": "array", "items": integer_schema(), "minItems": 1 })),
                    ("axis", string_schema()),
                ],
                &["root", "bones", "axis"],
            ),
        ),
        tool(
            "rig_drag_bone",
            "Move or rotate one DepthBone and refresh deform in that undo.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("bone", integer_schema()),
                    ("translation", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("rotation", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                ],
                &["root", "bone"],
            ),
        ),
        tool(
            "rig_set_vertex_weights",
            "Store vertexWeights on one binding. Each source row matches the target vertex count and sums to 1.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("target", integer_schema()),
                    ("weights", json!({ "type": "array" })),
                ],
                &["root", "target", "weights"],
            ),
        ),
        tool(
            "rig_clear_vertex_weights",
            "Remove vertexWeights from one binding and return to the gaussian score.",
            schema(
                &[("root", integer_schema()), ("target", integer_schema())],
                &["root", "target"],
            ),
        ),
        tool(
            "DepthBoneCommand_AddDepthBoneSource",
            "Add a DepthBone source to the binding of a GridDeformer, a PathDeformer or a Part on a DepthRigRoot. A bound Part is deformed at its own mesh vertices. The rig writes the deform keys of a target, which takes no manual deform key while a parameter drives the rig.",
            schema(
                &[("root", integer_schema()), ("target", integer_schema()), ("bone", integer_schema())],
                &["root", "target", "bone"],
            ),
        ),
        tool(
            "DepthBoneCommand_RemoveDepthBoneSource",
            "Remove a DepthBone source from the binding of a Grid, Path or Part target.",
            schema(
                &[("root", integer_schema()), ("target", integer_schema()), ("bone", integer_schema())],
                &["root", "target", "bone"],
            ),
        ),
        tool(
            "DepthBoneCommand_ListDepthBoneSources",
            "List depth bone sources for one Grid, Path or Part target.",
            schema(&[("root", integer_schema()), ("target", integer_schema())], &["root", "target"]),
        ),
        tool(
            "DepthBoneCommand_SetDepthBoneSourceSettings",
            "Set weight, depth offset, depth scale, and rotation for one bone source.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("target", integer_schema()),
                    ("bone", integer_schema()),
                    ("settings", json!({ "type": "object" })),
                ],
                &["root", "target", "bone", "settings"],
            ),
        ),
        tool(
            "DepthBoneCommand_SetDepthBoneInfluenceRule",
            "Set the influence rule for one Grid, Path or Part target.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("target", integer_schema()),
                    ("rule", json!({ "type": "object" })),
                ],
                &["root", "target", "rule"],
            ),
        ),
        tool(
            "DepthBoneCommand_GetDepthBoneInfluenceRule",
            "Read the influence rule for one Grid, Path or Part target.",
            schema(&[("root", integer_schema()), ("target", integer_schema())], &["root", "target"]),
        ),
        tool(
            "DepthBoneCommand_PreviewDepthBoneInfluence",
            "Read the influence rule and source ids. Does not write deformation.",
            schema(
                &[("root", integer_schema()), ("target", integer_schema()), ("bone", integer_schema())],
                &["root", "target", "bone"],
            ),
        ),
        tool(
            "DepthBoneCommand_PreviewDepthBoneDeform",
            "Write the current depth-bone deformation without history.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("targets", json!({ "type": "array", "items": integer_schema() })),
                    ("parameter", integer_schema()),
                    ("context", armed_context_schema()),
                ],
                &["root"],
            ),
        ),
        tool(
            "DepthBoneCommand_ApplyDepthBoneDeform",
            "Write depth-bone offsets into the current deform key of parameter.",
            schema(
                &[
                    ("root", integer_schema()),
                    ("targets", json!({ "type": "array", "items": integer_schema() })),
                    ("parameter", integer_schema()),
                    ("context", armed_context_schema()),
                ],
                &["root"],
            ),
        ),
        tool(
            "DepthBoneCommand_AddDepthBone",
            "Create a DepthBone under a DepthRigRoot or DepthBone.",
            schema(
                &[
                    ("parent", integer_schema()),
                    ("boneId", string_schema()),
                    ("restHead", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("restTail", json!({ "type": "array", "items": number_schema(), "minItems": 3, "maxItems": 3 })),
                    ("restRoll", number_schema()),
                    ("name", json!({ "type": "string", "description": "boneId is the name when name is absent" })),
                    ("context", context_nodes_schema()),
                ],
                &["restHead", "restTail"],
            ),
        ),
        tool(
            "DepthMapCommand_ListDepths",
            "List per-vertex depth values on a GridDeformer, a PathDeformer or a Part.",
            schema(&[("target", depth_target_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthMapCommand_SetDepths",
            "Set per-vertex depth values on a GridDeformer, a PathDeformer or a Part. Length must match the vertex count of the target.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("context", context_nodes_schema()),
                    ("depths", json!({ "type": "array", "items": number_schema() })),
                ],
                &["depths"],
            ),
        ),
        tool(
            "DepthMapCommand_ClearDepths",
            "Clear per-vertex depth values on a GridDeformer, a PathDeformer or a Part.",
            schema(&[("target", depth_target_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "depth_list",
            "List per-vertex depth values on a GridDeformer, a PathDeformer or a Part.",
            schema(&[("target", depth_target_schema())], &[]),
        ),
        tool(
            "depth_set",
            "Set per-vertex depth values on a GridDeformer, a PathDeformer or a Part, one for each vertex.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("depths", json!({ "type": "array", "items": number_schema() })),
                ],
                &["depths"],
            ),
        ),
        tool(
            "depth_clear",
            "Clear per-vertex depth values on a GridDeformer, a PathDeformer or a Part.",
            schema(&[("target", depth_target_schema())], &[]),
        ),
        tool(
            "DepthMapCommand_ListDepthOps",
            "List saved depth operations on a GridDeformer.",
            schema(&[("target", depth_target_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthMapCommand_SetDepthOps",
            "Replace saved depth operations on a GridDeformer.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("context", context_nodes_schema()),
                    ("operations", json!({ "type": "array" })),
                ],
                &["operations"],
            ),
        ),
        tool(
            "DepthMapCommand_AddDepthOp",
            "Add one saved depth operation.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("context", context_nodes_schema()),
                    ("operation", json!({ "type": "object" })),
                    ("index", json!({ "type": "integer" })),
                ],
                &["operation", "index"],
            ),
        ),
        tool(
            "DepthMapCommand_UpdateDepthOp",
            "Replace one saved depth operation.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("context", context_nodes_schema()),
                    ("index", json!({ "type": "integer" })),
                    ("operation", json!({ "type": "object" })),
                ],
                &["index", "operation"],
            ),
        ),
        tool(
            "DepthMapCommand_RemoveDepthOp",
            "Remove one saved depth operation.",
            schema(
                &[("target", depth_target_schema()), ("context", context_nodes_schema()), ("index", json!({ "type": "integer" }))],
                &["index"],
            ),
        ),
        tool(
            "DepthMapCommand_MoveDepthOp",
            "Move one saved depth operation.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("context", context_nodes_schema()),
                    ("fromIndex", json!({ "type": "integer" })),
                    ("toIndex", json!({ "type": "integer" })),
                ],
                &["fromIndex", "toIndex"],
            ),
        ),
        tool(
            "DepthMapCommand_ClearDepthOps",
            "Clear saved depth operations.",
            schema(&[("target", depth_target_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthMapCommand_ApplyDepthOps",
            "Bake saved depth operations into per-vertex depths.",
            schema(&[("target", depth_target_schema()), ("context", context_nodes_schema())], &[]),
        ),
        tool(
            "DepthMapCommand_ImportPSDDepths",
            "Sample a PSD depth map onto GridDeformer depths. One undo restores the edit.",
            schema(&psd_depth_properties(), &["path"]),
        ),
        tool(
            "depth_import_psd",
            "Sample a PSD depth map onto GridDeformer depths with explicit per-layer mappings and options. One undo restores the edit.",
            schema(&psd_depth_properties(), &["path"]),
        ),
        tool(
            "depth_import_psd_plan",
            "Dry-run depth_import_psd: layers with their automatic or chosen mapping, per-target results, retained-memory budget and D's diagnostics. Changes nothing.",
            schema(&psd_depth_properties(), &["path"]),
        ),
        tool(
            "depth_edit_apply",
            "Apply the depth editor's pending edits to one GridDeformer as ONE undo step (D applyToTargets). directEdits=true writes depths (clearing saved operations first); otherwise a changed operations list is saved and baked; an unchanged list does nothing.",
            schema(
                &[
                    ("target", depth_target_schema()),
                    ("operations", json!({ "type": "array" })),
                    ("depths", json!({ "type": "array", "items": number_schema() })),
                    ("directEdits", json!({ "type": "boolean" })),
                ],
                &["operations"],
            ),
        ),
        tool(
            "depth_draw_manifest_get",
            "Read the DepthDraw session: manifest (layers, bindings, selection, display, document size), which layers hold pixels, and the manifest check.",
            schema(&[], &[]),
        ),
        tool(
            "depth_draw_manifest_set",
            "Replace the DepthDraw session with a manifest (object, persistent JSON string, or null to clear) or the manifest file at path. Layers start without pixels; load them with depth_draw_load_png.",
            schema(&[("manifest", json!({})), ("path", string_schema())], &[]),
        ),
        tool(
            "depth_draw_load_png",
            "Load a PNG file as a DepthDraw layer (its RGBA is the depth). A saved layer with the same id and size gets the \
             pixels and keeps its settings, its sourcePath too. A relative path names a file beside the open document \
             when one is there, else in the working directory.",
            schema(&[("path", string_schema()), ("id", string_schema())], &["path"]),
        ),
        tool(
            "depth_draw_layer_update",
            "Update a DepthDraw layer: xyOffset/xyScale, backDepth/frontDepth/invert/zScale/zOffset, channel/convolution/customRadius/alphaThreshold, visible/enabled. Fields not given keep their value.",
            schema(
                &[
                    ("layer", string_schema()),
                    ("xyOffset", pair_schema()),
                    ("xyScale", pair_schema()),
                    ("backDepth", number_schema()),
                    ("frontDepth", number_schema()),
                    ("invert", json!({ "type": "boolean" })),
                    ("zScale", number_schema()),
                    ("zOffset", number_schema()),
                    ("channel", string_schema()),
                    ("convolution", string_schema()),
                    ("customRadius", json!({ "type": "integer" })),
                    ("alphaThreshold", number_schema()),
                    ("visible", json!({ "type": "boolean" })),
                    ("enabled", json!({ "type": "boolean" })),
                ],
                &["layer"],
            ),
        ),
        tool(
            "depth_draw_binding_update",
            "Change or create the binding of a DepthDraw layer to a GridDeformer/PathDeformer target: enabled, order, mergePolicy (Replace, Frontmost, Backmost, Add, KeepExistingWhereMissing), useNormalLayerAlpha, coverageThreshold.",
            schema(
                &[
                    ("layer", string_schema()),
                    ("target", integer_schema()),
                    ("enabled", json!({ "type": "boolean" })),
                    ("order", json!({ "type": "integer" })),
                    ("mergePolicy", string_schema()),
                    ("useNormalLayerAlpha", json!({ "type": "boolean" })),
                    ("coverageThreshold", number_schema()),
                ],
                &["layer", "target"],
            ),
        ),
        tool(
            "depth_draw_auto_bind",
            "Bind every DepthDraw layer to the Part (or, with matchDirectGridName, depth-mapped deformer) that matches its layer path, base name or name.",
            schema(&[("matchDirectGridName", json!({ "type": "boolean" }))], &[]),
        ),
        tool(
            "depth_draw_compose",
            "Compose the bound DepthDraw layers on one target (default: the selected one) without changing the document: depths, winning layers, per-layer statistics and what apply would change.",
            schema(&[("target", integer_schema())], &[]),
        ),
        tool(
            "depth_draw_apply",
            "Write the composed DepthDraw depths to the target as ONE undo step. A grid with saved depth operations gets them as its new base and re-bakes.",
            schema(&[("target", integer_schema())], &[]),
        ),
        tool(
            "depth_draw_cleanup",
            "Clean one layer's depth pixels: kind gap_fill (optional rules [{x,y,w,h,lift,radius}]) or contour_repair (thickness).",
            schema(
                &[
                    ("layer", string_schema()),
                    ("kind", json!({ "type": "string", "enum": ["gap_fill", "contour_repair"] })),
                    ("rules", json!({ "type": "array" })),
                    ("thickness", json!({ "type": "integer" })),
                ],
                &["layer", "kind"],
            ),
        ),
        tool(
            "depth_draw_fit_z",
            "Fit a layer's zScale/zOffset so its range on the target lies in a gap: gap adjacent, target, layers (backLayer, frontLayer) or values (back, front); margin; apply=false only reports.",
            schema(
                &[
                    ("layer", string_schema()),
                    ("target", integer_schema()),
                    ("gap", json!({ "type": "string", "enum": ["adjacent", "target", "layers", "values"] })),
                    ("backLayer", string_schema()),
                    ("frontLayer", string_schema()),
                    ("back", number_schema()),
                    ("front", number_schema()),
                    ("margin", number_schema()),
                    ("apply", json!({ "type": "boolean" })),
                ],
                &["layer"],
            ),
        ),
        tool(
            "depth_draw_export_png",
            "Export one grey depth PNG per DepthDraw layer plus a manifest into dir. Existing files are kept unless overwrite is true.",
            schema(
                &[("dir", string_schema()), ("manifest", string_schema()), ("overwrite", json!({ "type": "boolean" }))],
                &["dir"],
            ),
        ),
        tool(
            "depth_draw_diagnostics",
            "DepthDraw source counts, layer table, depth-space planes and gaps, and manifest check; composed per-layer statistics for the given or selected target.",
            schema(&[("target", integer_schema())], &[]),
        ),
        tool(
            "depth_draw_display",
            "Set DepthDraw display options: showNormalImage, showRawDepth, showCoverage, showComposite, showLayerPlanes, showDepthRanges, showMissingVertices, showWinningLayer, useGpuPreview.",
            schema(
                &[
                    ("showNormalImage", json!({ "type": "boolean" })),
                    ("showRawDepth", json!({ "type": "boolean" })),
                    ("showCoverage", json!({ "type": "boolean" })),
                    ("showComposite", json!({ "type": "boolean" })),
                    ("showLayerPlanes", json!({ "type": "boolean" })),
                    ("showDepthRanges", json!({ "type": "boolean" })),
                    ("showMissingVertices", json!({ "type": "boolean" })),
                    ("showWinningLayer", json!({ "type": "boolean" })),
                    ("useGpuPreview", json!({ "type": "boolean" })),
                ],
                &[],
            ),
        ),
        tool(
            "depth_draw_select",
            "Select a DepthDraw layer and/or target, the topmost layer plane at a document point [x, y], or clear the selection.",
            schema(
                &[
                    ("layer", string_schema()),
                    ("target", integer_schema()),
                    ("point", pair_schema()),
                    ("clear", json!({ "type": "boolean" })),
                ],
                &[],
            ),
        ),
        tool(
            "split_open",
            "Open a PNG, TGA, JPEG or PSD illustration as the part split draft source and clear its parts. A PSD is flattened: its visible layers are composited (groups, opacity, clipping and the Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, LinearDodge, ColorBurn, HardLight, SoftLight, Difference, Exclusion, Subtract, LinearBurn, LinearLight, Hue and Color blend modes; other modes draw as Normal) and the merged image stored in the file is not used. A PSD without layers is refused. The picture is limited to 8192 pixels a side and 16 megapixels. Does not touch the document or its history.",
            schema(&[("path", string_schema())], &["path"]),
        ),
        tool(
            "split_part_set",
            "Add or replace a part of the split draft. steps are evaluated in order into a coverage mask. Selection steps (alpha {threshold}, component {point, threshold}, flood {point, tolerance}, boundedFill {point, lineThreshold}, polygon {points}, box {rect: [x, y, w, h]}, part {name}, raster {path, crc32} from split_mask_inspect) take mode add (default), subtract or intersect; morphology steps are grow {radius}, shrink {radius}, feather {radius} and side {axisX, keep: left|right}. index is the draw order (0 is the back); padding is transparent pixels added around the texture (room for automesh and filtering; never colour). Returns the part's statistics.",
            schema(
                &[
                    ("name", string_schema()),
                    ("steps", json!({ "type": "array", "items": { "type": "object", "properties": { "op": { "type": "string" } }, "required": ["op"] } })),
                    ("padding", json!({ "type": "integer", "minimum": 0, "maximum": 64 })),
                    ("index", json!({ "type": "integer", "minimum": 0 })),
                ],
                &["name", "steps"],
            ),
        ),
        tool(
            "split_part_remove",
            "Remove a part of the split draft that no other part refers to.",
            schema(&[("name", string_schema())], &["name"]),
        ),
        tool(
            "split_part_order",
            "Set the draw order of the split draft, back to front, as a permutation of its part names.",
            schema(&[("names", json!({ "type": "array", "items": string_schema() }))], &["names"]),
        ),
        tool(
            "split_plan",
            "The split plan JSON (source and parts, replayable by split_apply plan) with per-part statistics: pixels, texture bbox, mean coverage and alpha, overlaps, and the opaque pixels no part claims or several parts claim.",
            schema(&[], &[]),
        ),
        tool(
            "split_components",
            "List alpha > threshold (default 0) four-connected islands of the loaded split source, in row-major seed order. Read-only; returns up to 32 {seed, pixels, bbox:[x,y,w,h]}, exact totalComponents and nextOffset (null when exhausted). Every page rescans the whole image; reuse the same source and threshold when following nextOffset. Use a seed in a split_part_set component step to select that island.",
            schema(&[
                ("threshold", json!({"type":"integer", "minimum":0, "maximum":255})),
                ("offset", json!({"type":"integer", "minimum":0, "maximum":4294967295u64})),
            ], &[]),
        ),
        tool(
            "split_mask_inspect",
            "Read a source-sized 8-bit grayscale PNG coverage mask and return its canonical raster step {op, path, crc32, mode:add}, width and height. Requires an open split source; read-only. Use the step in split_part_set, changing mode if needed. Keep the mask file: every evaluation and plan replay reloads it and checks the decoded-coverage CRC. This does not run or approve a model provider.",
            schema(&[("path", string_schema())], &["path"]),
        ),
        tool(
            "split_overlay",
            "A PNG of the split source with each part tinted and outlined; with part, only that one.",
            schema(&[("part", string_schema())], &[]),
        ),
        tool(
            "split_apply",
            "Create one Part per split part under parent (default: the root) in ONE undo step. plan replays a split_plan plan without an open draft. automesh (true, or an automesh_generate request) meshes each new Part in the same step.",
            schema(
                &[
                    ("parent", integer_schema()),
                    ("plan", json!({ "type": "object" })),
                    ("automesh", json!({ "anyOf": [{ "type": "boolean" }, { "type": "object" }] })),
                ],
                &[],
            ),
        ),
        tool(
            "split_close",
            "Drop the split draft source and parts.",
            schema(&[], &[]),
        ),
        tool(
            "EditCommand_Undo",
            "Undo the latest persistent edit.",
            schema(&[], &[]),
        ),
        tool(
            "EditCommand_Redo",
            "Redo the next persistent edit.",
            schema(&[], &[]),
        ),
        tool(
            "ModelCommand_SetRotationXBinding",
            "Set transform.r.x at an axis index. rotationRadians is radians and includes zero.",
            schema(
                &[
                    ("parameter", integer_schema()),
                    (
                        "nodes",
                        json!({ "type": "array", "items": integer_schema(), "minItems": 1 }),
                    ),
                    ("key", pair_integer_schema()),
                    ("rotationRadians", number_schema()),
                ],
                &["parameter", "nodes", "key", "rotationRadians"],
            ),
        ),
        tool(
            "ModelCommand_SetRotationYBinding",
            "Set transform.r.y at an axis index. rotationRadians is radians and includes zero.",
            schema(
                &[
                    ("parameter", integer_schema()),
                    (
                        "nodes",
                        json!({ "type": "array", "items": integer_schema(), "minItems": 1 }),
                    ),
                    ("key", pair_integer_schema()),
                    ("rotationRadians", number_schema()),
                ],
                &["parameter", "nodes", "key", "rotationRadians"],
            ),
        ),
        tool(
            "ModelCommand_SetTRSBinding",
            "Set bindings with explicit parameter, nodes, and key context.",
            schema(
                &[
                    ("parameter", integer_schema()),
                    (
                        "nodes",
                        json!({ "type": "array", "items": integer_schema(), "minItems": 1 }),
                    ),
                    ("key", pair_integer_schema()),
                    ("translation", pair_schema()),
                    ("scale", pair_schema()),
                    ("rotationDegrees", number_schema()),
                    ("applyRotation", json!({ "type": "boolean" })),
                ],
                &["parameter", "nodes", "key"],
            ),
        ),
        tool(
            "ViewCommand_CaptureLiveScreenshot",
            "Render the scene to a PNG image content item. width/height default to 640x480 and bounds to the scene bounds. context.parameters (one UUID) with context.parameterValue renders that parameter at the value without changing the session. The result _meta carries worldCapture, the world-to-image mapping. overlayObjects is not supported.",
            schema(
                &[
                    ("path", string_schema()),
                    ("width", integer_schema()),
                    ("height", integer_schema()),
                    ("bounds", quad_schema()),
                    ("values", values_schema()),
                    ("context", json!({"type": "object", "properties": {
                        "parameters": {"type": "array", "items": {"type": "integer"}, "minItems": 1, "maxItems": 1},
                        "parameterValue": pair_schema(),
                    }})),
                    ("overlayObjects", json!({"type": "array", "maxItems": 0})),
                ],
                &[],
            ),
        ),
    ]);
    let value = json!({});
    for (id, property) in INSPECTOR_NODE_PROPERTIES {
        tools.push(tool(
            &format!("Inspector_Apply_{id}"),
            &format!("Set the {property} property of the context node (node_property_set)."),
            context_schema(&[("value", value.clone())], &["value"]),
        ));
    }
    for (id, key) in INSPECTOR_PHYSICS_KEYS {
        tools.push(tool(
            &format!("Inspector_Apply_{id}"),
            &format!("Set SimplePhysics {key} on the context nodes (physics_set)."),
            context_schema(&[("value", value.clone())], &["value"]),
        ));
    }
    for (id, component) in INSPECTOR_TRANSFORM_COMPONENTS {
        tools.push(tool(
            &format!("Inspector_Apply_{id}"),
            &format!("Set the {component} transform component of the context node (node_transform)."),
            context_schema(&[("value", number_schema())], &["value"]),
        ));
    }
    let mut move_parent = integer_schema();
    move_parent["description"] = json!("newParent (an integer UUID) is read when parent is absent");
    tools.extend([
        tool("node_get", "Read one node record by UUID.", schema(&[("id", integer_schema())], &["id"])),
        tool("node_move", "Reparent nodes under parent, at index when given.",
            schema(&[("nodes", node_ids_schema()), ("parent", move_parent), ("index", integer_schema())], &[])),
        tool("node_delete", "Delete nodes and their subtrees.", schema(&[("nodes", node_ids_schema())], &[])),
        tool("deformer_define_mesh", "Define a MeshGroup mesh. Other drawables stay unsupported.",
            schema(&[
                ("nodes", node_ids_schema()),
                ("vertices", json!({ "type": "array", "items": number_schema() })),
                ("indices", json!({ "type": "array", "items": integer_schema() })),
            ], &["vertices", "indices"])),
    ]);
    for (name, sibling, required) in SIBLING_TOOLS {
        let mut copy = tools.iter().find(|tool| tool["name"] == *sibling).cloned().unwrap_or_default();
        copy["name"] = json!(name);
        copy["inputSchema"]["required"] = json!(required);
        if let Some(properties) = copy["inputSchema"]["properties"].as_object_mut() {
            properties.remove("context");
        }
        tools.push(copy);
    }
    // The classes the editor's Add and Insert menus offer besides the two deformers declared above.
    for class in ["Node", "Composite", "MeshGroup", "DynamicComposite", "Camera", "Mask", "DepthRigRoot", "DepthBone", "SimplePhysics"] {
        tools.push(tool(
            &format!("Node_Add_{class}"),
            &format!("Add a {class} under context.nodes or the root."),
            schema(&[("_suffix", string_schema()), ("context", context_nodes_schema())], &["_suffix"]),
        ));
    }
    for class in ["Node", "Composite", "MeshGroup", "DynamicComposite", "Camera"] {
        tools.push(tool(
            &format!("Node_Insert_{class}"),
            &format!("Insert a {class} above each context node."),
            context_schema(&[("_suffix", string_schema())], &["_suffix"]),
        ));
    }
    tools.extend([
        tool("import_inp", "Open an INP/INX file as a new unsaved project (no project path).",
            schema(&[("path", string_schema())], &["path"])),
        tool("merge_inp", "Merge another INP/INX file's nodes, parameters and animations into the open project in one undo step.",
            schema(&[("path", string_schema()), ("mergeParameters", json!({ "type": "boolean" })),
                ("mergeAnimations", json!({ "type": "boolean" }))], &["path"])),
        tool("import_folder", "Import the PNG/TGA files of a folder as one Part each into a new unsaved project.",
            schema(&[("path", string_schema())], &["path"])),
        tool("merge_images", "Add one Part per PNG/TGA file under parent (default root) in one undo step.",
            schema(&[("paths", json!({ "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] })),
                ("parent", integer_schema())], &["paths"])),
        tool("tool_regenerate_node_ids", "Give every node a new UUID and rewrite references to them.",
            schema(&[], &[])),
        tool("tool_repair", "Attempt full repair: regenerates node UUIDs.", schema(&[], &[])),
        tool("tool_generate_fake_layer_names", "Set every Part's layer path to /name.", schema(&[], &[])),
        tool("tool_premultiply_textures", "Premultiply every texture by its alpha.", schema(&[], &[])),
        tool("tool_rebleed_textures", "Bleed colour into fully transparent pixels of every texture.", schema(&[], &[])),
        tool("tool_import_session_data", "Copy the Inochi Session tracking bindings from another INP/INX file.",
            schema(&[("path", string_schema())], &["path"])),
        tool("sketch_snapshot", "Keep the puppet as it shows now (the active parameter values and previews) as a \
             guide image: a Part named Snapshot at opacity (default 0.25) over the puppet bounds, under the root and drawn \
             in front of every other node (Cubism snapshot saved as a guide image). One undo step. INX documents.",
            schema(&[("opacity", json!({"type": "number", "minimum": 0, "maximum": 1}))], &[])),
        tool("view_screenshot", "Save a PNG of the whole puppet; the longest side is size pixels (default 1024).",
            schema(&[("path", string_schema()), ("size", integer_schema())], &["path"])),
        tool("values_reset", "Reset every parameter to its default value.", schema(&[], &[])),
        tool("editor_quit", "Action: Request a non-discarding editor exit after this identified reply is locally flushed and a final safety check passes. \
             Refuses unsaved changes, unapplied drafts, active tools/work and pending dialogs. Acceptance is conditional: new work or failed \
             delivery cancels exit. No Save/Discard decisions. ACP requires exact human lifecycle approval, the initiating uncancelled \
             end_turn and release of its owned client before the fresh final check. Headless, notifications and internal calls refuse.",
            closed_schema(&[], &[])),
        tool("editor_selection_set", "Action: select in the editor. nodes: these nodes, in this order (the first is the \
             primary); an empty list clears the node selection. bindings: these {node, channel} bindings of the armed \
             parameter at its key (the Parameters panel's binding rows). vertices: these indices of the active mesh \
             edit's points (with nodes, the first node must be the edit's node). Give at least one; omitted fields \
             stay as they are. A refused call changes nothing. Editor only: a host without a window refuses it. Not a \
             document edit.",
            closed_schema(&[
                ("nodes", json!({"type": "array", "items": {"type": "integer"}, "maxItems": EDITOR_MAX_NODES})),
                ("bindings", json!({"type": "array", "maxItems": EDITOR_MAX_NODES, "items": {"type": "object",
                    "properties": {"node": {"type": "integer"}, "channel": {"type": "string"}},
                    "required": ["node", "channel"], "additionalProperties": false}})),
                ("vertices", json!({"type": "array", "items": {"type": "integer", "minimum": 0}, "maxItems": EDITOR_MAX_VERTICES})),
            ], &[])),
        tool("editor_armed_set", "Action: arm a parameter in the editor so its keypoint is edited, or disarm with null. \
             The parameter value is not moved; the key is the keypoint closest to it. Editor only. Not a document edit.",
            closed_schema(&[("parameter", json!({"type": ["integer", "null"]}))], &["parameter"])),
        tool("editor_camera_set", "Action: move the editor viewport. centre is the world point in the middle of the view; \
             zoom is in points per world unit and must be within the editor's zoom limits. mirror sets Mirror View. \
             Give at least one; omitted fields stay unchanged. Session only. Editor only. Not a document edit.",
            closed_schema(&[("centre", pair_schema()), ("zoom", number_schema()), ("mirror", json!({"type":"boolean"}))], &[])),
        tool("editor_camera_sync", "Action: give every view the active view's centre and zoom (Same display position). \
             INX and CMO3 documents only. Mirror View stays global. Session only. Editor only. Not a document edit.",
            closed_schema(&[], &[])),
        tool("editor_native_parameters_set", "Action: set the CMO3/CAN3 native parameter panel's case-insensitive name-or-ID \
             substring filter. Empty clears it. Available without an attached model (no rows). Returns nativeParameterPanel \
             in editor state. Retained across document replacement, reset on restart. Editor only. No document, settings, \
             parameter value or playback change.", closed_schema(&[("filter", string_schema())], &["filter"])),
        tool("editor_parameters_set", "Action: set the INX Parameters panel's name filter, selected animation, or transient \
             parameter bookmark. An empty filter clears it; bookmark null shows all parameters. labelIndex selects a stored \
             label of the selected animation, or part and model-space point resolve the Part's related parameters. \
             Omitted fields stay unchanged; changing animation pauses playback, resets the local seek frame and clears the \
             old bookmark. Give at least one field. Returns parameterPanel in editor state. Editor only. No document or settings write.",
            closed_schema(&[("filter", string_schema()), ("animation", string_schema()),
                ("bookmark", json!({"oneOf":[{"type":"null"},
                    closed_schema(&[("labelIndex", json!({"type":"integer","minimum":0}))], &["labelIndex"]),
                    closed_schema(&[("part", integer_schema()), ("point", pair_schema())], &["part","point"])]}))], &[])),
        tool("editor_camera_fit", "Action: frame the given nodes in the editor viewport, or the whole model when nodes is \
             left out. Editor only. Not a document edit.",
            closed_schema(&[("nodes", json!({"type": "array", "items": {"type": "integer"}, "minItems": 1, "maxItems": EDITOR_MAX_NODES}))], &[])),
        tool("editor_tool_set", "Action: switch the tool of the editor's current edit mode to one of tool.offered in \
             resource://soul2d/editor/state. Editor only. Not a document edit.",
            closed_schema(&[("tool", string_schema())], &["tool"])),
        tool("editor_recovery_list", "Read: the crash-recovery snapshots that earlier sessions left for unsaved documents, \
             newest first, as the editor offers them at startup. Each id is the snapshot's file name. Editor only.",
            closed_schema(&[], &[])),
        tool("editor_recovery_keep", "Action: dismiss this window's startup recovery offer, as Keep for later does. \
             Recovery files remain on disk and are offered again at the next startup. Repeating this call succeeds. \
             A temporarily hidden offer may also be dismissed; this does not answer a waiting dialog, change the \
             document or its drafts, or write settings. Returns editor state. Editor only.",
            closed_schema(&[], &[])),
        tool("editor_recovery_restore", "Action: open the recovery snapshot with this id (from editor_recovery_list) as an \
             untitled, modified document, as the Restore button does. Refused like open while a draft is open or the \
             document has unsaved changes, and while a dialog in the editor waits for the user's answer. Editor only.",
            closed_schema(&[("id", string_schema())], &["id"])),
        tool("editor_settings_get", "Read: listed editor preferences (Settings controls and View > Stats for Nerds) with \
             their current values, types and allowed values or ranges, plus the shortcut bindings. Editor only.",
            closed_schema(&[], &[])),
        tool("editor_settings_set", "Action: change one setting that editor_settings_get lists with writable true, \
             validated, applied and saved through the shared preference setter. NerdStats is the Stats for Nerds window's \
             visibility boolean, off by default. An ACP request that saves settings waits for explicit host approval of \
             the original call (up to two minutes); do not retry while it waits. Editor only. Not a document edit.",
            closed_schema(&[("key", string_schema()), ("value", json!({"type": ["boolean", "integer", "string"]}))], &["key", "value"])),
        tool("editor_shortcut_set", "Action: bind one editor command to a keyboard shortcut, or unbind it with null, as the \
             Settings window's Shortcuts pane does; the settings file is saved. command is a command id from the shortcuts \
             list of editor_settings_get; shortcut is that list's spec form: Ctrl, Alt, Shift or Super and one key (A-Z, 0-9, \
             F1-F12, Left, Right, Up, Down) joined by +, for example Ctrl+Shift+S. A chord another command held moves to \
             this one and the result names the displaced command. repeat (default: the command's current flag) lets a held \
             key repeat the command. An ACP request waits for host approval like editor_settings_set. Editor only. Not a \
             document edit.",
            closed_schema(&[("command", string_schema()), ("shortcut", json!({"type": ["string", "null"]})),
                ("repeat", json!({"type": "boolean"}))], &["command", "shortcut"])),
        tool("editor_about_set", "Action: open or close the About window, as Help > About and its close button do. \
             Read windows.about in the returned editor state. Repeated values are harmless. Retained across \
             document replacement, layout reset and listener restart; closed on process restart. Editor only, \
             all document formats. No document, history or settings write; no ACP host approval is needed.",
            closed_schema(&[("open", json!({"type":"boolean"}))], &["open"])),
        tool("editor_welcome_set", "Action: open or close the Welcome window, as Help > Welcome does. Opening shows \
             Quick Setup until the quick setup is done and the start page after it; an open window keeps its page. \
             Read windows.welcome in the returned editor state. Repeated values are harmless; closing does not mark \
             the quick setup done. The editor opens the window at start until the quick setup is done. Retained across \
             document replacement, layout reset and listener restart. Its choices are editor_settings_set (DarkMode, \
             lang, UIScale), new, open and import_psd. Editor only, all document formats. No document, history or \
             settings write; no ACP host approval is needed.",
            closed_schema(&[("open", json!({"type":"boolean"}))], &["open"])),
        tool("editor_command_browser_set", "Action: open or close the Command Browser, as Edit > Command Browser, its \
             shortcut, close button and Escape do. Read windows.commandBrowser in the returned editor state. Repeated \
             open preserves the current search, selection, argument draft and result; closing clears them. Retained \
             across document replacement, layout reset and listener restart; closed on process restart. Editor only, \
             all document formats. No document, history or settings write; no ACP host approval is needed.",
            closed_schema(&[("open", json!({"type":"boolean"}))], &["open"])),
        tool("editor_texture_viewer_set", "Action: open or close Texture Viewer, as Tools > Texture Viewer and its close \
             button do. Read windows.textureViewer in the returned editor state. Repeated open preserves Slot, Zoom, \
             loaded images, loading and errors; close drops the viewer and reopening starts unloaded at Slot 0, Zoom 1. \
             Uses the existing selected-Part texture_get loading path. Retained across document replacement, layout \
             reset and listener restart; closed on process restart. Editor only, all document formats. No document, \
             history or settings write; no ACP host approval is needed.",
            closed_schema(&[("open", json!({"type":"boolean"}))], &["open"])),
        tool("editor_address_dialog", "Action: open, copy or close the editor's existing address dialog. Open requires \
             target: configuration, online_documentation, soul2d_live_documentation, bug_report or feature_request. \
             Configuration uses the settings path's parent and refuses when unavailable. No browser, folder or \
             external app is launched. Copy and close forbid target, including null. Copy requires an open dialog \
             and queues its original address through egui's clipboard output; the reply does not confirm OS \
             clipboard delivery. Close is idempotent. Read windows.addressDialog: null or {target,title,address}. \
             Retained across document replacement and layout reset, closed on process restart. Editor only, \
             all document formats. No document, history or settings write. Manifest effect clipboard-write \
             applies to copy; open and close only change window state.",
            closed_schema(&[
                ("action", json!({"type":"string", "enum":["open","copy","close"]})),
                ("target", json!({"type":"string", "enum":crate::window_shell::AddressDialogTarget::ALL.map(|target| target.id())})),
            ], &["action"])),
        tool("editor_viewport_set", "Action: set the editor viewport's selection overlays (vertices, bounds and orientation \
             gizmo) and opaque RGB background [r, g, b], each an integer from 0 to 255. [0, 0, 0] resets the background \
             to black. Give at least one; omitted fields stay unchanged. The overlays depend on the selection and edit \
             mode. partSort (INX only) orders Part menu candidates front-to-back (z_sort) or by ascending \
             bounding-box area (size_sort); it does not change direct hit testing. Sort updates outside INX \
             refuse the entire patch. Retained across document replacement, reset on restart. Session only. \
             Editor only. Not a document edit or a settings write.",
            closed_schema(&[
                ("vertices", json!({"type":"boolean"})), ("bounds", json!({"type":"boolean"})),
                ("orientation", json!({"type":"boolean"})),
                ("partSort", json!({"type":"string","enum":["z_sort","size_sort"]})),
                ("background", json!({"type":"array", "items":{"type":"integer", "minimum":0, "maximum":255},
                    "minItems":3, "maxItems":3})),
            ], &[])),
        tool("editor_grid_set", "Action: show or hide the canvas grid of the editor viewport and set its spacing in world \
             units (1 to 1000000) and its colour as unmultiplied [r, g, b, a]. The lines run through the \
             world origin and are not drawn when they would be closer than 4 points. Editor only. Not a document edit.",
            closed_schema(&[
                ("show", json!({"type": "boolean"})),
                ("spacing", json!({"type": "number", "minimum": 1, "maximum": 1000000})),
                ("colour", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                    "minItems": 4, "maxItems": 4})),
            ], &[])),
        tool("editor_solo_set", "Action: turn the Solo view of the editor viewport on or off (Cubism Solo) and set its \
             inactive colour as unmultiplied [r, g, b, a], whose alpha is its opacity. With Solo on, the selected \
             nodes of an INX document are drawn in front and the rest under the inactive colour. Editor only. Not a \
             document edit.",
            closed_schema(&[
                ("show", json!({"type": "boolean"})),
                ("colour", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                    "minItems": 4, "maxItems": 4})),
            ], &[])),
        tool("editor_onion_set", "Action: turn Onion Slice and Trajectories of the editor viewport on or off. With a frame of \
             an animation previewed (animation_seek), Onion Slice draws the frames around it faintly (animation_onion) \
             and Trajectories draws the paths the selected Part's vertices take (animation_trajectory). INX \
             documents. Editor only. Not a document edit.",
            closed_schema(&[("onion", json!({"type": "boolean"})), ("trajectories", json!({"type": "boolean"}))], &[])),
        tool("editor_playback_set", "Action: turn Lightweight playback of the editor on or off (Cubism Lightweight \
             playback) and the timeline's lock of playback to the animation framerate, either or both. With \
             lightweight on, while the window's Play runs an animation, a CAN3 scene or a Motion3 clip, the parameter \
             panels keep the values they showed before it started. With snapToFramerate on, the window's Play moves \
             an INX animation in whole frames of its timestep and starts from the whole frame below a fractional \
             one. Playback driven over MCP (animation_advance, motion3_advance) is not the window's and is \
             unaffected. Session only. Editor only. Not a document edit.",
            closed_schema(&[("lightweight", json!({"type": "boolean"})), ("snapToFramerate", json!({"type": "boolean"}))],
                &[])),
        tool("editor_timeline_zoom_set", "Action: set the Timeline's absolute display multiplier through the same stored value as its \
             Zoom out, slider, Zoom in and hover zoom gesture. Requires a finite number; rounds to the window's f32 and clamps to \
             0.5 through 10. Returns editor state with timeline.zoom reporting the stored value. Retained across animations, \
             document replacement and layout changes, including while Timeline is hidden, no animation is selected or the document \
             is not INX. A fresh editor starts at 1. Does not seek, pause, edit the document or save settings. Editor only.",
            closed_schema(&[("zoom", json!({"type":"number"}))], &["zoom"])),
        tool("editor_panel_set", "Action: show or hide one editor panel, outline or timeline, through the same operation as View > Panels. \
             The other panel, panel sizes, drafts and document history stay unchanged. Returns editor state. \
             Saves the setting when the editor has a settings file; a failed write reports that the change holds only for this session. \
             ACP calls that save settings wait for host Allow/Deny on the original HTTP call, for at most two minutes; do not retry while waiting. \
             Editor only. Not a document edit.",
            closed_schema(&[("panel", json!({"type":"string","enum":["outline","timeline"]})),
                ("visible", json!({"type":"boolean"}))], &["panel","visible"])),
        tool("editor_layout_reset", "Action: reset the editor layout like View > Reset Layout. Show panels whose visibility \
             was stored, forget Outline and Timeline sizes and floating window positions. Never-written visibility keys stay absent. \
             Open windows, collapsed state, saved workspaces, drafts and document history stay unchanged. Returns editor state. \
             Saves existing visibility settings when the editor has a settings file; a failed write reports a session-only reset. \
             ACP calls wait for host file-write approval on the original HTTP call; do not retry while waiting. Editor only.",
            closed_schema(&[], &[])),
        tool("editor_workspace_save", "Action: save the editor layout as a named workspace (Cubism Workspace): whether \
             the Outline and Timeline panels are shown, and their sizes. A name that exists is replaced. At most 16 \
             workspaces; names are trimmed, 1 to 64 characters without control characters. Kept in the editor \
             settings. Editor only. Not a document edit.",
            closed_schema(&[("name", json!({"type": "string"}))], &["name"])),
        tool("editor_workspace_apply", "Action: restore a saved workspace: show or hide the Outline and Timeline \
             panels and give them the saved sizes. Floating windows keep their places. Editor only. Not a \
             document edit.",
            closed_schema(&[("name", json!({"type": "string"}))], &["name"])),
        tool("editor_workspace_delete", "Action: delete a saved workspace. Editor only. Not a document edit.",
            closed_schema(&[("name", json!({"type": "string"}))], &["name"])),
        tool("editor_physics_preview_set", "Action: enable or disable the editor's live physics preview. Enabling requires \
             an INX document; disabling is allowed for every document format. The window keeps the flag across document \
             replacement; physics runs only for INX. A failed step or successful physics bake turns it off. \
             Session only. Editor only. Not a document edit or a settings write.",
            closed_schema(&[("enabled", json!({"type":"boolean"}))], &["enabled"])),
        tool("editor_guides_set", "Action: set the guide lines of the editor viewport (Cubism Guide Line). open shows or \
             closes the Guide Settings window, independently of show. lines replaces \
             every guide: each is {axis: horizontal | vertical, position in world units, colour?: unmultiplied [r, g, b, a], \
             memo?}; a guide without a colour gets the default colour, which colour sets. show draws them; snap makes \
             vertices moved with the Point tool adhere to a guide within 8 points. At most 256 guides and 256 characters of \
             memo. Editor only. Not a document edit.",
            closed_schema(&[
                ("open", json!({"type": "boolean"})),
                ("show", json!({"type": "boolean"})),
                ("snap", json!({"type": "boolean"})),
                ("colour", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                    "minItems": 4, "maxItems": 4})),
                ("lines", json!({"type": "array", "maxItems": EDITOR_MAX_GUIDES, "items": {
                    "type": "object",
                    "properties": {
                        "axis": {"type": "string", "enum": ["horizontal", "vertical"]},
                        "position": {"type": "number", "minimum": -EDITOR_MAX_COORDINATE, "maximum": EDITOR_MAX_COORDINATE},
                        "colour": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
                            "minItems": 4, "maxItems": 4},
                        "memo": {"type": "string", "maxLength": EDITOR_MAX_MEMO},
                    },
                    "required": ["axis", "position"],
                    "additionalProperties": false,
                }})),
            ], &[])),
    ]);
    let aliases = crate::mcp_aliases::tools(&tools);
    tools.extend(aliases);
    tools
}

/// Options shared by `export_frames` and `export_video_start`.
fn export_video_properties() -> Vec<(&'static str, Value)> {
    vec![
        ("path", string_schema()),
        ("animation", string_schema()),
        ("camera", string_schema()),
        ("loops", json!({"type": "integer", "minimum": 1})),
        ("framerate", json!({"type": "integer", "minimum": 1, "maximum": crate::export::MAX_FRAMERATE})),
        ("snap_to_framerate", json!({"type": "boolean"})),
        ("transparency", json!({"type": "boolean"})),
        ("background", json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255},
            "minItems": 3, "maxItems": 3})),
        ("overwrite", json!({"type": "boolean"})),
    ]
}

/// `export_frames` options with the GIF ones in place of the still format.
fn gif_properties() -> Vec<(&'static str, Value)> {
    let mut properties = export_video_properties();
    properties.push(("colors", json!({"type": "integer", "minimum": 2, "maximum": 256})));
    properties.push(("repeat", json!({"type": "integer", "minimum": 0, "maximum": 65535})));
    properties
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn physics_set_schema() -> Value {
    let node_ids = json!({ "type": "array", "items": integer_schema(), "minItems": 1 });
    let param = json!({
        "anyOf": [
            integer_schema(),
            { "type": "null" }
        ]
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "node": integer_schema(),
            "nodes": node_ids,
            "param": param,
            "model_type": { "type": "string", "enum": ["Pendulum", "SpringPendulum"] },
            "map_mode": { "type": "string", "enum": ["AngleLength", "XY", "LengthAngle", "YX"] },
            "gravity": number_schema(),
            "length": number_schema(),
            "frequency": number_schema(),
            "angle_damping": number_schema(),
            "length_damping": number_schema(),
            "output_scale": { "type": "array", "items": number_schema(), "minItems": 2, "maxItems": 2 },
            "output_scale_x": number_schema(),
            "output_scale_y": number_schema(),
            "local_only": { "type": "boolean" },
            "preset": string_schema()
        },
        "oneOf": [
            { "required": ["node"], "not": { "required": ["nodes"] } },
            { "required": ["nodes"], "not": { "required": ["node"] } }
        ],
        "allOf": [
            { "not": { "required": ["output_scale", "output_scale_x"] } },
            { "not": { "required": ["output_scale", "output_scale_y"] } }
        ],
        "anyOf": [
            { "required": ["param"] },
            { "required": ["model_type"] },
            { "required": ["map_mode"] },
            { "required": ["gravity"] },
            { "required": ["length"] },
            { "required": ["frequency"] },
            { "required": ["angle_damping"] },
            { "required": ["length_damping"] },
            { "required": ["output_scale"] },
            { "required": ["output_scale_x"] },
            { "required": ["output_scale_y"] },
            { "required": ["local_only"] },
            { "required": ["preset"] }
        ]
    })
}

fn motionsync_shapes_schema() -> Value {
    let mut map = Map::new();
    let mut required = Vec::new();
    for shape in crate::motion_sync::SHAPES {
        map.insert(shape.to_owned(), number_schema());
        required.push(json!(shape));
    }
    json!({"type":"object","properties":map,"required":required,"additionalProperties":false})
}

fn schema(properties: &[(&str, Value)], required: &[&str]) -> Value {
    let mut map = Map::new();
    for (name, value) in properties {
        map.insert((*name).to_owned(), value.clone());
    }
    json!({ "type": "object", "properties": map, "required": required, "additionalProperties": true })
}

fn psd_depth_properties() -> Vec<(&'static str, Value)> {
    let layer = json!({
        "type": "object",
        "properties": {
            "layerPath": string_schema(),
            "target": {"description": "\"auto\", \"ignore\" or a GridDeformer node id"},
            "enabled": {"type": "boolean"},
            "visible": {"type": "boolean"},
            "depthEnabled": {"type": "boolean"},
            "invert": {"type": "boolean"},
            "depthScale": number_schema(),
            "depthOffset": number_schema(),
        },
        "required": ["layerPath"],
        "additionalProperties": false,
    });
    vec![
        ("path", string_schema()),
        ("invert", json!({ "type": "boolean" })),
        ("backDepth", number_schema()),
        ("frontDepth", number_schema()),
        ("depthScale", number_schema()),
        ("convolution", string_schema()),
        ("channel", string_schema()),
        ("customRadius", json!({ "type": "integer" })),
        ("alphaThreshold", number_schema()),
        ("missingPolicy", string_schema()),
        ("matchDirectGridName", json!({ "type": "boolean" })),
        ("layers", json!({ "type": "array", "items": layer })),
        ("disabledGrids", json!({ "type": "array", "items": integer_schema() })),
    ]
}

fn string_schema() -> Value {
    json!({ "type": "string" })
}
fn integer_schema() -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": 4294967294u64 })
}
fn frame_schema() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 2147483647i64 })
}
fn job_schema() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}
fn number_schema() -> Value {
    json!({ "type": "number" })
}
fn pair_schema() -> Value {
    json!({ "type": "array", "items": number_schema(), "minItems": 2, "maxItems": 2 })
}
fn pair_integer_schema() -> Value {
    json!({ "type": "array", "items": { "type": "integer", "minimum": 0 }, "minItems": 2, "maxItems": 2 })
}
fn quad_schema() -> Value {
    json!({ "type": "array", "items": number_schema(), "minItems": 4, "maxItems": 4 })
}
/// `boneTable` of the MMD tools: MMD bone name to a rig boneId, or null to ignore the name.
fn mmd_bone_table_schema() -> Value {
    json!({ "type": "object", "maxProperties": 64, "additionalProperties": { "type": ["string", "null"] } })
}
/// A non-empty list of node UUIDs, for a list that no alias stands in for.
fn node_list_schema() -> Value {
    json!({ "type": "array", "items": integer_schema(), "minItems": 1 })
}
/// `nodes` of a tool whose operation reads its targets the way the core's `node_ids` does: `id`, then `ids`, is
/// read before `nodes` when given.
fn node_ids_schema() -> Value {
    let mut nodes = node_list_schema();
    nodes["description"] =
        json!("id (one integer UUID), then ids (an array of them), is read in place of nodes when given");
    nodes
}
/// `parent` of `node_paste`.
fn paste_parent_schema() -> Value {
    let mut parent = integer_schema();
    parent["description"] =
        json!("newParent (an integer UUID), then the first of id, ids or nodes, is read when parent is absent");
    parent
}
/// `target` of a tool whose operation takes one depth target.
fn depth_target_schema() -> Value {
    let mut target = integer_schema();
    target["description"] =
        json!("id (an integer UUID) is read when target is absent, then a nodes or ids of one UUID");
    target
}
/// `context` of a D alias that reads `context.nodes`.
pub(crate) fn context_nodes_schema() -> Value {
    json!({ "type": "object", "properties": { "nodes": node_list_schema() }, "required": ["nodes"] })
}
/// `context` of the two D deform aliases: where they take the parameter from when `parameter` is absent.
fn armed_context_schema() -> Value {
    let ids = json!({ "type": "array", "items": integer_schema() });
    json!({ "type": "object", "properties": { "armedParameters": ids, "parameters": ids },
        "description": "The first of armedParameters, else of parameters, is read when parameter is absent" })
}
/// `schema` for a D alias that takes its target nodes from `context.nodes` and from nowhere else.
fn context_schema(properties: &[(&str, Value)], required: &[&str]) -> Value {
    schema(&[properties, &[("context", context_nodes_schema())]].concat(), &[required, &["context"]].concat())
}

/// Like `schema`, but undeclared arguments are refused (`check_declared_arguments`).
fn closed_schema(properties: &[(&str, Value)], required: &[&str]) -> Value {
    let mut closed = schema(properties, required);
    closed["additionalProperties"] = json!(false);
    closed
}

fn values_schema() -> Value {
    json!({ "type": "object", "additionalProperties": pair_schema() })
}

fn resources(session: &Session) -> Vec<Value> {
    let snapshot = session.snapshot();
    let model = snapshot.get("model").unwrap_or(&Value::Null);
    let mut resources = vec![
        resource("resource://vsoul/snapshot", "Current model snapshot"),
        resource("resource://vsoul/parameters", "Parameter records"),
        resource("resource://vsoul/nodes", "Node tree"),
    ];
    resources.extend(soul2d_resources(&snapshot));
    if snapshot["documentFormat"] == "inx" {
        for slot in 0..session.document.textures().len() {
            resources.push(resource(
                &format!("resource://soul2d/textures/{slot}"),
                &format!("Texture slot {slot}"),
            ));
        }
        resources.push(json!({"uri": PART_SPLIT_SOURCE_URI, "name": "Split source image", "mimeType": "image/png"}));
        resources.extend(session.split_mask_names().map(|name| json!({
            "uri": format!("{PART_SPLIT_MASK_PREFIX}{}", percent_encode(name)),
            "name": format!("Split mask: {name}"), "mimeType": "image/png",
        })));
    }
    fn add_params(list: &[Value], resources: &mut Vec<Value>) {
        for item in list {
            if let Some(id) = item
                .get("uuid")
                .or_else(|| item.get("groupUUID"))
                .and_then(Value::as_u64)
            {
                resources.push(resource(
                    &format!("resource://vsoul/parameters/{id}"),
                    "Parameter record",
                ));
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                add_params(children, resources);
            }
        }
    }
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        add_params(list, &mut resources);
    }
    fn add_nodes(node: &Value, resources: &mut Vec<Value>) {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64) {
            resources.push(resource(
                &format!("resource://vsoul/nodes/{id}"),
                "Node record",
            ));
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                add_nodes(child, resources);
            }
        }
    }
    if let Some(nodes) = model.get("nodes") {
        add_nodes(nodes, &mut resources);
    }
    resources
}

fn resource(uri: &str, name: &str) -> Value {
    json!({ "uri": uri, "name": name, "mimeType": "application/json" })
}

fn read_resource(session: &Session, params: &Value) -> Result<Value, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "resources/read requires a URI".into()))?;
    let mask = uri.strip_prefix(PART_SPLIT_MASK_PREFIX).map(|name| {
        if name.is_empty() || name.contains(['/', '?', '#']) {
            return Err((-32602, "split mask URI requires one percent-encoded part name".to_owned()));
        }
        percent_decode(name).map_err(|error| (-32602, error))
    }).transpose()?;
    let image = if uri == PART_SPLIT_SOURCE_URI {
        Some(crate::part_split::SplitImage::Source)
    } else {
        mask.as_deref().map(crate::part_split::SplitImage::Mask)
    };
    if let Some(image) = image {
        let png = session.split_image_png(image).map_err(|error| (-32002, error))?;
        return Ok(json!({"contents": [{"uri": uri, "mimeType": "image/png",
            "blob": base64::engine::general_purpose::STANDARD.encode(png)}]}));
    }
    let snapshot = session.snapshot();
    let model = snapshot.get("model").cloned().unwrap_or(Value::Null);
    if let Some(guide) = crate::mcp_guides::guide(uri).or_else(|| crate::mcp_guides::skill(uri)) {
        return Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": guide }] }));
    }
    let content = if uri == "resource://soul2d/document/state" {
        let keys =
            ["documentFormat", "revision", "modified", "canUndo", "canRedo", "values", "motion3", "defaultFormLocked",
                "views"];
        Value::Object(keys.iter().map(|key| ((*key).to_owned(), snapshot[*key].clone())).collect())
    } else if uri == RIG_IMPORT_REPORT_URI {
        session.rig_import_report().clone()
    } else if uri == GLTF_EXPORT_REPORT_URI {
        session.gltf_export_report().clone()
    } else if uri == VIDEO_COMMAND_URI {
        session.video_command_preview().clone()
    } else if uri == MESH_IMPORT_REPORT_URI {
        session.mesh_import_report().clone()
    } else if uri == MMD_REPORT_URI {
        session.mmd_report().clone()
    } else if uri == DEFORMER_VALIDATION_URI {
        session.op_deformer_validate(&json!({})).map_err(|error| (-32603, error))?
    } else if uri == AUTOMESH_PRESETS_URI {
        session.automesh_presets().map_err(|error| (-32603, error))?
    } else if uri == PHYSICS_PRESETS_URI {
        session.physics_presets().map_err(|error| (-32603, error))?
    } else if uri == PHYSICS3_PRESETS_URI {
        session.physics3_presets().map_err(|error| (-32603, error))?
    } else if uri == ANIMATION_TEMPLATES_URI {
        session.animation_templates().map_err(|error| (-32603, error))?
    } else if uri == OBJECT_NAMES_CSV_URI {
        let (csv, rows) = crate::object_names::export_csv(&model);
        json!({"csv": csv, "rows": rows})
    } else if uri == "resource://soul2d/document/animation-source" {
        // A CAN3 document's model is its stored scene, track and parameter-key source.
        if snapshot["documentFormat"] != "live2d.can3" {
            return Err((-32002, "no active CAN3 document".to_owned()));
        }
        model.clone()
    } else if uri == PHYSICS_SOURCE_URI {
        if snapshot["documentFormat"] != "live2d.cmo3" {
            return Err((-32002, "no active CMO3 document".to_owned()));
        }
        session.physics_source_view().map_err(|error| (-32002, error))?
    } else if uri == PART_SPLIT_PLAN_URI {
        if snapshot["documentFormat"] != "inx" {
            return Err((-32002, "split plans require an INP/INX document".to_owned()));
        }
        let mut body = session
            .op_split_plan(&Value::Null)
            .map_err(|error| (-32002, error))?;
        body["documentRevision"] = snapshot["revision"].clone();
        body["draftRevision"] = snapshot["partSplit"]["revision"].clone();
        body
    } else if let Some(query) = uri.strip_prefix("resource://soul2d/bindings/get?") {
        binding_resource(&model, query)
    } else if uri == "resource://vsoul/snapshot" {
        snapshot
    } else if uri == "resource://vsoul/parameters" {
        model.get("param").cloned().unwrap_or_else(|| json!([]))
    } else if uri == "resource://vsoul/nodes" {
        model.get("nodes").cloned().unwrap_or(Value::Null)
    } else if let Some(id) = uri.strip_prefix("resource://vsoul/parameters/") {
        let id = id
            .parse::<u32>()
            .map_err(|_| (-32602, "invalid parameter resource URI".into()))?;
        find_resource(model.get("param").unwrap_or(&Value::Null), id)
            .cloned()
            .ok_or_else(|| (-32002, format!("parameter {id} was not found")))?
    } else if let Some(id) = uri.strip_prefix("resource://vsoul/nodes/") {
        let id = id
            .parse::<u32>()
            .map_err(|_| (-32602, "invalid node resource URI".into()))?;
        find_node(model.get("nodes").unwrap_or(&Value::Null), id)
            .cloned()
            .ok_or_else(|| (-32002, format!("node {id} was not found")))?
    } else if let Some(query) = uri.strip_prefix("resource://soul2d/resources/find?") {
        let selector = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("selector="))
            .map_or_else(|| Ok("*".to_owned()), percent_decode)
            .map_err(|message| (-32602, message))?;
        crate::selector::find_response(&model, &selector).map_err(|message| (-32602, message))?
    } else if let Some(id) = uri.strip_prefix("resource://soul2d/resources/") {
        // The address njc builds for `read <uuid>`: a node or a parameter, whichever owns the UUID.
        let id = id
            .parse::<u32>()
            .map_err(|_| (-32602, "invalid resource URI".into()))?;
        // D: {"item": {typeId, uuid, name, data}}, or {"item": null} for an unknown UUID.
        let node = find_node(model.get("nodes").unwrap_or(&Value::Null), id).map(|node| {
            (node.get("type").and_then(Value::as_str).unwrap_or("Node"), node)
        });
        let found = node.or_else(|| {
            find_resource(model.get("param").unwrap_or(&Value::Null), id)
                .map(|item| (if item.get("groupUUID").is_some() { "Group" } else { "Parameter" }, item))
        });
        let item = found.map(|(type_id, data)| json!({
            "typeId": type_id, "uuid": id, "name": data.get("name").cloned().unwrap_or(Value::Null), "data": data,
        }));
        json!({ "item": item })
    } else if let Some(raw) = uri.strip_prefix("resource://soul2d/textures/") {
        if snapshot["documentFormat"] != "inx" {
            return Err((-32002, "texture slots require an INP/INX document".into()));
        }
        let slot = raw.parse::<usize>().map_err(|_| (-32602, "texture slot must be an integer".into()))?;
        let textures = session.document.textures();
        let blob = textures.get(slot).ok_or_else(|| {
            (-32002, format!("texture slot {slot} is out of range ({} textures)", textures.len()))
        })?;
        let format = match blob.tag {
            0 => "png",
            1 => "tga",
            tag => return Err((-32002, format!("texture slot {slot} has unsupported image tag {tag}"))),
        };
        let [width, height] = crate::render::texture_size(blob, slot).map_err(|error| (-32002, error))?;
        json!({
            "slot": slot,
            "width": width,
            "height": height,
            "format": format,
            "base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &blob.data),
        })
    } else {
        return Err((-32002, "resource was not found".into()));
    };
    Ok(
        json!({ "contents": [{ "uri": uri, "mimeType": "application/json", "text": content.to_string() }] }),
    )
}

fn resource_templates() -> Value {
    json!([
        { "uriTemplate": "resource://soul2d/resources/{uuid}", "name": "Read Resource Instance By UUID",
          "description": "Read one resource instance by UUID.", "mimeType": "application/json" },
        { "uriTemplate": "resource://soul2d/resources/find?selector={selector}",
          "name": "Start Here: Explore Entire Model Tree",
          "description": "Hierarchical resource discovery by selector.", "mimeType": "application/json" },
        { "uriTemplate": "resource://soul2d/bindings/get?parameter={parameter}&target={target}&name={name}",
          "name": "Read Binding By Descriptor",
          "description": "Read binding values by parameter UUID, target UUID, and binding name.",
          "mimeType": "application/json" },
        { "uriTemplate": "resource://soul2d/part-split/masks/{name}", "name": "Split part mask",
          "description": "Canonical 8-bit coverage at source dimensions; percent-encode the part name.",
          "mimeType": "image/png" },
        { "uriTemplate": "resource://soul2d/textures/{slot}", "name": "Texture slot",
          "description": "Stored bytes of one INX texture slot, the same JSON as texture_get.",
          "mimeType": "application/json" },
    ])
}

/// The `resource://soul2d/...` entries D lists: document state, guides, discovery, and one entry per node,
/// parameter and binding. A CAN3 document lists its animation source instead of the model tree.
fn soul2d_resources(snapshot: &Value) -> Vec<Value> {
    let mut out = vec![
        resource("resource://soul2d/document/state", "Rust document state"),
        resource(STATISTICS_URI, "Read: rendered node count, mask references and maximum offscreen depth"),
        resource(RIG_IMPORT_REPORT_URI, "Read: loss report of the last applied rig_import_gltf, or null"),
        resource(GLTF_EXPORT_REPORT_URI, "Read: loss report of the last applied export_gltf, or null"),
        resource(VIDEO_CODECS_URI, "Read: latest codec probe status, captured program and result"),
        resource(VIDEO_COMMAND_URI, "Read: last successful video command preview, or null"),
        resource(MESH_IMPORT_REPORT_URI, "Read: loss report of the last applied mesh_import_gltf, or null"),
        resource(MMD_REPORT_URI, "Read: report of the last rig_apply_mmd_pose or rig_bake_mmd_motion, or null"),
        resource(DEFORMER_VALIDATION_URI, "Read: deformer_validate at the current parameter values"),
        resource(AUTOMESH_PRESETS_URI, "Read: the automesh presets the user saved, by name"),
        resource(PHYSICS_PRESETS_URI, "Read: the SimplePhysics presets the user saved, by name"),
        resource(PHYSICS3_PRESETS_URI, "Read: the CMO3 physics input and model presets the user saved, by name"),
        resource(ANIMATION_TEMPLATES_URI, "Read: the animation templates the user saved, by name"),
        resource(OBJECT_NAMES_CSV_URI, "Read: the CSV text object_export_csv writes, and its row count"),
        resource(EDITOR_STATE_URI, "Read: editor selection, armed parameter, tool, camera, panels and nonsecret coding-agent lifecycle state"),
    ];
    if snapshot["documentFormat"] == "live2d.can3" {
        out.push(resource("resource://soul2d/document/animation-source", "Active CAN3 animation source"));
        out.push(resource("resource://soul2d/guides/resources", "Resources Guide"));
        return out;
    }
    if snapshot["documentFormat"] == "inx" {
        out.push(resource(PART_SPLIT_PLAN_URI, "Read: split draft plan, per-part statistics and revisions"));
    }
    if snapshot["documentFormat"] == "live2d.cmo3" {
        out.push(resource(PHYSICS_SOURCE_URI, "Read: full-precision CMO3 physics settings"));
    }
    out.push(resource("resource://soul2d/resources/find?selector=*", "Start Here: Explore Entire Model Tree"));
    for (uri, name) in crate::mcp_guides::GUIDE_URIS {
        out.push(resource(uri, name));
    }
    out.push(resource(crate::mcp_guides::SKILL_URI, "Operating skill: operate-soul2d SKILL.md"));
    for (family, _) in crate::mcp_guides::SKILL_FAMILIES {
        out.push(resource(&format!("{}{family}", crate::mcp_guides::SKILL_FAMILY_PREFIX), &format!("Operating skill map: {family}")));
    }
    let model = snapshot.get("model").unwrap_or(&Value::Null);
    fn nodes(node: &Value, out: &mut Vec<Value>) {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64) {
            let name = node.get("name").and_then(Value::as_str).unwrap_or("Node");
            out.push(resource(&format!("resource://soul2d/resources/{id}"), name));
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            nodes(child, out);
        }
    }
    fn parameters(list: &[Value], out: &mut Vec<Value>) {
        for item in list {
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                parameters(children, out);
                continue;
            }
            let Some(id) = item.get("uuid").and_then(Value::as_u64) else { continue };
            let name = item.get("name").and_then(Value::as_str).unwrap_or("Parameter");
            out.push(resource(&format!("resource://soul2d/resources/{id}"), name));
            for binding in item.get("bindings").and_then(Value::as_array).into_iter().flatten() {
                let (Some(target), Some(channel)) = (
                    binding.get("node").and_then(Value::as_u64),
                    binding.get("param_name").and_then(Value::as_str).filter(|name| !name.is_empty()),
                ) else {
                    continue;
                };
                out.push(resource(
                    &format!("resource://soul2d/bindings/get?parameter={id}&target={target}&name={}", percent_encode(channel)),
                    &format!("{name} -> {channel}"),
                ));
            }
        }
    }
    if let Some(root) = model.get("nodes") {
        nodes(root, &mut out);
    }
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        parameters(list, &mut out);
    }
    out
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// D `readBindingByDescriptor`: `{"item": {...}}`, or `{"item": null, "error": ...}`.
fn binding_resource(model: &Value, query: &str) -> Value {
    let fail = |message: String| json!({ "item": null, "error": message });
    let mut fields = HashMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match (percent_decode(key), percent_decode(value)) {
            (Ok(key), Ok(value)) => {
                fields.insert(key, value);
            }
            _ => return fail("Binding read parameter is not valid UTF-8".into()),
        }
    }
    let (Some(parameter), Some(target), Some(name)) = (fields.get("parameter"), fields.get("target"), fields.get("name")) else {
        return fail("Binding read requires parameter, target, and name".into());
    };
    let (Ok(parameter), Ok(target)) = (parameter.parse::<u32>(), target.parse::<u32>()) else {
        return fail("Binding read parameter and target must be integer UUIDs".into());
    };
    if name.is_empty() {
        return fail("Binding read name must not be empty".into());
    }
    let Some(param) = find_resource(model.get("param").unwrap_or(&Value::Null), parameter).filter(|p| p.get("groupUUID").is_none()) else {
        return fail(format!("Parameter not found: {parameter}"));
    };
    let Some(binding) = param.get("bindings").and_then(Value::as_array).into_iter().flatten().find(|binding| {
        binding.get("node").and_then(Value::as_u64) == Some(u64::from(target))
            && binding.get("param_name").and_then(Value::as_str) == Some(name.as_str())
    }) else {
        return fail("Binding not found".into());
    };
    let Some(node) = find_node(model.get("nodes").unwrap_or(&Value::Null), target) else {
        return fail(format!("Target not found: {target}"));
    };
    json!({ "item": {
        "typeId": "Binding",
        "parameter": { "uuid": parameter, "name": param.get("name").cloned().unwrap_or(Value::Null) },
        "target": { "uuid": target, "name": node.get("name").cloned().unwrap_or(Value::Null),
                    "typeId": node.get("type").and_then(Value::as_str).unwrap_or("Node") },
        "name": name,
        "interpolateMode": binding.get("interpolate_mode").and_then(Value::as_str).unwrap_or("Linear"),
        "data": binding,
    } })
}

fn prompts_list() -> Vec<Value> {
    crate::mcp_guides::PROMPTS
        .iter()
        .map(|(name, description, _, _)| json!({ "name": name, "description": description, "arguments": [] }))
        .collect()
}

fn prompt_get(params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).ok_or((-32602, "prompts/get requires a prompt name".to_owned()))?;
    let (_, _, description, text) = crate::mcp_guides::PROMPTS
        .iter()
        .find(|(candidate, ..)| *candidate == name)
        .ok_or_else(|| (-32602, format!("prompt {name:?} was not found")))?;
    Ok(json!({ "description": description,
               "messages": [{ "role": "assistant", "content": { "type": "text", "text": text } }] }))
}

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = text
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or("invalid percent escape in resource URI")?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "resource URI is not valid UTF-8".to_owned())
}

fn find_resource(value: &Value, id: u32) -> Option<&Value> {
    if let Some(items) = value.as_array() {
        return items.iter().find_map(|item| find_resource(item, id));
    }
    if value.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
        || value.get("groupUUID").and_then(Value::as_u64) == Some(u64::from(id))
    {
        return Some(value);
    }
    value
        .get("children")?
        .as_array()?
        .iter()
        .find_map(|child| find_resource(child, id))
}

fn tool_error(message: &str) -> Value {
    json!({ "isError": true, "content": [{ "type": "text", "text": message }] })
}

fn find_node(value: &Value, id: u32) -> Option<&Value> {
    if value.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
        return Some(value);
    }
    value
        .get("children")?
        .as_array()?
        .iter()
        .find_map(|child| find_node(child, id))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_animation_scene_delete_tool_routes_to_session() {
        assert_eq!(
            direct_operation("live2d_animation_scene_delete"),
            Some("live2d.animation.scene.delete")
        );
    }

    #[test]
    fn native_animation_resource_add_tool_routes_to_session() {
        assert_eq!(
            direct_operation("live2d_animation_image_resource_relink"),
            Some("live2d.animation.image_resource.relink")
        );
        assert_eq!(
            direct_operation("live2d_animation_image_resource_add"),
            Some("live2d.animation.image_resource.add")
        );
        assert_eq!(
            direct_operation("live2d_animation_resource_add"),
            Some("live2d.animation.resource.add")
        );
    }

    #[test]
    fn native_animation_track_tools_route_to_session() {
        assert_eq!(
            direct_operation("live2d_animation_track_add"),
            Some("live2d.animation.track.add")
        );
        assert_eq!(
            direct_operation("live2d_animation_track_delete"),
            Some("live2d.animation.track.delete")
        );
    }

    /// Reads this file's own text: the names are the string literals of the top-level match arms of
    /// `tool_request` and `direct_operation` (lines indented eight spaces that start with `"` or `| "`).
    /// The `Inspector_Apply_*` tables need no scan, since one table feeds both the arm and the declaration.
    /// `check_declared_arguments` already keeps an undeclared name from reaching either function; this names
    /// the arm that would be dead, and it cannot see what a prefix arm (`Node_Add_`, ...) would accept.
    #[test]
    fn every_name_the_dispatcher_matches_is_a_declared_tool() {
        let source = include_str!("mcp.rs");
        let declared = tools();
        let declared: Vec<&str> = declared.iter().filter_map(|tool| tool["name"].as_str()).collect();
        let mut matched = Vec::new();
        for function in ["\nfn tool_request(", "\nfn direct_operation("] {
            let body = source.split(function).nth(1).and_then(|rest| rest.split("\n}\n").next()).unwrap();
            for line in body.lines() {
                let Some(arm) = line.strip_prefix("        ").filter(|arm| arm.starts_with('"') || arm.starts_with("| \""))
                else {
                    continue;
                };
                let pattern = arm.split("=>").next().unwrap_or(arm);
                matched.extend(pattern.split('"').skip(1).step_by(2));
            }
        }
        assert!(matched.len() > 250, "the scan found only {} arms: has the layout of the dispatcher changed?", matched.len());
        for name in &matched {
            assert!(declared.contains(name), "{name} has a dispatcher arm, so it needs a tool declaration");
        }
    }

    #[test]
    fn a_name_the_tool_list_does_not_declare_is_refused_before_dispatch() {
        let mut session = Session::new();
        let created = session.handle_request(&json!({"op": "node.create", "className": "Node", "name": "A"}));
        let node = created["result"]["created"][0]["uuid"].as_u64().unwrap();
        let before = session.snapshot();
        // Each of these reaches a prefix arm of `tool_request`, which would build a valid core request.
        for name in ["Node_Insert_Mask", "Node_Add_Part", "Node_ConvertTo_SimplePhysics"] {
            let arguments = json!({"_suffix": "x", "context": {"nodes": [node]}});
            assert!(tool_request(name, &arguments).is_ok(), "{name} should still be a name the dispatcher maps");
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            assert_eq!(response["result"]["isError"], true, "{name}: {response}");
            assert_eq!(response["result"]["content"][0]["text"], format!("unknown tool: {name}"));
            assert_eq!(session.snapshot(), before);
        }
    }

    /// A value of the type a property schema names, good enough to get past the shape checks of most arms.
    fn sample(schema: &Value) -> Value {
        match schema["type"].as_str() {
            Some("integer") => json!(1),
            Some("number") => json!(1.0),
            Some("boolean") => json!(true),
            Some("string") => json!("x"),
            Some("array") => json!([sample(&schema["items"])]),
            Some("object") => {
                let required = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str);
                Value::Object(required.map(|key| (key.to_owned(), sample(&schema["properties"][key]))).collect())
            }
            _ => json!(1),
        }
    }

    #[test]
    fn no_tool_refuses_an_argument_its_own_schema_declares() {
        let mut probed = 0;
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            let properties = schema["properties"].as_object().cloned().unwrap_or_default();
            let required: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            // The required arguments plus one declared argument at a time (two alternates together may well collide).
            for extra in properties.keys() {
                let arguments: Map<String, Value> = properties
                    .iter()
                    .filter(|(key, _)| *key == extra || required.contains(&key.as_str()))
                    .map(|(key, property)| (key.clone(), sample(property)))
                    .collect();
                if let Err(message) = tool_request(name, &Value::Object(arguments.clone())) {
                    assert!(!message.contains("itself: do not pass"), "{name} {}: {message}", Value::Object(arguments));
                }
                probed += 1;
            }
        }
        assert!(probed > 800, "only {probed} arguments were probed");
    }

    #[test]
    fn an_argument_that_would_replace_what_the_tool_sets_is_refused() {
        let mut session = Session::new();
        let mut create = |name: &str| {
            let created = session.handle_request(&json!({"op": "node.create", "className": "Node", "name": name}));
            created["result"]["created"][0]["uuid"].as_u64().unwrap()
        };
        let (a, b) = (create("A"), create("B"));
        let before = session.snapshot();
        // Each call ran before, with the extra argument in place of the tool's own: another class, another
        // node, another property, another component. `id` is read by the core before the `nodes` of `context`.
        for (name, arguments, set, key) in [
            ("Node_Insert_GridDeformer", json!({"_suffix": "g", "className": "MeshGroup", "context": {"nodes": [a]}}),
                "className", "className"),
            ("Node_Insert_PathDeformer", json!({"_suffix": "p", "context": {"nodes": [a]}, "nodes": [b]}), "nodes", "nodes"),
            ("Node_ConvertTo_MeshGroup", json!({"className": "Camera", "context": {"nodes": [a]}}), "className", "className"),
            ("Inspector_Apply_LockToRoot", json!({"value": true, "property": "pinToMesh", "context": {"nodes": [a]}}),
                "property", "property"),
            ("Inspector_Apply_TranslationX", json!({"value": 5, "component": "translationY", "context": {"nodes": [a]}}),
                "component", "component"),
            ("Node_Insert_PathDeformer", json!({"_suffix": "p", "context": {"nodes": [a]}, "id": b}), "nodes", "id"),
        ] {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            assert_eq!(response["result"]["isError"], true, "{name}: {response}");
            assert_eq!(response["result"]["content"][0]["text"],
                format!("{name} sets {set} itself: do not pass the argument {key}"));
            assert_eq!(session.snapshot(), before, "{name} changed the document");
        }
        // `Node_Add_` leaves `parents` to the caller: given beside `context`, it is still where the node goes.
        let response = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "Node_Add_GridDeformer", "arguments": {"_suffix": "g", "parents": [a], "context": {"nodes": [b]}}}}))
            .unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        let snapshot = session.snapshot();
        assert_eq!(find_node(&snapshot["model"]["nodes"], a as u32).unwrap()["children"][0]["type"], "GridDeformer");
        assert_eq!(find_node(&snapshot["model"]["nodes"], b as u32).unwrap()["children"], json!([]));
    }

    #[test]
    fn a_call_that_omits_a_required_argument_is_refused_and_told_which() {
        let mut session = Session::new();
        let before = session.snapshot();
        let mut checked = 0;
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            let required: Vec<&str> =
                tool["inputSchema"]["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            for omitted in &required {
                // Every other required argument is present. Its value does not matter: the refusal comes first.
                let arguments: Map<String, Value> =
                    required.iter().filter(|key| *key != omitted).map(|key| ((*key).to_owned(), Value::Null)).collect();
                let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}});
                // A host without a window answers an editor tool with "needs the editor" whatever the arguments
                // are, so those are asked the way the editor asks them.
                let message = if EDITOR_TOOLS.contains(&name) {
                    editor_request(&request).unwrap().unwrap_err()
                } else {
                    let response = session.handle_mcp(&request).unwrap();
                    assert_eq!(response["result"]["isError"], true, "{name} without {omitted}: {response}");
                    response["result"]["content"][0]["text"].as_str().unwrap().to_owned()
                };
                assert_eq!(message, format!("missing required argument(s) for {name}: {omitted}"));
                checked += 1;
            }
        }
        assert!(checked > 400, "only {checked} required arguments were checked");
        assert_eq!(session.snapshot(), before);
    }

    /// Only the dispatcher reads `context`, so `tool_request` shows which tools do: one whose answer changes
    /// with a member of `context`, or that cannot be asked without `context`, has to declare what it reads.
    /// No tool is exempt. `ViewCommand_CaptureLiveScreenshot` reads `context` outside `tool_request`, which
    /// this cannot see; its declaration is kept by hand.
    #[test]
    fn a_tool_that_reads_context_declares_the_members_it_reads() {
        // A leftover `context` is forwarded as it is and no session operation reads it, so it is not a change.
        let request = |name: &str, arguments: &Map<String, Value>| {
            tool_request(name, &Value::Object(arguments.clone())).map(|mut request| {
                request.as_object_mut().unwrap().remove("context");
                request
            })
        };
        let names_context = |message: &String| message.contains("context") || message.contains("armed parameter");
        let mut readers = Vec::new();
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            let properties = schema["properties"].as_object().cloned().unwrap_or_default();
            let context = &schema["properties"]["context"];
            let requires_context = schema["required"].as_array().is_some_and(|required| required.contains(&json!("context")));
            let base: Map<String, Value> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|key| *key != "context")
                .map(|key| (key.to_owned(), sample(&properties[key])))
                .collect();
            let plain = request(name, &base);
            if plain.as_ref().is_err_and(names_context) {
                assert!(context.is_object(), "{name} asks for context, so its schema must declare context");
            }
            // An empty `context` is the baseline, so a tool that only asks for the object is not taken to read
            // every member. Refusals count too: `NodeCommand_SetNodeName` answers a different one with a node.
            let with = |context: Value| {
                let mut arguments = base.clone();
                arguments.insert("context".into(), context);
                request(name, &arguments)
            };
            let empty = with(json!({}));
            for member in ["nodes", "armedParameters", "parameters"] {
                if with(json!({ member: [1] })) != empty {
                    assert_eq!(context["properties"][member]["type"], "array",
                        "{name} reads context.{member}, so its schema must declare it");
                    readers.push(name.to_owned());
                }
            }
            // `context` is required exactly when no other declared argument lets the call do without it.
            let alternatives = properties.iter().filter(|(key, _)| *key != "context").map(|(key, property)| {
                let mut arguments = base.clone();
                arguments.insert(key.clone(), sample(property));
                request(name, &arguments)
            });
            let needs_context = std::iter::once(plain.clone())
                .chain(alternatives)
                .all(|answer| answer.is_err_and(|message| names_context(&message)));
            assert_eq!(needs_context, requires_context, "{name}");
            if requires_context {
                assert_eq!(context["required"], json!(["nodes"]), "{name}");
            }
        }
        readers.dedup();
        assert!(readers.len() > 100, "only {} tools were seen to read context", readers.len());
        let expected =
            ["NodeCommand_PasteNode", "Node_Add_Mask", "DepthBoneCommand_ApplyDepthBoneDeform", "DepthMapCommand_ListDepths"];
        for name in expected {
            assert!(readers.iter().any(|reader| reader == name), "{name} was not seen to read context");
        }
    }

    #[test]
    fn a_key_the_core_reads_under_another_name_is_not_required() {
        let mut session = Session::new();
        let mut create = |class: &str, name: &str| {
            let created = session.handle_request(&json!({"op": "node.create", "className": class, "name": name}));
            created["result"]["created"][0]["uuid"].as_u64().unwrap()
        };
        let (node, parent, grid) = (create("Node", "A"), create("Node", "P"), create("GridDeformer", "G"));
        let defined = session.handle_request(
            &json!({"op": "deformer.define_grid", "nodes": [grid], "axisX": [0.0, 1.0], "axisY": [0.0, 1.0]}));
        assert_eq!(defined["succeeded"], true, "{defined}");
        let call = |session: &mut Session, name: &str, arguments: Value| {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            (response["result"]["isError"] == true, response["result"]["content"][0]["text"].as_str().unwrap().to_owned())
        };
        // No target at all: the schema lets the call through and the core refuses it.
        let before = session.snapshot();
        let no_nodes = "node UUIDs are required";
        let no_target = "target must be a node UUID";
        for (name, arguments, refusal) in [
            ("node_copy", json!({}), no_nodes),
            ("node_cut", json!({}), no_nodes),
            ("node_insert", json!({"className": "Node"}), no_nodes),
            ("node_delete", json!({}), no_nodes),
            ("node_convert", json!({}), "className is required"),
            ("node_convert", json!({"className": "Camera"}), no_nodes),
            ("node_move", json!({"parent": parent}), no_nodes),
            ("node_move", json!({"nodes": [node]}), "parent must be a positive resource UUID"),
            ("node_paste", json!({}), "paste parent is required"),
            ("node_visibility_set", json!({}), no_nodes),
            ("node_lock_set", json!({}), no_nodes),
            ("deformer_set", json!({"property": "GridDeformDynamic", "value": true}), no_nodes),
            ("deformer_define_vertices", json!({"vertices": [0.0, 0.0]}), no_nodes),
            ("deformer_define_grid", json!({"axisX": [0.0, 1.0], "axisY": [0.0, 1.0]}), no_nodes),
            ("deformer_define_mesh", json!({"vertices": [0.0, 0.0], "indices": []}), no_nodes),
            ("depth_list", json!({}), no_target),
            ("depth_set", json!({"depths": [0.0, 0.0, 0.0, 0.0]}), no_target),
            ("depth_clear", json!({}), no_target),
            ("depth_edit_apply", json!({"operations": []}), no_target),
            ("depth_ops_list", json!({}), no_target),
            ("depth_ops_set", json!({"operations": []}), no_target),
            ("depth_ops_add", json!({"operation": {}, "index": 0}), no_target),
            ("depth_ops_update", json!({"index": 0, "operation": {}}), no_target),
            ("depth_ops_remove", json!({"index": 0}), no_target),
            ("depth_ops_move", json!({"fromIndex": 0, "toIndex": 0}), no_target),
            ("depth_ops_clear", json!({}), no_target),
            ("depth_ops_apply", json!({}), no_target),
        ] {
            let (refused, message) = call(&mut session, name, arguments.clone());
            assert!(refused, "{name} {arguments}");
            assert_eq!(message, refusal, "{name} {arguments}");
            assert_eq!(session.snapshot()["revision"], before["revision"], "{name} moved the revision");
            assert_eq!(session.snapshot(), before, "{name} changed the document");
        }
        // The other names the core reads reach it.
        let (refused, message) = call(&mut session, "node_copy", json!({"id": node}));
        assert!(!refused, "{message}");
        let (refused, message) = call(&mut session, "node_move", json!({"nodes": [node], "newParent": parent}));
        assert!(!refused, "{message}");
        let (refused, message) = call(&mut session, "node_convert", json!({"toType": "Camera", "ids": [node]}));
        assert!(!refused, "{message}");
        let (refused, message) = call(&mut session, "depth_list", json!({"id": grid}));
        assert!(!refused, "{message}");
        let snapshot = session.snapshot();
        let moved = &find_node(&snapshot["model"]["nodes"], parent as u32).unwrap()["children"][0];
        assert_eq!((moved["uuid"].as_u64(), moved["type"].as_str()), (Some(node), Some("Camera")));
    }

    #[test]
    fn a_tool_that_creates_a_node_says_what_it_creates() {
        let mut session = Session::new();
        let before = session.snapshot();
        let listed = tools();
        let classes: Vec<&str> = listed
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .filter(|name| name.starts_with("Node_Add_") || name.starts_with("Node_Insert_"))
            .collect();
        assert_eq!(classes.len(), 18);
        let named = [
            ("NodeCommand_AddNode", json!({}), "className, _suffix"),
            ("node_create", json!({}), "className"),
            ("node_insert", json!({"nodes": [1]}), "className"),
        ];
        let per_class = classes.into_iter().map(|name| (name, json!({"context": {"nodes": [1]}}), "_suffix"));
        // `null` counts as given for the host, so the operation refuses a class that is not a string.
        for (name, arguments) in [
            ("node_create", json!({"className": null, "name": "Camera"})),
            ("node_create", json!({"className": 5})),
            ("node_insert", json!({"className": null, "nodes": [1]})),
            ("NodeCommand_AddNode", json!({"className": null, "_suffix": "A"})),
        ] {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            assert_eq!(response["result"]["content"][0]["text"], "className must be a string", "{name}");
            assert_eq!(session.snapshot(), before, "{name} changed the document");
        }
        for (name, arguments, missing) in per_class.chain(named) {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            assert_eq!(response["result"]["content"][0]["text"],
                format!("missing required argument(s) for {name}: {missing}"));
            assert_eq!(session.snapshot(), before, "{name} changed the document");
        }
    }

    #[test]
    fn applying_a_bone_deform_requires_the_parameter_and_gets_past_arming_with_it_alone() {
        let mut session = Session::new();
        let rig = session.handle_request(&json!({"op": "node.create", "className": "DepthRigRoot"}));
        let rig = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let parameter = session.handle_request(&json!({"op": "parameter.create", "dimensions": 1}));
        let parameter = parameter["result"]["uuid"].as_u64().unwrap();
        let mut answer = |name: &str, arguments: Value| {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            response["result"]["content"][0]["text"].as_str().unwrap().to_owned()
        };
        assert_eq!(answer("rig_apply_deform", json!({"root": rig})),
            "missing required argument(s) for rig_apply_deform: parameter");
        // With the parameter the call is past "No armed parameter": the next check of the operation answers.
        let past_arming = "Armed parameter does not drive this DepthRigRoot";
        assert_eq!(answer("rig_apply_deform", json!({"root": rig, "parameter": parameter})), past_arming);
        assert_eq!(answer("DepthBoneCommand_ApplyDepthBoneDeform",
            json!({"root": rig, "context": {"armedParameters": [parameter]}})), past_arming);
        assert_eq!(answer("DepthBoneCommand_ApplyDepthBoneDeform", json!({"root": rig})), "No armed parameter");
        // A preview needs no parameter.
        assert_eq!(answer("rig_preview_deform", json!({"root": rig})), "No targets");
    }

    #[test]
    fn a_refusal_repeats_at_most_64_characters_of_a_name_the_caller_wrote() {
        let mut session = Session::new();
        let response = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "x".repeat(5000), "arguments": {}}}))
            .unwrap();
        assert_eq!(response["result"]["content"][0]["text"], format!("unknown tool: {}", "x".repeat(64)));
        assert_eq!(tool_request(&"y".repeat(5000), &json!({})).unwrap_err(), format!("unknown tool: {}", "y".repeat(64)));
        let mut answer = |name: &str, arguments: Value| {
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": name, "arguments": arguments}}))
                .unwrap();
            response["result"]["content"][0]["text"].as_str().unwrap().to_owned()
        };
        assert_eq!(answer("live2d_physics_stabilize", json!({"z".repeat(5000): 1})),
            format!("unknown argument(s) for live2d_physics_stabilize: {}", "z".repeat(64)));
        assert_eq!(
            answer("Motion3Command_SetPoint", json!({"name": "m", "curve": 0, "point": 0, "changes": {"w".repeat(5000): 1}})),
            format!("Unknown Motion3 point change: {}", "w".repeat(64)));
        let keys: Vec<String> = (0..20).map(|index| format!("{index:02}{}", "k".repeat(100))).collect();
        assert_eq!(echoed(keys.iter().map(String::as_str)).len(), 8 * 64 + 7 * 2);
    }

    #[test]
    fn transform_inspector_aliases_are_listed_and_set_one_component() {
        let mut session = Session::new();
        let created = session.handle_request(&json!({"op": "node.create", "className": "Node", "name": "A"}));
        let node = created["result"]["created"][0]["uuid"].as_u64().unwrap();
        let listed = tools();
        for (index, (id, _)) in INSPECTOR_TRANSFORM_COMPONENTS.iter().enumerate() {
            let name = format!("Inspector_Apply_{id}");
            assert!(listed.iter().any(|tool| tool["name"] == name.as_str()), "{name} is not listed");
            let response = session
                .handle_mcp(&json!({"jsonrpc": "2.0", "id": index, "method": "tools/call", "params": {
                    "name": name, "arguments": {"value": 0.25 + index as f64, "context": {"nodes": [node]}}}}))
                .unwrap();
            assert_eq!(response["result"]["isError"], false, "{name}: {response}");
        }
        let snapshot = session.snapshot();
        let node = find_node(&snapshot["model"]["nodes"], node as u32).unwrap();
        assert_eq!(node["transform"]["trans"], json!([0.25, 1.25, 2.25]));
        assert_eq!(node["transform"]["rot"], json!([3.25, 4.25, 5.25]));
        assert_eq!(node["transform"]["scale"], json!([6.25, 7.25]));
        assert_eq!(node["zsort"], 8.25);
    }

    #[test]
    fn invalid_rpc_envelopes_cannot_mutate_the_session() {
        let mut session = Session::new();
        let before = session.snapshot();
        for request in [
            json!([]),
            json!({}),
            json!({"jsonrpc":"1.0","id":1,"method":"tools/call","params":{
                "name":"parameter_create","arguments":{"dimensions":1}
            }}),
            json!({"jsonrpc":"2.0","id":{},"method":"tools/list"}),
        ] {
            let response = session.handle_mcp(&request).unwrap();
            assert_eq!(response["error"]["code"], -32600);
            assert_eq!(response["id"], Value::Null);
            assert_eq!(session.snapshot(), before);
        }
    }

    #[test]
    fn mcp_parameter_resource_read_finds_records_in_the_root_array() {
        let mut session = Session::new();
        let created = session.handle_request(&json!({
            "op": "parameter.create", "dimensions": 2, "name": "Blink",
        }));
        assert_eq!(created["succeeded"], true);
        let id = created["result"]["uuid"].as_u64().unwrap();
        let response = read_resource(
            &session,
            &json!({
                "uri": format!("resource://vsoul/parameters/{id}"),
            }),
        )
        .unwrap();
        let text = response["contents"][0]["text"].as_str().unwrap();
        let parameter: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parameter["uuid"], id);
        assert_eq!(parameter["name"], "Blink");
    }

    #[test]
    fn mcp_resource_templates_list_the_soul2d_reads_and_other_resource_methods_remain() {
        let mut session = Session::new();
        let templates = session
            .handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"resources/templates/list"}))
            .unwrap();
        let uris: Vec<&str> = templates["result"]["resourceTemplates"].as_array().unwrap().iter()
            .map(|template| template["uriTemplate"].as_str().unwrap()).collect();
        assert_eq!(uris, [
            "resource://soul2d/resources/{uuid}",
            "resource://soul2d/resources/find?selector={selector}",
            "resource://soul2d/bindings/get?parameter={parameter}&target={target}&name={name}",
            "resource://soul2d/part-split/masks/{name}",
            "resource://soul2d/textures/{slot}",
        ]);

        let listed = session
            .handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"resources/list"}))
            .unwrap();
        let resources = listed["result"]["resources"].as_array().unwrap();
        for uri in [
            "resource://vsoul/snapshot",
            "resource://vsoul/parameters",
            "resource://vsoul/nodes",
        ] {
            assert!(resources.iter().any(|resource| resource["uri"] == uri));
        }

        let unknown = session
            .handle_mcp(&json!({"jsonrpc":"2.0","id":3,"method":"resourceTemplates/unknown"}))
            .unwrap();
        assert_eq!(unknown["id"], 3);
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(unknown["error"]["message"], "method not found");
    }

    #[test]
    fn clipboard_and_convert_tools_map_to_session_ops() {
        let copy =
            tool_request("NodeCommand_CopyNode", &json!({"context": {"nodes": [4]}})).unwrap();
        assert_eq!(copy["op"], "node.copy");
        assert_eq!(copy["nodes"], json!([4]));
        let paste = tool_request(
            "NodeCommand_PasteNode",
            &json!({"context": {"nodes": [1]}, "index": 0}),
        )
        .unwrap();
        assert_eq!(paste["op"], "node.paste");
        assert_eq!(paste["nodes"], json!([1]));
        let convert = tool_request(
            "Node_ConvertTo_Composite",
            &json!({"context": {"nodes": [4]}}),
        )
        .unwrap();
        assert_eq!(convert["op"], "node.convert");
        assert_eq!(convert["className"], "Composite");
        let generic = tool_request(
            "NodeCommand_ConvertTo",
            &json!({"className": "Camera", "context": {"nodes": [4]}}),
        )
        .unwrap();
        assert_eq!(generic["className"], "Camera");
        let blocked = tool_request(
            "Node_ConvertTo_GridDeformer",
            &json!({"context": {"nodes": [4]}}),
        )
        .unwrap();
        assert_eq!(blocked["className"], "GridDeformer");
        assert_eq!(blocked["op"], "node.convert");
        let part = tool_request(
            "Node_ConvertTo_Part",
            &json!({"context": {"nodes": [4]}}))
        .unwrap();
        assert_eq!(part["className"], "Part");
        assert_eq!(part["op"], "node.convert");
        let set_depths = tool_request(
            "DepthMapCommand_SetDepths",
            &json!({"depths": [0.0, 1.0], "context": {"nodes": [4]}}),
        )
        .unwrap();
        assert_eq!(set_depths["op"], "depth.set");
        assert_eq!(set_depths["nodes"], json!([4]));
        assert_eq!(set_depths["depths"], json!([0.0, 1.0]));
        let clear_depths =
            tool_request("DepthMapCommand_ClearDepths", &json!({"target": "4"})).unwrap();
        assert_eq!(clear_depths["op"], "depth.clear");
        assert_eq!(clear_depths["target"], "4");
    }

    #[test]
    fn legacy_trs_tool_maps_explicit_context_to_axis_indexes() {
        let request = tool_request(
            "ModelCommand_SetTRSBinding",
            &json!({
                "parameter": 7, "nodes": [2], "key": [1, 0],
                "translation": [4, 5], "rotationDegrees": 45, "applyRotation": true,
            }),
        )
        .unwrap();
        assert_eq!(request["op"], "binding.set_trs");
        assert_eq!(request["parameter"], 7);
        assert_eq!(request["key"], json!([1, 0]));
        assert_eq!(request["rotation_degrees"], 45);
    }

    #[test]
    fn rotation_binding_tools_map_radians_including_zero() {
        let pitch = tool_request(
            "ModelCommand_SetRotationXBinding",
            &json!({"parameter": 7, "nodes": [2], "key": [0, 0], "rotationRadians": 0}),
        )
        .unwrap();
        assert_eq!(pitch["op"], "binding.set_trs");
        assert_eq!(pitch["rotation_x"], 0);
        assert!(pitch.get("rotation_y").is_none());
        let yaw = tool_request(
            "ModelCommand_SetRotationYBinding",
            &json!({"parameter": 7, "nodes": [4, 5], "key": [2, 1], "rotationRadians": -0.25}),
        )
        .unwrap();
        assert_eq!(yaw["rotation_y"], -0.25);
        assert_eq!(yaw["nodes"], json!([4, 5]));
    }

    #[test]
    fn rig_create_and_skeleton_tools_map_to_session_ops() {
        let root = tool_request(
            "DepthBoneCommand_CreateDepthRigRoot",
            &json!({"name": "", "context": {"nodes": [1]}}),
        )
        .unwrap();
        assert_eq!(root["op"], "node.create");
        assert_eq!(root["className"], "DepthRigRoot");
        assert_eq!(root["name"], "DepthRig");
        assert_eq!(root["parents"], json!([1]));
        let bone = tool_request(
            "DepthBoneCommand_AddDepthBone",
            &json!({
                "parent": 4, "boneId": "Spine",
                "restHead": [1.0, 2.0, 3.0], "restTail": [1.0, 12.0, 3.0], "restRoll": 0.25
            }),
        )
        .unwrap();
        assert_eq!(bone["className"], "DepthBone");
        assert_eq!(bone["boneId"], "Spine");
        assert_eq!(bone["parents"], json!([4]));
        assert_eq!(bone["boneId"], "Spine");
        let omitted = tool_request(
            "DepthBoneCommand_AddDepthBone",
            &json!({"parent": 4, "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 1.0, 0.0]}),
        )
        .unwrap();
        assert_eq!(omitted["boneId"], "");
        assert_eq!(omitted["name"], "");
        // A name the caller gives is the node's name; `boneId` is only its default.
        let named = tool_request(
            "DepthBoneCommand_AddDepthBone",
            &json!({"parent": 4, "boneId": "Spine", "name": "Back", "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 1.0, 0.0]}),
        )
        .unwrap();
        assert_eq!((named["name"].as_str(), named["boneId"].as_str()), (Some("Back"), Some("Spine")));
        let first_only = tool_request(
            "DepthBoneCommand_CreateDepthRigRoot",
            &json!({"context": {"nodes": [8, 9]}}),
        )
        .unwrap();
        assert_eq!(first_only["parents"], json!([8]));
        let fallback = tool_request(
            "DepthBoneCommand_CreateDepthRigRoot",
            &json!({"context": {"nodes": []}}),
        )
        .unwrap();
        assert!(fallback.get("parents").is_none());
        let skeleton = tool_request(
            "DepthBoneCommand_AddStandardDepthSkeleton",
            &json!({"root": 4, "scale": 0}),
        )
        .unwrap();
        assert_eq!(skeleton["op"], "rig.add_skeleton");
        assert_eq!(skeleton["root"], 4);
        assert_eq!(skeleton["scale"], 0);
        let listed = tool_request(
            "DepthBoneCommand_ListDepthBones",
            &json!({"context": {"nodes": [9, 10]}}),
        )
        .unwrap();
        assert_eq!(listed["op"], "rig.list_bones");
        assert_eq!(listed["root"], 9);
        assert!(listed.get("context").is_none());
        assert!(tool_request("DepthBoneCommand_AddStandardDepthSkeleton", &json!({})).is_err());
    }

    #[test]
    fn weld_tools_are_listed_and_map_to_session_ops() {
        let listed = tools();
        let names: Vec<_> = listed
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"part_set_weld"));
        assert!(names.contains(&"part_clear_weld"));
        let set = tool_request(
            "part_set_weld",
            &json!({"part": 4, "target": 5, "indices": [0, -1, -1], "weight": 0.25}),
        )
        .unwrap();
        assert_eq!(set["op"], "part.set_weld");
        assert_eq!(set["part"], 4);
        assert_eq!(set["target"], 5);
        assert_eq!(set["indices"], json!([0, -1, -1]));
        assert_eq!(set["weight"], 0.25);
        let clear = tool_request("part_clear_weld", &json!({"part": 4, "target": 5})).unwrap();
        assert_eq!(clear["op"], "part.clear_weld");
        assert!(tool_request("part_set_weld", &json!({})).is_ok());
    }

    #[test]
    fn render_packet_tool_maps_runtime_inputs_and_lists_its_schema() {
        let request = tool_request(
            "render_packet",
            &json!({
                "values": {"17": [0.5, 0.0]},
                "deformations": {"9": [[1.0, 2.0]]}
            }),
        )
        .unwrap();
        assert_eq!(request["op"], "render.packet");
        assert_eq!(request["values"]["17"], json!([0.5, 0.0]));
        assert_eq!(request["deformations"]["9"], json!([[1.0, 2.0]]));

        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "render_packet")
            .unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], true);
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        assert!(tool["inputSchema"]["properties"].get("values").is_some());
        assert!(tool["inputSchema"]["properties"]
            .get("deformations")
            .is_some());
    }

    #[test]
    fn native_vertex_tool_maps_exact_identity_and_revision() {
        let arguments = json!({"sourceGuid":"owner", "formGuid":"form", "coordinateType":"Canvas",
            "vertexIndex":0, "position":[1.25,-2.5], "expectedRevision":8});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.form.set_vertex");
        assert_eq!(tool_request("live2d_form_set_vertex", &arguments).unwrap(), expected);
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_form_set_vertex").unwrap();
        assert!(tool["inputSchema"]["required"].as_array().unwrap().iter()
            .any(|field| field == "expectedRevision"));
        assert_eq!(tool["inputSchema"]["properties"]["position"]["maxItems"], 2);
        assert_eq!(tool_request("live2d_authoring_read", &json!({})).unwrap(),
            json!({"op":"live2d.authoring.read"}));
        assert_eq!(tool_request("live2d_cdi3_export", &json!({})).unwrap(), json!({"op":"live2d.cdi3.export"}));
        assert_eq!(tool_request("live2d_physics3_export", &json!({"path":"sway.physics3.json"})).unwrap(),
            json!({"op":"live2d.physics3.export", "path":"sway.physics3.json"}));
        assert_eq!(tool_request("live2d_userdata3_export", &json!({})).unwrap(), json!({"op":"live2d.userdata3.export"}));
        assert_eq!(tool_request("live2d_user_data_set", &json!({"guid":"g", "value":"", "expectedRevision":1})).unwrap(),
            json!({"op":"live2d.user_data.set", "guid":"g", "value":"", "expectedRevision":1}));
        assert_eq!(tool_request("rig_import_gltf", &json!({"root":7, "path":"model.vrm"})).unwrap(),
            json!({"op":"rig.import_gltf", "root":7, "path":"model.vrm"}));
    }

    #[test]
    fn native_opacity_tool_maps_exact_identity_value_and_revision() {
        let arguments = json!({"sourceGuid":"owner", "formGuid":"form", "coordinateType":"Canvas",
            "opacity":1.00005, "expectedRevision":8});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.form.set_opacity");
        assert_eq!(
            tool_request("live2d_form_set_opacity", &arguments).unwrap(),
            expected
        );
        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "live2d_form_set_opacity")
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["required"],
            json!([
                "sourceGuid",
                "formGuid",
                "coordinateType",
                "opacity",
                "expectedRevision"
            ])
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["opacity"]["type"],
            "number"
        );
    }

    #[test]
    fn native_runtime_parameter_tool_preserves_id_and_scalar() {
        let arguments = json!({"parameterId":"ParamAngleX", "value":-12.5});
        assert_eq!(tool_request("live2d_parameter_set", &arguments).unwrap(),
            json!({"op":"live2d.parameter.set", "parameterId":"ParamAngleX", "value":-12.5}));
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_parameter_set").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!(["parameterId", "value"]));
        assert_eq!(tool["inputSchema"]["properties"]["value"]["type"], "number");
    }

    #[test]
    fn animation_bindings_tool_maps_required_identities_and_optional_path() {
        let arguments = json!({"modelPath":"model.cmo3", "resourceGuid":"res", "path":"scene.can3"});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.animation.bindings");
        assert_eq!(tool_request("live2d_animation_bindings", &arguments).unwrap(), expected);
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_animation_bindings").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!(["modelPath", "resourceGuid"]));
        assert_eq!(tool["inputSchema"]["properties"]["path"]["type"], "string");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool_request("live2d_animation_bindings",
            &json!({"modelPath":"model.cmo3", "resourceGuid":"res"})).unwrap(),
            json!({"op":"live2d.animation.bindings", "modelPath":"model.cmo3", "resourceGuid":"res"}));
    }

    #[test]
    fn native_source_tool_maps_path_and_lists_readonly_schema() {
        assert_eq!(tool_request("live2d_authoring_read", &json!({"path":"sample.cmo3"})).unwrap(),
            json!({"op":"live2d.authoring.read", "path":"sample.cmo3"}));
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_authoring_read").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        assert_eq!(tool["inputSchema"]["properties"]["path"]["type"], "string");
        assert!(tool["description"].as_str().unwrap().contains("not an evaluated canvas pose"));
    }

    #[test]
    fn primary_weights_tool_preserves_values_and_lists_schema() {
        let arguments = json!({"path":"sample.cmo3", "sourceGuid":"mesh", "parameterValues":{"ParamAngleX":15}});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.authoring.linear_weights");
        assert_eq!(tool_request("live2d_authoring_linear_weights", &arguments).unwrap(), expected);
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_authoring_linear_weights").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        assert_eq!(tool["inputSchema"]["properties"]["parameterValues"]["additionalProperties"]["type"], "number");
        assert_eq!(tool["inputSchema"]["properties"]["sourceGuid"]["type"], "string");
        assert!(tool["description"].as_str().unwrap().contains("without changing the session"));
    }

    #[test]
    fn primary_forms_tool_maps_request_and_discloses_source_spaces() {
        let arguments = json!({"path":"sample.cmo3", "sourceGuid":"mesh", "parameterValues":{"ParamAngleX":15}});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.authoring.primary_forms");
        assert_eq!(tool_request("live2d_authoring_primary_forms", &arguments).unwrap(), expected);
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_authoring_primary_forms").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        assert_eq!(tool["inputSchema"]["properties"]["parameterValues"]["additionalProperties"]["type"], "number");
        assert!(tool["description"].as_str().unwrap().contains("recorded coordinate spaces"));
    }

    #[test]
    fn morph_forms_tool_maps_request_and_discloses_unsupported_semantics() {
        let arguments = json!({"sourceGuid":"mesh", "parameterValues":{"ParamMorph":0.5}});
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.authoring.morph_forms");
        assert_eq!(tool_request("live2d_authoring_morph_forms", &arguments).unwrap(), expected);
        let tool = tools().into_iter().find(|tool| tool["name"] == "live2d_authoring_morph_forms").unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        assert_eq!(tool["inputSchema"]["properties"]["parameterValues"]["additionalProperties"]["type"], "number");
        assert!(tool["description"].as_str().unwrap().contains("not a Cubism pose"));
        assert!(tool["description"].as_str().unwrap().contains("smallest controller limit"));
        assert!(tool["description"].as_str().unwrap().contains("raw Glue intensity"));
    }

    #[test]
    fn physics_set_schema_matches_the_handler() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "physics_set")
            .unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["model_type"]["enum"],
            json!(["Pendulum", "SpringPendulum"])
        );
        assert_eq!(
            schema["properties"]["map_mode"]["enum"],
            json!(["AngleLength", "XY", "LengthAngle", "YX"])
        );
        assert!(schema["properties"]["param"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| branch["type"] == "null"));
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), 2);
        assert!(schema["anyOf"].as_array().unwrap().len() >= 10);
        // A call that only names a preset is a call the schema accepts.
        assert_eq!(schema["properties"]["preset"]["type"], "string");
        assert!(schema["anyOf"].as_array().unwrap().contains(&json!({ "required": ["preset"] })));
        let excluded = schema["allOf"].as_array().unwrap();
        assert!(excluded.iter().any(|rule| {
            rule["not"]["required"] == json!(["output_scale", "output_scale_x"])
        }));
        assert!(excluded.iter().any(|rule| {
            rule["not"]["required"] == json!(["output_scale", "output_scale_y"])
        }));
    }

    #[test]
    fn native_physics_option_tools_map_complete_requests() {
        let arguments = json!({
            "gravity": { "x": 1.0, "y": -0.5 },
            "wind": { "x": 0.25, "y": 0.0 }
        });
        let mut expected = arguments.clone();
        expected["op"] = json!("live2d.physics.options.set");
        assert_eq!(
            tool_request("live2d_physics_options_set", &arguments).unwrap(),
            expected
        );
        assert_eq!(
            tool_request("live2d_physics_stabilize", &json!({})).unwrap(),
            json!({ "op": "live2d.physics.stabilize" })
        );

        let tools = tools();
        let set_schema = &tools
            .iter()
            .find(|tool| tool["name"] == "live2d_physics_options_set")
            .unwrap()["inputSchema"];
        assert_eq!(set_schema["required"], json!(["gravity", "wind"]));
        assert_eq!(set_schema["additionalProperties"], false);
        for vector in ["gravity", "wind"] {
            assert_eq!(
                set_schema["properties"][vector]["required"],
                json!(["x", "y"])
            );
            assert_eq!(
                set_schema["properties"][vector]["properties"]["x"]["type"],
                "number"
            );
            assert_eq!(
                set_schema["properties"][vector]["properties"]["y"]["type"],
                "number"
            );
            assert_eq!(
                set_schema["properties"][vector]["additionalProperties"],
                false
            );
        }
        let stabilize_schema = &tools
            .iter()
            .find(|tool| tool["name"] == "live2d_physics_stabilize")
            .unwrap()["inputSchema"];
        assert_eq!(stabilize_schema["additionalProperties"], false);
        assert_eq!(stabilize_schema["properties"], json!({}));
    }

    #[test]
    fn tools_that_close_their_schema_refuse_undeclared_arguments() {
        let mut session = Session::new();
        let call = |session: &mut Session, name: &str, arguments: Value| {
            let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}});
            session.handle_mcp(&request).unwrap()["result"].clone()
        };
        let refused = call(&mut session, "live2d_physics_stabilize", json!({"path":"x.cmo3","op":"save"}));
        assert_eq!(refused["isError"], true);
        let message = refused["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("unknown argument(s) for live2d_physics_stabilize: ") && message.contains("path"), "{message}");
        // Tools whose schema allows additional properties are forwarded as before (the op key is still dropped).
        let open = call(&mut session, "values_reset", json!({"extra":1}));
        assert_eq!(open["isError"], false, "{open}");
        let internal = call(&mut session, "values_reset", json!({"_suffix":"x"}));
        assert_eq!(internal["content"][0]["text"], "unknown argument(s) for values_reset: _suffix", "{internal}");
        // Declared arguments of a closed schema are accepted.
        let options = json!({"gravity":{"x":0.0,"y":-1.0},"wind":{"x":1.0,"y":0.0}});
        let accepted = call(&mut session, "live2d_physics_options_set", options);
        assert!(!accepted["content"][0]["text"].as_str().unwrap().contains("unknown argument"), "{accepted}");
    }
}
