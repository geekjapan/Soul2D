use serde_json::{json, Value};
use soul2d_core::mcp_http::{handle_connection_deferred, handle_connection_within, serve_one, HttpDispatch, MAX_BODY_BYTES, MAX_HEAD_BYTES};
use soul2d_core::njc::run;
use soul2d_core::Session;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};

/// Serve exactly `connections` connections with a fresh session, then hand back its final snapshot (Session is not Send).
fn start(bearer: Option<&str>, connections: usize) -> (u16, JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bearer = bearer.map(String::from);
    let handle = thread::spawn(move || {
        let mut session = Session::new();
        for _ in 0..connections {
            serve_one(&listener, &mut session, bearer.as_deref()).unwrap();
        }
        session.snapshot()
    });
    (port, handle)
}

fn njc(port: u16, bearer: &str, command: &str) -> Result<Value, String> {
    let mut args = vec!["--port".to_string(), port.to_string()];
    args.extend(command.split(' ').map(String::from));
    run(&args, "", bearer).map(|output| serde_json::from_str(&output).unwrap())
}

/// Send raw bytes, half-close, and return everything the server answers.
fn raw(port: u16, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut data = Vec::new();
    stream.read_to_end(&mut data).unwrap();
    String::from_utf8(data).unwrap()
}

fn post_with(headers: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn post(body: &str) -> Vec<u8> {
    post_with("", body)
}

/// Split a response into (status line, body); asserts the framing is a closed, length-delimited reply.
fn split(response: &str) -> (&str, &str) {
    let (head, body) = response.split_once("\r\n\r\n").expect("response has a head");
    let status = head.lines().next().unwrap();
    assert!(head.contains("\r\nConnection: close"), "{head}");
    if status.contains(" 204 ") {
        assert!(!head.contains("Content-Length"), "{head}");
    } else {
        assert!(head.contains(&format!("\r\nContent-Length: {}", body.len())), "{head}");
        assert!(head.contains("\r\nContent-Type: application/json"), "{head}");
    }
    (status, body)
}

#[test]
fn a_deferred_reply_keeps_the_original_id_and_completes_only_when_resumed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let request = json!({"jsonrpc":"2.0", "id":"original", "method":"tools/call", "params":{"name":"pending"}});
    let bytes = post_with("Authorization: Bearer secret\r\n", &request.to_string());
    let client = thread::spawn(move || raw(port, &bytes));
    let (stream, _) = listener.accept().unwrap();
    let pending = handle_connection_deferred(stream, Some("secret"), std::time::Duration::from_millis(500), &mut |message| {
        assert_eq!(message, &request);
        HttpDispatch::Defer
    }).unwrap().expect("the authenticated call is retained");
    assert!(!client.is_finished(), "no premature tool error or HTTP reply");
    let mut executed = 0;
    pending.finish(&mut |message| {
        executed += 1;
        soul2d_core::mcp_http::RpcReply::plain(Some(json!({"jsonrpc":"2.0", "id":message["id"], "result":{"approved":true}})))
    }).unwrap();
    let response = client.join().unwrap();
    let (status, body) = split(&response);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(serde_json::from_str::<Value>(body).unwrap(), json!({"jsonrpc":"2.0", "id":"original", "result":{"approved":true}}));
    assert_eq!(executed, 1);
}

#[test]
fn deferred_transport_keeps_auth_parse_notification_and_panic_behavior() {
    for (body, token, expected, panic) in [
        (r#"{"jsonrpc":"2.0","method":"tools/call"}"#, "secret", "204 No Content", false),
        (r#"{"jsonrpc":"2.0","id":null,"method":"tools/call"}"#, "secret", "204 No Content", false),
        ("{broken", "secret", "200 OK", false),
        (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, "wrong", "401 Unauthorized", false),
        (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, "secret", "500 Internal Server Error", true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bytes = post_with(&format!("Authorization: Bearer {token}\r\n"), body);
        let client = thread::spawn(move || raw(port, &bytes));
        let (stream, _) = listener.accept().unwrap();
        let mut calls = 0;
        let pending = handle_connection_deferred(stream, Some("secret"), std::time::Duration::from_millis(500), &mut |_| {
            calls += 1;
            assert!(!panic, "handler panic stays a transport error");
            HttpDispatch::Defer
        });
        assert!(pending.unwrap().is_none());
        let response = client.join().unwrap();
        assert_eq!(split(&response).0, format!("HTTP/1.1 {expected}"));
        assert_eq!(calls, usize::from(token == "secret" && body != "{broken"));
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let client = thread::spawn(move || raw(port, &post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)));
    let (stream, _) = listener.accept().unwrap();
    let pending = handle_connection_deferred(stream, None, std::time::Duration::from_millis(500), &mut |_| HttpDispatch::Defer).unwrap().unwrap();
    pending.finish(&mut |_| panic!("completion panic stays a transport error")).unwrap();
    assert_eq!(split(&client.join().unwrap()).0, "HTTP/1.1 500 Internal Server Error");
}

#[test]
fn tools_list_returns_the_engine_tools() {
    let (port, server) = start(None, 2);
    let response = njc(port, "", "tools list").unwrap();
    server.join().unwrap();
    let expected = Session::new()
        .handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .unwrap();
    assert_eq!(response, expected);
    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "ParamCommand_Add1DParameter"));
}

#[test]
fn a_mutation_in_one_request_is_visible_in_a_later_request() {
    let (port, server) = start(None, 4);
    let created = njc(
        port,
        "",
        r#"tools call ParamCommand_Add1DParameter --json {"name":"Blink","min":0,"max":1}"#,
    )
    .unwrap();
    assert_eq!(created["result"]["isError"], false, "{created}");
    let listed = njc(port, "", "resources read resource://vsoul/parameters").unwrap();
    let text = listed["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains(r#""name":"Blink""#), "{text}");
    let snapshot = server.join().unwrap();
    assert_eq!(snapshot["model"]["param"][0]["name"], "Blink");
}

#[test]
fn notifications_are_answered_with_204_and_no_body() {
    let (port, server) = start(None, 3);
    for body in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
    ] {
        assert_eq!(split(&raw(port, &post(body))), ("HTTP/1.1 204 No Content", ""));
    }
    let ping = raw(port, &post(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#));
    assert_eq!(split(&ping), ("HTTP/1.1 200 OK", r#"{"id":7,"jsonrpc":"2.0","result":{}}"#));
    server.join().unwrap();
}

#[test]
fn malformed_json_is_a_200_parse_error() {
    let (port, server) = start(None, 3);
    let expected = r#"{"error":{"code":-32700,"message":"Parse error"},"id":null,"jsonrpc":"2.0"}"#;
    for body in [&b"{not json"[..], b"", b"\xff\xfe\x00"] {
        let mut request = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n", body.len())
            .into_bytes();
        request.extend_from_slice(body);
        assert_eq!(split(&raw(port, &request)), ("HTTP/1.1 200 OK", expected));
    }
    server.join().unwrap();
}

#[test]
fn a_json_value_that_is_not_a_request_object_is_an_invalid_request() {
    let (port, server) = start(None, 1);
    let response = raw(port, &post("[]"));
    server.join().unwrap();
    let (status, body) = split(&response);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        body,
        r#"{"error":{"code":-32600,"message":"invalid JSON-RPC request"},"id":null,"jsonrpc":"2.0"}"#
    );
}

#[test]
fn wrong_path_and_wrong_method_are_rejected() {
    let (port, server) = start(None, 4);
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let wrong_path = format!(
        "POST /nope HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert_eq!(
        split(&raw(port, wrong_path.as_bytes())),
        ("HTTP/1.1 404 Not Found", r#"{"error":"not_found"}"#)
    );
    // D served GET /events (SSE); this host does not.
    let events = raw(port, b"GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert_eq!(split(&events), ("HTTP/1.1 404 Not Found", r#"{"error":"not_found"}"#));
    for method in ["GET", "DELETE"] {
        let response = raw(port, format!("{method} /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes());
        assert!(response.contains("\r\nAllow: POST\r\n"), "{response}");
        assert_eq!(
            split(&response),
            ("HTTP/1.1 405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#)
        );
    }
    server.join().unwrap();
}

#[test]
fn bearer_token_is_enforced_when_configured() {
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let (port, server) = start(Some("s3cret"), 9);
    for header in [
        "",
        "Authorization: Bearer s3creT\r\n",
        "Authorization: Bearer s3cre\r\n",
        "Authorization: Bearer s3cret!\r\n",
        "Authorization: Bearer \r\n",
        "Authorization: Basic czNjcmV0\r\n",
        "Authorization: s3cret\r\n",
    ] {
        let response = raw(port, &post_with(header, ping));
        assert!(response.contains("\r\nWWW-Authenticate: Bearer\r\n"), "{header}: {response}");
        assert_eq!(
            split(&response),
            ("HTTP/1.1 401 Unauthorized", r#"{"error":"invalid_token"}"#),
            "{header}"
        );
    }
    let ok = raw(port, &post_with("Authorization: Bearer s3cret\r\n", ping));
    assert_eq!(split(&ok), ("HTTP/1.1 200 OK", r#"{"id":1,"jsonrpc":"2.0","result":{}}"#));
    let lower = raw(port, &post_with("Authorization: bearer s3cret\r\n", ping));
    assert_eq!(split(&lower).0, "HTTP/1.1 200 OK", "the scheme name is case-insensitive");
    server.join().unwrap();
}

#[test]
fn the_real_client_authenticates_with_its_bearer_token() {
    let (port, server) = start(Some("s3cret"), 3);
    let rejected = njc(port, "wrong", "tools list").unwrap_err();
    assert_eq!(rejected, "MCP server returned HTTP/1.1 401 Unauthorized");
    let accepted = njc(port, "s3cret", "rpc ping").unwrap();
    assert_eq!(accepted["result"], json!({}));
    server.join().unwrap();
}

#[test]
fn oversized_and_unframed_bodies_are_rejected() {
    let (port, server) = start(None, 5);
    let too_big = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_BYTES + 1
    );
    assert_eq!(
        split(&raw(port, too_big.as_bytes())),
        ("HTTP/1.1 413 Payload Too Large", r#"{"error":"payload_too_large"}"#)
    );
    let huge = "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 99999999999999999999999\r\n\r\n";
    assert_eq!(split(&raw(port, huge.as_bytes())).0, "HTTP/1.1 413 Payload Too Large");
    let unframed = raw(port, b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n{}");
    assert_eq!(split(&unframed), ("HTTP/1.1 411 Length Required", r#"{"error":"length_required"}"#));
    let chunked = raw(port, b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n");
    assert_eq!(split(&chunked), ("HTTP/1.1 411 Length Required", r#"{"error":"length_required"}"#));
    let at_cap = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {MAX_BODY_BYTES}\r\n\r\n");
    // A body exactly at the cap is accepted for reading; a truncated one is dropped without a reply.
    assert_eq!(raw(port, at_cap.as_bytes()), "");
    server.join().unwrap();
}

#[test]
fn oversized_and_malformed_heads_are_rejected() {
    let (port, server) = start(None, 7);
    let padding = "x".repeat(MAX_HEAD_BYTES);
    let big = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {padding}\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(
        split(&raw(port, big.as_bytes())),
        ("HTTP/1.1 431 Request Header Fields Too Large", r#"{"error":"header_too_large"}"#)
    );
    let endless = vec![b'a'; MAX_HEAD_BYTES * 2];
    assert_eq!(split(&raw(port, &endless)).0, "HTTP/1.1 431 Request Header Fields Too Large");
    let bad: [&[u8]; 5] = [
        b"garbage\r\n\r\n",
        b"POST /mcp\r\n\r\n",
        b"POST /mcp HTTP/1.1\r\nno colon here\r\n\r\n",
        b"POST /mcp HTTP/1.1\r\nContent-Length: abc\r\n\r\n",
        b"POST /mcp HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
    ];
    for request in bad {
        assert_eq!(
            split(&raw(port, request)),
            ("HTTP/1.1 400 Bad Request", r#"{"error":"bad_request"}"#),
            "{}",
            String::from_utf8_lossy(request)
        );
    }
    server.join().unwrap();
}

#[test]
fn non_utf8_head_bytes_are_a_400() {
    let (port, server) = start(None, 1);
    let response = raw(port, b"POST /mcp HTTP/1.1\r\nX-Bin: \xff\xfe\r\n\r\n");
    server.join().unwrap();
    assert_eq!(split(&response), ("HTTP/1.1 400 Bad Request", r#"{"error":"bad_request"}"#));
}

#[test]
fn host_and_origin_must_be_loopback() {
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let (port, server) = start(None, 8);
    let host = |value: &str| {
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {value}\r\nContent-Length: {}\r\n\r\n{ping}",
            ping.len()
        );
        raw(port, request.as_bytes())
    };
    for value in ["evil.example", "evil.example:8088", "127.0.0.1.evil.example"] {
        assert_eq!(split(&host(value)), ("HTTP/1.1 403 Forbidden", r#"{"error":"forbidden_host"}"#), "{value}");
    }
    let no_host = format!("POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n{ping}", ping.len());
    assert_eq!(split(&raw(port, no_host.as_bytes())).0, "HTTP/1.1 403 Forbidden");
    assert_eq!(split(&host("localhost:8088")).0, "HTTP/1.1 200 OK");

    let evil = raw(port, &post_with("Origin: http://evil.example\r\n", ping));
    assert_eq!(split(&evil), ("HTTP/1.1 403 Forbidden", r#"{"error":"forbidden_origin"}"#));
    let null_origin = raw(port, &post_with("Origin: null\r\n", ping));
    assert_eq!(split(&null_origin).0, "HTTP/1.1 403 Forbidden");
    let local = raw(port, &post_with("Origin: http://localhost:3000\r\n", ping));
    assert_eq!(split(&local), ("HTTP/1.1 200 OK", r#"{"id":1,"jsonrpc":"2.0","result":{}}"#));
    server.join().unwrap();
}

struct Host(Child, BufReader<std::process::ChildStderr>);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A command whose config directory (where a generated token file goes) is a scratch directory of this test run.
fn isolated(program: &str) -> Command {
    let home = std::env::temp_dir().join(format!("soul2d-mcp-http-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(program);
    command.env_remove("SOUL2D_MCP_BEARER").env_remove("INOCHI_CONFIG_PATH").env("HOME", &home).env("XDG_CONFIG_HOME", &home).env("APPDATA", &home);
    command
}

/// Start the real binary and return it with the port it printed on stderr.
fn spawn_cli(args: &[&str], bearer: Option<&str>) -> (Host, u16) {
    let mut command = isolated(env!("CARGO_BIN_EXE_soul2d-core"));
    command.args(args).stderr(Stdio::piped());
    if let Some(token) = bearer {
        command.env("SOUL2D_MCP_BEARER", token);
    }
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    stderr.read_line(&mut line).unwrap();
    let host = Host(child, stderr);
    let port = line
        .trim()
        .strip_prefix("headless MCP host listening on http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .unwrap_or_else(|| panic!("unexpected banner: {line:?}"))
        .parse()
        .unwrap();
    (host, port)
}

fn exit_code(args: &[&str]) -> (Option<i32>, String) {
    let output = isolated(env!("CARGO_BIN_EXE_soul2d-core"))
        .args(args)
        .output()
        .unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn cli_serves_mcp_over_http_and_opens_a_project() {
    let path = std::env::temp_dir().join(format!("soul2d-mcp-http-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut seed = Session::new();
    let created = seed.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"FromDisk"}));
    assert_eq!(created["succeeded"], true);
    let saved = seed.handle_request(&json!({"op":"save","path":path.to_string_lossy()}));
    assert_eq!(saved["succeeded"], true, "{saved}");

    let (_host, port) = spawn_cli(&["--mcp-port", "0", "--no-auth", path.to_str().unwrap()], None);
    let listed = njc(port, "", "resources read resource://vsoul/parameters").unwrap();
    let _ = std::fs::remove_file(&path);
    let text = listed["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains(r#""name":"FromDisk""#), "{text}");
}

#[test]
fn cli_reads_the_bearer_token_from_the_environment() {
    let (_host, port) = spawn_cli(&["--mcp-port", "0", "--mcp-host", "127.0.0.1"], Some("env-token"));
    assert_eq!(
        njc(port, "", "tools list").unwrap_err(),
        "MCP server returned HTTP/1.1 401 Unauthorized"
    );
    assert_eq!(njc(port, "env-token", "rpc ping").unwrap()["result"], json!({}));
}

#[test]
fn cli_without_a_token_writes_one_for_the_owner_that_njc_finds() {
    let (mut host, port) = spawn_cli(&["--mcp-port", "0"], None);
    let mut line = String::new();
    host.1.read_line(&mut line).unwrap();
    let path = std::path::PathBuf::from(line.trim().strip_prefix("bearer token written to ").expect(&line));
    assert!(path.ends_with(format!("mcp-{port}.token")), "{path:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let contents = std::fs::read_to_string(&path).unwrap();
    let (address, token) = contents.trim_end().split_once('\n').unwrap();
    assert_eq!((address, token.len()), (format!("127.0.0.1:{port}").as_str(), 64));
    assert_eq!(njc(port, "", "tools list").unwrap_err(), "MCP server returned HTTP/1.1 401 Unauthorized");
    assert_eq!(njc(port, token, "rpc ping").unwrap()["result"], json!({}));
    let found = isolated(env!("CARGO_BIN_EXE_njc")).args(["--port", &port.to_string(), "rpc", "ping"]).output().unwrap();
    assert!(found.status.success(), "{}", String::from_utf8_lossy(&found.stderr));
    // The other loopback address may belong to another account: njc does not send it the token, and `localhost`
    // goes to the address the token was made for even when the other one listens too.
    if let Ok(trap) = TcpListener::bind(("::1", port)) {
        let heard = thread::spawn(move || {
            let (stream, _) = trap.accept().unwrap();
            let mut head = String::new();
            let mut reader = BufReader::new(stream);
            while reader.read_line(&mut head).unwrap() > 2 && !head.ends_with("\r\n\r\n") {}
            head
        });
        let njc_at = |host: &str| isolated(env!("CARGO_BIN_EXE_njc"))
            .args(["--host", host, "--port", &port.to_string(), "rpc", "ping"]).output().unwrap();
        assert!(njc_at("localhost").status.success());
        assert!(!njc_at("::1").status.success());
        let head = heard.join().unwrap();
        assert!(head.starts_with("POST /mcp") && !head.to_ascii_lowercase().contains("authorization"), "{head}");
    }
    let _ = std::fs::remove_file(&path);

    let (_open, open_port) = spawn_cli(&["--mcp-port", "0", "--no-auth"], None);
    assert_eq!(njc(open_port, "", "rpc ping").unwrap()["result"], json!({}));
}

#[test]
fn cli_refuses_bad_options() {
    let (code, stderr) = exit_code(&["--mcp-host", "0.0.0.0", "--mcp-port", "0"]);
    assert_eq!(code, Some(2));
    assert!(stderr.starts_with("soul2d-core: refusing to serve a non-loopback address"), "{stderr}");
    assert_eq!(exit_code(&["--bogus"]).0, Some(2));
    assert_eq!(exit_code(&["--mcp-port"]).0, Some(2));
    assert_eq!(exit_code(&["--mcp-port", "70000"]).0, Some(2));
    let (code, stderr) = exit_code(&["--mcp-port", "0", "--project", "/nonexistent/soul2d.json"]);
    assert_eq!(code, Some(1));
    assert!(stderr.starts_with("soul2d-core: failed to open project: /nonexistent/soul2d.json"), "{stderr}");
    assert_eq!(exit_code(&["--help"]).0, Some(0));
}

#[test]
fn njc_read_by_uuid_and_find_by_selector_reach_the_session() {
    let (port, server) = start(None, 8);
    let nodes = njc(port, "", "resources read resource://vsoul/nodes").unwrap();
    let root: Value =
        serde_json::from_str(nodes["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    let uuid = root["uuid"].as_u64().unwrap();

    let read = njc(port, "", &format!("read {uuid}")).unwrap();
    let content = &read["result"]["contents"][0];
    assert_eq!(content["uri"], format!("resource://soul2d/resources/{uuid}"));
    // D's read shape: {"item": {typeId, uuid, name, data}}.
    let item = serde_json::from_str::<Value>(content["text"].as_str().unwrap()).unwrap();
    assert_eq!((item["item"]["typeId"].as_str(), item["item"]["uuid"].as_u64()), (Some("Node"), Some(uuid)));
    assert_eq!(item["item"]["data"], root);

    let found = njc(port, "", &format!("find #{uuid}")).unwrap();
    let found: Value =
        serde_json::from_str(found["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(found["selectorSyntax"], "soul2d.core.selector");
    let all = njc(port, "", "find *").unwrap();
    assert!(all["result"]["contents"][0]["text"].as_str().unwrap().contains("\"items\""));
    assert_eq!(found["items"].as_array().map(Vec::len), Some(0), "the root node is never a result");
    server.join().unwrap();
}

#[test]
fn a_stalled_client_holds_a_budgeted_connection_for_about_the_budget() {
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Half a request head and then silence, with the socket kept open.
    let mut stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stalled.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n").unwrap();
    let (stream, _) = listener.accept().unwrap();
    let budget = Duration::from_millis(300);
    let started = Instant::now();
    handle_connection_within(stream, &mut Session::new(), None, budget).unwrap();
    let took = started.elapsed();
    assert!(took >= budget && took < Duration::from_secs(5), "{took:?}");

    // A well-behaved client is answered at once, long before the budget.
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_connection_within(stream, &mut Session::new(), None, Duration::from_secs(30)).unwrap();
    });
    let started = Instant::now();
    let reply = raw(port, &post(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#));
    server.join().unwrap();
    assert_eq!(split(&reply), ("HTTP/1.1 200 OK", r#"{"id":7,"jsonrpc":"2.0","result":{}}"#));
    assert!(started.elapsed() < Duration::from_secs(5));
}
