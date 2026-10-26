//! The small editor windows and the top menu bar: settings, welcome, about, command palette, texture
//! viewer, flip pair configuration and a path prompt for the menu's Open / Save As / Import entries.
//!
//! Port of source/soul2d/windows/{settings,welcome,about,texviewer,flipconfig,command_browser}.d,
//! widgets/mainmenu.d and commands/viewport/palette.d. The module sits beside `editor_ui` and cannot see
//! its private state, so [`Windows::show`] gets what it needs in a [`Host`] and answers with
//! [`Request`]s that `editor_ui` carries out. MCP calls travel the same way: the window emits
//! `Request::Mcp` and the answer comes back through [`Windows::deliver`].

use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::autosave;
use soul2d_core::i18n::LocaleEntry;
use soul2d_core::mcp_listener::McpSettings;
use soul2d_core::settings::{Chord, Platform, Settings, ShortcutRegistry};
use soul2d_core::window_shell::{self, AddressDialogOperation, AddressDialogTarget, CloseSetting};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

// ------------------------------------------------------------------------------------------ constants

// source/soul2d/config.d
const BANNER_ARTIST_NAME: &str = "Soul2D contributors";
const BANNER_ARTIST_PAGE: &str = "https://github.com/geekjapan/Soul2D";
const BUG_REPORT_URI: &str = "https://github.com/geekjapan/Soul2D/issues/new?template=bug-report.yml";
const FEATURE_REQ_URI: &str = "https://github.com/geekjapan/Soul2D/issues/new?template=feature_request.yml";
const WEBSITE_URI: &str = "https://github.com/geekjapan/Soul2D";
const DOCS_URI: &str = "https://github.com/geekjapan/Soul2D/blob/main/README.md";

const RECENT_KEY: &str = "prev_projects";
const RECENT_LIMIT: usize = 10;
const WELCOME_RECENT: usize = 4;
const KEY_FIRST_RUN: &str = "firstrun_complete";
const KEY_QUICK_SETUP: &str = "hasDoneQuickSetup";

const PALETTE_ID: &str = "PaletteCommand.ListCommand";

fn opens_command_browser(id: &str) -> bool {
    matches!(id, "EditCommand.ShowCommandBrowserWindow" | PALETTE_ID)
}

/// The editor's commands, as (D command id, shortcut table category, label, description).
const COMMANDS: &[(&str, &str, &str, &str)] = &[
    ("FileCommand.NewFile", "File", "New", "Create new project."),
    ("FileCommand.ShowOpenFileDialog", "File", "Open", "Show \"Open\" dialog."),
    ("FileCommand.ShowSaveFileDialog", "File", "Save", "Show \"Save\" dialog."),
    ("FileCommand.ShowSaveFileAsDialog", "File", "Save As...", "Show \"Save as\" dialog."),
    ("EditCommand.Undo", "Edit", "Undo", "Undo last action"),
    ("EditCommand.Redo", "Edit", "Redo", "Redo previously undone action"),
    ("EditCommand.ShowSettingsWindow", "Edit", "Settings", "Show settings window"),
    (
        "EditCommand.ShowCommandBrowserWindow",
        "Edit",
        "Command Browser",
        "Inspect available commands and their inputs/outputs.",
    ),
    ("NodeCommand.CutNode", "Nodes", "Cut", "Cut Node"),
    ("NodeCommand.CopyNode", "Nodes", "Copy", "Copy Node"),
    ("NodeCommand.PasteNode", "Nodes", "Paste", "Paste Node"),
    ("AnimationCommand.KeyAll", "Animation", "Key All", "Key every parameter on the timeline's frame"),
    ("AnimationCommand.UnkeyAll", "Animation", "Unkey All", "Delete every key on the timeline's frame"),
    (PALETTE_ID, "Palette", "Show Command Palette.", "Show Command Palette."),
];

/// Shortcut ids this module dispatches itself; `editor_ui` handles the file and undo ones.
const OWN_SHORTCUTS: &[&str] = &[
    PALETTE_ID,
    "EditCommand.ShowSettingsWindow",
    "EditCommand.ShowCommandBrowserWindow",
    "NodeCommand.CutNode",
    "NodeCommand.CopyNode",
    "NodeCommand.PasteNode",
];

const KEEP_LAYER_FOLDER: [(&str, &str); 3] =
    [("Ask", "Always ask"), ("Preserve", "Preserve"), ("NotPreserve", "Don't preserve")];
const LAYER_GROUP_REPLACEMENT: [&str; 3] = ["Node", "MeshGroup", "DynamicComposite"];

/// CONTRIBUTORS.md as the D About window shows it, with the markdown links flattened to `name (url)`.
const CREDITS: &[(&str, &[&str])] = &[
    ("Project Managment", &["Project Creator and BDFL: seagetch (https://twitter.com/seagetch)"]),
    ("Designers", &["nijilive Logo: seagetch (https://twitter.com/seagetch)"]),
    (
        "Mascot",
        &[
            "Mascot Design: seagetch (https://twitter.com/seagetch)",
            "Mascot Model Art: DALL-E3 (https://chat.openai.com)",
            "Chibi UI Mascot: DALL-E3 (https://chat.openai.com)",
        ],
    ),
    ("Technology Used", &["Rust", "egui", "eframe", "glow", "OpenGL"]),
    ("Fonts Used", &["Noto", "Noto CJK", "OpenDyslexic", "Material Icons"]),
    ("Special Thanks", &["Inochi2D Project (https://inochi2d.com/)"]),
    (
        "Inochi2D Credits: Project Managment",
        &[
            "Project Creator and BDFL: Luna the Foxgirl (https://twitter.com/LunaFoxgirlVT)",
            "ImGui Binding Maintainer: Joshua T. Fisher (https://twitter.com/Timewatcher)",
            "UX Designer: Nethyr_UX (https://twitter.com/Nethyr_UX)",
        ],
    ),
    ("Inochi2D Credits: Designers", &["Inochi2D Logo: James Daniel (https://twitter.com/rakujira)"]),
    (
        "Inochi2D Credits: Code Contributors",
        &[
            "Asahi Lina (https://twitter.com/LinaAsahi)",
            "seagetch (https://twitter.com/seagetch)",
            "carbotaniuman (https://github.com/carbotaniuman)",
            "Grillo (https://vt.social/@grillo_delmal)",
            "Aki-nyan (https://twitter.com/lethalbit)",
            "PanzerKern (https://github.com/pankern)",
        ],
    ),
    (
        "Inochi2D Credits: Mascot",
        &[
            "Mascot Design: Kinu (https://twitter.com/Kinugitsune)",
            "Mascot Model Art: ku-ini (https://twitter.com/duckmastah)",
            "Chibi UI Mascot: いろは (https://twitter.com/iroha_matsurika)",
        ],
    ),
    ("Inochi2D Credits: Documentation", &["Luna the Foxgirl (https://twitter.com/LunaFoxgirlVT)"]),
];

// ------------------------------------------------------------------------------------------ requests

/// Which pending MCP call an answer belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTarget {
    AddressDialog,
    ToolList,
    ToolCall,
    /// The `texture_get` answer for the albedo (0), emissive (1) or bump (2) slot of the viewer's Part.
    Texture(u8),
    FlipRead,
    FlipWrite,
    /// An editor layout or window-visibility call from View.
    Layout,
    Viewport,
    ParameterPanel,
}

/// The File > Export entries. They run the Tools panel's exports with the panel's options and the typed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Image,
    Frames,
    Inp,
    /// Starts the Tools panel's background ffmpeg job with its codec and extra options.
    Video,
}

/// Settings key of the saved workspaces (Cubism "Workspace", UI01): name to the layout JSON (`editor_ui`).
pub const KEY_WORKSPACES: &str = "workspaces";

/// The saved workspaces. An error when the settings file holds the key in another shape, so that a save does not
/// write over workspaces it could not read.
pub fn saved_workspaces(store: &Settings) -> Result<BTreeMap<String, String>, String> {
    if !store.contains(KEY_WORKSPACES) {
        return Ok(BTreeMap::new());
    }
    store.get(KEY_WORKSPACES).map_err(|_| {
        format!("the {KEY_WORKSPACES:?} entry of the settings file is not a map of names to layouts; fix or remove it")
    })
}

/// The names View > Workspace and the editor state offer, sorted: only the ones the workspace tools accept (the
/// rule of `mcp.rs`), since a name edited into the settings file by hand could be neither applied nor deleted.
pub fn workspace_names(store: &Settings) -> Vec<String> {
    let usable = |name: &String| {
        !name.is_empty() && name.trim() == name && name.chars().count() <= 64 && !name.chars().any(char::is_control)
    };
    saved_workspaces(store).unwrap_or_default().into_keys().filter(usable).collect()
}

/// The panels View > Panels can hide, as (label, settings key). A missing key means shown.
pub const PANELS: [(&str, &str); 2] = [("Outline", "Outline.visible"), ("Timeline", "Timeline.visible")];

/// Whether the panel stored under `key` is shown.
pub fn panel_visible(store: &Settings, key: &str) -> bool {
    store.get_or(key, true).unwrap_or(true)
}

/// What `editor_ui` must do on behalf of a window.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// A D command id that `editor_ui` maps to its own editor action (new, save, undo, redo).
    Command(String),
    /// Open this path (goes through the autosave restore prompt).
    Open(String),
    /// File > Recent > Autosaves: open the backup under its project's path (D `incOpenProject(main, backup)`).
    OpenAutosave(autosave::AutosaveRecord),
    /// Save to this path and make it the document's path.
    SaveAs(String),
    /// A core session request, run like the toolbar's.
    Core(Value),
    /// A JSON-RPC request for the MCP endpoint; answer with [`Windows::deliver`].
    Mcp(McpTarget, Value),
    SetLanguage(String),
    /// The autosave keys changed in the store.
    AutosaveChanged,
    /// `KeepLayerFolder` or `LayerGroupReplacement` changed in the store.
    ImportDefaultsChanged,
    /// The `MCP.*` keys were stored: start, restart or stop the editor's MCP server.
    McpApply,
    NodeCopy,
    NodeCut,
    NodePaste,
    /// File > Export: run this export to the typed path.
    Export(ExportKind, String),
    /// File > Import > Depth Map: open the PSD depth map window.
    OpenPsdDepth,
    /// A message for the status area.
    Notice(String),
}

/// What the editor's own MCP server is doing, for the Integration pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpStatus {
    /// The URL it listens on while running.
    pub url: Option<String>,
    /// The generated bearer token (never one taken from the environment).
    pub token: Option<String>,
    /// Why it is not running although enabled.
    pub error: Option<String>,
}

/// Everything a frame of the windows reads from the editor.
pub struct Host<'a> {
    /// Whether key all and unkey all can run on the timeline's frame, in that order.
    pub all_keys: [bool; 2],
    pub snapshot: &'a Value,
    pub selected_node: Option<u64>,
    pub project_path: Option<&'a Path>,
    pub store: &'a mut Settings,
    pub store_path: Option<&'a Path>,
    pub shortcuts: &'a mut ShortcutRegistry,
    pub locales: &'a [LocaleEntry],
    pub lang: &'a str,
    pub tr: &'a dyn Fn(&str) -> String,
    pub mcp: &'a McpStatus,
}

// ------------------------------------------------------------------------------------------ recent files

/// D `incAddPrevProject`: the path goes to the front, its first earlier entry is dropped, 10 are kept.
pub fn add_recent(list: &[String], path: &str) -> Vec<String> {
    let mut rest = list.to_vec();
    if let Some(at) = rest.iter().position(|entry| entry == path) {
        rest.remove(at);
    }
    let mut out = vec![path.to_owned()];
    out.extend(rest);
    out.truncate(RECENT_LIMIT);
    out
}

pub fn recent_projects(store: &Settings) -> Vec<String> {
    store.get::<Vec<String>>(RECENT_KEY).unwrap_or_default()
}

fn remember(store: &mut Settings, store_path: Option<&Path>, path: &str) {
    // One spelling per file (`/tmp/x` and `/private/tmp/x` on macOS), when the file exists.
    let resolved = std::fs::canonicalize(path).ok().and_then(|p| p.to_str().map(str::to_owned));
    let list = add_recent(&recent_projects(store), resolved.as_deref().unwrap_or(path));
    // The recent list is a convenience: a store or disk failure must not disturb opening or saving.
    if store.set(RECENT_KEY, list).is_ok() {
        if let Some(file) = store_path {
            let _ = store.save(file);
        }
    }
}

/// Call after a document was opened from `path`.
pub fn note_opened(path: &str, store: &mut Settings, store_path: Option<&Path>) {
    remember(store, store_path, path);
}

/// Call after a core request succeeded: a `save` makes the document's path a recent project.
pub fn note_saved(request: &Value, project: Option<&Path>, store: &mut Settings, store_path: Option<&Path>) {
    if request["op"].as_str() != Some("save") {
        return;
    }
    if let Some(path) = project.and_then(Path::to_str) {
        remember(store, store_path, path);
    }
}

fn base_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------------------------------ settings logic

const KEY_NERD_STATS: &str = "NerdStats";
const KEY_OPEN_DYSLEXIC: &str = "useOpenDyslexic";

/// The stored `NerdStats` flag (D default off): whether the stats window is open.
pub fn nerd_stats(store: &Settings) -> bool {
    store.get_or(KEY_NERD_STATS, false).unwrap_or(false)
}

/// The stored `useOpenDyslexic` flag (D default off).
pub fn use_open_dyslexic(store: &Settings) -> bool {
    store.get_or(KEY_OPEN_DYSLEXIC, false).unwrap_or(false)
}

/// The rows of the stats window: (label, value). `dt` is the last frame's duration in seconds.
fn nerd_stats_rows(snapshot: &Value, dt: f32) -> Vec<(&'static str, String)> {
    let nodes = tree_nodes(&snapshot["model"]["nodes"]);
    let parts = nodes.iter().filter(|(_, _, kind)| kind == "Part").count();
    let animations = snapshot["model"]["animations"].as_object().map_or(0, serde_json::Map::len);
    let fps = if dt > 0.0 { format!("{:.0}", 1.0 / dt) } else { "-".to_owned() };
    vec![
        ("Frame time", format!("{:.1} ms", dt * 1000.0)),
        ("Frames per second", fps),
        ("Document revision", snapshot["revision"].as_u64().unwrap_or(0).to_string()),
        ("Nodes", nodes.len().to_string()),
        ("Parts", parts.to_string()),
        ("Animations", animations.to_string()),
    ]
}

/// Bounds of `history.limit.set`, which the stored `MaxUndoHistory` is clamped to.
const MAX_UNDO_HISTORY_RANGE: (u64, u64) = (2, 1000);

/// `MaxUndoHistory` (D default 100) clamped to the range the session accepts.
pub fn max_undo_history(store: &Settings) -> u64 {
    let (min, max) = MAX_UNDO_HISTORY_RANGE;
    store.get_or::<u64>("MaxUndoHistory", 100).unwrap_or(100).clamp(min, max)
}

/// The session request that applies the stored `MaxUndoHistory`.
pub fn history_limit_request(store: &Settings) -> Value {
    json!({"op": "history.limit.set", "limit": max_undo_history(store)})
}

/// `UIScale` as the whole percent D's input shows, clamped to 100..=200.
pub fn ui_scale_percent(store: &Settings) -> i64 {
    let scale = store.get_or::<f64>("UIScale", 1.0).unwrap_or(1.0);
    ((scale * 100.0).round() as i64).clamp(100, 200)
}

pub fn set_ui_scale_percent(store: &mut Settings, percent: i64) -> Result<(), String> {
    store.set("UIScale", percent.clamp(100, 200) as f64 / 100.0)
}

/// The zoom factor to apply: D ignores `UIScale` on macOS, which follows Retina scaling.
pub fn effective_zoom(store: &Settings, platform: Platform) -> f32 {
    if platform == Platform::MacOs {
        1.0
    } else {
        ui_scale_percent(store) as f32 / 100.0
    }
}

/// `DarkMode`, or `None` while the user never chose (the egui default stays).
pub fn dark_mode(store: &Settings) -> Option<bool> {
    store.contains("DarkMode").then(|| store.get_or("DarkMode", false).unwrap_or(false))
}

/// The stored string if it is one of `allowed`, else `default` (D falls back to its default).
fn choice(store: &Settings, key: &str, default: &str, allowed: &[&str]) -> String {
    let value = store.get_or::<String>(key, default.to_owned()).unwrap_or_else(|_| default.to_owned());
    if allowed.contains(&value.as_str()) {
        value
    } else {
        default.to_owned()
    }
}

/// The autosave keys as the form shows them, with D's clamps.
fn autosave_form(store: &Settings) -> autosave::Settings {
    let enabled = store.get_or("AutosaveEnabled", true).unwrap_or(true);
    let interval = store.get_or::<i64>("AutosaveInterval", 5).unwrap_or(5);
    let limit = store.get_or::<i64>("AutosaveFileLimit", 3).unwrap_or(3);
    autosave::Settings::new(
        enabled,
        u32::try_from(interval.max(0)).unwrap_or(u32::MAX),
        u32::try_from(limit.max(0)).unwrap_or(u32::MAX),
    )
}

fn store_autosave(store: &mut Settings, settings: &autosave::Settings) -> Result<(), String> {
    store.set("AutosaveEnabled", settings.enabled)?;
    store.set("AutosaveInterval", i64::from(settings.interval_minutes))?;
    store.set("AutosaveFileLimit", i64::from(settings.max_backups))
}

/// Stores `MaxUndoHistory` as the Look pane does and asks the session to follow.
pub fn set_max_undo_history(store: &mut Settings, limit: i64) -> Result<Request, String> {
    let (min, max) = MAX_UNDO_HISTORY_RANGE;
    store.set("MaxUndoHistory", limit.clamp(min as i64, max as i64))?;
    Ok(Request::Core(history_limit_request(store)))
}

/// Stores one of the import defaults of the Files pane and asks the editor to follow.
fn set_import_default(store: &mut Settings, key: &str, value: &str) -> Result<Request, String> {
    store.set(key, value.to_owned()).map(|()| Request::ImportDefaultsChanged)
}

// ------------------------------------------------------------------------------------------ settings over MCP

/// What a setting accepts, for the check before it is written.
enum Kind {
    Bool,
    Int(i64, i64),
    Choice(&'static [&'static str]),
}

struct SettingRow {
    key: &'static str,
    /// The label of the editor preference's GUI control.
    label: &'static str,
    kind: Kind,
    read: fn(&mut Settings) -> Value,
    /// Stores a value that passed `kind`. The request is what the editor must do afterwards. `None` for a
    /// setting an MCP caller may read but only the Settings window changes.
    write: Option<fn(&mut Settings, &Value) -> Result<Option<Request>, String>>,
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Editor preferences an MCP caller may read: Settings controls in window order, then View toggles.
/// `lang`, which is not a plain stored value, follows the colour theme. A caller changes rows with a writer.
/// The autosave settings and `SaveProjectOnClose` have none: they guard the user's unsaved work and the
/// backups on disk, and the caller may be a remote agent.
const SETTINGS: &[SettingRow] = &[
    SettingRow {
        key: "DarkMode",
        label: "Color Theme",
        kind: Kind::Bool,
        read: |store| dark_mode(store).map_or(Value::Null, Value::from),
        write: Some(|store, value| store.set("DarkMode", value.as_bool() == Some(true)).map(|()| None)),
    },
    SettingRow {
        key: "UIScale",
        label: "UI Scale",
        kind: Kind::Int(100, 200),
        read: |store| json!(ui_scale_percent(store)),
        write: Some(|store, value| set_ui_scale_percent(store, value.as_i64().unwrap_or(100)).map(|()| None)),
    },
    SettingRow {
        key: KEY_OPEN_DYSLEXIC,
        label: "Use OpenDyslexic Font",
        kind: Kind::Bool,
        read: |store| json!(use_open_dyslexic(store)),
        write: Some(|store, value| store.set(KEY_OPEN_DYSLEXIC, value.as_bool() == Some(true)).map(|()| None)),
    },
    SettingRow {
        key: "MaxUndoHistory",
        label: "Max Undo History",
        kind: Kind::Int(MAX_UNDO_HISTORY_RANGE.0 as i64, MAX_UNDO_HISTORY_RANGE.1 as i64),
        read: |store| json!(max_undo_history(store)),
        write: Some(|store, value| set_max_undo_history(store, value.as_i64().unwrap_or(0)).map(Some)),
    },
    SettingRow {
        key: "AutosaveEnabled",
        label: "Enable Autosaves",
        kind: Kind::Bool,
        read: |store| json!(autosave_form(store).enabled),
        write: None,
    },
    SettingRow {
        key: "AutosaveInterval",
        label: "Save Interval (Minutes)",
        kind: Kind::Int(1, 1440),
        read: |store| json!(autosave_form(store).interval_minutes),
        write: None,
    },
    SettingRow {
        key: "AutosaveFileLimit",
        label: "Maximum Autosaves",
        kind: Kind::Int(1, 1000),
        read: |store| json!(autosave_form(store).max_backups),
        write: None,
    },
    SettingRow {
        key: "SaveProjectOnClose",
        label: "Save project on close",
        kind: Kind::Choice(&["Ask", "dontSave"]),
        read: |store| json!(window_shell::close_setting(store).key()),
        write: None,
    },
    SettingRow {
        key: "KeepLayerFolder",
        label: "Preserve structure",
        kind: Kind::Choice(&["Ask", "Preserve", "NotPreserve"]),
        read: |store| json!(choice(store, "KeepLayerFolder", "Ask", &["Ask", "Preserve", "NotPreserve"])),
        write: Some(|store, value| set_import_default(store, "KeepLayerFolder", text(value)).map(Some)),
    },
    SettingRow {
        key: "LayerGroupReplacement",
        label: "Replace LayerGroup with",
        kind: Kind::Choice(&LAYER_GROUP_REPLACEMENT),
        read: |store| json!(choice(store, "LayerGroupReplacement", "Node", &LAYER_GROUP_REPLACEMENT)),
        write: Some(|store, value| set_import_default(store, "LayerGroupReplacement", text(value)).map(Some)),
    },
    SettingRow {
        key: KEY_NERD_STATS,
        label: "Stats for Nerds",
        kind: Kind::Bool,
        read: |store| json!(nerd_stats(store)),
        write: Some(|store, value| store.set(KEY_NERD_STATS, value.as_bool() == Some(true)).map(|()| None)),
    },
];

/// The Integration pane's MCP endpoint settings: (key, label, type). Listed with their values, never written:
/// they choose the address the editor serves. The pane's other settings are not listed at all, because they
/// can hold a secret: `ACP.Command` (an argument line), `ACP.Workdir` (an absolute directory) and
/// `MCP.Headers`.
const READ_ONLY_SETTINGS: &[(&str, &str, &str)] = &[
    ("MCP.Name", "Server name", "string"),
    ("MCP.Enabled", "Enable MCP server", "boolean"),
    ("MCP.Host", "MCP host", "string"),
    ("MCP.Port", "MCP port", "integer"),
    ("MCP.authEnabled", "Require a bearer token (MCP.authEnabled)", "boolean"),
];

fn read_only_value(store: &Settings, key: &str) -> Value {
    let mcp = McpSettings::load(store);
    match key {
        "MCP.Enabled" => json!(mcp.enabled),
        "MCP.Host" => json!(mcp.host),
        "MCP.Port" => json!(mcp.port),
        "MCP.authEnabled" => json!(mcp.auth),
        _ => json!(store.get_or::<String>("MCP.Name", "soul2d".to_owned()).unwrap_or_default()),
    }
}

/// A setting as `editor_settings_get` reports it.
fn setting_entry(row: &SettingRow, store: &mut Settings) -> Value {
    let mut entry = json!({"key": row.key, "label": row.label, "value": (row.read)(store), "writable": row.write.is_some()});
    match row.kind {
        Kind::Bool => entry["type"] = json!("boolean"),
        Kind::Int(min, max) => {
            entry["type"] = json!("integer");
            entry["min"] = json!(min);
            entry["max"] = json!(max);
        }
        Kind::Choice(allowed) => {
            entry["type"] = json!("string");
            entry["allowed"] = json!(allowed);
        }
    }
    entry
}

fn language_codes(locales: &[LocaleEntry]) -> Vec<&str> {
    std::iter::once("en").chain(locales.iter().map(|entry| entry.code.as_str())).collect()
}

fn language_entry(lang: &str, locales: &[LocaleEntry]) -> Value {
    json!({"key": "lang", "label": "Language", "type": "string", "allowed": language_codes(locales), "value": lang, "writable": true})
}

/// Whether the Settings window shows this row: it hides the scale on macOS, which follows Retina scaling.
fn shown(row: &SettingRow) -> bool {
    row.key != "UIScale" || Platform::current() != Platform::MacOs
}

/// Listed editor preferences, with their values, types and allowed values.
pub fn settings_list(store: &mut Settings, lang: &str, locales: &[LocaleEntry]) -> Vec<Value> {
    let mut list = Vec::new();
    for row in SETTINGS.iter().filter(|row| shown(row)) {
        list.push(setting_entry(row, store));
        if row.key == "DarkMode" {
            list.push(language_entry(lang, locales));
        }
    }
    for (key, label, kind) in READ_ONLY_SETTINGS {
        list.push(json!({"key": key, "label": label, "type": kind, "value": read_only_value(store, key), "writable": false}));
    }
    list
}

/// The shortcut bindings of the Shortcuts pane: command id, label and the chord as the window shows it.
pub fn shortcut_list(store: &Settings, shortcuts: &ShortcutRegistry) -> Vec<Value> {
    let swap = mac_swap(store);
    COMMANDS
        .iter()
        .map(|(id, _, label, _)| {
            let binding = shortcuts.binding_for(id);
            let chord = binding.map(|binding| chord_text(&binding.chord, Platform::current(), swap));
            json!({"command": id, "label": label, "shortcut": chord,
                "spec": binding.map(|binding| chord_spec(&binding.chord)), "repeat": binding.map(|binding| binding.repeat)})
        })
        .collect()
}

/// The machine-readable form of a chord, which `editor_shortcut_set` takes: plain modifier names and the key
/// joined by `+`, as in `Ctrl+Shift+S`, on every platform.
fn chord_spec(chord: &Chord) -> String {
    chord.format(Platform::Linux)
}

/// The `editor_settings_get` shortcut row of one command, after a change.
pub fn shortcut_listed(store: &Settings, shortcuts: &ShortcutRegistry, command: &str) -> Value {
    shortcut_list(store, shortcuts).into_iter().find(|row| row["command"] == command).unwrap_or(Value::Null)
}

/// Check before an ACP approval is staged; validation changes nothing. `None` means unbind.
pub(crate) fn validate_shortcut(command: &str, shortcut: &Value) -> Result<Option<Chord>, String> {
    if !COMMANDS.iter().any(|row| row.0 == command) {
        return Err(format!("{command:?} is not a command; editor_settings_get lists them under shortcuts"));
    }
    match shortcut {
        Value::Null => Ok(None),
        Value::String(text) => Chord::parse(text, Platform::Linux).map(Some),
        _ => Err("shortcut must be a chord such as Ctrl+Shift+S, or null to unassign".into()),
    }
}

/// Binds `chord` to `command`, or unbinds the command for `None`, as the Shortcuts pane's capture does. `repeat`
/// defaults to the command's current flag (false for a command without a binding). Returns the command that
/// lost the chord, if another one held it.
pub fn set_shortcut(shortcuts: &mut ShortcutRegistry, command: &str, chord: Option<Chord>, repeat: Option<bool>) -> Option<String> {
    match chord {
        Some(chord) => {
            let repeat = repeat.unwrap_or_else(|| shortcuts.binding_for(command).is_some_and(|binding| binding.repeat));
            shortcuts.register(command, chord, repeat)
        }
        None => {
            shortcuts.clear(command);
            None
        }
    }
}

/// Check before an ACP approval is staged; validation never changes editor or stored state.
pub(crate) fn validate_setting(locales: &[LocaleEntry], key: &str, value: &Value) -> Result<(), String> {
    if key == "lang" {
        let code = value.as_str().filter(|code| *code == "en" || locales.iter().any(|entry| entry.code == *code));
        code.ok_or_else(|| format!("lang must be one of: {}", language_codes(locales).join(", ")))?;
        return Ok(());
    }
    let window_only = || format!("{key} can only be changed in the Settings window");
    if READ_ONLY_SETTINGS.iter().any(|(read_only, ..)| *read_only == key) {
        return Err(window_only());
    }
    let row = SETTINGS.iter().find(|row| row.key == key && shown(row));
    let row = row.ok_or_else(|| format!("{key:?} is not a setting; editor_settings_get lists them"))?;
    row.write.ok_or_else(window_only)?;
    match row.kind {
        Kind::Bool if value.is_boolean() => {}
        Kind::Bool => return Err(format!("{key} must be true or false")),
        Kind::Int(min, max) if value.as_i64().is_some_and(|n| (min..=max).contains(&n)) => {}
        Kind::Int(min, max) => return Err(format!("{key} must be an integer from {min} to {max}")),
        Kind::Choice(allowed) if allowed.contains(&text(value)) => {}
        Kind::Choice(allowed) => return Err(format!("{key} must be one of: {}", allowed.join(", "))),
    }
    Ok(())
}

/// Store a validated preference and return any follow-up the editor must apply.
/// Refusals change nothing. `Request::SetLanguage` stores the language after swapping its catalogue.
pub fn set_setting(
    store: &mut Settings,
    locales: &[LocaleEntry],
    key: &str,
    value: &Value,
) -> Result<Option<Request>, String> {
    validate_setting(locales, key, value)?;
    if key == "lang" {
        return Ok(Some(Request::SetLanguage(text(value).to_owned())));
    }
    let row = SETTINGS.iter().find(|row| row.key == key).unwrap();
    (row.write.unwrap())(store, value)
}

/// The `editor_settings_get` entry of one setting, after a change.
pub fn setting_listed(store: &mut Settings, lang: &str, locales: &[LocaleEntry], key: &str) -> Value {
    settings_list(store, lang, locales).into_iter().find(|entry| entry["key"] == key).unwrap_or(Value::Null)
}

/// `MCP.Headers` must be a JSON array of `{name, value}` strings; anything else is ignored by the agent.
pub fn validate_headers(text: &str) -> Result<(), String> {
    let value: Value = serde_json::from_str(text).map_err(|error| format!("Not valid JSON: {error}"))?;
    let items = value.as_array().ok_or("Headers must be a JSON array")?;
    for (index, item) in items.iter().enumerate() {
        if item["name"].as_str().is_none() || item["value"].as_str().is_none() {
            return Err(format!("Header {} needs string fields \"name\" and \"value\"", index + 1));
        }
    }
    Ok(())
}

/// A shortcut from a key press: `key` is the `egui::Key::name()`; `command` is Ctrl (Cmd on macOS).
/// Keys D cannot bind give `None`.
/// A press's `Ctrl` and `Super` in a chord's terms. On macOS `Ctrl` is the Cmd key and `Super` the physical
/// Control key, so `Ctrl-N` is Cmd+N; `MacSwapCmdCtrl` (`swap`) exchanges the two. Elsewhere `Ctrl` is Ctrl,
/// `Super` is never pressed and the setting does nothing.
pub fn chord_modifiers(modifiers: egui::Modifiers, swap: bool) -> (bool, bool) {
    let mac = cfg!(target_os = "macos");
    // egui sets `ctrl` beside `command` for events built the non-Mac way; only a Control press of its own counts.
    let control = mac && modifiers.ctrl && (modifiers.mac_cmd || !modifiers.command);
    let command = modifiers.command;
    if mac && swap { (control, command) } else { (command, control) }
}

/// The stored `MacSwapCmdCtrl` setting.
pub fn mac_swap(store: &Settings) -> bool {
    store.get_or("MacSwapCmdCtrl", false).unwrap_or(false)
}

pub fn chord_from_press(key: &str, command: bool, alt: bool, shift: bool) -> Option<Chord> {
    Chord::from_spec(key).ok()?;
    Some(Chord { ctrl: command, alt, shift, super_key: false, key: key.to_owned() })
}

/// A chord as the UI shows it. The stored text uses D's macOS glyphs, which no bundled font has; and the
/// `Ctrl` of a chord is the Command key on macOS, unless `MacSwapCmdCtrl` (`swap`) makes it `Super`.
pub fn chord_text(chord: &Chord, platform: Platform, swap: bool) -> String {
    modifier_labels(chord.format(Platform::Linux), platform, swap)
}

fn modifier_labels(text: String, platform: Platform, swap: bool) -> String {
    match (platform == Platform::MacOs, swap) {
        (true, false) => text.replace("Ctrl", "Cmd"),
        (true, true) => text.replace("Super", "Cmd"),
        _ => text,
    }
}

/// A key press as the shortcut matcher sees it.
#[derive(Debug, PartialEq)]
pub struct Press {
    pub key: &'static str,
    /// The chord's `Ctrl`, see [`chord_modifiers`].
    pub command: bool,
    pub super_key: bool,
    pub alt: bool,
    pub shift: bool,
    pub repeat: bool,
}

/// The press an input event stands for. egui-winit turns Ctrl+X, Ctrl+C and Ctrl+V into cut, copy and
/// paste events and sends no key event for them, so those count as the chord's key.
pub fn key_press(event: &egui::Event, swap: bool) -> Option<Press> {
    // The clipboard events come from the platform's command key, which the swap turns into Super.
    let (command, super_key) = chord_modifiers(egui::Modifiers::COMMAND, swap);
    let clipboard = |key| Some(Press { key, command, super_key, alt: false, shift: false, repeat: false });
    match event {
        egui::Event::Key { key, pressed: true, repeat, modifiers, .. } => Some(Press {
            key: key.name(),
            command: chord_modifiers(*modifiers, swap).0,
            super_key: chord_modifiers(*modifiers, swap).1,
            alt: modifiers.alt,
            shift: modifiers.shift,
            repeat: *repeat,
        }),
        egui::Event::Cut => clipboard("X"),
        egui::Event::Copy => clipboard("C"),
        egui::Event::Paste(_) => clipboard("V"),
        _ => None,
    }
}

/// Whether a key press is exactly this chord, for callers that have no Super state (a Super chord never matches).
pub fn chord_matches(chord: &Chord, key: &str, command: bool, alt: bool, shift: bool) -> bool {
    chord.key == key && chord.ctrl == command && chord.alt == alt && chord.shift == shift && !chord.super_key
}

// ------------------------------------------------------------------------------------------ palette logic

/// The English words D searches for a command: its class name split at lower-to-upper changes and at
/// non-alphanumeric to alphanumeric changes, lower-cased (`OpenFile` gives `open file`).
pub fn english_token(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if index > 0 {
            let before = chars[index - 1];
            if (ch.is_ascii_uppercase() && before.is_ascii_lowercase())
                || (ch.is_ascii_alphanumeric() && !before.is_ascii_alphanumeric())
            {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out.to_lowercase()
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntryKind {
    /// An editor command, by D command id.
    Action,
    /// An MCP tool with its JSON input schema.
    Tool(Value),
}

/// One palette row.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub description: String,
    pub category: String,
    pub kind: EntryKind,
}

impl Entry {
    fn token(&self) -> String {
        match self.kind {
            // `FileCommand.NewFile` is the class `NewFileCommand`: D searches "new file".
            EntryKind::Action => english_token(self.id.rsplit('.').next().unwrap_or(&self.id)),
            // Tool names use underscores where D class names use capitals.
            EntryKind::Tool(_) => english_token(&self.id.replace('_', " "))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

pub fn action_entries() -> Vec<Entry> {
    COMMANDS
        .iter()
        .map(|(id, category, label, description)| Entry {
            id: (*id).to_owned(),
            label: (*label).to_owned(),
            description: (*description).to_owned(),
            category: (*category).to_owned(),
            kind: EntryKind::Action,
        })
        .collect()
}

/// The tools of a `tools/list` response; the category is the part of the name before the first underscore.
pub fn parse_tools(response: &Value) -> Result<Vec<Entry>, String> {
    if let Some(error) = response["error"]["message"].as_str() {
        return Err(error.to_owned());
    }
    let tools = response["result"]["tools"].as_array().ok_or("tools/list answered without a tools array")?;
    tools
        .iter()
        .map(|tool| {
            let name = tool["name"].as_str().ok_or("a tool has no name")?;
            Ok(Entry {
                id: name.to_owned(),
                label: name.to_owned(),
                description: tool["description"].as_str().unwrap_or("").to_owned(),
                category: name.split('_').next().unwrap_or("").to_owned(),
                kind: EntryKind::Tool(tool["inputSchema"].clone()),
            })
        })
        .collect()
}

/// D `filterCommands`: a case-insensitive substring of the label, the English token or the category
/// (an empty query keeps all), then the command browser's order: lower-cased label, then id.
pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let query = query.to_lowercase();
    let mut found: Vec<&Entry> = entries
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.label.to_lowercase().contains(&query)
                || entry.token().contains(&query)
                || (!entry.category.is_empty() && entry.category.to_lowercase().contains(&query))
        })
        .collect();
    found.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    found
}

pub fn required_fields(schema: &Value) -> Vec<String> {
    schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// A starting argument object with a placeholder for each required field.
pub fn args_template(schema: &Value) -> String {
    let mut args = serde_json::Map::new();
    for key in required_fields(schema) {
        let placeholder = match schema["properties"][&key]["type"].as_str() {
            Some("integer" | "number") => json!(0),
            Some("boolean") => json!(false),
            Some("array") => json!([]),
            Some("object") => json!({}),
            _ => json!(""),
        };
        args.insert(key, placeholder);
    }
    serde_json::to_string_pretty(&Value::Object(args)).unwrap_or_else(|_| "{}".to_owned())
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

/// Parses the typed arguments and checks them against the tool's schema: a JSON object, every required
/// field present and neither null nor an empty string (the template's placeholder), and each given field of
/// its declared simple type. Empty text means `{}`.
pub fn validate_arguments(text: &str, schema: &Value) -> Result<Value, String> {
    let value: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).map_err(|error| format!("Arguments are not valid JSON: {error}"))?
    };
    let args = value.as_object().ok_or("Arguments must be a JSON object")?;
    for key in required_fields(schema) {
        if args.get(&key).is_none_or(|given| given.is_null() || given.as_str() == Some("")) {
            return Err(format!("Missing required field `{key}`"));
        }
    }
    for (key, given) in args {
        if let Some(kind) = schema["properties"][key]["type"].as_str() {
            if !type_matches(kind, given) {
                return Err(format!("Field `{key}` must be of type {kind}"));
            }
        }
    }
    Ok(value)
}

pub fn list_request() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

pub fn call_request(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// What a tool call shows the user.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub text: String,
}

const OUTCOME_LIMIT: usize = 20_000;

fn clip(text: &str) -> String {
    match text.char_indices().nth(OUTCOME_LIMIT) {
        Some((end, _)) => format!("{}... (clipped)", &text[..end]),
        None => text.to_owned(),
    }
}

pub fn call_outcome(response: Option<&Value>) -> Outcome {
    let Some(response) = response else {
        return Outcome { ok: false, text: "The endpoint sent no response".to_owned() };
    };
    if let Some(message) = response["error"]["message"].as_str() {
        return Outcome { ok: false, text: message.to_owned() };
    }
    let result = &response["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|item| item["text"].as_str())
        .map_or_else(|| result.to_string(), str::to_owned);
    Outcome { ok: result["isError"].as_bool() != Some(true), text: clip(&text) }
}

/// The `structuredContent` of a successful tool answer, or the failure text.
fn structured(response: Option<&Value>) -> Result<Value, String> {
    let outcome = call_outcome(response);
    if !outcome.ok {
        return Err(outcome.text);
    }
    response
        .map(|response| response["result"]["structuredContent"].clone())
        .filter(|content| !content.is_null())
        .ok_or_else(|| "the answer had no structured content".to_owned())
}

// ------------------------------------------------------------------------------------------ texture logic

/// One decoded texture of a Part.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotImage {
    pub slot: u64,
    pub width: usize,
    pub height: usize,
    pub premultiplied: bool,
    pub rgba: Vec<u8>,
}

/// The albedo, emissive and bump slots of a Part.
pub type PartSlots = [Option<SlotImage>; 3];

pub const SLOT_NAMES: [&str; 3] = ["Albedo", "Emissive", "Bump"];
const MAX_TEXTURE_BYTES: usize = 256 << 20;

/// The decoded texture of a `texture_get` answer (`{slot, width, height, format, base64}`). The pixels
/// are the stored ones, straight alpha, decoded from the PNG or TGA bytes.
pub fn slot_image(answer: &Value) -> Result<SlotImage, String> {
    use base64::Engine;
    let slot = answer["slot"].as_u64().ok_or("texture answer without a slot")?;
    let width = answer["width"].as_u64().ok_or("texture without a width")? as usize;
    let height = answer["height"].as_u64().ok_or("texture without a height")? as usize;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|size| *size <= MAX_TEXTURE_BYTES)
        .ok_or_else(|| format!("Texture slot {slot} is too large"))?;
    let format = match answer["format"].as_str() {
        Some("png") => image::ImageFormat::Png,
        Some("tga") => image::ImageFormat::Tga,
        _ => return Err(format!("Texture slot {slot} has an unknown image format")),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(answer["base64"].as_str().ok_or("texture without data")?)
        .map_err(|error| format!("Texture slot {slot} is not valid base64: {error}"))?;
    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|error| format!("Texture slot {slot} could not be decoded: {error}"))?
        .into_rgba8();
    if decoded.dimensions() != (width as u32, height as u32) {
        let (w, h) = decoded.dimensions();
        return Err(format!("Texture slot {slot} is {w}x{h}, expected {width}x{height}"));
    }
    Ok(SlotImage { slot, width, height, premultiplied: false, rgba: decoded.into_raw() })
}

/// The node with this id in a snapshot's node tree (a single root object or a list of roots).
pub fn find_node(nodes: &Value, id: u64) -> Option<&Value> {
    if let Some(list) = nodes.as_array() {
        return list.iter().find_map(|node| find_node(node, id));
    }
    if nodes["uuid"].as_u64() == Some(id) {
        return Some(nodes);
    }
    nodes["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

/// The slot indices a Part's model lists (`None` for an empty slot), for when no pixels can be shown.
pub fn model_slots(node: &Value) -> [Option<u64>; 3] {
    let mut slots = [None; 3];
    for (position, raw) in node["textures"].as_array().into_iter().flatten().take(3).enumerate() {
        slots[position] = raw.as_u64().filter(|slot| *slot != u64::from(u32::MAX));
    }
    slots
}

pub fn zoom_clamped(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(0.1, 10.0)
    } else {
        1.0
    }
}

// ------------------------------------------------------------------------------------------ flip pairs

/// D stores flip pairs in the puppet's extension data under this name.
pub const FLIP_CONFIG_KEY: &str = "com.inochi2d.creator.FlipConfig";
const INVALID_UUID: u32 = u32::MAX;

/// A pair of nodes that mirror each other; part 0 is filled whenever part 1 is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlipPair {
    pub parts: [Option<u32>; 2],
}

impl FlipPair {
    #[allow(dead_code)] // used by `add_pair`
    fn holds(&self, node: u32) -> bool {
        self.parts.contains(&Some(node))
    }
}

/// D `incReplaceFlipPairs`: drops pairs without nodes and moves a lone part 1 into part 0.
pub fn normalize(pairs: Vec<FlipPair>) -> Vec<FlipPair> {
    pairs
        .into_iter()
        .filter(|pair| pair.parts != [None, None])
        .map(|pair| match pair.parts {
            [None, second] => FlipPair { parts: [second, None] },
            _ => pair,
        })
        .collect()
}

/// The pair that has both parts and holds `node` (D `incGetFlipPairFor`).
#[allow(dead_code)]
pub fn pair_for(pairs: &[FlipPair], node: u32) -> Option<&FlipPair> {
    pairs.iter().find(|pair| pair.parts[0].is_some() && pair.parts[1].is_some() && pair.holds(node))
}

/// D `incAddFlipPair`: any pair that holds either node goes, then the new pair is appended.
#[allow(dead_code)]
pub fn add_pair(pairs: &[FlipPair], left: u32, right: u32) -> Result<Vec<FlipPair>, String> {
    if left == right {
        return Err("Cannot pair a node with itself".to_owned());
    }
    let mut kept: Vec<FlipPair> =
        pairs.iter().filter(|pair| !pair.holds(left) && !pair.holds(right)).cloned().collect();
    kept.push(FlipPair { parts: [Some(left), Some(right)] });
    Ok(normalize(kept))
}

/// D `incRemoveFlipPair`: removes the first pair that is exactly these two nodes, in either order.
#[allow(dead_code)]
pub fn remove_pair(pairs: &[FlipPair], left: u32, right: u32) -> Result<Vec<FlipPair>, String> {
    let at = pairs
        .iter()
        .position(|pair| pair.parts == [Some(left), Some(right)] || pair.parts == [Some(right), Some(left)])
        .ok_or("Flip pair not found")?;
    let mut rest = pairs.to_vec();
    rest.remove(at);
    Ok(normalize(rest))
}

/// D `incAutoAddFlipPairs`: every node whose name contains `left_pattern` is paired with the node named
/// like it with every occurrence replaced by `right_pattern`, when neither is paired yet.
/// `nodes` are (uuid, name) in tree order. Returns the new pairs and how many were added.
pub fn auto_add(
    pairs: &[FlipPair],
    nodes: &[(u32, String)],
    left_pattern: &str,
    right_pattern: &str,
) -> Result<(Vec<FlipPair>, usize), String> {
    if left_pattern.is_empty() || right_pattern.is_empty() {
        return Err("Both leftPattern and rightPattern are required".to_owned());
    }
    // D reads names as C strings, so a NUL ends a name.
    let name_of = |name: &str| name.split('\0').next().unwrap_or("").to_owned();
    let mut taken: HashSet<u32> = pairs.iter().flat_map(|pair| pair.parts.iter().flatten().copied()).collect();
    let mut out = pairs.to_vec();
    let mut added = 0;
    for (uuid, name) in nodes {
        let name = name_of(name);
        if taken.contains(uuid) || !name.contains(left_pattern) {
            continue;
        }
        let target = name.replace(left_pattern, right_pattern);
        let partner = nodes
            .iter()
            .find(|(other, other_name)| other != uuid && !taken.contains(other) && name_of(other_name) == target);
        if let Some((other, _)) = partner {
            out.push(FlipPair { parts: [Some(*uuid), Some(*other)] });
            taken.insert(*uuid);
            taken.insert(*other);
            added += 1;
        }
    }
    Ok((normalize(out), added))
}

/// D's drop of a node on a pair row: the node leaves every pair that held it and fills `slot` of row `row`.
/// (D leaves a copy behind when the node sat in the other slot of the same row; here it moves.)
pub fn assign(pairs: &mut [FlipPair], node: u32, row: usize, slot: usize) {
    if row >= pairs.len() || slot > 1 {
        return;
    }
    for pair in pairs.iter_mut() {
        for part in pair.parts.iter_mut().filter(|part| **part == Some(node)) {
            *part = None;
        }
    }
    pairs[row].parts[slot] = Some(node);
}

/// Reads the `FlipConfig` payload (a JSON array of `{uuid1, uuid2}`); uuids not in `known` become empty
/// parts, then pairs are normalized, as D does after resolving the nodes.
pub fn parse_flip_config(payload: &[u8], known: &HashSet<u32>) -> Result<Vec<FlipPair>, String> {
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(payload).map_err(|error| format!("FlipConfig is not JSON: {error}"))?;
    let items = value.as_array().ok_or("FlipConfig is not an array")?;
    let mut pairs = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let mut parts = [None, None];
        for (slot, key) in ["uuid1", "uuid2"].into_iter().enumerate() {
            let uuid = item[key]
                .as_u64()
                .and_then(|uuid| u32::try_from(uuid).ok())
                .ok_or_else(|| format!("FlipConfig entry {index} has no valid {key}"))?;
            parts[slot] = known.contains(&uuid).then_some(uuid);
        }
        pairs.push(FlipPair { parts });
    }
    Ok(normalize(pairs))
}

/// The payload D stores for these pairs, or `None` when the key is to be removed (no pairs).
pub fn dump_flip_config(pairs: &[FlipPair]) -> Option<Vec<u8>> {
    if pairs.is_empty() {
        return None;
    }
    let items: Vec<Value> = pairs
        .iter()
        .map(|pair| {
            json!({
                "uuid1": pair.parts[0].unwrap_or(INVALID_UUID),
                "uuid2": pair.parts[1].unwrap_or(INVALID_UUID),
            })
        })
        .collect();
    serde_json::to_vec(&Value::Array(items)).ok()
}

/// The pairs and the message for a window that was opened from an `extdata_get` answer.
pub fn flip_from_answer(answer: &Value, known: &HashSet<u32>) -> (Vec<FlipPair>, String) {
    use base64::Engine;
    if answer["present"].as_bool() != Some(true) {
        return (Vec::new(), "The document has no flip pairs yet.".to_owned());
    }
    let parsed = answer["base64"]
        .as_str()
        .ok_or_else(|| "the stored data is missing".to_owned())
        .and_then(|text| {
            base64::engine::general_purpose::STANDARD.decode(text).map_err(|error| format!("not valid base64: {error}"))
        })
        .and_then(|payload| parse_flip_config(&payload, known));
    match parsed {
        Ok(pairs) => (pairs, "Read from the document.".to_owned()),
        Err(error) => (Vec::new(), format!("The stored flip pairs were not read: {error}")),
    }
}

/// The `extdata_set` arguments that store these pairs, or remove the key when there are none.
pub fn flip_store_arguments(pairs: &[FlipPair]) -> Value {
    use base64::Engine;
    json!({
        "key": FLIP_CONFIG_KEY,
        "base64": dump_flip_config(pairs).map(|payload| base64::engine::general_purpose::STANDARD.encode(payload)),
    })
}

/// (uuid, name, type) of every node in the snapshot's tree, parents before children.
pub fn tree_nodes(nodes: &Value) -> Vec<(u32, String, String)> {
    fn walk(node: &Value, out: &mut Vec<(u32, String, String)>) {
        if let Some(uuid) = node["uuid"].as_u64().and_then(|uuid| u32::try_from(uuid).ok()) {
            out.push((
                uuid,
                node["name"].as_str().unwrap_or("").to_owned(),
                node["type"].as_str().unwrap_or("Node").to_owned(),
            ));
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    match nodes.as_array() {
        Some(list) => list.iter().for_each(|node| walk(node, &mut out)),
        None => walk(nodes, &mut out),
    }
    out
}

// ------------------------------------------------------------------------------------------ frame state

/// What the windows of one frame share: the host's data plus the requests collected so far.
struct Env<'a> {
    all_keys: [bool; 2],
    ctx: &'a egui::Context,
    snapshot: &'a Value,
    selected: Option<u64>,
    project: Option<&'a Path>,
    store: &'a mut Settings,
    store_path: Option<&'a Path>,
    shortcuts: &'a mut ShortcutRegistry,
    locales: &'a [LocaleEntry],
    lang: &'a str,
    tr: &'a dyn Fn(&str) -> String,
    mcp: &'a McpStatus,
    out: Vec<Request>,
}

impl Env<'_> {
    fn t(&self, msgid: &str) -> String {
        (self.tr)(msgid)
    }

    fn save_store(&mut self) {
        if let Some(path) = self.store_path {
            if let Err(error) = self.store.save(path) {
                self.out.push(Request::Notice(format!("Settings not saved: {error}")));
            }
        }
    }

    fn save_shortcuts(&mut self) {
        match self.shortcuts.save_to(self.store, Platform::current()) {
            Ok(()) => self.save_store(),
            Err(error) => self.out.push(Request::Notice(error)),
        }
    }

    fn is_inx(&self) -> bool {
        self.snapshot["documentFormat"].as_str() == Some("inx")
    }

    fn command_enabled(&self, id: &str) -> bool {
        let flag = |key: &str| self.snapshot[key].as_bool() == Some(true);
        match id {
            "EditCommand.Undo" => flag("canUndo"),
            "EditCommand.Redo" => flag("canRedo"),
            "NodeCommand.CutNode" | "NodeCommand.CopyNode" => self.is_inx() && self.selected.is_some(),
            "AnimationCommand.KeyAll" => self.all_keys[0],
            "AnimationCommand.UnkeyAll" => self.all_keys[1],
            "NodeCommand.PasteNode" => {
                self.is_inx() && self.snapshot["clipboard"].as_array().is_some_and(|clipboard| !clipboard.is_empty())
            }
            _ => true,
        }
    }

    fn shortcut_text(&self, id: &str) -> String {
        let swap = mac_swap(self.store);
        self.shortcuts.binding_for(id).map(|binding| chord_text(&binding.chord, Platform::current(), swap)).unwrap_or_default()
    }
}

fn labelled_text(ui: &mut egui::Ui, label: &str, text: &mut String) -> egui::Response {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.text_edit_singleline(text).labelled_by(caption.id)
    })
    .inner
}

fn labelled_number<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    suffix: &str,
) -> egui::Response {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.add(egui::DragValue::new(value).range(range).suffix(suffix)).labelled_by(caption.id)
    })
    .inner
}

/// A button whose accessibility name differs from its (short) text.
fn named_button(ui: &mut egui::Ui, text: &str, name: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(enabled, egui::Button::new(text));
    let name = name.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name.clone()));
    response.clicked()
}

/// A text link that screen readers see as a button (egui turns selectable link labels into plain labels).
fn link_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let color = ui.visuals().hyperlink_color;
    ui.add(egui::Button::new(egui::RichText::new(text.into()).color(color)).frame(false))
}

fn link_row(ui: &mut egui::Ui, label: &str, url: &str) {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        ui.label(url);
    });
}

// ------------------------------------------------------------------------------------------ shared controls

fn language_combo(ui: &mut egui::Ui, env: &mut Env) {
    let locales = env.locales;
    let current = if env.lang == "en" {
        "English".to_owned()
    } else {
        locales.iter().find(|entry| entry.code == env.lang).map_or_else(|| env.lang.to_owned(), |e| e.name.clone())
    };
    egui::ComboBox::from_label(env.t("Language")).selected_text(current).show_ui(ui, |ui| {
        if ui.selectable_label(env.lang == "en", "English").clicked() {
            env.out.push(Request::SetLanguage("en".to_owned()));
        }
        for entry in locales {
            if ui.selectable_label(env.lang == entry.code, entry.name.as_str()).clicked() {
                env.out.push(Request::SetLanguage(entry.code.clone()));
            }
        }
    });
}

fn theme_combo(ui: &mut egui::Ui, env: &mut Env) -> bool {
    let dark = dark_mode(env.store).unwrap_or(ui.visuals().dark_mode);
    let mut changed = false;
    egui::ComboBox::from_label(env.t("Color Theme"))
        .selected_text(env.t(if dark { "Dark" } else { "Light" }))
        .show_ui(ui, |ui| {
            for (value, label) in [(true, "Dark"), (false, "Light")] {
                if ui.selectable_label(dark == value, env.t(label)).clicked() {
                    changed |= env.store.set("DarkMode", value).is_ok();
                }
            }
        });
    changed
}

fn scale_row(ui: &mut egui::Ui, env: &mut Env) -> bool {
    if Platform::current() == Platform::MacOs {
        return false;
    }
    let mut percent = ui_scale_percent(env.store);
    let label = env.t("UI Scale");
    let changed = labelled_number(ui, &label, &mut percent, 100..=200, "%").changed();
    changed && set_ui_scale_percent(env.store, percent).is_ok()
}

// ------------------------------------------------------------------------------------------ settings window

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    Look,
    Files,
    Shortcuts,
    Integration,
}

struct Capture {
    command: String,
    repeat: bool,
    preview: String,
}

#[derive(Default)]
struct SettingsWindow {
    pane: Pane,
    capture: Option<Capture>,
    filter: String,
    message: String,
    dirty: bool,
    headers: Option<String>,
    headers_error: Option<String>,
    /// The MCP fields as typed; stored and applied with the Apply button.
    mcp_draft: Option<McpSettings>,
}

fn modifier_preview(modifiers: egui::Modifiers, swap: bool) -> String {
    let mut parts = Vec::new();
    let (ctrl, super_key) = chord_modifiers(modifiers, swap);
    for (on, name) in [(ctrl, "Ctrl"), (super_key, "Super"), (modifiers.alt, "Alt"), (modifiers.shift, "Shift")] {
        if on {
            parts.push(name);
        }
    }
    modifier_labels(parts.join("+"), Platform::current(), swap)
}

impl SettingsWindow {
    /// Reads the key press that finishes a shortcut capture. Runs before anything else sees the events and
    /// removes the key events, so a press made for binding does not also fire the command it was bound from.
    fn capture_step(&mut self, env: &mut Env) {
        let Some(capture) = self.capture.as_mut() else { return };
        let events = env.ctx.input(|input| input.events.clone());
        let mut cancelled = false;
        let mut chord = None;
        for event in &events {
            if let egui::Event::Key { key, pressed: true, modifiers, .. } = event {
                if *key == egui::Key::Escape {
                    cancelled = true;
                } else {
                    let (ctrl, super_key) = chord_modifiers(*modifiers, mac_swap(env.store));
                    chord = chord_from_press(key.name(), ctrl, modifiers.alt, modifiers.shift)
                        .map(|found| Chord { super_key, ..found });
                }
                break;
            }
        }
        capture.preview = modifier_preview(env.ctx.input(|input| input.modifiers), mac_swap(env.store));
        env.ctx.input_mut(|input| input.events.retain(|event| !matches!(event, egui::Event::Key { .. })));
        if cancelled {
            self.capture = None;
        } else if let Some(chord) = chord {
            let text = chord_text(&chord, Platform::current(), mac_swap(env.store));
            let displaced = env.shortcuts.register(&capture.command, chord, capture.repeat);
            self.message = match displaced.and_then(|id| COMMANDS.iter().find(|row| row.0 == id)) {
                Some(row) => format!("{text} was used by {}; that shortcut is now unassigned", env.t(row.2)),
                None => format!("{text} assigned"),
            };
            self.capture = None;
            env.save_shortcuts();
        }
    }

    /// Returns false once the window is closed.
    fn show(&mut self, env: &mut Env) -> bool {
        let mut open = true;
        let mut done = false;
        let ctx = env.ctx;
        egui::Window::new(env.t("Settings"))
            .id(egui::Id::new("editor_windows_settings"))
            .open(&mut open)
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(130.0);
                        for (pane, label) in [
                            (Pane::Look, "Look and Feel"),
                            (Pane::Files, "File Handling"),
                            (Pane::Shortcuts, "Shortcuts"),
                            (Pane::Integration, "Integration"),
                        ] {
                            ui.selectable_value(&mut self.pane, pane, env.t(label));
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| match self.pane {
                        Pane::Look => self.look_pane(ui, env),
                        Pane::Files => self.files_pane(ui, env),
                        Pane::Shortcuts => self.shortcuts_pane(ui, env),
                        Pane::Integration => self.integration_pane(ui, env),
                    });
                });
                ui.separator();
                if ui.button(env.t("Done")).clicked() {
                    done = true;
                }
            });
        if !open || done {
            self.capture = None;
            env.save_shortcuts();
            return false;
        }
        true
    }

    fn look_pane(&mut self, ui: &mut egui::Ui, env: &mut Env) {
        ui.heading(env.t("Look and Feel"));
        let mut changed = theme_combo(ui, env);
        language_combo(ui, env);
        changed |= scale_row(ui, env);
        let mut dyslexic = use_open_dyslexic(env.store);
        if ui.checkbox(&mut dyslexic, env.t("Use OpenDyslexic Font")).changed()
            && env.store.set(KEY_OPEN_DYSLEXIC, dyslexic).is_ok()
        {
            changed = true;
        }
        ui.add_space(6.0);
        ui.heading(env.t("Undo History"));
        let mut limit = max_undo_history(env.store) as i64;
        let (min, max) = MAX_UNDO_HISTORY_RANGE;
        if labelled_number(ui, &env.t("Max Undo History"), &mut limit, min as i64..=max as i64, "").changed() {
            if let Ok(request) = set_max_undo_history(env.store, limit) {
                env.out.push(request);
                changed = true;
            }
        }
        if changed {
            env.save_store();
        }
    }

    fn files_pane(&mut self, ui: &mut egui::Ui, env: &mut Env) {
        ui.heading(env.t("Autosaves"));
        let mut form = autosave_form(env.store);
        let mut changed = ui.checkbox(&mut form.enabled, env.t("Enable Autosaves")).changed();
        let (mut interval, mut limit) = (i64::from(form.interval_minutes), i64::from(form.max_backups));
        changed |= labelled_number(ui, &env.t("Save Interval (Minutes)"), &mut interval, 1..=1440, "").changed();
        changed |= labelled_number(ui, &env.t("Maximum Autosaves"), &mut limit, 1..=1000, "").changed();
        if changed {
            let clamped = autosave::Settings::new(form.enabled, interval as u32, limit as u32);
            if store_autosave(env.store, &clamped).is_ok() {
                env.out.push(Request::AutosaveChanged);
                env.save_store();
            }
        }

        ui.add_space(6.0);
        ui.heading(env.t("Unsaved changes"));
        let current = window_shell::close_setting(env.store);
        let mut picked = None;
        egui::ComboBox::from_label(env.t("Save project on close"))
            .selected_text(env.t(if current == CloseSetting::Ask { "Always ask" } else { "Don't save" }))
            .show_ui(ui, |ui| {
                for (setting, label) in [(CloseSetting::Ask, "Always ask"), (CloseSetting::DontSave, "Don't save")] {
                    if ui.selectable_label(current == setting, env.t(label)).clicked() {
                        picked = Some(setting);
                    }
                }
            });
        if let Some(setting) = picked {
            if window_shell::set_close_setting(env.store, setting).is_ok() {
                env.save_store();
            }
        }
        ui.add_space(6.0);
        ui.heading(env.t("Import behaviour"));
        let keep = choice(env.store, "KeepLayerFolder", "Ask", &["Ask", "Preserve", "NotPreserve"]);
        let shown = KEEP_LAYER_FOLDER.iter().find(|(key, _)| *key == keep).map_or("", |(_, label)| *label);
        let mut picked = None;
        egui::ComboBox::from_label(env.t("Preserve structure")).selected_text(env.t(shown)).show_ui(ui, |ui| {
            for (key, label) in KEEP_LAYER_FOLDER {
                if ui.selectable_label(keep == key, env.t(label)).clicked() {
                    picked = Some(("KeepLayerFolder", key));
                }
            }
        });
        ui.add_space(6.0);
        ui.heading(env.t("Layer group replacement"));
        let group = choice(env.store, "LayerGroupReplacement", "Node", &LAYER_GROUP_REPLACEMENT);
        egui::ComboBox::from_label(env.t("Replace LayerGroup with")).selected_text(env.t(&group)).show_ui(ui, |ui| {
            for key in LAYER_GROUP_REPLACEMENT {
                if ui.selectable_label(group == key, env.t(key)).clicked() {
                    picked = Some(("LayerGroupReplacement", key));
                }
            }
        });
        ui.label(env.t("Node type to create for imported layer groups when preserving structure."));
        if group == "MeshGroup" {
            ui.weak("This editor imports a MeshGroup replacement as Node.");
        }
        if let Some((key, value)) = picked {
            if let Ok(request) = set_import_default(env.store, key, value) {
                env.out.push(request);
                env.save_store();
            }
        }
    }

    fn text_setting(&mut self, ui: &mut egui::Ui, env: &mut Env, label: &str, key: &str, default: &str) {
        let mut value = env.store.get_or::<String>(key, default.to_owned()).unwrap_or_else(|_| default.to_owned());
        let response = labelled_text(ui, label, &mut value);
        if response.changed() && env.store.set(key, value).is_ok() {
            self.dirty = true;
        }
        if response.lost_focus() && self.dirty {
            self.dirty = false;
            env.save_store();
        }
    }

    fn integration_pane(&mut self, ui: &mut egui::Ui, env: &mut Env) {
        ui.heading("Coding agent (ACP)");
        self.text_setting(ui, env, "Command", "ACP.Command", "");
        self.text_setting(ui, env, "Working Dir (optional)", "ACP.Workdir", "");
        ui.add_space(6.0);
        ui.heading("MCP endpoint offered to the agent");
        self.text_setting(ui, env, "Server name", "MCP.Name", "soul2d");
        let stored = env.store.get_or::<String>("MCP.Headers", "[]".to_owned()).unwrap_or_else(|_| "[]".to_owned());
        let draft = self.headers.get_or_insert(stored);
        let response = labelled_text(ui, "Extra headers (JSON array of name/value)", draft);
        if response.changed() {
            match validate_headers(draft) {
                Ok(()) => {
                    self.headers_error = None;
                    let text = draft.clone();
                    if env.store.set("MCP.Headers", text).is_ok() {
                        self.dirty = true;
                    }
                }
                Err(error) => self.headers_error = Some(error),
            }
        }
        if response.lost_focus() && self.dirty {
            self.dirty = false;
            env.save_store();
        }
        if let Some(error) = &self.headers_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error.as_str());
        }
        ui.add_space(6.0);
        self.mcp_server_section(ui, env);
    }

    /// The editor's own MCP server (D `MCP.Enabled`, `MCP.Host`, `MCP.Port`, `MCP.authEnabled`).
    fn mcp_server_section(&mut self, ui: &mut egui::Ui, env: &mut Env) {
        ui.heading("MCP server (this editor)");
        let stored = McpSettings::load(env.store);
        let draft = self.mcp_draft.get_or_insert(stored.clone());
        ui.checkbox(&mut draft.enabled, "Enable MCP server");
        labelled_text(ui, "MCP host", &mut draft.host);
        let mut port = i64::from(draft.port);
        labelled_number(ui, "MCP port", &mut port, 0..=65535, "");
        draft.port = port.clamp(0, 65535) as u16;
        ui.checkbox(&mut draft.auth, "Require a bearer token (MCP.authEnabled)");
        let changed = *draft != stored;
        if named_button(ui, "Apply MCP settings", "Apply MCP settings", changed || env.mcp.error.is_some()) {
            if draft.store(env.store).is_ok() {
                env.save_store();
                env.out.push(Request::McpApply);
            }
        }
        match (&env.mcp.url, &env.mcp.error) {
            (Some(url), _) => {
                ui.label(format!("Serving on {url}"));
            }
            (None, Some(error)) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error.as_str());
            }
            (None, None) => {
                ui.weak("Off: no port is opened.");
            }
        }
        if let Some(token) = &env.mcp.token {
            let caption = ui.label("Bearer token (copy it into the client; it changes every run)");
            let mut shown = token.clone();
            ui.add(egui::TextEdit::singleline(&mut shown).desired_width(480.0)).labelled_by(caption.id);
        }
        ui.weak("SOUL2D_MCP_BEARER, when set, is the token instead and is never shown; a non-loopback host needs it.");
        ui.weak("D's OAuth flow, approval popup and SSE stream are replaced by this static bearer token.");
    }

    fn shortcuts_pane(&mut self, ui: &mut egui::Ui, env: &mut Env) {
        ui.heading(env.t("Shortcuts"));
        ui.label(env.t("Assign shortcuts to commands. Click Set and press keys. Use Clear to remove a binding."));
        if let Some(capture) = &mut self.capture {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(230, 180, 50), env.t("Capturing shortcut..."));
                ui.checkbox(&mut capture.repeat, env.t("Repeat"));
                let shown = if capture.preview.is_empty() { env.t("Press keys...") } else { capture.preview.clone() };
                ui.label(shown);
            });
            if ui.button(env.t("Cancel")).clicked() {
                self.capture = None;
            }
        }
        if !self.message.is_empty() {
            ui.label(self.message.as_str());
        }
        let caption = ui.label(env.t("Filter actions by name:"));
        ui.text_edit_singleline(&mut self.filter).labelled_by(caption.id);
        let filter = self.filter.to_lowercase();
        let mut start = None;
        let mut clear = None;
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            let mut categories: Vec<&str> = COMMANDS.iter().map(|row| row.1).collect();
            categories.dedup();
            for category in categories {
                let rows: Vec<_> = COMMANDS
                    .iter()
                    .filter(|row| row.1 == category && env.t(row.2).to_lowercase().contains(&filter))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(env.t(category)).default_open(true).show(ui, |ui| {
                    egui::Grid::new(("shortcut_grid", category)).striped(true).show(ui, |ui| {
                        ui.strong(env.t("Action"));
                        ui.strong(env.t("Shortcut"));
                        ui.strong(env.t("Edit"));
                        ui.end_row();
                        for (id, _, label, _) in rows {
                            let label = env.t(label);
                            ui.label(label.as_str());
                            let capturing = self.capture.as_ref().is_some_and(|capture| capture.command == *id);
                            let current = env.shortcut_text(id);
                            ui.label(match (capturing, current.is_empty()) {
                                (true, _) => env.t("Press keys..."),
                                (false, true) => env.t("<none>"),
                                (false, false) => current,
                            });
                            ui.horizontal(|ui| {
                                if named_button(ui, "Set", &format!("Set shortcut for {label}"), true) {
                                    start = Some((*id).to_owned());
                                }
                                if named_button(ui, "Clear", &format!("Clear shortcut for {label}"), true) {
                                    clear = Some((*id).to_owned());
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
            }
        });
        if let Some(command) = start {
            self.capture = Some(Capture { command, repeat: false, preview: String::new() });
            self.message.clear();
        }
        if let Some(command) = clear {
            env.shortcuts.clear(&command);
            env.save_shortcuts();
        }
    }
}

// ------------------------------------------------------------------------------------------ stats for nerds

/// The small stats window. Returns false when the user closed it. It repaints only when the editor does,
/// so the frame time is that of the last repaint, not a steady rate.
fn show_nerd_stats(env: &mut Env) -> bool {
    let rows = nerd_stats_rows(env.snapshot, env.ctx.input(|input| input.unstable_dt));
    let mut open = true;
    egui::Window::new(env.t("Stats for Nerds"))
        .id(egui::Id::new("editor_windows_nerd_stats"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(env.ctx, |ui| {
            egui::Grid::new("editor_windows_nerd_stats_grid").show(ui, |ui| {
                for (label, value) in &rows {
                    ui.label(env.t(label));
                    ui.label(value.as_str());
                    ui.end_row();
                }
            });
        });
    open
}

// ------------------------------------------------------------------------------------------ welcome and about

struct Welcome {
    step: u8,
}

/// What the user chose on the welcome page.
enum WelcomeChoice {
    New,
    Open,
    Import,
    Recent(String),
}

impl Welcome {
    /// Returns false once the window is closed.
    fn show(&mut self, env: &mut Env) -> (bool, Option<WelcomeChoice>) {
        let mut open = true;
        let mut choice = None;
        let ctx = env.ctx;
        egui::Window::new(env.t("soul2d Start"))
            .id(egui::Id::new("editor_windows_welcome"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Soul2D");
                ui.label(format!("{} {}", env!("CARGO_PKG_VERSION"), BANNER_ARTIST_NAME));
                ui.label(BANNER_ARTIST_PAGE);
                ui.separator();
                if self.step == 0 {
                    ui.strong(env.t("Quick Setup"));
                    let mut changed = theme_combo(ui, env);
                    language_combo(ui, env);
                    changed |= scale_row(ui, env);
                    if changed {
                        env.save_store();
                    }
                    if ui.button(env.t("Next")).clicked() {
                        if env.store.set(KEY_QUICK_SETUP, true).is_ok() {
                            env.save_store();
                        }
                        self.step = 1;
                    }
                } else {
                    ui.columns(2, |columns| {
                        columns[0].strong(env.t("Create Project"));
                        if link_button(&mut columns[0], env.t("New...")).clicked() {
                            choice = Some(WelcomeChoice::New);
                        }
                        if link_button(&mut columns[0], env.t("Open...")).clicked() {
                            choice = Some(WelcomeChoice::Open);
                        }
                        if link_button(&mut columns[0], env.t("Import...")).on_hover_text("Import PSD").clicked() {
                            choice = Some(WelcomeChoice::Import);
                        }
                        columns[0].add_space(6.0);
                        columns[0].strong(env.t("Recent Projects"));
                        let recent = recent_projects(env.store);
                        if recent.is_empty() {
                            columns[0].label(env.t("No recent projects..."));
                        }
                        for path in recent.iter().take(WELCOME_RECENT) {
                            if link_button(&mut columns[0], base_name(path)).on_hover_text(path.as_str()).clicked() {
                                choice = Some(WelcomeChoice::Recent(path.clone()));
                            }
                        }
                        columns[1].strong(env.t("On the Web"));
                        link_row(&mut columns[1], &env.t("Website"), WEBSITE_URI);
                        link_row(&mut columns[1], &env.t("Documentation"), DOCS_URI);
                    });
                }
            });
        if !open && self.step == 1 {
            // D marks the quick setup done when the window closes after its first page.
            let _ = env.store.set(KEY_QUICK_SETUP, true);
            env.save_store();
        }
        (open && choice.is_none(), choice)
    }
}

fn show_about(env: &mut Env) -> bool {
    let mut open = true;
    let ctx = env.ctx;
    egui::Window::new(env.t("About"))
        .id(egui::Id::new("editor_windows_about"))
        .open(&mut open)
        .default_size([520.0, 440.0])
        .show(ctx, |ui| {
            ui.heading("Soul2D (development)");
            // D prints its gitver string and the soul2d_live and ImGui versions here.
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("BSD 2-Clause License. Copyright (c) 2020-2023 Inochi2D Project, 2024 nijigenerate Project, 2026 Soul2D contributors");
            ui.separator();
            link_row(ui, "Website", WEBSITE_URI);
            link_row(ui, &env.t("Documentation"), DOCS_URI);
            link_row(ui, &env.t("Report a Bug"), BUG_REPORT_URI);
            link_row(ui, &env.t("Request a Feature"), FEATURE_REQ_URI);
            link_row(ui, "Banner artist", &format!("{BANNER_ARTIST_NAME} ({BANNER_ARTIST_PAGE})"));
            if let Some(dir) = env.store_path.and_then(Path::parent) {
                link_row(ui, "Configuration folder", &dir.display().to_string());
            }
            ui.separator();
            ui.strong(env.t("Credits"));
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (heading, lines) in CREDITS {
                    ui.label(egui::RichText::new(*heading).strong());
                    for line in *lines {
                        ui.label(*line);
                    }
                    ui.add_space(4.0);
                }
            });
        });
    open
}

fn request_address_dialog(env: &mut Env, operation: AddressDialogOperation) {
    let arguments = match operation {
        AddressDialogOperation::Open(target) => json!({"action":"open", "target":target.id()}),
        AddressDialogOperation::Copy => json!({"action":"copy"}),
        AddressDialogOperation::Close => json!({"action":"close"}),
    };
    env.out.push(Request::Mcp(McpTarget::AddressDialog, call_request("editor_address_dialog", arguments)));
}

fn address_label(target: AddressDialogTarget) -> &'static str {
    match target {
        AddressDialogTarget::Configuration => "Open Configuration Folder",
        AddressDialogTarget::OnlineDocumentation => "Online Documentation",
        AddressDialogTarget::Soul2dLiveDocumentation => "Soul2DLive Documentation",
        AddressDialogTarget::BugReport => "Report a Bug",
        AddressDialogTarget::FeatureRequest => "Request a Feature",
    }
}

struct AddressDialog {
    target: AddressDialogTarget,
    title: String,
    address: String,
}

fn show_link(env: &mut Env, dialog: &AddressDialog) -> bool {
    let mut open = true;
    let ctx = env.ctx;
    egui::Window::new(&dialog.title)
        .id(egui::Id::new("editor_windows_link"))
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            let mut shown = dialog.address.clone();
            let caption = ui.label("Address (not opened by the editor)");
            ui.add(egui::TextEdit::singleline(&mut shown).desired_width(420.0)).labelled_by(caption.id);
            if ui.button("Copy").clicked() {
                request_address_dialog(env, AddressDialogOperation::Copy);
            }
        });
    open
}

// ------------------------------------------------------------------------------------------ path prompt

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Open,
    SaveAs,
    ImportPsd,
    ImportKra,
    ImportInp,
    ImportFolder,
    Export(ExportKind),
    MergeInp,
    MergeImages,
    ImportSession,
    Screenshot,
    /// The name View > Workspace > Save Workspace gives the layout.
    Workspace,
    /// Tools > Export textures. The text is an existing directory.
    TextureExport,
    /// Tools > Replace textures. The text is the directory `texture.export` wrote.
    TextureReplace,
}

struct PathPrompt {
    kind: PromptKind,
    text: String,
    focused: bool,
}

fn import_request(store: &Settings, op: &str, path: &str) -> Value {
    let group = choice(store, "LayerGroupReplacement", "Node", &LAYER_GROUP_REPLACEMENT);
    let group = if ["Node", "Composite", "DynamicComposite"].contains(&group.as_str()) { group } else { "Node".to_owned() };
    let keep = choice(store, "KeepLayerFolder", "Ask", &["Ask", "Preserve", "NotPreserve"]);
    json!({"op": op, "path": path, "keepStructure": keep != "NotPreserve", "layerGroupNodeType": group})
}

impl PathPrompt {
    fn new(kind: PromptKind, text: String) -> Self {
        Self { kind, text, focused: false }
    }

    /// Returns (still open, the request to send).
    fn show(&mut self, env: &mut Env) -> (bool, Option<Request>) {
        let mut open = true;
        let mut cancelled = false;
        let mut request = None;
        let (title, verb) = match self.kind {
            PromptKind::Open => (env.t("Open"), env.t("Open")),
            PromptKind::SaveAs => (env.t("Save As..."), env.t("Save")),
            PromptKind::ImportPsd => ("Import PSD".to_owned(), env.t("Import")),
            PromptKind::ImportKra => (env.t("Import KRA...").trim_end_matches('.').to_owned(), env.t("Import")),
            PromptKind::Export(ExportKind::Image) => (env.t("Export Image...").trim_end_matches('.').to_owned(), env.t("Export")),
            PromptKind::Export(ExportKind::Frames) => (env.t("Export Frames"), env.t("Export")),
            PromptKind::Export(ExportKind::Inp) => (env.t("Export soul2d_live puppet"), env.t("Export")),
            PromptKind::Export(ExportKind::Video) => (env.t("Export Video...").trim_end_matches('.').to_owned(), env.t("Export")),
            PromptKind::ImportInp => (env.t("Import soul2d_live puppet"), env.t("Import")),
            PromptKind::ImportFolder => (env.t("Import Image Folder"), env.t("Import")),
            PromptKind::MergeInp => (env.t("Merge soul2d project"), env.t("Merge")),
            PromptKind::MergeImages => (env.t("Merge Image Files"), env.t("Merge")),
            PromptKind::ImportSession => (env.t("Import Inochi Session Data"), env.t("Import")),
            PromptKind::Screenshot => (env.t("Save Screenshot"), env.t("Save")),
            PromptKind::Workspace => (env.t("Save Workspace"), env.t("Save")),
            PromptKind::TextureExport => (env.t("Export textures...").trim_end_matches('.').to_owned(), env.t("Export")),
            PromptKind::TextureReplace => (env.t("Replace textures...").trim_end_matches('.').to_owned(), env.t("Replace")),
        };
        let ctx = env.ctx;
        egui::Window::new(title)
            .id(egui::Id::new("editor_windows_path_prompt"))
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                let caption = ui.label(match self.kind {
                    PromptKind::Workspace => "Name",
                    PromptKind::TextureExport | PromptKind::TextureReplace => "Directory",
                    _ => "Path",
                });
                let field = ui.add(egui::TextEdit::singleline(&mut self.text).desired_width(380.0));
                field.clone().labelled_by(caption.id);
                if !self.focused {
                    field.request_focus();
                    self.focused = true;
                }
                let path = self.text.trim().to_owned();
                let enter = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if self.kind != PromptKind::Workspace && ui.button(env.t("Browse...")).clicked() {
                        if let Some(chosen) = browse(self.kind, &self.text) {
                            self.text = chosen;
                        }
                    }
                    if (ui.add_enabled(!path.is_empty(), egui::Button::new(verb)).clicked() || enter) && !path.is_empty() {
                        request = Some(match self.kind {
                            PromptKind::Open => Request::Open(path.clone()),
                            PromptKind::SaveAs => Request::SaveAs(path.clone()),
                            PromptKind::ImportPsd => Request::Core(import_request(env.store, "import.psd", &path)),
                            PromptKind::ImportKra => Request::Core(import_request(env.store, "import.kra", &path)),
                            PromptKind::Export(kind) => Request::Export(kind, path.clone()),
                            PromptKind::ImportInp => Request::Core(json!({"op": "import.inp", "path": path})),
                            PromptKind::ImportFolder => Request::Core(json!({"op": "import.folder", "path": path})),
                            PromptKind::MergeInp => Request::Core(json!({"op": "merge.inp", "path": path})),
                            PromptKind::MergeImages => {
                                // Several files are separated by `|`; the Parts go under the selected node.
                                let mut request = json!({"op": "merge.images", "paths": path});
                                if let Some(node) = env.selected {
                                    request["parent"] = json!(node);
                                }
                                Request::Core(request)
                            }
                            PromptKind::ImportSession => Request::Core(json!({"op": "tool.import_session_data", "path": path})),
                            PromptKind::Screenshot => Request::Core(json!({"op": "view.screenshot", "path": path})),
                            PromptKind::TextureExport => Request::Core(json!({"op": "texture.export", "directory": path})),
                            PromptKind::TextureReplace => Request::Core(json!({"op": "texture.replace", "directory": path})),
                            PromptKind::Workspace => Request::Mcp(
                                McpTarget::Layout,
                                call_request("editor_workspace_save", json!({"name": path})),
                            ),
                        });
                    }
                    if ui.button(env.t("Cancel")).clicked() {
                        cancelled = true;
                    }
                });
            });
        (open && !cancelled && request.is_none(), request)
    }
}

/// The operating system's file dialog for a path prompt (#87). It fills the prompt instead of submitting it, starting
/// in the typed directory when there is one; `None` when it is cancelled.
pub(crate) fn browse(kind: PromptKind, typed: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    let typed = std::path::Path::new(typed.trim());
    if let Some(directory) = std::iter::once(typed).chain(typed.parent()).find(|path| path.is_dir()) {
        dialog = dialog.set_directory(directory);
    }
    let chosen = match kind {
        PromptKind::Workspace => return None,
        PromptKind::ImportFolder | PromptKind::TextureExport | PromptKind::TextureReplace => dialog.pick_folder(),
        PromptKind::MergeImages => {
            let files = dialog.add_filter("Images", &["png", "tga", "jpg", "jpeg"]).pick_files()?;
            return Some(files.iter().map(|file| file.display().to_string()).collect::<Vec<_>>().join("|"));
        }
        PromptKind::Open => dialog.pick_file(),
        PromptKind::ImportSession => dialog.add_filter("Soul2D project", &["inx", "inp"]).pick_file(),
        // Save keeps a document's own format: INX, or CMO3/CAN3 for a native Live2D document.
        PromptKind::SaveAs => dialog.add_filter("Soul2D project", &["inx"]).add_filter("Live2D model", &["cmo3"])
            .add_filter("Live2D animation", &["can3"]).save_file(),
        PromptKind::ImportPsd => dialog.add_filter("Photoshop document", &["psd"]).pick_file(),
        PromptKind::ImportKra => dialog.add_filter("Krita document", &["kra"]).pick_file(),
        PromptKind::ImportInp => dialog.add_filter("soul2d_live puppet", &["inp"]).pick_file(),
        PromptKind::MergeInp => dialog.add_filter("Soul2D project", &["inx", "inp"]).pick_file(),
        PromptKind::Export(ExportKind::Inp) => dialog.add_filter("soul2d_live puppet", &["inp"]).save_file(),
        PromptKind::Export(ExportKind::Image) => dialog.add_filter("Image", &["png", "tga", "jpg", "jpeg"]).save_file(),
        PromptKind::Export(ExportKind::Frames) => dialog.add_filter("Image", &["png", "tga"]).save_file(),
        PromptKind::Export(ExportKind::Video) => dialog.add_filter("Video", &["mp4", "webm", "mov"]).save_file(),
        PromptKind::Screenshot => dialog.add_filter("PNG image", &["png"]).save_file(),
    };
    chosen.map(|path| path.display().to_string())
}

// ------------------------------------------------------------------------------------------ command palette

enum PaletteAction {
    Run(String),
    Call { name: String, arguments: Value },
    Refresh,
}

struct Palette {
    tools: Vec<Entry>,
    query: String,
    chosen: Option<String>,
    args: String,
    args_error: Option<String>,
    list_error: Option<String>,
    need_list: bool,
    list_sent: bool,
    running: bool,
    outcome: Option<Outcome>,
    focus: bool,
}

impl Palette {
    fn new() -> Self {
        Self {
            tools: Vec::new(),
            query: String::new(),
            chosen: None,
            args: String::new(),
            args_error: None,
            list_error: None,
            need_list: true,
            list_sent: false,
            running: false,
            outcome: None,
            focus: true,
        }
    }

    fn choose(&mut self, entry: &Entry) {
        if self.chosen.as_deref() != Some(&entry.id) {
            self.chosen = Some(entry.id.clone());
            self.args = match &entry.kind {
                EntryKind::Tool(schema) => args_template(schema),
                EntryKind::Action => String::new(),
            };
            self.args_error = None;
            self.outcome = None;
        }
    }

    /// Returns (still open, what to do).
    fn show(&mut self, env: &Env) -> (bool, Option<PaletteAction>) {
        let mut open = true;
        let mut action = None;
        let mut activate = false;
        let mut entries = action_entries();
        entries.extend(self.tools.iter().cloned());
        let ctx = env.ctx;
        egui::Window::new(env.t("Command Browser"))
            .id(egui::Id::new("editor_windows_palette"))
            .open(&mut open)
            .default_size([760.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let caption = ui.label(env.t("Search"));
                    let field = ui.add(egui::TextEdit::singleline(&mut self.query).desired_width(320.0));
                    field.clone().labelled_by(caption.id);
                    if self.focus {
                        field.request_focus();
                        self.focus = false;
                    }
                    if ui.button(env.t("Refresh")).clicked() {
                        action = Some(PaletteAction::Refresh);
                    }
                    if self.need_list || self.running {
                        ui.spinner();
                    }
                    if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                        field.request_focus();
                        activate = true;
                    }
                });
                if let Some(error) = &self.list_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("Tools unavailable: {error}"));
                }
                let found = filter_entries(&entries, &self.query);
                let position = found.iter().position(|entry| Some(&entry.id) == self.chosen.as_ref());
                let step = ui.input(|input| {
                    i32::from(input.key_pressed(egui::Key::ArrowDown)) - i32::from(input.key_pressed(egui::Key::ArrowUp))
                });
                if step != 0 && !found.is_empty() {
                    let next = position.map_or(0, |at| (at as i32 + step).clamp(0, found.len() as i32 - 1) as usize);
                    let entry = found[next].clone();
                    self.choose(&entry);
                } else if position.is_none() && !found.is_empty() {
                    let entry = found[0].clone();
                    self.choose(&entry);
                }
                ui.separator();
                ui.columns(2, |columns| {
                    egui::ScrollArea::vertical().id_salt("palette_list").show(&mut columns[0], |ui| {
                        if found.is_empty() {
                            ui.label(env.t("No commands match the filter."));
                        }
                        for entry in &found {
                            let text = if entry.category.is_empty() {
                                entry.label.clone()
                            } else {
                                format!("[{}] {}", entry.category, entry.label)
                            };
                            let selected = Some(&entry.id) == self.chosen.as_ref();
                            let response = ui.selectable_label(selected, text);
                            if response.clicked() {
                                self.choose(entry);
                            }
                            if response.double_clicked() {
                                activate = true;
                            }
                        }
                    });
                    let selected = entries.iter().find(|entry| Some(&entry.id) == self.chosen.as_ref()).cloned();
                    egui::ScrollArea::vertical().id_salt("palette_detail").show(&mut columns[1], |ui| {
                        if let Some(entry) = selected {
                            if let Some(chosen) = self.detail(ui, &entry, env) {
                                action = Some(chosen);
                            }
                        }
                    });
                });
            });
        if activate && action.is_none() {
            action = self.activate(&entries, env);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        (open, action)
    }

    /// Enter or a double click: run an action, call a tool that needs no arguments, or leave a tool
    /// that does need them selected for the argument box.
    fn activate(&mut self, entries: &[Entry], env: &Env) -> Option<PaletteAction> {
        let found = filter_entries(entries, &self.query);
        let entry = found.iter().find(|entry| Some(&entry.id) == self.chosen.as_ref())?;
        match &entry.kind {
            EntryKind::Action => env.command_enabled(&entry.id).then(|| PaletteAction::Run(entry.id.clone())),
            EntryKind::Tool(schema)
                if required_fields(schema).is_empty()
                    && schema["properties"].as_object().is_none_or(|properties| properties.is_empty()) =>
            {
                self.tool_call(entry, schema)
            }
            EntryKind::Tool(_) => None,
        }
    }

    fn tool_call(&mut self, entry: &Entry, schema: &Value) -> Option<PaletteAction> {
        match validate_arguments(&self.args, schema) {
            Ok(arguments) => {
                self.args_error = None;
                self.running = true;
                self.outcome = None;
                Some(PaletteAction::Call { name: entry.id.clone(), arguments })
            }
            Err(error) => {
                self.args_error = Some(error);
                None
            }
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui, entry: &Entry, env: &Env) -> Option<PaletteAction> {
        let mut action = None;
        ui.heading(entry.label.as_str());
        ui.weak(entry.id.as_str());
        if !entry.description.is_empty() {
            ui.label(entry.description.as_str());
        }
        match &entry.kind {
            EntryKind::Action => {
                let shortcut = env.shortcut_text(&entry.id);
                if !shortcut.is_empty() {
                    ui.label(format!("{}: {shortcut}", env.t("Shortcut")));
                }
                let enabled = env.command_enabled(&entry.id);
                if ui.add_enabled(enabled, egui::Button::new(env.t("Run Command"))).clicked() {
                    action = Some(PaletteAction::Run(entry.id.clone()));
                }
            }
            EntryKind::Tool(schema) => {
                let required = required_fields(schema);
                let caption = ui.label(format!("Arguments (JSON object); required: {}", required.join(", ")));
                ui.add(egui::TextEdit::multiline(&mut self.args).code_editor().desired_rows(6).desired_width(f32::INFINITY))
                    .labelled_by(caption.id);
                if let Some(error) = &self.args_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error.as_str());
                }
                if ui.add_enabled(!self.running, egui::Button::new(env.t("Run Command"))).clicked() {
                    action = self.tool_call(entry, schema);
                }
            }
        }
        if let Some(outcome) = &self.outcome {
            ui.separator();
            let color = if outcome.ok { egui::Color32::LIGHT_GREEN } else { egui::Color32::LIGHT_RED };
            ui.colored_label(color, if outcome.ok { "Succeeded" } else { "Failed" });
            let mut shown = outcome.text.clone();
            let caption = ui.label("Result");
            ui.add(egui::TextEdit::multiline(&mut shown).code_editor().desired_rows(6).desired_width(f32::INFINITY))
                .labelled_by(caption.id);
        }
        action
    }
}

// ------------------------------------------------------------------------------------------ texture viewer

struct Loaded {
    node: u64,
    revision: u64,
    slots: PartSlots,
    handles: [Option<egui::TextureHandle>; 3],
}

struct TextureViewer {
    slot: usize,
    zoom: f32,
    loaded: Option<Loaded>,
    error: Option<String>,
    loading: bool,
}

impl TextureViewer {
    fn new() -> Self {
        Self { slot: 0, zoom: 1.0, loaded: None, error: None, loading: false }
    }

    /// Stores the decoded textures that the `texture_get` calls made for `node` answered.
    fn deliver(&mut self, ctx: &egui::Context, node: u64, revision: u64, outcome: Result<PartSlots, String>) {
        self.loading = false;
        self.error = None;
        match outcome {
            Ok(slots) => {
                let limit = ctx.input(|input| input.max_texture_side);
                let mut handles = [None, None, None];
                for (position, image) in slots.iter().enumerate() {
                    let Some(image) = image else { continue };
                    if image.width > limit || image.height > limit {
                        self.error = Some(format!("Slot {} is {}x{}, larger than the {limit} px the GPU takes", image.slot, image.width, image.height));
                        continue;
                    }
                    let size = [image.width, image.height];
                    let color = if image.premultiplied {
                        egui::ColorImage::from_rgba_premultiplied(size, &image.rgba)
                    } else {
                        egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba)
                    };
                    handles[position] = Some(ctx.load_texture(
                        format!("part-{node}-slot-{position}"),
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                self.loaded = Some(Loaded { node, revision, slots, handles });
            }
            Err(error) => {
                self.loaded = None;
                self.error = Some(error);
            }
        }
    }

    /// Returns (still open, load requested for this node).
    fn show(&mut self, env: &Env) -> (bool, Option<u64>) {
        let mut open = true;
        let mut load = None;
        let part = env.selected.and_then(|id| find_node(&env.snapshot["model"]["nodes"], id).map(|node| (id, node)));
        let part = part.filter(|(_, node)| node["type"].as_str() == Some("Part"));
        let revision = env.snapshot["revision"].as_u64().unwrap_or(0);
        let ctx = env.ctx;
        egui::Window::new(env.t("Texture Viewer"))
            .id(egui::Id::new("editor_windows_textures"))
            .open(&mut open)
            .default_size([512.0, 512.0])
            .show(ctx, |ui| {
                let Some((id, node)) = part else {
                    ui.label("Select a Part in the node tree to see its textures.");
                    return;
                };
                ui.label(format!("{} (node {id})", node["name"].as_str().unwrap_or("")));
                let model = model_slots(node);
                let listed: Vec<String> = SLOT_NAMES
                    .iter()
                    .zip(model)
                    .map(|(name, slot)| format!("{name}: {}", slot.map_or("-".to_owned(), |slot| slot.to_string())))
                    .collect();
                ui.label(format!("Texture slots in the model: {}", listed.join(", ")));
                if ui.add_enabled(!self.loading, egui::Button::new("Load textures")).clicked() {
                    self.loading = true;
                    load = Some(id);
                }
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error.as_str());
                }
                let Some(loaded) = self.loaded.as_ref().filter(|loaded| loaded.node == id) else {
                    ui.weak("Textures are read on request from the session.");
                    return;
                };
                if loaded.revision != revision {
                    ui.colored_label(egui::Color32::YELLOW, "The document changed since these were loaded.");
                }
                let shown = SLOT_NAMES[self.slot.min(2)];
                egui::ComboBox::from_label("Slot").selected_text(shown).show_ui(ui, |ui| {
                    for (position, name) in SLOT_NAMES.iter().enumerate() {
                        let detail = loaded.slots[position]
                            .as_ref()
                            .map_or("empty".to_owned(), |image| format!("{}x{}", image.width, image.height));
                        ui.selectable_value(&mut self.slot, position, format!("{name} ({detail})"));
                    }
                });
                ui.horizontal(|ui| {
                    let caption = ui.label("Zoom");
                    ui.add(egui::Slider::new(&mut self.zoom, 0.1..=10.0).logarithmic(true).show_value(false))
                        .labelled_by(caption.id);
                    ui.label(format!("{:.2}x", self.zoom));
                });
                self.zoom = zoom_clamped(self.zoom);
                match (&loaded.slots[self.slot.min(2)], &loaded.handles[self.slot.min(2)]) {
                    (Some(image), Some(handle)) => {
                        ui.label(format!("{} x {} px, texture slot {}", image.width, image.height, image.slot));
                        let size = egui::vec2(image.width as f32, image.height as f32) * self.zoom;
                        egui::ScrollArea::both().show(ui, |ui| {
                            ui.add(egui::Image::new((handle.id(), size)));
                        });
                    }
                    _ => {
                        ui.label("This slot has no texture.");
                    }
                }
            });
        (open, load)
    }
}

// ------------------------------------------------------------------------------------------ flip pairs window

struct FlipWindow {
    pairs: Vec<FlipPair>,
    original: Vec<FlipPair>,
    nodes: Vec<(u32, String, String)>,
    filter: String,
    picked: Option<u32>,
    left: String,
    right: String,
    message: String,
}

/// Returned by the flip window: the pairs to keep, or `None` to leave things as they were.
enum FlipOutcome {
    Open,
    Cancelled,
    Saved(Vec<FlipPair>),
}

impl FlipWindow {
    fn new(pairs: Vec<FlipPair>, nodes: Vec<(u32, String, String)>, message: String) -> Self {
        let pairs = normalize(pairs);
        Self { original: pairs.clone(), pairs, nodes, filter: String::new(), picked: None, left: String::new(), right: String::new(), message }
    }

    fn name(&self, uuid: Option<u32>) -> String {
        uuid.map_or("<< Not assigned >>".to_owned(), |uuid| {
            self.nodes.iter().find(|node| node.0 == uuid).map_or(format!("(missing {uuid})"), |node| node.1.clone())
        })
    }

    fn show(&mut self, env: &Env) -> FlipOutcome {
        let mut outcome = FlipOutcome::Open;
        let mut open = true;
        let ctx = env.ctx;
        egui::Window::new(env.t("Flip Pairing"))
            .id(egui::Id::new("editor_windows_flip"))
            .open(&mut open)
            .default_size([760.0, 520.0])
            .show(ctx, |ui| {
                if !self.message.is_empty() {
                    ui.weak(self.message.as_str());
                }
                ui.columns(2, |columns| {
                    self.node_column(&mut columns[0], env);
                    self.pair_column(&mut columns[1], env);
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(env.t("Cancel")).clicked() {
                        outcome = FlipOutcome::Cancelled;
                    }
                    if ui.button(env.t("Save")).clicked() {
                        outcome = FlipOutcome::Saved(normalize(self.pairs.clone()));
                    }
                });
            });
        if !open {
            outcome = FlipOutcome::Cancelled;
        }
        outcome
    }

    fn node_column(&mut self, ui: &mut egui::Ui, env: &Env) {
        ui.strong("Nodes without a pair");
        let caption = ui.label("Filter nodes");
        ui.text_edit_singleline(&mut self.filter).labelled_by(caption.id);
        let filter = self.filter.to_lowercase();
        let taken: HashSet<u32> = self.pairs.iter().flat_map(|pair| pair.parts.iter().flatten().copied()).collect();
        egui::ScrollArea::vertical().id_salt("flip_nodes").max_height(300.0).show(ui, |ui| {
            for (uuid, name, kind) in &self.nodes {
                if taken.contains(uuid) || !name.to_lowercase().contains(&filter) {
                    continue;
                }
                if ui.selectable_label(self.picked == Some(*uuid), format!("{name} ({kind})")).clicked() {
                    self.picked = Some(*uuid);
                }
            }
        });
        let addable = self.picked.is_some_and(|node| !taken.contains(&node));
        if ui.add_enabled(addable, egui::Button::new("Add as new row")).clicked() {
            self.pairs.push(FlipPair { parts: [self.picked.take(), None] });
        }
        let _ = env;
    }

    fn pair_column(&mut self, ui: &mut egui::Ui, env: &Env) {
        let first = labelled_text(ui, &env.t("Part 1"), &mut self.left);
        let second = labelled_text(ui, &env.t("Part 2"), &mut self.right);
        let _ = (first, second);
        if ui.button(env.t("Pair")).clicked() {
            let names: Vec<(u32, String)> = self.nodes.iter().map(|node| (node.0, node.1.clone())).collect();
            let current = normalize(std::mem::take(&mut self.pairs));
            match auto_add(&current, &names, &self.left, &self.right) {
                Ok((pairs, added)) => {
                    self.pairs = pairs;
                    self.message = format!("{added} pairs added");
                }
                Err(error) => {
                    self.pairs = current;
                    self.message = error;
                }
            }
        }
        let mut remove = None;
        let mut put = None;
        egui::ScrollArea::vertical().id_salt("flip_pairs").max_height(300.0).show(ui, |ui| {
            egui::Grid::new("flip_grid").striped(true).show(ui, |ui| {
                for (row, pair) in self.pairs.iter().enumerate() {
                    ui.label(self.name(pair.parts[0]));
                    ui.label(self.name(pair.parts[1]));
                    let has_pick = self.picked.is_some();
                    ui.horizontal(|ui| {
                        let number = row + 1;
                        if named_button(ui, "1", &format!("Use selected node as Part 1 of row {number}"), has_pick) {
                            put = Some((row, 0));
                        }
                        if named_button(ui, "2", &format!("Use selected node as Part 2 of row {number}"), has_pick) {
                            put = Some((row, 1));
                        }
                        if named_button(ui, "x", &format!("Remove row {number}"), true) {
                            remove = Some(row);
                        }
                    });
                    ui.end_row();
                }
            });
        });
        if let (Some((row, slot)), Some(node)) = (put, self.picked) {
            assign(&mut self.pairs, node, row, slot);
            self.picked = None;
        }
        if let Some(row) = remove {
            self.pairs.remove(row);
        }
    }
}

// ------------------------------------------------------------------------------------------ the windows

/// A viewer load in flight: one `texture_get` per slot the Part lists.
struct TextureLoad {
    node: u64,
    revision: u64,
    /// Positions whose answer has not come yet.
    waiting: [bool; 3],
    slots: PartSlots,
    error: Option<String>,
}

#[derive(Default)]
pub struct Windows {
    ctx: Option<egui::Context>,
    started: bool,
    dark: Option<bool>,
    zoom: Option<f32>,
    /// The font choice last handed to egui.
    dyslexic: Option<bool>,
    welcome: Option<Welcome>,
    settings: Option<SettingsWindow>,
    about: bool,
    palette: Option<Palette>,
    texture: Option<TextureViewer>,
    /// The texture answers a load of the viewer still waits for.
    texture_request: Option<TextureLoad>,
    flip: Option<FlipWindow>,
    /// The nodes of the document while its flip pairs are being read.
    flip_nodes: Option<Vec<(u32, String, String)>>,
    /// Answers that need the editor's attention, handed over with the next frame's requests.
    notices: Vec<String>,
    prompt: Option<PathPrompt>,
    link: Option<AddressDialog>,
}

impl Windows {
    pub fn has_document_draft(&self) -> bool {
        self.flip.as_ref().is_some_and(|flip| normalize(flip.pairs.clone()) != flip.original)
    }

    pub fn quit_pending(&self, store: &Settings) -> bool {
        self.prompt.is_some()
            || self.has_document_draft()
            || self.settings.as_ref().is_some_and(|settings| settings.capture.is_some()
                || settings.mcp_draft.as_ref().is_some_and(|draft| *draft != McpSettings::load(store)))
    }

    pub fn forget_document(&mut self) {
        self.flip = None;
        self.flip_nodes = None;
    }

    pub fn about_open(&self) -> bool {
        self.about
    }

    pub fn set_about_open(&mut self, open: bool) {
        self.about = open;
    }

    pub fn command_browser_open(&self) -> bool {
        self.palette.is_some()
    }

    pub fn set_command_browser_open(&mut self, open: bool) {
        if open {
            self.palette.get_or_insert_with(Palette::new);
        } else {
            self.palette = None;
        }
    }

    pub fn texture_viewer_open(&self) -> bool {
        self.texture.is_some()
    }

    pub fn set_texture_viewer_open(&mut self, open: bool) {
        if open {
            self.texture.get_or_insert_with(TextureViewer::new);
        } else {
            self.texture = None;
        }
    }

    pub fn welcome_open(&self) -> bool {
        self.welcome.is_some()
    }

    /// Help > Welcome and `editor_welcome_set`. Opening a closed window starts on Quick Setup until the quick
    /// setup is done; an open window keeps its page. Closing writes nothing, unlike the window's own close button.
    pub fn set_welcome_open(&mut self, open: bool, store: &Settings) {
        if !open {
            self.welcome = None;
        } else if self.welcome.is_none() {
            let done = store.get_or(KEY_QUICK_SETUP, false).unwrap_or(false);
            self.welcome = Some(Welcome { step: u8::from(done) });
        }
    }

    pub fn address_dialog_state(&self) -> Value {
        self.link.as_ref().map_or(Value::Null, |dialog| json!({
            "target":dialog.target.id(), "title":dialog.title, "address":dialog.address,
        }))
    }

    pub fn apply_address_dialog(
        &mut self, operation: AddressDialogOperation, store_path: Option<&Path>,
        ctx: &egui::Context, translate: &dyn Fn(&str) -> String,
    ) -> Result<(), String> {
        match operation {
            AddressDialogOperation::Open(target) => {
                let address = match target {
                    AddressDialogTarget::Configuration => store_path.and_then(Path::parent)
                        .ok_or("the configuration folder is unavailable")?.display().to_string(),
                    AddressDialogTarget::OnlineDocumentation | AddressDialogTarget::Soul2dLiveDocumentation => DOCS_URI.to_owned(),
                    AddressDialogTarget::BugReport => BUG_REPORT_URI.to_owned(),
                    AddressDialogTarget::FeatureRequest => FEATURE_REQ_URI.to_owned(),
                };
                self.link = Some(AddressDialog { target, title: translate(address_label(target)), address });
            }
            AddressDialogOperation::Copy => {
                let dialog = self.link.as_ref().ok_or("no address dialog is open")?;
                ctx.copy_text(dialog.address.clone());
            }
            AddressDialogOperation::Close => self.link = None,
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Draws the menu bar and every open window; call once at the top of the editor's `update`.
    pub fn show(&mut self, ctx: &egui::Context, host: Host<'_>) -> Vec<Request> {
        let mut env = Env {
            all_keys: host.all_keys,
            ctx,
            snapshot: host.snapshot,
            selected: host.selected_node,
            project: host.project_path,
            store: host.store,
            store_path: host.store_path,
            shortcuts: host.shortcuts,
            locales: host.locales,
            lang: host.lang,
            tr: host.tr,
            mcp: host.mcp,
            out: Vec::new(),
        };
        self.ctx = Some(ctx.clone());
        self.apply_look(&env);
        self.start(&mut env);
        if let Some(settings) = self.settings.as_mut() {
            settings.capture_step(&mut env);
        }
        self.own_shortcuts(&mut env);
        self.menu_bar(&mut env);
        self.draw_windows(&mut env);
        env.out.extend(self.notices.drain(..).map(Request::Notice));
        env.out
    }

    /// Hands back the endpoint's answer to an `Request::Mcp`.
    pub fn deliver(&mut self, target: McpTarget, response: Option<Value>) {
        match target {
            McpTarget::ToolList => {
                if let Some(palette) = self.palette.as_mut() {
                    match response.as_ref().map(parse_tools) {
                        Some(Ok(tools)) => {
                            palette.tools = tools;
                            palette.list_error = None;
                        }
                        Some(Err(error)) => palette.list_error = Some(error),
                        None => palette.list_error = Some("the endpoint sent no response".to_owned()),
                    }
                    palette.need_list = false;
                }
            }
            McpTarget::ToolCall => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.outcome = Some(call_outcome(response.as_ref()));
                    palette.running = false;
                }
            }
            McpTarget::Texture(position) => self.texture_answer(usize::from(position).min(2), response),
            McpTarget::FlipRead => {
                let Some(nodes) = self.flip_nodes.take() else { return };
                let known: HashSet<u32> = nodes.iter().map(|node| node.0).collect();
                let (pairs, message) = match structured(response.as_ref()) {
                    Ok(answer) => flip_from_answer(&answer, &known),
                    Err(error) => (Vec::new(), format!("The document's flip pairs could not be read: {error}")),
                };
                self.flip = Some(FlipWindow::new(pairs, nodes, message));
            }
            McpTarget::FlipWrite => {
                if let Err(error) = structured(response.as_ref()) {
                    self.notices.push(format!("Flip pairs were not saved: {error}"));
                }
            }
            McpTarget::Viewport => {
                if let Err(error) = structured(response.as_ref()) {
                    self.notices.push(format!("Viewport: {error}"));
                }
            }
            McpTarget::ParameterPanel => {
                if let Err(error) = structured(response.as_ref()) {
                    self.notices.push(format!("Parameters: {error}"));
                }
            }
            McpTarget::AddressDialog => {
                if let Err(error) = structured(response.as_ref()) {
                    self.notices.push(format!("Address: {error}"));
                }
            }
            McpTarget::Layout => {
                if let Err(error) = structured(response.as_ref()) {
                    self.notices.push(format!("Layout: {error}"));
                }
            }
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// One slot of a viewer load arrived; the viewer is filled when the last one has.
    fn texture_answer(&mut self, position: usize, response: Option<Value>) {
        let Some(load) = self.texture_request.as_mut() else { return };
        load.waiting[position] = false;
        match structured(response.as_ref()).and_then(|answer| slot_image(&answer)) {
            Ok(image) => load.slots[position] = Some(image),
            Err(error) => {
                load.error.get_or_insert(error);
            }
        }
        if load.waiting.contains(&true) {
            return;
        }
        let Some(load) = self.texture_request.take() else { return };
        if let (Some(viewer), Some(ctx)) = (self.texture.as_mut(), self.ctx.as_ref()) {
            let outcome = match load.error {
                Some(error) => Err(error),
                None => Ok(load.slots),
            };
            viewer.deliver(ctx, load.node, load.revision, outcome);
        }
    }

    fn apply_look(&mut self, env: &Env) {
        let dark = dark_mode(env.store);
        if dark != self.dark {
            if let Some(dark) = dark {
                env.ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
            }
            self.dark = dark;
        }
        let dyslexic = use_open_dyslexic(env.store);
        if self.dyslexic != Some(dyslexic) {
            // The first frame installs the fonts `EditorApp::new` set up, so only a change is applied.
            if self.dyslexic.is_some() {
                env.ctx.set_fonts(crate::editor_ui::fonts(dyslexic));
            }
            self.dyslexic = Some(dyslexic);
        }
        let zoom = effective_zoom(env.store, Platform::current());
        if self.zoom != Some(zoom) {
            if self.zoom.is_some() || (zoom - 1.0).abs() > f32::EPSILON {
                env.ctx.set_zoom_factor(zoom);
            }
            self.zoom = Some(zoom);
        }
    }

    /// First frame: D sets `firstrun_complete` and opens the welcome window until the quick setup is done.
    fn start(&mut self, env: &mut Env) {
        if self.started {
            return;
        }
        self.started = true;
        let quick_done = env.store.get_or(KEY_QUICK_SETUP, false).unwrap_or(false);
        let first_run = !env.store.contains(KEY_FIRST_RUN);
        if first_run && env.store.set(KEY_FIRST_RUN, true).is_ok() {
            env.save_store();
        }
        if first_run || !quick_done {
            self.welcome = Some(Welcome { step: u8::from(quick_done) });
        }
    }

    fn own_shortcuts(&mut self, env: &mut Env) {
        let capturing = self.settings.as_ref().is_some_and(|settings| settings.capture.is_some());
        if capturing || env.ctx.wants_keyboard_input() {
            return;
        }
        let events = env.ctx.input(|input| input.events.clone());
        let swap = mac_swap(env.store);
        let mut fired = Vec::new();
        for event in events {
            let Some(press) = key_press(&event, swap) else { continue };
            for id in OWN_SHORTCUTS {
                let Some(binding) = env.shortcuts.binding_for(id) else { continue };
                let chord = Chord { super_key: false, ..binding.chord.clone() };
                let hit = binding.chord.super_key == press.super_key
                    && chord_matches(&chord, press.key, press.command, press.alt, press.shift);
                if hit && (!press.repeat || binding.repeat) {
                    fired.push(*id);
                }
            }
        }
        for id in fired {
            self.run_command(id, env);
        }
    }

    fn prompt_for(&mut self, kind: PromptKind, env: &Env) {
        let text = match (kind, env.project) {
            (PromptKind::SaveAs, Some(path)) => path.display().to_string(),
            _ => String::new(),
        };
        self.prompt = Some(PathPrompt::new(kind, text));
    }

    /// One command by D id, whether it came from a menu, a shortcut or the palette.
    fn run_command(&mut self, id: &str, env: &mut Env) {
        if !env.command_enabled(id) {
            return;
        }
        match id {
            "FileCommand.NewFile"
            | "EditCommand.Undo"
            | "EditCommand.Redo"
            | "AnimationCommand.KeyAll"
            | "AnimationCommand.UnkeyAll" => env.out.push(Request::Command(id.to_owned())),
            "FileCommand.ShowOpenFileDialog" => self.prompt_for(PromptKind::Open, env),
            "FileCommand.ShowSaveFileDialog" if env.project.is_some() => env.out.push(Request::Command(id.to_owned())),
            "FileCommand.ShowSaveFileDialog" | "FileCommand.ShowSaveFileAsDialog" => {
                self.prompt_for(PromptKind::SaveAs, env)
            }
            "EditCommand.ShowSettingsWindow" => {
                self.settings.get_or_insert_with(SettingsWindow::default);
            }
            id if opens_command_browser(id) => env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_command_browser_set", json!({"open":true})))),
            "NodeCommand.CutNode" => env.out.push(Request::NodeCut),
            "NodeCommand.CopyNode" => env.out.push(Request::NodeCopy),
            "NodeCommand.PasteNode" => env.out.push(Request::NodePaste),
            _ => {}
        }
    }
}

/// One click in the menu bar, carried out after the bar is drawn.
enum MenuAction {
    Command(&'static str),
    Open(String),
    OpenAutosave(autosave::AutosaveRecord),
    Address(AddressDialogTarget),
    ImportPsd,
    Prompt(PromptKind),
    /// File > Import > Depth Map.
    PsdDepth,
    /// A boolean setting toggled from a menu check box.
    Stats(bool),
    Panel(&'static str, bool),
    /// View > Workspace: an `editor_workspace_*` tool and the workspace name.
    Workspace(&'static str, String),
    ResetLayout,
    Tool(&'static str),
    About,
    Welcome,
    Textures,
    FlipPairs,
    Quit,
}

fn menu_item(ui: &mut egui::Ui, env: &Env, label: String, id: &str) -> bool {
    let mut button = egui::Button::new(label);
    let shortcut = env.shortcut_text(id);
    if !shortcut.is_empty() {
        button = button.shortcut_text(shortcut);
    }
    let clicked = ui.add_enabled(env.command_enabled(id), button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn plain_item(ui: &mut egui::Ui, label: String) -> bool {
    let clicked = ui.button(label).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

impl Windows {
    fn menu_bar(&mut self, env: &mut Env) {
        let mut action = None;
        let ctx = env.ctx;
        egui::TopBottomPanel::top("editor_windows_menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(env.t("File"), |ui| {
                    for (id, label) in [("FileCommand.NewFile", "New"), ("FileCommand.ShowOpenFileDialog", "Open")] {
                        if menu_item(ui, env, env.t(label), id) {
                            action = Some(MenuAction::Command(id));
                        }
                    }
                    let recent = recent_projects(env.store);
                    let autosaves = autosave::prev_autosaves(env.store);
                    ui.add_enabled_ui(!recent.is_empty(), |ui| {
                        ui.menu_button(env.t("Recent"), |ui| {
                            ui.add_enabled_ui(!autosaves.is_empty(), |ui| {
                                ui.menu_button(env.t("Autosaves"), |ui| {
                                    for record in &autosaves {
                                        if ui.button(base_name(&record.autosave)).on_hover_text(record.autosave.as_str()).clicked() {
                                            action = Some(MenuAction::OpenAutosave(record.clone()));
                                            ui.close();
                                        }
                                    }
                                });
                            });
                            for path in &recent {
                                if ui.button(base_name(path)).on_hover_text(path.as_str()).clicked() {
                                    action = Some(MenuAction::Open(path.clone()));
                                    ui.close();
                                }
                            }
                        });
                    });
                    for (id, label) in [("FileCommand.ShowSaveFileDialog", "Save"), ("FileCommand.ShowSaveFileAsDialog", "Save As...")] {
                        if menu_item(ui, env, env.t(label), id) {
                            action = Some(MenuAction::Command(id));
                        }
                    }
                    ui.menu_button(env.t("Import"), |ui| {
                        if plain_item(ui, "Import PSD...".to_owned()) {
                            action = Some(MenuAction::ImportPsd);
                        }
                        if plain_item(ui, env.t("Import KRA...")) {
                            action = Some(MenuAction::Prompt(PromptKind::ImportKra));
                        }
                        for (kind, label) in [
                            (PromptKind::ImportInp, "Import soul2d_live puppet"),
                            (PromptKind::ImportFolder, "Import Image Folder"),
                        ] {
                            if plain_item(ui, format!("{}...", env.t(label))) {
                                action = Some(MenuAction::Prompt(kind));
                            }
                        }
                        // The depth import operations refuse native Live2D documents.
                        let depth = egui::Button::new(format!("{}...", env.t("Import Depth Map")));
                        if ui.add_enabled(env.is_inx(), depth).clicked() {
                            ui.close();
                            action = Some(MenuAction::PsdDepth);
                        }
                    });
                    ui.add_enabled_ui(env.is_inx(), |ui| {
                        ui.menu_button(env.t("Export"), |ui| {
                            // "Export Image..." and "Export Video..." are shipped catalog ids; the others are new, so
                            // they get the dots here.
                            for (kind, label) in [
                                (ExportKind::Image, env.t("Export Image...")),
                                (ExportKind::Frames, format!("{}...", env.t("Export Frames"))),
                                (ExportKind::Inp, format!("{}...", env.t("Export soul2d_live puppet"))),
                                (ExportKind::Video, env.t("Export Video...")),
                            ] {
                                if plain_item(ui, label) {
                                    action = Some(MenuAction::Prompt(PromptKind::Export(kind)));
                                }
                            }
                        });
                    });
                    ui.menu_button(env.t("Merge"), |ui| {
                        for (kind, label) in [
                            (PromptKind::MergeImages, "Merge Image Files"),
                            (PromptKind::MergeInp, "Merge soul2d project"),
                        ] {
                            if ui.add_enabled(env.is_inx(), egui::Button::new(format!("{}...", env.t(label)))).clicked() {
                                action = Some(MenuAction::Prompt(kind));
                                ui.close();
                            }
                        }
                    });
                    if plain_item(ui, env.t("Close Project")) {
                        // D closes by starting an empty project; unsaved changes are refused first.
                        action = Some(MenuAction::Command("FileCommand.NewFile"));
                    }
                    ui.separator();
                    if plain_item(ui, env.t("Quit")) {
                        action = Some(MenuAction::Quit);
                    }
                });
                ui.menu_button(env.t("Edit"), |ui| {
                    for (id, label) in [("EditCommand.Undo", "Undo"), ("EditCommand.Redo", "Redo")] {
                        if menu_item(ui, env, env.t(label), id) {
                            action = Some(MenuAction::Command(id));
                        }
                    }
                    ui.separator();
                    for (id, label) in [
                        ("NodeCommand.CutNode", "Cut"),
                        ("NodeCommand.CopyNode", "Copy"),
                        ("NodeCommand.PasteNode", "Paste"),
                    ] {
                        if menu_item(ui, env, env.t(label), id) {
                            action = Some(MenuAction::Command(id));
                        }
                    }
                    ui.separator();
                    for (id, label) in [
                        ("EditCommand.ShowSettingsWindow", "Settings"),
                        ("EditCommand.ShowCommandBrowserWindow", "Command Browser"),
                    ] {
                        if menu_item(ui, env, env.t(label), id) {
                            action = Some(MenuAction::Command(id));
                        }
                    }
                });
                ui.menu_button(env.t("View"), |ui| {
                    let folder = env.store_path.and_then(Path::parent);
                    let label = env.t("Open Configuration Folder");
                    if ui.add_enabled(folder.is_some(), egui::Button::new(label.as_str())).clicked() {
                        action = Some(MenuAction::Address(AddressDialogTarget::Configuration));
                        ui.close();
                    }
                    if ui.add_enabled(env.is_inx(), egui::Button::new(format!("{}...", env.t("Save Screenshot")))).clicked() {
                        action = Some(MenuAction::Prompt(PromptKind::Screenshot));
                        ui.close();
                    }
                    if ui.add_enabled(env.is_inx(), egui::Button::new(env.t("Save Snapshot as Guide Image"))).clicked() {
                        action = Some(MenuAction::Tool("sketch.snapshot"));
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button(env.t("Panels"), |ui| {
                        for (label, key) in PANELS {
                            let mut shown = panel_visible(env.store, key);
                            if ui.checkbox(&mut shown, env.t(label)).changed() {
                                action = Some(MenuAction::Panel(label, shown));
                            }
                        }
                    });
                    let mut stats = nerd_stats(env.store);
                    if ui.checkbox(&mut stats, env.t("Stats for Nerds")).changed() {
                        action = Some(MenuAction::Stats(stats));
                    }
                    if plain_item(ui, env.t("Reset Layout")) {
                        action = Some(MenuAction::ResetLayout);
                    }
                    ui.menu_button(env.t("Workspace"), |ui| {
                        let names = workspace_names(env.store);
                        for name in &names {
                            if plain_item(ui, name.clone()) {
                                action = Some(MenuAction::Workspace("editor_workspace_apply", name.clone()));
                            }
                        }
                        if !names.is_empty() {
                            ui.separator();
                        }
                        if plain_item(ui, format!("{}...", env.t("Save Workspace"))) {
                            action = Some(MenuAction::Prompt(PromptKind::Workspace));
                        }
                        ui.add_enabled_ui(!names.is_empty(), |ui| {
                            ui.menu_button(env.t("Delete Workspace"), |ui| {
                                for name in &names {
                                    if plain_item(ui, name.clone()) {
                                        action = Some(MenuAction::Workspace("editor_workspace_delete", name.clone()));
                                    }
                                }
                            });
                        });
                    });
                });
                ui.menu_button(env.t("Tools"), |ui| {
                    ui.add_enabled_ui(env.is_inx(), |ui| {
                        ui.label(env.t("Puppet Data"));
                        if plain_item(ui, format!("{}...", env.t("Import Inochi Session Data"))) {
                            action = Some(MenuAction::Prompt(PromptKind::ImportSession));
                        }
                        ui.separator();
                        ui.label(env.t("Puppet Texturing"));
                        for (op, label) in [
                            ("tool.premultiply_textures", "Premultiply textures"),
                            ("tool.rebleed_textures", "Bleed textures..."),
                            ("tool.generate_fake_layer_names", "Generate fake layer name info..."),
                        ] {
                            if plain_item(ui, env.t(label)) {
                                action = Some(MenuAction::Tool(op));
                            }
                        }
                        if plain_item(ui, format!("{}...", env.t("Export textures"))) {
                            action = Some(MenuAction::Prompt(PromptKind::TextureExport));
                        }
                        if plain_item(ui, format!("{}...", env.t("Replace textures"))) {
                            action = Some(MenuAction::Prompt(PromptKind::TextureReplace));
                        }
                        ui.separator();
                        ui.label(env.t("Puppet Recovery"));
                        for (op, label) in [
                            ("tool.repair", "Attempt full repair..."),
                            ("tool.regenerate_node_ids", "Regenerate Node IDs"),
                        ] {
                            if plain_item(ui, env.t(label)) {
                                action = Some(MenuAction::Tool(op));
                            }
                        }
                    });
                    ui.separator();
                    if plain_item(ui, env.t("Texture Viewer")) {
                        action = Some(MenuAction::Textures);
                    }
                    if plain_item(ui, env.t("Flip Pairing")) {
                        action = Some(MenuAction::FlipPairs);
                    }
                });
                ui.menu_button(env.t("Help"), |ui| {
                    for target in AddressDialogTarget::ALL.into_iter().skip(1) {
                        if plain_item(ui, env.t(address_label(target))) {
                            action = Some(MenuAction::Address(target));
                        }
                    }
                    ui.separator();
                    if plain_item(ui, env.t("About")) {
                        action = Some(MenuAction::About);
                    }
                    if plain_item(ui, "Welcome".to_owned()) {
                        action = Some(MenuAction::Welcome);
                    }
                });
            });
        });
        match action {
            Some(MenuAction::Command(id)) => self.run_command(id, env),
            Some(MenuAction::Open(path)) => env.out.push(Request::Open(path)),
            Some(MenuAction::OpenAutosave(record)) => env.out.push(Request::OpenAutosave(record)),
            Some(MenuAction::Address(target)) => request_address_dialog(env, AddressDialogOperation::Open(target)),
            Some(MenuAction::ImportPsd) => self.prompt_for(PromptKind::ImportPsd, env),
            Some(MenuAction::Prompt(kind)) => self.prompt_for(kind, env),
            Some(MenuAction::PsdDepth) => env.out.push(Request::OpenPsdDepth),
            Some(MenuAction::Stats(visible)) => env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_settings_set", json!({"key":KEY_NERD_STATS, "value":visible})))),
            Some(MenuAction::Panel(label, visible)) => {
                env.out.push(Request::Mcp(McpTarget::Layout,
                    call_request("editor_panel_set", json!({"panel":label.to_lowercase(), "visible":visible}))));
            }
            Some(MenuAction::Workspace(tool, name)) => {
                env.out.push(Request::Mcp(McpTarget::Layout, call_request(tool, json!({"name": name}))));
            }
            Some(MenuAction::ResetLayout) => {
                env.out.push(Request::Mcp(McpTarget::Layout, call_request("editor_layout_reset", json!({}))));
            }
            Some(MenuAction::Tool(op)) => env.out.push(Request::Core(json!({"op": op}))),
            Some(MenuAction::About) => env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_about_set", json!({"open":true})))),
            Some(MenuAction::Welcome) => env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_welcome_set", json!({"open":true})))),
            Some(MenuAction::Textures) => env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_texture_viewer_set", json!({"open":true})))),
            Some(MenuAction::FlipPairs) => self.open_flip(env),
            Some(MenuAction::Quit) => env.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {}
        }
    }

    /// Opens the window once the document's stored pairs are known (`McpTarget::FlipRead`).
    fn open_flip(&mut self, env: &mut Env) {
        if !env.is_inx() {
            self.flip = Some(FlipWindow::new(Vec::new(), Vec::new(), "Flip pairs need an open INX document.".to_owned()));
            return;
        }
        self.flip_nodes = Some(tree_nodes(&env.snapshot["model"]["nodes"]));
        env.out.push(Request::Mcp(McpTarget::FlipRead, call_request("extdata_get", json!({"key": FLIP_CONFIG_KEY}))));
    }

    fn draw_windows(&mut self, env: &mut Env) {
        if let Some(mut welcome) = self.welcome.take() {
            let (keep, choice) = welcome.show(env);
            if keep {
                self.welcome = Some(welcome);
            }
            match choice {
                Some(WelcomeChoice::New) => env.out.push(Request::Command("FileCommand.NewFile".to_owned())),
                Some(WelcomeChoice::Open) => self.prompt_for(PromptKind::Open, env),
                Some(WelcomeChoice::Import) => self.prompt_for(PromptKind::ImportPsd, env),
                Some(WelcomeChoice::Recent(path)) => env.out.push(Request::Open(path)),
                None => {}
            }
        }
        if let Some(mut settings) = self.settings.take() {
            if settings.show(env) {
                self.settings = Some(settings);
            }
        }
        if self.about && !show_about(env) {
            env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_about_set", json!({"open":false}))));
        }
        if nerd_stats(env.store) && !show_nerd_stats(env) {
            env.out.push(Request::Mcp(McpTarget::Layout,
                call_request("editor_settings_set", json!({"key":KEY_NERD_STATS, "value":false}))));
        }
        let browser_frame = self.palette.as_mut().map(|palette| {
            let (keep, action) = palette.show(env);
            let refresh = keep && ((palette.need_list && !palette.list_sent)
                || matches!(&action, Some(PaletteAction::Refresh)));
            if refresh {
                palette.need_list = true;
                palette.list_sent = true;
            }
            (keep, action, refresh)
        });
        if let Some((keep, action, refresh)) = browser_frame {
            let mut close = !keep;
            if keep {
                match action {
                    Some(PaletteAction::Run(id)) => {
                        close = !opens_command_browser(&id);
                        self.run_command(&id, env);
                    }
                    Some(PaletteAction::Call { name, arguments }) => {
                        env.out.push(Request::Mcp(McpTarget::ToolCall, call_request(&name, arguments)));
                    }
                    Some(PaletteAction::Refresh) | None => {}
                }
                if refresh {
                    env.out.push(Request::Mcp(McpTarget::ToolList, list_request()));
                }
            }
            if close {
                env.out.push(Request::Mcp(McpTarget::Layout,
                    call_request("editor_command_browser_set", json!({"open":false}))));
            }
        }
        if let Some(viewer) = self.texture.as_mut() {
            let (keep, load) = viewer.show(env);
            if !keep {
                env.out.push(Request::Mcp(McpTarget::Layout,
                    call_request("editor_texture_viewer_set", json!({"open":false}))));
            }
            if let Some(node) = load {
                let revision = env.snapshot["revision"].as_u64().unwrap_or(0);
                let listed = find_node(&env.snapshot["model"]["nodes"], node).map_or([None; 3], model_slots);
                let mut pending = TextureLoad { node, revision, waiting: [false; 3], slots: [None, None, None], error: None };
                for (position, slot) in listed.iter().enumerate() {
                    let Some(slot) = slot else { continue };
                    pending.waiting[position] = true;
                    let call = call_request("texture_get", json!({"slot": slot}));
                    env.out.push(Request::Mcp(McpTarget::Texture(position as u8), call));
                }
                if pending.waiting.contains(&true) {
                    self.texture_request = Some(pending);
                } else {
                    viewer.deliver(env.ctx, node, revision, Ok(pending.slots));
                }
            }
        }
        if let Some(mut window) = self.flip.take() {
            match window.show(env) {
                FlipOutcome::Open => self.flip = Some(window),
                FlipOutcome::Cancelled => {}
                FlipOutcome::Saved(pairs) => {
                    let call = call_request("extdata_set", flip_store_arguments(&pairs));
                    env.out.push(Request::Mcp(McpTarget::FlipWrite, call));
                }
            }
        }
        if let Some(mut prompt) = self.prompt.take() {
            let (keep, request) = prompt.show(env);
            if keep {
                self.prompt = Some(prompt);
            }
            env.out.extend(request);
        }
        if let Some(dialog) = self.link.as_ref() {
            if !show_link(env, dialog) {
                request_address_dialog(env, AddressDialogOperation::Close);
            }
        }
    }
}

#[cfg(test)]
mod logic_tests {
    #[test]
    fn remote_quit_preserves_flip_pairs_path_decisions_and_unapplied_settings() {
        use super::*;
        let store = Settings::empty();
        let mut windows = Windows::default();
        windows.flip = Some(FlipWindow::new(vec![FlipPair { parts: [Some(1), Some(2)] }], Vec::new(), String::new()));
        assert!(!windows.quit_pending(&store));
        windows.flip.as_mut().unwrap().pairs[0].parts[1] = Some(3);
        assert!(windows.quit_pending(&store));
        windows.flip.as_mut().unwrap().pairs[0].parts[1] = Some(2);
        assert!(!windows.quit_pending(&store));
        windows.prompt = Some(PathPrompt::new(PromptKind::SaveAs, "pending.inx".into()));
        assert!(windows.quit_pending(&store));
        windows.prompt = None;
        let mut draft = McpSettings::load(&store);
        draft.enabled = !draft.enabled;
        windows.settings = Some(SettingsWindow { mcp_draft: Some(draft), ..Default::default() });
        assert!(windows.quit_pending(&store));
        windows.settings.as_mut().unwrap().mcp_draft = Some(McpSettings::load(&store));
        assert!(!windows.quit_pending(&store));
        windows.forget_document();
        assert!(windows.flip.is_none() && windows.flip_nodes.is_none());
    }
    use super::*;
    use std::io::Cursor;

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("soul2d-windows-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::scratch::scratch(dir.clone());
        dir.join("settings.json")
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn recent_projects_move_to_the_front_and_stop_at_ten() {
        assert_eq!(add_recent(&strings(&["a", "b", "c"]), "b"), strings(&["b", "a", "c"]));
        assert_eq!(add_recent(&[], "x"), strings(&["x"]));
        let full: Vec<String> = (0..10).map(|n| format!("p{n}")).collect();
        let next = add_recent(&full, "new");
        assert_eq!(next.len(), 10);
        assert_eq!((next[0].as_str(), next[9].as_str()), ("new", "p8"));
        // D removes only the first earlier copy.
        assert_eq!(add_recent(&strings(&["a", "x", "a"]), "a"), strings(&["a", "x", "a"]));
    }

    #[cfg(unix)]
    #[test]
    fn two_spellings_of_one_file_share_a_recent_entry() {
        let dir = std::env::temp_dir().join(format!("soul2d-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.inx"), b"").unwrap();
        let _ = std::fs::remove_file(dir.with_extension("link"));
        std::os::unix::fs::symlink(&dir, dir.with_extension("link")).unwrap();
        let mut store = Settings::empty();
        remember(&mut store, None, dir.join("a.inx").to_str().unwrap());
        remember(&mut store, None, dir.with_extension("link").join("a.inx").to_str().unwrap());
        let recent = recent_projects(&store);
        std::fs::remove_file(dir.with_extension("link")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn opening_and_saving_keep_prev_projects_in_the_settings_file() {
        let file = temp_file("recent");
        let mut store = Settings::empty();
        note_opened("/work/a.inx", &mut store, Some(&file));
        // Only a successful `save` request counts, and it records the document's own path.
        note_saved(&json!({"op": "open", "path": "x"}), Some(Path::new("/work/x.inx")), &mut store, Some(&file));
        note_saved(&json!({"op": "save", "path": "ignored"}), Some(Path::new("/work/b.inx")), &mut store, Some(&file));
        note_saved(&json!({"op": "save"}), None, &mut store, Some(&file));
        let reread = Settings::load(&file).unwrap().settings;
        assert_eq!(reread.get::<Vec<String>>("prev_projects").unwrap(), strings(&["/work/b.inx", "/work/a.inx"]));
    }

    #[test]
    fn ui_scale_is_a_clamped_percent_and_ignored_on_macos() {
        let mut store = Settings::empty();
        assert_eq!(ui_scale_percent(&store), 100);
        set_ui_scale_percent(&mut store, 125).unwrap();
        assert_eq!(store.get::<f64>("UIScale").unwrap(), 1.25);
        assert_eq!(effective_zoom(&store, Platform::Linux), 1.25);
        assert_eq!(effective_zoom(&store, Platform::MacOs), 1.0);
        store.set("UIScale", 3.0).unwrap();
        assert_eq!(ui_scale_percent(&store), 200);
        store.set("UIScale", 0.2).unwrap();
        assert_eq!(ui_scale_percent(&store), 100);
    }

    #[test]
    fn dark_mode_is_unset_until_the_user_chooses() {
        let mut store = Settings::empty();
        assert_eq!(dark_mode(&store), None);
        store.set("DarkMode", false).unwrap();
        assert_eq!(dark_mode(&store), Some(false));
        store.set("DarkMode", true).unwrap();
        assert_eq!(dark_mode(&store), Some(true));
    }

    #[test]
    fn autosave_form_uses_d_defaults_and_clamps() {
        let mut store = Settings::empty();
        assert_eq!(autosave_form(&store), autosave::Settings::new(true, 5, 3));
        store.set("AutosaveInterval", 0i64).unwrap();
        store.set("AutosaveFileLimit", 5000i64).unwrap();
        store.set("AutosaveEnabled", false).unwrap();
        assert_eq!(autosave_form(&store), autosave::Settings::new(false, 1, 1000));
        store_autosave(&mut store, &autosave::Settings::new(true, 7, 9)).unwrap();
        assert_eq!(store.get::<i64>("AutosaveInterval").unwrap(), 7);
        assert_eq!(store.get::<i64>("AutosaveFileLimit").unwrap(), 9);
    }

    #[test]
    fn choice_falls_back_to_the_default_for_unknown_values() {
        let mut store = Settings::empty();
        assert_eq!(choice(&store, "KeepLayerFolder", "Ask", &["Ask", "Preserve"]), "Ask");
        store.set("KeepLayerFolder", "Preserve".to_owned()).unwrap();
        assert_eq!(choice(&store, "KeepLayerFolder", "Ask", &["Ask", "Preserve"]), "Preserve");
        store.set("KeepLayerFolder", "Bogus".to_owned()).unwrap();
        assert_eq!(choice(&store, "KeepLayerFolder", "Ask", &["Ask", "Preserve"]), "Ask");
    }

    #[test]
    fn headers_must_be_an_array_of_name_value_strings() {
        assert_eq!(validate_headers("[]"), Ok(()));
        assert_eq!(validate_headers(r#"[{"name":"X-A","value":"1"}]"#), Ok(()));
        assert_eq!(validate_headers(r#"{"name":"X"}"#), Err("Headers must be a JSON array".to_owned()));
        assert_eq!(
            validate_headers(r#"[{"name":"X"}]"#),
            Err("Header 1 needs string fields \"name\" and \"value\"".to_owned())
        );
        assert!(validate_headers("[").unwrap_err().starts_with("Not valid JSON"));
    }

    #[test]
    fn a_pressed_key_becomes_a_chord_only_for_keys_d_can_bind() {
        let chord = chord_from_press("S", true, false, true).unwrap();
        assert_eq!(chord.format(Platform::Linux), "Ctrl+Shift+S");
        assert_eq!(chord_from_press("F5", false, false, false).unwrap().format(Platform::Linux), "F5");
        assert_eq!(chord_from_press("Left", false, true, false).unwrap().format(Platform::Linux), "Alt+Left");
        assert_eq!(chord_from_press("Escape", false, false, false), None);
        assert_eq!(chord_from_press("-", true, false, false), None);
        assert!(chord_matches(&chord, "S", true, false, true));
        assert!(!chord_matches(&chord, "S", true, false, false));
        assert!(!chord_matches(&chord, "D", true, false, true));
    }

    #[test]
    fn english_tokens_split_names_like_d() {
        assert_eq!(english_token("OpenFile"), "open file");
        assert_eq!(english_token("ListCommand"), "list command");
        assert_eq!(english_token("Add1DParameter"), "add1dparameter");
        // D keeps the punctuation and adds a space after it, which is why tools swap underscores first.
        assert_eq!(english_token("save_as"), "save_ as");
    }

    fn matching(entries: &[Entry], query: &str) -> Vec<String> {
        filter_entries(entries, query).iter().map(|entry| entry.id.clone()).collect()
    }

    #[test]
    fn the_palette_filters_by_label_token_or_category_and_orders_by_label() {
        let entries = action_entries();
        let all = matching(&entries, "");
        assert_eq!(
            all,
            [
                "EditCommand.ShowCommandBrowserWindow",
                "NodeCommand.CopyNode",
                "NodeCommand.CutNode",
                "AnimationCommand.KeyAll",
                "FileCommand.NewFile",
                "FileCommand.ShowOpenFileDialog",
                "NodeCommand.PasteNode",
                "EditCommand.Redo",
                "FileCommand.ShowSaveFileDialog",
                "FileCommand.ShowSaveFileAsDialog",
                "EditCommand.ShowSettingsWindow",
                "PaletteCommand.ListCommand",
                "EditCommand.Undo",
                "AnimationCommand.UnkeyAll",
            ]
        );
        // The label alone ("New") does not contain "new file"; the English token of NewFile does.
        assert_eq!(matching(&entries, "new file"), ["FileCommand.NewFile"]);
        // Category match, case-insensitive: the "Nodes" group.
        assert_eq!(matching(&entries, "NODES"), ["NodeCommand.CopyNode", "NodeCommand.CutNode", "NodeCommand.PasteNode"]);
        assert_eq!(matching(&entries, "redo"), ["EditCommand.Redo"]);
        assert_eq!(matching(&entries, "zzz"), Vec::<String>::new());
        // "save" hits the label of both Save entries; shorter label first.
        assert_eq!(matching(&entries, "save"), ["FileCommand.ShowSaveFileDialog", "FileCommand.ShowSaveFileAsDialog"]);
    }

    fn live_tools() -> Vec<Entry> {
        let mut session = soul2d_core::Session::new();
        let response = session.handle_mcp(&list_request()).unwrap();
        parse_tools(&response).unwrap()
    }

    #[test]
    fn the_sessions_real_tool_list_feeds_the_palette() {
        let tools = live_tools();
        let open = tools.iter().find(|tool| tool.id == "open").expect("the open tool");
        assert_eq!(open.category, "open");
        assert!(open.description.starts_with("Open an INP/INX"), "{}", open.description);
        let EntryKind::Tool(schema) = &open.kind else { panic!("not a tool") };
        assert_eq!(required_fields(schema), ["path"]);
        assert_eq!(args_template(schema), "{\n  \"path\": \"\"\n}");
        assert_eq!(validate_arguments("{}", schema), Err("Missing required field `path`".to_owned()));
        assert_eq!(validate_arguments(r#"{"path": null}"#, schema), Err("Missing required field `path`".to_owned()));
        assert_eq!(validate_arguments(r#"{"path": ""}"#, schema), Err("Missing required field `path`".to_owned()));
        assert_eq!(validate_arguments(r#"{"path": 5}"#, schema), Err("Field `path` must be of type string".to_owned()));
        assert_eq!(validate_arguments("[1]", schema), Err("Arguments must be a JSON object".to_owned()));
        assert!(validate_arguments("{oops", schema).unwrap_err().starts_with("Arguments are not valid JSON"));
        assert_eq!(validate_arguments(r#"{"path": "a.inx"}"#, schema), Ok(json!({"path": "a.inx"})));
        // A tool without required fields accepts empty text.
        let bounds = tools.iter().find(|tool| tool.id == "bounds").expect("the bounds tool");
        let EntryKind::Tool(schema) = &bounds.kind else { panic!("not a tool") };
        assert_eq!(validate_arguments("  ", schema), Ok(json!({})));
        assert_eq!(args_template(schema), "{}");
        // Underscores act as word breaks for the English token, categories come from the first word.
        let all: Vec<Entry> = action_entries().into_iter().chain(tools).collect();
        assert!(matching(&all, "parameter create").contains(&"parameter_create".to_owned()));
        assert!(matching(&all, "node rename").contains(&"node_rename".to_owned()));
    }

    #[test]
    fn a_tool_call_answer_is_summarised_for_the_user() {
        assert_eq!(call_outcome(None), Outcome { ok: false, text: "The endpoint sent no response".to_owned() });
        let rpc_error = json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32601, "message": "method not found"}});
        assert_eq!(call_outcome(Some(&rpc_error)), Outcome { ok: false, text: "method not found".to_owned() });
        let tool_error = json!({"result": {"isError": true, "content": [{"type": "text", "text": "no such file"}]}});
        assert_eq!(call_outcome(Some(&tool_error)), Outcome { ok: false, text: "no such file".to_owned() });
        let done = json!({"result": {"isError": false, "content": [{"type": "image", "data": "x"}, {"type": "text", "text": "{\"a\":1}"}]}});
        assert_eq!(call_outcome(Some(&done)), Outcome { ok: true, text: "{\"a\":1}".to_owned() });
        let long = json!({"result": {"isError": false, "content": [{"type": "text", "text": "é".repeat(OUTCOME_LIMIT + 5)}]}});
        let text = call_outcome(Some(&long)).text;
        assert!(text.ends_with("... (clipped)"));
        assert_eq!(text.chars().count(), OUTCOME_LIMIT + "... (clipped)".chars().count());
        assert_eq!(parse_tools(&rpc_error), Err("method not found".to_owned()));
        assert!(parse_tools(&json!({"result": {}})).is_err());
    }

    fn answer_of(width: u32, height: u32, format: image::ImageFormat, pixel: [u8; 4]) -> Value {
        use base64::Engine;
        let mut data = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(width, height, image::Rgba(pixel)))
            .write_to(&mut data, format)
            .unwrap();
        let name = if format == image::ImageFormat::Tga { "tga" } else { "png" };
        json!({"slot": 3, "width": width, "height": height, "format": name,
            "base64": base64::engine::general_purpose::STANDARD.encode(data.into_inner())})
    }

    #[test]
    fn slot_image_decodes_png_and_tga_answers_and_checks_them() {
        let png = slot_image(&answer_of(2, 1, image::ImageFormat::Png, [10, 20, 30, 40])).unwrap();
        assert_eq!((png.slot, png.width, png.height, png.premultiplied), (3, 2, 1, false));
        assert_eq!(png.rgba, [10, 20, 30, 40, 10, 20, 30, 40]);
        let tga = slot_image(&answer_of(1, 2, image::ImageFormat::Tga, [1, 2, 3, 255])).unwrap();
        assert_eq!((tga.width, tga.height, tga.rgba), (1, 2, vec![1, 2, 3, 255, 1, 2, 3, 255]));

        let mut wrong_size = answer_of(2, 1, image::ImageFormat::Png, [0; 4]);
        wrong_size["width"] = json!(3);
        assert_eq!(slot_image(&wrong_size).unwrap_err(), "Texture slot 3 is 2x1, expected 3x1");
        let mut huge = answer_of(1, 1, image::ImageFormat::Png, [0; 4]);
        huge["width"] = json!(100_000);
        huge["height"] = json!(100_000);
        assert_eq!(slot_image(&huge).unwrap_err(), "Texture slot 3 is too large");
        let mut garbled = answer_of(1, 1, image::ImageFormat::Png, [0; 4]);
        garbled["base64"] = json!("***");
        assert!(slot_image(&garbled).unwrap_err().contains("not valid base64"));
        let mut mislabelled = answer_of(1, 1, image::ImageFormat::Png, [0; 4]);
        mislabelled["format"] = json!("tga");
        assert!(slot_image(&mislabelled).unwrap_err().contains("could not be decoded"));
        let mut unknown = answer_of(1, 1, image::ImageFormat::Png, [0; 4]);
        unknown["format"] = json!("bmp");
        assert_eq!(slot_image(&unknown).unwrap_err(), "Texture slot 3 has an unknown image format");
        assert!(slot_image(&json!({})).is_err());
    }

    #[test]
    fn nodes_are_found_in_the_tree_and_their_model_slots_listed() {
        let tree = json!({"uuid": 1, "name": "Root", "type": "Node", "children": [
            {"uuid": 7, "name": "Left", "type": "Part", "textures": [2, 4294967295u64, null], "children": []},
            {"uuid": 8, "name": "Right", "type": "Node", "children": [{"uuid": 9, "name": "Deep", "type": "Part"}]},
        ]});
        assert_eq!(find_node(&tree, 9).unwrap()["name"], "Deep");
        assert!(find_node(&tree, 100).is_none());
        assert_eq!(model_slots(find_node(&tree, 7).unwrap()), [Some(2), None, None]);
        assert_eq!(
            tree_nodes(&tree),
            [
                (1, "Root".to_owned(), "Node".to_owned()),
                (7, "Left".to_owned(), "Part".to_owned()),
                (8, "Right".to_owned(), "Node".to_owned()),
                (9, "Deep".to_owned(), "Part".to_owned()),
            ]
        );
        assert_eq!(zoom_clamped(0.01), 0.1);
        assert_eq!(zoom_clamped(99.0), 10.0);
        assert_eq!(zoom_clamped(f32::NAN), 1.0);
    }

    fn pair(a: Option<u32>, b: Option<u32>) -> FlipPair {
        FlipPair { parts: [a, b] }
    }

    #[test]
    fn replacing_pairs_drops_empty_ones_and_shifts_a_lone_second_part() {
        let pairs = normalize(vec![pair(None, None), pair(None, Some(5)), pair(Some(1), None), pair(Some(2), Some(3))]);
        assert_eq!(pairs, [pair(Some(5), None), pair(Some(1), None), pair(Some(2), Some(3))]);
    }

    #[test]
    fn adding_a_pair_removes_older_pairs_that_hold_either_node() {
        let start = vec![pair(Some(1), Some(2)), pair(Some(3), Some(4)), pair(Some(5), Some(6))];
        assert_eq!(add_pair(&start, 2, 5), Ok(vec![pair(Some(3), Some(4)), pair(Some(2), Some(5))]));
        assert_eq!(add_pair(&start, 7, 8).unwrap().last(), Some(&pair(Some(7), Some(8))));
        assert_eq!(add_pair(&start, 3, 3), Err("Cannot pair a node with itself".to_owned()));
        assert_eq!(remove_pair(&start, 4, 3), Ok(vec![pair(Some(1), Some(2)), pair(Some(5), Some(6))]));
        assert_eq!(remove_pair(&start, 3, 5), Err("Flip pair not found".to_owned()));
        assert_eq!(pair_for(&start, 4), Some(&pair(Some(3), Some(4))));
        assert_eq!(pair_for(&[pair(Some(9), None)], 9), None);
    }

    fn named(items: &[(u32, &str)]) -> Vec<(u32, String)> {
        items.iter().map(|(id, name)| (*id, (*name).to_owned())).collect()
    }

    #[test]
    fn auto_pairing_matches_names_by_pattern_replacement() {
        let nodes = named(&[(1, "Root"), (2, "Eye_L"), (3, "Eye_R"), (4, "Arm_L_Upper"), (5, "Arm_R_Upper"), (6, "Hat_L")]);
        let (pairs, added) = auto_add(&[], &nodes, "_L", "_R").unwrap();
        assert_eq!(added, 2);
        assert_eq!(pairs, [pair(Some(2), Some(3)), pair(Some(4), Some(5))]);
        // Nodes already paired are skipped on both sides.
        let existing = vec![pair(Some(3), Some(9))];
        let (pairs, added) = auto_add(&existing, &nodes, "_L", "_R").unwrap();
        assert_eq!(added, 1);
        assert_eq!(pairs, [pair(Some(3), Some(9)), pair(Some(4), Some(5))]);
        // Every occurrence is replaced, and a NUL ends a name like in a C string.
        let tricky = named(&[(1, "a_L_a_L"), (2, "a_R_a_R"), (3, "x_L\0junk"), (4, "x_R")]);
        let (pairs, added) = auto_add(&[], &tricky, "_L", "_R").unwrap();
        assert_eq!((added, pairs), (2, vec![pair(Some(1), Some(2)), pair(Some(3), Some(4))]));
        assert_eq!(
            auto_add(&[], &nodes, "", "_R").unwrap_err(),
            "Both leftPattern and rightPattern are required"
        );
    }

    #[test]
    fn assigning_a_node_moves_it_out_of_its_old_pair() {
        let mut pairs = vec![pair(Some(1), Some(2)), pair(Some(3), None)];
        assign(&mut pairs, 2, 1, 1);
        assert_eq!(pairs, [pair(Some(1), None), pair(Some(3), Some(2))]);
        assign(&mut pairs, 3, 0, 1);
        assert_eq!(pairs, [pair(Some(1), Some(3)), pair(None, Some(2))]);
        assign(&mut pairs, 7, 5, 0);
        assert_eq!(pairs.len(), 2, "an unknown row changes nothing");
    }

    #[test]
    fn the_flip_config_payload_has_d_shape_and_resolves_against_known_nodes() {
        let known: HashSet<u32> = [1, 2, 3, 4].into_iter().collect();
        let payload = br#"[{"uuid1":1,"uuid2":2},{"uuid1":4294967295,"uuid2":3},{"uuid1":8,"uuid2":9},{"uuid1":4,"uuid2":77}]"#;
        let pairs = parse_flip_config(payload, &known).unwrap();
        assert_eq!(pairs, [pair(Some(1), Some(2)), pair(Some(3), None), pair(Some(4), None)]);
        assert_eq!(parse_flip_config(b"", &known), Ok(Vec::new()));
        assert!(parse_flip_config(b"{}", &known).unwrap_err().contains("not an array"));
        assert!(parse_flip_config(br#"[{"uuid1":1}]"#, &known).unwrap_err().contains("uuid2"));
        assert!(parse_flip_config(b"[", &known).is_err());
        assert_eq!(dump_flip_config(&[]), None);
        assert_eq!(
            dump_flip_config(&[pair(Some(1), Some(2)), pair(Some(3), None)]).unwrap(),
            br#"[{"uuid1":1,"uuid2":2},{"uuid1":3,"uuid2":4294967295}]"#
        );
    }

    /// A saved INX with one textured Part (node 9) and a flip config entry that pairs it with node 1.
    fn face_file(png: &[u8]) -> Vec<u8> {
        let session = soul2d_core::Session::new();
        let mut model = session.snapshot()["model"].clone();
        model["nodes"]["children"] = json!([{
            "uuid": 9, "name": "Face", "type": "Part", "enabled": true, "zsort": 0.0,
            "textures": [0, u32::MAX, u32::MAX],
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                     "indices": [0, 1, 2], "origin": [0.0, 0.0]},
        }]);
        let json = serde_json::to_vec(&model).unwrap();
        let mut bytes = b"TRNSRTS\0".to_vec();
        bytes.extend((json.len() as u32).to_be_bytes());
        bytes.extend(&json);
        bytes.extend(b"TEX_SECT");
        bytes.extend(1u32.to_be_bytes());
        bytes.extend((png.len() as u32).to_be_bytes());
        bytes.push(0);
        bytes.extend(png);
        let payload = br#"[{"uuid1":1,"uuid2":9}]"#;
        bytes.extend(b"EXT_SECT");
        bytes.extend(1u32.to_be_bytes());
        bytes.extend((FLIP_CONFIG_KEY.len() as u32).to_be_bytes());
        bytes.extend(FLIP_CONFIG_KEY.as_bytes());
        bytes.extend((payload.len() as u32).to_be_bytes());
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn a_real_session_answers_the_texture_and_flip_calls_the_windows_make() {
        let mut png = Vec::new();
        image::RgbaImage::from_raw(1, 1, vec![10, 20, 30, 40])
            .unwrap()
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let path = temp_file("face").with_file_name("face.inx");
        std::fs::write(&path, face_file(&png)).unwrap();
        let mut session = soul2d_core::Session::new();
        let opened = session.handle_request(&json!({"op": "open", "path": path.to_string_lossy()}));
        assert_eq!(opened["succeeded"], true, "{opened}");

        let answer = session.handle_mcp(&call_request("texture_get", json!({"slot": 0}))).unwrap();
        let image = slot_image(&structured(Some(&answer)).unwrap()).unwrap();
        assert_eq!((image.slot, image.width, image.height, image.rgba), (0, 1, 1, vec![10, 20, 30, 40]));
        let missing = session.handle_mcp(&call_request("texture_get", json!({"slot": 1}))).unwrap();
        assert_eq!(structured(Some(&missing)).unwrap_err(), "texture slot 1 is out of range (1 textures)");

        // The stored pairs come back through `extdata_get` ...
        let known: HashSet<u32> = [1, 9].into_iter().collect();
        let read = session.handle_mcp(&call_request("extdata_get", json!({"key": FLIP_CONFIG_KEY}))).unwrap();
        let (pairs, message) = flip_from_answer(&structured(Some(&read)).unwrap(), &known);
        assert_eq!((pairs.clone(), message.as_str()), (vec![pair(Some(1), Some(9))], "Read from the document."));

        // ... and new ones are stored with `extdata_set`, removing the key when none are left.
        let wanted = vec![pair(Some(9), None)];
        let write = session.handle_mcp(&call_request("extdata_set", flip_store_arguments(&wanted))).unwrap();
        assert!(structured(Some(&write)).is_ok(), "{write}");
        let read = session.handle_mcp(&call_request("extdata_get", json!({"key": FLIP_CONFIG_KEY}))).unwrap();
        assert_eq!(flip_from_answer(&structured(Some(&read)).unwrap(), &known).0, [pair(Some(9), None)]);
        assert_eq!(session.snapshot()["modified"], true);
        let clear = session.handle_mcp(&call_request("extdata_set", flip_store_arguments(&[]))).unwrap();
        assert!(structured(Some(&clear)).is_ok());
        let read = session.handle_mcp(&call_request("extdata_get", json!({"key": FLIP_CONFIG_KEY}))).unwrap();
        let (pairs, message) = flip_from_answer(&structured(Some(&read)).unwrap(), &known);
        assert_eq!((pairs, message.as_str()), (Vec::new(), "The document has no flip pairs yet."));
    }

    #[test]
    fn stored_flip_data_that_cannot_be_read_is_reported_not_guessed() {
        let known: HashSet<u32> = [1].into_iter().collect();
        let bad = json!({"present": true, "base64": "e30="}); // "{}"
        let (pairs, message) = flip_from_answer(&bad, &known);
        assert_eq!((pairs, message.as_str()), (Vec::new(), "The stored flip pairs were not read: FlipConfig is not an array"));
        let garbled = json!({"present": true, "base64": "***"});
        assert!(flip_from_answer(&garbled, &known).1.contains("not valid base64"));
        assert_eq!(flip_store_arguments(&[]), json!({"key": FLIP_CONFIG_KEY, "base64": null}));
        assert_eq!(
            flip_store_arguments(&[pair(Some(1), Some(2))]),
            json!({"key": FLIP_CONFIG_KEY, "base64": "W3sidXVpZDEiOjEsInV1aWQyIjoyfV0="})
        );
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use egui::accesskit::Role;
    use std::collections::HashMap;

    /// Everything a frame needs, owned.
    struct Rig {
        snapshot: Value,
        store: Settings,
        store_path: Option<std::path::PathBuf>,
        shortcuts: ShortcutRegistry,
        locales: Vec<LocaleEntry>,
        selected: Option<u64>,
        /// Key events still in the input after the windows ran: what `editor_ui` would see next.
        keys_left: usize,
        mcp: McpStatus,
    }

    impl Rig {
        fn new() -> Self {
            let part = json!({"uuid": 9, "name": "Face", "type": "Part", "textures": [0, 4294967295u64, null], "children": []});
            let eye = json!({"uuid": 12, "name": "Eye_L", "type": "Node", "children": []});
            let eye2 = json!({"uuid": 13, "name": "Eye_R", "type": "Node", "children": []});
            Self {
                snapshot: json!({
                    "documentFormat": "inx", "revision": 3, "canUndo": true, "canRedo": false,
                    "clipboard": [{"uuid": 1}],
                    "model": {"nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": [part, eye, eye2]}},
                }),
                store: Settings::empty(),
                store_path: None,
                shortcuts: ShortcutRegistry::with_defaults(),
                locales: vec![LocaleEntry {
                    code: "ja".to_owned(),
                    name: "日本語".to_owned(),
                    file: "ja.mo".into(),
                    dir: ".".into(),
                }],
                selected: Some(9),
                keys_left: 0,
                mcp: McpStatus::default(),
            }
        }
    }

    /// The accessibility tree as the screen reader would see it: egui sends only changed nodes, so they
    /// are merged and only those reachable from the root count.
    type Out = Vec<(egui::accesskit::NodeId, egui::accesskit::Node)>;

    #[derive(Default)]
    struct Tree {
        all: HashMap<egui::accesskit::NodeId, egui::accesskit::Node>,
        root: Option<egui::accesskit::NodeId>,
    }

    impl Tree {
        fn absorb(&mut self, output: &egui::FullOutput) -> Out {
            let update = output.platform_output.accesskit_update.clone().expect("accesskit is enabled");
            if let Some(tree) = &update.tree {
                self.root = Some(tree.root);
            }
            for (id, node) in update.nodes {
                self.all.insert(id, node);
            }
            let mut out = Vec::new();
            let mut stack = vec![self.root.expect("a root")];
            while let Some(id) = stack.pop() {
                if let Some(node) = self.all.get(&id) {
                    stack.extend(node.children().iter().copied());
                    out.push((id, node.clone()));
                }
            }
            out
        }
    }

    thread_local! {
        static TREE: std::cell::RefCell<Tree> = std::cell::RefCell::new(Tree::default());
        /// When set, the lookup in `frame` wraps every message id it translates in brackets.
        static MARK_TR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn frame(ctx: &egui::Context, windows: &mut Windows, rig: &mut Rig, events: Vec<egui::Event>) -> (Vec<Request>, Out) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 800.0))),
            events,
            ..Default::default()
        };
        let tr = |msgid: &str| if MARK_TR.get() { format!("[{msgid}]") } else { msgid.to_owned() };
        let mut requests = Vec::new();
        let output = ctx.run(input, |ctx| {
            requests = windows.show(
                ctx,
                Host {
                    all_keys: [false; 2],
                    snapshot: &rig.snapshot,
                    selected_node: rig.selected,
                    project_path: None,
                    store: &mut rig.store,
                    store_path: rig.store_path.as_deref(),
                    shortcuts: &mut rig.shortcuts,
                    locales: &rig.locales,
                    lang: "en",
                    tr: &tr,
                    mcp: &rig.mcp,
                },
            );
            rig.keys_left = ctx.input(|input| input.events.iter().filter(|event| matches!(event, egui::Event::Key { .. })).count());
        });
        (requests, TREE.with(|tree| tree.borrow_mut().absorb(&output)))
    }

    fn nodes(output: &Out) -> Out {
        output.clone()
    }

    fn named(node: &egui::accesskit::Node) -> bool {
        node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty()
    }

    fn texts(output: &Out) -> Vec<String> {
        nodes(output).iter().flat_map(|(_, node)| [node.label(), node.value()]).flatten().map(str::to_owned).collect()
    }

    fn has(output: &Out, expected: &str) -> bool {
        texts(output).iter().any(|text| text.contains(expected))
    }

    const CONTROLS: [Role; 7] = [
        Role::Button,
        Role::CheckBox,
        Role::ComboBox,
        Role::SpinButton,
        Role::TextInput,
        Role::MultilineTextInput,
        Role::Slider,
    ];

    /// Returns how many controls were checked.
    fn assert_all_named(output: &Out, context: &str) -> usize {
        let mut count = 0;
        for (_, node) in nodes(output) {
            if CONTROLS.contains(&node.role()) {
                count += 1;
                assert!(named(&node), "{context}: unlabelled {:?} control {:?}", node.role(), node.value());
            }
        }
        count
    }

    fn center(output: &Out, role: Role, label: &str) -> egui::Pos2 {
        let (_, node) = nodes(output)
            .into_iter()
            .find(|(_, node)| {
                // A menu entry's name carries its shortcut after a space.
                node.role() == role
                    && node.label().or_else(|| node.value()).is_some_and(|name| name == label || name.starts_with(&format!("{label} ")))
            })
            .unwrap_or_else(|| {
                let have: Vec<_> = output.iter().map(|(_, n)| (n.role(), n.label().map(str::to_owned))).collect();
                panic!("no {role:?} named {label:?}; have {have:?}")
            });
        let bounds = node.bounds().expect("bounds");
        egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32)
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// Lays out twice, clicks the control, and returns the requests of the click frame and the frame after it.
    fn click(ctx: &egui::Context, windows: &mut Windows, rig: &mut Rig, role: Role, label: &str) -> (Vec<Request>, Out) {
        frame(ctx, windows, rig, Vec::new());
        let (_, laid_out) = frame(ctx, windows, rig, Vec::new());
        let at = center(&laid_out, role, label);
        let (mut requests, _) = frame(ctx, windows, rig, vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
        let (later, output) = frame(ctx, windows, rig, Vec::new());
        requests.extend(later);
        (requests, output)
    }

    fn key(key: egui::Key, command: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers { command, ctrl: command, shift, ..Default::default() },
        }
    }

    fn context() -> egui::Context {
        // One tree per test thread; a new context starts a new tree.
        TREE.with(|tree| *tree.borrow_mut() = Tree::default());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        ctx
    }

    fn loaded_palette(windows: &mut Windows) {
        let mut session = soul2d_core::Session::new();
        windows.set_command_browser_open(true);
        windows.deliver(McpTarget::ToolList, session.handle_mcp(&list_request()));
    }

    fn choose(windows: &mut Windows, id: &str) {
        let palette = windows.palette.as_mut().unwrap();
        let entry = palette.tools.iter().find(|entry| entry.id == id).cloned().expect("a tool of that name");
        palette.choose(&entry);
    }

    #[test]
    fn every_control_of_every_window_has_an_accessibility_name() {
        let ctx = context();
        let mut rig = Rig::new();
        let mut windows = Windows::new();
        // First frame: first run, so the welcome window opens on its quick setup page.
        let (requests, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(requests.is_empty(), "drawing alone asks for nothing: {requests:?}");
        assert!(rig.store.get::<bool>("firstrun_complete").unwrap());
        assert!(has(&output, "Quick Setup") && has(&output, "Color Theme") && has(&output, "Next"));
        let mut checked = assert_all_named(&output, "welcome step 0");

        windows.welcome = Some(Welcome { step: 1 });
        rig.store.set("prev_projects", strings(&["/work/rig/a.inx", "/work/b.inx"])).unwrap();
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["Create Project", "New...", "Recent Projects", "a.inx", "On the Web", "https://github.com/geekjapan/Soul2D"] {
            assert!(has(&output, expected), "welcome: missing {expected:?} in {:?}", texts(&output));
        }
        checked += assert_all_named(&output, "welcome step 1");
        windows.welcome = None;

        for pane in [Pane::Look, Pane::Files, Pane::Shortcuts, Pane::Integration] {
            windows.settings = Some(SettingsWindow { pane, ..Default::default() });
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
            checked += assert_all_named(&output, &format!("settings {pane:?}"));
            let expected: &[&str] = match pane {
                Pane::Look => &["Color Theme", "Language", "UI Scale", "Max Undo History"],
                Pane::Files => &["Enable Autosaves", "Save Interval (Minutes)", "Maximum Autosaves", "Preserve structure", "Replace LayerGroup with"],
                Pane::Shortcuts => &[
                    "Set shortcut for New",
                    "Clear shortcut for Undo",
                    "Set shortcut for Unkey All",
                    "Shift+P",
                    "Filter actions by name:",
                ],
                Pane::Integration => &["Command", "Working Dir (optional)", "Server name", "Extra headers (JSON array of name/value)"],
            };
            for text in expected {
                if *text == "UI Scale" && Platform::current() == Platform::MacOs {
                    continue;
                }
                assert!(has(&output, text), "{pane:?}: missing {text:?} in {:?}", texts(&output));
            }
        }
        windows.settings = None;

        windows.about = true;
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
        for expected in ["Soul2D (development)", version.as_str(), "BSD 2-Clause License", "Credits", "seagetch", "issues/new?template=bug-report.yml"] {
            assert!(has(&output, expected), "about: missing {expected:?}");
        }
        windows.about = false;

        loaded_palette(&mut windows);
        windows.palette.as_mut().unwrap().query = "open".to_owned();
        choose(&mut windows, "open");
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["Search", "Refresh", "open", "Arguments (JSON object); required: path", "Run Command"] {
            assert!(has(&output, expected), "palette: missing {expected:?} in {:?}", texts(&output));
        }
        checked += assert_all_named(&output, "palette");
        windows.palette.as_mut().unwrap().outcome = Some(Outcome { ok: false, text: "no such file".to_owned() });
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "Failed") && has(&output, "no such file") && has(&output, "Result"));
        checked += assert_all_named(&output, "palette result");
        windows.set_command_browser_open(false);

        windows.set_texture_viewer_open(true);
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["Face (node 9)", "Texture slots in the model: Albedo: 0, Emissive: -, Bump: -", "Load textures"] {
            assert!(has(&output, expected), "textures: missing {expected:?} in {:?}", texts(&output));
        }
        checked += assert_all_named(&output, "texture viewer");
        windows.set_texture_viewer_open(false);

        let nodes_list = tree_nodes(&rig.snapshot["model"]["nodes"]);
        windows.flip = Some(FlipWindow::new(vec![pair_of(12, 13)], nodes_list, "message".to_owned()));
        windows.flip.as_mut().unwrap().picked = Some(9);
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["Nodes without a pair", "Filter nodes", "Part 1", "Part 2", "Use selected node as Part 1 of row 1", "Remove row 1", "Eye_L", "Eye_R"] {
            assert!(has(&output, expected), "flip: missing {expected:?} in {:?}", texts(&output));
        }
        checked += assert_all_named(&output, "flip pairs");
        windows.flip = None;

        windows.prompt = Some(PathPrompt::new(PromptKind::SaveAs, "/tmp/a.inx".to_owned()));
        windows.apply_address_dialog(AddressDialogOperation::Open(AddressDialogTarget::BugReport), None, &ctx, &str::to_owned).unwrap();
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "Path") && has(&output, "Address (not opened by the editor)") && has(&output, "Copy"));
        checked += assert_all_named(&output, "prompt and link");
        assert!(checked >= 60, "only {checked} named controls checked");
    }

    fn pair_of(a: u32, b: u32) -> FlipPair {
        FlipPair { parts: [Some(a), Some(b)] }
    }

    #[test]
    fn every_menu_entry_has_a_name_and_the_expected_label() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.store.set("prev_projects", strings(&["/work/a.inx"])).unwrap();
        let mut windows = Windows::new();
        let expected: [(&str, &[&str]); 5] = [
            ("File", &["New", "Open", "Recent", "Save", "Save As...", "Import", "Export", "Merge", "Close Project", "Quit"]),
            ("Edit", &["Undo", "Redo", "Cut", "Copy", "Paste", "Settings", "Command Browser"]),
            ("View", &[
                "Open Configuration Folder", "Save Screenshot...", "Save Snapshot as Guide Image", "Panels",
                "Stats for Nerds", "Reset Layout",
            ]),
            ("Tools", &[
                "Import Inochi Session Data...", "Premultiply textures", "Bleed textures...",
                "Generate fake layer name info...", "Export textures...", "Replace textures...",
                "Attempt full repair...", "Regenerate Node IDs",
                "Texture Viewer", "Flip Pairing",
            ]),
            ("Help", &["Online Documentation", "Soul2DLive Documentation", "Report a Bug", "Request a Feature", "About", "Welcome"]),
        ];
        let mut checked = 0;
        for (menu, entries) in expected {
            // The bar is laid out first; clicking the menu button opens its list for the next frames.
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
            let at = center(&laid_out, Role::Button, menu);
            frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (_, open) = frame(&ctx, &mut windows, &mut rig, Vec::new());
            for entry in entries {
                assert!(has(&open, entry), "{menu}: missing {entry:?} in {:?}", texts(&open));
            }
            checked += assert_all_named(&open, menu);
            // Close the menu by clicking an empty spot before opening the next one.
            let away = egui::pos2(900.0, 700.0);
            frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(away), press(away, true), press(away, false)]);
            frame(&ctx, &mut windows, &mut rig, Vec::new());
        }
        assert!(checked >= 20, "only {checked} menu controls checked");
    }

    #[test]
    fn the_shell_prompts_send_the_import_merge_tool_and_screenshot_requests() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.selected = Some(7);
        for (kind, button, expected) in [
            (PromptKind::ImportKra, "Import", json!({"op": "import.kra", "path": "/p/a|b.inp",
                "keepStructure": true, "layerGroupNodeType": "Node"})),
            (PromptKind::ImportInp, "Import", json!({"op": "import.inp", "path": "/p/a|b.inp"})),
            (PromptKind::ImportFolder, "Import", json!({"op": "import.folder", "path": "/p/a|b.inp"})),
            (PromptKind::MergeInp, "Merge", json!({"op": "merge.inp", "path": "/p/a|b.inp"})),
            (PromptKind::MergeImages, "Merge", json!({"op": "merge.images", "paths": "/p/a|b.inp", "parent": 7})),
            (PromptKind::ImportSession, "Import", json!({"op": "tool.import_session_data", "path": "/p/a|b.inp"})),
            (PromptKind::Screenshot, "Save", json!({"op": "view.screenshot", "path": "/p/a|b.inp"})),
            (PromptKind::TextureExport, "Export", json!({"op": "texture.export", "directory": "/p/a|b.inp"})),
            (PromptKind::TextureReplace, "Replace", json!({"op": "texture.replace", "directory": "/p/a|b.inp"})),
        ] {
            let mut windows = Windows::new();
            windows.started = true;
            windows.prompt = Some(PathPrompt::new(kind, " /p/a|b.inp ".to_owned()));
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, button);
            assert_eq!(only(requests), Request::Core(expected), "{kind:?}");
        }
    }

    #[test]
    fn export_prompts_send_the_typed_path_with_their_kind() {
        let ctx = context();
        let mut rig = Rig::new();
        for kind in [ExportKind::Image, ExportKind::Frames, ExportKind::Inp, ExportKind::Video] {
            let mut windows = Windows::new();
            windows.started = true;
            windows.prompt = Some(PathPrompt::new(PromptKind::Export(kind), " /p/out ".to_owned()));
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Export");
            assert_eq!(only(requests), Request::Export(kind, "/p/out".to_owned()), "{kind:?}");
        }
    }

    /// Opens `menu`, then the submenu `sub`, and clicks `entry`.
    fn click_in_submenu(ctx: &egui::Context, windows: &mut Windows, rig: &mut Rig, path: [&str; 3], last: Role) -> Vec<Request> {
        click(ctx, windows, rig, Role::Button, path[0]);
        click(ctx, windows, rig, Role::Button, path[1]);
        click(ctx, windows, rig, last, path[2]).0
    }

    #[test]
    fn import_and_export_submenus_open_the_prompts_and_the_depth_window() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        for (path, kind) in [
            (["File", "Import", "Import KRA..."], Some(PromptKind::ImportKra)),
            (["File", "Export", "Export Image..."], Some(PromptKind::Export(ExportKind::Image))),
            (["File", "Export", "Export Frames..."], Some(PromptKind::Export(ExportKind::Frames))),
            (["File", "Export", "Export soul2d_live puppet..."], Some(PromptKind::Export(ExportKind::Inp))),
            (["File", "Export", "Export Video..."], Some(PromptKind::Export(ExportKind::Video))),
            (["File", "Import", "Import Depth Map..."], None),
        ] {
            let mut windows = Windows::new();
            let requests = click_in_submenu(&ctx, &mut windows, &mut rig, path, Role::Button);
            match kind {
                Some(kind) => {
                    assert!(requests.is_empty(), "{path:?}: {requests:?}");
                    assert_eq!(windows.prompt.as_ref().map(|prompt| prompt.kind), Some(kind), "{path:?}");
                }
                None => {
                    assert_eq!(only(requests), Request::OpenPsdDepth);
                    assert!(windows.prompt.is_none());
                }
            }
        }
    }

    #[test]
    fn menu_labels_look_up_the_message_ids_the_shipped_catalogs_already_have() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        MARK_TR.set(true);
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "[File]");
        let (_, imports) = click(&ctx, &mut windows, &mut rig, Role::Button, "[Import]");
        let (_, exports) = click(&ctx, &mut windows, &mut rig, Role::Button, "[Export]");
        MARK_TR.set(false);
        let catalog = include_str!("../../tl/ja.po");
        for (output, id) in [(&imports, "Import KRA..."), (&exports, "Export Image..."), (&exports, "Export Video...")] {
            assert!(has(output, &format!("[{id}]")), "the menu looks up {id:?}: {:?}", texts(output));
            assert!(catalog.contains(&format!("msgid \"{id}\"")), "{id:?} is a shipped message id");
        }
    }

    #[test]
    fn export_is_disabled_without_an_inx_document() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.snapshot["documentFormat"] = json!("live2d.cmo3");
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "File");
        let (_, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Export");
        assert!(!has(&output, "Export Image"), "a disabled menu does not open: {:?}", texts(&output));
    }

    #[test]
    fn view_workspace_saves_by_name_and_applies_or_deletes_a_saved_one() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        let path = ["View", "Workspace", "Save Workspace..."];
        let requests = click_in_submenu(&ctx, &mut windows, &mut rig, path, Role::Button);
        assert!(requests.is_empty());
        assert_eq!(windows.prompt.as_ref().map(|prompt| prompt.kind), Some(PromptKind::Workspace));
        let mut windows = Windows::new();
        windows.started = true;
        windows.prompt = Some(PathPrompt::new(PromptKind::Workspace, " Modeling ".to_owned()));
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Save");
        assert_eq!(only(requests),
            Request::Mcp(McpTarget::Layout, call_request("editor_workspace_save", json!({"name": "Modeling"}))));

        let saved: BTreeMap<String, String> = [("Animation", "{}"), ("Modeling", "{}"), (" Padded", "{}")]
            .map(|(name, layout)| (name.to_owned(), layout.to_owned())).into();
        rig.store.set("workspaces", saved).unwrap();
        let mut windows = Windows::new();
        let requests = click_in_submenu(&ctx, &mut windows, &mut rig, ["View", "Workspace", "Modeling"], Role::Button);
        assert_eq!(only(requests),
            Request::Mcp(McpTarget::Layout, call_request("editor_workspace_apply", json!({"name": "Modeling"}))));
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "View");
        let (_, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Workspace");
        assert!(has(&output, "Modeling") && !has(&output, "Padded"), "a name the tools refuse is not offered");
        click(&ctx, &mut windows, &mut rig, Role::Button, "Delete Workspace");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Animation");
        assert_eq!(only(requests),
            Request::Mcp(McpTarget::Layout, call_request("editor_workspace_delete", json!({"name": "Animation"}))));
        // A refusal shows as a notice.
        windows.deliver(McpTarget::Layout, Some(json!({"jsonrpc": "2.0", "id": 1, "result": {"isError": true,
            "content": [{"type": "text", "text": "workspace \"Animation\" was not found"}]}})));
        let notice = "Layout: workspace \"Animation\" was not found";
        assert_eq!(windows.notices.last().map(String::as_str), Some(notice));
    }

    #[test]
    fn view_panels_and_reset_layout_dispatch_tools_without_mutating_settings() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        assert!(panel_visible(&rig.store, "Outline.visible") && panel_visible(&rig.store, "Timeline.visible"));
        for (label, key, visible) in [("Outline", "Outline.visible", false),
            ("Timeline", "Timeline.visible", false), ("Timeline", "Timeline.visible", true)]
        {
            rig.store.set(key, !visible).unwrap();
            let before = (panel_visible(&rig.store, "Outline.visible"), panel_visible(&rig.store, "Timeline.visible"));
            let mut windows = Windows::new();
            let requests = click_in_submenu(&ctx, &mut windows, &mut rig, ["View", "Panels", label], Role::CheckBox);
            assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
                call_request("editor_panel_set", json!({"panel":label.to_lowercase(),"visible":visible}))));
            assert_eq!((panel_visible(&rig.store, "Outline.visible"), panel_visible(&rig.store, "Timeline.visible")), before);
        }
        rig.store.set("Outline.visible", false).unwrap();
        rig.store.set("Timeline.visible", false).unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "View");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Reset Layout");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout, call_request("editor_layout_reset", json!({}))));
        assert!(!panel_visible(&rig.store, "Outline.visible"), "the shared handler applies the reset");
        assert!(!panel_visible(&rig.store, "Timeline.visible"));
    }

    #[test]
    fn reset_layout_leaves_never_written_panel_keys_absent() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "View");
        click(&ctx, &mut windows, &mut rig, Role::Button, "Reset Layout");
        assert!(!rig.store.contains("Outline.visible") && !rig.store.contains("Timeline.visible"));
    }

    #[test]
    fn stats_for_nerds_follows_the_stored_flag_and_shows_the_document_numbers() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(!has(&output, "Document revision"), "off by default");

        // The View menu uses the same preference setter as MCP.
        click(&ctx, &mut windows, &mut rig, Role::Button, "View");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::CheckBox, "Stats for Nerds");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_settings_set", json!({"key":"NerdStats", "value":true}))));
        assert!(!nerd_stats(&rig.store), "the request has not been applied yet");
        set_setting(&mut rig.store, &[], "NerdStats", &json!(true)).unwrap();
        assert_eq!(rig.store.get::<bool>("NerdStats"), Ok(true));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["Frame time", "Frames per second", "Document revision", "Nodes", "Parts", "Animations"] {
            assert!(has(&output, expected), "missing {expected:?} in {:?}", texts(&output));
        }

        // Closing the window clears the flag, so the menu and the window agree.
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let close = center(&output, Role::Button, "Close window");
        let (requests, _) = frame(&ctx, &mut windows, &mut rig,
            vec![egui::Event::PointerMoved(close), press(close, true), press(close, false)]);
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_settings_set", json!({"key":"NerdStats", "value":false}))));
        set_setting(&mut rig.store, &[], "NerdStats", &json!(false)).unwrap();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(rig.store.get::<bool>("NerdStats"), Ok(false));
    }

    #[test]
    fn stats_rows_count_the_snapshot_and_guard_a_zero_frame_time() {
        let rig = Rig::new();
        let rows = nerd_stats_rows(&rig.snapshot, 0.02);
        let value = |label: &str| rows.iter().find(|(name, _)| *name == label).map(|(_, value)| value.as_str()).unwrap();
        assert_eq!(value("Frame time"), "20.0 ms");
        assert_eq!(value("Frames per second"), "50");
        assert_eq!(value("Document revision"), "3");
        assert_eq!(value("Nodes"), "4");
        assert_eq!(value("Parts"), "1");
        assert_eq!(value("Animations"), "0");
        assert_eq!(nerd_stats_rows(&rig.snapshot, 0.0)[1].1, "-");
    }

    #[test]
    fn the_open_dyslexic_setting_is_off_by_default_and_the_look_pane_stores_it() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        assert!(!use_open_dyslexic(&rig.store));
        let mut windows = Windows::new();
        windows.settings = Some(SettingsWindow { pane: Pane::Look, ..Default::default() });
        click(&ctx, &mut windows, &mut rig, Role::CheckBox, "Use OpenDyslexic Font");
        assert_eq!(rig.store.get::<bool>("useOpenDyslexic"), Ok(true));
        assert!(use_open_dyslexic(&rig.store));
    }

    #[test]
    fn the_stored_open_dyslexic_flag_swaps_the_fonts_on_the_next_frame() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(windows.dyslexic, Some(false));
        rig.store.set("useOpenDyslexic", true).unwrap();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(windows.dyslexic, Some(true));
        // egui takes new definitions at the start of the pass after `set_fonts`.
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let loaded = ctx.fonts_mut(|fonts| fonts.definitions().font_data.contains_key("open-dyslexic"));
        assert!(loaded, "the context received the OpenDyslexic definitions");
    }

    #[test]
    fn address_dialog_snapshots_resolved_configuration_and_translated_title() {
        let ctx = context();
        let mut windows = Windows::new();
        windows.apply_address_dialog(AddressDialogOperation::Open(AddressDialogTarget::Configuration),
            Some(Path::new("/missing/config/settings.json")), &ctx, &|title| format!("Translated {title}")).unwrap();
        let snapshot = windows.address_dialog_state();
        assert_eq!(snapshot, json!({"target":"configuration", "title":"Translated Open Configuration Folder", "address":"/missing/config"}));
        assert!(windows.apply_address_dialog(AddressDialogOperation::Open(AddressDialogTarget::Configuration), None, &ctx, &str::to_owned).is_err());
        assert_eq!(windows.address_dialog_state(), snapshot);
        windows.apply_address_dialog(AddressDialogOperation::Copy, Some(Path::new("/changed/settings.json")), &ctx, &str::to_owned).unwrap();
        assert_eq!(ctx.output(|output| output.commands.clone()), vec![egui::OutputCommand::CopyText("/missing/config".into())]);
        assert_eq!(windows.address_dialog_state(), snapshot);
    }

    #[test]
    fn welcome_menu_emits_the_shared_request_and_the_operation_opens_and_closes_it() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "Help");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Welcome");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_welcome_set", json!({"open":true}))));
        assert!(!windows.welcome_open());
        windows.set_welcome_open(true, &rig.store);
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(windows.welcome_open() && has(&output, "Create Project"), "the quick setup is done: the start page");
        // A second open keeps the window and its page; false closes it and writes nothing.
        windows.set_welcome_open(true, &rig.store);
        assert!(windows.welcome_open());
        windows.set_welcome_open(false, &rig.store);
        windows.set_welcome_open(false, &rig.store);
        assert!(!windows.welcome_open());
        // Until the quick setup is done the editor opens the window itself; closing through the operation
        // leaves the flag as it was.
        let mut fresh = Rig::new();
        fresh.store.set("firstrun_complete", true).unwrap();
        fresh.store.set("hasDoneQuickSetup", false).unwrap();
        let mut started = Windows::new();
        let (_, output) = frame(&ctx, &mut started, &mut fresh, Vec::new());
        assert!(started.welcome_open() && has(&output, "Quick Setup"));
        started.set_welcome_open(false, &fresh.store);
        assert!(!started.welcome_open());
        assert_eq!(fresh.store.get::<bool>("hasDoneQuickSetup"), Ok(false));
        started.set_welcome_open(true, &fresh.store);
        let (_, output) = frame(&ctx, &mut started, &mut fresh, Vec::new());
        assert!(has(&output, "Quick Setup") && !has(&output, "Create Project"));
    }

    #[test]
    fn about_menu_and_close_emit_shared_requests() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "Help");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "About");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_about_set", json!({"open":true}))));
        assert!(!windows.about_open());
        windows.set_about_open(true);
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Close window");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_about_set", json!({"open":false}))));
        assert!(windows.about_open());
        windows.set_about_open(false);
        assert!(!windows.about_open());
    }

    #[test]
    fn address_dialog_help_copy_and_close_emit_shared_requests() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        rig.store_path = Some("/missing/config/settings.json".into());
        for target in AddressDialogTarget::ALL {
            let menu = if target == AddressDialogTarget::Configuration { "View" } else { "Help" };
            click(&ctx, &mut windows, &mut rig, Role::Button, menu);
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, address_label(target));
            assert_eq!(only(requests), Request::Mcp(McpTarget::AddressDialog,
                call_request("editor_address_dialog", json!({"action":"open", "target":target.id()}))));
            assert_eq!(windows.address_dialog_state(), Value::Null);
        }
        windows.apply_address_dialog(AddressDialogOperation::Open(AddressDialogTarget::BugReport), None, &ctx, &str::to_owned).unwrap();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let original = windows.address_dialog_state()["address"].as_str().unwrap().to_owned();
        click(&ctx, &mut windows, &mut rig, Role::TextInput, &original);
        let (_, output) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Text(" edited".into())]);
        assert!(has(&output, "edited"));
        let (requests, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Copy");
        assert!(!has(&output, "edited"));
        assert_eq!(only(requests), Request::Mcp(McpTarget::AddressDialog, call_request("editor_address_dialog", json!({"action":"copy"}))));
        let snapshot = windows.address_dialog_state();
        windows.apply_address_dialog(AddressDialogOperation::Copy, rig.store_path.as_deref(), &ctx, &str::to_owned).unwrap();
        assert_eq!(ctx.output_mut(|output| std::mem::take(&mut output.commands)), vec![egui::OutputCommand::CopyText(original)]);
        assert_eq!(windows.address_dialog_state(), snapshot);
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Close window");
        assert_eq!(only(requests), Request::Mcp(McpTarget::AddressDialog, call_request("editor_address_dialog", json!({"action":"close"}))));
        assert_eq!(windows.address_dialog_state(), snapshot);
    }

    #[test]
    fn texture_export_and_replace_menu_entries_open_directory_prompts() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        for (label, kind) in [
            ("Export textures...", PromptKind::TextureExport),
            ("Replace textures...", PromptKind::TextureReplace),
        ] {
            let mut windows = Windows::new();
            click(&ctx, &mut windows, &mut rig, Role::Button, "Tools");
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, label);
            assert!(requests.is_empty(), "{label}: {requests:?}");
            assert_eq!(windows.prompt.as_ref().map(|prompt| prompt.kind), Some(kind), "{label}");
        }
    }

    #[test]
    fn tools_menu_entries_run_the_tool_operations() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        for (menu, label, op) in [
            ("Tools", "Regenerate Node IDs", "tool.regenerate_node_ids"),
            ("Tools", "Attempt full repair...", "tool.repair"),
            ("Tools", "Bleed textures...", "tool.rebleed_textures"),
            ("View", "Save Snapshot as Guide Image", "sketch.snapshot"),
        ] {
            let mut windows = Windows::new();
            click(&ctx, &mut windows, &mut rig, Role::Button, menu);
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, label);
            assert_eq!(only(requests), Request::Core(json!({"op": op})), "{label}");
        }
    }

    fn only(requests: Vec<Request>) -> Request {
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.into_iter().next().unwrap()
    }

    #[test]
    fn the_first_run_writes_the_key_once_and_the_quick_setup_ends_with_next() {
        let ctx = context();
        let mut rig = Rig::new();
        let mut windows = Windows::new();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(rig.store.get::<bool>("firstrun_complete"), Ok(true));
        assert_eq!(rig.store.get::<bool>("hasDoneQuickSetup"), Ok(false));
        let (_, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Next");
        assert_eq!(rig.store.get::<bool>("hasDoneQuickSetup"), Ok(true));
        assert!(has(&output, "Create Project"), "the welcome page follows the quick setup");
        // A later start with both keys set shows no welcome window.
        let mut again = Windows::new();
        let (_, output) = frame(&ctx, &mut again, &mut rig, Vec::new());
        assert!(!has(&output, "Create Project") && !has(&output, "Quick Setup"));
    }

    #[test]
    fn welcome_entries_ask_for_new_open_import_and_a_recent_file() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.store.set("prev_projects", strings(&["/work/rig/a.inx"])).unwrap();
        let fresh = |windows: &mut Windows| windows.welcome = Some(Welcome { step: 1 });

        let mut windows = Windows::new();
        fresh(&mut windows);
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "New...");
        assert_eq!(requests, [Request::Command("FileCommand.NewFile".to_owned())]);
        assert!(windows.welcome.is_none(), "choosing closes the window");

        let mut windows = Windows::new();
        fresh(&mut windows);
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "a.inx");
        assert_eq!(requests, [Request::Open("/work/rig/a.inx".to_owned())]);

        let mut windows = Windows::new();
        fresh(&mut windows);
        let (requests, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Open...");
        assert!(requests.is_empty());
        assert!(has(&output, "Path"), "Open asks for a path");

        let mut windows = Windows::new();
        fresh(&mut windows);
        click(&ctx, &mut windows, &mut rig, Role::Button, "Import...");
        assert_eq!(windows.prompt.as_ref().map(|prompt| prompt.kind), Some(PromptKind::ImportPsd));
    }

    #[test]
    fn the_path_prompt_sends_open_save_as_and_import_requests() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("KeepLayerFolder", "NotPreserve".to_owned()).unwrap();
        rig.store.set("LayerGroupReplacement", "DynamicComposite".to_owned()).unwrap();
        for (kind, button, expected) in [
            (PromptKind::Open, "Open", Request::Open("/p/x.inx".to_owned())),
            (PromptKind::SaveAs, "Save", Request::SaveAs("/p/x.inx".to_owned())),
            (
                PromptKind::ImportPsd,
                "Import",
                Request::Core(json!({"op": "import.psd", "path": "/p/x.inx", "keepStructure": false, "layerGroupNodeType": "DynamicComposite"})),
            ),
        ] {
            let mut windows = Windows::new();
            windows.started = true;
            windows.prompt = Some(PathPrompt::new(kind, " /p/x.inx ".to_owned()));
            let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, button);
            assert_eq!(only(requests), expected);
            assert!(windows.prompt.is_none());
        }
        // An empty path cannot be sent.
        let mut windows = Windows::new();
        windows.started = true;
        windows.prompt = Some(PathPrompt::new(PromptKind::Open, String::new()));
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, node) = nodes(&laid_out).into_iter().find(|(_, n)| n.role() == Role::Button && n.label() == Some("Open")).unwrap();
        assert!(node.is_disabled());
    }

    #[test]
    fn menu_commands_become_requests_and_respect_enabled_state() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.started = true;
        // File > New
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let at = center(&laid_out, Role::Button, "File");
        frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, open) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let new = center(&open, Role::Button, "New");
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(new), press(new, true), press(new, false)]);
        assert_eq!(only(requests), Request::Command("FileCommand.NewFile".to_owned()));

        // The pure enabled rules the menu uses.
        let tr = |msgid: &str| msgid.to_owned();
        let env = |rig: &mut Rig, selected: Option<u64>| {
            let mut copy = Rig {
                snapshot: rig.snapshot.clone(),
                store: Settings::empty(),
                store_path: None,
                shortcuts: ShortcutRegistry::new(),
                locales: Vec::new(),
                selected,
                keys_left: 0,
                mcp: McpStatus::default(),
            };
            let outcome = {
                let e = Env {
                    all_keys: [true, false],
                    ctx: &ctx,
                    snapshot: &copy.snapshot,
                    selected: copy.selected,
                    project: None,
                    store: &mut copy.store,
                    store_path: None,
                    shortcuts: &mut copy.shortcuts,
                    locales: &[],
                    lang: "en",
                    tr: &tr,
                    mcp: &copy.mcp,
                    out: Vec::new(),
                };
                [
                    "EditCommand.Undo",
                    "EditCommand.Redo",
                    "NodeCommand.CopyNode",
                    "NodeCommand.PasteNode",
                    "FileCommand.NewFile",
                    "AnimationCommand.KeyAll",
                    "AnimationCommand.UnkeyAll",
                ]
                .map(|id| e.command_enabled(id))
            };
            outcome
        };
        // Key all and unkey all follow what the editor says the timeline can do.
        assert_eq!(env(&mut rig, Some(9)), [true, false, true, true, true, true, false]);
        assert_eq!(env(&mut rig, None), [true, false, false, true, true, true, false]);
        rig.snapshot["documentFormat"] = json!("live2d.can3");
        assert_eq!(env(&mut rig, Some(9)), [true, false, false, false, true, true, false]);
    }

    #[test]
    fn the_palette_shortcut_opens_the_command_list_and_a_tool_runs_after_validation() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::P, true, true)]);
        assert_eq!(requests, [Request::Mcp(McpTarget::Layout,
            call_request("editor_command_browser_set", json!({"open":true})))]);
        assert!(!windows.command_browser_open());
        windows.set_command_browser_open(true);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(requests, [Request::Mcp(McpTarget::ToolList, list_request())]);
        assert!(windows.palette.is_some());
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(requests.is_empty(), "the tool list is asked for once");

        let mut session = soul2d_core::Session::new();
        windows.deliver(McpTarget::ToolList, session.handle_mcp(&list_request()));
        {
            let palette = windows.palette.as_mut().unwrap();
            assert!(palette.tools.len() > 50, "{} tools", palette.tools.len());
            assert!(!palette.need_list);
            palette.query = "open".to_owned();
        }
        // Selecting `open` fills a template; running it with the empty path is refused locally.
        choose(&mut windows, "open");
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (requests, output) = click(&ctx, &mut windows, &mut rig, Role::Button, "Run Command");
        assert!(requests.is_empty(), "invalid arguments never reach the endpoint: {requests:?}");
        assert!(has(&output, "Missing") || has(&output, "required"), "{:?}", texts(&output));
        windows.palette.as_mut().unwrap().args = r#"{"path": "/tmp/none.inx"}"#.to_owned();
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Run Command");
        let call = call_request("open", json!({"path": "/tmp/none.inx"}));
        assert_eq!(only(requests), Request::Mcp(McpTarget::ToolCall, call.clone()));
        // The failing answer of the real session is shown as a failure with its message.
        windows.deliver(McpTarget::ToolCall, session.handle_mcp(&call));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "Failed"), "{:?}", texts(&output));
        assert!(!windows.palette.as_ref().unwrap().running);
    }

    #[test]
    fn palette_enter_only_runs_tools_without_declared_arguments() {
        for (id, runs) in [("parameter_group_create", false), ("history_summary", true), ("open", false)] {
            let ctx = context();
            let mut rig = Rig::new();
            rig.store.set("firstrun_complete", true).unwrap();
            rig.store.set("hasDoneQuickSetup", true).unwrap();
            let mut windows = Windows::new();
            loaded_palette(&mut windows);
            windows.palette.as_mut().unwrap().query = id.to_owned();
            choose(&mut windows, id);
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::Enter, false, false)]);
            if runs {
                assert_eq!(requests, [Request::Mcp(McpTarget::ToolCall, call_request(id, json!({})))]);
            } else {
                assert!(requests.is_empty(), "{id} ran before its arguments were confirmed: {requests:?}");
            }
            if id == "parameter_group_create" {
                assert_eq!(windows.palette.as_ref().unwrap().args, "{}");
                let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Run Command");
                assert_eq!(requests, [Request::Mcp(McpTarget::ToolCall, call_request(id, json!({})))]);
            }
        }
    }

    #[test]
    fn running_an_editor_action_from_the_palette_sends_its_request_and_closes_it() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.set_command_browser_open(true);
        windows.palette.as_mut().unwrap().need_list = false;
        windows.palette.as_mut().unwrap().list_sent = true;
        windows.palette.as_mut().unwrap().query = "redo".to_owned();
        windows.palette.as_mut().unwrap().chosen = Some("EditCommand.Undo".to_owned());
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        // Redo is disabled while nothing was undone; Undo (the earlier choice) is hidden by the filter.
        let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, run) = nodes(&laid_out).into_iter().find(|(_, n)| n.role() == Role::Button && n.label() == Some("Run Command")).unwrap();
        assert!(run.is_disabled());
        windows.palette.as_mut().unwrap().query = "undo".to_owned();
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Run Command");
        assert_eq!(requests, [Request::Command("EditCommand.Undo".to_owned()),
            Request::Mcp(McpTarget::Layout, call_request("editor_command_browser_set", json!({"open":false})))]);
        assert!(windows.command_browser_open(), "the shared operation applies after drawing");
        windows.set_command_browser_open(false);
        assert!(windows.palette.is_none(), "D closes the palette after a command ran");
    }

    #[test]
    fn command_browser_duplicate_and_self_open_preserve_its_payload() {
        let contents = |palette: &Palette| (palette.tools.clone(), palette.query.clone(), palette.chosen.clone(),
            palette.args.clone(), palette.args_error.clone(), palette.list_error.clone(), palette.need_list,
            palette.list_sent, palette.running, palette.outcome.clone(), palette.focus);
        for id in ["EditCommand.ShowCommandBrowserWindow", PALETTE_ID] {
            let ctx = context();
            let mut rig = Rig::new();
            rig.store.set("firstrun_complete", true).unwrap();
            rig.store.set("hasDoneQuickSetup", true).unwrap();
            let mut windows = Windows::new();
            loaded_palette(&mut windows);
            let palette = windows.palette.as_mut().unwrap();
            palette.query = "command".into();
            palette.chosen = Some(id.into());
            palette.args = "retained argument draft".into();
            palette.args_error = Some("retained validation message".into());
            palette.list_error = Some("retained listing message".into());
            palette.list_sent = true;
            palette.running = true;
            palette.outcome = Some(Outcome { ok: true, text: "retained result".into() });
            palette.focus = false;
            let before = contents(palette);
            windows.set_command_browser_open(true);
            windows.set_command_browser_open(true);
            assert_eq!(contents(windows.palette.as_ref().unwrap()), before);

            windows.palette.as_mut().unwrap().running = false;
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
            let at = center(&laid_out, Role::Button, "Run Command");
            let before = contents(windows.palette.as_ref().unwrap());
            let (requests, _) = frame(&ctx, &mut windows, &mut rig,
                vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
            assert_eq!(requests, [Request::Mcp(McpTarget::Layout,
                call_request("editor_command_browser_set", json!({"open":true})))]);
            assert_eq!(contents(windows.palette.as_ref().unwrap()), before, "{id}");
            windows.set_command_browser_open(true);
            assert_eq!(contents(windows.palette.as_ref().unwrap()), before, "{id}");
        }
    }

    #[test]
    fn command_browser_menu_and_native_dismissal_share_the_operation() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "Edit");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Command Browser");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_command_browser_set", json!({"open":true}))));
        assert!(!windows.command_browser_open());
        windows.set_command_browser_open(true);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::Escape, false, false)]);
        assert_eq!(requests, [Request::Mcp(McpTarget::Layout,
            call_request("editor_command_browser_set", json!({"open":false})))]);
        windows.set_command_browser_open(false);
        windows.set_command_browser_open(false);
        let mut session = soul2d_core::Session::new();
        windows.deliver(McpTarget::ToolList, session.handle_mcp(&list_request()));
        windows.deliver(McpTarget::ToolCall, Some(json!({"result":{"isError":false}})));
        assert!(!windows.command_browser_open(), "closed responses never recreate the browser");
        windows.set_command_browser_open(true);
        let palette = windows.palette.as_ref().unwrap();
        assert!(palette.query.is_empty() && palette.chosen.is_none() && palette.args.is_empty() && palette.outcome.is_none());
    }

    #[test]
    fn command_browser_native_close_suppresses_same_frame_commands_tools_and_refresh() {
        for action in ["command", "tool", "refresh"] {
            let ctx = context();
            let mut rig = Rig::new();
            rig.store.set("firstrun_complete", true).unwrap();
            rig.store.set("hasDoneQuickSetup", true).unwrap();
            let mut windows = Windows::new();
            loaded_palette(&mut windows);
            if action == "tool" {
                choose(&mut windows, "editor_command_browser_set");
                windows.palette.as_mut().unwrap().args = r#"{"open":true}"#.into();
            } else {
                windows.palette.as_mut().unwrap().chosen = Some("FileCommand.NewFile".into());
            }
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
            let at = center(&laid_out, Role::Button, if action == "refresh" { "Refresh" } else { "Run Command" });
            let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(at),
                press(at, true), press(at, false), key(egui::Key::Escape, false, false)]);
            assert_eq!(requests, [Request::Mcp(McpTarget::Layout,
                call_request("editor_command_browser_set", json!({"open":false})))], "{action}");
            windows.set_command_browser_open(false);
            assert!(!windows.command_browser_open());
        }
    }

    #[test]
    fn enter_on_a_tool_that_creates_a_node_waits_for_its_arguments() {
        let ctx = context();
        let mut rig = Rig::new();
        let mut windows = Windows::new();
        loaded_palette(&mut windows);
        let tr = |msgid: &str| msgid.to_owned();
        let env = flip_env(&ctx, &mut rig, &tr);
        let entries = windows.palette.as_ref().unwrap().tools.clone();
        choose(&mut windows, "Node_Add_GridDeformer");
        let palette = windows.palette.as_mut().unwrap();
        palette.args = "{}".to_owned();
        assert!(palette.activate(&entries, &env).is_none(), "Enter must not create a node");
        // A tool that needs no argument still runs at once.
        choose(&mut windows, "nodes_list");
        let palette = windows.palette.as_mut().unwrap();
        assert!(matches!(palette.activate(&entries, &env), Some(PaletteAction::Call { name, .. }) if name == "nodes_list"));
    }

    #[test]
    fn settings_controls_write_the_d_keys_and_ask_the_editor_to_follow() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.settings = Some(SettingsWindow { pane: Pane::Files, ..Default::default() });
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::CheckBox, "Enable Autosaves");
        assert_eq!(only(requests), Request::AutosaveChanged);
        assert_eq!(rig.store.get::<bool>("AutosaveEnabled"), Ok(false));
        assert_eq!(rig.store.get::<i64>("AutosaveInterval"), Ok(5));
        assert_eq!(rig.store.get::<i64>("AutosaveFileLimit"), Ok(3));

        // The import behaviour combos store D's option keys.
        windows.settings = Some(SettingsWindow { pane: Pane::Files, ..Default::default() });
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let at = center(&laid_out, Role::ComboBox, "Always ask");
        frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
        let (_, open) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let pick = center(&open, Role::Button, "Preserve");
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(pick), press(pick, true), press(pick, false)]);
        assert_eq!(only(requests), Request::ImportDefaultsChanged);
        assert_eq!(rig.store.get::<String>("KeepLayerFolder"), Ok("Preserve".to_owned()));

        // Language goes to the editor, which owns the catalogue.
        windows.settings = Some(SettingsWindow::default());
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, laid_out) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let at = center(&laid_out, Role::ComboBox, "English");
        frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(at), press(at, true), press(at, false)]);
        let (_, open) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let pick = center(&open, Role::Button, "日本語");
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::PointerMoved(pick), press(pick, true), press(pick, false)]);
        assert_eq!(only(requests), Request::SetLanguage("ja".to_owned()));
    }

    #[test]
    fn the_integration_pane_starts_the_editors_mcp_server_from_named_fields_and_shows_a_generated_token() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.settings = Some(SettingsWindow { pane: Pane::Integration, ..Default::default() });
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for name in ["Enable MCP server", "Require a bearer token (MCP.authEnabled)"] {
            assert!(has(&output, name), "{name}");
        }
        assert!(has(&output, "Off: no port is opened."), "{:?}", texts(&output));
        assert!(!has(&output, "Bearer token"), "no token is shown while the server is off");
        assert!(assert_all_named(&output, "integration pane") >= 5);
        // Nothing is stored or applied until Apply; the button is disabled without a change.
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::CheckBox, "Enable MCP server");
        assert!(requests.is_empty());
        assert_eq!(rig.store.get::<bool>("MCP.Enabled"), Ok(false));
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Apply MCP settings");
        assert_eq!(only(requests), Request::McpApply);
        assert_eq!(rig.store.get::<bool>("MCP.Enabled"), Ok(true));
        assert_eq!(rig.store.get::<i64>("MCP.Port"), Ok(8088));
        assert_eq!(rig.store.get::<String>("MCP.Host"), Ok("127.0.0.1".to_owned()));
        assert_eq!(rig.store.get::<bool>("MCP.authEnabled"), Ok(false));

        // The running server's URL, error and generated token come from the host.
        rig.mcp = McpStatus { url: Some("http://127.0.0.1:8088/mcp".into()), token: Some("ab".repeat(32)), error: None };
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "Serving on http://127.0.0.1:8088/mcp"), "{:?}", texts(&output));
        assert!(has(&output, &"ab".repeat(32)), "the generated token is shown as copyable text");
        assert!(has(&output, "D's OAuth flow, approval popup and SSE stream are replaced by this static bearer token."));
        assert_all_named(&output, "integration pane with a token");
        rig.mcp = McpStatus { url: None, token: None, error: Some("refusing to serve 0.0.0.0".into()) };
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "refusing to serve 0.0.0.0"));
        assert!(!has(&output, "Serving on"));
    }

    #[test]
    fn the_files_pane_offers_the_save_on_close_choice() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.settings = Some(SettingsWindow { pane: Pane::Files, ..Default::default() });
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "Save project on close"), "{:?}", texts(&output));
        assert!(has(&output, "Unsaved changes"));
        assert_all_named(&output, "files pane");
    }

    #[test]
    fn file_recent_lists_the_autosaves_and_opening_one_asks_for_its_backup_under_the_project() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.store.set("prev_projects", strings(&["/work/a.inx"])).unwrap();
        autosave::add_prev_autosave(&mut rig.store, "/config/autosaves/a/a_100.inx", "/work/a.inx").unwrap();
        let mut windows = Windows::new();
        click(&ctx, &mut windows, &mut rig, Role::Button, "File");
        click(&ctx, &mut windows, &mut rig, Role::Button, "Recent");
        let (_, menu) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&menu, "Autosaves"), "{:?}", texts(&menu));
        assert!(has(&menu, "a.inx"), "the recent project stays listed");
        click(&ctx, &mut windows, &mut rig, Role::Button, "Autosaves");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "a_100.inx");
        assert_eq!(
            only(requests),
            Request::OpenAutosave(autosave::AutosaveRecord {
                autosave: "/config/autosaves/a/a_100.inx".into(),
                mainsave: "/work/a.inx".into(),
            })
        );
    }

    #[test]
    fn rebinding_a_shortcut_takes_it_from_its_old_command_and_persists_it_in_the_store() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.settings = Some(SettingsWindow { pane: Pane::Shortcuts, ..Default::default() });
        click(&ctx, &mut windows, &mut rig, Role::Button, "Set shortcut for New");
        assert!(windows.settings.as_ref().unwrap().capture.is_some());
        // Ctrl+O belongs to Open: the key press is swallowed by the capture and moves the binding.
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::O, true, false)]);
        assert!(requests.is_empty(), "the captured press must not run Open: {requests:?}");
        assert_eq!(rig.keys_left, 0, "the key event is removed before editor_ui's own shortcut handler runs");
        let chord = |text: &str| Chord::from_spec(text).unwrap();
        assert_eq!(rig.shortcuts.binding_for("FileCommand.NewFile").unwrap().chord, chord("Ctrl-O"));
        assert!(rig.shortcuts.binding_for("FileCommand.ShowOpenFileDialog").is_none());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let shown = chord_text(&chord("Ctrl-O"), Platform::current(), false);
        let message = format!("{shown} was used by Open; that shortcut is now unassigned");
        assert!(has(&output, &message), "{:?}", texts(&output));
        let stored = rig.store.get::<std::collections::BTreeMap<String, String>>("Shortcuts").unwrap();
        let stored_text = chord("Ctrl-O").format(Platform::current());
        assert_eq!(stored.get("FileCommand.NewFile"), Some(&stored_text));
        assert_eq!(stored.get("FileCommand.ShowOpenFileDialog"), None);

        // Without a capture the same key stays in the input for editor_ui.
        frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::O, true, false)]);
        assert_eq!(rig.keys_left, 1);

        // Clear removes a binding; Escape cancels a capture without binding anything.
        let (_, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Clear shortcut for Undo");
        assert!(rig.shortcuts.binding_for("EditCommand.Undo").is_none());
        click(&ctx, &mut windows, &mut rig, Role::Button, "Set shortcut for Redo");
        let before = rig.shortcuts.clone();
        frame(&ctx, &mut windows, &mut rig, vec![key(egui::Key::Escape, false, false)]);
        assert!(windows.settings.as_ref().unwrap().capture.is_none());
        assert_eq!(rig.shortcuts, before);
    }

    /// The answer of a `texture_get` call as the MCP endpoint wraps it.
    fn texture_response(slot: u64, width: u32, height: u32) -> Value {
        use base64::Engine;
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255])))
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        let answer = json!({"slot": slot, "width": width, "height": height, "format": "png",
            "base64": base64::engine::general_purpose::STANDARD.encode(data.into_inner())});
        json!({"result": {"isError": false, "content": [{"type": "text", "text": "{}"}], "structuredContent": answer}})
    }

    #[test]
    fn texture_viewer_menu_close_and_repeated_open_preserve_payload() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        assert!(!windows.texture_viewer_open());
        click(&ctx, &mut windows, &mut rig, Role::Button, "Tools");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Texture Viewer");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_texture_viewer_set", json!({"open":true}))));
        assert!(!windows.texture_viewer_open());
        windows.set_texture_viewer_open(true);
        click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        windows.deliver(McpTarget::Texture(0), Some(texture_response(0, 2, 1)));
        let viewer = windows.texture.as_mut().unwrap();
        viewer.slot = 2;
        viewer.zoom = 2.75;
        let loaded = viewer.loaded.as_ref().unwrap();
        let handle = loaded.handles[0].as_ref().unwrap().id();
        let pixels = loaded.slots[0].as_ref().unwrap().rgba.clone();
        let stamp = (loaded.node, loaded.revision);
        for _ in 0..2 {
            windows.set_texture_viewer_open(true);
            let viewer = windows.texture.as_ref().unwrap();
            assert_eq!((viewer.slot, viewer.zoom), (2, 2.75));
            let loaded = viewer.loaded.as_ref().unwrap();
            assert_eq!((loaded.node, loaded.revision), stamp);
            assert_eq!(loaded.handles[0].as_ref().unwrap().id(), handle);
            assert_eq!(loaded.slots[0].as_ref().unwrap().rgba, pixels);
        }
        click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        assert!(windows.texture.as_ref().unwrap().loading);
        let waiting = windows.texture_request.as_ref().unwrap().waiting;
        windows.set_texture_viewer_open(true);
        assert!(windows.texture.as_ref().unwrap().loading);
        assert_eq!(windows.texture_request.as_ref().unwrap().waiting, waiting);
        windows.deliver(McpTarget::Texture(0), Some(json!({"result":{"isError":true,
            "content":[{"type":"text","text":"retained texture failure"}]}})));
        windows.set_texture_viewer_open(true);
        assert_eq!(windows.texture.as_ref().unwrap().error.as_deref(), Some("retained texture failure"));
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Close window");
        assert_eq!(only(requests), Request::Mcp(McpTarget::Layout,
            call_request("editor_texture_viewer_set", json!({"open":false}))));
        assert!(windows.texture_viewer_open(), "only the shared handler destroys the viewer");
        windows.set_texture_viewer_open(false);
        windows.set_texture_viewer_open(false);
        assert!(!windows.texture_viewer_open());
        windows.set_texture_viewer_open(true);
        let viewer = windows.texture.as_ref().unwrap();
        assert_eq!((viewer.slot, viewer.zoom), (0, 1.0));
        assert!(viewer.loaded.is_none() && viewer.error.is_none() && !viewer.loading);
        assert!(!Windows::new().texture_viewer_open());
    }

    #[test]
    fn texture_viewer_same_frame_close_precedes_reads_and_closed_replies() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.snapshot["model"]["nodes"]["children"][0]["textures"] = json!([4, 7, 5]);
        let mut windows = Windows::new();
        windows.set_texture_viewer_open(true);
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        let events = ["Close window", "Load textures"].map(|label| {
            let target = output.iter().find(|(_, node)| node.role() == Role::Button
                && node.label() == Some(label)).unwrap().0;
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click, target, data: None })
        });
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, events.into());
        let mut expected = vec![Request::Mcp(McpTarget::Layout,
            call_request("editor_texture_viewer_set", json!({"open":false})))];
        expected.extend([(0u8, 4u64), (1, 7), (2, 5)].into_iter().map(|(position, slot)|
            Request::Mcp(McpTarget::Texture(position), call_request("texture_get", json!({"slot":slot})))));
        assert_eq!(requests, expected);
        assert!(windows.texture_viewer_open());
        windows.set_texture_viewer_open(false);
        assert!(windows.texture_request.is_some(), "close retains the existing aggregation lifetime");
        windows.deliver(McpTarget::Texture(2), Some(texture_response(5, 1, 1)));
        windows.deliver(McpTarget::Texture(0), Some(texture_response(4, 3, 2)));
        assert!(windows.texture_request.is_some());
        assert!(!windows.texture_viewer_open());
        windows.deliver(McpTarget::Texture(1), Some(texture_response(7, 2, 2)));
        assert!(windows.texture_request.is_none());
        assert!(!windows.texture_viewer_open(), "replies cannot reopen the viewer");
        windows.set_texture_viewer_open(true);
        assert!(windows.texture.as_ref().unwrap().loaded.is_none());
    }

    #[test]
    fn the_texture_viewer_loads_each_listed_slot_with_texture_get_and_shows_size_and_slots() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.set_texture_viewer_open(true);
        // The Part lists only its albedo slot (the 4294967295 entry is the empty marker).
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        let call = call_request("texture_get", json!({"slot": 0}));
        assert_eq!(only(requests), Request::Mcp(McpTarget::Texture(0), call));
        windows.deliver(McpTarget::Texture(0), Some(texture_response(0, 2, 1)));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        for expected in ["2 x 1 px, texture slot 0", "Slot", "Zoom"] {
            assert!(has(&output, expected), "missing {expected:?} in {:?}", texts(&output));
        }
        assert_all_named(&output, "loaded texture viewer");
        // A document change after loading is flagged.
        rig.snapshot["revision"] = json!(4);
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "The document changed since these were loaded."), "{:?}", texts(&output));
        // A failing read is reported, not shown.
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        assert_eq!(requests.len(), 1);
        let failed = json!({"result": {"isError": true, "content": [{"type": "text", "text": "texture slot 0 is out of range (0 textures)"}]}});
        windows.deliver(McpTarget::Texture(0), Some(failed));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "texture slot 0 is out of range (0 textures)"), "{:?}", texts(&output));
        assert!(!has(&output, "2 x 1 px"));
    }

    #[test]
    fn a_part_with_three_slots_asks_for_each_and_waits_for_all_answers() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.snapshot["model"]["nodes"]["children"][0]["textures"] = json!([4, 7, 5]);
        let mut windows = Windows::new();
        windows.set_texture_viewer_open(true);
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        let expected: Vec<Request> = [(0u8, 4u64), (1, 7), (2, 5)]
            .into_iter()
            .map(|(position, slot)| Request::Mcp(McpTarget::Texture(position), call_request("texture_get", json!({"slot": slot}))))
            .collect();
        assert_eq!(requests, expected);

        windows.deliver(McpTarget::Texture(2), Some(texture_response(5, 1, 1)));
        windows.deliver(McpTarget::Texture(0), Some(texture_response(4, 3, 2)));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(!has(&output, "Zoom"), "still waiting for the emissive slot: {:?}", texts(&output));
        windows.deliver(McpTarget::Texture(1), Some(texture_response(7, 2, 2)));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "3 x 2 px, texture slot 4"), "{:?}", texts(&output));
        let loaded = windows.texture.as_ref().unwrap().loaded.as_ref().unwrap();
        let sizes: Vec<Option<(usize, usize)>> = loaded.slots.iter().map(|s| s.as_ref().map(|i| (i.width, i.height))).collect();
        assert_eq!(sizes, [Some((3, 2)), Some((2, 2)), Some((1, 1))]);

        // One failing slot fails the whole load with its message.
        click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        windows.deliver(McpTarget::Texture(0), Some(texture_response(4, 3, 2)));
        windows.deliver(McpTarget::Texture(1), Some(json!({"result": {"isError": true, "content": [{"type": "text", "text": "boom"}]}})));
        windows.deliver(McpTarget::Texture(2), Some(texture_response(5, 1, 1)));
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "boom") && !has(&output, "Zoom"), "{:?}", texts(&output));
    }

    #[test]
    fn a_part_without_slots_loads_nothing_and_asks_for_nothing() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.snapshot["model"]["nodes"]["children"][0]["textures"] = json!([4294967295u64, null, null]);
        let mut windows = Windows::new();
        windows.set_texture_viewer_open(true);
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Load textures");
        assert!(requests.is_empty(), "{requests:?}");
        let (_, output) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(has(&output, "This slot has no texture."), "{:?}", texts(&output));
    }

    /// An `extdata_get` answer as the MCP endpoint wraps it.
    fn flip_response(present: bool, payload: &[u8]) -> Value {
        use base64::Engine;
        let answer = json!({"key": FLIP_CONFIG_KEY, "present": present,
            "base64": present.then(|| base64::engine::general_purpose::STANDARD.encode(payload))});
        json!({"result": {"isError": false, "content": [{"type": "text", "text": "{}"}], "structuredContent": answer}})
    }

    fn flip_env<'a>(
        ctx: &'a egui::Context,
        rig: &'a mut Rig,
        tr: &'a dyn Fn(&str) -> String,
    ) -> Env<'a> {
        Env {
            all_keys: [false; 2],
            ctx,
            snapshot: &rig.snapshot,
            selected: None,
            project: None,
            store: &mut rig.store,
            store_path: None,
            shortcuts: &mut rig.shortcuts,
            locales: &[],
            lang: "en",
            tr,
            mcp: &rig.mcp,
            out: Vec::new(),
        }
    }

    #[test]
    fn the_flip_window_reads_the_document_with_extdata_get_and_saves_with_extdata_set() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.started = true;
        let tr = |msgid: &str| msgid.to_owned();

        // Opening asks the session for the stored pairs and shows nothing until they arrive.
        let requests = {
            let mut env = flip_env(&ctx, &mut rig, &tr);
            windows.open_flip(&mut env);
            env.out
        };
        assert_eq!(
            requests,
            [Request::Mcp(McpTarget::FlipRead, call_request("extdata_get", json!({"key": FLIP_CONFIG_KEY})))]
        );
        assert!(windows.flip.is_none());
        windows.deliver(McpTarget::FlipRead, Some(flip_response(true, br#"[{"uuid1":12,"uuid2":13}]"#)));
        assert_eq!(windows.flip.as_ref().unwrap().pairs, [pair_of(12, 13)]);
        assert_eq!(windows.flip.as_ref().unwrap().message, "Read from the document.");

        // Pair more by name, then Save sends the whole list to the document.
        windows.flip.as_mut().unwrap().pairs.clear();
        windows.flip.as_mut().unwrap().left = "_L".to_owned();
        windows.flip.as_mut().unwrap().right = "_R".to_owned();
        click(&ctx, &mut windows, &mut rig, Role::Button, "Pair");
        assert_eq!(windows.flip.as_ref().unwrap().pairs, [pair_of(12, 13)]);
        assert_eq!(windows.flip.as_ref().unwrap().message, "1 pairs added");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Save");
        let stored = json!({"key": FLIP_CONFIG_KEY, "base64": "W3sidXVpZDEiOjEyLCJ1dWlkMiI6MTN9XQ=="});
        assert_eq!(only(requests), Request::Mcp(McpTarget::FlipWrite, call_request("extdata_set", stored)));
        assert!(windows.flip.is_none());

        // A successful write needs no message; a refused one is handed to the editor on the next frame.
        windows.deliver(McpTarget::FlipWrite, Some(flip_response(true, b"")));
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(requests.is_empty(), "{requests:?}");
        let refused = json!({"result": {"isError": true, "content": [{"type": "text", "text": "base64 payload is too large"}]}});
        windows.deliver(McpTarget::FlipWrite, Some(refused));
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(only(requests), Request::Notice("Flip pairs were not saved: base64 payload is too large".to_owned()));
        windows.deliver(McpTarget::FlipWrite, None);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert_eq!(only(requests), Request::Notice("Flip pairs were not saved: The endpoint sent no response".to_owned()));
    }

    #[test]
    fn saving_no_pairs_removes_the_key_and_a_failed_read_says_so() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        windows.started = true;
        let tr = |msgid: &str| msgid.to_owned();
        {
            let mut env = flip_env(&ctx, &mut rig, &tr);
            windows.open_flip(&mut env);
        }
        windows.deliver(McpTarget::FlipRead, Some(flip_response(false, b"")));
        assert_eq!(windows.flip.as_ref().unwrap().pairs, Vec::<FlipPair>::new());
        assert_eq!(windows.flip.as_ref().unwrap().message, "The document has no flip pairs yet.");
        let (requests, _) = click(&ctx, &mut windows, &mut rig, Role::Button, "Save");
        let removal = json!({"key": FLIP_CONFIG_KEY, "base64": null});
        assert_eq!(only(requests), Request::Mcp(McpTarget::FlipWrite, call_request("extdata_set", removal)));

        // The read failed: the window opens empty with the reason.
        windows.flip = None;
        {
            let mut env = flip_env(&ctx, &mut rig, &tr);
            windows.open_flip(&mut env);
        }
        let failed = json!({"result": {"isError": true, "content": [{"type": "text", "text": "unrecognised data after the texture section"}]}});
        windows.deliver(McpTarget::FlipRead, Some(failed));
        assert_eq!(
            windows.flip.as_ref().unwrap().message,
            "The document's flip pairs could not be read: unrecognised data after the texture section"
        );

        // A document that is not INX cannot hold pairs and nothing is asked of the session.
        windows.flip = None;
        rig.snapshot["documentFormat"] = json!("live2d.can3");
        let requests = {
            let mut env = flip_env(&ctx, &mut rig, &tr);
            windows.open_flip(&mut env);
            env.out
        };
        assert!(requests.is_empty());
        assert_eq!(windows.flip.as_ref().unwrap().message, "Flip pairs need an open INX document.");
    }

    #[test]
    fn look_settings_are_applied_to_the_context_once() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        rig.store.set("DarkMode", false).unwrap();
        let mut windows = Windows::new();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(!ctx.style().visuals.dark_mode, "DarkMode=false selects the light theme");
        rig.store.set("DarkMode", true).unwrap();
        frame(&ctx, &mut windows, &mut rig, Vec::new());
        assert!(ctx.style().visuals.dark_mode);
        if Platform::current() != Platform::MacOs {
            rig.store.set("UIScale", 1.5).unwrap();
            frame(&ctx, &mut windows, &mut rig, Vec::new());
            assert_eq!(ctx.zoom_factor(), 1.5);
        }
    }

    #[test]
    fn clipboard_chords_reach_the_node_commands_through_egui_clipboard_events() {
        let ctx = context();
        let mut rig = Rig::new();
        rig.store.set("firstrun_complete", true).unwrap();
        rig.store.set("hasDoneQuickSetup", true).unwrap();
        let mut windows = Windows::new();
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Copy]);
        assert_eq!(requests, [Request::NodeCopy]);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Cut]);
        assert_eq!(requests, [Request::NodeCut]);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Paste("text".to_owned())]);
        assert_eq!(requests, [Request::NodePaste]);
        // Nothing is selected: copy and cut are refused, paste still works with a clipboard.
        rig.selected = None;
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Copy, egui::Event::Cut]);
        assert!(requests.is_empty());
        // A rebinding moves the command: Ctrl+C no longer copies.
        rig.shortcuts.clear("NodeCommand.CopyNode");
        rig.selected = Some(9);
        let (requests, _) = frame(&ctx, &mut windows, &mut rig, vec![egui::Event::Copy]);
        assert!(requests.is_empty());
        assert_eq!(
            key_press(&key(egui::Key::ArrowLeft, true, true), false),
            Some(Press { key: "Left", command: true, super_key: false, alt: false, shift: true, repeat: false })
        );
        assert_eq!(key_press(&egui::Event::Text("x".to_owned()), false), None);
        // MacSwapCmdCtrl only acts on macOS: there Cmd becomes Super and Control becomes the chord's Ctrl,
        // and the clipboard events, which the platform sends for Cmd, follow.
        let mac = cfg!(target_os = "macos");
        let cmd = egui::Modifiers { command: true, mac_cmd: mac, ctrl: !mac, ..Default::default() };
        let control = egui::Modifiers { ctrl: true, command: !mac, ..Default::default() };
        assert_eq!(chord_modifiers(cmd, false), (true, false));
        assert_eq!(chord_modifiers(cmd, true), if mac { (false, true) } else { (true, false) });
        assert_eq!(chord_modifiers(control, false), if mac { (false, true) } else { (true, false) });
        assert_eq!(chord_modifiers(control, true), (true, false));
        let chord = Chord::from_spec("Ctrl-N").unwrap();
        assert_eq!(chord_text(&chord, Platform::MacOs, false), "Cmd+N");
        assert_eq!(chord_text(&chord, Platform::MacOs, true), "Ctrl+N", "swapped, Ctrl is the Control key");
        assert_eq!(chord_text(&Chord::from_spec("Super-N").unwrap(), Platform::MacOs, true), "Cmd+N");
        assert_eq!(chord_text(&chord, Platform::Linux, true), "Ctrl+N");
        let copy = key_press(&egui::Event::Copy, true).unwrap();
        assert_eq!((copy.command, copy.super_key), if mac { (false, true) } else { (true, false) });
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }
}
