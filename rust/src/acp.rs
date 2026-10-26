//! Agent Client Protocol (ACP) client: JSON-RPC over an agent process's stdio.
//!
//! Ported from `soul2d/api/acp/{protocol,client}.d` and the non-GUI half of `panels/agent.d`.
//! Layers, from the wire up:
//! - [`FrameDecoder`]: bounded framing. Newline-delimited JSON (what D sends and reads) and
//!   `Content-Length` frames (what `examples/acp-echo.rs` sends) are both accepted on input.
//! - [`Client`]: owns a writer, a reader thread feeding a bounded queue, an optional child process and a
//!   stderr ring. Nothing blocks: the editor calls [`Client::poll`] once per frame and receives
//!   [`Incoming`] items.
//! - [`Conversation`]: pure reducer that folds `session/update` items and permission requests into a chat log,
//!   a plan, a usage line and a permission queue. [`Client`] owns one and keeps it current.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::settings::Settings;

pub const PROTOCOL_VERSION: i64 = 1;
/// Largest accepted or sent JSON-RPC message body, and the longest accepted line.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Decoded events buffered between the reader thread and the editor; the reader blocks (and so
/// back-pressures the child) when the editor stops polling.
pub const EVENT_QUEUE_LEN: usize = 1024;
/// Newest stderr bytes kept.
pub const STDERR_RING_BYTES: usize = 64 * 1024;
/// Longest `Content-Length` header block accepted.
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Conversation line cap; oldest lines are evicted first.
pub const MAX_LINES: usize = 4000;
/// How long `shutdown` waits for the agent to exit after its stdin closes, before killing it.
const EXIT_GRACE: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------------------------
// Framing

/// Incremental decoder for the agent's stdout. Memory is bounded by `max` plus one header block:
/// oversized lines and bodies are skipped without being buffered.
pub struct FrameDecoder {
    buf: Vec<u8>,
    max: usize,
    /// Dropping the rest of an over-long line.
    skipping_line: bool,
    /// Body bytes of an oversized `Content-Length` frame still to be dropped.
    skip_bytes: usize,
    /// `buf[..scanned]` is known to hold no newline, so a long line is not rescanned on every push.
    scanned: usize,
}

impl FrameDecoder {
    pub fn new(max: usize) -> Self {
        Self { buf: Vec::new(), max, skipping_line: false, skip_bytes: 0, scanned: 0 }
    }

    /// Feed bytes; returns every message completed by them, or a description of each rejected frame.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Value, String>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.skip_bytes > 0 {
                let n = self.skip_bytes.min(self.buf.len());
                self.buf.drain(..n);
                self.skip_bytes -= n;
                if self.skip_bytes > 0 {
                    break;
                }
                continue;
            }
            if self.skipping_line {
                match self.buf.iter().position(|&b| b == b'\n') {
                    Some(nl) => {
                        self.buf.drain(..=nl);
                        self.skipping_line = false;
                        continue;
                    }
                    None => {
                        self.buf.clear();
                        break;
                    }
                }
            }
            let from = self.scanned.min(self.buf.len());
            let Some(rel) = self.buf[from..].iter().position(|&b| b == b'\n') else {
                self.scanned = self.buf.len();
                if self.buf.len() > self.max {
                    out.push(Err(format!("frame exceeds {} bytes", self.max)));
                    self.buf.clear();
                    self.scanned = 0;
                    self.skipping_line = true;
                }
                break;
            };
            let nl = from + rel;
            self.scanned = 0;
            if nl > self.max {
                out.push(Err(format!("frame exceeds {} bytes", self.max)));
                self.buf.drain(..=nl);
                continue;
            }
            let line = trim_line(&self.buf[..nl]);
            if let Some(length) = content_length(line) {
                match self.content_frame(nl + 1, length) {
                    Frame::Wait => break,
                    Frame::Done(result) => out.push(result),
                    Frame::Skipped(error) => out.push(Err(error)),
                }
                continue;
            }
            let line = match line.iter().position(|&b| b == 0) {
                Some(nul) => &line[..nul],
                None => line,
            };
            if !line.is_empty() {
                out.push(parse_message(line));
            }
            self.buf.drain(..=nl);
        }
        out
    }

    /// `header_start` is the offset just after the `Content-Length` line.
    fn content_frame(&mut self, header_start: usize, length: Result<usize, String>) -> Frame {
        let mut pos = header_start;
        let body_start = loop {
            let Some(rel) = self.buf[pos..].iter().position(|&b| b == b'\n') else {
                if self.buf.len() - header_start > MAX_HEADER_BYTES {
                    self.buf.clear();
                    self.skipping_line = true;
                    return Frame::Skipped("Content-Length header block is too long".to_string());
                }
                return Frame::Wait;
            };
            let blank = trim_line(&self.buf[pos..pos + rel]).is_empty();
            pos += rel + 1;
            if blank {
                break pos;
            }
        };
        let length = match length {
            Ok(length) => length,
            Err(error) => {
                self.buf.drain(..body_start);
                return Frame::Skipped(error);
            }
        };
        if length > self.max {
            self.buf.drain(..body_start);
            self.skip_bytes = length;
            return Frame::Skipped(format!("frame of {length} bytes exceeds {} bytes", self.max));
        }
        if self.buf.len() < body_start + length {
            return Frame::Wait;
        }
        let result = parse_message(&self.buf[body_start..body_start + length]);
        self.buf.drain(..body_start + length);
        Frame::Done(result)
    }
}

enum Frame {
    Wait,
    Done(Result<Value, String>),
    Skipped(String),
}

fn trim_line(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `Some(length or parse error)` when the line is a `Content-Length` header.
fn content_length(line: &[u8]) -> Option<Result<usize, String>> {
    const NAME: &[u8] = b"content-length:";
    if line.len() < NAME.len() || !line[..NAME.len()].eq_ignore_ascii_case(NAME) {
        return None;
    }
    let value = String::from_utf8_lossy(&line[NAME.len()..]);
    Some(value.trim().parse::<usize>().map_err(|_| format!("invalid Content-Length: {}", value.trim())))
}

fn parse_message(bytes: &[u8]) -> Result<Value, String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err("JSON-RPC message is not an object".to_string()),
        Err(error) => Err(format!("JSON parse failed: {error}")),
    }
}

/// What the reader thread reports.
#[derive(Debug)]
enum Event {
    Frame(Value, Instant),
    Malformed(String),
    Disconnected(String),
}

enum ReaderIntake {
    Open(u64),
    Closed,
}

fn close_intake(intake: &Mutex<ReaderIntake>) {
    if let Ok(mut state) = intake.lock() { *state = ReaderIntake::Closed; }
}

fn reader_loop(mut reader: impl Read, tx: SyncSender<Event>, intake: Arc<Mutex<ReaderIntake>>) {
    let mut decoder = FrameDecoder::new(MAX_FRAME_BYTES);
    let mut chunk = [0u8; 4096];
    let reason = loop {
        match reader.read(&mut chunk) {
            Ok(0) => break "end of stream".to_string(),
            Ok(n) => {
                let (items, received_at) = {
                    let Ok(mut state) = intake.lock() else { break "reader intake failed".into() };
                    if matches!(*state, ReaderIntake::Closed) { break "reader intake closed".into(); }
                    let received_at = Instant::now();
                    let items = decoder.push(&chunk[..n]);
                    // Count the whole decoded batch before blocking enqueue or the release cutoff.
                    for item in &items {
                        if item.as_ref().ok().and_then(|message| str_of(message, "method")).is_some_and(is_permission_method) {
                            *state = match *state {
                                ReaderIntake::Open(epoch) => epoch.checked_add(1).filter(|epoch| *epoch < u64::MAX)
                                    .map(ReaderIntake::Open).unwrap_or(ReaderIntake::Closed),
                                ReaderIntake::Closed => ReaderIntake::Closed,
                            };
                        }
                    }
                    if matches!(*state, ReaderIntake::Closed) { break "permission history exhausted".into(); }
                    (items, received_at)
                };
                for item in items {
                    let event = match item {
                        Ok(value) => Event::Frame(value, received_at),
                        Err(error) => Event::Malformed(error),
                    };
                    if tx.send(event).is_err() {
                        close_intake(&intake);
                        return;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => break format!("read failed: {error}"),
        }
    };
    close_intake(&intake);
    let _ = tx.send(Event::Disconnected(reason));
}

/// Newest [`STDERR_RING_BYTES`] bytes of the agent's stderr.
#[derive(Clone, Default)]
pub struct StderrRing(Arc<Mutex<VecDeque<u8>>>);

impl StderrRing {
    fn push(&self, bytes: &[u8]) {
        let mut ring = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ring.extend(bytes);
        let excess = ring.len().saturating_sub(STDERR_RING_BYTES);
        ring.drain(..excess);
    }

    pub fn text(&self) -> String {
        let ring = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let bytes: Vec<u8> = ring.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

// ---------------------------------------------------------------------------------------------
// Settings boundary

/// Split a command line into an argument vector. Handles single and double quotes, no escapes.
/// The result is passed to `Command` directly, never to a shell.
pub fn parse_command_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match (quote, ch) {
            (None, ' ' | '\t') => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (_, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The agent process to launch: program plus arguments, and an optional working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    pub command: Vec<String>,
    pub workdir: Option<PathBuf>,
}

impl LaunchSpec {
    /// `ACP.Command` and `ACP.Workdir`, falling back to `ACP.Workingdir`. No agent ships with Soul2D, so the
    /// command has no default.
    pub fn from_settings(settings: &Settings) -> Result<Self, String> {
        let line = settings.get_or("ACP.Command", String::new())?;
        let command = parse_command_line(&line);
        if command.is_empty() {
            return Err("ACP.Command is not set; enter the command that starts an ACP agent".to_string());
        }
        let fallback = settings.get::<String>("ACP.Workingdir")?;
        let workdir = settings.get_or("ACP.Workdir", fallback)?;
        Ok(Self { command, workdir: (!workdir.is_empty()).then(|| PathBuf::from(workdir)) })
    }
}

/// The embedded MCP HTTP server to offer the agent in `session/new`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpHttp {
    pub name: String,
    pub url: String,
    pub headers: Value,
}

impl McpHttp {
    /// `url` is the live endpoint (`http://host:port/mcp`, IPv6 hosts bracketed); `MCP.Name` defaults to
    /// "soul2d" and `MCP.Headers` (a JSON array in a string, default "[]") falls back to `[]` when invalid.
    pub fn from_settings(settings: &Settings, url: &str) -> Result<Self, String> {
        let name = settings.get_or("MCP.Name", "soul2d".to_string())?;
        let raw = settings.get_or("MCP.Headers", "[]".to_string())?;
        let headers = serde_json::from_str::<Value>(&raw).ok().filter(Value::is_array).unwrap_or_else(|| json!([]));
        Ok(Self { name, url: url.to_string(), headers })
    }
}

/// Lexically normalized absolute path (no symlink resolution), as `session/new` requires.
fn normalize_cwd(cwd: Option<&Path>) -> String {
    let base = match cwd {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => std::env::current_dir().unwrap_or_default().join(path),
        None => std::env::current_dir().unwrap_or_default(),
    };
    let mut out = PathBuf::new();
    for part in base.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------------------------
// JSON-RPC helpers

/// A response (not a request) that carries `id` and has `result` or `error`.
pub fn response_matches(message: &Value, id: &Value) -> bool {
    is_response(message) && message.get("id") == Some(id)
}

fn is_response(message: &Value) -> bool {
    message.get("id").is_some()
        && message.get("method").is_none()
        && (message.get("result").is_some() || message.get("error").is_some())
}

fn rpc_error_message(error: &Value) -> String {
    if let Some(text) = error.as_str() {
        return text.to_string();
    }
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return match error.get("code") {
            Some(code) => format!("{message} (code {code})"),
            None => message.to_string(),
        };
    }
    error.to_string()
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

// ---------------------------------------------------------------------------------------------
// Conversation reducer

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoleKind {
    #[default]
    Other,
    You,
    Tool,
    AssistantFinal,
    AssistantThinking,
    Error,
}

/// Consecutive lines from one speaker. Tool blocks are keyed by `tool_id` and hold the merged payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub role: String,
    pub kind: RoleKind,
    pub title: String,
    pub lines: Vec<String>,
    pub payload_merged: Option<Value>,
    pub status: String,
    pub tool_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub lines: Vec<String>,
    pub payload: Option<Value>,
    pub status: String,
}

/// A queued `session/request_permission`. `id` is the JSON-RPC id exactly as the agent sent it (a number
/// stays a number); `Null` means the agent sent none and the request cannot be answered.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionRequest {
    pub id: Value,
    pub reason: String,
    pub params: Value,
    pub generation: u64,
}

/// Result of [`Conversation::apply_permission_request`].
#[derive(Clone, Debug, PartialEq)]
pub enum PermissionIntake {
    /// Waiting for the user.
    Queued(PermissionRequest),
    /// Arrived while cancelling; this `cancelled` response must be sent at once.
    AutoCancelled(Value),
    /// Same id already outstanding, or id-less while cancelling.
    Ignored,
}

#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub blocks: Vec<Block>,
    pub plan: Option<Plan>,
    pub usage_text: String,
    pub usage_payload: Option<Value>,
    pub permissions: Vec<PermissionRequest>,
    tool_index: HashMap<String, usize>,
    /// Role of the last streamed chunk while it may still continue; cleared by `flush_pending`.
    pending: Option<(String, RoleKind)>,
    /// The last streamed chunk ended with a newline, so the next one starts a new line.
    pending_newline: bool,
    /// Outstanding permission ids (JSON text) mapped to their generation.
    outstanding: HashMap<String, u64>,
    next_generation: u64,
    cancelling: bool,
}

const PERMISSION_METHODS: [&str; 4] =
    ["session/request_permission", "request_permission", "request_permissions", "request_prermissions"];

/// True for the method names the agent may use for a permission request (D tolerates legacy spellings).
pub fn is_permission_method(method: &str) -> bool {
    PERMISSION_METHODS.contains(&method)
}

impl Conversation {
    pub fn cancelling(&self) -> bool {
        self.cancelling
    }

    /// Show the user's prompt in the log.
    pub fn record_user(&mut self, text: &str) {
        self.append_stream("You", text, RoleKind::You);
    }

    /// Show the text the editor added to the user's prompt, so the user sees what was sent on their behalf.
    pub fn record_briefing(&mut self, text: &str) {
        self.append_stream("Soul2D", text, RoleKind::Other);
    }

    /// Fold one `session/update` payload into the log.
    pub fn apply_update(&mut self, upd: &Value) {
        let kind = str_of(upd, "sessionUpdate").unwrap_or("session/update");
        if kind == "usage_update" {
            self.update_usage(upd);
            return;
        }
        let tool_id = str_of(upd, "toolCallId").unwrap_or("");
        let role = role_from_kind(kind, tool_id);
        let status = str_of(upd, "status").unwrap_or("");
        let title = str_of(upd, "title").unwrap_or("");
        let mut text = extract_text(upd);

        if kind == "tool_call" || kind == "tool_call_update" {
            if text.is_empty() {
                text = str_of(upd, "message").unwrap_or("").to_string();
            }
            if text.is_empty() {
                text = "tool update".to_string();
            }
            if !title.is_empty() {
                text = format!("{title} {text}");
            }
            if !status.is_empty() {
                text = format!("{text} (status: {status})");
            }
            if !tool_id.is_empty() {
                text = format!("[{tool_id}] {text}");
            }
            self.update_tool(&role, title, text, upd, kind == "tool_call", status, tool_id);
            return;
        }
        if kind == "plan" {
            self.update_plan(&text, upd, status);
            return;
        }
        if text.is_empty() {
            text = fallback_text(upd);
        }
        self.append_stream(&role, &text, role_kind(kind));
    }

    /// End the streamed-line continuation (called on tool, plan and turn boundaries).
    pub fn flush_pending(&mut self) {
        self.pending = None;
        self.pending_newline = false;
    }

    /// Queue a permission request message from the agent (or answer it `cancelled` while cancelling).
    pub fn apply_permission_request(&mut self, message: &Value) -> PermissionIntake {
        let params = message.get("params");
        let params_obj = params.filter(|p| p.is_object());
        let id = message.get("id").cloned().or_else(|| {
            let p = params_obj?;
            ["id", "requestId", "request_id"].iter().find_map(|key| p.get(*key).cloned())
        });
        let reason = params_obj.and_then(|p| str_of(p, "reason")).unwrap_or("").to_string();
        let params = params.cloned().unwrap_or(Value::Null);
        let id_text = id.as_ref().map(Value::to_string).unwrap_or_default();
        // D mirrors the tool call before this check; a repeated id must not rewrite the visible tool block.
        if !id.as_ref().is_some_and(|id| self.outstanding.contains_key(&id.to_string())) {
            self.show_permission_tool_call(&params, &id_text, &reason);
        }

        let Some(id) = id.filter(|id| !id.is_null()) else {
            if self.cancelling {
                return PermissionIntake::Ignored;
            }
            let request = PermissionRequest { id: Value::Null, reason, params, generation: 0 };
            self.permissions.push(request.clone());
            return PermissionIntake::Queued(request);
        };
        if self.outstanding.contains_key(&id.to_string()) {
            return PermissionIntake::Ignored;
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        if self.cancelling {
            return PermissionIntake::AutoCancelled(permission_response(&id, None));
        }
        self.outstanding.insert(id.to_string(), generation);
        let request = PermissionRequest { id, reason, params, generation };
        self.permissions.push(request.clone());
        PermissionIntake::Queued(request)
    }

    /// Answer a queued request: `Some(option_id)` selects it, `None` or `Some("")` denies (cancelled).
    /// Returns the response to send, or `None` when the request is gone, stale (wrong generation) or the
    /// turn is being cancelled.
    pub fn answer(&mut self, id: &Value, generation: u64, option: Option<&str>) -> Option<Value> {
        let key = id.to_string();
        if self.cancelling || self.outstanding.get(&key) != Some(&generation) {
            return None;
        }
        self.outstanding.remove(&key);
        self.permissions.retain(|p| !(p.id == *id && p.generation == generation));
        Some(permission_response(id, option.filter(|o| !o.is_empty())))
    }

    /// Start cancelling the turn: every outstanding request is answered `cancelled`, and requests arriving
    /// later are too. Returns the responses to send. A no-op while already cancelling.
    pub fn cancel(&mut self) -> Vec<Value> {
        if self.cancelling {
            return Vec::new();
        }
        self.cancelling = true;
        let mut requests: Vec<PermissionRequest> =
            self.permissions.iter().filter(|p| self.outstanding.contains_key(&p.id.to_string())).cloned().collect();
        requests.sort_by_key(|p| p.generation);
        self.permissions.retain(|p| !self.outstanding.contains_key(&p.id.to_string()));
        self.outstanding.clear();
        requests.iter().map(|p| permission_response(&p.id, None)).collect()
    }

    /// The prompt finished or the agent went away: forget permissions and the cancelling flag.
    pub fn end_turn(&mut self) {
        self.flush_pending();
        self.outstanding.clear();
        self.permissions.clear();
        self.cancelling = false;
    }

    /// Start over (new session).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    // -- log building ------------------------------------------------------------------------

    fn ensure_block(&mut self, role: &str, kind: RoleKind) {
        if self.blocks.last().map_or(true, |b| b.role != role || b.kind != kind) {
            self.blocks.push(Block { role: role.to_string(), kind, ..Block::default() });
        }
    }

    fn push_line(&mut self, role: &str, kind: RoleKind, line: &str) {
        self.ensure_block(role, kind);
        if let Some(block) = self.blocks.last_mut() {
            block.lines.push(line.to_string());
        }
        let total: usize = self.blocks.iter().map(|b| b.lines.len()).sum();
        if total > MAX_LINES {
            self.evict(total - MAX_LINES);
        }
    }

    fn evict(&mut self, mut to_drop: usize) {
        while to_drop > 0 && !self.blocks.is_empty() {
            let n = self.blocks[0].lines.len();
            if n <= to_drop {
                to_drop -= n;
                self.blocks.remove(0);
            } else {
                self.blocks[0].lines.drain(..to_drop);
                to_drop = 0;
            }
        }
        self.tool_index = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.tool_id.is_empty())
            .map(|(i, b)| (b.tool_id.clone(), i))
            .collect();
    }

    fn append_to_last_line(&mut self, role: &str, kind: RoleKind, fragment: &str) {
        self.ensure_block(role, kind);
        if let Some(block) = self.blocks.last_mut() {
            match block.lines.last_mut() {
                Some(last) => last.push_str(fragment),
                None => block.lines.push(fragment.to_string()),
            }
        }
    }

    /// Append a streamed chunk: its first line continues the previous line of the same speaker unless the
    /// previous chunk ended with a newline.
    fn append_stream(&mut self, role: &str, text: &str, kind: RoleKind) {
        if self.pending.as_ref().is_some_and(|(r, k)| r != role || *k != kind) {
            self.flush_pending();
        }
        self.pending = Some((role.to_string(), kind));
        let mut start_new_line = self.pending_newline;
        self.pending_newline = false;
        let mut start = 0;
        loop {
            let Some(rel) = text[start..].find('\n') else {
                let rest = &text[start..];
                if !rest.is_empty() {
                    if start_new_line || start > 0 {
                        self.push_line(role, kind, rest);
                    } else {
                        self.append_to_last_line(role, kind, rest);
                    }
                }
                self.pending = None;
                return;
            };
            let end = start + rel;
            let line = &text[start..end];
            let extends_last = !start_new_line
                && start == 0
                && self.blocks.last().is_some_and(|b| b.role == role && b.kind == kind && !b.lines.is_empty());
            if extends_last {
                self.append_to_last_line(role, kind, line);
            } else {
                self.push_line(role, kind, line);
            }
            start = end + 1;
            start_new_line = false;
            if start == text.len() {
                self.pending_newline = true;
                return;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_tool(&mut self, role: &str, title: &str, text: String, payload: &Value, initial: bool, status: &str,
                   tool_id: &str) {
        self.flush_pending();
        let index = if tool_id.is_empty() {
            self.blocks.iter().rposition(|b| b.role == role)
        } else {
            let found = self.tool_index.get(tool_id).copied().filter(|&i| i < self.blocks.len());
            if found.is_none() {
                self.tool_index.remove(tool_id);
            }
            found
        };
        let Some(index) = index else {
            self.blocks.push(Block {
                role: role.to_string(),
                kind: RoleKind::Tool,
                title: title.to_string(),
                lines: if text.is_empty() { Vec::new() } else { vec![text] },
                payload_merged: Some(payload.clone()),
                status: status.to_string(),
                tool_id: tool_id.to_string(),
            });
            if !tool_id.is_empty() {
                self.tool_index.insert(tool_id.to_string(), self.blocks.len() - 1);
            }
            return;
        };
        let block = &mut self.blocks[index];
        block.kind = RoleKind::Tool;
        if !title.is_empty() {
            block.title = title.to_string();
        }
        if !text.is_empty() {
            match block.lines.last_mut() {
                None => block.lines.push(text),
                // A permission request can create the tool block before its tool_call arrives; replace
                // that placeholder instead of showing the tool twice.
                Some(last) if initial => *last = text,
                Some(last) => last.push_str(&text),
            }
        }
        block.payload_merged = Some(match block.payload_merged.take() {
            Some(old) => merge_json(&old, payload),
            None => payload.clone(),
        });
        if !status.is_empty() {
            block.status = status.to_string();
        }
    }

    fn update_plan(&mut self, text: &str, payload: &Value, status: &str) {
        self.flush_pending();
        let plan = self.plan.get_or_insert_with(Plan::default);
        match payload.get("entries") {
            Some(Value::Array(entries)) => plan.lines = plan_entry_lines(entries),
            Some(_) => {}
            None if !text.is_empty() => plan.lines = text.split('\n').map(str::to_string).collect(),
            None => {}
        }
        plan.payload = Some(match plan.payload.take() {
            Some(old) => merge_json(&old, payload),
            None => payload.clone(),
        });
        if !status.is_empty() {
            plan.status = status.to_string();
        }
    }

    fn update_usage(&mut self, upd: &Value) {
        self.usage_payload = Some(upd.clone());
        let src = upd.get("usage").filter(|u| u.is_object()).unwrap_or(upd);
        let groups: [(&str, &[&str]); 4] = [
            ("in", &["inputTokens", "input_tokens", "promptTokens", "prompt_tokens"]),
            ("out", &["outputTokens", "output_tokens", "completionTokens", "completion_tokens"]),
            ("total", &["totalTokens", "total_tokens"]),
            (
                "cached",
                &["cachedInputTokens", "cached_input_tokens", "cacheReadInputTokens", "cache_read_input_tokens"],
            ),
        ];
        let parts: Vec<String> = groups
            .iter()
            .filter_map(|(label, keys)| {
                let value = keys.iter().map(|key| scalar_text(src, key)).find(|v| !v.is_empty())?;
                Some(format!("{label} {value}"))
            })
            .collect();
        self.usage_text = if parts.is_empty() { upd.to_string() } else { parts.join(", ") };
    }

    /// Mirror the tool call of a permission request into the log, as a pending `tool_call`.
    fn show_permission_tool_call(&mut self, params: &Value, id_text: &str, reason: &str) {
        let Some(Value::Object(tool_call)) = params.get("toolCall") else { return };
        let mut upd = tool_call.clone();
        upd.insert("sessionUpdate".to_string(), json!("tool_call"));
        if !upd.get("status").is_some_and(Value::is_string) {
            upd.insert("status".to_string(), json!("pending"));
        }
        if !upd.get("toolCallId").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
            let tool_id = if id_text.is_empty() { "permission".to_string() } else { format!("permission:{id_text}") };
            upd.insert("toolCallId".to_string(), json!(tool_id));
        }
        if !upd.get("title").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
            let title = if reason.is_empty() { "Permission requested" } else { reason };
            upd.insert("title".to_string(), json!(title));
        }
        self.apply_update(&Value::Object(upd));
    }
}

fn permission_response(id: &Value, option: Option<&str>) -> Value {
    let outcome = match option {
        Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
        None => json!({"outcome": "cancelled"}),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}})
}

fn role_from_kind(kind: &str, tool_id: &str) -> String {
    match kind {
        "agent_message_chunk" => "Assistant".to_string(),
        "agent_thought_chunk" => "Assistant (thinking)".to_string(),
        "user_message_chunk" => "You (echo)".to_string(),
        "plan" => "Plan".to_string(),
        "error" => "Error".to_string(),
        "tool_call" | "tool_call_update" if tool_id.is_empty() => "Tool".to_string(),
        "tool_call" | "tool_call_update" => format!("Tool#{tool_id}"),
        "available_commands_update" => "Command list".to_string(),
        "current_mode_update" => "Mode".to_string(),
        other => other.to_string(),
    }
}

fn role_kind(kind: &str) -> RoleKind {
    match kind {
        "user_message_chunk" => RoleKind::You,
        "tool_call" | "tool_call_update" => RoleKind::Tool,
        "agent_message_chunk" => RoleKind::AssistantFinal,
        "agent_thought_chunk" => RoleKind::AssistantThinking,
        "error" => RoleKind::Error,
        _ => RoleKind::Other,
    }
}

/// Text of `content` (one block, or an array of blocks whose annotation titles are appended).
fn extract_text(upd: &Value) -> String {
    match upd.get("content") {
        Some(Value::Object(content)) => content.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
        Some(Value::Array(items)) => {
            let mut acc = String::new();
            for item in items {
                let Some(text) = str_of(item, "text") else { continue };
                acc.push_str(text);
                for annotation in item.get("annotations").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(title) = str_of(annotation, "title") {
                        acc.push_str(&format!(" ({title})"));
                    }
                }
            }
            acc
        }
        _ => String::new(),
    }
}

fn fallback_text(upd: &Value) -> String {
    if let Some(message) = str_of(upd, "message") {
        return message.to_string();
    }
    match upd.get("rawOutput") {
        Some(raw) => raw.to_string(),
        None => upd.to_string(),
    }
}

/// Strings and numbers/booleans as text; anything else as empty.
fn scalar_text(obj: &Value, key: &str) -> String {
    match obj.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => v.to_string(),
        _ => String::new(),
    }
}

fn plan_entry_lines(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| {
            let content = str_of(entry, "content")?;
            let priority = str_of(entry, "priority").filter(|p| ["high", "medium", "low"].contains(p))?;
            let status = str_of(entry, "status").filter(|s| ["pending", "in_progress", "completed"].contains(s))?;
            Some(format!("[{priority}] {content} ({status})"))
        })
        .collect()
}

/// Merge a tool-call payload into the previous one. `contents` arrays concatenate (promoting scalars);
/// `content` concatenates text of two text blocks and otherwise collects into an array; every other key is
/// replaced. Non-objects are replaced outright.
pub fn merge_json(dst: &Value, src: &Value) -> Value {
    let (Value::Object(dst), Value::Object(src)) = (dst, src) else { return src.clone() };
    let mut res: Map<String, Value> = dst.clone();
    for (key, value) in src {
        let merged = match (key.as_str(), res.get(key)) {
            ("contents", Some(old)) => {
                let mut items = match old {
                    Value::Array(items) => items.clone(),
                    other => vec![other.clone()],
                };
                match value {
                    Value::Array(more) => items.extend(more.iter().cloned()),
                    other => items.push(other.clone()),
                }
                Value::Array(items)
            }
            ("content", Some(old)) => match (str_of(old, "text"), str_of(value, "text")) {
                (Some(a), Some(b)) if old.is_object() && value.is_object() => {
                    let mut merged = old.clone();
                    merged["text"] = json!(format!("{a}{b}"));
                    merged
                }
                _ => {
                    let mut items = match old {
                        Value::Array(items) => items.clone(),
                        other => vec![other.clone()],
                    };
                    items.push(value.clone());
                    Value::Array(items)
                }
            },
            _ => value.clone(),
        };
        res.insert(key.clone(), merged);
    }
    Value::Object(res)
}

// ---------------------------------------------------------------------------------------------
// Client

/// What [`Client::poll`] reports. Updates and permission requests have already been folded into
/// [`Client::conversation`]; they are repeated here so a UI can react (status line, repaint).
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Initialized,
    InitFailed(String),
    SessionReady(String),
    SessionFailed(String),
    /// A `session/update` payload.
    Update(Value),
    /// A request waiting for [`Client::answer_permission`].
    Permission(PermissionRequest),
    /// A permission request answered `cancelled` on arrival because the turn is being cancelled.
    PermissionCancelled(Value),
    PromptFinished(PromptCompletion),
    Ping(Result<(), String>),
    /// A message that matched nothing: a response with an unknown id, or an unhandled request/notification.
    Other(Value),
    /// A frame that was rejected (bad JSON, too large); the stream continues.
    Malformed(String),
    /// A failure while sending a follow-up message (queued prompt, permission cancellation).
    Error(String),
    /// The agent's stdout closed. Always the last item.
    Disconnected(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptId(u64);

#[derive(Clone, Debug, PartialEq)]
pub enum PromptOutcome {
    Completed,
    Cancelled,
    Stopped(Option<String>),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptCompletion {
    pub id: PromptId,
    pub outcome: PromptOutcome,
    pub received_at: Instant,
}

enum PromptState {
    Idle,
    Queued { id: PromptId, text: String, briefing: Option<String> },
    Running { id: PromptId, cancelled: bool },
    Finished(PromptCompletion),
}

impl PromptState {
    fn active_id(&self) -> Option<PromptId> {
        match self {
            Self::Queued { id, .. } | Self::Running { id, .. } => Some(*id),
            _ => None,
        }
    }
}

enum Pending {
    Initialize,
    NewSession,
    Prompt(PromptId),
    Ping,
}

enum InitState {
    Idle,
    Pending,
    Ready(Value),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// One JSON object per line (what agents such as codex-acp expect).
    Line,
    ContentLength,
}

struct Process {
    child: Child,
    threads: Vec<JoinHandle<()>>,
}

pub struct Client<W: Write> {
    writer: Option<W>,
    rx: Option<Receiver<Event>>,
    reader_thread: Option<JoinHandle<()>>,
    process: Option<Process>,
    stderr: StderrRing,
    send_mode: SendMode,
    cwd: String,
    mcp: Option<McpHttp>,
    next_id: u64,
    pending: HashMap<String, Pending>,
    init: InitState,
    supports_mcp_http: bool,
    session_id: Option<String>,
    prompt: PromptState,
    next_prompt: u64,
    intake: Arc<Mutex<ReaderIntake>>,
    #[cfg(test)]
    fail_shutdown: bool,
    disconnected: bool,
    /// The log the agent's updates are folded into.
    pub conversation: Conversation,
}

impl<W: Write> Client<W> {
    /// Drive an existing connection: `reader` is the agent's output, `writer` its input. A reader thread
    /// is started and runs until `reader` ends.
    pub fn new(reader: impl Read + Send + 'static, writer: W) -> Self {
        let (tx, rx) = sync_channel(EVENT_QUEUE_LEN);
        let intake = Arc::new(Mutex::new(ReaderIntake::Open(0)));
        let reader_intake = intake.clone();
        let reader_thread = std::thread::spawn(move || reader_loop(reader, tx, reader_intake));
        Self {
            writer: Some(writer),
            rx: Some(rx),
            reader_thread: Some(reader_thread),
            process: None,
            stderr: StderrRing::default(),
            send_mode: SendMode::Line,
            cwd: normalize_cwd(None),
            mcp: None,
            next_id: 0,
            pending: HashMap::new(),
            init: InitState::Idle,
            supports_mcp_http: false,
            session_id: None,
            prompt: PromptState::Idle,
            next_prompt: 0,
            intake,
            #[cfg(test)]
            fail_shutdown: false,
            disconnected: false,
            conversation: Conversation::default(),
        }
    }

    pub fn set_send_mode(&mut self, mode: SendMode) {
        self.send_mode = mode;
    }

    /// Offer this MCP HTTP server in `session/new`, when the agent advertised HTTP MCP support.
    pub fn set_mcp_http(&mut self, mcp: Option<McpHttp>) {
        self.mcp = mcp;
    }

    /// Capture the agent's stderr (newest bytes only) on a thread that ends with the stream.
    pub fn attach_stderr(&mut self, reader: impl Read + Send + 'static) {
        let thread = spawn_stderr_thread(reader, self.stderr.clone());
        match &mut self.process {
            Some(process) => process.threads.push(thread),
            None => drop(thread),
        }
    }

    pub fn stderr_text(&self) -> String {
        self.stderr.text()
    }

    /// Whether a valid ACP v1 `initialize` response was accepted on this live connection.
    pub fn is_initialized(&self) -> bool {
        matches!(self.init, InitState::Ready(_)) && !self.disconnected
    }

    pub fn init_error(&self) -> Option<&str> {
        match &self.init {
            InitState::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// The `result` of the accepted `initialize` response.
    pub fn init_result(&self) -> Option<&Value> {
        match &self.init {
            InitState::Ready(result) => Some(result),
            _ => None,
        }
    }

    pub fn agent_supports_mcp_http(&self) -> bool {
        self.supports_mcp_http
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// A prompt is waiting for its session or for the agent's response.
    pub fn prompt_active(&self) -> bool {
        self.prompt.active_id().is_some()
    }

    /// Only an actually transmitted, uncancelled prompt can initiate lifecycle consent.
    pub fn active_prompt(&self) -> Option<PromptId> {
        if self.permission_epoch() == u64::MAX { return None; }
        match self.prompt { PromptState::Running { id, cancelled: false } => Some(id), _ => None }
    }

    pub fn prompt_completion(&self) -> Option<&PromptCompletion> {
        match &self.prompt { PromptState::Finished(completion) => Some(completion), _ => None }
    }

    pub fn permission_epoch(&self) -> u64 {
        match self.intake.lock().as_deref() {
            Ok(ReaderIntake::Open(epoch)) => *epoch,
            _ => u64::MAX,
        }
    }

    /// Atomically end reader admission only if no permission/disconnection raced with the final poll.
    pub fn seal_permission_intake(&mut self, expected: u64) -> bool {
        let Ok(mut intake) = self.intake.lock() else { return false };
        if matches!(*intake, ReaderIntake::Open(epoch) if epoch == expected) {
            *intake = ReaderIntake::Closed;
            true
        } else { false }
    }

    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(|p| p.child.id())
    }

    // -- requests -----------------------------------------------------------------------------

    /// Send `initialize`. The outcome arrives from [`Client::poll`] as `Initialized` or `InitFailed`.
    pub fn initialize(&mut self) -> Result<(), String> {
        self.init = InitState::Pending;
        self.supports_mcp_http = false;
        self.session_id = None;
        self.prompt = PromptState::Idle;
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "soul2d", "version": "0.0.0"},
        });
        self.request("initialize", params, Pending::Initialize).map(drop).map_err(|error| {
            self.init = InitState::Failed(error.clone());
            error
        })
    }

    pub fn ping(&mut self) -> Result<(), String> {
        self.request("ping", json!({}), Pending::Ping).map(drop)
    }

    /// Send `session/new` with the launch directory and, when offered and supported, the MCP server.
    pub fn new_session(&mut self) -> Result<(), String> {
        if !self.is_initialized() {
            return Err("ACP agent is not initialized".to_string());
        }
        if matches!(self.prompt, PromptState::Running { .. }) {
            return Err("a prompt is already active".into());
        }
        let servers = match &self.mcp {
            Some(mcp) if self.supports_mcp_http && !mcp.url.is_empty() => {
                json!([{"type": "http", "name": mcp.name, "url": mcp.url, "headers": mcp.headers}])
            }
            _ => json!([]),
        };
        let params = json!({"cwd": self.cwd, "mcpServers": servers});
        self.request("session/new", params, Pending::NewSession).map(drop)
    }

    /// Send a prompt turn. Without a session one is created first and the prompt follows its response.
    pub fn send_prompt(&mut self, text: &str) -> Result<PromptId, String> {
        self.send_prompt_with(text, None)
    }

    /// `send_prompt` with a second text block the host adds (the editor's briefing), shown in the log as "Soul2D".
    pub fn send_prompt_with(&mut self, text: &str, briefing: Option<&str>) -> Result<PromptId, String> {
        if !self.is_initialized() {
            return Err("ACP agent is not initialized".to_string());
        }
        if self.prompt_active() {
            return Err("a prompt is already active".to_string());
        }
        if text.is_empty() {
            return Err("prompt is empty".to_string());
        }
        let id = PromptId(self.next_prompt);
        self.next_prompt = self.next_prompt.checked_add(1).ok_or("prompt identity exhausted")?;
        self.conversation.record_user(text);
        if let Some(briefing) = briefing {
            self.conversation.record_briefing(briefing);
        }
        if self.session_id.is_some() {
            self.send_prompt_now(id, text, briefing)?;
            return Ok(id);
        }
        self.prompt = PromptState::Queued { id, text: text.to_owned(), briefing: briefing.map(str::to_owned) };
        if !self.pending.values().any(|p| matches!(p, Pending::NewSession)) {
            if let Err(error) = self.new_session() {
                self.prompt = PromptState::Idle;
                return Err(error);
            }
        }
        Ok(id)
    }

    fn send_prompt_now(&mut self, id: PromptId, text: &str, briefing: Option<&str>) -> Result<(), String> {
        let session_id = self.session_id.clone().unwrap_or_default();
        let mut blocks = vec![json!({"type": "text", "text": text})];
        blocks.extend(briefing.map(|briefing| json!({"type": "text", "text": briefing})));
        let params = json!({"sessionId": session_id, "prompt": blocks});
        self.prompt = PromptState::Running { id, cancelled: false };
        if let Err(error) = self.request("session/prompt", params, Pending::Prompt(id)) {
            self.prompt = PromptState::Finished(PromptCompletion { id, outcome: PromptOutcome::Failed(error.clone()), received_at: Instant::now() });
            self.conversation.end_turn();
            return Err(error);
        }
        Ok(())
    }

    /// Abort the current turn: `session/cancel`, then every outstanding permission answered `cancelled`.
    /// Does nothing when no prompt is active or a cancel is already in progress.
    pub fn cancel(&mut self) -> Result<(), String> {
        if let PromptState::Queued { id, .. } = self.prompt {
            self.prompt = PromptState::Finished(PromptCompletion { id, outcome: PromptOutcome::Cancelled, received_at: Instant::now() });
            return Ok(());
        }
        let PromptState::Running { cancelled, .. } = &mut self.prompt else { return Ok(()) };
        if *cancelled {
            return Ok(());
        }
        *cancelled = true;
        let responses = self.conversation.cancel();
        let mut first_error = None;
        if let Some(session_id) = self.session_id.clone() {
            let message = json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}});
            if let Err(error) = self.send(&message) {
                first_error = Some(error);
            }
        }
        for response in responses {
            if let Err(error) = self.send(&response) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Answer a queued permission request. `Some(option_id)` selects an option; `None` denies. Returns
    /// `Ok(false)` when the request is no longer answerable (stale generation, or the turn is cancelling).
    pub fn answer_permission(&mut self, id: &Value, generation: u64, option: Option<&str>) -> Result<bool, String> {
        match self.conversation.answer(id, generation, option) {
            Some(response) => self.send(&response).map(|_| true),
            None => Ok(false),
        }
    }

    fn request(&mut self, method: &str, params: Value, pending: Pending) -> Result<Value, String> {
        let id = Value::from(self.next_id);
        self.next_id += 1;
        let key = id.to_string();
        self.pending.insert(key.clone(), pending);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(error) = self.send(&message) {
            self.pending.remove(&key);
            return Err(error);
        }
        Ok(id)
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let payload = message.to_string();
        if payload.len() > MAX_FRAME_BYTES {
            return Err(format!("message of {} bytes exceeds {MAX_FRAME_BYTES} bytes", payload.len()));
        }
        let frame = match self.send_mode {
            SendMode::Line => format!("{payload}\n"),
            SendMode::ContentLength => format!("Content-Length: {}\r\n\r\n{payload}", payload.len()),
        };
        let result = match self.writer.as_mut() {
            Some(writer) => writer.write_all(frame.as_bytes()).and_then(|_| writer.flush()).map_err(|e| e.to_string()),
            None => Err("connection closed".to_string()),
        };
        result.map_err(|error| self.with_stderr(format!("send failed: {error}")))
    }

    fn with_stderr(&self, message: String) -> String {
        let stderr = self.stderr.text();
        if stderr.is_empty() {
            message
        } else {
            format!("{message} | stderr: {stderr}")
        }
    }

    // -- events -------------------------------------------------------------------------------

    /// Take everything the agent has sent so far. Never blocks.
    pub fn poll(&mut self) -> Vec<Incoming> {
        self.collect(None)
    }

    /// Like [`Client::poll`], but waits up to `timeout` for the first event.
    pub fn poll_wait(&mut self, timeout: Duration) -> Vec<Incoming> {
        let first = match self.rx.as_ref().map(|rx| rx.recv_timeout(timeout)) {
            Some(Ok(event)) => Some(event),
            _ => return Vec::new(),
        };
        self.collect(first)
    }

    fn collect(&mut self, first: Option<Event>) -> Vec<Incoming> {
        let mut out = Vec::new();
        let mut next = first;
        loop {
            if let Some(event) = next.take() {
                self.handle_event(event, &mut out);
            }
            match self.rx.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => next = Some(event),
                Some(Err(TryRecvError::Empty | TryRecvError::Disconnected)) | None => return out,
            }
        }
    }

    fn handle_event(&mut self, event: Event, out: &mut Vec<Incoming>) {
        match event {
            Event::Frame(message, received_at) => self.handle_message(message, received_at, out),
            Event::Malformed(error) => out.push(Incoming::Malformed(error)),
            Event::Disconnected(reason) => self.handle_disconnect(reason, out),
        }
    }

    fn handle_disconnect(&mut self, reason: String, out: &mut Vec<Incoming>) {
        self.disconnected = true;
        let was_pending_init = matches!(self.init, InitState::Pending);
        let prompt = self.prompt.active_id();
        let session_pending = self.pending.values().any(|p| matches!(p, Pending::NewSession));
        self.pending.clear();
        self.conversation.end_turn();
        if was_pending_init {
            let error = self.with_stderr("Agent stream closed before initialize response".to_string());
            self.init = InitState::Failed(error.clone());
            out.push(Incoming::InitFailed(error));
        } else if session_pending {
            let error = self.with_stderr("Agent stream closed before session/new response".to_string());
            out.push(Incoming::SessionFailed(error));
        }
        if let Some(id) = prompt {
            let error = self.with_stderr("Agent stream closed before prompt response".to_string());
            let completion = PromptCompletion { id, outcome: PromptOutcome::Failed(error), received_at: Instant::now() };
            self.prompt = PromptState::Finished(completion.clone());
            out.push(Incoming::PromptFinished(completion));
        }
        out.push(Incoming::Disconnected(reason));
    }

    fn handle_message(&mut self, message: Value, received_at: Instant, out: &mut Vec<Incoming>) {
        if let Some(method) = str_of(&message, "method") {
            if is_permission_method(method) {
                match self.conversation.apply_permission_request(&message) {
                    PermissionIntake::Queued(request) => out.push(Incoming::Permission(request)),
                    PermissionIntake::AutoCancelled(response) => {
                        if let Err(error) = self.send(&response) {
                            out.push(Incoming::Error(format!("permission cancellation failed: {error}")));
                        }
                        out.push(Incoming::PermissionCancelled(response["id"].clone()));
                    }
                    PermissionIntake::Ignored => {}
                }
                return;
            }
            if method == "session/update" {
                if let Some(update) = message.get("params").and_then(|p| p.get("update")) {
                    self.conversation.apply_update(update);
                    out.push(Incoming::Update(update.clone()));
                    return;
                }
            }
            out.push(Incoming::Other(message));
            return;
        }
        let pending = if is_response(&message) { self.pending.remove(&message["id"].to_string()) } else { None };
        match pending {
            Some(Pending::Initialize) => out.push(self.accept_initialize(&message)),
            Some(Pending::NewSession) => self.accept_session(&message, out),
            Some(Pending::Prompt(id)) => {
                let cancelled = !matches!(self.prompt, PromptState::Running { id: current, cancelled: false } if current == id);
                self.conversation.end_turn();
                let stop_reason = message.get("result").and_then(|r| str_of(r, "stopReason")).map(str::to_string);
                let error = message.get("error").map(rpc_error_message);
                let outcome = if cancelled || stop_reason.as_deref() == Some("cancelled") { PromptOutcome::Cancelled }
                    else if let Some(error) = error { PromptOutcome::Failed(error) }
                    else if message["jsonrpc"] != "2.0" { PromptOutcome::Failed("Malformed ACP prompt response".into()) }
                    else if stop_reason.as_deref() == Some("end_turn") { PromptOutcome::Completed }
                    else { PromptOutcome::Stopped(stop_reason) };
                let completion = PromptCompletion { id, outcome, received_at };
                self.prompt = PromptState::Finished(completion.clone());
                out.push(Incoming::PromptFinished(completion));
            }
            Some(Pending::Ping) => out.push(Incoming::Ping(match message.get("error") {
                Some(error) => Err(format!("ping error: {}", rpc_error_message(error))),
                None => Ok(()),
            })),
            None => out.push(Incoming::Other(message)),
        }
    }

    fn accept_initialize(&mut self, message: &Value) -> Incoming {
        self.supports_mcp_http = false;
        let result = match (message.get("error"), message.get("result")) {
            (Some(error), _) => Err(format!("initialize error: {}", rpc_error_message(error))),
            (None, Some(result)) if result.is_object() => match result.get("protocolVersion") {
                Some(version) if version.as_i64() == Some(PROTOCOL_VERSION) => Ok(result),
                Some(version) => {
                    Err(format!("initialize protocolVersion mismatch: expected {PROTOCOL_VERSION}, got {version}"))
                }
                None => Err(format!("initialize protocolVersion mismatch: expected {PROTOCOL_VERSION}, got missing")),
            },
            _ => Err("initialize response missing result".to_string()),
        };
        match result {
            Ok(result) => {
                self.supports_mcp_http = result
                    .get("agentCapabilities")
                    .and_then(|c| c.get("mcpCapabilities"))
                    .and_then(|m| m.get("http"))
                    .and_then(Value::as_bool)
                    == Some(true);
                self.init = InitState::Ready(result.clone());
                Incoming::Initialized
            }
            Err(error) => {
                self.init = InitState::Failed(error.clone());
                Incoming::InitFailed(error)
            }
        }
    }

    fn accept_session(&mut self, message: &Value, out: &mut Vec<Incoming>) {
        if let Some(error) = message.get("error") {
            out.push(Incoming::SessionFailed(format!("session/new error: {}", rpc_error_message(error))));
            self.prompt = PromptState::Idle;
            return;
        }
        let Some(session_id) = message.get("result").and_then(|r| str_of(r, "sessionId")) else {
            out.push(Incoming::SessionFailed("session/new missing sessionId".to_string()));
            self.prompt = PromptState::Idle;
            return;
        };
        self.session_id = Some(session_id.to_string());
        out.push(Incoming::SessionReady(session_id.to_string()));
        let prompt = std::mem::replace(&mut self.prompt, PromptState::Idle);
        if let PromptState::Queued { id, text, briefing } = prompt {
            if let Err(error) = self.send_prompt_now(id, &text, briefing.as_deref()) {
                out.push(Incoming::Error(format!("prompt send failed: {error}")));
                if let Some(completion) = self.prompt_completion() { out.push(Incoming::PromptFinished(completion.clone())); }
            }
        } else {
            self.prompt = prompt;
        }
    }

    // -- shutdown -----------------------------------------------------------------------------

    /// Close input and reap the owned child. `Ok(None)` means no child was owned; an error retains it
    /// for retry. `Child::kill` is SIGKILL on Unix; there is no gentler signal in std.
    pub fn shutdown(&mut self) -> Result<Option<ExitStatus>, String> {
        close_intake(&self.intake);
        self.writer = None;
        self.rx = None; // unblocks a reader stuck on a full queue
        self.disconnected = true;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_shutdown) { return Err("injected owned shutdown failure".into()); }
        let Some(process) = self.process.as_mut() else { return Ok(None) };
        let deadline = Instant::now() + EXIT_GRACE;
        let status = loop {
            match process.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    process.child.kill().map_err(|error| format!("could not stop owned ACP child: {error}"))?;
                    break process.child.wait().map_err(|error| format!("could not reap owned ACP child: {error}"))?;
                }
            }
        };
        let mut process = self.process.take().expect("reaped owned process");
        let mut threads = std::mem::take(&mut process.threads);
        threads.extend(self.reader_thread.take());
        for thread in threads {
            // A grandchild that inherited the pipes could keep a thread alive; do not hang on it.
            for _ in 0..100 {
                if thread.is_finished() {
                    let _ = thread.join();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(Some(status))
    }
}

impl Client<ChildStdin> {
    /// Launch the agent (program and arguments, no shell) with piped stdio.
    pub fn spawn(spec: &LaunchSpec) -> Result<Self, String> {
        let (program, args) = spec.command.split_first().ok_or_else(|| "Command is empty".to_string())?;
        let cwd = normalize_cwd(spec.workdir.as_deref());
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("spawn failed: {error}"))?;
        let (stdin, stdout, stderr) = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("spawn failed: missing stdio pipe".to_string());
            }
        };
        let mut client = Client::new(stdout, stdin);
        client.cwd = cwd;
        client.process = Some(Process { child, threads: Vec::new() });
        client.attach_stderr(stderr);
        Ok(client)
    }
}

impl<W: Write> Drop for Client<W> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn spawn_stderr_thread(mut reader: impl Read + Send + 'static, ring: StderrRing) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => ring.push(&chunk[..n]),
            }
        }
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn unpolled_permission_and_eof_compete_with_the_release_cutoff() {
        for permission in [false, true] {
            let (read, mut write) = std::io::pipe().unwrap();
            let mut client = Client::new(read, Vec::new());
            let epoch = client.permission_epoch();
            assert!(client.poll().is_empty());
            if permission {
                writeln!(write, "{}", serde_json::json!({"jsonrpc":"2.0","id":null,"method":"session/request_permission","params":{}})).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while client.permission_epoch() == epoch && Instant::now() < deadline { std::thread::yield_now(); }
                assert_eq!(client.permission_epoch(), epoch + 1);
            } else {
                drop(write);
                let deadline = Instant::now() + Duration::from_secs(5);
                while client.permission_epoch() == epoch && Instant::now() < deadline { std::thread::yield_now(); }
                assert_eq!(client.permission_epoch(), u64::MAX);
            }
            assert!(!client.seal_permission_intake(epoch), "an event unobserved by poll must still prevent release");
            assert_eq!(client.shutdown(), Ok(None));
        }
    }

    #[test]
    fn sealed_reader_admits_no_later_decoded_frames() {
        let intake = Arc::new(Mutex::new(ReaderIntake::Closed));
        let (tx, rx) = sync_channel(4);
        reader_loop(std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n"), tx, intake);
        assert!(matches!(rx.recv().unwrap(), Event::Disconnected(_)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn decoded_batch_keeps_early_intake_time_across_queue_backpressure() {
        let bytes = (0..4).map(|id| format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}\n")).collect::<String>();
        let (tx, rx) = sync_channel(1);
        let intake = Arc::new(Mutex::new(ReaderIntake::Open(0)));
        let reader = std::thread::spawn(move || reader_loop(std::io::Cursor::new(bytes), tx, intake));
        let mut times = Vec::new();
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
            match event {
                Event::Frame(_, at) => times.push(at),
                Event::Disconnected(_) => break,
                Event::Malformed(error) => panic!("{error}"),
            }
        }
        reader.join().unwrap();
        assert_eq!(times.len(), 4);
        assert!(times.iter().all(|at| *at == times[0]), "backpressure must not move a decoded response beyond the admission fence");
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_shutdown_retains_owned_child_for_the_same_owner_to_retry() {
        let spec = LaunchSpec { command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 300".into()], workdir: None };
        let mut client = Client::spawn(&spec).unwrap();
        let pid = client.pid().unwrap();
        client.fail_shutdown = true;
        assert!(client.shutdown().is_err());
        assert_eq!(client.pid(), Some(pid));
        assert!(client.shutdown().unwrap().is_some());
        assert_eq!(client.pid(), None);
        assert_eq!(client.shutdown(), Ok(None));
    }
}
