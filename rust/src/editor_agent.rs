//! Agent panel of the editor: the chat UI of D `panels/agent.d` on top of the finished [`acp`] client.
//!
//! Opt-in: nothing is spawned and no socket is bound until the user enables the panel and presses Start.
//! While an agent runs, a loopback-only MCP HTTP endpoint serves the editor's own [`Session`] to it. The
//! session lives on the UI thread, so the listener is non-blocking and [`AgentPanel::serve_mcp`] serves the
//! connections that are waiting once per frame. A stalled client can hold the UI for at most [`MCP_BUDGET`]
//! per connection and [`MAX_CONNECTIONS_PER_FRAME`] connections per frame. Every endpoint requires a random
//! bearer token that is generated at Start and handed to the agent in the MCP server entry's headers.
//!
//! The pure functions (button states, block lines, permission choices, URL and token shape) are tested
//! without egui; the panel logic is generic over the agent's writer so tests can drive it over pipes.

use eframe::egui;
use serde_json::{json, Map, Value};
use soul2d_core::acp::{Block, Client, Incoming, LaunchSpec, McpHttp, Plan, PermissionRequest, PromptId, PromptOutcome, RoleKind};
use soul2d_core::mcp_http::{handle_connection_deferred, HttpDispatch, PendingReply, RpcReply};
use soul2d_core::settings::Settings;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::time::{Duration, Instant};

/// Bound for every I/O phase of one MCP connection (see the module doc).
pub const MCP_BUDGET: Duration = Duration::from_millis(500);
pub const MAX_CONNECTIONS_PER_FRAME: usize = 4;
const REPAINT_EVERY: Duration = Duration::from_millis(50);
/// Longest line drawn; longer lines are cut (D `kMaxDrawCharsPerLine`).
const MAX_DRAW_CHARS: usize = 4000;
const LABEL_MAX_CHARS: usize = 15;
const STDERR_TAIL_LINES: usize = 12;
const TOOL_FOCUS_KEYS: [&str; 4] = ["rawInput", "rawOutput", "content", "contents"];
const ALLOW_ONCE: &str = "allow-once";
const HOST_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// `Client` appends the agent's stderr to its error strings after this marker.
const STDERR_MARKER: &str = " | stderr: ";

// ---------------------------------------------------------------------------------------------
// Pure functions

/// `http://host:port/mcp`, with IPv6 hosts in brackets.
pub fn mcp_url(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(v4) => format!("http://{}:{}/mcp", v4.ip(), v4.port()),
        SocketAddr::V6(v6) => format!("http://[{}]:{}/mcp", v6.ip(), v6.port()),
    }
}

#[cfg(test)]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub use soul2d_core::mcp_http::random_token;

/// `headers` (the `MCP.Headers` array of `{name, value}`) with any `Authorization` entry replaced by ours.
pub fn authorized_headers(headers: &Value, token: &str) -> Value {
    let mut list: Vec<Value> = headers
        .as_array()
        .into_iter()
        .flatten()
        .filter(|header| !header["name"].as_str().is_some_and(|name| name.eq_ignore_ascii_case("authorization")))
        .cloned()
        .collect();
    list.push(json!({"name": "Authorization", "value": format!("Bearer {token}")}));
    Value::Array(list)
}

/// What decides which controls are usable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelState {
    pub enabled: bool,
    pub has_client: bool,
    /// The agent's stream has closed.
    pub closed: bool,
    /// `initialize` was accepted and the stream is open.
    pub ready: bool,
    pub turn_active: bool,
    pub cancelling: bool,
    pub awaiting_permission: bool,
    pub prompt_present: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    pub start: bool,
    pub stop: bool,
    pub new_session: bool,
    pub send: bool,
    pub cancel: bool,
}

pub fn controls(state: &PanelState) -> Controls {
    let busy = state.turn_active || state.awaiting_permission || state.cancelling;
    Controls {
        start: state.enabled && (!state.has_client || state.closed),
        stop: state.has_client,
        new_session: state.enabled,
        send: state.ready && !busy && state.prompt_present,
        cancel: state.ready && state.turn_active && !state.cancelling,
    }
}

/// Cut a line to [`MAX_DRAW_CHARS`] characters.
pub fn clip_line(line: &str) -> String {
    match line.char_indices().nth(MAX_DRAW_CHARS) {
        Some((end, _)) => format!("{} ... (display truncated)", &line[..end]),
        None => line.to_string(),
    }
}

/// `lines` without leading and trailing blank lines.
pub fn trim_blank_edges(lines: &[String]) -> &[String] {
    let blank = |line: &String| line.trim().is_empty();
    let start = lines.iter().position(|line| !blank(line)).unwrap_or(lines.len());
    let end = lines.iter().rposition(|line| !blank(line)).map_or(start, |last| last + 1);
    &lines[start..end]
}

/// What to draw for one conversation block.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockView {
    pub kind: RoleKind,
    pub header: String,
    pub status: String,
    pub body: Vec<String>,
    /// Drawn as a collapsing section (tool calls, thinking, errors, other roles) instead of plain text.
    pub collapsible: bool,
    pub open: bool,
}

pub fn block_view(block: &Block, is_last: bool) -> BlockView {
    let clipped = |lines: &[String]| trim_blank_edges(lines).iter().map(|line| clip_line(line)).collect::<Vec<_>>();
    let mut view = BlockView {
        kind: block.kind,
        header: block.role.clone(),
        status: String::new(),
        body: Vec::new(),
        collapsible: true,
        open: is_last,
    };
    match block.kind {
        RoleKind::You | RoleKind::AssistantFinal => {
            view.collapsible = false;
            view.body = clipped(&block.lines);
        }
        RoleKind::AssistantThinking => {
            // The first non-blank line is the summary; the rest is folded away.
            let first = block.lines.iter().position(|line| !line.trim().is_empty());
            if let Some(index) = first {
                view.header = format!("{}: {}", block.role, clip_line(&block.lines[index]));
                view.body = clipped(&block.lines[index + 1..]);
            }
            view.open = false;
        }
        RoleKind::Tool => {
            let title = if block.title.is_empty() { block.role.as_str() } else { block.title.as_str() };
            view.header = format!("tool: {}", short_title(title));
            view.status = block.status.clone();
            view.body = match &block.payload_merged {
                Some(payload) => tool_detail_lines(payload),
                None => clipped(&block.lines),
            };
        }
        RoleKind::Error | RoleKind::Other => view.body = clipped(&block.lines),
    }
    view
}

fn short_title(title: &str) -> String {
    match title.split_once('\n') {
        Some((first, _)) => format!("{first} ..."),
        None => title.to_string(),
    }
}

/// Pretty JSON lines of the parts of a tool payload worth reading (`rawInput`, `rawOutput`, `content`,
/// `contents`, and whatever is below them); the whole payload when it has none of them.
pub fn tool_detail_lines(payload: &Value) -> Vec<String> {
    let entries: Vec<(String, &Value)> = match payload {
        Value::Object(map) => map.iter().map(|(key, value)| (key.clone(), value)).collect(),
        Value::Array(items) => items.iter().enumerate().map(|(index, value)| (format!("[{index}]"), value)).collect(),
        _ => Vec::new(),
    };
    let mut focused = Map::new();
    for (label, value) in entries {
        if let Some(kept) = prune(value, &label, false) {
            focused.insert(label, kept);
        }
    }
    let shown = if focused.is_empty() { payload.clone() } else { Value::Object(focused) };
    serde_json::to_string_pretty(&shown)
        .unwrap_or_default()
        .lines()
        .map(clip_line)
        .collect()
}

fn contains_focus_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, child)| TOOL_FOCUS_KEYS.contains(&key.as_str()) || contains_focus_key(child)),
        Value::Array(items) => items.iter().any(contains_focus_key),
        _ => false,
    }
}

/// Keep the branches of `value` that lead to a focus key, and everything below such a key.
fn prune(value: &Value, label: &str, below_focus: bool) -> Option<Value> {
    match value {
        Value::Object(map) => {
            let matched = TOOL_FOCUS_KEYS.contains(&label);
            if !below_focus && !matched && !contains_focus_key(value) {
                return None;
            }
            let mut out = Map::new();
            for (key, child) in map {
                let child_matched = TOOL_FOCUS_KEYS.contains(&key.as_str());
                let allow = below_focus || matched || child_matched;
                if allow || contains_focus_key(child) {
                    if let Some(kept) = prune(child, key, allow) {
                        out.insert(key.clone(), kept);
                    }
                }
            }
            Some(Value::Object(out))
        }
        Value::Array(items) => {
            if !below_focus && !contains_focus_key(value) {
                return None;
            }
            let kept = items
                .iter()
                .enumerate()
                .filter(|(_, item)| below_focus || contains_focus_key(item))
                .filter_map(|(index, item)| prune(item, &format!("[{index}]"), below_focus))
                .collect();
            Some(Value::Array(kept))
        }
        scalar => (below_focus || TOOL_FOCUS_KEYS.contains(&label)).then(|| scalar.clone()),
    }
}

/// Plan lines as drawn: `1. [high] Do it (pending)`.
pub fn plan_lines(plan: &Plan) -> Vec<String> {
    plan.lines.iter().enumerate().map(|(index, line)| format!("{}. {}", index + 1, clip_line(line))).collect()
}

pub fn status_color(status: &str) -> egui::Color32 {
    match status {
        "in_progress" => egui::Color32::from_rgb(0, 102, 204),
        "completed" => egui::Color32::from_rgb(0, 191, 0),
        "failed" => egui::Color32::from_rgb(230, 0, 0),
        "queued" => egui::Color32::from_rgb(230, 128, 0),
        "cancelled" => egui::Color32::from_rgb(128, 128, 128),
        _ => egui::Color32::from_rgb(153, 153, 153),
    }
}

/// Shorten a button label to `max` characters, adding "..." when cut.
pub fn ellipsize(label: &str, max: usize) -> String {
    match label.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &label[..end]),
        None => label.to_string(),
    }
}

/// One button of the permission prompt. `option_id: None` answers "denied".
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionChoice {
    pub label: String,
    pub shown: String,
    pub option_id: Option<String>,
    /// False for an option the agent sent without an id: it cannot be selected.
    pub selectable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionView {
    pub heading: String,
    /// False when the agent sent no request id: the answer would have nowhere to go.
    pub answerable: bool,
    pub choices: Vec<PermissionChoice>,
}

pub fn permission_view(request: &PermissionRequest) -> PermissionView {
    let reason = if request.reason.is_empty() { "(no reason)" } else { request.reason.as_str() };
    let text = |option: &Value, key: &str| option[key].as_str().filter(|text| !text.is_empty()).map(str::to_string);
    let mut choices = Vec::new();
    let options = request.params["options"].as_array().filter(|options| !options.is_empty());
    for option in options.into_iter().flatten() {
        let id = text(option, "optionId").or_else(|| text(option, "id"));
        let label = ["name", "label", "title", "kind", "action", "optionId", "id"]
            .iter()
            .find_map(|key| text(option, key))
            .unwrap_or_else(|| "Option".to_string());
        choices.push(PermissionChoice {
            shown: ellipsize(&label, LABEL_MAX_CHARS),
            label,
            selectable: id.is_some(),
            option_id: id,
        });
    }
    if options.is_none() {
        choices.push(PermissionChoice {
            label: "Allow".into(),
            shown: "Allow".into(),
            option_id: Some(ALLOW_ONCE.into()),
            selectable: true,
        });
    }
    choices.push(PermissionChoice { label: "Deny".into(), shown: "Deny".into(), option_id: None, selectable: true });
    PermissionView { heading: format!("Permission requested: {reason}"), answerable: !request.id.is_null(), choices }
}

/// The last `max_lines` non-empty lines of the agent's stderr.
pub fn stderr_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

/// An error string without the stderr dump `Client` appends to it.
pub fn error_head(error: &str) -> &str {
    error.split_once(STDERR_MARKER).map_or(error, |(head, _)| head)
}

// ---------------------------------------------------------------------------------------------
// MCP endpoint

/// The loopback listener offered to the agent, and the token it must present.
pub struct McpEndpoint {
    listener: TcpListener,
    token: String,
    url: String,
}

impl McpEndpoint {
    /// Bind `127.0.0.1:0` (or `[::1]:0` when IPv4 loopback is unavailable) and generate a token.
    pub fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .or_else(|_| TcpListener::bind("[::1]:0"))
            .map_err(|error| format!("cannot bind the MCP listener: {error}"))?;
        listener.set_nonblocking(true).map_err(|error| format!("cannot configure the MCP listener: {error}"))?;
        let addr = listener.local_addr().map_err(|error| format!("cannot read the MCP address: {error}"))?;
        Ok(Self { listener, token: random_token()?, url: mcp_url(addr) })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    #[cfg(test)]
    pub(crate) fn token_for_test(&self) -> &str {
        &self.token
    }

    /// Connections that are waiting right now; never blocks.
    fn accept_pending(&self, max: usize) -> Vec<TcpStream> {
        let mut streams = Vec::new();
        while streams.len() < max {
            match self.listener.accept() {
                // Some platforms hand out accepted sockets in the listener's non-blocking mode.
                Ok((stream, _)) => match stream.set_nonblocking(false) {
                    Ok(()) => streams.push(stream),
                    Err(_) => continue,
                },
                Err(_) => break,
            }
        }
        streams
    }
}

// ---------------------------------------------------------------------------------------------
// Panel state

enum ApprovalDecision {
    Pending,
    AllowedOnce,
    Denied,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalDocument {
    pub revision: u64,
    pub draft_dirty: bool,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ApprovalTarget {
    Quit(AcpQuitTarget),
    Settings(Option<PathBuf>),
    Output(PathBuf),
    Outputs(Vec<PathBuf>),
    ExternalProcess(soul2d_core::export::VideoConsent),
    Collection(soul2d_core::export::CollectionPlan),
    Gltf(soul2d_core::export::GltfPlan),
    DepthPng(soul2d_core::export::DepthPngPlan),
    Store(soul2d_core::StoreWritePlan),
    NewDocument,
    SourceFile(PathBuf),
    SourceDirectory(PathBuf),
    Recovery(PathBuf),
}

struct ApprovalRequest {
    call: Value,
    target: ApprovalTarget,
    document: Option<ApprovalDocument>,
    effect: &'static str,
    decision: ApprovalDecision,
    created: std::time::Instant,
}

#[derive(Default)]
pub(crate) struct HostApproval {
    active: bool,
    request: Option<ApprovalRequest>,
    defer_current: bool,
    invocation: Option<AcpQuitTarget>,
}

/// Assigned only inside authenticated hosting; equal wire IDs do not identify the same invocation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AcpQuitTarget {
    run: u64,
    prompt: PromptId,
    call: u64,
    permissions: u64,
}

pub(crate) enum QuitRelease {
    Waiting,
    Released,
    Invalid(String),
}

impl AcpQuitTarget {
    pub(crate) fn owner(&self) -> u64 { self.run }
}

impl HostApproval {
    pub(crate) fn covers(call: &Value) -> bool {
        matches!(call["op"].as_str(), Some("editor.quit" | "editor.panel.set" | "editor.settings.set" | "editor.shortcut.set" |
            "editor.workspace.save" | "editor.workspace.apply" | "editor.workspace.delete" | "editor.layout.reset" | "save" |
            "export.inp" | "export.image" | "export.frames" | "export.video.start" | "export.collect" | "export.gltf" | "depth.draw.export_png" | "export.gif" | "view.screenshot" | "object.export_csv" | "model.template.export" | "motion3.export" | "texture.export"))
            || matches!(call["op"].as_str(), Some("automesh.preset.save" | "automesh.preset.delete" | "physics.preset.save" |
                "physics.preset.delete" | "live2d.physics3.preset.save" | "live2d.physics3.preset.delete" |
                "animation.template.save" | "animation.template.delete"))
            || (matches!(call["op"].as_str(), Some("render" | "animation.onion" | "live2d.cdi3.export" |
                "live2d.physics3.export" | "live2d.userdata3.export")) && call["path"].as_str().is_some())
            || soul2d_core::editor_controller::replaces_document(call) || call["op"] == "editor.recovery.restore"
    }

    pub(crate) fn quit_target(&self) -> Result<AcpQuitTarget, String> {
        self.invocation.clone().filter(|_| self.active).ok_or_else(||
            "ACP Quit needs an authenticated request during a transmitted uncancelled prompt".into())
    }

    pub(crate) fn quit_review(&self) -> Option<AcpQuitTarget> {
        match &self.request.as_ref()?.target { ApprovalTarget::Quit(target) => Some(target.clone()), _ => None }
    }

    pub(crate) fn invalidate_quit(&mut self, target: &AcpQuitTarget) {
        if self.quit_review().as_ref() == Some(target) {
            if let Some(request) = &mut self.request {
                if !matches!(request.decision, ApprovalDecision::Denied) { request.decision = ApprovalDecision::Stale; }
            }
        }
    }

    pub(crate) fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.request = None;
            self.defer_current = false;
        }
    }

    pub(crate) fn guard(&mut self, call: &Value, target: Result<ApprovalTarget, String>, document: Option<ApprovalDocument>) -> Result<(), String> {
        if !Self::covers(call) {
            return Ok(());
        }
        if !self.active {
            return Err("ACP protected operations need an active, uncancelled prompt".into());
        }
        if self.request.as_ref().is_some_and(|request| matches!(request.decision, ApprovalDecision::Denied)) {
            return Err("The host denied protected changes for this prompt".into());
        }
        if self.request.as_ref().is_some_and(|request| matches!(request.decision, ApprovalDecision::Stale)) {
            return Err("The destination or document changed; request the operation again".into());
        }
        if self.request.as_ref().is_some_and(|request| request.call != *call) {
            return Err("The host is reviewing another operation".into());
        }
        let (target, document) = match target.and_then(|target| {
            let document = if matches!(target, ApprovalTarget::Settings(_) | ApprovalTarget::Store(_)) { None }
                else { Some(document.ok_or("The operation needs the current document state")?) };
            Ok((target, document))
        }) {
            Ok(value) => value,
            Err(error) => {
                if let Some(request) = &mut self.request { request.decision = ApprovalDecision::Stale; }
                return Err(error);
            }
        };
        if let Some(request) = &mut self.request {
            if request.target != target || request.document != document {
                request.decision = ApprovalDecision::Stale;
                return Err("The destination or document changed; request the operation again".into());
            }
        }
        if call["op"] == "save" && document.as_ref().is_some_and(|document| document.draft_dirty) {
            return Err(soul2d_core::editor_controller::FILE_OP_BLOCKED_MESSAGE.into());
        }
        if matches!(target, ApprovalTarget::Settings(None)) { return Ok(()) }
        let tool = call["op"].as_str().unwrap_or_default().replace('.', "_");
        let effect = soul2d_core::manifest::effect_of(&tool).filter(|effect| matches!(*effect, "file-write" | "project-reset" | "import" | "application-lifecycle"))
            .ok_or("The operation effect is missing from the command manifest")?;
        if let Some(request) = &self.request {
            if matches!(request.decision, ApprovalDecision::AllowedOnce) {
                self.request = None;
                return Ok(());
            }
        } else {
            self.request = Some(ApprovalRequest { call: call.clone(), target, document, effect, decision: ApprovalDecision::Pending,
                created: std::time::Instant::now() });
            self.defer_current = true;
        }
        Err(format!("Waiting for host approval of {}", call["op"].as_str().unwrap().replace('.', "_")))
    }

    fn resolved(&self) -> bool {
        self.request.as_ref().is_none_or(|request| !matches!(request.decision, ApprovalDecision::Pending))
    }

    fn refresh_destination(&mut self, path: Option<&Path>) {
        if let Some(request) = &mut self.request {
            if let ApprovalTarget::Settings(destination) = &request.target {
                if !matches!(request.decision, ApprovalDecision::Denied) && destination.as_deref() != path {
                    request.decision = ApprovalDecision::Stale;
                }
            }
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, path: Option<&Path>) {
        self.refresh_destination(path);
        let Some(request) = &mut self.request else { return };
        if matches!(request.decision, ApprovalDecision::Denied | ApprovalDecision::Stale) {
            return;
        }
        egui::Window::new("Coding agent operation approval").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(format!("Tool: {}", request.call["op"].as_str().unwrap_or_default().replace('.', "_")));
            ui.label(format!("Effect: {}", request.effect));
            match request.call["op"].as_str() {
                Some("editor.settings.set") => {
                    ui.label(format!("Setting: {}   Value: {}", request.call["key"].as_str().unwrap_or_default(), request.call["value"]));
                }
                Some("editor.shortcut.set") => {
                    ui.label(format!("Command: {}   Shortcut: {}", request.call["command"].as_str().unwrap_or_default(), request.call["shortcut"]));
                }
                Some("editor.panel.set") => {
                    ui.label(format!("Panel: {}   Visible: {}", request.call["panel"].as_str().unwrap_or_default(), request.call["visible"]));
                }
                Some("editor.layout.reset") => {
                    ui.label("Layout: Reset Layout (panel visibility, panel sizes and window positions)");
                }
                Some("editor.workspace.save" | "editor.workspace.apply" | "editor.workspace.delete") => {
                    ui.label(format!("Workspace: {}", request.call["name"].as_str().unwrap_or_default()));
                }
                _ => {},
            }
            match &request.target {
                ApprovalTarget::Quit(_) => {
                    ui.label("Target: this Soul2D editor window and this initiating ACP prompt");
                    ui.label("Allow this original request once. After its reply is written and this prompt finishes successfully, Soul2D stops this agent and rechecks whether it can close safely.");
                    ui.label("This does not save or discard work. Failed delivery or new work cancels Quit. The agent may be stopped while the editor stays open if the final check refuses.");
                },
                ApprovalTarget::Settings(Some(path)) => { ui.label(format!("Settings file: {}", path.display())); },
                ApprovalTarget::Output(path) => {
                    let kind = if path.is_dir() { "Output folder" } else { "Output file" };
                    ui.label(format!("{kind}: {}", path.display()));
                },
                ApprovalTarget::Outputs(paths) => {
                    ui.label(format!("Output files: {}", paths.len()));
                    egui::ScrollArea::vertical().max_height(180.0).show_rows(ui, ui.text_style_height(&egui::TextStyle::Body), paths.len(), |ui, rows| {
                        for row in rows { ui.label(paths[row].display().to_string()); }
                    });
                },
                ApprovalTarget::ExternalProcess(video) => {
                    ui.label(format!("Executable: {}", video.command.program));
                    ui.label(format!("Working directory: {}", video.directory.display()));
                    ui.label(format!("Animation: {}   Frames: {}   Size: {} x {}", video.plan.animation,
                        video.plan.frames(), video.plan.camera.width, video.plan.camera.height));
                    ui.label(format!("Frame rate: {}   Camera: {}", video.plan.plan.fps, video.plan.camera.name));
                    ui.label(format!("Primary output or pattern: {}", video.command.args.last().map(String::as_str).unwrap_or_default()));
                    ui.label(format!("Overwrite: {}", video.overwrite));
                    ui.label("This external encoder may create additional outputs. Failure or cancellation may leave partial files.");
                    egui::ScrollArea::vertical().id_salt("video-command-arguments").max_height(180.0)
                        .show(ui, |ui| { for argument in &video.command.args { ui.label(format!("{argument:?}")); } });
                    for warning in &video.audio_left_out { ui.label(warning); }
                },
                ApprovalTarget::Collection(plan) => {
                    ui.label(format!("Collection directory: {}", plan.directory.display()));
                    ui.label(format!("Document output: {}", plan.document.display()));
                    ui.label(format!("Overwrite: {}", plan.overwrite));
                    ui.label("Source files are read when the export runs. Failure can leave folders and already replaced files.");
                    egui::ScrollArea::vertical().id_salt("collection-copies").max_height(180.0)
                        .show_rows(ui, ui.text_style_height(&egui::TextStyle::Body), plan.copies.len(), |ui, rows| {
                            for row in rows {
                                let file = &plan.copies[row];
                                ui.label(format!("{} -> {}", file.source.display(), file.destination.display()));
                            }
                        });
                },
                ApprovalTarget::Gltf(plan) => {
                    ui.label(format!("Output file: {}", plan.target.display()));
                    ui.label(format!("Output bytes: {}   Overwrite: {}", plan.byte_len(), plan.overwrite));
                    ui.collapsing("Conversion report", |ui| {
                        egui::ScrollArea::vertical().id_salt("gltf-conversion-report").max_height(180.0).show(ui, |ui| {
                            ui.label(serde_json::to_string_pretty(&plan.report).unwrap_or_default());
                        });
                    });
                },
                ApprovalTarget::DepthPng(plan) => {
                    ui.label(format!("Depth PNG directory: {}", plan.directory.display()));
                    ui.label(format!("Overwrite: {}", plan.overwrite));
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for path in &plan.layers { ui.label(format!("PNG: {}", path.display())); }
                        ui.label(format!("Manifest: {}", plan.manifest.display()));
                    });
                    ui.label("A failed export removes new files; folders and files already replaced may remain.");
                },
                ApprovalTarget::Store(plan) => {
                    ui.label(format!("Preset store: {}", plan.target.display()));
                    ui.label(format!("{} entry: {}", if plan.deleting { "Delete" } else { "Save" }, plan.name));
                    if plan.replaced { ui.label("An existing entry will be replaced."); }
                },
                ApprovalTarget::NewDocument => { ui.label("Target: New document (replaces the current document)"); },
                ApprovalTarget::SourceFile(path) => { ui.label(format!("Input file: {}", path.display())); },
                ApprovalTarget::SourceDirectory(path) => { ui.label(format!("Input folder: {}", path.display())); },
                ApprovalTarget::Recovery(path) => {
                    ui.label(format!("Recovery snapshot: {}", request.call["id"].as_str().unwrap_or_default()));
                    ui.label(format!("Input file: {}", path.display()));
                }
                ApprovalTarget::Settings(None) => {},
            }
            if let Some(document) = &request.document {
                ui.label(format!("Source document: {}", document.path.as_ref()
                    .map(|path| path.display().to_string()).unwrap_or_else(|| "Untitled".into())));
                ui.label(format!("Document revision: {}", document.revision));
                if request.call["op"] == "render" { ui.label("Captures the current preview when this call runs."); }
            }
            if soul2d_core::editor_controller::replaces_document(&request.call) || request.call["op"] == "editor.recovery.restore" {
                ui.label("Replaces the current document. Source paths are read when this call runs.");
            }
            ui.label("The original call waits here. Allow applies this exact change once.");
            ui.label("Expires after two minutes or when this ACP turn ends. Deny blocks covered file writes, document replacement and Quit for this turn.");
            if ui.add_enabled(matches!(request.decision, ApprovalDecision::Pending), egui::Button::new("Allow once")).clicked() {
                request.decision = ApprovalDecision::AllowedOnce;
            }
            if ui.button("Deny changes this turn").clicked() {
                request.decision = ApprovalDecision::Denied;
            }
        });
    }
}

struct Run<W: Write> {
    id: u64,
    client: Client<W>,
    endpoint: Option<McpEndpoint>,
    /// The briefing went with the first prompt of this run.
    briefed: bool,
    closed: bool,
    host_approval: HostApproval,
    pending_reply: Option<(u64, PendingReply)>,
    next_call: u64,
}

impl<W: Write> Run<W> {
    fn clear_approval(&mut self, reason: &str) {
        self.host_approval.set_active(false);
        if let Some((_, pending)) = self.pending_reply.take() {
            if let Err(error) = pending.finish(&mut |message| RpcReply::plain(Some(json!({"jsonrpc":"2.0", "id":message["id"], "result":{
                "isError":true, "content":[{"type":"text", "text":reason}]}})))) {
                eprintln!("acp-mcp: response failed: {error}");
            }
        }
    }
}

pub struct AgentPanel<W: Write = ChildStdin> {
    enabled: bool,
    status: String,
    error: Option<String>,
    prompt: String,
    /// What the editor tells the agent with the first prompt of a session (`set_briefing`); none for a host
    /// without a document.
    briefing: Option<String>,
    /// A settings field changed and was not yet reported to the caller.
    settings_dirty: bool,
    launch: fn(&LaunchSpec) -> Result<Client<W>, String>,
    run: Option<Run<W>>,
    next_run: u64,
    #[cfg(test)]
    before_tick_for_test: Option<Box<dyn FnOnce() + Send>>,
}

impl AgentPanel<ChildStdin> {
    pub fn new() -> Self {
        Self::with_launcher(Client::spawn)
    }
}

impl Default for AgentPanel<ChildStdin> {
    fn default() -> Self {
        Self::new()
    }
}

fn mcp_entry(settings: &Settings, endpoint: &McpEndpoint) -> Result<McpHttp, String> {
    let mut mcp = McpHttp::from_settings(settings, endpoint.url())?;
    mcp.headers = authorized_headers(&mcp.headers, &endpoint.token);
    Ok(mcp)
}

impl<W: Write> AgentPanel<W> {
    /// A panel that starts agents with `launch` (the editor uses [`Client::spawn`]).
    pub fn with_launcher(launch: fn(&LaunchSpec) -> Result<Client<W>, String>) -> Self {
        Self {
            enabled: false,
            status: "Idle".to_string(),
            error: None,
            prompt: String::new(),
            briefing: None,
            settings_dirty: false,
            launch,
            run: None,
            next_run: 0,
            #[cfg(test)]
            before_tick_for_test: None,
        }
    }

    /// Bind the MCP endpoint, launch the agent from `ACP.Command` / `ACP.Workdir`, and send `initialize`.
    /// Does nothing while the panel is disabled.
    pub fn start(&mut self, settings: &Settings) {
        self.stop();
        if !self.enabled || self.run.is_some() {
            return;
        }
        let launched = (|| {
            let spec = LaunchSpec::from_settings(settings)?;
            let endpoint = McpEndpoint::bind()?;
            let mcp = mcp_entry(settings, &endpoint)?;
            let client = (self.launch)(&spec)?;
            Ok::<_, String>((client, mcp, endpoint))
        })();
        match launched {
            Ok((client, mcp, endpoint)) => self.attach(client, Some(mcp), Some(endpoint)),
            Err(error) => self.fail("Start failed", error),
        }
    }

    /// Restart the agent with an empty conversation.
    pub fn new_session(&mut self, settings: &Settings) {
        self.start(settings);
    }

    /// Take over a connected client (the real launch path and the tests both end here) and initialize it.
    pub fn attach(&mut self, mut client: Client<W>, mcp: Option<McpHttp>, endpoint: Option<McpEndpoint>) {
        self.stop();
        if self.run.is_some() { return; }
        let Some(next) = self.next_run.checked_add(1) else {
            self.fail("Start failed", "ACP run identity exhausted".into());
            return;
        };
        let id = self.next_run;
        self.next_run = next;
        client.set_mcp_http(mcp);
        self.error = None;
        match client.initialize() {
            Ok(()) => self.status = "Initializing...".to_string(),
            Err(error) => self.fail("Initialize failed", error),
        }
        self.run = Some(Run { id, client, endpoint, briefed: false, closed: false,
            host_approval: HostApproval::default(), pending_reply: None, next_call: 0 });
    }

    #[cfg(test)]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            self.stop();
            if self.run.is_none() { self.status = "Idle".to_string(); }
        }
        self.enabled = enabled;
    }

    #[cfg(test)]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn prompt_mut(&mut self) -> &mut String {
        &mut self.prompt
    }

    #[cfg(test)]
    pub(crate) fn before_next_tick_for_test(&mut self, hook: impl FnOnce() + Send + 'static) {
        assert!(self.before_tick_for_test.is_none());
        self.before_tick_for_test = Some(Box::new(hook));
    }

    /// The URL offered to the running agent (the token is never exposed).
    pub fn mcp_url(&self) -> Option<&str> {
        self.run.as_ref()?.endpoint.as_ref().map(McpEndpoint::url)
    }

    pub fn state(&self) -> PanelState {
        let client = self.run.as_ref().map(|run| &run.client);
        PanelState {
            enabled: self.enabled,
            has_client: client.is_some(),
            closed: self.run.as_ref().is_some_and(|run| run.closed),
            ready: client.is_some_and(Client::is_initialized),
            turn_active: client.is_some_and(Client::prompt_active),
            cancelling: client.is_some_and(|client| client.conversation.cancelling()),
            awaiting_permission: client.is_some_and(|client| !client.conversation.permissions.is_empty()),
            prompt_present: !self.prompt.trim().is_empty(),
        }
    }

    pub fn controls(&self) -> Controls {
        controls(&self.state())
    }

    fn fail(&mut self, status: &str, error: String) {
        self.status = status.to_string();
        self.error = Some(error_head(&error).to_string());
    }

    /// Stop the agent (waits briefly for it to exit, then kills it) and close the MCP endpoint.
    pub fn stop(&mut self) {
        self.stop_with(Client::shutdown);
    }

    fn stop_with(&mut self, shutdown: impl FnOnce(&mut Client<W>) -> Result<Option<std::process::ExitStatus>, String>) {
        if let Some(run) = self.run.as_mut() {
            run.closed = true;
            run.clear_approval("The ACP session stopped before the operation was approved");
            run.endpoint = None;
            if let Err(error) = shutdown(&mut run.client) {
                self.fail("Stop failed; client ownership retained", error);
                return;
            }
            self.run = None;
            self.status = "Stopped".into();
        }
        self.error = None;
    }

    pub(crate) fn owner(&self) -> Option<u64> {
        self.run.as_ref().map(|run| run.id)
    }

    pub(crate) fn invalidate_quit(&mut self, target: &AcpQuitTarget) {
        if let Some(run) = self.run.as_mut().filter(|run| run.id == target.run) {
            run.host_approval.invalidate_quit(target);
        }
    }

    pub(crate) fn quit_status(&self, target: &AcpQuitTarget, admitted_at: Option<Instant>) -> Result<bool, String> {
        let run = self.run.as_ref().filter(|run| run.id == target.run).ok_or("ACP client changed or stopped")?;
        if run.closed || run.client.permission_epoch() != target.permissions {
            return Err("ACP disconnected or received a new permission request".into());
        }
        if admitted_at.is_none() && (run.host_approval.quit_review().as_ref() != Some(target)
            || run.host_approval.request.as_ref().is_none_or(|request|
                !matches!(request.decision, ApprovalDecision::Pending | ApprovalDecision::AllowedOnce))) {
            return Err("Lifecycle review expired, was denied or was invalidated".into());
        }
        if run.client.active_prompt() == Some(target.prompt) { return Ok(false); }
        match (admitted_at, run.client.prompt_completion()) {
            (Some(admitted), Some(done)) if done.id == target.prompt && done.outcome == PromptOutcome::Completed
                && done.received_at > admitted => Ok(true),
            _ => Err("Initiating ACP prompt ended, changed or was cancelled".into()),
        }
    }

    pub(crate) fn release_for_quit(&mut self, target: &AcpQuitTarget, admitted_at: Instant) -> QuitRelease {
        self.pump();
        match self.quit_status(target, Some(admitted_at)) {
            Ok(false) => QuitRelease::Waiting,
            Err(reason) => QuitRelease::Invalid(reason),
            Ok(true) => {
                if !self.run.as_mut().unwrap().client.seal_permission_intake(target.permissions) {
                    return QuitRelease::Invalid("ACP permission or disconnection raced with release".into());
                }
                self.stop();
                if self.run.is_none() { QuitRelease::Released }
                else { QuitRelease::Invalid(self.error.clone().unwrap_or_else(|| "ACP shutdown was not confirmed".into())) }
            }
        }
    }

    /// The text that goes with the first prompt of each session as a second content block: the MCP server on
    /// offer, the skill resources and the selection at that moment. Set by the editor before each `show`.
    pub fn set_briefing(&mut self, briefing: Option<String>) {
        self.briefing = briefing;
    }

    /// Send the prompt box to the agent, with the briefing once per run.
    pub fn send(&mut self) {
        if !self.controls().send {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        let briefing = if run.briefed { None } else { self.briefing.as_deref() };
        match run.client.send_prompt_with(&self.prompt, briefing) {
            Ok(_) => {
                run.briefed |= briefing.is_some();
                run.clear_approval("A new ACP prompt invalidated the pending settings change");
                self.prompt.clear();
                self.status = "Waiting for the agent".to_string();
                self.error = None;
            }
            Err(error) => self.fail("Send failed", error),
        }
    }

    pub fn cancel(&mut self) {
        if !self.controls().cancel {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        run.clear_approval("The ACP prompt was cancelled before the operation was approved");
        match run.client.cancel() {
            Ok(()) => self.status = "Cancelling".to_string(),
            Err(error) => self.fail("Cancel failed", error),
        }
    }

    /// The permission request shown to the user: the oldest one that is waiting.
    pub fn permission(&self) -> Option<PermissionRequest> {
        self.run.as_ref()?.client.conversation.permissions.first().cloned()
    }

    /// Press button `choice` of [`permission_view`] for the current request.
    pub fn answer_permission(&mut self, choice: usize) {
        let Some(request) = self.permission() else { return };
        let view = permission_view(&request);
        let Some(chosen) = view.choices.get(choice).filter(|chosen| chosen.selectable && view.answerable) else {
            return;
        };
        let Some(run) = self.run.as_mut() else { return };
        match run.client.answer_permission(&request.id, request.generation, chosen.option_id.as_deref()) {
            Ok(true) => self.status = "Permission answered".to_string(),
            Ok(false) => self.status = "Permission request expired".to_string(),
            Err(error) => self.fail("Permission answer failed", error),
        }
    }

    /// Fold everything the agent has sent into the panel. Never blocks.
    pub fn pump(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        let items = run.client.poll();
        for item in items {
            match item {
                Incoming::Initialized => self.status = "Initialized".to_string(),
                Incoming::InitFailed(error) => self.fail("Initialize failed", error),
                Incoming::SessionReady(_) => self.status = "Session ready".to_string(),
                Incoming::SessionFailed(error) => self.fail("Session failed", error),
                Incoming::PromptFinished(done) => match done.outcome {
                    PromptOutcome::Failed(error) => self.fail("Prompt failed", error),
                    PromptOutcome::Completed => self.status = "Done (end_turn)".into(),
                    PromptOutcome::Cancelled => self.status = "Done (cancelled)".into(),
                    PromptOutcome::Stopped(reason) => self.status = reason.map(|reason| format!("Done ({reason})")).unwrap_or_else(|| "Done".into()),
                },
                Incoming::Error(error) | Incoming::Malformed(error) => self.error = Some(error_head(&error).to_string()),
                Incoming::Disconnected(_) => {
                    self.status = "Agent stream closed".to_string();
                    if let Some(run) = self.run.as_mut() {
                        run.closed = true;
                        run.endpoint = None;
                    }
                }
                Incoming::Update(_) | Incoming::Permission(_) | Incoming::PermissionCancelled(_) | Incoming::Ping(_)
                | Incoming::Other(_) => {}
            }
        }
        self.sync_approval();
    }

    fn sync_approval(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        let active = run.client.prompt_active() && !run.client.conversation.cancelling() && !run.closed;
        if !active {
            run.clear_approval("The ACP prompt ended before the operation was approved");
        } else if run.pending_reply.is_some() && run.host_approval.request.as_ref().is_some_and(|request|
            matches!(request.decision, ApprovalDecision::Pending | ApprovalDecision::AllowedOnce)
                && request.created.elapsed() >= HOST_APPROVAL_TIMEOUT) {
            run.clear_approval("Host approval expired; the operation was not applied");
        }
        run.host_approval.set_active(active);
    }

    pub fn host_approval_window(&mut self, ctx: &egui::Context, path: Option<&Path>) {
        if let Some(run) = self.run.as_mut() {
            run.host_approval.show(ctx, path);
        }
    }

    /// Serve the MCP connections that are waiting. Each JSON-RPC message goes to `handler`, which the editor
    /// routes through its controller so the agent meets the same file-operation gates as other clients.
    /// `handler` is only called when a request arrives. Returns how many connections were served.
    pub(crate) fn serve_mcp(&mut self, path: Option<&Path>, handler: &mut dyn FnMut(&Value, &mut HostApproval) -> RpcReply) -> usize {
        self.sync_approval();
        let Some(run) = self.run.as_mut() else { return 0 };
        let Some(endpoint) = run.endpoint.as_ref() else { return 0 };
        run.host_approval.refresh_destination(path);
        let mut served = 0;
        if run.host_approval.resolved() {
            if let Some((call, pending)) = run.pending_reply.take() {
                if let Err(error) = pending.finish(&mut |message| {
                    run.host_approval.defer_current = false;
                    run.host_approval.invocation = run.client.active_prompt().map(|prompt| AcpQuitTarget {
                        run: run.id, prompt, call, permissions: run.client.permission_epoch() });
                    handler(message, &mut run.host_approval)
                }) {
                    eprintln!("acp-mcp: response failed: {error}");
                }
                if run.host_approval.request.as_ref().is_some_and(|request|
                    matches!(request.decision, ApprovalDecision::Stale | ApprovalDecision::AllowedOnce)) {
                    run.host_approval.request = None;
                }
                served += 1;
            }
        }
        let mut streams = endpoint.accept_pending(MAX_CONNECTIONS_PER_FRAME);
        if streams.is_empty() {
            return served;
        }
        served += streams.len();
        let token = endpoint.token.clone();
        for stream in streams.drain(..) {
            let mut staged = None;
            let mut occurrence = 0;
            let pending = handle_connection_deferred(stream, Some(&token), MCP_BUDGET, &mut |message| {
                run.host_approval.defer_current = false;
                occurrence = run.next_call;
                run.host_approval.invocation = run.next_call.checked_add(1).and_then(|next| {
                    run.next_call = next;
                    run.client.active_prompt().map(|prompt| AcpQuitTarget {
                        run: run.id, prompt, call: occurrence, permissions: run.client.permission_epoch() })
                });
                let response = handler(message, &mut run.host_approval);
                if run.host_approval.defer_current {
                    staged = run.host_approval.request.take();
                    HttpDispatch::Defer
                } else {
                    HttpDispatch::Reply(response)
                }
            });
            if let Err(error) = &pending { eprintln!("acp-mcp: response failed: {error}"); }
            if let (Ok(Some(pending)), Some(request)) = (pending, staged) {
                run.host_approval.request = Some(request);
                run.pending_reply = Some((occurrence, pending));
            } else if run.host_approval.defer_current {
                run.host_approval.request = None;
            }
            run.host_approval.defer_current = false;
            run.host_approval.invocation = None;
        }
        served
    }

    /// Process lifecycle events before constructing the view used to serve MCP, drawn or not.
    pub(crate) fn tick(&mut self, ctx: &egui::Context) {
        #[cfg(test)]
        if let Some(hook) = self.before_tick_for_test.take() { hook(); }
        self.pump();
        if self.run.as_ref().is_some_and(|run| !run.closed) {
            ctx.request_repaint_after(REPAINT_EVERY);
        }
    }

    // -- drawing --------------------------------------------------------------------------------

    /// Draw the panel. The caller must also tick and serve MCP every frame, drawn or not.
    /// Returns true when a settings field was edited and the caller should write the settings file.
    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings, snapshot: &Value) -> bool {
        let mut enabled = self.enabled;
        ui.checkbox(&mut enabled, "Enable coding agent (ACP)")
            .on_hover_text("Runs the configured program and offers it a local, token-protected MCP endpoint.");
        if enabled != self.enabled {
            self.set_enabled(enabled);
        }
        if !self.enabled {
            if self.run.is_some() {
                ui.weak("Off. An ACP client is still owned because shutdown failed; enable the panel to retry Stop.");
            } else {
                ui.weak("Off: no agent is started and no port is opened.");
            }
            return false;
        }
        let saved = self.settings_fields(ui, settings);
        self.buttons(ui, settings);
        self.status_line(ui);
        self.error_view(ui);
        ui.separator();
        self.conversation_view(ui);
        self.plan_view(ui);
        self.permission_prompt(ui);
        self.split_prompt(ui, snapshot);
        self.prompt_row(ui);
        saved
    }

    fn split_prompt(&mut self, ui: &mut egui::Ui, snapshot: &Value) {
        if snapshot["documentFormat"] != "inx" || snapshot["partSplit"]["open"] != true {
            return;
        }
        let http = self.run.as_ref().is_some_and(|run|
            run.client.is_initialized() && run.client.agent_supports_mcp_http());
        let state = self.state();
        let available = http && self.prompt.trim().is_empty()
            && !state.turn_active && !state.cancelling && !state.awaiting_permission;
        if ui.add_enabled(available, egui::Button::new("Split this image")).clicked() {
            self.prompt = format!(
                "Split the illustration currently loaded in this editor into proposed Parts.\n\
                 Use only the existing Soul2D MCP server offered to this ACP session; skip the Skill's host setup.\n\
                 First read resource://soul2d/part-split/plan and note its documentRevision and draftRevision.\n\
                 Inspect resource://soul2d/part-split/source and the overlays, propose masks, and review coverage.\n\
                 Leave the result as a draft: do not call split_apply. Ask the person to inspect Part split review\n\
                 and press Apply reviewed split. This replaces the Skill's raw apply step for ACP.\n\
                 Do not invoke external segmentation or inpainting providers.\n\
                 Follow this Skill for the taxonomy, mask operations and failure cases:\n\n{}",
                include_str!("../../.cursor/skills/split-illustration/SKILL.md")
            );
        }
        if !http {
            ui.weak("Start an ACP agent with HTTP MCP support to prepare a split request.");
        }
        ui.weak("Prepares text only. Review it before Send; apply the result in Part split review.");
    }

    fn settings_fields(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        let mut saved = false;
        let command = settings.get_or("ACP.Command", String::new()).unwrap_or_default();
        let fallback = settings.get::<String>("ACP.Workingdir").unwrap_or_default();
        let workdir = settings.get_or("ACP.Workdir", fallback).unwrap_or_default();
        for (label, key, mut value) in [
            ("Command", "ACP.Command", command),
            ("Working Dir (optional)", "ACP.Workdir", workdir),
        ] {
            let caption = ui.label(label);
            let response = ui.text_edit_singleline(&mut value).labelled_by(caption.id);
            if response.changed() {
                match settings.set(key, value) {
                    Ok(()) => self.settings_dirty = true,
                    Err(error) => self.error = Some(error),
                }
            }
            if response.lost_focus() && self.settings_dirty {
                self.settings_dirty = false;
                saved = true;
            }
        }
        saved
    }

    fn buttons(&mut self, ui: &mut egui::Ui, settings: &Settings) {
        let controls = self.controls();
        ui.horizontal(|ui| {
            if ui.add_enabled(controls.start, egui::Button::new("Start")).clicked() {
                self.start(settings);
            }
            if ui.add_enabled(controls.stop, egui::Button::new("Stop")).clicked() {
                self.stop();
            }
            if ui
                .add_enabled(controls.new_session, egui::Button::new("New session"))
                .on_hover_text("Restart the agent with an empty conversation")
                .clicked()
            {
                self.new_session(settings);
            }
        });
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Status: {}", self.status));
            let usage = self.run.as_ref().map(|run| run.client.conversation.usage_text.as_str()).unwrap_or("");
            if !usage.is_empty() {
                ui.weak(format!("Usage: {usage}"));
            }
            if let Some(url) = self.mcp_url() {
                ui.weak(format!("MCP: {url}"));
            }
        });
    }

    fn error_view(&self, ui: &mut egui::Ui) {
        let Some(error) = &self.error else { return };
        ui.colored_label(egui::Color32::LIGHT_RED, format!("Error: {error}"));
        let stderr = self.run.as_ref().map(|run| run.client.stderr_text()).unwrap_or_default();
        let tail = stderr_tail(&stderr, STDERR_TAIL_LINES);
        if !tail.is_empty() {
            egui::CollapsingHeader::new("Agent stderr").default_open(true).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(tail).monospace()).wrap());
            });
        }
    }

    fn conversation_view(&self, ui: &mut egui::Ui) {
        let Some(run) = &self.run else { return };
        egui::ScrollArea::vertical().id_salt("agent_log").max_height(320.0).stick_to_bottom(true).show(ui, |ui| {
            let blocks = &run.client.conversation.blocks;
            for (index, block) in blocks.iter().enumerate() {
                let view = block_view(block, index + 1 == blocks.len());
                draw_block(ui, index, &view);
            }
        });
    }

    fn plan_view(&self, ui: &mut egui::Ui) {
        let Some(plan) = self.run.as_ref().and_then(|run| run.client.conversation.plan.as_ref()) else { return };
        let lines = plan_lines(plan);
        if lines.is_empty() {
            return;
        }
        ui.separator();
        ui.strong("Plan");
        let color = status_color(if plan.status.is_empty() { "in_progress" } else { &plan.status });
        for line in lines {
            ui.colored_label(color, line);
        }
    }

    fn permission_prompt(&mut self, ui: &mut egui::Ui) {
        let Some(request) = self.permission() else { return };
        let view = permission_view(&request);
        ui.separator();
        ui.label(&view.heading);
        if !view.answerable {
            ui.colored_label(egui::Color32::LIGHT_RED, "Cannot respond: missing request id");
        }
        let mut pressed = None;
        for (index, choice) in view.choices.iter().enumerate() {
            let button = egui::Button::new(&choice.shown);
            let enabled = choice.selectable && view.answerable;
            let mut response = ui.add_enabled(enabled, button);
            if choice.shown != choice.label {
                response = response.on_hover_text(&choice.label).on_disabled_hover_text(&choice.label);
            }
            if enabled && response.clicked() {
                pressed = Some(index);
            }
        }
        if let Some(index) = pressed {
            self.answer_permission(index);
        }
    }

    fn prompt_row(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        let caption = ui.label("User message:");
        let mut submit = false;
        let controls = self.controls();
        ui.horizontal(|ui| {
            let response = ui.text_edit_singleline(&mut self.prompt).labelled_by(caption.id);
            submit = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ui.add_enabled(controls.send, egui::Button::new("Send")).clicked() {
                submit = true;
            }
            if ui.add_enabled(controls.cancel, egui::Button::new("Cancel")).clicked() {
                self.cancel();
            }
        });
        if submit {
            self.send();
        }
    }
}

fn draw_block(ui: &mut egui::Ui, index: usize, view: &BlockView) {
    let body = |ui: &mut egui::Ui| {
        for line in &view.body {
            let text = egui::RichText::new(line);
            let text = if view.kind == RoleKind::Error { text.color(egui::Color32::LIGHT_RED) } else { text };
            ui.add(egui::Label::new(text).wrap());
        }
    };
    if !view.collapsible {
        ui.strong(&view.header);
        body(ui);
        return;
    }
    let mut title = egui::RichText::new(if view.status.is_empty() {
        view.header.clone()
    } else {
        format!("{} ({})", view.header, view.status)
    });
    if view.kind == RoleKind::Tool {
        title = title.color(status_color(&view.status));
    }
    if view.kind == RoleKind::Error {
        title = title.color(egui::Color32::LIGHT_RED);
    }
    egui::CollapsingHeader::new(title).id_salt(("agent_block", index)).default_open(view.open).show(ui, body);
}

// ---------------------------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use soul2d_core::Session;
    use std::io::{pipe, BufRead, BufReader, Read, PipeWriter};
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(10);

    // -------------------------------------------------------------------- pure functions

    #[test]
    fn mcp_urls_bracket_ipv6_hosts_only() {
        assert_eq!(mcp_url("127.0.0.1:8088".parse().unwrap()), "http://127.0.0.1:8088/mcp");
        assert_eq!(mcp_url("[::1]:4000".parse().unwrap()), "http://[::1]:4000/mcp");
        assert_eq!(mcp_url("[fe80::1]:1".parse().unwrap()), "http://[fe80::1]:1/mcp");
    }

    #[test]
    fn tokens_are_64_lowercase_hex_characters_and_differ() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        let first = random_token().unwrap();
        let second = random_token().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')), "{first}");
        assert_ne!(first, second);
        assert_ne!(first, "0".repeat(64));
    }

    #[test]
    fn our_authorization_header_replaces_a_configured_one() {
        let configured = json!([{"name": "X", "value": "1"}, {"name": "authorization", "value": "Bearer old"}]);
        assert_eq!(
            authorized_headers(&configured, "tok"),
            json!([{"name": "X", "value": "1"}, {"name": "Authorization", "value": "Bearer tok"}])
        );
        assert_eq!(
            authorized_headers(&json!("not an array"), "t"),
            json!([{"name": "Authorization", "value": "Bearer t"}])
        );
    }

    fn ready() -> PanelState {
        PanelState { enabled: true, has_client: true, ready: true, prompt_present: true, ..PanelState::default() }
    }

    #[test]
    fn control_states_follow_the_client_state() {
        let off = controls(&PanelState::default());
        assert_eq!(off, Controls::default(), "a disabled panel offers nothing");

        let idle = controls(&PanelState { enabled: true, ..PanelState::default() });
        assert_eq!(idle, Controls { start: true, new_session: true, ..Controls::default() });

        let initializing = controls(&PanelState { enabled: true, has_client: true, prompt_present: true, ..PanelState::default() });
        assert_eq!(initializing, Controls { stop: true, new_session: true, ..Controls::default() });

        assert_eq!(controls(&ready()), Controls { stop: true, new_session: true, send: true, ..Controls::default() });
        let empty_prompt = PanelState { prompt_present: false, ..ready() };
        assert!(!controls(&empty_prompt).send);

        let running = controls(&PanelState { turn_active: true, ..ready() });
        assert_eq!(running, Controls { stop: true, new_session: true, cancel: true, ..Controls::default() });
        let cancelling = controls(&PanelState { turn_active: true, cancelling: true, ..ready() });
        assert!(!cancelling.cancel && !cancelling.send);
        let awaiting = controls(&PanelState { turn_active: true, awaiting_permission: true, ..ready() });
        assert!(awaiting.cancel && !awaiting.send);

        let closed = controls(&PanelState { closed: true, ready: false, ..ready() });
        assert!(closed.start && closed.stop && !closed.send, "a dead agent can be restarted or cleared");
    }

    fn block(kind: RoleKind, role: &str, lines: &[&str]) -> Block {
        Block { kind, role: role.into(), lines: lines.iter().map(|line| line.to_string()).collect(), ..Block::default() }
    }

    #[test]
    fn blank_edges_and_long_lines_are_trimmed() {
        let lines: Vec<String> = ["", "  ", "a", "", "b", " "].iter().map(|line| line.to_string()).collect();
        assert_eq!(trim_blank_edges(&lines), ["a", "", "b"]);
        assert!(trim_blank_edges(&["".to_string()]).is_empty());
        let long = "é".repeat(MAX_DRAW_CHARS + 5);
        let clipped = clip_line(&long);
        assert_eq!(clipped, format!("{} ... (display truncated)", "é".repeat(MAX_DRAW_CHARS)));
        assert_eq!(clip_line("short"), "short");
    }

    #[test]
    fn plain_roles_show_their_trimmed_lines_under_the_role() {
        let you = block_view(&block(RoleKind::You, "You", &["", "hello", "world", ""]), true);
        assert_eq!(
            (you.header.as_str(), you.body.as_slice(), you.collapsible),
            ("You", ["hello".to_string(), "world".to_string()].as_slice(), false)
        );
        let assistant = block_view(&block(RoleKind::AssistantFinal, "Assistant", &["done"]), false);
        assert_eq!((assistant.header.as_str(), assistant.collapsible), ("Assistant", false));
        let error = block_view(&block(RoleKind::Error, "Error", &["boom"]), true);
        assert_eq!((error.collapsible, error.open, error.body), (true, true, vec!["boom".to_string()]));
        let older = block_view(&block(RoleKind::Other, "Mode", &["code"]), false);
        assert_eq!((older.header.as_str(), older.open), ("Mode", false));
    }

    #[test]
    fn thinking_is_folded_under_its_first_line() {
        let view = block_view(
            &block(RoleKind::AssistantThinking, "Assistant (thinking)", &["", "Plan the edit", "", "step one", ""]),
            true,
        );
        assert_eq!(view.header, "Assistant (thinking): Plan the edit");
        assert_eq!(view.body, ["step one"]);
        assert!(view.collapsible && !view.open);
    }

    #[test]
    fn tool_blocks_show_title_status_and_the_focused_payload() {
        let tool = Block {
            kind: RoleKind::Tool,
            role: "Tool#t1".into(),
            title: "Read file\nextra".into(),
            lines: vec!["[t1] Read file (status: completed)".into()],
            status: "completed".into(),
            tool_id: "t1".into(),
            payload_merged: Some(json!({
                "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read file",
                "rawInput": {"path": "a.txt"},
                "other": {"deep": {"content": "kept"}, "dropped": 1},
            })),
        };
        let view = block_view(&tool, true);
        assert_eq!(view.header, "tool: Read file ...");
        assert_eq!(view.status, "completed");
        assert_eq!(
            view.body,
            [
                "{",
                "  \"other\": {",
                "    \"deep\": {",
                "      \"content\": \"kept\"",
                "    }",
                "  },",
                "  \"rawInput\": {",
                "    \"path\": \"a.txt\"",
                "  }",
                "}"
            ]
        );
        // Without any focus key the whole payload is shown; without a payload the text lines are.
        assert_eq!(tool_detail_lines(&json!({"a": 1})), ["{", "  \"a\": 1", "}"]);
        let no_payload = Block { payload_merged: None, ..tool };
        assert_eq!(block_view(&no_payload, false).body, ["[t1] Read file (status: completed)"]);
    }

    #[test]
    fn plan_lines_are_numbered_and_status_colours_differ() {
        let plan = Plan { lines: vec!["[high] Inspect (pending)".into(), "[low] Save (completed)".into()], ..Plan::default() };
        assert_eq!(plan_lines(&plan), ["1. [high] Inspect (pending)", "2. [low] Save (completed)"]);
        let colours: Vec<_> = ["in_progress", "completed", "failed", "queued", "cancelled", ""]
            .iter()
            .map(|status| status_color(status))
            .collect();
        for (index, colour) in colours.iter().enumerate() {
            assert!(colours[index + 1..].iter().all(|other| other != colour), "{index}");
        }
    }

    fn request(id: Value, params: Value) -> PermissionRequest {
        PermissionRequest { id, reason: "Run rm".into(), params, generation: 1 }
    }

    #[test]
    fn permission_choices_come_from_the_options_plus_deny() {
        let view = permission_view(&request(
            json!(7),
            json!({"options": [
                {"optionId": "allow:once", "name": "Allow just this one time please"},
                {"id": "reject", "kind": "reject_once"},
                {"name": "No id"},
                "not an object",
            ]}),
        ));
        assert_eq!(view.heading, "Permission requested: Run rm");
        assert!(view.answerable);
        let summary: Vec<_> = view
            .choices
            .iter()
            .map(|choice| (choice.shown.as_str(), choice.option_id.as_deref(), choice.selectable))
            .collect();
        assert_eq!(
            summary,
            [
                ("Allow just this...", Some("allow:once"), true),
                ("reject_once", Some("reject"), true),
                ("No id", None, false),
                ("Option", None, false),
                ("Deny", None, true),
            ]
        );
        assert_eq!(view.choices[0].label, "Allow just this one time please");
    }

    #[test]
    fn a_request_without_options_offers_allow_and_deny_and_without_id_cannot_be_answered() {
        let mut no_options = request(json!("abc"), json!({"reason": "x"}));
        no_options.reason.clear();
        let view = permission_view(&no_options);
        assert_eq!(view.heading, "Permission requested: (no reason)");
        let choices: Vec<_> = view.choices.iter().map(|c| (c.label.as_str(), c.option_id.as_deref())).collect();
        assert_eq!(choices, [("Allow", Some("allow-once")), ("Deny", None)]);
        assert!(!permission_view(&request(Value::Null, json!({}))).answerable);
        assert_eq!(permission_view(&request(json!(1), json!({"options": []}))).choices.len(), 2);
    }

    #[test]
    fn stderr_tail_keeps_the_newest_non_empty_lines_and_errors_lose_the_dump() {
        assert_eq!(stderr_tail("a\n\nb\nc\nd\n", 2), "c\nd");
        assert_eq!(stderr_tail("", 3), "");
        assert_eq!(error_head("initialize error: nope | stderr: line1\nline2"), "initialize error: nope");
        assert_eq!(error_head("plain"), "plain");
        assert_eq!(ellipsize("exactly15chars!", 15), "exactly15chars!");
        assert_eq!(ellipsize("日本語のラベルです", 3), "日本語...");
    }

    // -------------------------------------------------------------------- panel over pipes

    struct Agent {
        to_client: PipeWriter,
        lines: Receiver<String>,
    }

    impl Agent {
        fn send(&mut self, message: Value) {
            writeln!(self.to_client, "{message}").unwrap();
            self.to_client.flush().unwrap();
        }

        fn recv(&self) -> Value {
            serde_json::from_str(&self.lines.recv_timeout(WAIT).expect("the panel wrote nothing")).unwrap()
        }
    }

    fn connect() -> (Client<PipeWriter>, Agent) {
        let (agent_stdin, client_writer) = pipe().unwrap();
        let (client_reader, agent_stdout) = pipe().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(agent_stdin).lines() {
                if line.is_err() || tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        (Client::new(client_reader, client_writer), Agent { to_client: agent_stdout, lines })
    }

    fn no_launch_panel() -> AgentPanel<PipeWriter> {
        let mut panel = AgentPanel::with_launcher(|_| Err("no launcher in this test".to_string()));
        panel.set_enabled(true);
        panel
    }

    fn pump_until(panel: &mut AgentPanel<PipeWriter>, done: impl Fn(&AgentPanel<PipeWriter>) -> bool) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            panel.pump();
            if done(panel) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out; status {:?}, error {:?}, state {:?}", panel.status(), panel.error(), panel.state());
    }

    fn panel_over_pipes() -> (AgentPanel<PipeWriter>, Agent) {
        panel_over_pipes_with_http(false)
    }

    fn panel_over_pipes_with_http(http: bool) -> (AgentPanel<PipeWriter>, Agent) {
        let (client, mut agent) = connect();
        let mut panel = no_launch_panel();
        panel.attach(client, None, None);
        assert_eq!(panel.status(), "Initializing...");
        let init = agent.recv();
        assert_eq!(init["method"], "initialize");
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {
            "protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": http}}
        }}));
        pump_until(&mut panel, |panel| panel.state().ready);
        assert_eq!(panel.status(), "Initialized");
        (panel, agent)
    }

    #[test]
    fn coding_agent_tick_keeps_repainting_without_drawing_and_stops_after_disconnect() {
        let (mut panel, agent) = panel_over_pipes();
        let ctx = egui::Context::default();
        let mut delay = Duration::MAX;
        for _ in 0..8 {
            let output = ctx.run(Default::default(), |ctx| panel.tick(ctx));
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            assert!(delay <= REPAINT_EVERY);
        }
        assert!(delay > Duration::ZERO && delay <= REPAINT_EVERY);
        drop(agent);
        pump_until(&mut panel, |panel| panel.state().closed);
        assert!(panel.state().has_client);
        for _ in 0..8 {
            let output = ctx.run(Default::default(), |ctx| panel.tick(ctx));
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        }
        assert_eq!(delay, Duration::MAX);
        panel.stop();
    }

    #[test]
    fn remote_quit_disconnected_acp_retains_its_client_until_explicit_stop() {
        let (mut panel, agent) = panel_over_pipes();
        drop(agent);
        pump_until(&mut panel, |panel| panel.state().closed);
        assert!(panel.state().has_client, "stream EOF does not release owned client/process state");
        panel.stop();
        assert!(!panel.state().has_client);
    }

    #[test]
    fn split_entry_prepares_a_skill_prompt_without_sending_or_overwriting_text() {
        use crate::editor_test_support::{has, Harness};
        use base64::Engine;
        use egui::accesskit::Role;
        let (mut panel, mut agent) = panel_over_pipes_with_http(true);
        let mut settings = Settings::empty();
        let endpoint = McpEndpoint::bind().unwrap();
        let entry = mcp_entry(&settings, &endpoint).unwrap();
        let port = endpoint.url().rsplit(':').next().unwrap().trim_end_matches("/mcp").parse().unwrap();
        let token = endpoint.token.clone();
        panel.run.as_mut().unwrap().client.set_mcp_http(Some(entry));
        panel.run.as_mut().unwrap().endpoint = Some(endpoint);
        let dir = crate::editor_test_support::temp_dir("acp-split-entry");
        let path = dir.join("source.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 8, 7, 255])).save(&path).unwrap();
        let mut session = Session::new();
        assert_eq!(session.handle_request(&json!({"op":"split.open", "path":path}))["succeeded"], true);
        let snapshot = session.snapshot();
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1800.0);
        let output = harness.click(Role::Button, "Split this image", |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { panel.show(ui, &mut settings, &snapshot); });
        });
        assert!(has(&output, Role::Button, "Split this image"));
        assert!(panel.prompt.contains("resource://soul2d/part-split/source"));
        assert!(panel.prompt.contains("resource://soul2d/part-split/plan"));
        assert!(panel.prompt.contains("do not call split_apply"));
        assert!(panel.prompt.contains("HairBack") && panel.prompt.contains("Eyewhite.L"));
        assert!(!panel.prompt.contains("data:image/") && !panel.prompt.contains("\"blob\":"));
        assert!(!panel.state().turn_active);
        assert!(agent.lines.try_recv().is_err(), "preparing sends no ACP request");
        let prepared = panel.prompt.clone();
        panel.prompt = "Keep my existing text".to_owned();
        harness.click(Role::Button, "Split this image", |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { panel.show(ui, &mut settings, &snapshot); });
        });
        assert_eq!(panel.prompt, "Keep my existing text");
        panel.prompt = prepared.clone();
        panel.send();
        let new_session = agent.recv();
        assert_eq!(new_session["method"], "session/new");
        assert_eq!(new_session["params"]["mcpServers"][0]["type"], "http");
        assert_eq!(new_session["params"]["mcpServers"][0]["url"], panel.mcp_url().unwrap());
        assert_eq!(new_session["params"]["mcpServers"][0]["headers"][0]["name"], "Authorization");
        assert!(new_session["params"]["mcpServers"][0]["headers"][0]["value"]
            .as_str().unwrap().starts_with("Bearer "));
        agent.send(json!({"jsonrpc":"2.0", "id":new_session["id"], "result":{"sessionId":"split"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        let sent = agent.recv();
        assert_eq!(sent["method"], "session/prompt");
        assert_eq!(sent["params"]["prompt"][0]["text"], prepared);
        for suffix in ["source", "plan"] {
            let call = json!({"jsonrpc":"2.0", "id":2, "method":"resources/read",
                "params":{"uri":format!("resource://soul2d/part-split/{suffix}")}}).to_string();
            let auth = token.clone();
            let reply = serve_while(&mut panel, &mut session, move || http_post(port, Some(&auth), &call));
            assert!(reply.starts_with("HTTP/1.1 200 OK"));
            let response: Value = serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap();
            let content = &response["result"]["contents"][0];
            if suffix == "source" {
                assert_eq!(content["mimeType"], "image/png");
                let bytes = base64::engine::general_purpose::STANDARD.decode(content["blob"].as_str().unwrap()).unwrap();
                let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
                assert_eq!(image.dimensions(), (8, 8));
                assert_eq!(image.into_raw(), [9, 8, 7, 255].repeat(64));
            } else {
                let plan: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
                assert_eq!(plan["unclaimedPixels"], 64);
                assert_eq!(plan["draftRevision"], 1);
            }
            assert_eq!(session.snapshot(), snapshot);
        }
    }

    #[test]
    fn split_entry_needs_an_open_inx_source_and_an_http_capable_agent() {
        use crate::editor_test_support::{has, Harness};
        use egui::accesskit::Role;
        let (mut panel, agent) = panel_over_pipes();
        let mut settings = Settings::empty();
        let mut harness = Harness::new();
        harness.click(Role::Button, "Split this image", |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.show(ui, &mut settings, &json!({"documentFormat":"inx", "partSplit":{"open":true}}));
            });
        });
        assert!(panel.prompt.is_empty());
        assert!(agent.lines.try_recv().is_err());
        for snapshot in [json!({"documentFormat":"inx", "partSplit":{"open":false}}),
                         json!({"documentFormat":"live2d.cmo3", "partSplit":{"open":true}})] {
            let output = harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| { panel.show(ui, &mut settings, &snapshot); });
            });
            assert!(!has(&output, Role::Button, "Split this image"));
        }
    }

    #[test]
    fn the_first_prompt_of_a_run_carries_the_briefing_once() {
        let (mut panel, mut agent) = panel_over_pipes();
        panel.set_briefing(Some("Soul2D briefing: read resource://soul2d/editor/state".to_owned()));
        *panel.prompt_mut() = "fix the nose".to_string();
        panel.send();
        let new_session = agent.recv();
        assert_eq!(new_session["method"], "session/new");
        agent.send(json!({"jsonrpc": "2.0", "id": new_session["id"], "result": {"sessionId": "s1"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        let prompt = agent.recv();
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"], json!([
            {"type": "text", "text": "fix the nose"},
            {"type": "text", "text": "Soul2D briefing: read resource://soul2d/editor/state"}]));
        let roles: Vec<String> = panel.run.as_ref().unwrap().client.conversation.blocks.iter().map(|block| block.role.clone()).collect();
        assert_eq!(roles, ["You", "Soul2D"], "the user sees what was sent on their behalf");
        agent.send(json!({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "end_turn"}}));
        pump_until(&mut panel, |panel| !panel.state().turn_active);
        *panel.prompt_mut() = "second".to_string();
        panel.send();
        assert_eq!(agent.recv()["params"]["prompt"], json!([{"type": "text", "text": "second"}]), "once per run");
        // A new run (restart) is briefed again; a run without a briefing sends one block.
        let (client, mut agent) = connect();
        panel.attach(client, None, None);
        let init = agent.recv();
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {
            "protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": false}}}}));
        pump_until(&mut panel, |panel| panel.state().ready);
        *panel.prompt_mut() = "third".to_string();
        panel.send();
        let new_session = agent.recv();
        agent.send(json!({"jsonrpc": "2.0", "id": new_session["id"], "result": {"sessionId": "s2"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        assert_eq!(agent.recv()["params"]["prompt"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_turn_with_a_permission_request_and_a_cancel() {
        let (mut panel, mut agent) = panel_over_pipes();
        assert!(!panel.controls().send, "an empty prompt box cannot be sent");

        *panel.prompt_mut() = "fix the nose".to_string();
        assert!(panel.controls().send);
        panel.send();
        assert_eq!(panel.prompt_mut().as_str(), "", "a sent prompt leaves the box");
        assert_eq!(panel.status(), "Waiting for the agent");
        let new_session = agent.recv();
        assert_eq!(new_session["method"], "session/new");
        agent.send(json!({"jsonrpc": "2.0", "id": new_session["id"], "result": {"sessionId": "s1"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        let prompt = agent.recv();
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "fix the nose");

        // While the turn runs, nothing can be sent but it can be cancelled.
        *panel.prompt_mut() = "second".to_string();
        assert_eq!(panel.controls(), Controls { stop: true, new_session: true, cancel: true, ..Controls::default() });

        agent.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {
            "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Looking"}}}}));
        agent.send(json!({"jsonrpc": "2.0", "id": 41, "method": "session/request_permission", "params": {
            "reason": "Edit mesh",
            "toolCall": {"toolCallId": "t1", "title": "Edit mesh"},
            "options": [{"optionId": "allow:once", "name": "Allow once"}, {"optionId": "reject:once", "name": "Reject"}]}}));
        pump_until(&mut panel, |panel| panel.permission().is_some());

        let blocks = &panel.run.as_ref().unwrap().client.conversation.blocks;
        let roles: Vec<_> = blocks.iter().map(|block| block.role.as_str()).collect();
        assert_eq!(roles, ["You", "Assistant", "Tool#t1"]);
        let shown = permission_view(&panel.permission().unwrap());
        assert_eq!(shown.choices.len(), 3, "two options and Deny");
        assert!(!panel.controls().send, "a pending permission blocks sending");

        panel.answer_permission(0);
        assert_eq!(panel.status(), "Permission answered");
        let answer = agent.recv();
        assert_eq!(answer["id"], 41);
        assert_eq!(answer["result"]["outcome"], json!({"outcome": "selected", "optionId": "allow:once"}));
        assert!(panel.permission().is_none());

        // A second request is denied from the Deny button (last choice).
        agent.send(json!({"jsonrpc": "2.0", "id": "p2", "method": "session/request_permission", "params": {}}));
        pump_until(&mut panel, |panel| panel.permission().is_some());
        let deny = permission_view(&panel.permission().unwrap()).choices.len() - 1;
        panel.answer_permission(deny);
        let denied = agent.recv();
        assert_eq!((denied["id"].clone(), denied["result"]["outcome"].clone()), (json!("p2"), json!({"outcome": "cancelled"})));

        panel.cancel();
        assert_eq!(panel.status(), "Cancelling");
        let cancel = agent.recv();
        assert_eq!((cancel["method"].clone(), cancel["params"]["sessionId"].clone()), (json!("session/cancel"), json!("s1")));
        assert!(!panel.controls().cancel, "cancelling once is enough");
        agent.send(json!({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "cancelled"}}));
        pump_until(&mut panel, |panel| !panel.state().turn_active);
        assert_eq!(panel.status(), "Done (cancelled)");
        assert!(panel.controls().send, "the second prompt can go out after the turn");
        panel.stop();
        assert_eq!((panel.status(), panel.state().has_client), ("Stopped", false));
    }

    #[test]
    fn an_agent_that_hangs_up_is_reported_and_can_be_restarted() {
        let (mut panel, agent) = panel_over_pipes();
        drop(agent);
        pump_until(&mut panel, |panel| panel.state().closed);
        assert_eq!(panel.status(), "Agent stream closed");
        let controls = panel.controls();
        assert!(controls.start && controls.stop && !controls.send);
        assert!(panel.run.as_ref().unwrap().endpoint.is_none());
    }

    #[test]
    fn a_failed_initialize_shows_the_error_without_the_stderr_dump() {
        let (client, mut agent) = connect();
        let mut panel = no_launch_panel();
        panel.attach(client, None, None);
        let init = agent.recv();
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "error": {"code": -1, "message": "no thanks"}}));
        pump_until(&mut panel, |panel| panel.error().is_some());
        assert_eq!(panel.status(), "Initialize failed");
        assert_eq!(panel.error(), Some("initialize error: no thanks (code -1)"));
        assert!(!panel.controls().send);
    }

    // -------------------------------------------------------------------- opt-in and launch

    #[test]
    fn nothing_is_spawned_or_bound_until_the_user_enables_and_starts() {
        let mut panel = AgentPanel::new();
        let mut settings = Settings::empty();
        settings.set("ACP.Command", "definitely-not-a-program-xyz".to_string()).unwrap();
        assert!(!panel.is_enabled());
        panel.start(&settings);
        assert!(panel.run.is_none() && panel.error().is_none(), "start is a no-op while disabled");

        panel.set_enabled(true);
        assert!(panel.run.is_none(), "enabling alone starts nothing");
        panel.start(&settings);
        assert_eq!(panel.status(), "Start failed");
        assert!(panel.error().is_some_and(|error| error.starts_with("spawn failed")), "{:?}", panel.error());
        assert!(panel.run.is_none(), "a failed launch leaves nothing bound");
    }

    #[cfg(unix)]
    #[test]
    fn start_binds_a_loopback_endpoint_and_stop_releases_it() {
        let mut panel = AgentPanel::new();
        let mut settings = Settings::empty();
        settings.set("ACP.Command", "cat".to_string()).unwrap();
        panel.set_enabled(true);
        panel.start(&settings);
        assert_eq!(panel.status(), "Initializing...", "{:?}", panel.error());
        let url = panel.mcp_url().expect("endpoint url").to_string();
        let port: u16 = url.strip_prefix("http://127.0.0.1:").and_then(|rest| rest.strip_suffix("/mcp")).unwrap().parse().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        assert!(panel.controls().stop && !panel.controls().start);
        panel.set_enabled(false);
        assert_eq!((panel.status(), panel.state().has_client), ("Idle", false));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "the port is closed after disabling");
    }

    // -------------------------------------------------------------------- MCP over the non-blocking listener

    fn http_post(port: u16, token: Option<&str>, body: &str) -> String {
        let auth = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    /// Run `request` on a thread while this one serves the panel's endpoint, as a frame loop would.
    fn serve_while(panel: &mut AgentPanel<PipeWriter>, session: &mut Session, request: impl FnOnce() -> String + Send + 'static) -> String {
        let client = std::thread::spawn(request);
        let deadline = Instant::now() + WAIT;
        while !client.is_finished() && Instant::now() < deadline {
            panel.serve_mcp(None, &mut |message, _| RpcReply::plain(session.handle_mcp(message)));
            std::thread::sleep(Duration::from_millis(5));
        }
        client.join().unwrap()
    }

    fn active_panel_endpoint() -> (AgentPanel<PipeWriter>, Agent, Value) {
        let (mut panel, mut agent) = panel_over_pipes();
        panel.set_enabled(true);
        *panel.prompt_mut() = "panel approval".into();
        panel.send();
        let session = agent.recv();
        agent.send(json!({"jsonrpc":"2.0", "id":session["id"], "result":{"sessionId":"panel-test"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        let prompt = agent.recv();
        assert_eq!(prompt["method"], "session/prompt");
        panel.run.as_mut().unwrap().endpoint = Some(McpEndpoint::bind().unwrap());
        (panel, agent, prompt)
    }

    #[test]
    fn failed_shutdown_retains_the_same_run_and_does_not_report_stopped() {
        let (mut panel, _agent, _) = active_panel_endpoint();
        let owner = panel.owner();
        panel.stop_with(|_| Err("owned reaping could not be confirmed".into()));
        assert_eq!(panel.owner(), owner);
        assert!(panel.state().has_client);
        assert!(panel.mcp_url().is_none());
        assert!(panel.status().contains("ownership retained"));
        assert!(panel.error().unwrap().contains("could not be confirmed"));
        panel.enabled = false;
        let output = crate::editor_test_support::Harness::new().frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { panel.show(ui, &mut Settings::empty(), &Value::Null); });
        });
        assert!(crate::editor_test_support::has(&output, egui::accesskit::Role::Label,
            "Off. An ACP client is still owned because shutdown failed; enable the panel to retry Stop."));
        panel.stop();
        assert!(!panel.state().has_client);
    }

    #[test]
    fn quit_evidence_requires_post_admission_completion_and_unchanged_permission_history() {
        for case in ["early", "late", "permission", "replacement", "failed_stop"] {
            let (mut panel, mut agent, prompt) = active_panel_endpoint();
            let run = panel.run.as_ref().unwrap();
            let target = AcpQuitTarget { run: run.id, prompt: run.client.active_prompt().unwrap(), call: 1,
                permissions: run.client.permission_epoch() };
            let before = Instant::now();
            if case == "permission" {
                agent.send(json!({"jsonrpc":"2.0","id":null,"method":"session/request_permission","params":{}}));
            }
            agent.send(json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"stopReason":"end_turn"}}));
            pump_until(&mut panel, |panel| !panel.state().turn_active);
            let admitted = if case == "early" { Instant::now() } else { before };
            let mut replacement_agent = None;
            if case == "replacement" {
                let (client, agent) = connect();
                replacement_agent = Some(agent);
                panel.attach(client, None, Some(McpEndpoint::bind().unwrap()));
                assert_ne!(panel.owner(), Some(target.run));
            }
            if case == "failed_stop" { panel.stop_with(|_| Err("unconfirmed shutdown".into())); }
            if case == "late" {
                assert_eq!(panel.quit_status(&target, Some(admitted)), Ok(true));
                assert!(matches!(panel.release_for_quit(&target, admitted), QuitRelease::Released));
                assert!(!panel.state().has_client);
            } else {
                assert!(panel.quit_status(&target, Some(admitted)).is_err(), "{case}");
                assert!(matches!(panel.release_for_quit(&target, admitted), QuitRelease::Invalid(_)), "{case}");
                assert!(panel.state().has_client);
            }
            panel.stop();
            drop(replacement_agent);
        }
    }

    #[test]
    fn authenticated_http_preserves_writer_receipts_for_immediate_and_deferred_replies() {
        use crate::editor_test_support::Harness;
        use egui::accesskit::Role;
        for deferred in [false, true] {
            let (mut panel, _agent, _) = active_panel_endpoint();
            let client = panel_http(&panel, 1, Some("outline"));
            let (sender, receipt) = std::sync::mpsc::sync_channel(1);
            let mut sender = Some(sender);
            let path = Some(Path::new("owned-receipt-settings.json"));
            let mut handler = |message: &Value, approval: &mut HostApproval| {
                if deferred {
                    if let Err(error) = approval.guard(&json!({"op":"editor.panel.set","panel":"outline","visible":false}),
                        Ok(ApprovalTarget::Settings(path.map(Path::to_owned))), None) {
                        return RpcReply::plain(Some(json!({"jsonrpc":"2.0","id":message["id"],"result":{
                            "isError":true,"content":[{"type":"text","text":error}]}})));
                    }
                }
                RpcReply::tracked(message, Some(json!({"jsonrpc":"2.0","id":message["id"],"result":{
                    "isError":false,"structuredContent":{"accepted":true}}})), sender.take().unwrap())
            };
            let deadline = Instant::now() + WAIT;
            if deferred {
                while panel.run.as_ref().unwrap().pending_reply.is_none() && Instant::now() < deadline {
                    panel.serve_mcp(path, &mut handler);
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(panel.run.as_ref().unwrap().pending_reply.is_some());
                assert!(!client.is_finished() && receipt.try_recv().is_err());
                Harness::new().click(Role::Button, "Allow once", |ctx| panel.host_approval_window(ctx, path));
            }
            while !client.is_finished() && Instant::now() < deadline {
                panel.serve_mcp(path, &mut handler);
                std::thread::sleep(Duration::from_millis(5));
            }
            panel.stop();
            assert!(client.is_finished());
            assert_eq!(receipt.recv_timeout(WAIT).unwrap(), Ok(()));
            let response = http_json(client.join().unwrap());
            assert_eq!(response["id"], 1);
            assert_eq!(response["result"]["structuredContent"]["accepted"], true);
        }
    }

    fn panel_http(panel: &AgentPanel<PipeWriter>, id: u64, panel_name: Option<&str>) -> std::thread::JoinHandle<String> {
        let message = match panel_name {
            Some(panel) => json!({"jsonrpc":"2.0", "id":id, "method":"tools/call",
                "params":{"name":"editor_panel_set", "arguments":{"panel":panel,"visible":false}}}),
            None => json!({"jsonrpc":"2.0", "id":id, "method":"ping"}),
        };
        approval_http(panel, message)
    }

    fn approval_http(panel: &AgentPanel<PipeWriter>, message: Value) -> std::thread::JoinHandle<String> {
        let endpoint = panel.run.as_ref().unwrap().endpoint.as_ref().unwrap();
        let token = endpoint.token.clone();
        let port = endpoint.url().rsplit(':').next().unwrap().trim_end_matches("/mcp").parse().unwrap();
        std::thread::spawn(move || http_post(port, Some(&token), &message.to_string()))
    }

    fn panel_frame(panel: &mut AgentPanel<PipeWriter>, path: Option<&Path>, executed: &mut Vec<Value>) {
        panel.serve_mcp(path, &mut |message, approval| {
            let mut call = message["params"]["arguments"].clone();
            let is_write = message["method"] == "tools/call";
            if is_write { call["op"] = json!(message["params"]["name"].as_str().unwrap().replace('_', ".")); }
            let outcome = approval.guard(&call, Ok(ApprovalTarget::Settings(path.map(Path::to_owned))), None);
            let result = match outcome {
                Ok(()) => {
                    if is_write { executed.push(message["id"].clone()); }
                    json!({"isError":false})
                }
                Err(error) => json!({"isError":true,"content":[{"type":"text","text":error}]}),
            };
            RpcReply::plain(Some(json!({"jsonrpc":"2.0", "id":message["id"], "result":result})))
        });
    }

    fn park_panel(panel: &mut AgentPanel<PipeWriter>, executed: &mut Vec<Value>) -> std::thread::JoinHandle<String> {
        park_settings(panel, executed, "editor_panel_set")
    }

    fn park_settings(panel: &mut AgentPanel<PipeWriter>, executed: &mut Vec<Value>, tool: &str) -> std::thread::JoinHandle<String> {
        let arguments = match tool {
            "editor_panel_set" => json!({"panel":"outline", "visible":false}),
            "editor_layout_reset" => json!({}),
            _ => json!({"name":"Modeling"}),
        };
        let client = approval_http(panel, json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
            "params":{"name":tool, "arguments":arguments}}));
        let deadline = Instant::now() + WAIT;
        while panel.run.as_ref().unwrap().pending_reply.is_none() && Instant::now() < deadline {
            panel_frame(panel, Some(Path::new("owned-settings.json")), executed);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(panel.run.as_ref().unwrap().pending_reply.is_some());
        assert!(!client.is_finished());
        client
    }

    fn http_json(response: String) -> Value {
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[test]
    fn host_panel_approval_keeps_one_call_waiting_and_completes_it_after_a_gui_choice() {
        use crate::editor_test_support::Harness;
        use egui::accesskit::Role;
        let (mut panel, _agent, _) = active_panel_endpoint();
        let mut executed = Vec::new();
        let first = park_panel(&mut panel, &mut executed);
        let mut agent = _agent;
        agent.send(json!({"jsonrpc":"2.0", "id":99, "method":"session/request_permission", "params":{
            "reason":"Agent-supplied permission does not approve the host file write"}}));
        pump_until(&mut panel, |panel| panel.permission().is_some());
        panel.answer_permission(0);
        assert_eq!(agent.recv()["id"], 99);
        assert!(matches!(panel.run.as_ref().unwrap().host_approval.request.as_ref().unwrap().decision, ApprovalDecision::Pending));
        assert!(panel.run.as_ref().unwrap().pending_reply.is_some() && executed.is_empty());
        for (id, target) in [(2, Some("timeline")), (3, None)] {
            let client = panel_http(&panel, id, target);
            let deadline = Instant::now() + WAIT;
            while !client.is_finished() && Instant::now() < deadline {
                panel_frame(&mut panel, Some(Path::new("owned-settings.json")), &mut executed);
                std::thread::sleep(Duration::from_millis(5));
            }
            let answer = http_json(client.join().unwrap());
            assert_eq!(answer["id"], id);
            assert_eq!(answer["result"]["isError"], target.is_some());
            assert!(panel.run.as_ref().unwrap().pending_reply.is_some());
        }
        assert!(executed.is_empty());
        let mut harness = Harness::new();
        harness.click(Role::Button, "Allow once", |ctx|
            panel.host_approval_window(ctx, Some(Path::new("owned-settings.json"))));
        panel_frame(&mut panel, Some(Path::new("owned-settings.json")), &mut executed);
        let answer = http_json(first.join().unwrap());
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["isError"], false);
        assert_eq!(executed, [json!(1)]);
        let again = park_panel(&mut panel, &mut executed);
        harness.click(Role::Button, "Deny changes this turn", |ctx|
            panel.host_approval_window(ctx, Some(Path::new("owned-settings.json"))));
        panel_frame(&mut panel, Some(Path::new("owned-settings.json")), &mut executed);
        assert_eq!(http_json(again.join().unwrap())["result"]["isError"], true);
        let veto = panel_http(&panel, 4, Some("timeline"));
        let deadline = Instant::now() + WAIT;
        while !veto.is_finished() && Instant::now() < deadline {
            panel_frame(&mut panel, Some(Path::new("changed-settings.json")), &mut executed);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(http_json(veto.join().unwrap())["result"]["isError"], true, "denial survives a destination change");
        assert_eq!(executed, [json!(1)]);
    }

    #[test]
    fn parked_host_approval_calls_are_refused_when_the_prompt_or_destination_expires() {
        for (tool, event) in ["editor_panel_set", "editor_workspace_save", "editor_workspace_apply", "editor_workspace_delete", "editor_layout_reset"]
            .into_iter().flat_map(|tool| ["cancel", "done", "disconnect", "stop", "timeout", "no-path", "other-path"]
                .into_iter().map(move |event| (tool, event))) {
            let (mut panel, agent, prompt) = active_panel_endpoint();
            let mut agent = Some(agent);
            let mut executed = Vec::new();
            let client = park_settings(&mut panel, &mut executed, tool);
            match event {
                "cancel" => panel.cancel(),
                "done" => {
                    agent.as_mut().unwrap().send(json!({"jsonrpc":"2.0", "id":prompt["id"], "result":{"stopReason":"end_turn"}}));
                    pump_until(&mut panel, |panel| !panel.state().turn_active);
                }
                "disconnect" => {
                    drop(agent.take());
                    pump_until(&mut panel, |panel| panel.state().closed);
                }
                "stop" => panel.stop(),
                "timeout" => {
                    panel.run.as_mut().unwrap().host_approval.request.as_mut().unwrap().created =
                        Instant::now() - HOST_APPROVAL_TIMEOUT;
                    panel_frame(&mut panel, Some(Path::new("owned-settings.json")), &mut executed);
                }
                "no-path" => panel_frame(&mut panel, None, &mut executed),
                "other-path" => panel_frame(&mut panel, Some(Path::new("changed-settings.json")), &mut executed),
                _ => unreachable!(),
            }
            let answer = http_json(client.join().unwrap());
            assert_eq!(answer["id"], 1, "{event}");
            assert_eq!(answer["result"]["isError"], true, "{event}");
            assert!(executed.is_empty(), "{tool}: {event}");
            assert!(panel.run.as_ref().is_none_or(|run| run.pending_reply.is_none()), "{event}");
        }
    }

    #[test]
    fn host_workspace_and_layout_approval_complete_only_the_original_http_call() {
        use crate::editor_test_support::{has, Harness};
        use egui::accesskit::Role;
        let path = Some(Path::new("owned-settings.json"));
        for tool in ["editor_workspace_save", "editor_workspace_apply", "editor_workspace_delete", "editor_layout_reset"] {
            let (mut panel, _agent, _) = active_panel_endpoint();
            let mut executed = Vec::new();
            let client = park_settings(&mut panel, &mut executed, tool);
            let mut harness = Harness::new();
            let output = harness.frame(Vec::new(), |ctx| panel.host_approval_window(ctx, path));
            assert!(has(&output, Role::Label, &format!("Tool: {tool}")));
            assert!(has(&output, Role::Label, if tool == "editor_layout_reset" {
                "Layout: Reset Layout (panel visibility, panel sizes and window positions)"
            } else { "Workspace: Modeling" }));
            for (id, target) in [(2, Some("timeline")), (3, None)] {
                let other = panel_http(&panel, id, target);
                let deadline = Instant::now() + WAIT;
                while !other.is_finished() && Instant::now() < deadline {
                    panel_frame(&mut panel, path, &mut executed);
                    std::thread::sleep(Duration::from_millis(5));
                }
                let answer = http_json(other.join().unwrap());
                assert_eq!(answer["id"], id);
                assert_eq!(answer["result"]["isError"], target.is_some());
                assert!(!client.is_finished());
                assert!(executed.is_empty());
            }
            harness.click(Role::Button, "Allow once", |ctx| panel.host_approval_window(ctx, path));
            panel_frame(&mut panel, path, &mut executed);
            let answer = http_json(client.join().unwrap());
            assert_eq!(answer["id"], 1);
            assert_eq!(answer["result"]["isError"], false);
            assert_eq!(executed, [json!(1)]);
            let second = park_settings(&mut panel, &mut executed, tool);
            harness.click(Role::Button, "Deny changes this turn", |ctx| panel.host_approval_window(ctx, path));
            panel_frame(&mut panel, path, &mut executed);
            assert_eq!(http_json(second.join().unwrap())["result"]["isError"], true);
            assert_eq!(executed, [json!(1)]);
        }
    }

    #[test]
    fn a_stale_panel_approval_cannot_be_reused_by_a_queued_second_call() {
        for path in [None, Some(Path::new("changed-settings.json"))] {
            let (mut panel, _agent, _) = active_panel_endpoint();
            let mut executed = Vec::new();
            let first = park_panel(&mut panel, &mut executed);
            let second = panel_http(&panel, 2, Some("timeline"));
            let deadline = Instant::now() + WAIT;
            while Instant::now() < deadline {
                panel_frame(&mut panel, path, &mut executed);
                if first.is_finished() && (second.is_finished() || panel.run.as_ref().unwrap().pending_reply.is_some()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(http_json(first.join().unwrap())["result"]["isError"], true);
            if path.is_some() {
                assert!(panel.run.as_ref().unwrap().pending_reply.is_some(), "the new destination needs its own decision");
                panel.cancel();
            }
            assert_eq!(http_json(second.join().unwrap())["result"]["isError"], path.is_some());
            assert_eq!(executed, if path.is_none() { vec![json!(2)] } else { Vec::new() });
        }
    }

    #[test]
    fn parked_document_file_calls_expire_without_executing() {
        for (tool, event) in ["save", "render", "new", "open", "import.psd", "import.kra", "import.inp", "import.folder", "editor.recovery.restore",
            "export.inp", "export.image", "export.frames", "export.video.start", "export.collect", "export.gltf", "depth.draw.export_png", "export.gif", "view.screenshot", "object.export_csv", "model.template.export", "motion3.export",
            "animation.onion", "live2d.cdi3.export", "live2d.physics3.export", "live2d.userdata3.export", "texture.export"]
            .into_iter().flat_map(|tool|
            ["cancel", "done", "disconnect", "stop", "timeout", "edit", "draft", "replace"]
                .into_iter().map(move |event| (tool, event))) {
            let (mut panel, agent, prompt) = active_panel_endpoint();
            let mut agent = Some(agent);
            let mut document = ApprovalDocument { revision: 1, draft_dirty: false, path: None };
            let path = std::env::temp_dir().join("soul2d-host-approval-test-output");
            let (call, target) = match tool {
                "export.collect" | "depth.draw.export_png" => (json!({"op":tool,"dir":path}), ApprovalTarget::Output(path.clone())),
                "texture.export" => (json!({"op":tool,"directory":path}), ApprovalTarget::Output(path.clone())),
                "export.frames" => (json!({"op":tool,"path":path}), ApprovalTarget::Outputs(vec![soul2d_core::normalize_target_path(&path).unwrap()])),
                "new" => (json!({"op":tool}), ApprovalTarget::NewDocument),
                "editor.recovery.restore" => (json!({"op":tool,"id":"rust-recovery-test.inx"}), ApprovalTarget::Recovery(path)),
                "import.folder" => (json!({"op":tool,"path":std::env::temp_dir()}), ApprovalTarget::SourceDirectory(std::env::temp_dir())),
                "save" | "render" | "export.inp" | "export.image" | "export.video.start" | "export.gltf" | "export.gif" | "view.screenshot" |
                "object.export_csv" | "model.template.export" | "motion3.export" | "animation.onion" | "live2d.cdi3.export" |
                "live2d.physics3.export" | "live2d.userdata3.export" => (json!({"op":tool,"path":path}), ApprovalTarget::Output(soul2d_core::normalize_target_path(&path).unwrap())),
                _ => (json!({"op":tool,"path":path}), ApprovalTarget::SourceFile(path)),
            };
            let mut arguments = call.clone();arguments.as_object_mut().unwrap().remove("op");
            let client = approval_http(&panel, json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":tool.replace('.',"_"),"arguments":arguments}}));
            let mut executed = false;
            let mut frame = |panel: &mut AgentPanel<PipeWriter>, document: &ApprovalDocument| {
                panel.serve_mcp(None, &mut |message, approval| {
                    let result = match approval.guard(&call, Ok(target.clone()), Some(document.clone())) {
                        Ok(()) => { executed = true; json!({"isError":false}) },
                        Err(error) => json!({"isError":true,"content":[{"type":"text","text":error}]}),
                    };
                    RpcReply::plain(Some(json!({"jsonrpc":"2.0","id":message["id"],"result":result})))
                });
            };
            let deadline = Instant::now() + WAIT;
            while panel.run.as_ref().unwrap().pending_reply.is_none() && Instant::now() < deadline {
                frame(&mut panel, &document);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(panel.run.as_ref().unwrap().pending_reply.is_some(), "{tool}/{event}");
            match event {
                "cancel" => panel.cancel(),
                "done" => {
                    agent.as_mut().unwrap().send(json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"stopReason":"end_turn"}}));
                    pump_until(&mut panel, |panel| !panel.state().turn_active);
                }
                "disconnect" => { drop(agent.take());pump_until(&mut panel, |panel| panel.state().closed); }
                "stop" => panel.stop(),
                _ => {
                    let request = panel.run.as_mut().unwrap().host_approval.request.as_mut().unwrap();
                    request.decision = ApprovalDecision::AllowedOnce;
                    match event {
                        "timeout" => request.created = Instant::now() - HOST_APPROVAL_TIMEOUT,
                        "edit" => document.revision += 1,
                        "draft" => document.draft_dirty = true,
                        "replace" => document.path = Some(PathBuf::from("replacement.inx")),
                        _ => unreachable!(),
                    }
                    frame(&mut panel, &document);
                }
            }
            assert_eq!(http_json(client.join().unwrap())["result"]["isError"], true, "{tool}/{event}");
            assert!(!executed, "{tool}/{event}");
            assert!(panel.run.as_ref().is_none_or(|run| run.pending_reply.is_none()), "{tool}/{event}");
        }
    }

    #[test]
    fn a_recomputed_call_cannot_leave_an_unused_one_time_grant() {
        use crate::editor_test_support::Harness;
        use egui::accesskit::Role;
        let (mut panel, _agent, _) = active_panel_endpoint();
        let mut executed = Vec::new();
        let client = park_panel(&mut panel, &mut executed);
        let path = Some(Path::new("owned-settings.json"));
        Harness::new().click(Role::Button, "Allow once", |ctx| panel.host_approval_window(ctx, path));
        panel.serve_mcp(path, &mut |message, approval| {
            let error = approval.guard(&json!({"op":"editor.panel.set","panel":"timeline","visible":false}), Ok(ApprovalTarget::Settings(path.map(Path::to_owned))), None)
                .expect_err("a normalized target that changed cannot consume the grant");
            RpcReply::plain(Some(json!({"jsonrpc":"2.0","id":message["id"],"result":{"isError":true,"content":[{"type":"text","text":error}]}})))
        });
        assert_eq!(http_json(client.join().unwrap())["result"]["isError"], true);
        let run = panel.run.as_ref().unwrap();
        assert!(run.pending_reply.is_none() && run.host_approval.request.is_none());
        let again = park_panel(&mut panel, &mut executed);
        assert!(executed.is_empty());
        panel.cancel();
        assert_eq!(http_json(again.join().unwrap())["result"]["isError"], true);
    }

    #[test]
    fn an_approval_candidate_from_a_panicking_handler_is_not_installed() {
        let (mut panel, _agent, _) = active_panel_endpoint();
        let client = panel_http(&panel, 1, Some("outline"));
        let deadline = Instant::now() + WAIT;
        while !client.is_finished() && Instant::now() < deadline {
            panel.serve_mcp(Some(Path::new("owned-settings.json")), &mut |_, approval| {
                assert!(approval.guard(&json!({"op":"editor.panel.set","panel":"outline","visible":false}),
                    Ok(ApprovalTarget::Settings(Some(PathBuf::from("owned-settings.json")))), None).is_err());
                panic!("an error after staging must not leave a prompt or parked reply");
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(client.join().unwrap().starts_with("HTTP/1.1 500"));
        let run = panel.run.as_ref().unwrap();
        assert!(run.pending_reply.is_none() && run.host_approval.request.is_none() && !run.host_approval.defer_current);
    }

    #[test]
    fn the_endpoint_rejects_a_wrong_token_and_serves_the_session_for_the_right_one() {
        let (client, _agent) = connect();
        let endpoint = McpEndpoint::bind().unwrap();
        let token = endpoint.token.clone();
        let port: u16 = endpoint.url().rsplit(':').next().unwrap().trim_end_matches("/mcp").parse().unwrap();
        let mut panel = no_launch_panel();
        panel.attach(client, None, Some(endpoint));
        let mut session = Session::new();
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ParamCommand_Add1DParameter","arguments":{"name":"Blink","min":0,"max":1}}}"#;

        let nothing_waiting = panel.serve_mcp(None, &mut |_, _| panic!("the session must not be touched without a connection"));
        assert_eq!(nothing_waiting, 0);

        for bad in [None, Some("wrong"), Some(token.to_ascii_uppercase().as_str().to_owned().as_str())] {
            let bad = bad.map(str::to_owned);
            let reply = serve_while(&mut panel, &mut session, move || http_post(port, bad.as_deref(), call));
            assert!(reply.starts_with("HTTP/1.1 401 Unauthorized"), "{reply}");
        }
        assert!(session.snapshot()["model"]["param"].as_array().is_none_or(|params| params.is_empty()), "a rejected call changed nothing");

        let right = token.clone();
        let reply = serve_while(&mut panel, &mut session, move || http_post(port, Some(&right), call));
        assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
        assert!(reply.contains(r#""isError":false"#), "{reply}");
        assert_eq!(session.snapshot()["model"]["param"][0]["name"], "Blink");
    }

    // -------------------------------------------------------------------- headless egui frames

    fn frame(
        ctx: &egui::Context,
        panel: &mut AgentPanel<PipeWriter>,
        settings: &mut Settings,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 1600.0))),
            events,
            ..Default::default()
        };
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.show(ui, settings, &json!({}));
            });
        })
    }

    fn nodes(output: &egui::FullOutput) -> Vec<(egui::accesskit::NodeId, egui::accesskit::Node)> {
        output.platform_output.accesskit_update.clone().expect("accesskit is enabled").nodes
    }

    fn named(node: &egui::accesskit::Node) -> bool {
        node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty()
    }

    fn texts(output: &egui::FullOutput) -> Vec<String> {
        nodes(output).iter().flat_map(|(_, node)| [node.label(), node.value()]).flatten().map(str::to_owned).collect()
    }

    #[test]
    fn a_disabled_panel_draws_only_the_opt_in_checkbox() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut panel = AgentPanel::new();
        let mut settings = Settings::empty();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 400.0))),
            ..Default::default()
        };
        let mut saved = true;
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| saved = panel.show(ui, &mut settings, &json!({})));
        });
        assert!(!saved);
        let controls: Vec<_> = nodes(&output)
            .into_iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::CheckBox)
            .collect();
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].1.label(), Some("Enable coding agent (ACP)"));
        assert!(!texts(&output).iter().any(|text| text == "Start"), "no Start button while disabled");
        assert!(panel.run.is_none());
    }

    #[test]
    fn every_control_of_a_busy_panel_has_an_accessibility_name() {
        let (mut panel, mut agent) = panel_over_pipes();
        *panel.prompt_mut() = "go".to_string();
        panel.send();
        let session_new = agent.recv();
        agent.send(json!({"jsonrpc": "2.0", "id": session_new["id"], "result": {"sessionId": "s1"}}));
        pump_until(&mut panel, |panel| panel.status() == "Session ready");
        let _prompt = agent.recv();
        {
            let conversation = &mut panel.run.as_mut().unwrap().client.conversation;
            for update in [
                json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Thinking hard\nmore"}}),
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Answer text"}}),
                json!({"sessionUpdate": "tool_call", "toolCallId": "t9", "title": "Probe", "status": "in_progress",
                       "rawInput": {"q": 1}}),
                json!({"sessionUpdate": "error", "content": {"type": "text", "text": "it broke"}}),
                json!({"sessionUpdate": "plan", "entries": [{"content": "Do", "priority": "high", "status": "pending"}]}),
                json!({"sessionUpdate": "usage_update", "inputTokens": 3, "outputTokens": 4}),
            ] {
                conversation.apply_update(&update);
            }
            conversation.apply_permission_request(&json!({"jsonrpc": "2.0", "id": 5, "method": "session/request_permission",
                "params": {"reason": "Edit", "options": [{"optionId": "a", "name": "A very long option label here"}]}}));
        }
        panel.error = Some("something failed".to_string());

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut settings = Settings::empty();
        let output = frame(&ctx, &mut panel, &mut settings, Vec::new());
        use egui::accesskit::Role;
        let mut controls = 0;
        for (_, node) in nodes(&output) {
            if matches!(node.role(), Role::Button | Role::CheckBox | Role::ComboBox | Role::SpinButton | Role::TextInput) {
                controls += 1;
                assert!(named(&node), "unlabelled {:?} control {:?}", node.role(), node.value());
            }
        }
        assert!(controls >= 9, "only {controls} controls drawn");
        let texts = texts(&output);
        for expected in [
            "Enable coding agent (ACP)", "Command", "Working Dir (optional)", "Start", "Stop", "New session",
            "Status: Session ready", "Usage: in 3, out 4", "Error: something failed", "Plan", "1. [high] Do (pending)",
            "Permission requested: Edit", "A very long opt...", "Deny", "User message:", "Send", "Cancel", "Answer text",
            "tool: Probe (in_progress)",
        ] {
            assert!(texts.iter().any(|text| text.contains(expected)), "missing {expected:?} in {texts:?}");
        }
    }

    #[test]
    fn editing_a_settings_field_reports_a_save_when_the_field_loses_focus() {
        let (mut panel, _agent) = panel_over_pipes();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut settings = Settings::empty();
        settings.set("ACP.Command", "agent".to_string()).unwrap();
        let laid_out = frame(&ctx, &mut panel, &mut settings, Vec::new());
        let (_, field) = nodes(&laid_out)
            .into_iter()
            .find(|(_, node)| node.role() == egui::accesskit::Role::TextInput && node.value() == Some("agent"))
            .expect("the command field");
        let bounds = field.bounds().expect("bounds");
        let at = egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32);
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, &mut panel, &mut settings, vec![egui::Event::PointerMoved(at), click(true), click(false)]);
        frame(&ctx, &mut panel, &mut settings, vec![egui::Event::Text("x".into())]);
        assert_eq!(settings.get::<String>("ACP.Command").unwrap(), "agentx");
        assert!(panel.settings_dirty);
        let away = egui::pos2(at.x, 1500.0);
        let mut saved = false;
        for events in [vec![egui::Event::PointerMoved(away)], vec![
            egui::Event::PointerButton { pos: away, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            egui::Event::PointerButton { pos: away, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
        ]] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 1600.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| saved |= panel.show(ui, &mut settings, &json!({})));
            });
        }
        assert!(saved, "leaving the field must ask the caller to save");
        assert!(!panel.settings_dirty);
    }
}
