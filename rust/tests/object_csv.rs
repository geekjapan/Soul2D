use serde_json::{json, Value};
use soul2d_core::Session;
use std::{env, fs};

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn node(uuid: u64, name: &str, kind: &str, children: Value) -> Value {
    json!({
        "uuid": uuid, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": children,
    })
}

fn plain(id: u64, name: &str) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false,
        "min": [-1.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive", "bindings": [],
    })
}

/// Root 1 with Part 10 around Node 11, and Nodes 12 and 13 whose names need quotes and a guard. Parameters 20
/// and 21, and group 30 around parameter 31.
fn fixture() -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", json!([
            node(10, "Hair", "Part", json!([node(11, "Arm", "Node", json!([]))])),
            node(12, "Sleeve, \"left\"\nlower", "Node", json!([])),
            node(13, "=SUM(A1)", "Node", json!([])),
        ])),
        "param": [
            plain(20, "Angle X"),
            {"groupUUID": 30, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [plain(31, "'Eye")]},
            plain(21, "Mouth"),
        ],
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

const EXPORTED: &str = "uuid,type,name\n1,Node,Root\n10,Part,Hair\n11,Node,Arm\n\
    12,Node,\"Sleeve, \"\"left\"\"\nlower\"\n13,Node,'=SUM(A1)\n20,Parameter,Angle X\n30,Parameter,Face\n\
    31,Parameter,''Eye\n21,Parameter,Mouth\n";

fn history_count(session: &mut Session) -> u64 {
    let response = request(session, json!({"op": "history.summary"}));
    result(&response)["count"].as_u64().unwrap()
}

fn name_of(session: &Session, id: u64) -> String {
    fn find(item: &Value, id: u64) -> Option<String> {
        if item["uuid"].as_u64().or(item["groupUUID"].as_u64()) == Some(id) {
            return item["name"].as_str().map(str::to_owned);
        }
        item["children"].as_array().into_iter().flatten().find_map(|child| find(child, id))
    }
    let model = &session.snapshot()["model"];
    let parameter = || model["param"].as_array().unwrap().iter().find_map(|item| find(item, id));
    find(&model["nodes"], id).or_else(parameter).unwrap()
}

/// The refusal of an import, which leaves the document and its history as they were.
fn refused(session: &mut Session, arguments: Value) -> String {
    let before = session.snapshot();
    let count = history_count(session);
    let mut call = json!({"op": "object.import_csv"});
    call.as_object_mut().unwrap().extend(arguments.as_object().unwrap().clone());
    let response = request(session, call);
    assert_eq!(response["succeeded"], false, "expected a refusal: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
    response["message"].as_str().unwrap().to_owned()
}

fn temp(name: &str) -> String {
    let directory = env::temp_dir().join(format!("soul2d-object-csv-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    directory.join(name).to_string_lossy().into_owned()
}

#[test]
fn the_export_lists_every_object_and_the_resource_holds_the_same_text() {
    let mut session = fixture();
    let read = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": {"uri": "resource://soul2d/object/names-csv"}}))
        .unwrap();
    let content: Value = serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(content, json!({"csv": EXPORTED, "rows": 9}));

    let path = temp("names.csv");
    let _ = fs::remove_file(&path);
    let before = session.snapshot();
    let call = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "object_export_csv", "arguments": {"path": path}}}))
        .unwrap();
    assert_eq!(call["result"]["isError"], false, "{call}");
    let answer = &call["result"]["structuredContent"];
    assert_eq!((answer["rows"].as_u64(), answer["bytes"].as_u64()), (Some(9), Some(EXPORTED.len() as u64)));
    assert_eq!(fs::read_to_string(answer["path"].as_str().unwrap()).unwrap(), EXPORTED);
    assert_eq!(session.snapshot(), before, "an export changes nothing");

    // A file that exists is kept unless the call says otherwise, and only a .csv is written.
    let again = request(&mut session, json!({"op": "object.export_csv", "path": path}));
    assert_eq!(again["message"], "export destination already exists");
    result(&request(&mut session, json!({"op": "object.export_csv", "path": path, "overwrite": true})));
    let other = request(&mut session, json!({"op": "object.export_csv", "path": temp("names.txt")}));
    assert_eq!(other["message"], "path must name a .csv file");
    fs::remove_file(&path).unwrap();

    // The open document is not written over, whatever its file is called.
    let document = temp("document.csv");
    result(&request(&mut session, json!({"op": "save", "path": document})));
    let over = request(&mut session, json!({"op": "object.export_csv", "path": document, "overwrite": true}));
    assert_eq!(over["message"], "object.export_csv would overwrite the open document");
    fs::remove_file(&document).unwrap();
}

#[test]
fn every_name_comes_back_from_the_text_as_it_was() {
    // What a cell quotes, what a spreadsheet takes for a formula, and spaces and a carriage return at the ends.
    let names = [
        (10, "\ttab"), (11, " both ends "), (12, "a \"quote\", a comma\r\nand a line"), (13, "\rreturn and tail\r"),
        (20, "=1+1"), (21, "+plus, more"), (30, "-minus\t=3"), (31, "@at;=2"),
    ];
    let mut written = fixture();
    for (id, name) in names {
        let op = if id < 20 { "node.rename" } else { "parameter.rename" };
        result(&request(&mut written, json!({"op": op, "id": id, "name": name})));
    }
    let path = temp("round.csv");
    result(&request(&mut written, json!({"op": "object.export_csv", "path": path, "overwrite": true})));
    // Each start of a formula stands after an apostrophe, inside the quotes when the cell has them. A tab and a
    // semicolon are quoted, so that a reader that splits on them finds no cell that starts with `=`.
    let text = "uuid,type,name\n1,Node,Root\n10,Part,\"'\ttab\"\n11,Node, both ends \n\
        12,Node,\"a \"\"quote\"\", a comma\r\nand a line\"\n13,Node,\"'\rreturn and tail\r\"\n20,Parameter,'=1+1\n\
        30,Parameter,\"'-minus\t=3\"\n31,Parameter,\"'@at;=2\"\n21,Parameter,\"'+plus, more\"\n";
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    let mut read = fixture();
    let answer = request(&mut read, json!({"op": "object.import_csv", "path": path}));
    fs::remove_file(&path).unwrap();
    assert_eq!(result(&answer)["renamed"].as_array().unwrap().len(), 8);
    assert_eq!(names.map(|(id, _)| name_of(&read, id)), names.map(|(_, name)| name.to_owned()));
    assert_eq!(read.snapshot()["model"], written.snapshot()["model"]);
}

#[test]
fn a_type_that_a_spreadsheet_would_run_is_guarded_like_a_name() {
    // A document of somebody else may give a node any type.
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", json!([node(10, "Hair", "=1+1", json!([]))])),
        "param": [],
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    let path = temp("types.csv");
    result(&request(&mut session, json!({"op": "object.export_csv", "path": path, "overwrite": true})));
    let text = fs::read_to_string(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(text, "uuid,type,name\n1,Node,Root\n10,'=1+1,Hair\n");
    // The row names the type as it was written, and another type is refused.
    let same = request(&mut session, json!({"op": "object.import_csv", "csv": text}));
    assert_eq!(result(&same)["unchanged"], json!([1, 10]));
    let other = "uuid,type,name\n10,=1+1+1,Hair\n";
    assert_eq!(refused(&mut session, json!({"csv": other})), "row 2: 10 is not of the type the row says");
}

#[test]
fn an_edited_file_renames_nodes_parameters_and_groups_in_one_undo_step() {
    let mut session = fixture();
    // The text as it was written renames nothing and adds no undo step.
    let count = history_count(&mut session);
    let same = request(&mut session, json!({"op": "object.import_csv", "csv": EXPORTED}));
    assert_eq!(result(&same)["applied"], false);
    assert_eq!(result(&same)["renamed"], json!([]));
    assert_eq!(result(&same)["unchanged"], json!([1, 10, 11, 12, 13, 20, 30, 31, 21]));
    assert_eq!(history_count(&mut session), count);

    // A spreadsheet's byte order mark and line ends. The two parameters swap their names, the group and a node
    // get new ones, a quoted cell holds a comma and a quote, and an apostrophe before a minus sign is taken off.
    let edited = "\u{feff}uuid,type,name\r\n20,Parameter,Mouth\r\n21,Parameter,Angle X\r\n\r\n\
        30,Parameter,\"Head, \"\"upper\"\"\"\r\n11,Node,'-arm\r\n10,Part,Hair";
    let dry = request(&mut session, json!({"op": "object.import_csv", "csv": edited, "dryRun": true}));
    assert_eq!(result(&dry)["applied"], false);
    let planned = result(&dry)["renamed"].clone();
    assert_eq!(name_of(&session, 20), "Angle X", "a dry run renames nothing");
    assert_eq!(history_count(&mut session), count);

    let path = temp("edited.csv");
    fs::write(&path, edited).unwrap();
    let call = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "object_import_csv", "arguments": {"path": path}}}))
        .unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(call["result"]["isError"], false, "{call}");
    let answer = &call["result"]["structuredContent"];
    assert_eq!(answer["applied"], true);
    assert_eq!(answer["unchanged"], json!([10]));
    assert_eq!(answer["renamed"], planned, "a dry run answers what the import does");
    assert_eq!(answer["renamed"], json!([
        {"kind": "parameter", "uuid": 20, "from": "Angle X", "to": "Mouth"},
        {"kind": "parameter", "uuid": 21, "from": "Mouth", "to": "Angle X"},
        {"kind": "parameter", "uuid": 30, "from": "Face", "to": "Head, \"upper\""},
        {"kind": "node", "uuid": 11, "from": "Arm", "to": "-arm"},
    ]));
    let names = [20, 21, 30, 11, 12].map(|id| name_of(&session, id));
    assert_eq!(names, ["Mouth", "Angle X", "Head, \"upper\"", "-arm", "Sleeve, \"left\"\nlower"]);
    assert_eq!(history_count(&mut session), count + 1);

    // The new name of the node is written after an apostrophe again.
    let path = temp("renamed.csv");
    result(&request(&mut session, json!({"op": "object.export_csv", "path": path, "overwrite": true})));
    assert!(fs::read_to_string(&path).unwrap().contains("\n11,Node,'-arm\n"));
    fs::remove_file(&path).unwrap();

    // The first text brings the four names back as a second step, and an undo takes one step.
    let back = request(&mut session, json!({"op": "object.import_csv", "csv": EXPORTED}));
    assert_eq!(result(&back)["renamed"].as_array().unwrap().len(), 4);
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(name_of(&session, 11), "-arm");
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!([20, 21, 30, 11].map(|id| name_of(&session, id)), ["Angle X", "Mouth", "Face", "Arm"]);
}

#[test]
fn a_file_that_is_not_a_list_of_the_objects_is_refused_whole() {
    let mut session = fixture();
    let cases: [(&str, &str); 15] = [
        ("uuid,name\n20,Mouth\n", "the first row of the CSV must be uuid,type,name"),
        ("", "the first row of the CSV must be uuid,type,name"),
        ("uuid,type,name\n20,Parameter\n", "row 2 has 2 fields, and 3 are needed"),
        ("uuid,type,name\n20,Parameter,A,B\n", "row 2 has more than 3 fields"),
        ("uuid,type,name,note\n20,Parameter,A\n", "row 1 has more than 3 fields"),
        ("uuid,type,name\n20,Parameter,Jaw\n0,Parameter,Jaw\n", "row 3: the uuid must be a nonzero u32"),
        ("uuid,type,name\n4294967296,Parameter,Jaw\n", "row 2: the uuid must be a nonzero u32"),
        ("uuid,type,name\n99,Parameter,Jaw\n", "row 2: 99 names no node, parameter or parameter group"),
        ("uuid,type,name\n10,Node,Hair\n", "row 2: 10 is not of the type the row says"),
        ("uuid,type,name\n20,Parameter,Jaw\n20,Parameter,Chin\n", "row 3: 20 has a row already"),
        // A name another object keeps, an empty name, and the root, which no call renames.
        ("uuid,type,name\n20,Parameter,Mouth\n", "parameter 20 cannot be named \"Mouth\": Name already taken"),
        ("uuid,type,name\n20,Parameter,\n", "parameter 20 cannot be named \"\": the name would be empty"),
        ("uuid,type,name\n1,Node,Top\n",
            "node 1 cannot be named \"Top\": the root node cannot be renamed in Rust Core mode"),
        ("uuid,type,name\n20,Parameter,\"Jaw\n", "a quoted field does not end"),
        ("uuid,type,name\n20,Parameter,\"Jaw\"s\n", "row 2 has text after the closing quote of a field"),
    ];
    for (csv, message) in cases {
        assert_eq!(refused(&mut session, json!({"csv": csv})), message, "{csv:?}");
    }
    let stray = "uuid,type,name\n20,Parameter,Ja\"w\n";
    assert_eq!(refused(&mut session, json!({"csv": stray})), "row 2 has a quote inside a field that is not quoted");

    // The limits: 4096 rows after the header, 4096 characters of a name, 1 MiB of text.
    let rows = "20,Parameter,Jaw\n".repeat(4097);
    let many = format!("uuid,type,name\n{rows}");
    assert_eq!(refused(&mut session, json!({"csv": many})), "the CSV has more than 4096 rows after its header");
    // 4096 rows are read, and these are refused for what they hold.
    let most = format!("uuid,type,name\n{}", "20,Parameter,Jaw\n".repeat(4096));
    assert_eq!(refused(&mut session, json!({"csv": most})), "row 3: 20 has a row already");
    let long = format!("uuid,type,name\n20,Parameter,{}\n", "n".repeat(4097));
    let message = "parameter 20 would get a name longer than 4096 characters";
    assert_eq!(refused(&mut session, json!({"csv": long})), message);
    let large = format!("uuid,type,name\n20,Parameter,{}\n", "n".repeat(1024 * 1024));
    assert_eq!(refused(&mut session, json!({"csv": large})), "the CSV text is larger than 1048576 bytes");
    let path = temp("large.csv");
    fs::write(&path, &large).unwrap();
    assert_eq!(refused(&mut session, json!({"path": path})), "the CSV text is larger than 1048576 bytes");
    // Shift_JIS, which Cubism Editor can write, is not read.
    fs::write(&path, b"uuid,type,name\n20,Parameter,\x83\x7d\n").unwrap();
    assert_eq!(refused(&mut session, json!({"path": path})), "the CSV file is not UTF-8");
    fs::remove_file(&path).unwrap();

    assert_eq!(refused(&mut session, json!({})), "give the CSV as exactly one of path and csv");
    let both = json!({"path": path, "csv": EXPORTED});
    assert_eq!(refused(&mut session, both), "give the CSV as exactly one of path and csv");
    assert!(refused(&mut session, json!({"path": path})).starts_with("could not open the CSV file: "));
}
