use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-nodevis-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn node(uuid: u64, name: &str, node_type: &str, children: Value, extra: Value) -> Value {
    let mut entry = json!({
        "uuid": uuid, "name": name, "type": node_type, "enabled": true,
        "zsort": 0.0, "transform": transform(),
        "lockToRoot": false, "pinToMesh": false,
    });
    if !children.is_null() {
        entry["children"] = children;
    }
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            entry[key] = value;
        }
    }
    entry
}

fn fixture() -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", json!([
            node(4, "Parent", "Node", json!([
                node(5, "Child", "Node", Value::Null, Value::Null),
            ]), Value::Null),
        ]), Value::Null),
        "param": [],
        "animations": {},
    })
}

fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": fixture()})));
    session
}

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

fn find_node_mut(node: &mut Value, id: u64) -> Option<&mut Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"]
        .as_array_mut()?
        .iter_mut()
        .find_map(|child| find_node_mut(child, id))
}

fn model_node(session: &Session, id: u64) -> Value {
    find_node(&session.snapshot()["model"]["nodes"], id)
        .unwrap_or_else(|| panic!("node {id} missing"))
        .clone()
}

#[test]
fn node_visibility_set_records_history_and_model_delta() {
    let mut session = session();
    let before = session.snapshot();
    let reply =
        result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})))
            .clone();
    assert_eq!(reply, json!({"uuid": 4, "enabled": false}));
    let after = session.snapshot();
    assert_eq!(after["revision"].as_u64(), before["revision"].as_u64().map(|v| v + 1));
    assert_eq!(after["history"]["count"].as_u64(), before["history"]["count"].as_u64().map(|v| v + 1));
    assert_eq!(after["modified"], json!(true));
    let mut expected = before["model"].clone();
    find_node_mut(&mut expected["nodes"], 4).unwrap()["enabled"] = json!(false);
    assert_eq!(after["model"], expected);
    assert_eq!(model_node(&session, 5)["enabled"], json!(true));
    assert_eq!(model_node(&session, 4)["transform"], transform());

    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(model_node(&session, 4)["enabled"], json!(true));
    result(&request(&mut session, json!({"op": "redo"})));
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));
}

#[test]
fn node_visibility_noop_preserves_exact_snapshot_and_stored_keys() {
    let mut session = session();
    let before = session.snapshot();
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    assert_eq!(session.snapshot(), before, "same-value Set must be a full no-op");

    let mut session = Session::new();
    let mut model = fixture();
    find_node_mut(&mut model["nodes"], 4).unwrap().as_object_mut().unwrap().remove("enabled");
    find_node_mut(&mut model["nodes"], 5).unwrap()["enabled"] = json!("yes");
    result(&request(&mut session, json!({"op": "new", "model": model})));
    let before = session.snapshot();
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    assert_eq!(session.snapshot(), before);
    assert!(model_node(&session, 4).get("enabled").is_none(), "absent key stays absent");
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 5, "enabled": true})));
    assert_eq!(session.snapshot(), before);
    assert_eq!(model_node(&session, 5)["enabled"], json!("yes"), "malformed key stays stored");

    let reply = result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": 5}))).clone();
    assert_eq!(reply, json!({"uuid": 5, "enabled": false}));
    assert_eq!(model_node(&session, 5)["enabled"], json!(false));
}

#[test]
fn node_visibility_does_not_fabricate_transform_key() {
    let mut session = Session::new();
    let mut model = fixture();
    find_node_mut(&mut model["nodes"], 4)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("transform");
    result(&request(&mut session, json!({"op": "new", "model": model})));

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})));
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));
    assert!(
        model_node(&session, 4).get("transform").is_none(),
        "set must not fabricate a transform key"
    );

    result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": 4})));
    assert_eq!(model_node(&session, 4)["enabled"], json!(true));
    assert!(
        model_node(&session, 4).get("transform").is_none(),
        "toggle must not fabricate a transform key"
    );
}

#[test]
fn node_visibility_noop_preserves_redo_branch_and_runtime() {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": physics_fixture()})));
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})));
    result(&request(&mut session, json!({"op": "undo"})));
    for _ in 0..5 {
        result(&request(&mut session, json!({"op": "physics.step", "dt": 0.05})));
    }
    let before = session.snapshot();
    assert_eq!(before["physics"]["active"], json!(true));
    assert_eq!(before["canRedo"], json!(true), "undo must leave a redo branch");
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    assert_eq!(
        session.snapshot(),
        before,
        "no-op Set must preserve the redo branch and active runtime"
    );
}

#[test]
fn node_visibility_toggle_twice_and_undo_restores_absence() {
    let mut session = Session::new();
    let mut model = fixture();
    find_node_mut(&mut model["nodes"], 4).unwrap().as_object_mut().unwrap().remove("enabled");
    result(&request(&mut session, json!({"op": "new", "model": model})));

    let reply = result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": 4}))).clone();
    assert_eq!(reply, json!({"uuid": 4, "enabled": false}));
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));
    let reply = result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": 4}))).clone();
    assert_eq!(reply, json!({"uuid": 4, "enabled": true}));
    assert_eq!(model_node(&session, 4)["enabled"], json!(true));

    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));
    result(&request(&mut session, json!({"op": "undo"})));
    assert!(model_node(&session, 4).get("enabled").is_none(), "undo restores key absence");
    assert!(model_node(&session, 4).get("transform").is_some());
    assert_eq!(model_node(&session, 4)["transform"], transform());
}

#[test]
fn node_visibility_all_node_types_editable() {
    let mut session = Session::new();
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", json!([
            node(10, "Masky", "Mask", Value::Null, json!({"mask_keeping": [1, 2]})),
            node(11, "Cam", "Camera", Value::Null, json!({"viewport": [1920.0, 1080.0]})),
            node(12, "Rig", "DepthRigRoot", json!([
                node(13, "Bone", "DepthBone", Value::Null, json!({"restHead": [0.0, 0.0, 0.0]})),
            ]), json!({"rigdata": {"keep": true}})),
            node(14, "Phys", "SimplePhysics", Value::Null, json!({"param": 7, "model_type": "Pendulum"})),
            node(15, "Odd", "FutureType", Value::Null, json!({"unknown_payload": {"a": 1}})),
        ]), Value::Null),
        "param": [],
        "animations": {},
    });
    result(&request(&mut session, json!({"op": "new", "model": model})));

    for id in [1u64, 10, 11, 12, 13, 14, 15] {
        let before_node = model_node(&session, id);
        result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": id})));
        let after_node = model_node(&session, id);
        let mut expected = before_node.clone();
        expected["enabled"] = json!(false);
        assert_eq!(after_node, expected, "node {id} changed beyond enabled");
        assert_eq!(after_node["transform"], before_node["transform"]);
    }
}

#[test]
fn node_visibility_invalid_inputs_fail_unchanged() {
    let mut session = session();
    let before = session.snapshot();
    for enabled in [Value::Null, json!("yes"), json!(1), json!([true])] {
        for op in ["node.enabled.set", "node.enabled.toggle"] {
            let response = request(
                &mut session,
                json!({"op": op, "id": 4, "enabled": enabled}),
            );
            assert_eq!(response["succeeded"], false, "{op} accepted {enabled}");
            assert_eq!(session.snapshot(), before);
        }
    }
    let response = request(&mut session, json!({"op": "node.enabled.set", "id": 4}));
    assert_eq!(response["succeeded"], false);
    assert_eq!(session.snapshot(), before);
    for id in [json!(0), json!(-1), json!(1.5), json!("4"), json!(4294967295u64), json!(4294967296u64), Value::Null] {
        for op in ["node.enabled.set", "node.enabled.toggle"] {
            let mut request_body = json!({"op": op, "id": id});
            if op == "node.enabled.set" {
                request_body["enabled"] = json!(false);
            }
            let response = request(&mut session, request_body);
            assert_eq!(response["succeeded"], false, "{op} accepted id {id}");
            assert_eq!(session.snapshot(), before);
        }
    }
}

#[test]
fn node_visibility_mcp_direct_tools_and_legacy_alias() {
    let mut session = session();
    let call = |session: &mut Session, id: u64, name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .expect("tools/call did not reply")
    };
    let reply = call(&mut session, 1, "node_enabled_set", json!({"id": 4, "enabled": false}));
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let text: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, json!({"uuid": 4, "enabled": false}));
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));

    let reply = call(&mut session, 2, "node_enabled_toggle", json!({"id": 4}));
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let text: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, json!({"uuid": 4, "enabled": true}));

    let reply = call(&mut session, 3, "node_enabled_toggle", json!({"id": 4, "enabled": true}));
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(model_node(&session, 4)["enabled"], json!(true));

    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 90, "method": "tools/list"}))
        .expect("tools/list did not reply");
    let tools = reply["result"]["tools"].as_array().unwrap();
    let tool_schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))["inputSchema"]
            .clone()
    };
    let set_schema = tool_schema("node_enabled_set");
    assert_eq!(set_schema["required"], json!(["id", "enabled"]));
    assert_eq!(set_schema["properties"]["id"]["type"], json!("integer"));
    assert_eq!(set_schema["properties"]["enabled"]["type"], json!("boolean"));
    let toggle_schema = tool_schema("node_enabled_toggle");
    assert_eq!(toggle_schema["required"], json!(["id"]));
    assert_eq!(toggle_schema["properties"]["id"]["type"], json!("integer"));
    assert!(
        toggle_schema["properties"].get("enabled").is_none(),
        "toggle schema must not accept enabled"
    );
    let legacy_schema = tool_schema("NodeCommand_ToggleVisibility");
    assert_eq!(legacy_schema["required"], json!(["context"]));
    let context_schema = &legacy_schema["properties"]["context"];
    assert_eq!(context_schema["required"], json!(["nodes"]));
    assert_eq!(context_schema["properties"]["nodes"]["type"], json!("array"));
    assert_eq!(context_schema["properties"]["nodes"]["minItems"], json!(1));
    assert_eq!(
        context_schema["properties"]["nodes"]["items"]["type"],
        json!("integer")
    );

    let before = session.snapshot();
    let reply = call(
        &mut session,
        4,
        "NodeCommand_ToggleVisibility",
        json!({"context": {"nodes": [4, 5]}}),
    );
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert_eq!(model_node(&session, 4)["enabled"], json!(false));
    assert_eq!(model_node(&session, 5)["enabled"], json!(true), "later context entries untouched");

    let state = session.snapshot();
    for arguments in [
        json!({"context": {"nodes": ["bad", 4]}}),
        json!({"context": {"nodes": []}}),
        json!({"context": {"nodes": "bad"}}),
        json!({"context": {}}),
        json!({}),
        json!({"context": {"nodes": [4]}, "id": 5}),
        json!({"context": {"nodes": [4]}, "enabled": true}),
    ] {
        let reply = call(&mut session, 5, "NodeCommand_ToggleVisibility", arguments.clone());
        assert_eq!(reply["result"]["isError"], true, "accepted {arguments}");
        assert_eq!(session.snapshot(), state);
    }
    assert_eq!(session.snapshot()["revision"], state["revision"]);
    assert_eq!(before["history"]["count"].as_u64().map(|v| v + 1), session.snapshot()["history"]["count"].as_u64());
}

fn textured_fixture() -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let texture = png.into_inner();
    let part = json!({
        "uuid": 5, "name": "Child", "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0],
                 "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "vertices": [[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]],
        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        "indices": [0, 1, 2, 0, 2, 3],
        "textures": [0], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false,
            "children": [
                {"uuid": 4, "name": "Parent", "type": "Node", "enabled": true, "zsort": 0.0,
                 "transform": transform(), "lockToRoot": false, "pinToMesh": false,
                 "children": [part]},
            ]},
        "param": [], "animations": {}, "future_field": {"keep": [1, 2, 3]},
    });
    let raw = serde_json::to_vec(&model).unwrap();
    let mut file = b"TRNSRTS\0".to_vec();
    file.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    file.extend_from_slice(&raw);
    file.extend_from_slice(b"TEX_SECT");
    file.extend_from_slice(&1u32.to_be_bytes());
    file.extend_from_slice(&(texture.len() as u32).to_be_bytes());
    file.push(0);
    file.extend_from_slice(&texture);
    file.extend_from_slice(b"EXT_SECT");
    file.extend_from_slice(&1u32.to_be_bytes());
    file.extend_from_slice(&4u32.to_be_bytes());
    file.extend_from_slice(b"blob");
    file.extend_from_slice(&3u32.to_be_bytes());
    file.extend_from_slice(b"\x01\x02\x03");
    file.extend_from_slice(b"FUTURE_TAIL");
    file
}

fn render_png(session: &mut Session) -> image::RgbaImage {
    let response = request(
        session,
        json!({"op": "render", "width": 8, "height": 8, "bounds": [0, 0, 8, 8]}),
    );
    let png = result(&response)["png"].as_str().unwrap().to_owned();
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).unwrap();
    image::load_from_memory(&bytes).unwrap().into_rgba8()
}

#[test]
fn node_visibility_pixels_inherit_and_save_opaque_tail() {
    let directory = temp_dir();
    let source = directory.join("fixture.inx");
    let saved = directory.join("saved.inx");
    let file_bytes = textured_fixture();
    fs::write(&source, &file_bytes).unwrap();
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "open", "path": source.to_str().unwrap()})));

    let image = render_png(&mut session);
    assert_eq!(image.get_pixel(4, 4).0, [255, 0, 0, 255], "center must be opaque red");
    assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 0], "corner must be transparent");

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})));
    let image = render_png(&mut session);
    assert_eq!(image.get_pixel(4, 4).0, [0, 0, 0, 0], "hidden parent suppresses child pixels");
    assert_eq!(model_node(&session, 5)["enabled"], json!(true), "local flag stays true");

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 5, "enabled": false})));
    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    let image = render_png(&mut session);
    assert_eq!(image.get_pixel(4, 4).0, [0, 0, 0, 0], "locally disabled child stays hidden under shown parent");

    result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": 5})));
    let image = render_png(&mut session);
    assert_eq!(image.get_pixel(4, 4).0, [255, 0, 0, 255], "shown child renders red again");

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})));
    result(&request(&mut session, json!({"op": "save", "path": saved})));
    let saved_model = result(&request(&mut session, json!({"op": "snapshot"})))["model"].clone();
    let saved_bytes = fs::read(&saved).unwrap();
    assert_eq!(saved_bytes[..8], b"TRNSRTS\0"[..]);
    let length = u32::from_be_bytes(saved_bytes[8..12].try_into().unwrap()) as usize;
    let saved_json: Value = serde_json::from_slice(&saved_bytes[12..12 + length]).unwrap();
    assert_eq!(saved_json, saved_model);
    let source_length =
        u32::from_be_bytes(file_bytes[8..12].try_into().unwrap()) as usize;
    assert_eq!(
        &saved_bytes[12 + length..],
        &file_bytes[12 + source_length..],
        "opaque tail bytes must be preserved exactly"
    );
    assert_eq!(saved_json["future_field"], json!({"keep": [1, 2, 3]}));
    assert_eq!(fs::read(&source).unwrap(), file_bytes, "fixture bytes unchanged");

    let mut reopened = Session::new();
    result(&request(&mut reopened, json!({"op": "open", "path": saved.to_str().unwrap()})));
    assert_eq!(model_node(&reopened, 4)["enabled"], json!(false));
    assert_eq!(model_node(&reopened, 5)["enabled"], json!(true));
    let image = render_png(&mut reopened);
    assert_eq!(image.get_pixel(4, 4).0, [0, 0, 0, 0]);
    fs::remove_dir_all(directory).unwrap();
}

fn physics_fixture() -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "physics": {"gravity": 1.0, "pixelsPerMeter": 1.0},
        "param": [{"uuid": 7, "defaults": [0.0, 0.0], "is_vec2": false,
                   "min": [0.0, 0.0], "max": [1.0, 1.0],
                   "axis_points": [[0.0, 1.0], [0.0]], "bindings": []}],
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true,
            "transform": transform(), "children": [
                {"uuid": 4, "name": "Swing", "type": "Node", "enabled": true,
                 "transform": {"trans": [1.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                 "children": [
                    {"uuid": 5, "name": "Path", "type": "PathDeformer", "enabled": true,
                     "vertices": [0.0, 0.0, 0.0, 10.0], "physics_only": true,
                     "physics": {"type": "Pendulum", "damping": 0.0, "restore_constant": 0.0,
                                 "gravity": 1.0, "input_scale": 0.0, "propagate_scale": 0.0},
                     "transform": transform(), "children": []},
                    {"uuid": 6, "name": "Phys", "type": "SimplePhysics", "enabled": true,
                     "param": 7, "model_type": "Pendulum", "map_mode": "AngleLength",
                     "gravity": 1.0, "length": 10.0, "frequency": 1.0,
                     "angle_damping": 0.5, "length_damping": 0.5,
                     "output_scale": [1.0, 1.0], "local_only": false,
                     "transform": transform(), "children": []},
                 ]},
            ]},
        "animations": {},
    })
}

#[test]
fn node_visibility_physics_reset_and_driver_exclusion() {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": physics_fixture()})));
    for _ in 0..5 {
        result(&request(&mut session, json!({"op": "physics.step", "dt": 0.05})));
    }
    let stepped = session.snapshot();
    assert_eq!(stepped["physics"]["active"], json!(true));
    assert!(stepped["physics"]["pathDeformation"]
        .as_object().is_some_and(|map| !map.is_empty()), "{stepped}");

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    assert_eq!(session.snapshot(), stepped, "no-op Set must not reset physics output");

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false})));
    assert_eq!(session.snapshot()["physics"]["active"], json!(false), "changed Set resets physics");

    for _ in 0..5 {
        result(&request(&mut session, json!({"op": "physics.step", "dt": 0.05})));
    }
    let deformation = &session.snapshot()["physics"]["pathDeformation"];
    assert!(
        deformation.is_null() || deformation.as_object().is_some_and(|map| map.is_empty()),
        "disabled ancestor excludes the path driver: {deformation}"
    );
    assert_eq!(model_node(&session, 5)["enabled"], json!(true));

    result(&request(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": true})));
    for _ in 0..5 {
        result(&request(&mut session, json!({"op": "physics.step", "dt": 0.05})));
    }
    assert!(session.snapshot()["physics"]["pathDeformation"]
        .as_object().is_some_and(|map| !map.is_empty()),
        "shown ancestor restores the path driver");
}

#[test]
fn node_visibility_pending_deform_gate_and_bound_target() {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new"})));
    let rig = result(&request(&mut session, json!({
        "op": "node.create", "className": "DepthRigRoot", "name": "Rig"}))).clone();
    let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
    let bone = result(&request(&mut session, json!({
        "op": "node.create", "className": "DepthBone", "name": "Bone", "parents": [rig_id],
        "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 100.0, 0.0]}))).clone();
    let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
    let grid = result(&request(&mut session, json!({
        "op": "node.create", "className": "GridDeformer", "name": "Grid"}))).clone();
    let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
    result(&request(&mut session, json!({
        "op": "deformer.define_grid", "nodes": [grid_id],
        "axisX": [0.0, 1.0], "axisY": [0.0, 1.0]})));
    result(&request(&mut session, json!({
        "op": "rig.add_source", "root": rig_id, "target": grid_id, "bone": bone_id})));

    let before = session.snapshot();
    assert_eq!(before["deformPending"], json!(false));
    result(&request(&mut session, json!({"op": "node.enabled.toggle", "id": grid_id})));
    let after = session.snapshot();
    let mut expected_model = before["model"].clone();
    find_node_mut(&mut expected_model["nodes"], grid_id).unwrap()["enabled"] = json!(false);
    assert_eq!(
        after["model"], expected_model,
        "toggle must change only the bound target's enabled flag"
    );
    assert_eq!(after["deformPending"], json!(false), "visibility must not leave a pending deform");
    result(&request(&mut session, json!({
        "op": "node.rename", "id": grid_id, "name": "GridTwo"})));
    assert_eq!(model_node(&session, grid_id)["name"], json!("GridTwo"));

    let big = result(&request(&mut session, json!({
        "op": "node.create", "className": "GridDeformer", "name": "Big"}))).clone();
    let big_id = big["created"][0]["uuid"].as_u64().unwrap();
    let axis_x: Vec<f64> = (0..400).map(|i| i as f64 * 0.01).collect();
    let axis_y: Vec<f64> = (0..500).map(|i| i as f64 * 0.01).collect();
    result(&request(&mut session, json!({
        "op": "deformer.define_grid", "nodes": [big_id],
        "axisX": axis_x, "axisY": axis_y})));
    result(&request(&mut session, json!({
        "op": "rig.add_source", "root": rig_id, "target": big_id, "bone": bone_id})));
    let parameter = result(&request(&mut session, json!({
        "op": "parameter.create", "name": "Pose", "dimensions": 1,
        "min": [-1.0, 0.0], "max": [1.0, 1.0]}))).clone();
    let parameter_id = parameter["uuid"].as_u64().unwrap();
    result(&request(&mut session, json!({
        "op": "binding.set_trs", "parameter": parameter_id, "nodes": [bone_id],
        "key": [2, 0], "translation": [4.0, 0.0]})));
    result(&request(&mut session, json!({
        "op": "node.transform", "id": bone_id, "translation": [12.0, 0.0, 0.0]})));
    assert_eq!(session.snapshot()["deformPending"], json!(true), "over-budget flush leaves pending deform");
    let pending = session.snapshot();
    session.set_video_encoder("");
    assert_eq!(request(&mut session, json!({"op":"export.video.probe"}))["succeeded"], true);
    assert_eq!(request(&mut session, json!({"op":"export.video.probe.status"}))["succeeded"], true);
    let command = request(&mut session, json!({"op":"export.video.command","path":"unused.mp4"}));
    assert_eq!(command["succeeded"], false);
    assert_eq!(command["message"], "a deform refresh is still pending");
    assert_eq!(session.snapshot(), pending);
    for op in ["node.enabled.set", "node.enabled.toggle"] {
        let mut body = json!({"op": op, "id": grid_id});
        if op == "node.enabled.set" {
            body["enabled"] = json!(true);
        }
        let response = request(&mut session, body);
        assert_eq!(response["succeeded"], false, "{op} bypassed pending deform");
        assert_eq!(session.snapshot(), pending);
    }
}
