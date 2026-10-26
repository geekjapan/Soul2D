use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn session() -> Session {
    session_of(model())
}

fn session_of(model: Value) -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":model})));
    session
}

fn model() -> Value {
    json!({
        "meta": { "name": "fixture", "version": "1.0-alpha" },
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
            "zsort": 0.0,
            "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
            "lockToRoot": false, "pinToMesh": false,
        },
        "param": [{
            "uuid": 17, "name": "Move", "is_vec2": true,
            "min": [-1.0,-1.0], "max": [1.0,1.0], "defaults": [0.0,0.0],
            "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive", "bindings": [],
        }],
        "animations": {"Idle": {
            "length": 12, "timestep": 0.1, "additive": false, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1,
            "lanes": [
                {"uuid":17,"target":0,"interpolation":"Linear","merge_mode":"Additive","keyframes":[]},
                {"uuid":17,"target":1,"interpolation":"Linear","merge_mode":"Forced",
                 "keyframes":[{"frame":3,"value":0.5,"tension":0.5}]},
            ],
        }},
    })
}

fn lanes(session: &Session) -> Value {
    session.snapshot()["model"]["animations"]["Idle"]["lanes"].clone()
}

#[test]
fn set_changes_one_lane_even_without_keys_and_undoes() {
    let mut session = session();
    result(&request(&mut session, json!({
        "op":"animation.lane.set","name":"Idle","parameter":17,"axis":0,
        "interpolation":"Cubic","merge_mode":"Forced",
    })));
    let after = lanes(&session);
    assert_eq!(after[0]["interpolation"], "Cubic");
    assert_eq!(after[0]["merge_mode"], "Forced");
    assert_eq!(after[1]["interpolation"], "Linear");
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(lanes(&session)[0]["interpolation"], "Linear");
    assert_eq!(lanes(&session)[0]["merge_mode"], "Additive");
}

#[test]
fn delete_removes_only_the_addressed_lane_and_undoes() {
    let mut session = session();
    result(&request(&mut session, json!({
        "op":"animation.lane.delete","name":"Idle","parameter":17,"axis":0,
    })));
    assert_eq!(
        lanes(&session),
        json!([{"uuid":17,"target":1,"interpolation":"Linear","merge_mode":"Forced",
                "keyframes":[{"frame":3,"value":0.5,"tension":0.5}]}])
    );
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(lanes(&session).as_array().unwrap().len(), 2);
}

#[test]
fn malformed_lane_requests_are_rejected() {
    let mut session = session();
    for bad in [
        json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0}),
        json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0,"interpolation":"Wavy"}),
        json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0,"merge_mode":"Odd"}),
        json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":2,"interpolation":"Linear"}),
        json!({"op":"animation.lane.delete","name":"Missing","parameter":17,"axis":0}),
        json!({"op":"animation.lane.delete","name":"Idle","parameter":99,"axis":0}),
    ] {
        assert_eq!(request(&mut session, bad.clone())["succeeded"], false, "{bad}");
    }
    assert_eq!(lanes(&session).as_array().unwrap().len(), 2);
}

#[test]
fn deleting_the_same_lane_twice_fails_the_second_time() {
    let mut session = session();
    let delete = json!({"op":"animation.lane.delete","name":"Idle","parameter":17,"axis":0});
    result(&request(&mut session, delete.clone()));
    assert_eq!(request(&mut session, delete)["succeeded"], false);
}

fn clip(session: &Session) -> Value {
    session.snapshot()["model"]["animations"]["Idle"].clone()
}

/// The fixture plus a parameter 18 whose X lane has a key.
fn shy_session() -> Session {
    let mut model = model();
    let mut plain = model["param"][0].clone();
    plain["uuid"] = json!(18);
    plain["name"] = json!("Tilt");
    model["param"].as_array_mut().unwrap().push(plain);
    model["animations"]["Idle"]["lanes"].as_array_mut().unwrap().push(
        json!({"uuid":18,"target":0,"interpolation":"Linear","merge_mode":"Forced","keyframes":[{"frame":1,"value":0.5}]}));
    session_of(model)
}

fn shy_flags(session: &Session) -> (Vec<bool>, bool) {
    let clip = clip(session);
    let lanes = clip["lanes"].as_array().unwrap().iter().map(|lane| lane.get("shy") == Some(&json!(true))).collect();
    (lanes, clip.get("shyActive") == Some(&json!(true)))
}

#[test]
fn shy_marks_lanes_and_turns_on_like_the_cubism_menu_and_undoes() {
    let mut session = shy_session();
    let opened = clip(&session);
    let shy = |fields: Value| {
        let mut call = json!({"op":"animation.shy.set","name":"Idle"});
        call.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        call
    };
    result(&request(&mut session, json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":1,"shy":true})));
    assert_eq!(clip(&session)["lanes"][1]["shy"], true);
    assert_eq!(shy_flags(&session), (vec![false, true, false], false));
    for (fields, expected) in [
        (json!({"mark":"unkeyed"}), (vec![true, false, false], true)),
        (json!({"active":false}), (vec![true, false, false], false)),
        (json!({"mark":"listed","parameters":[18]}), (vec![false, false, true], true)),
        (json!({"active":false}), (vec![false, false, true], false)),
        (json!({"mark":"unlisted","parameters":[18]}), (vec![true, true, false], true)),
        (json!({"active":false}), (vec![true, true, false], false)),
        (json!({"mark":"all"}), (vec![true, true, true], false)),
        (json!({"active":true}), (vec![true, true, true], true)),
        (json!({"mark":"all"}), (vec![true, true, true], true)),
        (json!({"mark":"unkeyed","active":false}), (vec![true, false, false], false)),
    ] {
        result(&request(&mut session, shy(fields.clone())));
        assert_eq!(shy_flags(&session), expected, "{fields}");
    }
    result(&request(&mut session, shy(json!({"mark":"all","active":true}))));
    result(&request(&mut session, shy(json!({"mark":"none"}))));
    assert_eq!(clip(&session), opened, "clearing leaves no shy field behind");
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(shy_flags(&session), (vec![true, true, true], true));
    result(&request(&mut session, json!({"op":"animation.lane.set","name":"Idle","parameter":18,"axis":0,"shy":false})));
    assert_eq!(clip(&session)["lanes"][2], opened["lanes"][2]);
}

#[test]
fn malformed_shy_requests_and_fields_are_refused() {
    let mut session = shy_session();
    let opened = session.snapshot();
    for (bad, message) in [
        (json!({"op":"animation.shy.set","name":"Idle"}), "animation.shy.set requires mark or active"),
        (json!({"op":"animation.shy.set","name":"Idle","mark":"some"}), "mark must be all, unkeyed, listed, unlisted or none"),
        (json!({"op":"animation.shy.set","name":"Idle","mark":1}), "mark must be all, unkeyed, listed, unlisted or none"),
        (json!({"op":"animation.shy.set","name":"Idle","mark":"listed"}),
            "parameters is required with mark listed or unlisted, and only with them"),
        (json!({"op":"animation.shy.set","name":"Idle","mark":"all","parameters":[18]}),
            "parameters is required with mark listed or unlisted, and only with them"),
        (json!({"op":"animation.shy.set","name":"Idle","active":true,"parameters":[18]}),
            "parameters is required with mark listed or unlisted, and only with them"),
        (json!({"op":"animation.shy.set","name":"Idle","mark":"listed","parameters":[99]}), "parameter 99 was not found"),
        (json!({"op":"animation.shy.set","name":"Idle","active":"yes"}), "active must be a boolean"),
        (json!({"op":"animation.shy.set","name":"Missing","active":true}), "animation \"Missing\" was not found"),
        (json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0,"shy":"yes"}), "shy must be a boolean"),
        (json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0}),
            "animation.lane.set requires interpolation, merge_mode, shy or frozen"),
    ] {
        assert_eq!(request(&mut session, bad.clone())["message"], message, "{bad}");
        assert_eq!(session.snapshot(), opened);
    }
    for (fields, message) in [
        (json!({"shyActive":1}), "animation.shyActive must be a boolean"),
        (json!({"shyActive":1, "lanes":null}), "animation.shyActive must be a boolean"),
        (json!({"lanes":[{"uuid":17,"target":0,"shy":"yes","keyframes":[]}]}), "animation.lanes[0].shy must be a boolean"),
    ] {
        let mut model = model();
        model["animations"]["Idle"].as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        let mut session = session_of(model);
        let reply = request(&mut session, json!({"op":"animation.set","name":"Idle","length":20}));
        assert_eq!(reply["message"], message);
    }
}

#[test]
fn frozen_lanes_hold_the_freeze_frame_and_refuse_edits_until_freeze_is_off() {
    let mut session = shy_session();
    let key = |parameter: u32, axis: u32, frame: i64, value: f64| {
        json!({"op":"animation.key.set","name":"Idle","parameter":parameter,"axis":axis,"frame":frame,"value":value})
    };
    let values = |session: &mut Session, frame: f64| {
        result(&request(session, json!({"op":"animation.sample","name":"Idle","frame":frame})))["values"].clone()
    };
    let sample = |session: &mut Session, frame: f64| values(session, frame)["18"].clone();
    result(&request(&mut session, key(18, 0, 9, -0.5)));
    result(&request(&mut session, key(17, 1, 9, -0.25)));
    let free = values(&mut session, 9.0)["17"].clone();
    assert_ne!(free, values(&mut session, 1.0)["17"], "Move Y moves between frames 1 and 9");
    assert_eq!(sample(&mut session, 9.0), json!([-0.5, 0.0]));
    result(&request(&mut session, json!({"op":"animation.lane.set","name":"Idle","parameter":18,"axis":0,"frozen":true})));
    assert_eq!(sample(&mut session, 9.0), json!([-0.5, 0.0]), "a mark alone holds nothing");
    result(&request(&mut session, json!({"op":"animation.freeze.set","name":"Idle","frame":1})));
    assert_eq!(clip(&session)["freezeFrame"], 1);
    assert_eq!(sample(&mut session, 9.0), json!([0.5, 0.0]), "the frozen lane holds frame 1");
    assert_eq!(values(&mut session, 9.0)["17"], free, "a lane that is not frozen still moves");
    assert_eq!(sample(&mut session, 5.0), json!([0.5, 0.0]));

    let frozen = "the lane of parameter 18 axis 0 in animation \"Idle\" is frozen; turn freeze off first";
    let opened = session.snapshot();
    for edit in [
        key(18, 0, 5, 0.0),
        json!({"op":"animation.key.delete","name":"Idle","parameter":18,"axis":0,"frame":9}),
        json!({"op":"animation.key.add","name":"Idle","parameter":18,"frame":4}),
        json!({"op":"animation.lane.set","name":"Idle","parameter":18,"axis":0,"interpolation":"Cubic"}),
        json!({"op":"animation.lane.set","name":"Idle","parameter":18,"axis":0,"merge_mode":"Additive"}),
        json!({"op":"animation.lane.delete","name":"Idle","parameter":18,"axis":0}),
    ] {
        assert_eq!(request(&mut session, edit.clone())["message"], frozen, "{edit}");
        assert_eq!(session.snapshot(), opened);
    }
    let frozen_keys = |session: &Session| clip(session)["lanes"][2]["keyframes"].clone();
    let kept = frozen_keys(&session);
    result(&request(&mut session, json!({"op":"animation.key.delete_all","name":"Idle","frame":9})));
    assert_eq!(frozen_keys(&session), kept, "unkey all passes the frozen lane by");
    assert_eq!(clip(&session)["lanes"][1]["keyframes"], json!([{"frame":3,"value":0.5,"tension":0.5}]));
    result(&request(&mut session, json!({"op":"animation.key.add_all","name":"Idle","frame":4})));
    assert_eq!(frozen_keys(&session), kept, "key all passes the frozen lane by");
    assert_eq!(clip(&session)["lanes"][0]["keyframes"][0]["frame"], 4);
    result(&request(&mut session, json!({"op":"undo"})));
    result(&request(&mut session, json!({"op":"undo"})));
    result(&request(&mut session, key(17, 1, 5, 0.25)));
    result(&request(&mut session, json!({"op":"animation.lane.set","name":"Idle","parameter":18,"axis":0,"shy":true})));
    result(&request(&mut session, json!({"op":"animation.freeze.set","name":"Idle","frame":null})));
    assert!(clip(&session).get("freezeFrame").is_none());
    assert_eq!(sample(&mut session, 9.0), json!([-0.5, 0.0]));
    result(&request(&mut session, json!({"op":"animation.key.add_all","name":"Idle","frame":6})));
    let frames: Vec<Value> = frozen_keys(&session).as_array().unwrap().iter().map(|key| key["frame"].clone()).collect();
    assert_eq!(frames, [json!(1), json!(6), json!(9)], "key all reaches a marked lane while freeze is off");
    result(&request(&mut session, json!({"op":"undo"})));
    result(&request(&mut session, key(18, 0, 5, 0.0)));
    result(&request(&mut session, json!({"op":"undo"})));
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(clip(&session)["freezeFrame"], 1, "undo brings freeze back");

    let flags = |session: &Session| {
        let clip = clip(session);
        let lanes: Vec<bool> = clip["lanes"].as_array().unwrap().iter().map(|lane| lane.get("frozen") == Some(&json!(true))).collect();
        (lanes, clip.get("freezeFrame").cloned())
    };
    for (fields, expected) in [
        (json!({"mark":"all"}), (vec![true, true, true], Some(json!(1)))),
        (json!({"mark":"listed","parameters":[17]}), (vec![true, true, false], Some(json!(1)))),
        (json!({"mark":"unlisted","parameters":[17]}), (vec![false, false, true], Some(json!(1)))),
        (json!({"mark":"none","frame":3}), (vec![false, false, false], Some(json!(3)))),
        (json!({"mark":"all","frame":null}), (vec![true, true, true], None)),
        (json!({"frame":2}), (vec![true, true, true], Some(json!(2)))),
        (json!({"mark":"none"}), (vec![false, false, false], None)),
    ] {
        let mut call = json!({"op":"animation.freeze.set","name":"Idle"});
        call.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        result(&request(&mut session, call));
        assert_eq!(flags(&session), expected, "{fields}");
    }
}

#[test]
fn malformed_freeze_requests_and_fields_are_refused() {
    let mut session = shy_session();
    let opened = session.snapshot();
    for (bad, message) in [
        (json!({"op":"animation.freeze.set","name":"Idle"}), "animation.freeze.set requires mark or frame"),
        (json!({"op":"animation.freeze.set","name":"Idle","mark":"unkeyed"}), "mark must be all, listed, unlisted or none"),
        (json!({"op":"animation.freeze.set","name":"Idle","mark":true}), "mark must be all, listed, unlisted or none"),
        (json!({"op":"animation.freeze.set","name":"Idle","frame":12}), "frame must be between 0 and 11"),
        (json!({"op":"animation.freeze.set","name":"Idle","frame":1.5}), "frame must be an integer"),
        (json!({"op":"animation.freeze.set","name":"Idle","mark":"listed"}),
            "parameters is required with mark listed or unlisted, and only with them"),
        (json!({"op":"animation.freeze.set","name":"Idle","frame":1,"parameters":[18]}),
            "parameters is required with mark listed or unlisted, and only with them"),
        (json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0,"frozen":1}), "frozen must be a boolean"),
        (json!({"op":"animation.lane.set","name":"Idle","parameter":17,"axis":0}),
            "animation.lane.set requires interpolation, merge_mode, shy or frozen"),
    ] {
        assert_eq!(request(&mut session, bad.clone())["message"], message, "{bad}");
        assert_eq!(session.snapshot(), opened);
    }
    for (fields, message) in [
        (json!({"freezeFrame":12}), "animation.freezeFrame must be a frame inside the animation"),
        (json!({"freezeFrame":"1"}), "animation.freezeFrame must be a frame inside the animation"),
        (json!({"lanes":[{"uuid":17,"target":0,"frozen":"yes","keyframes":[]}]}), "animation.lanes[0].frozen must be a boolean"),
    ] {
        let mut model = model();
        model["animations"]["Idle"].as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        let mut session = session_of(model);
        let reply = request(&mut session, json!({"op":"animation.set","name":"Idle","length":12}));
        assert_eq!(reply["message"], message, "{fields}");
    }
    let mut model = model();
    model["animations"]["Idle"]["freezeFrame"] = json!(5);
    let mut session = session_of(model);
    let reply = request(&mut session, json!({"op":"animation.set","name":"Idle","length":4}));
    assert_eq!(reply["message"], "animation.freezeFrame must be a frame inside the animation", "a length that cuts the frame");
}

#[test]
fn a_frozen_clip_can_be_renamed_and_deleted_and_its_parameter_deleted() {
    let mut session = shy_session();
    result(&request(&mut session, json!({"op":"animation.freeze.set","name":"Idle","mark":"all","frame":2})));
    result(&request(&mut session, json!({"op":"animation.rename","name":"Idle","new_name":"Rest"})));
    let edit = json!({"op":"animation.key.set","name":"Rest","parameter":18,"axis":0,"frame":5,"value":0.0});
    assert_eq!(request(&mut session, edit)["message"],
        "the lane of parameter 18 axis 0 in animation \"Rest\" is frozen; turn freeze off first", "the freeze moves with the name");
    result(&request(&mut session, json!({"op":"parameter.delete","id":18})));
    assert_eq!(session.snapshot()["model"]["animations"]["Rest"]["lanes"].as_array().unwrap().len(), 2);
    result(&request(&mut session, json!({"op":"animation.delete","name":"Rest"})));
}
