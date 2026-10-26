//! The PSD depth map import window: choose a PSD, see how its layers map to the model's grids, edit the
//! mapping and the per-layer depth options, read the budget and D's diagnostics, apply.
//!
//! Port of source/soul2d/windows/psddepthmap.d (the Source / Mapping tab and the settings pane) onto the
//! session operations `depth.import_psd.plan` and `depth.import_psd`. The window keeps a [`Draft`] of
//! the dialog state and never touches the document: [`PsdDepthWindow::show`] returns the requests the
//! editor must send, and [`PsdDepthWindow::deliver`] takes the answers back. Like D, every change to the
//! draft re-plans (D's `previewDirty` rebuild), so the table always shows the effect of the draft.
//!
//! Not offered (the session has no operation behind them): the 3D Adjust tab and every preview image or
//! tooltip, "Select/Clear Color Source", "Repair contour band", "Smooth wavy surface", "Fill alpha-depth
//! gaps", "GPU Composition", the dialog's own undo/redo (the import is one history step, undone from the
//! Edit menu). Strings are not routed through the translation
//! catalogue.
// Until the editor's menu opens the window.

use eframe::egui;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const CHANNELS: [&str; 5] = ["AverageRGB", "R", "G", "B", "Luminance"];
const CONVOLUTIONS: [&str; 13] = [
    "Nearest",
    "Box3x3",
    "Box5x5",
    "Gaussian3x3",
    "Gaussian5x5",
    "Median3x3",
    "Frontmost3x3",
    "Backmost3x3",
    "BoxCustom",
    "GaussianCustom",
    "MedianCustom",
    "FrontmostCustom",
    "BackmostCustom",
];
const MISSING_POLICIES: [&str; 4] = ["KeepExisting", "SetZero", "SetBack", "SkipGrid"];
const MIB: f64 = 1024.0 * 1024.0;

// ------------------------------------------------------------------------------------------- draft

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Auto,
    Ignore,
    Node(u64),
}

/// The edits of one PSD layer (D `PsdDepthComposedLayer` fields plus the mapping choice).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerDraft {
    pub target: Target,
    pub enabled: bool,
    /// `None` keeps the PSD's own visibility.
    pub visible: Option<bool>,
    pub depth_enabled: bool,
    pub invert: bool,
    pub depth_scale: f32,
    pub depth_offset: f32,
}

impl Default for LayerDraft {
    fn default() -> Self {
        Self {
            target: Target::Auto,
            enabled: true,
            visible: None,
            depth_enabled: true,
            invert: false,
            depth_scale: 1.0,
            depth_offset: 0.0,
        }
    }
}

impl LayerDraft {
    fn to_json(&self, path: &str) -> Value {
        let mut entry = Map::new();
        entry.insert("layerPath".into(), json!(path));
        match self.target {
            Target::Auto => {}
            Target::Ignore => {
                entry.insert("target".into(), json!("ignore"));
            }
            Target::Node(id) => {
                entry.insert("target".into(), json!(id));
            }
        }
        entry.insert("enabled".into(), json!(self.enabled));
        if let Some(visible) = self.visible {
            entry.insert("visible".into(), json!(visible));
        }
        entry.insert("depthEnabled".into(), json!(self.depth_enabled));
        entry.insert("invert".into(), json!(self.invert));
        entry.insert("depthScale".into(), json!(self.depth_scale));
        entry.insert("depthOffset".into(), json!(self.depth_offset));
        Value::Object(entry)
    }

    /// What a plan row shows for a layer the draft has not edited.
    fn from_row(row: &Value) -> Self {
        Self {
            enabled: row["enabled"].as_bool().unwrap_or(true),
            depth_enabled: row["depthEnabled"].as_bool().unwrap_or(true),
            invert: row["invert"].as_bool().unwrap_or(false),
            depth_scale: row["depthScale"].as_f64().unwrap_or(1.0) as f32,
            depth_offset: row["depthOffset"].as_f64().unwrap_or(0.0) as f32,
            ..Self::default()
        }
    }
}

/// The dialog state: the global settings of D's settings pane and the edits made to layers and targets.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub path: String,
    pub invert: bool,
    pub back_depth: f32,
    pub front_depth: f32,
    pub depth_scale: f32,
    pub channel: usize,
    pub convolution: usize,
    pub custom_radius: i32,
    pub alpha_threshold: f32,
    pub missing_policy: usize,
    pub match_direct_grid_name: bool,
    /// Edited layers by layer path. An entry equal to the default is not sent.
    pub layers: BTreeMap<String, LayerDraft>,
    /// Targets whose "Use" box is off.
    pub disabled_grids: BTreeSet<u64>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            path: String::new(),
            invert: false,
            back_depth: -1.0,
            front_depth: 1.0,
            depth_scale: 1.0,
            channel: 0,
            convolution: 3, // D's default sampling is Gaussian3x3
            custom_radius: 3,
            alpha_threshold: 0.01,
            missing_policy: 0,
            match_direct_grid_name: true,
            layers: BTreeMap::new(),
            disabled_grids: BTreeSet::new(),
        }
    }
}

impl Draft {
    /// The session request for `op` (`depth.import_psd.plan` or `depth.import_psd`).
    pub fn request(&self, op: &str) -> Value {
        let mut request = json!({
            "op": op,
            "path": self.path.trim(),
            "invert": self.invert,
            "backDepth": self.back_depth,
            "frontDepth": self.front_depth,
            "depthScale": self.depth_scale,
            "channel": CHANNELS[self.channel.min(CHANNELS.len() - 1)],
            "convolution": CONVOLUTIONS[self.convolution.min(CONVOLUTIONS.len() - 1)],
            "customRadius": self.custom_radius,
            "alphaThreshold": self.alpha_threshold,
            "missingPolicy": MISSING_POLICIES[self.missing_policy.min(MISSING_POLICIES.len() - 1)],
            "matchDirectGridName": self.match_direct_grid_name,
        });
        let layers: Vec<Value> = self
            .layers
            .iter()
            .filter(|(_, layer)| **layer != LayerDraft::default())
            .map(|(path, layer)| layer.to_json(path))
            .collect();
        if !layers.is_empty() {
            request["layers"] = Value::Array(layers);
        }
        if !self.disabled_grids.is_empty() {
            request["disabledGrids"] = json!(self.disabled_grids);
        }
        request
    }

    /// Drops edits of layers a newer plan no longer lists (the core refuses unknown layer paths).
    fn forget_missing_layers(&mut self, plan: &Value) {
        let known: BTreeSet<&str> = rows(plan).filter_map(|row| row["layerPath"].as_str()).collect();
        self.layers.retain(|path, _| known.contains(path.as_str()));
    }

    fn effective(&self, row: &Value) -> LayerDraft {
        let path = row["layerPath"].as_str().unwrap_or_default();
        self.layers.get(path).cloned().unwrap_or_else(|| LayerDraft::from_row(row))
    }

    fn edit(&mut self, row: &Value) -> &mut LayerDraft {
        let current = self.effective(row);
        self.layers.entry(row["layerPath"].as_str().unwrap_or_default().to_owned()).or_insert(current)
    }
}

// --------------------------------------------------------------------------------- plan accessors

fn rows(plan: &Value) -> impl Iterator<Item = &Value> {
    plan["layers"].as_array().into_iter().flatten()
}

/// D's "problem layer": unmatched, ambiguous or ignored.
pub fn is_problem(row: &Value) -> bool {
    row["problem"].as_bool().unwrap_or(false)
}

/// The target a remap combo currently shows for a row.
pub fn mapping_label(layer: &LayerDraft, plan: &Value) -> String {
    match layer.target {
        Target::Auto => "Auto".to_owned(),
        Target::Ignore => "Ignore".to_owned(),
        Target::Node(id) => plan["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|target| target["id"].as_u64() == Some(id))
            .and_then(|target| target["name"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "Missing Target".to_owned()),
    }
}

pub fn budget_text(plan: &Value) -> String {
    let budget = &plan["budget"];
    format!(
        "Retained image memory: {:.1} MiB of {:.0} MiB ({} layers)",
        budget["retainedBytes"].as_f64().unwrap_or(0.0) / MIB,
        budget["maxRetainedBytes"].as_f64().unwrap_or(0.0) / MIB,
        budget["layerCount"].as_u64().unwrap_or(0),
    )
}

fn range_text(row: &Value) -> String {
    row["depthRange01"].as_f64().map_or_else(|| "-".to_owned(), |range| format!("{range:.3}"))
}

fn grid_status(grid: &Value) -> &'static str {
    if grid["skipped"].as_bool().unwrap_or(false) {
        "Skipped"
    } else {
        "Will Apply"
    }
}

fn apply_summary(report: &Value) -> String {
    format!(
        "Imported depths into {} target(s); {} unmatched layer(s), {} skipped target(s).",
        report["changedGrids"].as_u64().unwrap_or(0),
        report["unmatchedLayers"].as_u64().unwrap_or(0),
        report["skippedGrids"].as_u64().unwrap_or(0),
    )
}

// ------------------------------------------------------------------------------------------ window

#[derive(Default)]
pub struct PsdDepthWindow {
    pub open: bool,
    draft: Draft,
    plan: Option<Value>,
    error: Option<String>,
    summary: Option<String>,
    /// The draft changed (or the document did) since the last plan was requested.
    dirty: bool,
    planning: bool,
    applying: bool,
    /// `snapshot["revision"]` when the last request was queued; a different one means the document moved.
    seen_revision: Option<u64>,
    revision: Option<u64>,
    only_problems: bool,
    selected: Option<String>,
    queued: Vec<Value>,
}

/// Makes a control's accessibility name differ from its (repeated or empty) visible text.
fn named(response: egui::Response, kind: egui::WidgetType, name: String) -> egui::Response {
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(kind, enabled, name.clone()));
    response
}

fn labelled_drag<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    speed: f64,
) -> egui::Response {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.add(egui::DragValue::new(value).range(range).speed(speed)).labelled_by(caption.id)
    })
    .inner
}

fn labelled_combo(ui: &mut egui::Ui, label: &str, names: &[&str], index: &mut usize) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        let combo = egui::ComboBox::from_id_salt(("psd-depth-setting", label))
            .selected_text(names[(*index).min(names.len() - 1)])
            .show_ui(ui, |ui| {
                for (position, name) in names.iter().enumerate() {
                    changed |= ui.selectable_value(index, position, *name).changed();
                }
            });
        combo.response.labelled_by(caption.id);
    });
    changed
}

impl PsdDepthWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    /// Draws the window when it is open and returns the session requests to send. Send each one and give
    /// the answer to [`Self::deliver`].
    pub fn show(&mut self, ctx: &egui::Context, snapshot: &Value) -> Vec<Value> {
        if !self.open {
            return Vec::new();
        }
        let revision = snapshot["revision"].as_u64();
        self.revision = revision;
        if self.plan.is_some() && !self.planning && !self.applying && revision != self.seen_revision {
            self.dirty = true;
        }
        let mut open = true;
        egui::Window::new("PSD Depth Map")
            .id(egui::Id::new("editor_psd_depth_window"))
            .open(&mut open)
            .default_size([760.0, 700.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.contents(ui);
                });
            });
        // The Close button clears `self.open` inside the window; the title bar's close box clears `open`.
        self.open = self.open && open;
        let busy = self.planning || self.applying;
        // Not for a window that was just closed: the plan would be computed for nobody.
        if self.open && self.dirty && self.plan.is_some() && !busy && !self.draft.path.trim().is_empty() {
            self.queue("depth.import_psd.plan");
        }
        std::mem::take(&mut self.queued)
    }

    fn queue(&mut self, op: &str) {
        self.dirty = false;
        self.seen_revision = self.revision;
        self.error = None;
        if op == "depth.import_psd" {
            self.applying = true;
            self.summary = None;
        } else {
            self.planning = true;
        }
        self.queued.push(self.draft.request(op));
    }

    /// Takes the session's answer to a request [`Self::show`] returned.
    pub fn deliver(&mut self, request: &Value, response: &Value) {
        let applying = request["op"] == "depth.import_psd";
        if applying {
            self.applying = false;
        } else {
            self.planning = false;
        }
        if response["succeeded"] != true {
            self.error = Some(response["message"].as_str().unwrap_or("the request failed").to_owned());
            return;
        }
        let report = response["result"].clone();
        if applying {
            self.summary = Some(apply_summary(&report));
            self.dirty = true;
        } else {
            self.draft.forget_missing_layers(&report);
            if self.selected.as_ref().is_some_and(|path| !rows(&report).any(|row| row["layerPath"] == path.as_str())) {
                self.selected = None;
            }
        }
        self.plan = Some(report);
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        let before = self.draft.clone();
        let busy = self.planning || self.applying;
        ui.horizontal(|ui| {
            let caption = ui.label("PSD path");
            ui.add(egui::TextEdit::singleline(&mut self.draft.path).desired_width(360.0)).labelled_by(caption.id);
            if ui.add_enabled(!busy, egui::Button::new("Browse...")).clicked() {
                if let Some(path) = crate::editor_windows::browse(crate::editor_windows::PromptKind::ImportPsd, &self.draft.path) {
                    self.draft.path = path;
                }
            }
            let usable = !self.draft.path.trim().is_empty() && !busy;
            if ui.add_enabled(usable, egui::Button::new("Plan")).clicked() {
                self.queue("depth.import_psd.plan");
            }
            if ui.add_enabled(usable, egui::Button::new("Apply")).clicked() {
                self.queue("depth.import_psd");
            }
            if ui.button("Close").clicked() {
                self.open = false;
            }
        });
        self.settings(ui);
        if busy {
            ui.label(if self.applying { "Applying..." } else { "Planning..." });
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, format!("Preview failed: {error}"));
        }
        if let Some(summary) = &self.summary {
            ui.label(summary);
        }
        if let Some(plan) = self.plan.clone() {
            ui.separator();
            ui.label(budget_text(&plan));
            ui.label(format!(
                "Composition: {}x{}  layers: {}  matched: {}  unmatched: {}  ambiguous: {}",
                plan["document"]["width"],
                plan["document"]["height"],
                plan["layers"].as_array().map_or(0, Vec::len),
                plan["matchedLayers"],
                plan["unmatchedLayers"],
                plan["ambiguousLayers"],
            ));
            for line in plan["diagnostics"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                ui.colored_label(ui.visuals().warn_fg_color, line);
            }
            self.target_table(ui, &plan);
            self.layer_table(ui, &plan);
            self.layer_detail(ui, &plan);
        }
        if self.draft != before {
            self.dirty = true;
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let draft = &mut self.draft;
        egui::CollapsingHeader::new("Settings").default_open(true).show(ui, |ui| {
            ui.checkbox(&mut draft.invert, "Invert Depth").on_hover_text("Default: white is front and black is back.");
            labelled_drag(ui, "Back Depth", &mut draft.back_depth, -10.0..=10.0, 0.01);
            labelled_drag(ui, "Front Depth", &mut draft.front_depth, -10.0..=10.0, 0.01);
            labelled_drag(ui, "Depth Scale", &mut draft.depth_scale, 0.0..=100.0, 0.01)
                .on_hover_text("Imported depth values are multiplied by this scale before applying.");
            labelled_combo(ui, "Channel", &CHANNELS, &mut draft.channel);
            labelled_combo(ui, "Sampling", &CONVOLUTIONS, &mut draft.convolution);
            if CONVOLUTIONS[draft.convolution.min(CONVOLUTIONS.len() - 1)].ends_with("Custom") {
                labelled_drag(ui, "Custom Radius", &mut draft.custom_radius, 1..=64, 0.1);
            }
            labelled_drag(ui, "Alpha Threshold", &mut draft.alpha_threshold, 0.0..=1.0, 0.001);
            labelled_combo(ui, "Missing Vertex Pixel", &MISSING_POLICIES, &mut draft.missing_policy);
            ui.checkbox(&mut draft.match_direct_grid_name, "Direct Grid Name Match")
                .on_hover_text("Also match PSD layer names directly against GridDeformer names.");
        });
        ui.checkbox(&mut self.only_problems, "Only show problem layers");
    }

    fn target_table(&mut self, ui: &mut egui::Ui, plan: &Value) {
        let grids = plan["grids"].as_array().cloned().unwrap_or_default();
        if grids.is_empty() {
            return;
        }
        ui.label("Targets");
        egui::Grid::new("psd_depth_targets").striped(true).show(ui, |ui| {
            for header in ["Use", "Sampled", "Missing", "Coverage", "Min", "Max", "Status"] {
                ui.strong(header);
            }
            ui.end_row();
            for grid in &grids {
                let id = grid["target"].as_u64().unwrap_or(0);
                let name = grid["name"].as_str().unwrap_or("");
                let mut used = !self.draft.disabled_grids.contains(&id);
                if ui.checkbox(&mut used, name).changed() {
                    if used {
                        self.draft.disabled_grids.remove(&id);
                    } else {
                        self.draft.disabled_grids.insert(id);
                    }
                }
                ui.label(grid["sampledVertices"].to_string());
                ui.label(grid["missingVertices"].to_string());
                ui.label(grid["coverageSources"].to_string());
                ui.label(format!("{:.3}", grid["minDepth"].as_f64().unwrap_or(0.0)));
                ui.label(format!("{:.3}", grid["maxDepth"].as_f64().unwrap_or(0.0)));
                ui.label(grid_status(grid));
                ui.end_row();
            }
        });
    }

    fn layer_table(&mut self, ui: &mut egui::Ui, plan: &Value) {
        ui.label("Depth Layers");
        let layers: Vec<Value> = rows(plan).filter(|row| !self.only_problems || is_problem(row)).cloned().collect();
        egui::Grid::new("psd_depth_layers").striped(true).show(ui, |ui| {
            for header in
                ["Layer", "Depth Layer", "Matched Node", "Remap", "Status", "Sampled", "Selected", "Mask px", "Zero px", "Range"]
            {
                ui.strong(header);
            }
            ui.end_row();
            for row in &layers {
                let path = row["layerPath"].as_str().unwrap_or("").to_owned();
                let mut layer = self.draft.effective(row);
                if named(ui.checkbox(&mut layer.enabled, path.as_str()), egui::WidgetType::Checkbox, format!("Use {path}"))
                    .changed()
                {
                    self.draft.edit(row).enabled = layer.enabled;
                }
                let selected = self.selected.as_deref() == Some(path.as_str());
                if ui.selectable_label(selected, row["layerName"].as_str().unwrap_or("")).clicked() {
                    self.selected = Some(path.clone());
                }
                ui.label(row["matchedNodeName"].as_str().unwrap_or("-"));
                self.remap_combo(ui, plan, row, &path, &layer);
                ui.label(row["status"].as_str().unwrap_or(""));
                ui.label(row["sampledVertices"].to_string());
                ui.label(row["selectedVertices"].to_string());
                ui.label(row["maskedPixels"].to_string());
                ui.label(row["zeroPixels"].to_string());
                ui.label(range_text(row));
                ui.end_row();
            }
        });
    }

    fn remap_combo(&mut self, ui: &mut egui::Ui, plan: &Value, row: &Value, path: &str, layer: &LayerDraft) {
        let mut chosen = None;
        let combo = egui::ComboBox::from_id_salt(("psd-depth-map", path))
            .selected_text(mapping_label(layer, plan))
            .show_ui(ui, |ui| {
                if ui.selectable_label(layer.target == Target::Auto, "Auto").clicked() {
                    chosen = Some(Target::Auto);
                }
                if ui.selectable_label(layer.target == Target::Ignore, "Ignore").clicked() {
                    chosen = Some(Target::Ignore);
                }
                ui.separator();
                for target in plan["targets"].as_array().into_iter().flatten() {
                    let Some(id) = target["id"].as_u64() else { continue };
                    let name = target["name"].as_str().filter(|name| !name.is_empty()).unwrap_or("(unnamed)");
                    if ui.selectable_label(layer.target == Target::Node(id), name).clicked() {
                        chosen = Some(Target::Node(id));
                    }
                }
            });
        named(combo.response, egui::WidgetType::ComboBox, format!("Remap {path}"));
        if let Some(target) = chosen {
            self.draft.edit(row).target = target;
        }
    }

    fn layer_detail(&mut self, ui: &mut egui::Ui, plan: &Value) {
        let Some(path) = self.selected.clone() else { return };
        let Some(row) = rows(plan).find(|row| row["layerPath"] == path.as_str()) else { return };
        let mut layer = self.draft.effective(row);
        let mut edited = false;
        ui.separator();
        ui.label(format!(
            "Layer: {}  {}x{}  ({}, {})",
            path, row["width"], row["height"], row["left"], row["top"]
        ));
        let mut shown = layer.visible.unwrap_or_else(|| row["visible"].as_bool().unwrap_or(true));
        if ui.checkbox(&mut shown, "Show").changed() {
            layer.visible = Some(shown);
            edited = true;
        }
        edited |= ui
            .checkbox(&mut layer.depth_enabled, "Enable Depth")
            .on_hover_text("When disabled, use the nearest enabled depth below at each document coordinate.")
            .changed();
        if layer.depth_enabled {
            edited |= ui.checkbox(&mut layer.invert, "Invert Layer Depth").changed();
            edited |= labelled_drag(ui, "Z Scale", &mut layer.depth_scale, -100.0..=100.0, 0.01).changed();
            edited |= labelled_drag(ui, "Z Offset", &mut layer.depth_offset, -100.0..=100.0, 0.01).changed();
            if ui.button("Reset Z").clicked() {
                layer.depth_scale = 1.0;
                layer.depth_offset = 0.0;
                edited = true;
            }
        } else {
            ui.label("Attached to depth below");
        }
        if edited {
            let target = self.draft.effective(row).target;
            *self.draft.edit(row) = LayerDraft { target, ..layer };
        }
    }
}

// ------------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use std::collections::HashMap;

    #[test]
    fn the_default_draft_asks_for_d_defaults_and_no_overrides() {
        let request = Draft { path: " /tmp/depth.psd ".into(), ..Draft::default() }.request("depth.import_psd.plan");
        assert_eq!(
            request,
            json!({
                "op": "depth.import_psd.plan", "path": "/tmp/depth.psd", "invert": false,
                "backDepth": -1.0, "frontDepth": 1.0, "depthScale": 1.0, "channel": "AverageRGB",
                "convolution": "Gaussian3x3", "customRadius": 3, "alphaThreshold": 0.01f32,
                "missingPolicy": "KeepExisting", "matchDirectGridName": true,
            })
        );
    }

    #[test]
    fn edits_become_layer_entries_and_default_entries_are_not_sent() {
        let mut draft = Draft { path: "a.psd".into(), channel: 4, convolution: 8, missing_policy: 2, ..Draft::default() };
        draft.disabled_grids.insert(6);
        draft.layers.insert("/Face".into(), LayerDraft { target: Target::Node(4), depth_scale: 2.0, ..LayerDraft::default() });
        draft.layers.insert("/Eye".into(), LayerDraft { target: Target::Ignore, visible: Some(false), ..LayerDraft::default() });
        draft.layers.insert("/Untouched".into(), LayerDraft::default());
        let request = draft.request("depth.import_psd");
        assert_eq!(request["channel"], "Luminance");
        assert_eq!(request["convolution"], "BoxCustom");
        assert_eq!(request["missingPolicy"], "SetBack");
        assert_eq!(request["disabledGrids"], json!([6]));
        assert_eq!(
            request["layers"],
            json!([
                {"layerPath": "/Eye", "target": "ignore", "enabled": true, "visible": false,
                 "depthEnabled": true, "invert": false, "depthScale": 1.0, "depthOffset": 0.0},
                {"layerPath": "/Face", "target": 4, "enabled": true, "depthEnabled": true,
                 "invert": false, "depthScale": 2.0, "depthOffset": 0.0},
            ])
        );
    }

    fn sample_plan() -> Value {
        json!({
            "document": {"width": 3, "height": 2}, "matchedLayers": 1, "unmatchedLayers": 1, "ambiguousLayers": 0,
            "budget": {"retainedBytes": 3145728, "maxRetainedBytes": 268435456, "layerCount": 2},
            "diagnostics": ["unused-depth-layer: /Extra"],
            "targets": [{"id": 4, "name": "FaceGrid", "type": "GridDeformer"}],
            "grids": [{"target": 4, "name": "FaceGrid", "enabled": true, "skipped": false, "coverageSources": 0,
                       "sampledVertices": 6, "missingVertices": 0, "minDepth": 0.6, "maxDepth": 0.6}],
            "layers": [
                {"layerPath": "/Face", "layerName": "Face", "status": "Matched", "problem": false, "target": 4,
                 "matchedNodeName": "Face", "enabled": true, "depthEnabled": true, "invert": false,
                 "depthScale": 1.0, "depthOffset": 0.0, "visible": true, "sampledVertices": 6, "selectedVertices": 6,
                 "maskedPixels": 6, "zeroPixels": 0, "depthRange01": 0.0, "width": 3, "height": 2, "left": 0, "top": 0},
                {"layerPath": "/Extra", "layerName": "Extra", "status": "Unmatched", "problem": true, "target": null,
                 "matchedNodeName": null, "enabled": true, "depthEnabled": true, "invert": false,
                 "depthScale": 1.0, "depthOffset": 0.0, "visible": true, "sampledVertices": 0, "selectedVertices": 0,
                 "maskedPixels": 6, "zeroPixels": 0, "depthRange01": null, "width": 3, "height": 2, "left": 0, "top": 0},
            ],
        })
    }

    #[test]
    fn plan_accessors_format_what_d_shows() {
        let plan = sample_plan();
        assert_eq!(budget_text(&plan), "Retained image memory: 3.0 MiB of 256 MiB (2 layers)");
        let problems: Vec<&str> = rows(&plan).filter(|row| is_problem(row)).filter_map(|row| row["layerPath"].as_str()).collect();
        assert_eq!(problems, ["/Extra"]);
        assert_eq!(range_text(&plan["layers"][0]), "0.000");
        assert_eq!(range_text(&plan["layers"][1]), "-");
        assert_eq!(grid_status(&plan["grids"][0]), "Will Apply");
        assert_eq!(mapping_label(&LayerDraft::default(), &plan), "Auto");
        assert_eq!(mapping_label(&LayerDraft { target: Target::Node(4), ..LayerDraft::default() }, &plan), "FaceGrid");
        assert_eq!(mapping_label(&LayerDraft { target: Target::Node(9), ..LayerDraft::default() }, &plan), "Missing Target");
        assert_eq!(
            apply_summary(&json!({"changedGrids": 2, "unmatchedLayers": 1, "skippedGrids": 0})),
            "Imported depths into 2 target(s); 1 unmatched layer(s), 0 skipped target(s)."
        );
    }

    #[test]
    fn a_newer_plan_drops_edits_of_layers_it_no_longer_lists() {
        let mut draft = Draft::default();
        draft.layers.insert("/Face".into(), LayerDraft { enabled: false, ..LayerDraft::default() });
        draft.layers.insert("/Gone".into(), LayerDraft { enabled: false, ..LayerDraft::default() });
        draft.forget_missing_layers(&sample_plan());
        assert_eq!(draft.layers.keys().collect::<Vec<_>>(), ["/Face"]);
    }

    // ------------------------------------------------------------------------- headless frames

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

    struct Rig {
        ctx: egui::Context,
        tree: Tree,
        snapshot: Value,
    }

    impl Rig {
        fn new() -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            Self { ctx, tree: Tree::default(), snapshot: json!({"revision": 1}) }
        }

        fn frame(&mut self, window: &mut PsdDepthWindow, events: Vec<egui::Event>) -> (Vec<Value>, Out) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 800.0))),
                events,
                ..Default::default()
            };
            let mut requests = Vec::new();
            let output = self.ctx.run(input, |ctx| requests = window.show(ctx, &self.snapshot));
            (requests, self.tree.absorb(&output))
        }

        /// Two frames so the layout settles, then the nodes.
        fn settle(&mut self, window: &mut PsdDepthWindow) -> Out {
            self.frame(window, Vec::new());
            self.frame(window, Vec::new()).1
        }

        fn click(&mut self, window: &mut PsdDepthWindow, role: Role, label: &str) -> Vec<Value> {
            let laid_out = self.settle(window);
            let at = center(&laid_out, role, label);
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let (mut requests, _) = self.frame(window, vec![egui::Event::PointerMoved(at), press(true), press(false)]);
            requests.extend(self.frame(window, Vec::new()).0);
            requests
        }
    }

    fn center(output: &Out, role: Role, label: &str) -> egui::Pos2 {
        let (_, node) = output
            .iter()
            .find(|(_, node)| node.role() == role && node.label().or_else(|| node.value()).is_some_and(|name| name == label))
            .unwrap_or_else(|| {
                let have: Vec<_> = output.iter().map(|(_, n)| (n.role(), n.label().map(str::to_owned))).collect();
                panic!("no {role:?} named {label:?}; have {have:?}")
            });
        let bounds = node.bounds().expect("bounds");
        egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32)
    }

    fn texts(output: &Out) -> Vec<String> {
        output.iter().flat_map(|(_, node)| [node.label(), node.value()]).flatten().map(str::to_owned).collect()
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

    fn assert_all_named(output: &Out) -> usize {
        let mut count = 0;
        for (_, node) in output {
            if CONTROLS.contains(&node.role()) {
                count += 1;
                let named = node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty();
                assert!(named, "unlabelled {:?} control {:?}", node.role(), node.value());
            }
        }
        count
    }

    fn open_window_with_plan() -> (Rig, PsdDepthWindow) {
        let mut window = PsdDepthWindow::new();
        window.open();
        window.draft.path = "/tmp/depth.psd".into();
        let request = window.draft.request("depth.import_psd.plan");
        window.planning = true;
        window.deliver(&request, &json!({"succeeded": true, "result": sample_plan()}));
        let mut rig = Rig::new();
        window.seen_revision = Some(1);
        window.revision = Some(1);
        window.dirty = false;
        window.selected = Some("/Face".into());
        rig.settle(&mut window);
        (rig, window)
    }

    #[test]
    fn every_control_has_an_accessibility_name_and_the_plan_is_shown() {
        let (mut rig, mut window) = open_window_with_plan();
        let output = rig.settle(&mut window);
        for expected in [
            "PSD path", "Plan", "Apply", "Invert Depth", "Back Depth", "Sampling", "Missing Vertex Pixel",
            "Only show problem layers", "Retained image memory: 3.0 MiB of 256 MiB (2 layers)",
            "unused-depth-layer: /Extra", "Use /Face", "Use /Extra", "Remap /Face", "Remap /Extra",
            "FaceGrid", "Will Apply", "Show", "Enable Depth", "Z Scale", "Reset Z", "Unmatched",
        ] {
            assert!(has(&output, expected), "missing {expected:?} in {:?}", texts(&output));
        }
        assert!(assert_all_named(&output) >= 20);
    }

    #[test]
    fn the_problem_filter_hides_matched_layers() {
        let (mut rig, mut window) = open_window_with_plan();
        window.only_problems = true;
        let output = rig.settle(&mut window);
        assert!(has(&output, "Use /Extra") && !has(&output, "Use /Face"), "{:?}", texts(&output));
    }

    #[test]
    fn plan_apply_and_edits_emit_the_session_requests() {
        let (mut rig, mut window) = open_window_with_plan();
        let requests = rig.click(&mut window, Role::Button, "Plan");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "depth.import_psd.plan");
        assert_eq!(requests[0]["path"], "/tmp/depth.psd");
        assert!(window.planning);
        window.deliver(&requests[0], &json!({"succeeded": true, "result": sample_plan()}));
        assert!(!window.planning);

        // Unticking "Use /Face" edits the draft and re-plans with the override.
        let requests = rig.click(&mut window, Role::CheckBox, "Use /Face");
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(requests[0]["op"], "depth.import_psd.plan");
        assert_eq!(
            requests[0]["layers"],
            json!([{"layerPath": "/Face", "enabled": false, "depthEnabled": true,
                    "invert": false, "depthScale": 1.0, "depthOffset": 0.0}])
        );
        window.deliver(&requests[0], &json!({"succeeded": true, "result": sample_plan()}));

        let requests = rig.click(&mut window, Role::Button, "Apply");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "depth.import_psd");
        assert_eq!(requests[0]["layers"][0]["enabled"], false);
        assert!(window.applying);
        window.deliver(&requests[0], &json!({"succeeded": true, "result": {"changedGrids": 1, "unmatchedLayers": 1, "skippedGrids": 0}}));
        assert_eq!(window.summary.as_deref(), Some("Imported depths into 1 target(s); 1 unmatched layer(s), 0 skipped target(s)."));
    }

    #[test]
    fn the_layer_detail_edits_the_selected_layer_and_replans() {
        let (mut rig, mut window) = open_window_with_plan();
        let requests = rig.click(&mut window, Role::CheckBox, "Enable Depth");
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(
            requests[0]["layers"],
            json!([{"layerPath": "/Face", "enabled": true, "depthEnabled": false,
                    "invert": false, "depthScale": 1.0, "depthOffset": 0.0}])
        );
        window.deliver(&requests[0], &json!({"succeeded": true, "result": sample_plan()}));
        let output = rig.settle(&mut window);
        assert!(has(&output, "Attached to depth below"));
        assert!(!has(&output, "Z Scale"), "the Z controls are hidden while depth is off");
    }

    #[test]
    fn a_document_change_replans_the_open_window() {
        let (mut rig, mut window) = open_window_with_plan();
        assert!(rig.frame(&mut window, Vec::new()).0.is_empty());
        rig.snapshot = json!({"revision": 2});
        let (requests, _) = rig.frame(&mut window, Vec::new());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "depth.import_psd.plan");
        assert!(rig.frame(&mut window, Vec::new()).0.is_empty(), "one plan per change while it is in flight");
    }

    #[test]
    fn errors_are_shown_and_the_previous_plan_stays() {
        let (mut rig, mut window) = open_window_with_plan();
        let request = window.draft.request("depth.import_psd.plan");
        window.planning = true;
        window.deliver(&request, &json!({"succeeded": false, "message": "PSD layer not found: /Nope"}));
        let output = rig.settle(&mut window);
        assert!(has(&output, "Preview failed: PSD layer not found: /Nope"));
        assert!(has(&output, "Use /Face"), "the last good plan is still listed");
    }

    #[test]
    fn the_close_button_closes_the_window() {
        let (mut rig, mut window) = open_window_with_plan();
        assert!(window.open);
        rig.click(&mut window, Role::Button, "Close");
        assert!(!window.open, "the Close button left the window open");
    }

    #[test]
    fn a_window_closed_while_its_draft_changed_asks_for_no_plan() {
        // The frames of a click on Close; `dirty_at` marks the draft changed just before that frame.
        fn close(dirty_at: Option<usize>) -> (Vec<bool>, Vec<Vec<Value>>) {
            let (mut rig, mut window) = open_window_with_plan();
            let at = center(&rig.settle(&mut window), Role::Button, "Close");
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let frames = [vec![egui::Event::PointerMoved(at), press(true), press(false)], Vec::new()];
            let mut seen = (Vec::new(), Vec::new());
            for (index, events) in frames.into_iter().enumerate() {
                window.dirty |= dirty_at == Some(index);
                seen.1.push(rig.frame(&mut window, events).0);
                seen.0.push(window.open);
            }
            seen
        }
        let closing = close(None).0.iter().position(|open| !open).expect("the click closes the window");
        let (open, requests) = close(Some(closing));
        assert!(!open[closing]);
        assert_eq!(requests[closing], Vec::<Value>::new(), "the closed window asked for a plan");
    }

    #[test]
    fn a_closed_window_draws_and_asks_for_nothing() {
        let mut window = PsdDepthWindow::new();
        let mut rig = Rig::new();
        let (requests, _) = rig.frame(&mut window, Vec::new());
        assert!(requests.is_empty());
    }
}
