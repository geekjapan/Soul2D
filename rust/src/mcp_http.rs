//! Plain-HTTP transport for the MCP engine (`POST /mcp`), served from one thread.
//!
//! One [`Session`] is shared by every connection; connections are handled one at a time because the
//! session is not designed for concurrent access. Wire contract (ported from the D headless host):
//! only `POST /mcp`; a body with no `id` (or a null `id`) is a notification and is answered with
//! `204`; malformed JSON is answered with a `200` JSON-RPC `-32700` error; a configured bearer
//! token is required in `Authorization: Bearer <token>` or the answer is `401`.
//! Stricter than D: size caps, `Host`/`Origin` loopback checks, constant-time token comparison.

use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use crate::Session;

/// A reply and its optional one-use local delivery receipt. Only the writer completes it.
pub struct RpcReply {
    message: Option<Value>,
    delivery: Option<(bool, SyncSender<Result<(), String>>)>,
}

impl RpcReply {
    pub fn plain(message: Option<Value>) -> Self {
        Self { message, delivery: None }
    }

    /// Construct after dispatch returns: unwinding before this point drops the receipt sender.
    pub fn tracked(request: &Value, message: Option<Value>, delivery: SyncSender<Result<(), String>>) -> Self {
        let valid = request["jsonrpc"] == "2.0"
            && request.get("id").is_some_and(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some())
            && message.as_ref().is_some_and(|reply|
            reply["jsonrpc"] == "2.0" && reply.get("id") == request.get("id")
                && reply.get("error").is_none() && reply.get("result").is_some()
                && reply["result"]["isError"] != true);
        Self { message, delivery: Some((valid, delivery)) }
    }

    /// Internal consumers have no writer; dropping any receipt cannot authorize closing.
    pub fn into_message(self) -> Option<Value> {
        self.message
    }

    fn finish(self, outcome: io::Result<()>) -> io::Result<()> {
        if let Some((valid, delivery)) = self.delivery {
            let result = match &outcome {
                Err(error) => Err(error.to_string()),
                Ok(()) if valid => Ok(()),
                Ok(()) => Err("The accepted reply was not produced".to_owned()),
            };
            let _ = delivery.try_send(result);
        }
        outcome
    }

    /// Emit the complete JSON line, including newline and flush, before completing its receipt.
    pub fn write_line(self, writer: &mut impl Write) -> io::Result<()> {
        let result = if let Some(message) = &self.message {
            let line = format!("{message}\n");
            writer.write_all(line.as_bytes()).and_then(|()| writer.flush())
        } else {
            Ok(())
        };
        self.finish(result)
    }

    fn write_http(self, stream: &mut TcpStream, notification: bool, budget: Duration) -> io::Result<()> {
        let (status, body) = match (&self.message, notification) {
            (Some(message), false) => (200, message.to_string()),
            _ => (204, String::new()),
        };
        let result = write_response(stream, status, &[], &body, budget);
        if notification {
            // Even a malformed handler reply cannot turn a notification into a delivery acknowledgement.
            let Self { message, .. } = self;
            drop(message);
            return result;
        }
        self.finish(result)
    }
}

/// Largest accepted request head (request line plus headers).
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Total time one connection may take to deliver a complete request.
const REQUEST_BUDGET: Duration = Duration::from_secs(30);
/// Time spent discarding unread client bytes after a reply, so the reply is not lost to a reset.
const DRAIN_BUDGET: Duration = Duration::from_secs(1);
const DRAIN_MAX_BYTES: usize = 64 * 1024;

struct Request {
    method: String,
    target: String,
    /// Header names are lower-cased.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A refused request: the status code and a short machine-readable error code.
struct Reject {
    status: u16,
    error: &'static str,
    headers: &'static [(&'static str, &'static str)],
}

fn reject(status: u16, error: &'static str) -> Reject {
    Reject { status, error, headers: &[] }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

/// True for `localhost` and loopback IP literals (`[::1]` brackets accepted).
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Host name part of a `Host` header value or an `Origin` URL, without scheme, port or brackets.
fn authority_host(value: &str) -> Option<&str> {
    let rest = value.split_once("://").map_or(value, |(_, rest)| rest);
    let authority = rest.split('/').next()?;
    if authority.contains('@') {
        return None;
    }
    if let Some(inner) = authority.strip_prefix('[') {
        return inner.split_once(']').map(|(host, _)| host);
    }
    authority.split(':').next().filter(|host| !host.is_empty())
}

/// Compare without stopping at the first differing byte; only the expected length is observable.
fn token_matches(given: &[u8], expected: &[u8]) -> bool {
    let mut diff = given.len() ^ expected.len();
    for (index, byte) in expected.iter().enumerate() {
        diff |= usize::from(given.get(index).copied().unwrap_or(0) ^ byte);
    }
    diff == 0
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Read once into `buf`; `None` on timeout, end of stream or I/O error (the connection is dropped).
fn fill(stream: &mut TcpStream, deadline: Instant, buf: &mut Vec<u8>) -> Option<()> {
    let left = deadline.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())?;
    stream.set_read_timeout(Some(left)).ok()?;
    let mut chunk = [0u8; 4096];
    match stream.read(&mut chunk) {
        Ok(0) | Err(_) => None,
        Ok(count) => {
            buf.extend_from_slice(&chunk[..count]);
            Some(())
        }
    }
}

fn parse_head(head: &[u8]) -> Result<Request, Reject> {
    let bad = || reject(400, "bad_request");
    let text = std::str::from_utf8(head).map_err(|_| bad())?;
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/1.") {
        return Err(bad());
    }
    let mut headers = Vec::new();
    for line in lines {
        // A bare CR or LF inside a line is a request-smuggling vector.
        if line.contains(['\r', '\n']) {
            return Err(bad());
        }
        let (name, value) = line.split_once(':').ok_or_else(bad)?;
        if name.is_empty() || name.contains([' ', '\t']) {
            return Err(bad());
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(Request { method: method.into(), target: target.into(), headers, body: Vec::new() })
}

/// Read one complete request. `gate` sees the framed head before any body byte is read, so an
/// unauthenticated peer cannot make the host buffer a body. `Err(None)` means the peer vanished or stalled:
/// drop without a reply.
fn read_request(
    stream: &mut TcpStream,
    deadline: Instant,
    gate: &dyn Fn(&Request) -> Result<(), Reject>,
) -> Result<Request, Option<Reject>> {
    let mut buf = Vec::new();
    let end = loop {
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            break end;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(Some(reject(431, "header_too_large")));
        }
        fill(stream, deadline, &mut buf).ok_or(None)?;
    };
    if end + 4 > MAX_HEAD_BYTES {
        return Err(Some(reject(431, "header_too_large")));
    }
    let mut request = parse_head(&buf[..end]).map_err(Some)?;

    if request.header("transfer-encoding").is_some() {
        return Err(Some(reject(411, "length_required")));
    }
    let mut lengths = request.headers.iter().filter(|(key, _)| key == "content-length").map(|(_, v)| v);
    let length = match lengths.next() {
        None if request.method == "POST" => return Err(Some(reject(411, "length_required"))),
        None => 0,
        Some(first) => {
            let valid = !first.is_empty() && first.bytes().all(|byte| byte.is_ascii_digit());
            if !valid || lengths.any(|other| other != first) {
                return Err(Some(reject(400, "bad_request")));
            }
            // A digit string too long for usize is certainly over the cap.
            first.parse::<usize>().unwrap_or(usize::MAX)
        }
    };
    if length > MAX_BODY_BYTES {
        return Err(Some(reject(413, "payload_too_large")));
    }
    gate(&request).map_err(Some)?;
    buf.drain(..end + 4);
    while buf.len() < length {
        fill(stream, deadline, &mut buf).ok_or(None)?;
    }
    buf.truncate(length);
    request.body = buf;
    Ok(request)
}

/// Gate before the body is read: route, Host/Origin, bearer token.
fn check(request: &Request, local_loopback: bool, bearer: Option<&str>) -> Result<(), Reject> {
    if request.target.split('?').next() != Some("/mcp") {
        return Err(reject(404, "not_found"));
    }
    if request.method != "POST" {
        return Err(Reject { status: 405, error: "method_not_allowed", headers: &[("Allow", "POST")] });
    }
    // DNS-rebinding and cross-site defence: a loopback listener only answers loopback Host names,
    // and a browser-supplied Origin must itself be loopback.
    if local_loopback && !request.header("host").and_then(authority_host).is_some_and(is_loopback_host) {
        return Err(reject(403, "forbidden_host"));
    }
    if let Some(origin) = request.header("origin") {
        if !authority_host(origin).is_some_and(is_loopback_host) {
            return Err(reject(403, "forbidden_origin"));
        }
    }
    if let Some(expected) = bearer {
        // The scheme name is case-insensitive (RFC 9110).
        let given = request.header("authorization").and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer")).map(|(_, token)| token.trim_start());
        if !given.is_some_and(|token| token_matches(token.as_bytes(), expected.as_bytes())) {
            return Err(Reject {
                status: 401,
                error: "invalid_token",
                headers: &[("WWW-Authenticate", "Bearer")],
            });
        }
    }
    Ok(())
}

/// Reply to a gated request: `(status, body)`; an empty body is sent as `204`.
fn dispatch(handler: &mut dyn FnMut(&Value) -> Option<Value>, body: &[u8]) -> (u16, String) {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        let error = json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "Parse error" },
        });
        return (200, error.to_string());
    };
    dispatch_message(handler, &message)
}

fn dispatch_message(handler: &mut dyn FnMut(&Value) -> Option<Value>, message: &Value) -> (u16, String) {
    let is_notification = message.is_object() && message.get("id").is_none_or(Value::is_null);
    match catch_unwind(AssertUnwindSafe(|| handler(message))) {
        Ok(Some(response)) if !is_notification => (200, response.to_string()),
        Ok(_) => (204, String::new()),
        Err(_) => (500, json!({ "error": "internal_error" }).to_string()),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &str, budget: Duration) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {status} {}\r\nConnection: close\r\n", reason(status));
    if status != 204 {
        out.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    // One slow reader must not hold the only server thread forever.
    // The budget covers the whole response: a reader that trickles must not reset it on every write.
    // The peer may already be gone; there is nobody to report that to.
    let write_deadline = Instant::now() + budget;
    let mut rest = out.as_bytes();
    while !rest.is_empty() {
        let left = write_deadline.saturating_duration_since(Instant::now());
        if left.is_zero() { return Err(io::Error::new(io::ErrorKind::TimedOut, "MCP response write timed out")); }
        stream.set_write_timeout(Some(left))?;
        let written = stream.write(rest)?;
        if written == 0 { return Err(io::Error::new(io::ErrorKind::WriteZero, "MCP response write stopped")); }
        rest = &rest[written..];
    }
    stream.flush()?;
    // Half-close, then swallow whatever the client still sends so closing does not reset the reply.
    stream.shutdown(Shutdown::Write)?;
    let deadline = (Instant::now() + DRAIN_BUDGET.min(budget)).min(write_deadline);
    let mut sink = Vec::new();
    while sink.len() < DRAIN_MAX_BYTES && fill(stream, deadline, &mut sink).is_some() {}
    Ok(())
}

/// Serve exactly one request on an accepted connection and close it.
/// `bearer`: the required token, or `None` for an open (loopback-only) endpoint.
pub fn handle_connection(stream: TcpStream, session: &mut Session, bearer: Option<&str>) -> io::Result<()> {
    handle_connection_within(stream, session, bearer, REQUEST_BUDGET)
}

/// Like [`handle_connection`], but every I/O phase (reading the request, writing the reply, draining the
/// peer) is bounded by `budget`, so a caller on a UI thread knows how long a stalled peer can hold it.
/// The session call itself is not bounded.
pub fn handle_connection_within(stream: TcpStream, session: &mut Session, bearer: Option<&str>, budget: Duration) -> io::Result<()> {
    handle_connection_with(stream, bearer, budget, &mut |message| RpcReply::plain(session.handle_mcp(message)))
}

/// Like [`handle_connection_within`], but each JSON-RPC message goes to `handler` instead of straight to a
/// session, so an editor can route it through its own guards (the answer `None` means a notification).
pub fn handle_connection_with(
    stream: TcpStream,
    bearer: Option<&str>,
    budget: Duration,
    handler: &mut dyn FnMut(&Value) -> RpcReply,
) -> io::Result<()> {
    handle_connection_deferred(stream, bearer, budget, &mut |message| HttpDispatch::Reply(handler(message))).map(|_| ())
}

pub enum HttpDispatch {
    Reply(RpcReply),
    Defer,
}

pub struct PendingReply {
    stream: TcpStream,
    message: Value,
    budget: Duration,
}

impl PendingReply {
    pub fn finish(mut self, handler: &mut dyn FnMut(&Value) -> RpcReply) -> io::Result<()> {
        match catch_unwind(AssertUnwindSafe(|| handler(&self.message))) {
            Ok(reply) => reply.write_http(&mut self.stream, false, self.budget),
            Err(_) => write_response(&mut self.stream, 500, &[], &json!({"error":"internal_error"}).to_string(), self.budget),
        }
    }
}

pub fn handle_connection_deferred(
    mut stream: TcpStream,
    bearer: Option<&str>,
    budget: Duration,
    handler: &mut dyn FnMut(&Value) -> HttpDispatch,
) -> io::Result<Option<PendingReply>> {
    let deadline = Instant::now() + budget;
    let local_loopback = stream.local_addr().map_or(true, |addr| addr.ip().is_loopback());
    let gate = |request: &Request| check(request, local_loopback, bearer);
    match read_request(&mut stream, deadline, &gate) {
        Ok(request) => {
            let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
                let (status, body) = dispatch(&mut |_| None, &request.body);
                write_response(&mut stream, status, &[], &body, budget)?;
                return Ok(None);
            };
            let notification = message.is_object() && message.get("id").is_none_or(Value::is_null);
            match catch_unwind(AssertUnwindSafe(|| handler(&message))) {
                Ok(HttpDispatch::Defer) if !notification => return Ok(Some(PendingReply { stream, message, budget })),
                Ok(HttpDispatch::Reply(reply)) => reply.write_http(&mut stream, notification, budget)?,
                Ok(_) => write_response(&mut stream, 204, &[], "", budget)?,
                Err(_) => write_response(&mut stream, 500, &[], &json!({"error":"internal_error"}).to_string(), budget)?,
            }
        }
        Err(None) => {}
        Err(Some(rejection)) => {
            let body = json!({ "error": rejection.error }).to_string();
            write_response(&mut stream, rejection.status, rejection.headers, &body, budget)?;
        }
    }
    Ok(None)
}

/// Accept one connection and serve it; the unit tests and [`serve`] build on this.
pub fn serve_one(listener: &TcpListener, session: &mut Session, bearer: Option<&str>) -> std::io::Result<()> {
    let (stream, _) = listener.accept()?;
    handle_connection(stream, session, bearer)
}

/// Serve connections forever, one at a time.
pub fn serve(listener: &TcpListener, session: &mut Session, bearer: Option<&str>) -> ! {
    loop {
        if let Err(error) = serve_one(listener, session, bearer) {
            eprintln!("mcp-http: accept failed: {error}");
            // Avoid a hot loop on a persistent accept failure such as fd exhaustion.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Where `soul2d-core --mcp-port` leaves the token it generated for `port` and where `njc` looks for it:
/// `mcp-<port>.token` in the config directory of the account.
pub fn token_file(port: u16) -> Result<std::path::PathBuf, String> {
    Ok(crate::settings::config_dir()?.join(format!("mcp-{port}.token")))
}

/// Writes the listening `address` and `token` to [`token_file`], readable by the owner only (on Windows, as private
/// as the config directory). A file left by an earlier run is removed and created anew, so a symlink in its place is
/// not written through.
pub fn write_token_file(address: std::net::SocketAddr, token: &str) -> Result<std::path::PathBuf, String> {
    let path = token_file(address.port())?;
    let fail = |error: std::io::Error| format!("cannot write {}: {error}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(fail(error)),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path).and_then(|mut file| file.write_all(format!("{address}\n{token}\n").as_bytes())).map_err(fail)?;
    Ok(path)
}

/// The address and token in [`token_file`] for `port`, if both are there and the token fits in a header. A client
/// sends the token only to that address: another account may listen on the same port of the other loopback address.
pub fn read_token_file(port: u16) -> Option<(std::net::SocketAddr, String)> {
    let text = std::fs::read_to_string(token_file(port).ok()?).ok()?;
    let mut lines = text.lines();
    let address: std::net::SocketAddr = lines.next()?.trim().parse().ok()?;
    let token = lines.next()?.trim();
    (address.port() == port && token.len() <= 256 && !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| (address, token.to_owned()))
}

/// A fresh 256-bit token as 64 lowercase hex characters, from the operating system's random source.
pub fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    getrandom::fill(bytes).map_err(|error| format!("cannot read random bytes: {error}"))
}

#[cfg(test)]
mod reply_delivery_tests {
    use super::*;
    use std::sync::mpsc::{sync_channel, TryRecvError};

    fn request() -> Value { json!({"jsonrpc":"2.0","id":"quit","method":"tools/call"}) }
    fn response() -> Value { json!({"jsonrpc":"2.0","id":"quit","result":{"isError":false,"accepted":true}}) }

    #[test]
    fn remote_quit_receipt_rejects_missing_wrong_and_error_responses() {
        for message in [None, Some(json!({})), Some(json!({"jsonrpc":"2.0","id":"other","result":{}})),
            Some(json!({"jsonrpc":"2.0","id":"quit","error":{"code":-32603}})),
            Some(json!({"jsonrpc":"2.0","id":"quit","result":{"isError":true}}))] {
            let (sender, receipt) = sync_channel(1);
            let mut bytes = Vec::new();
            RpcReply::tracked(&request(), message, sender).write_line(&mut bytes).unwrap();
            assert!(receipt.recv().unwrap().is_err());
        }
        for id in [Value::Null, json!(false), json!([])] {
            let mut request = request(); request["id"] = id.clone();
            let mut response = response(); response["id"] = id;
            let (sender, receipt) = sync_channel(1);
            RpcReply::tracked(&request, Some(response), sender).write_line(&mut Vec::new()).unwrap();
            assert!(receipt.recv().unwrap().is_err());
        }
    }

    struct FaultWriter { bytes: Vec<u8>, remaining: usize, flush_error: bool }
    impl Write for FaultWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 { return Err(io::ErrorKind::BrokenPipe.into()); }
            let count = bytes.len().min(self.remaining).min(3);
            self.bytes.extend_from_slice(&bytes[..count]); self.remaining -= count; Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.flush_error { Err(io::ErrorKind::BrokenPipe.into()) } else { Ok(()) }
        }
    }

    #[test]
    fn remote_quit_partial_and_flush_failures_never_acknowledge_delivery() {
        for (remaining, flush_error) in [(5, false), (usize::MAX, true), (usize::MAX, false)] {
            let (sender, receipt) = sync_channel(1);
            let mut writer = FaultWriter { bytes: Vec::new(), remaining, flush_error };
            let result = RpcReply::tracked(&request(), Some(response()), sender).write_line(&mut writer);
            let expected = !flush_error && remaining == usize::MAX;
            assert_eq!(result.is_ok(), expected);
            assert_eq!(receipt.recv().unwrap().is_ok(), expected);
            if expected { assert_eq!(writer.bytes, format!("{}\n", response()).as_bytes()); }
        }
    }

    #[test]
    fn remote_quit_receipt_waits_for_the_actual_flush_and_newline() {
        struct Gated { bytes: Vec<u8>, entered: SyncSender<Vec<u8>>, release: std::sync::mpsc::Receiver<()> }
        impl Write for Gated {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.bytes.extend_from_slice(bytes); Ok(bytes.len()) }
            fn flush(&mut self) -> io::Result<()> {
                self.entered.send(self.bytes.clone()).unwrap(); self.release.recv().unwrap(); Ok(())
            }
        }
        let (sender, receipt) = sync_channel(1);
        let (entered, waiting) = sync_channel(1);
        let (release, gate) = sync_channel(1);
        let reply = RpcReply::tracked(&request(), Some(response()), sender);
        let worker = std::thread::spawn(move || reply.write_line(&mut Gated { bytes: Vec::new(), entered, release: gate }));
        assert_eq!(waiting.recv_timeout(Duration::from_secs(2)).unwrap(), format!("{}\n", response()).as_bytes());
        assert!(matches!(receipt.try_recv(), Err(TryRecvError::Empty)));
        release.send(()).unwrap(); worker.join().unwrap().unwrap();
        assert_eq!(receipt.recv_timeout(Duration::from_secs(2)).unwrap(), Ok(()));
    }

    #[test]
    fn remote_quit_http_fallbacks_cannot_report_an_admitted_reply() {
        for behavior in ["panic", "none", "error", "success", "deferred"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = request().to_string();
            let client = std::thread::spawn(move || {
                let mut socket = TcpStream::connect(address).unwrap();
                write!(socket, "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
                socket.shutdown(Shutdown::Write).unwrap();
                let mut bytes = String::new(); socket.read_to_string(&mut bytes).unwrap(); bytes
            });
            let (stream, _) = listener.accept().unwrap();
            let (sender, receipt) = sync_channel(1);
            let mut sender = Some(sender);
            let pending = handle_connection_deferred(stream, None, Duration::from_secs(2), &mut |message| {
                if behavior == "deferred" { return HttpDispatch::Defer; }
                let sender = sender.take().unwrap();
                if behavior == "panic" { drop(sender); panic!("armed operation unwinds before producing its envelope"); }
                let response = match behavior {
                    "none" => None,
                    "error" => Some(json!({"jsonrpc":"2.0","id":message["id"],"result":{"isError":true}})),
                    _ => Some(response()),
                };
                HttpDispatch::Reply(RpcReply::tracked(message, response, sender))
            }).unwrap();
            if let Some(pending) = pending {
                pending.finish(&mut |message| RpcReply::tracked(message, Some(response()), sender.take().unwrap())).unwrap();
            }
            let bytes = client.join().unwrap();
            let successful = matches!(behavior, "success" | "deferred");
            assert_eq!(receipt.recv().is_ok_and(|result| result.is_ok()), successful);
            if successful {
                let (head, body) = bytes.split_once("\r\n\r\n").unwrap();
                assert!(head.starts_with("HTTP/1.1 200"));
                assert!(head.contains(&format!("Content-Length: {}", body.len())));
                assert_eq!(serde_json::from_str::<Value>(body).unwrap(), response());
            }
        }
    }
}
