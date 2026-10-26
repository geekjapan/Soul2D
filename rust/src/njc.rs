//! Command-line MCP client for a running Soul2D instance over plain HTTP.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;

pub const USAGE: &str = "Usage:
  njc [connection options] tools list
  njc [connection options] tools call <name> [--json <object>]
  njc [connection options] resources list
  njc [connection options] resources templates
  njc [connection options] resources read <uri>
  njc [connection options] find <selector>
  njc [connection options] read <uuid-or-uri>
  njc [connection options] rpc <method> [--json <params>]

Connection options:
  --host <host>       MCP host, default 127.0.0.1
  --port <port>       MCP port, default 8088
  --path <path>       MCP HTTP path, default /mcp
  --url <url>         HTTP MCP endpoint; SOUL2D_MCP_URL is also honored
  --bearer <token>    Authorization bearer token; SOUL2D_MCP_BEARER is also honored, and without either
                      the token in mcp-<port>.token in the config directory is used for the address it names
  --json              Alias for the default raw JSON-RPC output
  --summary           Print a human-readable summary for list/template commands
";

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub bearer: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub endpoint: Endpoint,
    pub command: Vec<String>,
    pub help: bool,
    pub raw_json: bool,
    pub summary: bool,
}

fn parse_port(text: &str) -> Result<u16, String> {
    match text.parse::<u16>() {
        Ok(port) if port >= 1 => Ok(port),
        _ => Err("port must be in 1..65535".into()),
    }
}

fn apply_url(endpoint: &mut Endpoint, url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("--url supports only plain HTTP MCP endpoints")?;
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/mcp"),
    };
    // `[::1]:8088` is how the hosts print an IPv6 endpoint.
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']').ok_or("IPv6 endpoint host needs a closing bracket")?;
            match after.strip_prefix(':') {
                Some(port) => (host, Some(parse_port(port)?)),
                None if after.is_empty() => (host, None),
                None => return Err("endpoint authority is malformed".into()),
            }
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(parse_port(port)?)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return Err("endpoint host is empty".into());
    }
    endpoint.host = host.into();
    endpoint.path = path.into();
    if let Some(port) = port {
        endpoint.port = port;
    }
    Ok(())
}

/// `args` excludes the program name; the two environment values come from
/// `SOUL2D_MCP_URL` and `SOUL2D_MCP_BEARER`.
pub fn parse_options(args: &[String], env_url: &str, env_bearer: &str) -> Result<Options, String> {
    let mut options = Options {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port: 8088,
            path: "/mcp".into(),
            bearer: env_bearer.into(),
        },
        command: Vec::new(),
        help: false,
        raw_json: false,
        summary: false,
    };
    if !env_url.is_empty() {
        apply_url(&mut options.endpoint, env_url)?;
    }
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        let mut value = || {
            let value = args.get(index).ok_or(format!("{arg} requires a value"));
            index += 1;
            value
        };
        match arg {
            "-h" | "--help" => options.help = true,
            "--url" => apply_url(&mut options.endpoint, value()?)?,
            "--host" => options.endpoint.host = value()?.clone(),
            "--port" => options.endpoint.port = parse_port(value()?)?,
            "--path" => {
                let path = value()?;
                options.endpoint.path = if path.starts_with('/') {
                    path.clone()
                } else {
                    format!("/{path}")
                };
            }
            "--bearer" => options.endpoint.bearer = value()?.clone(),
            "--json" => options.raw_json = true,
            "--summary" => options.summary = true,
            _ => {
                options.command = args[index - 1..].to_vec();
                break;
            }
        }
    }
    if !options.help && options.command.is_empty() {
        return Err("missing command; use --help".into());
    }
    Ok(options)
}

/// Percent-encode everything outside the URI unreserved set, as `encodeURIComponent` does.
fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn json_option(args: &[String]) -> Result<Value, String> {
    match args {
        [] => Ok(json!({})),
        [flag, text] if flag == "--json" => {
            serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))
        }
        _ => Err("expected --json <json>".into()),
    }
}

fn read_request(uri: String) -> (String, Value) {
    ("resources/read".into(), json!({ "uri": uri }))
}

/// Map a command line onto the JSON-RPC method and params it sends.
pub fn rpc_for(command: &[String]) -> Result<(String, Value), String> {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => Err("missing command".into()),
        ["tools"] => Err("tools requires list or call".into()),
        ["tools", "list"] => Ok(("tools/list".into(), json!({}))),
        ["tools", "list", ..] => Err("tools list takes no arguments".into()),
        ["tools", "call"] => Err("tools call requires a tool name".into()),
        ["tools", "call", name, ..] => Ok((
            "tools/call".into(),
            json!({ "name": name, "arguments": json_option(&command[3..])? }),
        )),
        ["resources"] => Err("resources requires list, templates, or read".into()),
        ["resources", "list"] => Ok(("resources/list".into(), json!({}))),
        ["resources", "list", ..] => Err("resources list takes no arguments".into()),
        ["resources", "templates"] => Ok(("resources/templates/list".into(), json!({}))),
        ["resources", "templates", ..] => Err("resources templates takes no arguments".into()),
        ["resources", "read", uri] => Ok(read_request((*uri).into())),
        ["resources", "read", ..] => Err("resources read requires a URI".into()),
        ["find", selector] => Ok(read_request(format!(
            "resource://soul2d/resources/find?selector={}",
            encode_component(selector)
        ))),
        ["find", ..] => Err("find requires one selector".into()),
        ["read", uri] if uri.starts_with("resource://") => Ok(read_request((*uri).into())),
        ["read", uuid] => Ok(read_request(format!(
            "resource://soul2d/resources/{}",
            encode_component(uuid)
        ))),
        ["read", ..] => Err("read requires a resource UUID or URI".into()),
        ["rpc"] => Err("rpc requires a method".into()),
        ["rpc", method, ..] => Ok(((*method).into(), json_option(&command[2..])?)),
        [other, ..] => Err(format!("unknown command: {other}")),
    }
}

fn decode_chunked(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut position = 0;
    loop {
        let line_end = body[position..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or("invalid chunked response")?
            + position;
        let size_text = String::from_utf8_lossy(&body[position..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = if size_text.is_empty() {
            0
        } else {
            usize::from_str_radix(size_text, 16).map_err(|_| "invalid chunk size")?
        };
        position = line_end + 2;
        if size == 0 {
            return Ok(out);
        }
        let end = position
            .checked_add(size)
            .filter(|end| end.checked_add(2).is_some_and(|after| after <= body.len()))
            .ok_or("truncated chunked response")?;
        out.extend_from_slice(&body[position..end]);
        position = end + 2;
    }
}

/// Extract the JSON body from one complete HTTP/1.1 response.
pub fn parse_http_response(data: &[u8]) -> Result<Value, String> {
    let separator = data
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("invalid HTTP response")?;
    let header = String::from_utf8_lossy(&data[..separator]);
    let status = header.lines().next().unwrap_or("");
    if !status.contains(" 200 ") && !status.contains(" 204 ") {
        return Err(format!("MCP server returned {status}"));
    }
    let body = &data[separator + 4..];
    let body = if header.to_lowercase().contains("transfer-encoding: chunked") {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({ "jsonrpc": "2.0", "result": null }));
    }
    serde_json::from_slice(&body).map_err(|error| format!("invalid JSON response: {error}"))
}

fn call(endpoint: &Endpoint, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    // An IPv6 literal is bracketed in the Host header.
    let host = if endpoint.host.contains(':') { format!("[{}]", endpoint.host) } else { endpoint.host.clone() };
    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        endpoint.path,
        host,
        endpoint.port,
        body.len()
    );
    if !endpoint.bearer.is_empty() {
        request.push_str(&format!("Authorization: Bearer {}\r\n", endpoint.bearer));
    }
    request.push_str("\r\n");
    request.push_str(&body);

    let io = |error: std::io::Error| error.to_string();
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).map_err(io)?;
    // ponytail: fixed idle timeout and response cap; make them flags if a tool legitimately needs more.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(300);
    const MAX_RESPONSE: u64 = 512 * 1024 * 1024;
    stream.set_read_timeout(Some(IDLE)).map_err(io)?;
    stream.set_write_timeout(Some(IDLE)).map_err(io)?;
    stream.write_all(request.as_bytes()).map_err(io)?;
    let mut data = Vec::new();
    (&mut stream).take(MAX_RESPONSE + 1).read_to_end(&mut data).map_err(io)?;
    if data.len() as u64 > MAX_RESPONSE {
        return Err("MCP response exceeds the size limit".into());
    }
    if data.is_empty() {
        return Err("empty response from MCP server".into());
    }
    let response = parse_http_response(&data)?;
    if !response.is_object() {
        return Err("MCP response is not a JSON object".into());
    }
    if let Some(error) = response.get("error") {
        return Err(format!("MCP error: {error}"));
    }
    Ok(response)
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

fn text<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Human-readable listing for `--summary`; `None` when the command has no summary form.
pub fn summarize(command: &[String], response: &Value) -> Option<String> {
    let (title, key) = match (command.first()?.as_str(), command.get(1)?.as_str()) {
        ("resources", "list") => ("resources", "resources"),
        ("tools", "list") => ("tools", "tools"),
        ("resources", "templates") => ("resource templates", "resourceTemplates"),
        _ => return None,
    };
    let items = response.get("result")?.get(key)?.as_array()?;
    let mut out = format!("{title}: {}\n", items.len());
    for item in items {
        out.push_str(&format!("- {}\n", text(item, "name")));
        let location = if key == "resources" { "uri" } else { "uriTemplate" };
        if key != "tools" {
            out.push_str(&format!("  {location}: {}\n", text(item, location)));
        }
        let description = first_line(text(item, "description"));
        if !description.is_empty() {
            out.push_str(&format!("  {description}\n"));
        }
    }
    Some(out)
}

/// Run one njc invocation and return the text it prints on standard output.
pub fn run(args: &[String], env_url: &str, env_bearer: &str) -> Result<String, String> {
    let mut options = parse_options(args, env_url, env_bearer)?;
    let host = &options.endpoint.host;
    if options.endpoint.bearer.is_empty() && crate::mcp_http::is_loopback_host(host) {
        if let Some((address, token)) = crate::mcp_http::read_token_file(options.endpoint.port) {
            // Connect to the address the token was made for, never to whatever else `localhost` resolves to.
            if host == "localhost" || host.parse::<std::net::IpAddr>() == Ok(address.ip()) {
                options.endpoint.host = address.ip().to_string();
                options.endpoint.bearer = token;
            }
        }
    }
    if options.help {
        return Ok(format!("{USAGE}\n"));
    }
    let (method, params) = rpc_for(&options.command)?;
    let initialized = call(
        &options.endpoint,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "njc", "version": env!("CARGO_PKG_VERSION") },
        }),
    )?;
    if initialized.get("result").is_none() {
        return Err("initialize response has no result".into());
    }
    let response = call(&options.endpoint, 2, &method, params)?;
    let summary = if options.summary && !options.raw_json {
        summarize(&options.command, &response)
    } else {
        None
    };
    Ok(summary.unwrap_or_else(|| format!("{response}\n")))
}
