use serde_json::{json, Value};
use soul2d_core::njc::{parse_http_response, parse_options, rpc_for, run, summarize};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(String::from).collect()
}

fn read_request(stream: &mut impl Read) -> String {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        data.extend_from_slice(&buffer[..count]);
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some((header, body)) = text.split_once("\r\n\r\n") {
            let length: usize = header
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            if body.len() >= length {
                return text;
            }
        }
    }
}

/// Serve the given raw HTTP responses, one connection each, and return what was received.
fn serve(responses: Vec<String>) -> (u16, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                stream.write_all(response.as_bytes()).unwrap();
                request
            })
            .collect()
    });
    (port, handle)
}

fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

const INITIALIZED: &str = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;

#[test]
fn tool_call_sends_initialize_then_the_call_and_prints_the_raw_response() {
    let reply = r#"{"jsonrpc":"2.0","id":2,"result":{"isError":false}}"#;
    let (port, server) = serve(vec![ok(INITIALIZED), ok(reply)]);
    let mut args = words(&format!("--port {port} --bearer secret tools call Rename --json"));
    args.push(r#"{"name":"a\"b"}"#.into());

    let output = run(&args, "", "").unwrap();

    // Object keys print sorted; the D client printed them in unspecified hash order.
    assert_eq!(output, "{\"id\":2,\"jsonrpc\":\"2.0\",\"result\":{\"isError\":false}}\n");
    let requests = server.join().unwrap();
    let (header, body) = requests[1].split_once("\r\n\r\n").unwrap();
    assert_eq!(
        header,
        format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
             Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\
             Authorization: Bearer secret",
            body.len()
        )
    );
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap(),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "Rename", "arguments": { "name": "a\"b" } },
        })
    );
    let initialize: Value =
        serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["params"]["clientInfo"]["name"], "njc");
}

#[test]
fn server_errors_and_bad_status_fail_with_the_reported_reason() {
    let failure = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601}}"#;
    let (port, server) = serve(vec![ok(INITIALIZED), ok(failure)]);
    let error = run(&words(&format!("--port {port} rpc nope")), "", "").unwrap_err();
    assert_eq!(error, r#"MCP error: {"code":-32601}"#);
    server.join().unwrap();

    let (port, server) = serve(vec!["HTTP/1.1 401 Unauthorized\r\n\r\n".into()]);
    let error = run(&words("tools list"), &format!("http://127.0.0.1:{port}/x"), "").unwrap_err();
    assert_eq!(error, "MCP server returned HTTP/1.1 401 Unauthorized");
    assert!(server.join().unwrap()[0].starts_with("POST /x HTTP/1.1\r\n"));
}

#[test]
fn chunked_and_empty_bodies_decode() {
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    8;ext=1\r\n{\"a\":[1,\r\n3\r\n2]}\r\n0\r\n\r\n";
    assert_eq!(parse_http_response(chunked).unwrap(), json!({ "a": [1, 2] }));
    assert_eq!(
        parse_http_response(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap(),
        json!({ "jsonrpc": "2.0", "result": null })
    );
    assert_eq!(
        parse_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nff\r\nab\r\n")
            .unwrap_err(),
        "truncated chunked response"
    );
    assert_eq!(
        parse_http_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nab\r\n"
        )
        .unwrap_err(),
        "truncated chunked response"
    );
    assert_eq!(parse_http_response(b"garbage").unwrap_err(), "invalid HTTP response");
}

#[test]
fn commands_map_to_methods_and_encoded_resource_uris() {
    let request = |text: &str| rpc_for(&words(text));
    assert_eq!(request("tools list").unwrap(), ("tools/list".into(), json!({})));
    assert_eq!(
        request("resources templates").unwrap(),
        ("resources/templates/list".into(), json!({}))
    );
    assert_eq!(
        request("find Node[name=\"a/b\"]").unwrap().1,
        json!({ "uri": "resource://soul2d/resources/find?selector=Node%5Bname%3D%22a%2Fb%22%5D" })
    );
    assert_eq!(
        request("read 42").unwrap().1,
        json!({ "uri": "resource://soul2d/resources/42" })
    );
    assert_eq!(
        request("read resource://soul2d/x?y=1").unwrap().1,
        json!({ "uri": "resource://soul2d/x?y=1" })
    );
    assert_eq!(
        request("rpc ping --json [1]").unwrap(),
        ("ping".into(), json!([1]))
    );
    assert_eq!(request("tools call").unwrap_err(), "tools call requires a tool name");
    assert_eq!(request("tools call x --json").unwrap_err(), "expected --json <json>");
    assert_eq!(request("resources read").unwrap_err(), "resources read requires a URI");
    assert_eq!(request("launch").unwrap_err(), "unknown command: launch");
}

#[test]
fn options_layer_environment_then_flags_and_reject_bad_endpoints() {
    let options = parse_options(
        &words("--path mcp2 --port 9 --summary tools list --json"),
        "http://example.test:7000/base",
        "token",
    )
    .unwrap();
    assert_eq!(options.endpoint.host, "example.test");
    assert_eq!(options.endpoint.port, 9);
    assert_eq!(options.endpoint.path, "/mcp2");
    assert_eq!(options.endpoint.bearer, "token");
    assert_eq!(options.command, words("tools list --json"));
    assert!(options.summary && !options.raw_json);

    let defaults = parse_options(&words("--url http://h tools list"), "", "").unwrap();
    assert_eq!((defaults.endpoint.port, defaults.endpoint.path.as_str()), (8088, "/mcp"));

    let error = |text: &str| parse_options(&words(text), "", "").unwrap_err();
    assert_eq!(error("--url https://h tools list"), "--url supports only plain HTTP MCP endpoints");
    assert_eq!(error("--port 0 tools list"), "port must be in 1..65535");
    assert_eq!(error("--port 65536 tools list"), "port must be in 1..65535");
    assert_eq!(error("--host"), "--host requires a value");
    assert_eq!(error("--summary"), "missing command; use --help");
    assert!(run(&words("--help"), "", "").unwrap().starts_with("Usage:\n  njc "));
}

#[test]
fn summary_lists_names_locations_and_first_description_lines() {
    let response = json!({ "result": { "resources": [
        { "name": "Head", "uri": "resource://soul2d/resources/1", "description": " top \nrest" },
        { "name": "Body", "uri": "resource://soul2d/resources/2" },
    ] } });
    assert_eq!(
        summarize(&words("resources list"), &response).unwrap(),
        "resources: 2\n- Head\n  uri: resource://soul2d/resources/1\n  top\n\
         - Body\n  uri: resource://soul2d/resources/2\n"
    );
    let tools = json!({ "result": { "tools": [{ "name": "Undo", "description": "Step back" }] } });
    assert_eq!(
        summarize(&words("tools list"), &tools).unwrap(),
        "tools: 1\n- Undo\n  Step back\n"
    );
    assert_eq!(summarize(&words("read 1"), &response), None);
}

#[test]
fn bracketed_ipv6_urls_are_parsed() {
    let parse = |text: &str| parse_options(&words(text), "", "").map(|options| options.endpoint);
    let v6 = parse("--url http://[::1]:18088/mcp tools list").unwrap();
    assert_eq!((v6.host.as_str(), v6.port, v6.path.as_str()), ("::1", 18088, "/mcp"));
    assert_eq!(parse("--url http://[::1] tools list").unwrap().host, "::1");
    assert!(parse("--url http://[::1 tools list").is_err());
    assert!(parse("--url http://[::1]x tools list").is_err());
}
