//! D command names (`Live2DCommand_*`, `Motion3Command_*`, `FileCommand_*`, `ToolCommand_*`,
//! `ViewCommand_SaveScreenshot`, `ViewportCommand_*`, `CommandScopeCommand_*`) for the commands that still
//! exist in D, mapped onto the session operations that carry their behaviour. Argument names are D's.

use serde_json::{json, Map, Value};

/// D tool name, session op. Arguments pass through unchanged.
const DIRECT: &[(&str, &str)] = &[
    ("Live2DCommand_AuthoringInspect", "live2d.authoring.inspect"),
    ("Live2DCommand_AuthoringRead", "live2d.authoring.read"),
    ("Live2DCommand_AnimationRead", "live2d.animation.read"),
    ("Live2DCommand_AnimationBindings", "live2d.animation.bindings"),
    ("Live2DCommand_AnimationKeySet", "live2d.animation.key.set"),
    ("Live2DCommand_AuthoringLinearWeights", "live2d.authoring.linear_weights"),
    ("Live2DCommand_AuthoringPrimaryForms", "live2d.authoring.primary_forms"),
    ("Live2DCommand_AuthoringMorphForms", "live2d.authoring.morph_forms"),
    ("Live2DCommand_FormSetVertex", "live2d.form.set_vertex"),
    ("Live2DCommand_FormSetOpacity", "live2d.form.set_opacity"),
    ("Live2DCommand_Rename", "live2d.rename"),
    ("Live2DCommand_ParameterSet", "live2d.parameter.set"),
    ("Motion3Command_Import", "motion3.import"),
    ("Motion3Command_List", "motion3.list"),
    ("Motion3Command_Read", "motion3.read"),
    ("Motion3Command_Sample", "motion3.sample"),
    ("Motion3Command_Export", "motion3.export"),
    ("Motion3Command_Bind", "motion3.bind"),
    ("Motion3Command_Bindings", "motion3.bindings"),
    ("Motion3Command_Pose", "motion3.pose"),
    ("Motion3Command_Seek", "motion3.seek"),
    ("Motion3Command_Advance", "motion3.advance"),
    ("Motion3Command_Reset", "motion3.reset"),
    ("FileCommand_ImportINP", "import.inp"),
    ("FileCommand_ImportImageFolder", "import.folder"),
    ("FileCommand_MergeINP", "merge.inp"),
    ("ToolCommand_ImportSessionData", "tool.import_session_data"),
    ("ToolCommand_PremultTexture", "tool.premultiply_textures"),
    ("ToolCommand_RebleedTexture", "tool.rebleed_textures"),
    ("ToolCommand_GenerateFakeLayerName", "tool.generate_fake_layer_names"),
    ("ToolCommand_AttemptRepairPuppet", "tool.repair"),
    ("ToolCommand_RegenerateNodeIDs", "tool.regenerate_node_ids"),
    ("ViewportCommand_ResetPhysics", "physics.reset"),
    ("ViewportCommand_ResetParameters", "values.reset"),
];

/// Tools whose arguments are reshaped; the schema is written out in [`tools`].
const SHAPED: &[&str] = &[
    "Motion3Command_SetPoint",
    "FileCommand_OpenFile",
    "FileCommand_SaveFile",
    "FileCommand_MergeImageFiles",
    "ViewCommand_SaveScreenshot",
];

pub(crate) const SCOPE_TOOL: &str = "CommandScopeCommand_GetCurrentCommandScope";

fn passthrough(op: &str, args: &Map<String, Value>) -> Map<String, Value> {
    let mut request = args.clone();
    request.remove("op");
    request.insert("op".into(), json!(op));
    request
}

fn rename(args: &Map<String, Value>, op: &str, from: &str, to: &str) -> Result<Value, String> {
    let mut request = passthrough(op, args);
    match request.remove(from) {
        Some(value) => {
            request.insert(to.into(), value);
            Ok(Value::Object(request))
        }
        None => Err(format!("{from} is required")),
    }
}

/// `None` when `name` is not one of the D aliases.
pub(crate) fn request(name: &str, args: &Map<String, Value>) -> Option<Result<Value, String>> {
    if let Some((_, op)) = DIRECT.iter().find(|(alias, _)| *alias == name) {
        return Some(Ok(Value::Object(passthrough(op, args))));
    }
    Some(match name {
        "FileCommand_OpenFile" => rename(args, "open", "file", "path"),
        "FileCommand_SaveFile" => rename(args, "save", "file", "path"),
        "ViewCommand_SaveScreenshot" => rename(args, "view.screenshot", "filename", "path"),
        "FileCommand_MergeImageFiles" => {
            let mut request = passthrough("merge.images", args);
            // D adds the Parts under the selected node.
            let first = args.get("context").and_then(|context| context.get("nodes")).and_then(|nodes| nodes.get(0));
            if let (Some(parent), false) = (first, request.contains_key("parent")) {
                request.insert("parent".into(), parent.clone());
            }
            request.remove("context");
            Ok(Value::Object(request))
        }
        "Motion3Command_SetPoint" => (|| {
            if args.contains_key("time") || args.contains_key("value") {
                return Err("Put Motion3 point time/value inside changes".to_owned());
            }
            let changes = args
                .get("changes")
                .and_then(Value::as_object)
                .ok_or("Motion3 point changes must be an object with time and/or value")?;
            let mut request = passthrough("motion3.point.set", args);
            request.remove("changes");
            for (key, value) in changes {
                if key != "time" && key != "value" {
                    return Err(format!("Unknown Motion3 point change: {}", crate::mcp::echoed([key.as_str()])));
                }
                request.insert(key.clone(), value.clone());
            }
            Ok(Value::Object(request))
        })(),
        _ => return None,
    })
}

fn prop(kind: &str) -> Value {
    match kind {
        "s" => json!({ "type": "string" }),
        "i" => json!({ "type": "integer" }),
        "n" => json!({ "type": "number" }),
        "b" => json!({ "type": "boolean" }),
        "a" => json!({ "type": "array" }),
        _ => json!({ "type": "object" }),
    }
}

fn shaped(name: &str, description: &str, props: &[(&str, &str)], required: &[&str]) -> Value {
    let properties: Map<String, Value> = props.iter().map(|(key, kind)| ((*key).to_owned(), prop(kind))).collect();
    json!({ "name": name, "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": true } })
}

/// Tool definitions of the aliases. `existing` is the tool list so far: an alias of a plain operation copies
/// the description and schema of that operation's own tool.
pub(crate) fn tools(existing: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for (alias, op) in DIRECT {
        let own = op.replace('.', "_");
        let found = existing.iter().find(|tool| tool["name"] == own.as_str());
        let (description, schema) = match found {
            Some(tool) => (tool["description"].clone(), tool["inputSchema"].clone()),
            None => (
                json!(format!("D command {alias} (operation {op}).")),
                json!({ "type": "object", "additionalProperties": true }),
            ),
        };
        out.push(json!({ "name": alias, "description": description, "inputSchema": schema }));
    }
    out.push(shaped("Motion3Command_SetPoint", "Update optional time or value fields on a Motion3 curve point.",
        &[("name", "s"), ("curve", "i"), ("point", "i"), ("changes", "o")], &["name", "curve", "point", "changes"]));
    out.push(shaped("FileCommand_OpenFile", "Open puppet from specified file.", &[("file", "s")], &["file"]));
    out.push(shaped("FileCommand_SaveFile", "Save puppet to specified file.", &[("file", "s")], &["file"]));
    let mut merge = shaped("FileCommand_MergeImageFiles", "Merge image files into the current puppet without showing a dialog.",
        &[("paths", "s"), ("context", "o")], &["paths"]);
    merge["inputSchema"]["properties"]["context"] = crate::mcp::context_nodes_schema();
    out.push(merge);
    out.push(shaped("ViewCommand_SaveScreenshot", "Save a PNG of the whole puppet to a file.",
        &[("filename", "s"), ("size", "i")], &["filename"]));
    out.push(shaped(SCOPE_TOOL,
        "Return the command scope. The Rust host always reports the normal scope with no finish or cancel commands.",
        &[], &[]));
    debug_assert!(SHAPED.iter().all(|name| out.iter().any(|tool| tool["name"] == *name)));
    out
}

/// The answer of `CommandScopeCommand_GetCurrentCommandScope`: this build has the normal scope only, so
/// no command finishes or cancels a scope and every listed tool is available.
pub(crate) fn scope_response(tool_list: &[Value]) -> Value {
    let scope = json!({ "id": "normal", "type": "NormalCommandScope", "description": "Normal editor command scope" });
    let mut available: Vec<Value> = tool_list
        .iter()
        .filter(|tool| tool["name"].as_str().is_some_and(|name| name.contains("Command_")))
        .map(|tool| {
            let name = tool["name"].as_str().unwrap_or_default();
            json!({ "commandId": name.replacen("Command_", "Command.", 1), "tool": name,
                    "label": name, "description": tool["description"] })
        })
        .collect();
    available.sort_by(|a, b| a["tool"].as_str().cmp(&b["tool"].as_str()));
    json!({
        "scope": scope,
        "scopeStack": [scope],
        "availableCommands": available,
        "lifecycle": { "finish": [], "cancel": [] },
        "toolSpecification": "Use tools/list to read the input schema for each returned tool name.",
    })
}
