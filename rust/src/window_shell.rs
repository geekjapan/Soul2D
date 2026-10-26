//! Window-level behaviour of the D editor that does not depend on a toolkit: the unsaved-changes policy
//! (`io/save.d` `CloseAskHandler`, setting `SaveProjectOnClose`), the window title (`core/window.d`
//! `incRefreshWindowTitle`), the window size settings (`WinW`, `WinH`, `WinMax`) and how dropped files are
//! handled (`panels/viewport.d` file drop).
//!
//! D's drop handler knew `.png .tga .jpeg .jpg` (new Parts) and `.inx` (open); everything else produced a
//! "not supported" error. This port also opens `.inp` and imports `.psd` / `.kra`, because the editor has
//! those operations; the order and "an opened project ends the drop" rules are D's.

use crate::settings::Settings;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "Soul2D";
const CLOSE_KEY: &str = "SaveProjectOnClose";

/// The fixed destinations offered by the editor's address dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressDialogTarget {
    Configuration,
    OnlineDocumentation,
    Soul2dLiveDocumentation,
    BugReport,
    FeatureRequest,
}

impl AddressDialogTarget {
    pub const ALL: [Self; 5] = [Self::Configuration, Self::OnlineDocumentation,
        Self::Soul2dLiveDocumentation, Self::BugReport, Self::FeatureRequest];

    pub fn id(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::OnlineDocumentation => "online_documentation",
            Self::Soul2dLiveDocumentation => "soul2d_live_documentation",
            Self::BugReport => "bug_report",
            Self::FeatureRequest => "feature_request",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.id() == id)
    }
}

/// Open replaces the current address; Copy and Close have no destination argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressDialogOperation {
    Open(AddressDialogTarget),
    Copy,
    Close,
}

impl AddressDialogOperation {
    pub fn from_fields(action: &str, target: Option<&str>) -> Result<Self, String> {
        match action {
            "open" => target.and_then(AddressDialogTarget::from_id).map(Self::Open)
                .ok_or_else(|| "open requires a known address dialog target".to_owned()),
            "copy" | "close" => {
                if target.is_some() {
                    return Err("target is allowed only for open".into());
                }
                Ok(if action == "copy" { Self::Copy } else { Self::Close })
            }
            _ => Err("action must be open, copy or close".into()),
        }
    }
}

/// What to do with unsaved changes when a document is about to be replaced or the window closed.
/// D offers `Ask` and `dontSave`; a stored `Save` was disabled in D to keep users from getting stuck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseSetting {
    Ask,
    DontSave,
}

impl CloseSetting {
    pub fn key(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::DontSave => "dontSave",
        }
    }
}

/// D `incGetSaveProjectOnClose`: an unknown stored value is replaced by `Ask` (and written back).
pub fn close_setting(store: &mut Settings) -> CloseSetting {
    match store.get_or::<String>(CLOSE_KEY, "Ask".to_owned()).as_deref() {
        Ok("dontSave") => CloseSetting::DontSave,
        Ok("Ask") => CloseSetting::Ask,
        _ => {
            let _ = store.set(CLOSE_KEY, "Ask".to_owned());
            CloseSetting::Ask
        }
    }
}

pub fn set_close_setting(store: &mut Settings, setting: CloseSetting) -> Result<(), String> {
    store.set(CLOSE_KEY, setting.key().to_owned())
}

/// The outcome of D `CloseAskHandler.start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Nothing to lose, or the user chose "Don't save": continue (discarding any changes).
    Proceed,
    /// Show the Save / Don't Save / Cancel dialog.
    Ask,
}

pub fn gate(modified: bool, setting: CloseSetting) -> Gate {
    if modified && setting == CloseSetting::Ask {
        Gate::Ask
    } else {
        Gate::Proceed
    }
}

/// The answer to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Save,
    DontSave,
    Cancel,
}

/// Where "Save" writes: the document's own path, else the path typed into the dialog. `None` means the dialog
/// has to ask for one.
pub fn save_target(project: Option<&Path>, typed: &str) -> Option<String> {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Some(typed.to_owned());
    }
    project.and_then(Path::to_str).map(str::to_owned)
}

// ------------------------------------------------------------------------------------------ title

/// D title text: `Soul2D` without a document name, else `Soul2D - <name>` plus ` *` while modified.
pub fn window_title(subtitle: Option<&str>, modified: bool) -> String {
    match subtitle.filter(|name| !name.is_empty()) {
        Some(name) => format!("{APP_NAME} - {name}{}", if modified { " *" } else { "" }),
        None => APP_NAME.to_owned(),
    }
}

/// D `incSetWindowTitle` argument: the project's file name, or "New Project" (`new_label`) before it has a path.
pub fn document_subtitle(project: Option<&Path>, has_document: bool, new_label: &str) -> Option<String> {
    match project.and_then(Path::file_name) {
        Some(name) => Some(name.to_string_lossy().into_owned()),
        None => has_document.then(|| new_label.to_owned()),
    }
}

// ------------------------------------------------------------------------------------------ geometry

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowGeometry {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

const MIN_SIDE: i64 = 320;
const MAX_SIDE: i64 = 16_384;

impl WindowGeometry {
    /// D defaults: 1280x800, not maximised. Stored sizes are clamped so a bad file cannot make a tiny or
    /// absurd window.
    pub fn load(store: &Settings) -> Self {
        let side = |key: &str, default: i64| store.get_or::<i64>(key, default).unwrap_or(default).clamp(MIN_SIDE, MAX_SIDE) as f32;
        Self {
            width: side("WinW", 1280),
            height: side("WinH", 800),
            maximized: store.get_or::<bool>("WinMax", false).unwrap_or(false),
        }
    }

    pub fn store(&self, store: &mut Settings) -> Result<(), String> {
        store.set("WinW", (self.width.round() as i64).clamp(MIN_SIDE, MAX_SIDE))?;
        store.set("WinH", (self.height.round() as i64).clamp(MIN_SIDE, MAX_SIDE))?;
        store.set("WinMax", self.maximized)
    }

    /// Fold in what the window reports this frame. The size is only taken while not maximised, so the next
    /// start restores the normal size under a maximised window.
    pub fn observe(&mut self, inner: Option<(f32, f32)>, maximized: Option<bool>) {
        if let Some(maximized) = maximized {
            self.maximized = maximized;
        }
        if let (Some((width, height)), false) = (inner, self.maximized) {
            if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                self.width = width;
                self.height = height;
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ drop

/// One step of handling a drop, in the order D would carry them out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropAction {
    /// `.inx` / `.inp`: open as the document (through the unsaved-changes dialog).
    Open(PathBuf),
    /// `.psd` / `.kra`: import as a new document (through the unsaved-changes dialog).
    ImportLayers(PathBuf),
    /// `.png .tga .jpg .jpeg`: merge as new Parts (`merge.images`), one undo step for the batch.
    MergeImages(Vec<PathBuf>),
    /// Anything else: D showed "<name> is not supported".
    Unsupported(String),
}

/// D walks the dropped files in order; images are merged where they stand, and a project file ends the
/// walk (`break mainLoop`), so files after it are left alone.
pub fn plan_drops(paths: &[PathBuf]) -> Vec<DropAction> {
    let mut plan = Vec::new();
    let mut images: Vec<PathBuf> = Vec::new();
    let flush = |images: &mut Vec<PathBuf>, plan: &mut Vec<DropAction>| {
        if !images.is_empty() {
            plan.push(DropAction::MergeImages(std::mem::take(images)));
        }
    };
    for path in paths {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        match extension.as_str() {
            "png" | "tga" | "jpeg" | "jpg" => images.push(path.clone()),
            "inx" | "inp" | "cmo3" | "can3" => {
                flush(&mut images, &mut plan);
                plan.push(DropAction::Open(path.clone()));
                return plan;
            }
            "psd" | "kra" => {
                flush(&mut images, &mut plan);
                plan.push(DropAction::ImportLayers(path.clone()));
                return plan;
            }
            _ => {
                flush(&mut images, &mut plan);
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                plan.push(DropAction::Unsupported(name));
            }
        }
    }
    flush(&mut images, &mut plan);
    plan
}

/// The `merge.images` request for dropped images: the Parts go under `parent` (the selected node, as the
/// menu's Merge Image Files does). Paths travel as an array, so a `|` in a name is harmless.
pub fn merge_images_request(paths: &[PathBuf], parent: Option<u64>) -> Result<serde_json::Value, String> {
    let list: Vec<&str> = paths
        .iter()
        .map(|p| p.to_str().ok_or("a dropped file name is not valid UTF-8"))
        .collect::<Result<_, _>>()?;
    let mut request = serde_json::json!({"op": "merge.images", "paths": list});
    if let Some(parent) = parent {
        request["parent"] = serde_json::json!(parent);
    }
    Ok(request)
}
