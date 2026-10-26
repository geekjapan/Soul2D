//! Persistent editor settings, config paths and keyboard shortcut bindings.
//!
//! Port of source/soul2d/core/{settings,path}.d and core/shortcut. The `settings.json` layout is
//! the D one: a single JSON object of free-form keys. Shortcut overrides live under the
//! `"Shortcuts"` key as an object `{ "<CommandType>.<Value>": "<Ctrl+Shift+S>" }`.
//!
//! Settings keys the D app uses (type, default when absent):
//! `ViewportZoomMode` string "MousePosition" (new users; read fallback "ScreenCenter"),
//! `ViewportZoomSpeed` float 5.0 (new users; read fallback 1.0), `KeepLayerFolder` string "Ask",
//! `LayerGroupReplacement` string "Node", `MaxUndoHistory` uint 100, `UIScale` float 1.0,
//! `lang` string "en", `WinW` int 1280, `WinH` int 800, `WinMax` bool false, `DisableCompositor`
//! bool false, `useOpenDyslexic` bool false, `TripleBufferFallback` bool (platform default),
//! `DarkMode` bool false, `NerdStats` bool false, `firstrun_complete` bool (presence only),
//! `hasDoneQuickSetup` bool false, `AutosaveEnabled` bool true, `AutosaveInterval` int 5,
//! `AutosaveFileLimit` int 3, `prev_autosaves` / `prev_autosave_mainpaths` / `prev_projects`
//! string[], `SaveProjectOnClose` string "Ask", `MacSwapCmdCtrl` bool false, `MCP.Enabled` bool
//! false, `MCP.Host` string "127.0.0.1", `MCP.Port` int 8088, `MCP.authEnabled` bool false,
//! `MCP.Name` string "soul2d", `MCP.Headers` string "[]", `ACP.Command` string
//! "" (no agent ships with Soul2D), `ACP.Workdir` string, `Outline.visible` / `Timeline.visible` bool (default true; View > Panels), `Shortcuts` string map.

use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const APP_FOLDER_NAME: &str = "soul2d";
pub const ENV_CONFIG_PATH: &str = "INOCHI_CONFIG_PATH";
const SETTINGS_FILE: &str = "settings.json";
const SHORTCUTS_KEY: &str = "Shortcuts";

// ---------------------------------------------------------------------------------------------
// Config paths
// ---------------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    OtherPosix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::OtherPosix
        }
    }
}

/// Resolves the application config directory without touching the process environment.
/// `env` looks up a variable (unset and empty are treated alike), `exists` answers whether a path
/// exists (used for the legacy `.soul2d` directory on Linux and macOS).
pub fn resolve_config_dir(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let get = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(forced) = get(ENV_CONFIG_PATH) {
        return PathBuf::from(forced);
    }
    let legacy = format!(".{APP_FOLDER_NAME}");
    // Older installs used `.soul2d`; keep using it when present, otherwise use the plain name.
    let prefer_legacy = |base: PathBuf| {
        let old = base.join(&legacy);
        if exists(&old) {
            old
        } else {
            base.join(APP_FOLDER_NAME)
        }
    };
    match platform {
        Platform::Windows => {
            let base = get("AppData").map(PathBuf::from).unwrap_or_else(|| cwd.to_path_buf());
            base.join(legacy)
        }
        Platform::Linux => {
            let base = match get("XDG_CONFIG_HOME") {
                Some(xdg) => PathBuf::from(xdg),
                // D joins an unset HOME to an empty path, giving a relative ".config".
                None => get("HOME").map_or_else(|| PathBuf::from(".config"), |home| Path::new(&home).join(".config")),
            };
            prefer_legacy(base)
        }
        Platform::MacOs => {
            let base = match get("HOME") {
                Some(home) => Path::new(&home).join("Library").join("Application Support"),
                None => cwd.to_path_buf(),
            };
            prefer_legacy(base)
        }
        Platform::OtherPosix => {
            let base = get("HOME").map(PathBuf::from).unwrap_or_else(|| cwd.to_path_buf());
            base.join(legacy)
        }
    }
}

/// Resolves the config directory from the real environment. It is not created.
pub fn config_dir() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("cannot read current directory: {error}"))?;
    Ok(resolve_config_dir(Platform::current(), &|name| std::env::var(name).ok(), &cwd, &|path| path.exists()))
}

/// Resolves the config directory from the real environment and creates it.
pub fn app_config_dir() -> Result<PathBuf, String> {
    let dir = config_dir()?;
    ensure_dir(&dir)?;
    Ok(dir)
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SETTINGS_FILE)
}

pub fn imgui_ini_path(config_dir: &Path) -> PathBuf {
    config_dir.join("imgui.ini")
}

/// Custom fonts directory, created on demand like the D getter.
pub fn ensure_fonts_dir(config_dir: &Path) -> Result<PathBuf, String> {
    let dir = config_dir.join("fonts");
    ensure_dir(&dir)?;
    Ok(dir)
}

/// Custom locales directory, created on demand like the D getter.
pub fn ensure_locale_dir(config_dir: &Path) -> Result<PathBuf, String> {
    let dir = config_dir.join("i18n");
    ensure_dir(&dir)?;
    Ok(dir)
}

/// Extra read-only locale directory: the AppImage root (`$HERE`) on Linux, none elsewhere.
pub fn locale_dir_extra(platform: Platform, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if platform != Platform::Linux {
        return None;
    }
    env("HERE").filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))
}

// ---------------------------------------------------------------------------------------------
// Settings store
// ---------------------------------------------------------------------------------------------

/// A value type that can live in `settings.json`. Mirrors the D `incSettingsGet/Set` overloads.
pub trait SettingValue: Sized + Default {
    fn from_json(value: &Value) -> Result<Self, String>;
    fn to_json(&self) -> Result<Value, String>;
}

impl SettingValue for bool {
    fn from_json(value: &Value) -> Result<Self, String> {
        value.as_bool().ok_or_else(|| "expected a boolean".to_string())
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::Bool(*self))
    }
}

impl SettingValue for i64 {
    fn from_json(value: &Value) -> Result<Self, String> {
        value.as_i64().ok_or_else(|| "expected an integer".to_string())
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::from(*self))
    }
}

impl SettingValue for u64 {
    fn from_json(value: &Value) -> Result<Self, String> {
        value.as_u64().ok_or_else(|| "expected an unsigned integer".to_string())
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::from(*self))
    }
}

impl SettingValue for f64 {
    // Like D `get!float`, integer-valued JSON numbers are accepted.
    fn from_json(value: &Value) -> Result<Self, String> {
        value.as_f64().ok_or_else(|| "expected a number".to_string())
    }
    fn to_json(&self) -> Result<Value, String> {
        Number::from_f64(*self)
            .map(Value::Number)
            .ok_or_else(|| "cannot store a non-finite number".to_string())
    }
}

impl SettingValue for String {
    fn from_json(value: &Value) -> Result<Self, String> {
        value.as_str().map(str::to_string).ok_or_else(|| "expected a string".to_string())
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::String(self.clone()))
    }
}

impl SettingValue for Vec<String> {
    fn from_json(value: &Value) -> Result<Self, String> {
        let items = value.as_array().ok_or_else(|| "expected an array".to_string())?;
        items.iter().map(String::from_json).collect()
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::Array(self.iter().cloned().map(Value::String).collect()))
    }
}

impl SettingValue for BTreeMap<String, String> {
    fn from_json(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or_else(|| "expected an object".to_string())?;
        object
            .iter()
            .map(|(key, item)| Ok((key.clone(), String::from_json(item)?)))
            .collect()
    }
    fn to_json(&self) -> Result<Value, String> {
        Ok(Value::Object(
            self.iter().map(|(key, item)| (key.clone(), Value::String(item.clone()))).collect(),
        ))
    }
}

/// The settings file was unreadable as an editor settings object and was moved aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corrupted {
    pub backup: PathBuf,
    pub reason: String,
}

pub struct Loaded {
    pub settings: Settings,
    /// Set when the existing file was corrupt; its original bytes are at `backup`.
    pub corrupted: Option<Corrupted>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    values: Map<String, Value>,
}

impl Settings {
    /// Empty store, no defaults applied.
    pub fn empty() -> Self {
        Self { values: Map::new() }
    }

    /// Defaults D applies for new users (no file, or a corrupt one).
    pub fn new_user_defaults() -> Self {
        let mut settings = Self::empty();
        settings.values.insert("ViewportZoomMode".into(), Value::from("MousePosition"));
        settings.values.insert("ViewportZoomSpeed".into(), Value::from(5.0));
        settings.values.insert("KeepLayerFolder".into(), Value::from("Ask"));
        settings.values.insert("LayerGroupReplacement".into(), Value::from("Node"));
        settings
    }

    /// Parses a settings document. Anything but a non-empty JSON object is rejected, as in D.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| format!("settings file is not UTF-8: {error}"))?;
        match serde_json::from_str::<Value>(text).map_err(|error| error.to_string())? {
            Value::Object(values) if values.is_empty() => Err("Settings file is empty".to_string()),
            Value::Object(values) => Ok(Self { values }),
            _ => Err("settings file is not a JSON object".to_string()),
        }
    }

    /// Loads `path`. A missing file yields the new-user defaults; a corrupt file is renamed to
    /// `<path>.<UTC timestamp>` (original bytes untouched) and also yields the defaults.
    pub fn load(path: &Path) -> Result<Loaded, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Loaded { settings: Self::new_user_defaults(), corrupted: None });
            }
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        match Self::parse(&bytes) {
            Ok(settings) => Ok(Loaded { settings, corrupted: None }),
            Err(reason) => {
                let mut backup = path.as_os_str().to_owned();
                backup.push(format!(".{}", iso_timestamp_utc()));
                let backup = PathBuf::from(backup);
                fs::rename(path, &backup)
                    .map_err(|error| format!("cannot move corrupt {} aside: {error}", path.display()))?;
                Ok(Loaded { settings: Self::new_user_defaults(), corrupted: Some(Corrupted { backup, reason }) })
            }
        }
    }

    /// Writes the compact JSON object to `<path>.swp` and renames it over `path`.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            ensure_dir(parent)?;
        }
        let mut swap: OsString = path.as_os_str().to_owned();
        swap.push(".swp");
        let swap = PathBuf::from(swap);
        let text = serde_json::to_string(&Value::Object(self.values.clone())).map_err(|error| error.to_string())?;
        fs::write(&swap, text).map_err(|error| format!("cannot write {}: {error}", swap.display()))?;
        fs::rename(&swap, path).map_err(|error| format!("cannot replace {}: {error}", path.display()))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Value or `T::default()` when absent. A present value of the wrong type is an error (D throws).
    pub fn get<T: SettingValue>(&self, name: &str) -> Result<T, String> {
        self.get_or(name, T::default())
    }

    pub fn get_or<T: SettingValue>(&self, name: &str, default: T) -> Result<T, String> {
        match self.values.get(name) {
            Some(value) => T::from_json(value).map_err(|error| format!("setting {name}: {error}")),
            None => Ok(default),
        }
    }

    pub fn set<T: SettingValue>(&mut self, name: &str, value: T) -> Result<(), String> {
        let json = value.to_json().map_err(|error| format!("setting {name}: {error}"))?;
        self.values.insert(name.to_string(), json);
        Ok(())
    }
}

/// `YYYYMMDDTHHMMSS.nnnnnnnnnZ` for the current UTC time.
fn iso_timestamp_utc() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:09}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60,
        now.subsec_nanos()
    )
}

// ---------------------------------------------------------------------------------------------
// Shortcuts
// ---------------------------------------------------------------------------------------------

const MODIFIER_NAMES: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

fn is_valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    match bytes {
        [b'A'..=b'Z'] | [b'0'..=b'9'] => true,
        [b'F', rest @ ..] => matches!(std::str::from_utf8(rest), Ok("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "10" | "11" | "12")),
        _ => matches!(key, "Left" | "Right" | "Up" | "Down"),
    }
}

/// OS-specific text for a canonical modifier name (`Ctrl`, `Alt`, `Shift`, `Super`).
fn modifier_label(platform: Platform, name: &str) -> &str {
    match (platform, name) {
        (Platform::MacOs, "Super") => "\u{eae7}",
        (Platform::MacOs, "Alt") => "\u{eae8}",
        (Platform::MacOs, "Ctrl") => "\u{eae6}",
        _ => name,
    }
}

/// A key chord: exact modifier set plus one key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    /// One of A-Z, 0-9, F1-F12, Left, Right, Up, Down.
    pub key: String,
}

impl Chord {
    fn with_modifier(&mut self, name: &str) -> bool {
        match name {
            "Ctrl" => self.ctrl = true,
            "Alt" => self.alt = true,
            "Shift" => self.shift = true,
            "Super" => self.super_key = true,
            _ => return false,
        }
        true
    }

    fn parse_tokens(text: &str, separator: char, label_of: impl Fn(&str) -> &str) -> Result<Self, String> {
        let tokens: Vec<&str> = text.split(separator).collect();
        let (key, modifiers) = tokens.split_last().expect("split yields at least one token");
        if !is_valid_key(key) {
            return Err(format!("unknown key {key:?} in shortcut {text:?}"));
        }
        let mut chord = Chord { ctrl: false, alt: false, shift: false, super_key: false, key: (*key).to_string() };
        for token in modifiers {
            let name = MODIFIER_NAMES.iter().find(|name| label_of(name) == *token);
            if !name.is_some_and(|name| chord.with_modifier(name)) {
                return Err(format!("unknown modifier {token:?} in shortcut {text:?}"));
            }
        }
        Ok(chord)
    }

    /// Parses the platform-independent source notation used by `_K!"..."`, e.g. `Ctrl-Shift-S`.
    pub fn from_spec(spec: &str) -> Result<Self, String> {
        Self::parse_tokens(spec, '-', |name| name)
    }

    /// Parses a stored shortcut string such as `Ctrl+Shift+S`; macOS uses glyph modifier labels.
    pub fn parse(text: &str, platform: Platform) -> Result<Self, String> {
        Self::parse_tokens(text, '+', |name| modifier_label(platform, name))
    }

    /// Formats in the D order Ctrl, Alt, Shift, Super, key, joined by `+`.
    pub fn format(&self, platform: Platform) -> String {
        let flags = [self.ctrl, self.alt, self.shift, self.super_key];
        let mut parts: Vec<&str> = MODIFIER_NAMES
            .iter()
            .zip(flags)
            .filter(|(_, on)| *on)
            .map(|(name, _)| modifier_label(platform, name))
            .collect();
        parts.push(&self.key);
        parts.join("+")
    }

    /// Applies the macOS "Swap Command and Control" option (`MacSwapCmdCtrl`), which D uses both
    /// when formatting a pressed chord and when matching a stored one. Involutive.
    pub fn swap_ctrl_super(&self) -> Self {
        Chord { ctrl: self.super_key, super_key: self.ctrl, ..self.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub chord: Chord,
    /// Whether holding the key repeats the command.
    pub repeat: bool,
}

/// Built-in bindings: (D `_K!` spec, command id, repeat).
pub const DEFAULT_SHORTCUTS: &[(&str, &str, bool)] = &[
    ("Ctrl-N", "FileCommand.NewFile", false),
    ("Ctrl-O", "FileCommand.ShowOpenFileDialog", false),
    ("Ctrl-S", "FileCommand.ShowSaveFileDialog", false),
    ("Ctrl-Shift-S", "FileCommand.ShowSaveFileAsDialog", false),
    ("Ctrl-Shift-Z", "EditCommand.Redo", true),
    ("Ctrl-Z", "EditCommand.Undo", true),
    ("Ctrl-X", "NodeCommand.CutNode", true),
    ("Ctrl-C", "NodeCommand.CopyNode", true),
    ("Ctrl-V", "NodeCommand.PasteNode", true),
    ("Ctrl-Shift-P", "PaletteCommand.ListCommand", false),
    ("Ctrl-K", "AnimationCommand.KeyAll", false),
    ("Ctrl-Shift-K", "AnimationCommand.UnkeyAll", false),
];

/// Command id to binding. A chord belongs to at most one command and a command has at most one chord.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShortcutRegistry {
    bindings: BTreeMap<String, Binding>,
}

impl ShortcutRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        for (spec, id, repeat) in DEFAULT_SHORTCUTS {
            let chord = Chord::from_spec(spec).expect("default shortcut specs are valid");
            registry.register(id, chord, *repeat);
        }
        registry
    }

    /// Binds `chord` to `command`, replacing the command's old chord. A different command that
    /// already held the chord loses it; its id is returned.
    pub fn register(&mut self, command: &str, chord: Chord, repeat: bool) -> Option<String> {
        let displaced = self.command_for(&chord).filter(|holder| *holder != command).map(str::to_string);
        if let Some(holder) = &displaced {
            self.bindings.remove(holder);
        }
        self.bindings.insert(command.to_string(), Binding { chord, repeat });
        displaced
    }

    /// Removes the command's binding; returns whether there was one.
    pub fn clear(&mut self, command: &str) -> bool {
        self.bindings.remove(command).is_some()
    }

    pub fn binding_for(&self, command: &str) -> Option<&Binding> {
        self.bindings.get(command)
    }

    /// The command currently holding `chord`.
    pub fn command_for(&self, chord: &Chord) -> Option<&str> {
        self.bindings.iter().find(|(_, binding)| binding.chord == *chord).map(|(id, _)| id.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Binding)> {
        self.bindings.iter().map(|(id, binding)| (id.as_str(), binding))
    }

    /// Stores every binding as `Shortcuts: { id: "Ctrl+Shift+S" }`, replacing the old object.
    pub fn save_to(&self, settings: &mut Settings, platform: Platform) -> Result<(), String> {
        let map: BTreeMap<String, String> =
            self.iter().map(|(id, binding)| (id.to_string(), binding.chord.format(platform))).collect();
        settings.set(SHORTCUTS_KEY, map)
    }

    /// Applies stored overrides on top of the current bindings (overrides never repeat, as in D).
    /// Empty strings are ignored. Entries whose text is not a valid chord are skipped and their
    /// ids returned. Errors only when `Shortcuts` is not a string map.
    pub fn load_from(&mut self, settings: &Settings, platform: Platform) -> Result<Vec<String>, String> {
        let stored: BTreeMap<String, String> = settings.get(SHORTCUTS_KEY)?;
        let mut skipped = Vec::new();
        for (id, text) in stored {
            if text.is_empty() {
                continue;
            }
            match Chord::parse(&text, platform) {
                Ok(chord) => {
                    self.clear(&id);
                    self.register(&id, chord, false);
                }
                Err(_) => skipped.push(id),
            }
        }
        Ok(skipped)
    }
}
