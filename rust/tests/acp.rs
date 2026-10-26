use serde_json::{json, Value};
use soul2d_core::acp::{
    is_permission_method, merge_json, parse_command_line, response_matches, Client, Conversation, FrameDecoder,
    Incoming, LaunchSpec, McpHttp, PermissionIntake, PermissionRequest, PromptOutcome, RoleKind, SendMode, MAX_FRAME_BYTES,
    STDERR_RING_BYTES,
};
use soul2d_core::settings::Settings;
use std::io::{pipe, BufRead, BufReader, Cursor, PipeWriter, Write};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn prompt_success_requires_the_matched_explicit_uncancelled_end_turn() {
    for (reason, cancel, expected) in [
        (Some("end_turn"), false, PromptOutcome::Completed),
        (Some("end_turn"), true, PromptOutcome::Cancelled),
        (Some("cancelled"), false, PromptOutcome::Cancelled),
        (Some("max_tokens"), false, PromptOutcome::Stopped(Some("max_tokens".into()))),
        (None, false, PromptOutcome::Stopped(None)),
    ] {
        let (mut client, mut agent) = ready(true);
        let id = client.send_prompt("lifecycle correlation").unwrap();
        let prompt = agent.recv();
        if cancel { client.cancel().unwrap(); agent.recv(); }
        agent.send(json!({"jsonrpc":"2.0", "id":"wrong", "result":{"stopReason":"end_turn"}}));
        poll_until(&mut client, |item| matches!(item, Incoming::Other(_)));
        assert!(client.prompt_completion().is_none());
        let boundary = Instant::now();
        let result = reason.map(|reason| json!({"stopReason":reason})).unwrap_or_else(|| json!({}));
        agent.send(json!({"jsonrpc":"2.0", "id":prompt["id"], "result":result}));
        poll_until(&mut client, |item| matches!(item, Incoming::PromptFinished(_)));
        let done = client.prompt_completion().unwrap();
        assert_eq!(done.id, id);
        assert_eq!(done.outcome, expected);
        assert!(done.received_at > boundary);
        let next = client.send_prompt("new prompt").unwrap();
        assert_ne!(next, id);
        assert!(client.prompt_completion().is_none());
    }
}

#[test]
fn permission_intake_history_survives_completion_and_ignored_requests() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("permission history").unwrap();
    let prompt = agent.recv();
    let epoch = client.permission_epoch();
    let mut permission = permission_request(json!(1), "Late request");
    permission.as_object_mut().unwrap().remove("id");
    agent.send(permission);
    agent.send(permission_request(json!(2), "Late request"));
    agent.send(permission_request(json!(2), "Duplicate"));
    agent.send(json!({"jsonrpc":"2.0", "id":prompt["id"], "result":{"stopReason":"end_turn"}}));
    poll_until(&mut client, |item| matches!(item, Incoming::PromptFinished(_)));
    assert_eq!(client.permission_epoch(), epoch + 3);
    assert!(client.conversation.permissions.is_empty());
}

#[test]
fn malformed_or_errored_prompt_results_never_supply_successful_completion() {
    for case in ["missing_version", "wrong_version", "error_and_result", "missing_reason"] {
        let (mut client, mut agent) = ready(true);
        client.send_prompt("malformed completion").unwrap();
        let prompt = agent.recv();
        let mut response = json!({"jsonrpc":"2.0", "id":prompt["id"], "result":{"stopReason":"end_turn"}});
        match case {
            "missing_version" => { response.as_object_mut().unwrap().remove("jsonrpc"); },
            "wrong_version" => response["jsonrpc"] = json!("1.0"),
            "error_and_result" => response["error"] = json!({"code":-32000,"message":"failure"}),
            _ => response["result"] = json!({}),
        }
        agent.send(response);
        poll_until(&mut client, |item| matches!(item, Incoming::PromptFinished(_)));
        assert_ne!(client.prompt_completion().unwrap().outcome, PromptOutcome::Completed, "{case}");
    }
}

// ---------------------------------------------------------------------------------------------
// Harness: an in-memory agent on the far end of two pipes.

struct Agent {
    to_client: Option<PipeWriter>,
    lines: Receiver<String>,
}

impl Agent {
    fn send(&mut self, message: Value) {
        self.send_raw(&format!("{message}\n"));
    }

    fn send_raw(&mut self, text: &str) {
        let writer = self.to_client.as_mut().expect("agent already hung up");
        writer.write_all(text.as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    fn hang_up(&mut self) {
        self.to_client = None;
    }

    /// Next raw line the client wrote.
    fn line(&self) -> String {
        self.lines.recv_timeout(WAIT).expect("client wrote nothing")
    }

    fn recv(&self) -> Value {
        serde_json::from_str(&self.line()).unwrap()
    }

    fn silent(&self) -> bool {
        self.lines.recv_timeout(Duration::from_millis(150)).is_err()
    }
}

fn connect() -> (Client<PipeWriter>, Agent) {
    let (agent_stdin, client_writer) = pipe().unwrap();
    let (client_reader, agent_stdout) = pipe().unwrap();
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(agent_stdin).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (Client::new(client_reader, client_writer), Agent { to_client: Some(agent_stdout), lines })
}

/// Poll until `done` accepts an item; returns everything collected, in order.
fn poll_until<W: Write>(client: &mut Client<W>, done: impl Fn(&Incoming) -> bool) -> Vec<Incoming> {
    let deadline = Instant::now() + WAIT;
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(client.poll_wait(Duration::from_millis(50)));
        if all.iter().any(&done) {
            return all;
        }
    }
    panic!("timed out; collected {all:?}");
}

fn init_result(id: &Value, http: bool) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {
        "protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": http}}}})
}

fn initialize(client: &mut Client<PipeWriter>, agent: &mut Agent, http: bool) {
    client.initialize().unwrap();
    let request = agent.recv();
    agent.send(init_result(&request["id"], http));
    assert_eq!(poll_until(client, |i| matches!(i, Incoming::Initialized)), vec![Incoming::Initialized]);
}

const SESSION: &str = "sess-\"\\";

/// Initialized client with a session; the three setup requests have been consumed from the agent.
fn ready(http: bool) -> (Client<PipeWriter>, Agent) {
    let (mut client, mut agent) = connect();
    initialize(&mut client, &mut agent, http);
    client.new_session().unwrap();
    let request = agent.recv();
    agent.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"sessionId": SESSION}}));
    poll_until(&mut client, |i| matches!(i, Incoming::SessionReady(_)));
    (client, agent)
}

fn update(kind: &str, text: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": SESSION,
        "update": {"sessionUpdate": kind, "content": {"type": "text", "text": text}}}})
}

fn permission_request(id: Value, title: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission", "params": {
        "sessionId": SESSION, "reason": title,
        "toolCall": {"toolCallId": format!("fixture-{id}"), "title": title, "kind": "edit", "rawInput": {}},
        "options": [{"optionId": "allow:once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject:once", "name": "Reject once", "kind": "reject_once"}]}})
}

fn permissions(items: &[Incoming]) -> Vec<PermissionRequest> {
    items.iter().filter_map(|i| if let Incoming::Permission(p) = i { Some(p.clone()) } else { None }).collect()
}

// ---------------------------------------------------------------------------------------------
// initialize

#[test]
fn initialize_request_is_literal_and_response_marks_the_client_ready() {
    let (mut client, mut agent) = connect();
    assert!(!client.is_initialized());
    client.initialize().unwrap();
    assert_eq!(
        agent.line(),
        r#"{"id":0,"jsonrpc":"2.0","method":"initialize","params":{"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"clientInfo":{"name":"soul2d","version":"0.0.0"},"protocolVersion":1}}"#
    );
    agent.send(init_result(&json!(0), true));
    assert_eq!(poll_until(&mut client, |i| matches!(i, Incoming::Initialized)), vec![Incoming::Initialized]);
    assert!(client.is_initialized());
    assert!(client.agent_supports_mcp_http());
    assert_eq!(client.init_result().unwrap()["protocolVersion"], json!(1));
}

#[test]
fn content_length_send_mode_frames_requests() {
    let (mut client, agent) = connect();
    client.set_send_mode(SendMode::ContentLength);
    client.ping().unwrap();
    let body = r#"{"id":0,"jsonrpc":"2.0","method":"ping","params":{}}"#;
    // The harness reads with `lines()`, which also strips the `\r`.
    assert_eq!(agent.line(), format!("Content-Length: {}", body.len()));
    assert_eq!(agent.line(), "");
    drop(client); // the unterminated body is only yielded at end of input
    assert_eq!(agent.line(), body);
}

#[test]
fn messages_before_the_initialize_response_stay_in_order() {
    let (mut client, mut agent) = connect();
    client.initialize().unwrap();
    agent.recv();
    agent.send(update("agent_message_chunk", "queued-before-init"));
    agent.send(json!({"jsonrpc": "2.0", "id": 1000, "result": {"protocolVersion": 1, "wrongRequest": true}}));
    agent.send(init_result(&json!(0), true));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Initialized));
    assert_eq!(items.len(), 3);
    assert!(matches!(&items[0], Incoming::Update(u) if u["content"]["text"] == "queued-before-init"));
    assert_eq!(
        items[1],
        Incoming::Other(json!({"jsonrpc": "2.0", "id": 1000, "result": {"protocolVersion": 1, "wrongRequest": true}}))
    );
    assert_eq!(items[2], Incoming::Initialized);
}

#[test]
fn bad_initialize_responses_fail_and_a_reinitialize_clears_stale_state() {
    let (mut client, mut agent) = connect();
    let cases = [
        (
            json!({"protocolVersion": 99, "agentCapabilities": {"mcpCapabilities": {"http": true}}}),
            "initialize protocolVersion mismatch: expected 1, got 99",
        ),
        (json!({"agentCapabilities": {}}), "initialize protocolVersion mismatch: expected 1, got missing"),
        (json!({"protocolVersion": "1"}), r#"initialize protocolVersion mismatch: expected 1, got "1""#),
    ];
    for (result, expected) in cases {
        client.initialize().unwrap();
        let id = agent.recv()["id"].clone();
        agent.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let items = poll_until(&mut client, |i| matches!(i, Incoming::InitFailed(_)));
        assert_eq!(items, vec![Incoming::InitFailed(expected.to_string())]);
        assert!(!client.is_initialized());
        assert!(!client.agent_supports_mcp_http());
        assert_eq!(client.init_error(), Some(expected));
        assert!(client.send_prompt("x").unwrap_err().contains("not initialized"));
    }

    // A JSON-RPC error wins even when the same message also has a valid result.
    client.initialize().unwrap();
    let id = agent.recv()["id"].clone();
    agent.send(json!({"jsonrpc": "2.0", "id": id,
        "error": {"code": -32001, "message": "fixture initialize failure"},
        "result": {"protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": true}}}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::InitFailed(_)));
    assert_eq!(items, vec![Incoming::InitFailed("initialize error: fixture initialize failure (code -32001)".into())]);

    client.initialize().unwrap();
    let id = agent.recv()["id"].clone();
    agent.send(json!({"jsonrpc": "2.0", "id": id, "result": 5}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::InitFailed(_)));
    assert_eq!(items, vec![Incoming::InitFailed("initialize response missing result".into())]);

    // HTTP capability from one successful initialize does not leak into the next.
    initialize(&mut client, &mut agent, true);
    assert!(client.agent_supports_mcp_http());
    initialize(&mut client, &mut agent, false);
    assert!(!client.agent_supports_mcp_http());
    assert!(client.is_initialized());
}

#[test]
fn prompts_and_sessions_are_refused_before_initialize() {
    let (mut client, agent) = connect();
    assert_eq!(client.send_prompt("hello").unwrap_err(), "ACP agent is not initialized");
    assert_eq!(client.new_session().unwrap_err(), "ACP agent is not initialized");
    assert!(agent.silent());
}

// ---------------------------------------------------------------------------------------------
// session/new

fn expected_cwd() -> String {
    std::env::current_dir().unwrap().to_string_lossy().into_owned()
}

#[test]
fn session_new_carries_cwd_and_an_empty_server_list_without_http_support() {
    let (mut client, mut agent) = connect();
    client.set_mcp_http(Some(McpHttp {
        name: "soul2d".into(),
        url: "http://127.0.0.1:8088/mcp".into(),
        headers: json!([]),
    }));
    initialize(&mut client, &mut agent, false);
    client.new_session().unwrap();
    assert_eq!(
        agent.recv(),
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new",
               "params": {"cwd": expected_cwd(), "mcpServers": []}})
    );
}

#[test]
fn session_new_advertises_the_mcp_server_when_the_agent_supports_http() {
    let (mut client, mut agent) = connect();
    client.set_mcp_http(Some(McpHttp {
        name: "soul2d".into(),
        url: "http://127.0.0.1:8088/mcp".into(),
        headers: json!([{"name": "Authorization", "value": "Bearer t"}]),
    }));
    initialize(&mut client, &mut agent, true);
    client.new_session().unwrap();
    assert_eq!(
        agent.recv(),
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": {"cwd": expected_cwd(),
            "mcpServers": [{"type": "http", "name": "soul2d", "url": "http://127.0.0.1:8088/mcp",
                            "headers": [{"name": "Authorization", "value": "Bearer t"}]}]}})
    );
    agent.send(json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": SESSION}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::SessionReady(_)));
    assert_eq!(items, vec![Incoming::SessionReady(SESSION.to_string())]);
    assert_eq!(client.session_id(), Some(SESSION));
}

#[test]
fn session_new_failures_are_reported() {
    let (mut client, mut agent) = connect();
    initialize(&mut client, &mut agent, true);
    client.new_session().unwrap();
    agent.recv();
    agent.send(json!({"jsonrpc": "2.0", "id": 1,
        "error": {"code": -32002, "message": "fixture session/new rejected"}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::SessionFailed(_)));
    assert_eq!(items, vec![Incoming::SessionFailed("session/new error: fixture session/new rejected (code -32002)".into())]);

    client.new_session().unwrap();
    agent.recv();
    agent.send(json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": 5}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::SessionFailed(_)));
    assert_eq!(items, vec![Incoming::SessionFailed("session/new missing sessionId".into())]);
    assert_eq!(client.session_id(), None);
}

// ---------------------------------------------------------------------------------------------
// prompt

#[test]
fn prompt_request_and_streamed_updates_round_trip() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("hello \"x\"\nline two").unwrap();
    let line = agent.line();
    assert!(line.contains(r#""sessionId":"sess-\"\\""#), "session id must be escaped: {line}");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": {
            "sessionId": SESSION, "prompt": [{"type": "text", "text": "hello \"x\"\nline two"}]}})
    );
    assert!(client.prompt_active());
    assert_eq!(client.send_prompt("again").unwrap_err(), "a prompt is already active");

    agent.send(update("agent_message_chunk", "Hi "));
    agent.send(update("agent_message_chunk", "there\n"));
    agent.send(json!({"jsonrpc": "2.0", "id": 1002, "result": {"stopReason": "wrong-request"}}));
    // A permission request and a string id with the prompt's number must not complete the prompt.
    agent.send(permission_request(json!(2), "Same id as the prompt"));
    agent.send(json!({"jsonrpc": "2.0", "id": "2", "result": {}}));
    agent.send(json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::PromptFinished(_)));
    assert_eq!(items.len(), 6);
    assert!(matches!(&items[2], Incoming::Other(m) if m["id"] == 1002));
    assert!(matches!(&items[3], Incoming::Permission(p) if p.id == json!(2)));
    assert!(matches!(&items[4], Incoming::Other(m) if m["id"] == "2"));
    assert!(matches!(&items[5], Incoming::PromptFinished(done) if done.outcome == PromptOutcome::Completed));
    assert!(!client.prompt_active());

    let blocks = &client.conversation.blocks;
    assert_eq!((blocks[0].role.as_str(), blocks[0].kind), ("You", RoleKind::You));
    assert_eq!(blocks[0].lines, ["hello \"x\"", "line two"]);
    assert_eq!((blocks[1].role.as_str(), blocks[1].kind), ("Assistant", RoleKind::AssistantFinal));
    assert_eq!(blocks[1].lines, ["Hi there"]);
    // The turn ended, so the permission queue is empty again.
    assert!(client.conversation.permissions.is_empty());
}

#[test]
fn a_prompt_without_a_session_creates_one_first() {
    let (mut client, mut agent) = connect();
    initialize(&mut client, &mut agent, false);
    client.send_prompt("first").unwrap();
    let request = agent.recv();
    assert_eq!(request["method"], "session/new");
    assert!(agent.silent(), "the prompt must wait for the session");
    agent.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"sessionId": "s9"}}));
    poll_until(&mut client, |i| matches!(i, Incoming::SessionReady(_)));
    assert_eq!(
        agent.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt",
               "params": {"sessionId": "s9", "prompt": [{"type": "text", "text": "first"}]}})
    );
    assert!(client.prompt_active());
}

#[test]
fn ping_reports_success_and_errors() {
    let (mut client, mut agent) = ready(false);
    client.ping().unwrap();
    assert_eq!(agent.recv(), json!({"jsonrpc": "2.0", "id": 2, "method": "ping", "params": {}}));
    agent.send(json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
    assert_eq!(poll_until(&mut client, |i| matches!(i, Incoming::Ping(_))), vec![Incoming::Ping(Ok(()))]);
    client.ping().unwrap();
    agent.recv();
    agent.send(json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32601, "message": "nope"}}));
    assert_eq!(
        poll_until(&mut client, |i| matches!(i, Incoming::Ping(_))),
        vec![Incoming::Ping(Err("ping error: nope (code -32601)".into()))]
    );
}

#[test]
fn response_matching_requires_type_and_shape() {
    let ok = json!({"jsonrpc": "2.0", "id": 7, "result": {}});
    assert!(response_matches(&ok, &json!(7)));
    assert!(!response_matches(&json!({"jsonrpc": "2.0", "id": 8, "result": {}}), &json!(7)));
    assert!(!response_matches(&json!({"jsonrpc": "2.0", "id": "7", "result": {}}), &json!(7)));
    assert!(!response_matches(
        &json!({"jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": {}}),
        &json!(7)
    ));
    assert!(response_matches(&json!({"id": 7, "error": {"message": "x"}}), &json!(7)));
    assert!(!response_matches(&json!({"id": 7}), &json!(7)));
}

// ---------------------------------------------------------------------------------------------
// permissions

#[test]
fn permission_requests_are_queued_and_answered_allow_and_deny() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("go").unwrap();
    agent.recv();
    let odd_id = json!("pending:1:\"\\権限");
    agent.send(permission_request(json!(41), "Allow the fixture edit"));
    agent.send(permission_request(odd_id.clone(), "Second"));
    agent.send(permission_request(json!(41), "Duplicate while outstanding"));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Permission(p) if p.id == odd_id));
    let queued = permissions(&items);
    assert_eq!(queued.len(), 2, "a duplicate outstanding id is ignored");
    assert_eq!((queued[0].id.clone(), queued[0].generation, queued[0].reason.as_str()), (json!(41), 1, "Allow the fixture edit"));
    assert_eq!((queued[1].id.clone(), queued[1].generation), (odd_id.clone(), 2));
    assert_eq!(client.conversation.permissions, queued);

    // The tool call of the request is mirrored into the log as a pending tool block.
    let tool = client.conversation.blocks.iter().find(|b| b.tool_id == "fixture-41").unwrap();
    assert_eq!((tool.role.as_str(), tool.kind, tool.status.as_str()), ("Tool#fixture-41", RoleKind::Tool, "pending"));
    assert_eq!(tool.lines, ["[fixture-41] Allow the fixture edit tool update (status: pending)"]);

    assert!(client.answer_permission(&json!(41), 1, Some("allow:once")).unwrap());
    assert_eq!(
        agent.line(),
        r#"{"id":41,"jsonrpc":"2.0","result":{"outcome":{"optionId":"allow:once","outcome":"selected"}}}"#
    );
    assert!(client.answer_permission(&odd_id, 2, None).unwrap());
    let denied = agent.recv();
    assert_eq!(denied, json!({"jsonrpc": "2.0", "id": odd_id, "result": {"outcome": {"outcome": "cancelled"}}}));
    // Empty option id is a denial too, but the request is already gone: stale and unknown are no-ops.
    assert!(!client.answer_permission(&json!(41), 1, Some("allow:once")).unwrap());
    assert!(!client.answer_permission(&json!(41), 99, Some("allow:once")).unwrap());
    assert!(agent.silent());
    assert!(client.conversation.permissions.is_empty());
}

#[test]
fn an_empty_option_id_denies() {
    let mut convo = Conversation::default();
    convo.apply_permission_request(&permission_request(json!(5), "Edit"));
    assert_eq!(
        convo.answer(&json!(5), 1, Some("")),
        Some(json!({"jsonrpc": "2.0", "id": 5, "result": {"outcome": {"outcome": "cancelled"}}}))
    );
}

#[test]
fn legacy_method_names_and_params_ids_are_accepted() {
    for method in ["session/request_permission", "request_permission", "request_permissions", "request_prermissions"] {
        assert!(is_permission_method(method), "{method}");
    }
    assert!(!is_permission_method("session/update"));

    let mut convo = Conversation::default();
    let intake = convo.apply_permission_request(&json!({
        "jsonrpc": "2.0", "method": "request_prermissions",
        "params": {"requestId": "r-1", "reason": "legacy"}}));
    assert_eq!(
        intake,
        PermissionIntake::Queued(PermissionRequest {
            id: json!("r-1"),
            reason: "legacy".into(),
            params: json!({"requestId": "r-1", "reason": "legacy"}),
            generation: 1
        })
    );
    // No id anywhere: shown to the user but cannot be answered.
    let intake = convo.apply_permission_request(&json!({"jsonrpc": "2.0", "method": "request_permission",
        "params": {"reason": "anonymous"}}));
    assert!(matches!(&intake, PermissionIntake::Queued(p) if p.id.is_null() && p.generation == 0));
    assert_eq!(convo.answer(&Value::Null, 0, Some("allow:once")), None);
    assert_eq!(convo.permissions.len(), 2);
}

#[test]
fn cancel_sends_session_cancel_then_cancels_every_permission() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("go").unwrap();
    agent.recv();
    agent.send(permission_request(json!(41), "First"));
    agent.send(permission_request(json!("p2"), "Second"));
    poll_until(&mut client, |i| matches!(i, Incoming::Permission(p) if p.id == json!("p2")));

    client.cancel().unwrap();
    assert_eq!(
        agent.line(),
        r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"sess-\"\\"}}"#
    );
    let cancelled = |id: Value| json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": {"outcome": "cancelled"}}});
    assert_eq!(agent.recv(), cancelled(json!(41)));
    assert_eq!(agent.recv(), cancelled(json!("p2")));
    assert!(client.conversation.permissions.is_empty());
    assert!(client.conversation.cancelling());

    // A second cancel is a no-op, a late selection is dropped, and a late request is cancelled on arrival.
    client.cancel().unwrap();
    assert!(!client.answer_permission(&json!(41), 1, Some("allow:once")).unwrap());
    agent.send(permission_request(json!("late"), "Late"));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::PermissionCancelled(_)));
    assert_eq!(items, vec![Incoming::PermissionCancelled(json!("late"))]);
    assert_eq!(agent.recv(), cancelled(json!("late")));
    assert!(agent.silent());
    assert!(client.conversation.permissions.is_empty());

    // The cancelled turn ends; the next turn may reuse an id and gets a newer generation.
    agent.send(json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "cancelled"}}));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::PromptFinished(_)));
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Incoming::PromptFinished(done) if done.outcome == PromptOutcome::Cancelled));
    assert!(!client.conversation.cancelling());
    client.send_prompt("again").unwrap();
    agent.recv();
    agent.send(permission_request(json!(41), "Reused"));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Permission(_)));
    assert_eq!(permissions(&items)[0].generation, 4);
}

#[test]
fn a_selection_made_before_cancel_is_sent_before_the_cancel() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("go").unwrap();
    agent.recv();
    agent.send(permission_request(json!(41), "First"));
    poll_until(&mut client, |i| matches!(i, Incoming::Permission(_)));
    assert!(client.answer_permission(&json!(41), 1, Some("allow:once")).unwrap());
    client.cancel().unwrap();
    assert_eq!(agent.recv()["result"]["outcome"]["outcome"], "selected");
    assert_eq!(agent.recv()["method"], "session/cancel");
    assert!(agent.silent());
}

#[test]
fn cancel_without_an_active_prompt_sends_nothing() {
    let (mut client, agent) = ready(true);
    // `ready` consumed initialize and session/new above, so the pipe holds nothing else.
    client.cancel().unwrap();
    assert!(agent.silent());
}

// ---------------------------------------------------------------------------------------------
// framing

#[test]
fn malformed_frames_are_reported_and_the_stream_continues() {
    let (mut client, mut agent) = connect();
    agent.send_raw("{not json\n");
    agent.send_raw("[1,2]\n");
    agent.send_raw("\n\r\n");
    agent.send_raw("\"a string\"\n");
    agent.send_raw(&format!("{}\u{0}trailing garbage\n", json!({"jsonrpc": "2.0", "id": 9, "result": {}})));
    agent.send_raw(&format!("{}\r\n", update("agent_message_chunk", "after")));
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Update(_)));
    assert_eq!(items.len(), 5);
    assert!(matches!(&items[0], Incoming::Malformed(m) if m.starts_with("JSON parse failed: ")));
    assert_eq!(items[1], Incoming::Malformed("JSON-RPC message is not an object".into()));
    assert_eq!(items[2], Incoming::Malformed("JSON-RPC message is not an object".into()));
    assert_eq!(items[3], Incoming::Other(json!({"jsonrpc": "2.0", "id": 9, "result": {}})));
    assert!(matches!(&items[4], Incoming::Update(u) if u["content"]["text"] == "after"));
}

#[test]
fn an_oversized_line_is_rejected_without_losing_the_next_frame() {
    let (mut client, mut agent) = connect();
    let writer = agent.to_client.take().unwrap();
    // Written from a thread: the pipe is only 64 KiB, so the reader must keep draining it.
    let sender = std::thread::spawn(move || {
        let mut writer = writer;
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..(MAX_FRAME_BYTES / chunk.len() + 2) {
            writer.write_all(&chunk).unwrap();
        }
        writer.write_all(b"\n").unwrap();
        writer.write_all(format!("{}\n", json!({"id": 3, "result": {}})).as_bytes()).unwrap();
        writer
    });
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Other(_)));
    assert_eq!(items, vec![
        Incoming::Malformed(format!("frame exceeds {MAX_FRAME_BYTES} bytes")),
        Incoming::Other(json!({"id": 3, "result": {}})),
    ]);
    drop(sender.join().unwrap());
}

fn decode_all(decoder: &mut FrameDecoder, pieces: &[&[u8]]) -> Vec<Result<Value, String>> {
    pieces.iter().flat_map(|piece| decoder.push(piece)).collect()
}

#[test]
fn decoder_skips_an_oversized_line_across_pushes() {
    let mut decoder = FrameDecoder::new(16);
    let items = decode_all(&mut decoder, &[b"{\"a\":\"xxxxxxxxxxxxxxxxxxxxxxxx", b"yyyyyyyy", b"zz\"}\n{\"b\":1}\n"]);
    assert_eq!(items, vec![Err("frame exceeds 16 bytes".to_string()), Ok(json!({"b": 1}))]);
    // A line that is too long but arrives complete.
    let items = decoder.push(b"{\"a\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n{\"c\":2}\n");
    assert_eq!(items, vec![Err("frame exceeds 16 bytes".to_string()), Ok(json!({"c": 2}))]);
}

#[test]
fn decoder_reads_content_length_frames_even_one_byte_at_a_time() {
    let mut decoder = FrameDecoder::new(1024);
    let wire = b"Content-Length: 7\r\n\r\n{\"a\":1}\ncontent-length:9\n\n{\"b\":\"x\"}\n{\"c\":3}\n";
    let mut items = Vec::new();
    for byte in wire {
        items.extend(decoder.push(&[*byte]));
    }
    assert_eq!(items, vec![Ok(json!({"a": 1})), Ok(json!({"b": "x"})), Ok(json!({"c": 3}))]);
}

#[test]
fn decoder_rejects_bad_and_oversized_content_length_without_buffering_the_body() {
    let mut decoder = FrameDecoder::new(64);
    let items = decoder.push(b"Content-Length: abc\r\n\r\n{\"ok\":1}\n");
    assert_eq!(items, vec![Err("invalid Content-Length: abc".to_string()), Ok(json!({"ok": 1}))]);

    // Declared 100 bytes, cap 64: the body is dropped as it arrives, then framing resumes.
    let mut wire = b"Content-Length: 100\r\n\r\n".to_vec();
    wire.extend([b'x'; 100]);
    wire.extend(b"{\"after\":1}\n");
    let items = decode_all(&mut decoder, &[&wire[..30], &wire[30..90], &wire[90..]]);
    assert_eq!(items, vec![Err("frame of 100 bytes exceeds 64 bytes".to_string()), Ok(json!({"after": 1}))]);

    // An absurd length is refused, not allocated.
    let items = decoder.push(b"Content-Length: 99999999999999999999\r\n\r\n");
    assert_eq!(items, vec![Err("invalid Content-Length: 99999999999999999999".to_string())]);
    let items = decoder.push(b"Content-Length: 99999999999\r\n\r\n");
    assert_eq!(items, vec![Err("frame of 99999999999 bytes exceeds 64 bytes".to_string())]);
}

// ---------------------------------------------------------------------------------------------
// agent exit and I/O failures

#[test]
fn agent_exit_before_the_initialize_response_fails_the_initialize() {
    let (mut client, mut agent) = connect();
    client.initialize().unwrap();
    agent.recv();
    agent.hang_up();
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Disconnected(_)));
    assert_eq!(items, vec![
        Incoming::InitFailed("Agent stream closed before initialize response".into()),
        Incoming::Disconnected("end of stream".into()),
    ]);
    assert!(!client.is_initialized());
    assert_eq!(client.init_error(), Some("Agent stream closed before initialize response"));
}

#[test]
fn agent_exit_mid_prompt_completes_the_prompt_with_an_error_and_clears_permissions() {
    let (mut client, mut agent) = ready(true);
    client.send_prompt("go").unwrap();
    agent.recv();
    agent.send(permission_request(json!(41), "Pending"));
    poll_until(&mut client, |i| matches!(i, Incoming::Permission(_)));
    agent.hang_up();
    let items = poll_until(&mut client, |i| matches!(i, Incoming::Disconnected(_)));
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], Incoming::PromptFinished(done) if done.outcome == PromptOutcome::Failed("Agent stream closed before prompt response".into())));
    assert_eq!(items[1], Incoming::Disconnected("end of stream".into()));
    assert!(!client.prompt_active());
    assert!(client.conversation.permissions.is_empty());
    assert!(!client.is_initialized());
    assert_eq!(client.send_prompt("again").unwrap_err(), "ACP agent is not initialized");
}

struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe gone"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wait_for_stderr<W: Write>(client: &Client<W>, needle: &str) {
    let deadline = Instant::now() + WAIT;
    while !client.stderr_text().contains(needle) {
        assert!(Instant::now() < deadline, "stderr never contained {needle:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn send_failures_report_the_captured_stderr() {
    let mut client = Client::new(Cursor::new(Vec::new()), BrokenPipe);
    client.attach_stderr(Cursor::new(b"agent crashed: boom\n".to_vec()));
    wait_for_stderr(&client, "boom");
    let error = client.initialize().unwrap_err();
    assert_eq!(error, "send failed: pipe gone | stderr: agent crashed: boom\n");
    assert_eq!(client.init_error(), Some(error.as_str()));
}

#[test]
fn stderr_keeps_only_the_newest_bytes() {
    let mut client = Client::new(Cursor::new(Vec::new()), Vec::<u8>::new());
    let mut data = vec![b'x'; STDERR_RING_BYTES * 3];
    data.extend(b"<END>");
    client.attach_stderr(Cursor::new(data));
    let deadline = Instant::now() + WAIT;
    while !client.stderr_text().ends_with("<END>") {
        assert!(Instant::now() < deadline, "stderr never reached the end marker");
        std::thread::sleep(Duration::from_millis(5));
    }
    let text = client.stderr_text();
    assert_eq!(text.len(), STDERR_RING_BYTES);
    assert!(text.starts_with("xxxx") && text.ends_with("x<END>"));
}

#[test]
fn oversized_outgoing_messages_are_refused() {
    let (mut client, agent) = ready(true);
    let huge = "a".repeat(MAX_FRAME_BYTES + 1);
    let error = client.send_prompt(&huge).unwrap_err();
    assert!(error.contains("exceeds"), "{error}");
    assert!(!client.prompt_active());
    assert!(agent.silent());
    // The client stays usable.
    client.send_prompt("fine").unwrap();
    assert_eq!(agent.recv()["params"]["prompt"][0]["text"], "fine");
}

#[test]
fn shutdown_is_idempotent_and_closes_the_connection() {
    let (mut client, agent) = ready(true);
    assert_eq!(client.shutdown(), Ok(None), "no child process is owned");
    assert_eq!(client.shutdown(), Ok(None));
    assert_eq!(client.ping().unwrap_err(), "send failed: connection closed");
    assert!(client.poll().is_empty());
    drop(agent);
}

// ---------------------------------------------------------------------------------------------
// settings

#[test]
fn command_lines_split_into_argument_vectors() {
    assert_eq!(parse_command_line(""), Vec::<String>::new());
    assert_eq!(
        parse_command_line(r#"./agent --flag  "two words" 'x "y"' a"b c"d '' "#),
        ["./agent", "--flag", "two words", "x \"y\"", "ab cd"]
    );
    assert_eq!(parse_command_line("node\tagent.js"), ["node", "agent.js"]);
}

#[test]
fn launch_spec_and_mcp_config_come_from_settings() {
    let mut settings = Settings::empty();
    // No agent ships with Soul2D, so an unset command is an error that names the setting.
    assert_eq!(LaunchSpec::from_settings(&settings).unwrap_err(),
        "ACP.Command is not set; enter the command that starts an ACP agent");
    settings.set("ACP.Command", "node 'my agent.js' --acp".to_string()).unwrap();
    settings.set("ACP.Workingdir", "/old/dir".to_string()).unwrap();
    let spec = LaunchSpec::from_settings(&settings).unwrap();
    assert_eq!(spec.command, ["node", "my agent.js", "--acp"]);
    assert_eq!(spec.workdir, Some("/old/dir".into()));
    settings.set("ACP.Workdir", "/new/dir".to_string()).unwrap();
    assert_eq!(LaunchSpec::from_settings(&settings).unwrap().workdir, Some("/new/dir".into()));
    settings.set("ACP.Command", "   ".to_string()).unwrap();
    assert_eq!(LaunchSpec::from_settings(&settings).unwrap_err(),
        "ACP.Command is not set; enter the command that starts an ACP agent");

    let mut settings = Settings::empty();
    let url = "http://127.0.0.1:8088/mcp";
    assert_eq!(
        McpHttp::from_settings(&settings, url).unwrap(),
        McpHttp { name: "soul2d".into(), url: url.into(), headers: json!([]) }
    );
    settings.set("MCP.Name", "editor".to_string()).unwrap();
    settings.set("MCP.Headers", r#"[{"name":"X","value":"1"}]"#.to_string()).unwrap();
    let mcp = McpHttp::from_settings(&settings, url).unwrap();
    assert_eq!((mcp.name.as_str(), mcp.headers.clone()), ("editor", json!([{"name": "X", "value": "1"}])));
    settings.set("MCP.Headers", r#"{"not":"an array"}"#.to_string()).unwrap();
    assert_eq!(McpHttp::from_settings(&settings, url).unwrap().headers, json!([]));
    settings.set("MCP.Headers", "garbage".to_string()).unwrap();
    assert_eq!(McpHttp::from_settings(&settings, url).unwrap().headers, json!([]));
}

// ---------------------------------------------------------------------------------------------
// conversation reducer

fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

#[test]
fn streamed_chunks_join_lines_until_a_newline_or_a_role_change() {
    let mut convo = Conversation::default();
    convo.apply_update(&chunk("agent_message_chunk", "Hel"));
    convo.apply_update(&chunk("agent_message_chunk", "lo\nwor"));
    convo.apply_update(&chunk("agent_message_chunk", "ld\n"));
    assert_eq!(convo.blocks[0].lines, ["Hello", "world"]);
    // The trailing newline makes the next chunk start a fresh line, until the stream is flushed.
    convo.apply_update(&chunk("agent_message_chunk", "next"));
    assert_eq!(convo.blocks[0].lines, ["Hello", "world", "next"]);
    convo.flush_pending();
    convo.apply_update(&chunk("agent_message_chunk", "more"));
    assert_eq!(convo.blocks[0].lines, ["Hello", "world", "nextmore"]);

    convo.apply_update(&chunk("agent_thought_chunk", "hmm"));
    assert_eq!(convo.blocks.len(), 2);
    assert_eq!(
        (convo.blocks[1].role.as_str(), convo.blocks[1].kind, convo.blocks[1].lines.clone()),
        ("Assistant (thinking)", RoleKind::AssistantThinking, vec!["hmm".to_string()])
    );
    // Japanese text and a trailing newline keep the role until flushed.
    convo.apply_update(&chunk("agent_message_chunk", "日本語の応答\n"));
    assert_eq!(convo.blocks[2].kind, RoleKind::AssistantFinal);
    assert_eq!(convo.blocks[2].lines, ["日本語の応答"]);
}

#[test]
fn unknown_updates_fall_back_to_their_raw_text() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "available_commands_update", "availableCommands": []}));
    assert_eq!(convo.blocks[0].role, "Command list");
    assert_eq!(convo.blocks[0].lines, [r#"{"availableCommands":[],"sessionUpdate":"available_commands_update"}"#]);
    convo.apply_update(&json!({"sessionUpdate": "error", "message": "it broke"}));
    assert_eq!((convo.blocks[1].kind, convo.blocks[1].lines.clone()), (RoleKind::Error, vec!["it broke".to_string()]));
    convo.apply_update(&json!({"sessionUpdate": "agent_message_chunk", "content": [
        {"type": "text", "text": "a", "annotations": [{"title": "T"}]}, {"type": "text", "text": "b"}]}));
    assert_eq!(convo.blocks[2].lines, ["a (T)b"]);
}

#[test]
fn tool_calls_update_in_place_and_keep_their_merged_payload() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Inspect",
        "status": "completed", "rawOutput": {"answer": "VISIBLE_RESULT"}}));
    let block = &convo.blocks[0];
    assert_eq!((block.role.as_str(), block.kind, block.status.as_str()), ("Tool#t1", RoleKind::Tool, "completed"));
    assert_eq!(block.lines, ["[t1] Inspect tool update (status: completed)"]);
    assert_eq!(block.payload_merged.as_ref().unwrap()["rawOutput"]["answer"], "VISIBLE_RESULT");

    convo.apply_update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "failed",
        "content": {"type": "text", "text": "!"}, "rawOutput": "second"}));
    let block = &convo.blocks[0];
    assert_eq!(block.lines, ["[t1] Inspect tool update (status: completed)[t1] ! (status: failed)"]);
    assert_eq!((block.status.as_str(), block.title.as_str()), ("failed", "Inspect"));
    assert_eq!(block.payload_merged.as_ref().unwrap()["rawOutput"], "second");
    assert_eq!(block.payload_merged.as_ref().unwrap()["content"]["text"], "!");

    // A permission placeholder is replaced by the later tool_call rather than shown twice.
    convo.apply_permission_request(&permission_request(json!(1), "Edit"));
    assert_eq!(convo.blocks.len(), 2);
    convo.apply_update(&json!({"sessionUpdate": "tool_call", "toolCallId": "fixture-1", "title": "Edit",
        "status": "in_progress"}));
    assert_eq!(convo.blocks.len(), 2);
    assert_eq!(convo.blocks[1].lines, ["[fixture-1] Edit tool update (status: in_progress)"]);
}

#[test]
fn tool_updates_without_an_id_use_the_latest_block_of_the_same_role() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "tool_call", "title": "A"}));
    convo.apply_update(&chunk("agent_message_chunk", "between"));
    convo.apply_update(&json!({"sessionUpdate": "tool_call_update", "content": {"text": "+"}}));
    assert_eq!(convo.blocks.len(), 2);
    assert_eq!(convo.blocks[0].lines, ["A tool update+"]);
    assert_eq!(convo.blocks[0].role, "Tool");
}

#[test]
fn plans_replace_entries_but_drop_invalid_ones() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "plan", "entries": [
        {"content": "Inspect puppet", "priority": "high", "status": "pending"},
        {"content": "Edit", "priority": "medium", "status": "in_progress"},
        {"content": "bad priority", "priority": "urgent", "status": "pending"},
        {"content": "bad status", "priority": "low", "status": "done"},
        {"content": "no status", "priority": "low"}]}));
    assert_eq!(convo.plan.as_ref().unwrap().lines, ["[high] Inspect puppet (pending)", "[medium] Edit (in_progress)"]);
    convo.apply_update(&json!({"sessionUpdate": "plan", "status": "in_progress", "entries": [
        {"content": "Save", "priority": "low", "status": "completed"}]}));
    let plan = convo.plan.as_ref().unwrap();
    assert_eq!(plan.lines, ["[low] Save (completed)"]);
    assert_eq!(plan.payload.as_ref().unwrap()["entries"].as_array().unwrap().len(), 1);
    assert_eq!(plan.status, "in_progress");
    convo.apply_update(&json!({"sessionUpdate": "plan", "entries": []}));
    let plan = convo.plan.as_ref().unwrap();
    assert!(plan.lines.is_empty() && plan.payload.as_ref().unwrap()["entries"].as_array().unwrap().is_empty());
    // Plans never enter the chat log; a plan with only text splits it into lines.
    convo.apply_update(&chunk("plan", "step one\nstep two"));
    assert_eq!(convo.plan.as_ref().unwrap().lines, ["step one", "step two"]);
    assert!(convo.blocks.is_empty());
}

#[test]
fn usage_updates_summarize_known_token_fields() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "usage_update",
        "usage": {"inputTokens": 10, "output_tokens": 5, "total_tokens": "15", "cacheReadInputTokens": 2}}));
    assert_eq!(convo.usage_text, "in 10, out 5, total 15, cached 2");
    convo.apply_update(&json!({"sessionUpdate": "usage_update", "prompt_tokens": 7}));
    assert_eq!(convo.usage_text, "in 7");
    convo.apply_update(&json!({"sessionUpdate": "usage_update", "used": 3}));
    assert_eq!(convo.usage_text, r#"{"sessionUpdate":"usage_update","used":3}"#);
    assert!(convo.blocks.is_empty());
}

#[test]
fn the_log_is_capped_and_tool_indices_survive_eviction() {
    let mut convo = Conversation::default();
    convo.apply_update(&json!({"sessionUpdate": "tool_call", "toolCallId": "old", "title": "Old",
        "rawOutput": "old result"}));
    convo.apply_update(&json!({"sessionUpdate": "tool_call", "toolCallId": "survivor", "title": "Survivor",
        "rawOutput": "survivor result"}));
    let lines = vec!["line"; 3999].join("\n");
    convo.apply_update(&chunk("error", &lines));
    let total: usize = convo.blocks.iter().map(|b| b.lines.len()).sum();
    assert_eq!(total, 4000);
    assert_eq!(convo.blocks[0].tool_id, "survivor");

    convo.apply_update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "survivor",
        "status": "completed", "rawOutput": "updated survivor"}));
    convo.apply_update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "old",
        "status": "completed", "rawOutput": "fresh old"}));
    assert_eq!(convo.blocks[0].payload_merged.as_ref().unwrap()["rawOutput"], "updated survivor");
    let last = convo.blocks.last().unwrap();
    assert_eq!((last.tool_id.as_str(), last.payload_merged.as_ref().unwrap()["rawOutput"].clone()), ("old", json!("fresh old")));
}

#[test]
fn partial_eviction_keeps_the_newest_lines_of_the_oldest_block() {
    let mut convo = Conversation::default();
    convo.apply_update(&chunk("agent_message_chunk", "first\nsecond"));
    let lines = vec!["x"; 3999].join("\n");
    convo.apply_update(&chunk("error", &lines));
    assert_eq!(convo.blocks[0].lines, ["second"]);
}

#[test]
fn merge_json_follows_the_tool_payload_rules() {
    assert_eq!(
        merge_json(&json!({"contents": [1], "k": 1}), &json!({"contents": [2, 3], "k": 2})),
        json!({"contents": [1, 2, 3], "k": 2})
    );
    assert_eq!(merge_json(&json!({"contents": [1]}), &json!({"contents": 2})), json!({"contents": [1, 2]}));
    assert_eq!(merge_json(&json!({"contents": 1}), &json!({"contents": [2]})), json!({"contents": [1, 2]}));
    assert_eq!(merge_json(&json!({"contents": 1}), &json!({"contents": 2})), json!({"contents": [1, 2]}));
    assert_eq!(merge_json(&json!({}), &json!({"contents": 2})), json!({"contents": 2}));
    assert_eq!(
        merge_json(&json!({"content": {"text": "ab", "type": "text"}}), &json!({"content": {"text": "cd"}})),
        json!({"content": {"text": "abcd", "type": "text"}})
    );
    assert_eq!(
        merge_json(&json!({"content": {"type": "image"}}), &json!({"content": {"text": "cd"}})),
        json!({"content": [{"type": "image"}, {"text": "cd"}]})
    );
    assert_eq!(merge_json(&json!({"content": [1]}), &json!({"content": 2})), json!({"content": [1, 2]}));
    assert_eq!(merge_json(&json!("old"), &json!({"a": 1})), json!({"a": 1}));
    assert_eq!(merge_json(&json!({"a": 1}), &json!(5)), json!(5));
}

#[test]
fn clear_starts_a_fresh_conversation() {
    let mut convo = Conversation::default();
    convo.record_user("hi");
    convo.apply_permission_request(&permission_request(json!(1), "Edit"));
    convo.cancel();
    convo.clear();
    assert!(convo.blocks.is_empty() && convo.permissions.is_empty() && !convo.cancelling());
    let intake = convo.apply_permission_request(&permission_request(json!(1), "Edit"));
    assert!(matches!(intake, PermissionIntake::Queued(p) if p.generation == 1));
}

// ---------------------------------------------------------------------------------------------
// real child processes

#[cfg(unix)]
mod child {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("soul2d-acp-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The pid no longer exists at all (a zombie would still answer `kill -0`).
    fn reaped(pid: u32) -> bool {
        !Command::new("kill").arg("-0").arg(pid.to_string()).output().unwrap().status.success()
    }

    const STUB: &str = r##"
echo "stub started in $(pwd)" >&2
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true}}}}' ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s1"}}' ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working\n"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"s1","reason":"Edit","options":[{"optionId":"allow:once","name":"Allow once","kind":"allow_once"}]}}'
      IFS= read -r answer
      case "$answer" in *'"optionId":"allow:once"'*) verdict=allowed ;; *) verdict=other ;; esac
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"got: '"$verdict"'"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}' ;;
  esac
done
"##;

    fn sh(args: &[&str], workdir: Option<PathBuf>) -> LaunchSpec {
        LaunchSpec { command: args.iter().map(|a| a.to_string()).collect(), workdir }
    }

    #[test]
    fn stub_agent_round_trip_then_clean_shutdown_leaves_no_zombie() {
        let dir = TempDir::new("roundtrip");
        let script = dir.0.join("agent.sh");
        std::fs::write(&script, STUB).unwrap();
        let mut client = Client::spawn(&sh(&["/bin/sh", script.to_str().unwrap()], Some(dir.0.clone()))).unwrap();
        let pid = client.pid().unwrap();

        client.initialize().unwrap();
        let items = poll_until(&mut client, |i| matches!(i, Incoming::Initialized));
        assert_eq!(items, vec![Incoming::Initialized]);
        assert!(client.agent_supports_mcp_http());

        client.send_prompt("hi").unwrap();
        let items = poll_until(&mut client, |i| matches!(i, Incoming::Permission(_)));
        let request = permissions(&items).remove(0);
        assert_eq!((request.id.clone(), request.generation, request.reason.as_str()), (json!(7), 1, "Edit"));
        assert!(client.answer_permission(&request.id, request.generation, Some("allow:once")).unwrap());
        poll_until(&mut client, |i| matches!(i, Incoming::PromptFinished(_)));

        let blocks = &client.conversation.blocks;
        assert_eq!(blocks.len(), 2);
        assert_eq!((blocks[0].role.as_str(), blocks[0].lines.clone()), ("You", vec!["hi".to_string()]));
        assert_eq!(blocks[1].lines, ["working", "got: allowed"]);
        assert!(!client.prompt_active());

        wait_for_stderr(&client, "stub started in");
        assert!(client.stderr_text().contains("soul2d-acp-roundtrip"), "{}", client.stderr_text());

        let status = client.shutdown().expect("shutdown confirmed").expect("owned child");
        assert!(status.success(), "{status:?}");
        assert!(reaped(pid), "child {pid} is still a process (zombie?)");
        assert_eq!(client.shutdown(), Ok(None));
    }

    #[test]
    fn shutdown_kills_an_agent_that_ignores_stdin_closing() {
        let mut client = Client::spawn(&sh(&["/bin/sh", "-c", "exec sleep 300"], None)).unwrap();
        let pid = client.pid().unwrap();
        let started = Instant::now();
        let status = client.shutdown().expect("shutdown confirmed").expect("owned child");
        assert!(!status.success());
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(reaped(pid), "child {pid} is still a process (zombie?)");
    }

    #[test]
    fn dropping_the_client_reaps_the_child() {
        let client = Client::spawn(&sh(&["/bin/sh", "-c", "exec sleep 300"], None)).unwrap();
        let pid = client.pid().unwrap();
        drop(client);
        assert!(reaped(pid));
    }

    #[test]
    fn an_agent_that_exits_on_its_own_is_reported_with_its_stderr_and_status() {
        let script = "IFS= read -r line; echo oops >&2; exit 3";
        let mut client = Client::spawn(&sh(&["/bin/sh", "-c", script], None)).unwrap();
        client.initialize().unwrap();
        let items = poll_until(&mut client, |i| matches!(i, Incoming::Disconnected(_)));
        assert!(matches!(&items[0], Incoming::InitFailed(m)
            if m.starts_with("Agent stream closed before initialize response")));
        assert_eq!(items[1], Incoming::Disconnected("end of stream".into()));
        wait_for_stderr(&client, "oops");
        let status = client.shutdown().unwrap().unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn spawn_rejects_missing_programs_and_empty_commands() {
        let error = Client::spawn(&sh(&["/nonexistent/soul2d-agent"], None)).err().unwrap();
        assert!(error.starts_with("spawn failed: "), "{error}");
        assert_eq!(Client::spawn(&sh(&[], None)).err().unwrap(), "Command is empty");
    }
}

// ---------------------------------------------------------------------------------------------
// tools/acp-claude.py: the bridge to the claude CLI, driven as a real process with a stand-in claude

/// Drives `tools/acp-claude.py` through `Client::spawn` with a shell script in place of `claude` that records its
/// arguments and input and answers one user turn with text, a tool call, its result and a `result` event.
#[cfg(unix)]
#[test]
fn the_claude_bridge_offers_http_mcp_and_streams_one_turn_of_the_cli() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let dir = std::env::temp_dir().join(format!("acp-claude-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("claude");
    std::fs::write(&stub, concat!(
        "#!/bin/sh\n",
        "printf '%s\\n' \"$@\" > \"$STUB_DIR/args.txt\"\n",
        "printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[],\"mcp_servers\":[]}'\n",
        "while IFS= read -r line; do\n",
        "  printf '%s\\n' \"$line\" >> \"$STUB_DIR/input.txt\"\n",
        "  printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hello \"},",
        "{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"mcp__soul2d__nodes_list\",\"input\":{}}]}}'\n",
        "  printf '%s\\n' '{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_1\",",
        "\"content\":[{\"type\":\"text\",\"text\":\"{\\\"nodes\\\":[]}\"}]}]}}'\n",
        "  printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}'\n",
        "  printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"stop_reason\":\"end_turn\",\"result\":\"done\"}'\n",
        "done\n",
    ))
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = LaunchSpec {
        command: vec![
            "env".to_string(),
            format!("ACP_CLAUDE_BIN={}", stub.display()),
            format!("ACP_CLAUDE_LOG={}", dir.join("updates.jsonl").display()),
            format!("STUB_DIR={}", dir.display()),
            "python3".to_string(),
            "-I".to_string(),
            root.join("tools/acp-claude.py").display().to_string(),
        ],
        workdir: Some(dir.clone()),
    };
    let mut client = Client::spawn(&spec).unwrap();
    client.initialize().unwrap();
    poll_until(&mut client, |i| matches!(i, Incoming::Initialized));
    assert!(client.agent_supports_mcp_http(), "the bridge advertises HTTP MCP: {:?}", client.init_result());
    client.set_mcp_http(Some(McpHttp {
        name: "soul2d".to_string(),
        url: "http://127.0.0.1:1/mcp".to_string(),
        headers: json!([{"name": "Authorization", "value": "Bearer t"}]),
    }));
    client.send_prompt_with("hi", Some("briefing")).unwrap();
    let items = poll_until(&mut client, |i| matches!(i, Incoming::PromptFinished(_)));
    assert!(
        matches!(items.last(), Some(Incoming::PromptFinished(done)) if done.outcome == PromptOutcome::Completed),
        "{items:?}"
    );
    let text: String = client.conversation.blocks.iter().map(|b| format!("{} {}\n", b.title, b.lines.join("\n"))).collect();
    for needle in ["hello", "done", "mcp__soul2d__nodes_list"] {
        assert!(text.contains(needle), "{needle:?} is missing from the log: {text}");
    }
    // The CLI runs with the offered server as its only MCP configuration and nothing else: no built-in tools but the
    // two that read that server's resources, no user settings, hooks, skills or saved session, and only that
    // server's tools and resources allowed.
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    for needle in [
        "--strict-mcp-config\n",
        "--tools\nListMcpResourcesTool,ReadMcpResourceTool\n",
        "--setting-sources\n\n",
        "--disable-slash-commands\n",
        "--no-session-persistence\n",
        "\"headers\": {\"Authorization\": \"Bearer t\"}",
        "\"url\": \"http://127.0.0.1:1/mcp\"",
        "--allowedTools\nmcp__soul2d__* ListMcpResourcesTool ReadMcpResourceTool\n",
        "--max-budget-usd\n",
    ] {
        assert!(args.contains(needle), "{needle:?} is missing from the claude arguments: {args}");
    }
    // The user's text and the host's briefing reach the CLI as one user turn.
    assert_eq!(
        std::fs::read_to_string(dir.join("input.txt")).unwrap().trim(),
        r#"{"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "hi\n\nbriefing"}]}}"#
    );
    // ACP_CLAUDE_LOG holds every update the client received and ends with the turn's stopReason.
    let log: Vec<Value> = std::fs::read_to_string(dir.join("updates.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        log.iter().any(|entry| entry["update"]["sessionUpdate"] == "tool_call"
            && entry["update"]["title"] == "mcp__soul2d__nodes_list"),
        "{log:?}"
    );
    assert_eq!(log.last(), Some(&json!({"stopReason": "end_turn"})), "{log:?}");
    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}
