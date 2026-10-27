//! The Live2D source-form editor of a CMO3 document (port of `drawLive2dAuthoringEditor` and
//! `ngRustCoreLive2d{Vertex,Opacity}EditRequest` in `panels/rustcore.d`): pick an ArtMesh or Warp source and
//! one of its primary forms, then edit one vertex position or the form opacity (also for ArtPath). Each Apply is one
//! `live2d.form.set_vertex` / `live2d.form.set_opacity` request guarded by `expectedRevision`, so a document
//! that changed since the draft was loaded is refused instead of overwritten.
//!
//! The same panel edits the stored settings of the selected source and form (issue #144): draw order and colours
//! (`live2d.form.set`), rotation angle, origin, scale and reflection, Glue intensity, and the blend mode, clipping
//! inversion, offscreen drawing and clipping masks of the source (`live2d.source.set`). ArtPath stored metadata
//! includes closure, division count, listed brush, clipping and primary-form draw order and opacity, without point editing.
//! A request names only the
//! settings that differ from the loaded ones, so the file is asked to store nothing the user did not change.

use eframe::egui;
use serde_json::{json, Map, Value};
use soul2d_core::editor_controller::EditorController;

/// The colorComposition and alphaComposition names `live2d.source.set` accepts (`native_blend::Mode::parse`).
const COLOR_COMPOSITIONS: [&str; 17] = [
    "NORMAL", "ADD", "ADD_R2", "ADD_GLOW", "DARKEN", "MULTIPLY_R2", "COLOR_BURN", "LINEAR_BURN", "LIGHTEN", "SCREEN",
    "COLOR_DODGE", "OVERLAY", "SOFTLIGHT", "HARDLIGHT", "LINEAR_LIGHT", "HSL_HUE", "HSL_COLOR",
];
const ALPHA_COMPOSITIONS: [&str; 5] = ["OVER", "ATOP", "OUT", "CONJOINT", "DISJOINT"];

/// ArtMesh and Warp sources have vertices to edit; ArtPath has form opacity; other kinds have stored settings only.
pub fn editable_source(source: &Value) -> bool {
    matches!(source["data"]["type"].as_str(), Some("ArtMesh" | "Warp" | "Part" | "Rotation" | "Glue" | "ArtPath"))
}

fn source_name(source: &Value) -> &str {
    ["name", "id", "guid"].into_iter().filter_map(|key| source[key].as_str())
        .find(|value| !value.trim().is_empty()).unwrap_or("Source")
}

fn source_label(source: &Value) -> String {
    format!("{} [{}]", source_name(source), source["data"]["type"].as_str().unwrap_or(""))
}

fn form_label(form: &Value) -> String {
    let guid = form["guid"].as_str().unwrap_or("");
    form["coordinateType"].as_str().map_or_else(|| guid.to_owned(), |coordinate| format!("{guid} ({coordinate})"))
}

/// The forms of `source` that can be edited: same data type as the source. ArtMesh and Warp forms need positions
/// and a coordinate type, Rotation and ArtPath forms a coordinate type; Part and Glue forms have neither.
pub fn editable_forms(source: &Value) -> Vec<&Value> {
    let source_type = source["data"]["type"].as_str().unwrap_or_default();
    source["forms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|form| {
            form["data"]["type"].as_str() == Some(source_type)
                && match source_type {
                    "ArtMesh" | "Warp" => form["data"].get("positions").is_some() && form["coordinateType"].is_string(),
                    "Rotation" => form["coordinateType"].is_string(),
                    "ArtPath" => form["coordinateType"].as_str().is_some_and(|coordinate| !coordinate.is_empty()),
                    _ => true,
                }
        })
        .collect()
}

/// Part and Glue forms have no coordinate type, so the empty string stands for none.
fn same_form(form: &Value, guid: &str, coordinate: &str) -> bool {
    form["guid"].as_str() == Some(guid) && form["coordinateType"].as_str().unwrap_or_default() == coordinate
}

pub fn vertex_request(
    source: &str,
    form: &str,
    coordinate: &str,
    index: usize,
    position: [f64; 2],
    revision: u64,
) -> Result<Value, String> {
    check_identity(source, form, coordinate)?;
    if !position.iter().all(|n| n.is_finite()) {
        return Err("Live2D source coordinates must be finite".into());
    }
    Ok(json!({
        "op": "live2d.form.set_vertex", "sourceGuid": source, "formGuid": form, "coordinateType": coordinate,
        "vertexIndex": index, "position": position, "expectedRevision": revision,
    }))
}

pub fn opacity_request(source: &str, form: &str, coordinate: &str, opacity: f64, revision: u64) -> Result<Value, String> {
    check_identity(source, form, coordinate)?;
    if !opacity.is_finite() {
        return Err("Live2D opacity must be finite".into());
    }
    Ok(json!({
        "op": "live2d.form.set_opacity", "sourceGuid": source, "formGuid": form, "coordinateType": coordinate,
        "opacity": opacity, "expectedRevision": revision,
    }))
}

/// What a form without a stored draw order shows, as Cubism's default.
const DEFAULT_DRAW_ORDER: i64 = 500;

/// The stored settings of one primary form that `live2d.form.set` edits, as the draft holds them.
#[derive(Clone, Default, PartialEq)]
pub struct FormSettings {
    draw_order: i64,
    multiply: [f64; 4],
    screen: [f64; 4],
    angle: f64,
    origin: [f64; 2],
    scale: f64,
    reflect_x: bool,
    reflect_y: bool,
    intensity: f64,
}

impl FormSettings {
    fn from_form(form: &Value) -> Self {
        let data = &form["data"];
        let number = |value: &Value, default: f64| value.as_f64().filter(|n| n.is_finite()).unwrap_or(default);
        let channels = |value: &Value, default: [f64; 4]| {
            let mut out = default;
            for (slot, channel) in out.iter_mut().zip(value.as_array().into_iter().flatten()) {
                *slot = number(channel, *slot);
            }
            out
        };
        Self {
            draw_order: form["drawOrder"].as_i64().unwrap_or(DEFAULT_DRAW_ORDER),
            multiply: channels(&form["multiplyColor"], [1.0; 4]),
            screen: channels(&form["screenColor"], [0.0, 0.0, 0.0, 1.0]),
            angle: number(&data["angle"], 0.0),
            origin: [number(&data["origin"][0], 0.0), number(&data["origin"][1], 0.0)],
            scale: number(&data["scale"], 1.0),
            reflect_x: data["reflectX"].as_bool().unwrap_or(false),
            reflect_y: data["reflectY"].as_bool().unwrap_or(false),
            intensity: number(&data["intensity"], 1.0),
        }
    }
}

/// The stored settings of one supported source that `live2d.source.set` edits, as the draft holds them.
#[derive(Clone, Default, PartialEq)]
pub struct SourceSettings {
    color_composition: String,
    alpha_composition: String,
    invert_clipping_mask: bool,
    use_offscreen: bool,
    clip_guids: Vec<String>,
    closed: bool,
    division_count: u32,
    brush_guid: String,
}

impl SourceSettings {
    fn from_source(source: &Value) -> Self {
        let data = &source["data"];
        Self {
            color_composition: data["colorComposition"].as_str().unwrap_or("NORMAL").to_owned(),
            alpha_composition: data["alphaComposition"].as_str().unwrap_or("OVER").to_owned(),
            invert_clipping_mask: data["invertClippingMask"].as_bool().unwrap_or(false),
            use_offscreen: data["useOffscreen"].as_bool().unwrap_or(false),
            clip_guids: data["clipGuids"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
            closed: data["closed"].as_bool().unwrap_or(false),
            division_count: data["divisionCount"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or_default(),
            brush_guid: data["brushGuid"].as_str().unwrap_or_default().to_owned(),
        }
    }
}

/// One `live2d.form.set` request with the settings of `draft` that differ from `original`. `coordinate` is empty for
/// the Part and Glue forms, which have none.
pub fn form_set_request(
    source: &str,
    form: &str,
    coordinate: &str,
    kind: &str,
    draft: &FormSettings,
    original: &FormSettings,
    revision: u64,
) -> Result<Value, String> {
    if source.is_empty() || form.is_empty() {
        return Err("Live2D source and form GUIDs are required".into());
    }
    if matches!(kind, "ArtMesh" | "Rotation" | "ArtPath") && coordinate.is_empty() {
        return Err("Live2D coordinate type is required".into());
    }
    let mut numbers = draft.multiply.iter().chain(&draft.screen).chain(&draft.origin).chain([&draft.angle, &draft.scale, &draft.intensity]);
    if kind != "ArtPath" && !numbers.all(|n| n.is_finite()) {
        return Err("Live2D form settings must be finite".into());
    }
    let mut set = Map::new();
    match kind {
        "ArtPath" => {
            if draft.draw_order != original.draw_order {
                set.insert("drawOrder".into(), json!(draft.draw_order));
            }
        }
        "ArtMesh" | "Part" => {
            if kind == "ArtMesh" && draft.draw_order != original.draw_order {
                set.insert("drawOrder".into(), json!(draft.draw_order));
            }
            for (key, new, old) in
                [("multiplyColor", &draft.multiply, &original.multiply), ("screenColor", &draft.screen, &original.screen)]
            {
                if new != old {
                    // The alpha channel goes along only when it changed, so an unchanged one is not written.
                    let sent: &[f64] = if new[3] == old[3] { &new[..3] } else { &new[..] };
                    set.insert(key.into(), json!(sent));
                }
            }
        }
        "Rotation" => {
            if draft.angle != original.angle {
                set.insert("angle".into(), json!(draft.angle));
            }
            if draft.origin != original.origin {
                set.insert("origin".into(), json!(draft.origin));
            }
            if draft.scale != original.scale {
                set.insert("scale".into(), json!(draft.scale));
            }
            if draft.reflect_x != original.reflect_x {
                set.insert("reflectX".into(), json!(draft.reflect_x));
            }
            if draft.reflect_y != original.reflect_y {
                set.insert("reflectY".into(), json!(draft.reflect_y));
            }
        }
        "Glue" => {
            if draft.intensity != original.intensity {
                set.insert("intensity".into(), json!(draft.intensity));
            }
        }
        _ => return Err(format!("{kind} forms have no stored settings")),
    }
    if set.is_empty() {
        return Err("No Live2D form setting changed".into());
    }
    let mut request = json!({"op": "live2d.form.set", "sourceGuid": source, "formGuid": form, "expectedRevision": revision});
    if !coordinate.is_empty() {
        request["coordinateType"] = json!(coordinate);
    }
    request.as_object_mut().expect("a JSON object").extend(set);
    Ok(request)
}

/// One `live2d.source.set` request with the settings of `draft` that differ from `original`.
pub fn source_set_request(
    guid: &str,
    kind: &str,
    draft: &SourceSettings,
    original: &SourceSettings,
    revision: u64,
) -> Result<Value, String> {
    if guid.is_empty() {
        return Err("Live2D source GUID is required".into());
    }
    if !matches!(kind, "ArtMesh" | "Part" | "ArtPath") {
        return Err(format!("{kind} sources have no stored settings"));
    }
    let mut set = Map::new();
    if kind != "ArtPath" && draft.color_composition != original.color_composition {
        set.insert("colorComposition".into(), json!(draft.color_composition));
    }
    if kind != "ArtPath" && draft.alpha_composition != original.alpha_composition {
        set.insert("alphaComposition".into(), json!(draft.alpha_composition));
    }
    if draft.invert_clipping_mask != original.invert_clipping_mask {
        set.insert("invertClippingMask".into(), json!(draft.invert_clipping_mask));
    }
    if kind == "Part" && draft.use_offscreen != original.use_offscreen {
        set.insert("useOffscreen".into(), json!(draft.use_offscreen));
    }
    if draft.clip_guids != original.clip_guids {
        set.insert("clipGuids".into(), json!(draft.clip_guids));
    }
    if kind == "ArtPath" {
        if draft.closed != original.closed {
            set.insert("closed".into(), json!(draft.closed));
        }
        if draft.division_count != original.division_count {
            set.insert("divisionCount".into(), json!(draft.division_count));
        }
        if draft.brush_guid != original.brush_guid {
            set.insert("brushGuid".into(), json!(draft.brush_guid));
        }
    }
    if set.is_empty() {
        return Err("No Live2D source setting changed".into());
    }
    let mut request = json!({"op": "live2d.source.set", "guid": guid, "expectedRevision": revision});
    request.as_object_mut().expect("a JSON object").extend(set);
    Ok(request)
}

fn check_identity(source: &str, form: &str, coordinate: &str) -> Result<(), String> {
    if source.is_empty() || form.is_empty() {
        return Err("Live2D source and form GUIDs are required".into());
    }
    if coordinate.is_empty() {
        return Err("Live2D coordinate type is required".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Half {
    Form,
    Source,
}

/// The drafts of the stored settings of the selected source and form. The two halves are applied separately and
/// share the revision they were loaded at.
#[derive(Default)]
struct Stored {
    /// The selection the drafts were loaded for: source, form and coordinate type.
    key: String,
    loaded: bool,
    revision: u64,
    form: FormSettings,
    form_original: FormSettings,
    source: SourceSettings,
    source_original: SourceSettings,
}

impl Stored {
    fn is_dirty(&self) -> bool {
        self.form != self.form_original || self.source != self.source_original
    }

    fn load_form(&mut self, form: Option<&Value>) {
        self.form = form.map(FormSettings::from_form).unwrap_or_default();
        self.form_original = self.form.clone();
    }

    fn load_source(&mut self, source: &Value) {
        self.source = SourceSettings::from_source(source);
        self.source_original = self.source.clone();
    }
}

#[derive(Default)]
pub struct FormEditor {
    source: String,
    form: String,
    coordinate: String,
    vertex: usize,
    loaded: bool,
    dirty: bool,
    conflict: bool,
    error: String,
    /// The revision the draft was loaded at; Apply sends it as `expectedRevision`.
    revision: u64,
    opacity_mode: bool,
    position: [f64; 2],
    original_position: [f64; 2],
    opacity: f64,
    original_opacity: f64,
    status: String,
    stored: Stored,
}

impl FormEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A different document: forget the selection and any draft.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn has_draft(&self) -> bool {
        self.dirty || self.stored.is_dirty()
    }

    #[cfg(test)]
    pub fn mark_dirty_for_test(&mut self) {
        self.dirty = true;
    }

    #[cfg(test)]
    pub fn fail_for_test(&mut self, error: &str) {
        self.error = error.to_owned();
    }

    #[cfg(test)]
    pub fn state(&self) -> (bool, bool, &str) {
        (self.dirty, self.conflict, self.error.as_str())
    }

    fn load_draft(&mut self, form: &Value, revision: u64) {
        self.error.clear();
        if form["data"]["type"] == "ArtPath" {
            self.opacity_mode = true;
        } else {
            let positions = form["data"]["positions"].as_array().filter(|p| !p.is_empty() && p.len() % 2 == 0);
            let Some(positions) = positions else {
                self.loaded = false;
                self.error = "This form has no editable vertex positions".into();
                return;
            };
            self.vertex = self.vertex.min(positions.len() / 2 - 1);
            let at = self.vertex * 2;
            let (Some(x), Some(y)) = (positions[at].as_f64(), positions[at + 1].as_f64()) else {
                self.loaded = false;
                self.error = "This form has invalid vertex coordinates".into();
                return;
            };
            (self.position, self.original_position) = ([x, y], [x, y]);
        }
        let Some(opacity) = form["opacity"].as_f64().filter(|o| o.is_finite()) else {
            self.loaded = false;
            self.error = "This form has invalid opacity".into();
            return;
        };
        (self.opacity, self.original_opacity) = (opacity, opacity);
        self.revision = revision;
        self.loaded = true;
        self.dirty = false;
        self.conflict = false;
    }

    fn select_source(&mut self, guid: &str) {
        self.source = guid.to_owned();
        self.form.clear();
        self.coordinate.clear();
        self.vertex = 0;
        self.opacity_mode = false;
        self.loaded = false;
        self.dirty = false;
        self.conflict = false;
        self.error.clear();
    }

    fn select_form(&mut self, guid: &str, coordinate: &str) {
        self.form = guid.to_owned();
        self.coordinate = coordinate.to_owned();
        self.vertex = 0;
        self.loaded = false;
        self.dirty = false;
        self.conflict = false;
        self.error.clear();
    }

    /// Returns true when the session changed.
    pub fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController) -> bool {
        let snapshot = controller.snapshot().clone();
        let model = &snapshot["model"];
        let revision = snapshot["revision"].as_u64().unwrap_or(0);
        ui.label("Live2D source forms");
        let sources: Vec<&Value> = model["sources"].as_array().into_iter().flatten().filter(|s| editable_source(s)).collect();
        if !model["sources"].is_array() {
            ui.label("No native source records are available");
            return false;
        }
        if sources.is_empty() {
            ui.label("No editable ArtMesh, Warp, Part, Rotation, Glue or ArtPath sources are available");
            return false;
        }
        if self.loaded && self.revision != revision {
            if self.dirty {
                self.conflict = true;
                self.error = "Document changed elsewhere. Your draft is preserved; reload before applying.".into();
            } else {
                self.loaded = false;
                self.error.clear();
            }
        }
        let masks: Vec<(String, String)> = sources
            .iter()
            .filter(|s| s["data"]["type"] == "ArtMesh")
            .map(|s| (s["guid"].as_str().unwrap_or_default().to_owned(), source_name(s).to_owned()))
            .collect();
        let brushes = model["brushes"].as_array().map(Vec::as_slice).unwrap_or_default();
        let editing = self.dirty || self.stored.is_dirty();
        let mut selected = sources.iter().copied().find(|s| s["guid"].as_str() == Some(self.source.as_str()));
        if selected.is_none() {
            if editing {
                self.conflict = true;
                self.error = "Selected source changed elsewhere. Your draft is preserved; reload before applying.".into();
            } else {
                selected = Some(sources[0]);
                let guid = sources[0]["guid"].as_str().unwrap_or_default().to_owned();
                self.select_source(&guid);
            }
        }
        let mut chosen_source = None;
        ui.add_enabled_ui(!editing, |ui| {
            egui::ComboBox::from_label("Source")
                .selected_text(selected.map_or_else(|| "Source unavailable".to_owned(), source_label))
                .show_ui(ui, |ui| {
                    for source in &sources {
                        let guid = source["guid"].as_str().unwrap_or_default();
                        if ui.selectable_label(guid == self.source, source_label(source)).clicked() {
                            chosen_source = Some(guid.to_owned());
                        }
                    }
                });
        });
        if let Some(guid) = chosen_source {
            self.select_source(&guid);
            selected = sources.iter().copied().find(|s| s["guid"].as_str() == Some(guid.as_str()));
        }
        let forms = selected.map(editable_forms).unwrap_or_default();
        let (current_form, current_coordinate) = (self.form.clone(), self.coordinate.clone());
        let matches = move |form: &Value| same_form(form, &current_form, &current_coordinate);
        let mut selected_form = forms.iter().copied().find(|form| matches(form));
        if selected_form.is_none() && !forms.is_empty() {
            if editing {
                self.conflict = true;
                self.error = "Selected form changed elsewhere. Your draft is preserved; reload before applying.".into();
            } else {
                let first = forms[0];
                self.select_form(first["guid"].as_str().unwrap_or_default(), first["coordinateType"].as_str().unwrap_or_default());
                selected_form = Some(first);
            }
        }
        let mut chosen_form = None;
        ui.add_enabled_ui(!editing && selected.is_some(), |ui| {
            egui::ComboBox::from_label("Form")
                .selected_text(selected_form.map_or_else(|| "Form unavailable".to_owned(), form_label))
                .show_ui(ui, |ui| {
                    for form in &forms {
                        let is_current = matches(*form);
                        if ui.selectable_label(is_current, form_label(form)).clicked() {
                            chosen_form = Some(*form);
                        }
                    }
                });
        });
        if let Some(form) = chosen_form {
            self.select_form(form["guid"].as_str().unwrap_or_default(), form["coordinateType"].as_str().unwrap_or_default());
            selected_form = Some(form);
        }
        let Some(form) = selected_form else {
            ui.label(if forms.is_empty() { "This source has no editable primary forms" } else { "Selected form is unavailable" });
            self.notices(ui);
            // Source settings do not require a primary form.
            let changed = selected.is_some_and(|source| self.stored_ui(ui, controller, source, None, revision, &masks, brushes));
            if ui.button("Reload forms").clicked() {
                self.reset();
            }
            return changed;
        };
        // ArtPath opacity is independent of its control points.
        let artpath = form["data"]["type"] == "ArtPath";
        let vertex_form = form["data"].get("positions").is_some();
        if (vertex_form || artpath) && !self.loaded {
            self.load_draft(form, revision);
        }
        self.notices(ui);
        let mut changed = false;
        let mut apply_main = false;
        if self.loaded {
            let artmesh = selected.is_some_and(|s| s["data"]["type"] == "ArtMesh");
            if !artmesh && !artpath && !self.dirty {
                self.opacity_mode = false;
            }
            if !artpath {
                ui.add_enabled_ui(!self.dirty && !self.conflict && artmesh, |ui| {
                    ui.checkbox(&mut self.opacity_mode, "Edit opacity");
                });
                let count = form["data"]["positions"].as_array().map_or(0, |p| p.len() / 2);
                ui.add_enabled_ui(!self.dirty && !self.conflict && !self.opacity_mode, |ui| {
                    let mut index = self.vertex as i64;
                    ui.horizontal(|ui| {
                        let caption = ui.label("Vertex index");
                        let range = 0..=(count.max(1) as i64 - 1);
                        if ui.add(egui::DragValue::new(&mut index).range(range)).labelled_by(caption.id).changed() {
                            self.vertex = index.max(0) as usize;
                            self.loaded = false;
                            self.load_draft(form, revision);
                        }
                    });
                });
            }
            ui.add_enabled_ui(!self.conflict, |ui| {
                if self.opacity_mode {
                    ui.horizontal(|ui| {
                        let caption = ui.label("Opacity");
                        let response = ui.add(egui::DragValue::new(&mut self.opacity).speed(0.01).max_decimals(6)).labelled_by(caption.id);
                        if response.changed() {
                            self.dirty = self.opacity != self.original_opacity;
                            self.error.clear();
                        }
                    });
                } else {
                    for (axis, label) in [(0, "Source X"), (1, "Source Y")] {
                        ui.horizontal(|ui| {
                            let caption = ui.label(label);
                            let response =
                                ui.add(egui::DragValue::new(&mut self.position[axis]).speed(0.01).max_decimals(4)).labelled_by(caption.id);
                            if response.changed() {
                                self.dirty = self.position != self.original_position;
                                self.error.clear();
                            }
                        });
                    }
                }
            });
            let apply = if self.opacity_mode { "Apply opacity" } else { "Apply vertex" };
            if ui.add_enabled(self.dirty && !self.conflict, egui::Button::new(apply)).clicked() {
                apply_main = true;
            }
        }
        if let Some(source) = selected {
            changed |= self.stored_ui(ui, controller, source, Some(form), revision, &masks, brushes);
        }
        if ui.button("Reload form").clicked() {
            self.loaded = false;
            self.dirty = false;
            self.conflict = false;
            self.error.clear();
            self.status.clear();
            self.stored.loaded = false;
            apply_main = false;
        }
        // Finish consuming this snapshot before a main edit advances the revision.
        if apply_main {
            changed |= self.apply(controller);
        }
        changed
    }

    fn notices(&self, ui: &mut egui::Ui) {
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, self.error.as_str());
        }
        if !self.status.is_empty() {
            ui.label(self.status.as_str());
        }
    }

    /// Loads the drafts of the stored settings when the selection changed, or when the document did and no draft
    /// would be lost; a draft the document outran is kept and marked as a conflict.
    fn sync_stored(&mut self, source: &Value, form: Option<&Value>, revision: u64) {
        let key = format!("{}|{}|{}", self.source, self.form, self.coordinate);
        let stored = &mut self.stored;
        if stored.loaded && stored.key == key && stored.revision != revision && stored.is_dirty() {
            self.conflict = true;
            self.error = "Document changed elsewhere. Your draft is preserved; reload before applying.".into();
        } else if !stored.loaded || stored.key != key || stored.revision != revision {
            stored.load_form(form);
            stored.load_source(source);
            (stored.key, stored.revision, stored.loaded) = (key, revision, true);
        }
    }

    /// The stored settings of the selected source and, when there is one, form. Returns true when the session changed.
    fn stored_ui(
        &mut self,
        ui: &mut egui::Ui,
        controller: &mut EditorController,
        source: &Value,
        form: Option<&Value>,
        revision: u64,
        masks: &[(String, String)],
        brushes: &[Value],
    ) -> bool {
        let kind = source["data"]["type"].as_str().unwrap_or_default();
        let form_settings = form.is_some() && matches!(kind, "ArtMesh" | "Part" | "Rotation" | "Glue" | "ArtPath");
        let source_settings = matches!(kind, "ArtMesh" | "Part" | "ArtPath");
        if !form_settings && !source_settings {
            return false;
        }
        if kind == "ArtPath" && (source["data"]["closed"].as_bool().is_none()
            || source["data"]["divisionCount"].as_u64().and_then(|n| u32::try_from(n).ok()).is_none()
            || source["data"]["brushGuid"].as_str().is_none())
        {
            self.error = "ArtPath stored metadata is invalid".into();
            ui.colored_label(egui::Color32::LIGHT_RED, self.error.as_str());
            return false;
        }
        self.sync_stored(source, form, revision);
        ui.separator();
        ui.label("Stored settings");
        let (own, mut apply) = (self.source.clone(), None);
        ui.add_enabled_ui(!self.conflict, |ui| {
            let stored = &mut self.stored;
            if form_settings {
                form_fields(ui, kind, &mut stored.form);
                if ui.add_enabled(stored.form != stored.form_original, egui::Button::new("Apply form settings")).clicked() {
                    apply = Some(Half::Form);
                }
            }
            if source_settings {
                source_fields(ui, kind, &mut stored.source, masks, &own, brushes);
                if ui.add_enabled(stored.source != stored.source_original, egui::Button::new("Apply source settings")).clicked() {
                    apply = Some(Half::Source);
                }
            }
        });
        apply.is_some_and(|half| self.apply_stored(controller, half, kind))
    }

    fn apply_stored(&mut self, controller: &mut EditorController, half: Half, kind: &str) -> bool {
        let stored = &self.stored;
        let request = match half {
            Half::Form => form_set_request(&self.source, &self.form, &self.coordinate, kind, &stored.form, &stored.form_original, stored.revision),
            Half::Source => source_set_request(&self.source, kind, &stored.source, &stored.source_original, stored.revision),
        };
        match send(controller, request) {
            Ok(()) => {
                // The edit was guarded by the loaded revision, so only the applied half has to be read back; the
                // other half keeps its draft on the new revision.
                let snapshot = controller.snapshot().clone();
                // The vertex/opacity draft is untouched by a settings edit; keep it on the new revision.
                if self.loaded {
                    self.revision = snapshot["revision"].as_u64().unwrap_or(0);
                }
                let source = snapshot["model"]["sources"].as_array().into_iter().flatten().find(|s| s["guid"].as_str() == Some(self.source.as_str()));
                if let Some(source) = source {
                    match half {
                        Half::Form => {
                            let form = editable_forms(source).into_iter().find(|form| same_form(form, &self.form, &self.coordinate));
                            self.stored.load_form(form);
                        }
                        Half::Source => self.stored.load_source(source),
                    }
                    self.stored.revision = snapshot["revision"].as_u64().unwrap_or(0);
                } else {
                    self.stored.loaded = false;
                }
                self.error.clear();
                self.status = "Live2D settings updated".into();
                true
            }
            Err(message) => {
                self.conflict |= message.contains("revision conflict");
                self.error = message;
                self.status.clear();
                false
            }
        }
    }

    fn apply(&mut self, controller: &mut EditorController) -> bool {
        let request = if self.opacity_mode {
            opacity_request(&self.source, &self.form, &self.coordinate, self.opacity, self.revision)
        } else {
            vertex_request(&self.source, &self.form, &self.coordinate, self.vertex, self.position, self.revision)
        };
        match send(controller, request) {
            Ok(()) => {
                self.revision = controller.snapshot()["revision"].as_u64().unwrap_or(0);
                self.loaded = false;
                self.dirty = false;
                self.conflict = false;
                // A settings draft is untouched by a vertex/opacity edit; keep it on the new revision.
                if self.stored.loaded {
                    self.stored.revision = self.revision;
                }
                self.error.clear();
                self.status = "Live2D form updated".into();
                true
            }
            Err(message) => {
                self.conflict |= message.contains("revision conflict");
                self.error = message;
                false
            }
        }
    }
}

fn send(controller: &mut EditorController, request: Result<Value, String>) -> Result<(), String> {
    request.and_then(|request| {
        let response = controller.core_request(&request);
        if response["succeeded"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(response["message"].as_str().unwrap_or("Live2D form edit failed").to_owned())
        }
    })
}

/// A labelled drag value; the caller compares the draft with the loaded one to see an edit.
fn drag<N: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, value: &mut N, speed: f64, range: std::ops::RangeInclusive<N>) {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        let drag = egui::DragValue::new(value).speed(speed).range(range).max_decimals(6).clamp_existing_to_range(false);
        ui.add(drag).labelled_by(caption.id);
    });
}

fn channels(ui: &mut egui::Ui, name: &str, values: &mut [f64; 4]) {
    for (value, channel) in values.iter_mut().zip(["R", "G", "B", "A"]) {
        drag(ui, &format!("{name} {channel}"), value, 0.01, 0.0..=1.0);
    }
}

fn form_fields(ui: &mut egui::Ui, kind: &str, draft: &mut FormSettings) {
    if matches!(kind, "ArtMesh" | "ArtPath") {
        drag(ui, "Draw order", &mut draft.draw_order, 1.0, 0..=1000);
    }
    if matches!(kind, "ArtMesh" | "Part") {
        channels(ui, "Multiply color", &mut draft.multiply);
        channels(ui, "Screen color", &mut draft.screen);
    }
    if kind == "Rotation" {
        drag(ui, "Angle", &mut draft.angle, 0.1, -1.0e6..=1.0e6);
        drag(ui, "Origin X", &mut draft.origin[0], 0.1, -1.0e6..=1.0e6);
        drag(ui, "Origin Y", &mut draft.origin[1], 0.1, -1.0e6..=1.0e6);
        drag(ui, "Scale", &mut draft.scale, 0.01, 1.0e-6..=1.0e6);
        ui.checkbox(&mut draft.reflect_x, "Reflect X");
        ui.checkbox(&mut draft.reflect_y, "Reflect Y");
    }
    if kind == "Glue" {
        drag(ui, "Intensity", &mut draft.intensity, 0.01, 0.0..=1.0);
    }
}

fn source_fields(ui: &mut egui::Ui, kind: &str, draft: &mut SourceSettings, masks: &[(String, String)], own: &str, brushes: &[Value]) {
    if kind == "ArtPath" {
        ui.checkbox(&mut draft.closed, "Closed");
        drag(ui, "Division count", &mut draft.division_count, 1.0, 0..=u32::MAX);
        let label = |brush: &Value| format!("{} [{}]", brush["name"].as_str().filter(|name| !name.is_empty())
            .or_else(|| brush["id"].as_str()).unwrap_or("Brush"), brush["guid"].as_str().unwrap_or_default());
        let selected = brushes.iter().find(|brush| brush["guid"] == draft.brush_guid)
            .map(&label).unwrap_or_else(|| draft.brush_guid.clone());
        ui.add_enabled_ui(!brushes.is_empty(), |ui| {
            egui::ComboBox::from_label("Brush").selected_text(selected).show_ui(ui, |ui| {
                for brush in brushes {
                    if let Some(guid) = brush["guid"].as_str() {
                        ui.selectable_value(&mut draft.brush_guid, guid.to_owned(), label(brush));
                    }
                }
            });
        });
    } else {
        let choices = [
            ("Color composition", &COLOR_COMPOSITIONS[..], &mut draft.color_composition),
            ("Alpha composition", &ALPHA_COMPOSITIONS[..], &mut draft.alpha_composition),
        ];
        for (label, names, value) in choices {
            egui::ComboBox::from_label(label).selected_text(value.as_str()).show_ui(ui, |ui| {
                for name in names {
                    ui.selectable_value(value, (*name).to_owned(), *name);
                }
            });
        }
    }
    ui.checkbox(&mut draft.invert_clipping_mask, "Invert clipping mask");
    if kind == "Part" {
        ui.checkbox(&mut draft.use_offscreen, "Use offscreen");
    }
    ui.label("Clipping masks");
    egui::ScrollArea::vertical().max_height(160.0).id_salt("clipping-masks").show(ui, |ui| {
        for (guid, name) in masks.iter().filter(|(guid, _)| guid.as_str() != own) {
            let mut on = draft.clip_guids.contains(guid);
            if ui.checkbox(&mut on, name.as_str()).changed() {
                if on {
                    draft.clip_guids.push(guid.clone());
                } else {
                    draft.clip_guids.retain(|clip| clip != guid);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::{assert_all_named, cmo3_bytes, has, temp_dir, texts, Harness};
    use egui::accesskit::Role;

    #[test]
    fn requests_carry_identity_and_the_expected_revision_and_refuse_bad_input() {
        assert_eq!(
            vertex_request("mesh-guid", "form-guid", "Canvas", 1, [30.25, -40.5], 8).unwrap(),
            json!({"op": "live2d.form.set_vertex", "sourceGuid": "mesh-guid", "formGuid": "form-guid", "coordinateType": "Canvas",
                "vertexIndex": 1, "position": [30.25, -40.5], "expectedRevision": 8})
        );
        assert_eq!(
            opacity_request("mesh-guid", "form-guid", "Canvas", 0.5, 3).unwrap(),
            json!({"op": "live2d.form.set_opacity", "sourceGuid": "mesh-guid", "formGuid": "form-guid", "coordinateType": "Canvas",
                "opacity": 0.5, "expectedRevision": 3})
        );
        assert_eq!(vertex_request("", "f", "Canvas", 0, [0.0, 0.0], 1).unwrap_err(), "Live2D source and form GUIDs are required");
        assert_eq!(vertex_request("m", "f", "", 0, [0.0, 0.0], 1).unwrap_err(), "Live2D coordinate type is required");
        assert_eq!(vertex_request("m", "f", "Canvas", 0, [f64::NAN, 0.0], 1).unwrap_err(), "Live2D source coordinates must be finite");
        assert_eq!(opacity_request("m", "f", "Canvas", f64::INFINITY, 1).unwrap_err(), "Live2D opacity must be finite");
    }

    #[test]
    fn source_labels_fall_back_to_nonempty_identifiers_without_changing_names() {
        for (name, id, guid, expected) in [
            (Some("Face"), Some("mesh"), Some("g"), "Face"),
            (Some("  顔  "), Some("mesh"), Some("g"), "  顔  "),
            (Some(""), Some("mesh"), Some("g"), "mesh"),
            (Some(" \t"), Some("mesh"), Some("g"), "mesh"),
            (None, Some("mesh"), Some("g"), "mesh"),
            (Some(""), Some(" "), Some("g"), "g"),
            (None, None, Some("g"), "g"),
            (Some(""), Some(" "), Some("\t"), "Source"),
            (None, None, None, "Source"),
        ] {
            let source = json!({"name": name, "id": id, "guid": guid, "data": {"type": "ArtMesh"}});
            assert_eq!(source_name(&source), expected);
            assert_eq!(source_label(&source), format!("{expected} [ArtMesh]"));
        }
    }

    #[test]
    fn sources_and_forms_are_filtered_like_d() {
        let source = json!({"guid": "a", "data": {"type": "ArtMesh"}, "forms": [
            {"guid": "ok", "coordinateType": "Canvas", "data": {"type": "ArtMesh", "positions": [1, 2]}},
            {"guid": "other-type", "coordinateType": "Canvas", "data": {"type": "Warp", "positions": [1, 2]}},
            {"guid": "no-positions", "coordinateType": "Canvas", "data": {"type": "ArtMesh"}},
            {"guid": "no-coords", "data": {"type": "ArtMesh", "positions": [1, 2]}}]});
        assert!(editable_source(&source) && editable_source(&json!({"data": {"type": "Warp"}})));
        for kind in ["Part", "Rotation", "Glue", "ArtPath"] {
            assert!(editable_source(&json!({"data": {"type": kind}})), "{kind}");
        }
        assert!(!editable_source(&json!({})));
        let forms = editable_forms(&source);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0]["guid"], "ok");
        // Part and Glue forms have no coordinate type and no positions; a Rotation form needs a coordinate type.
        let part = json!({"data": {"type": "Part"}, "forms": [{"guid": "p", "data": {"type": "Part"}}]});
        let glue = json!({"data": {"type": "Glue"}, "forms": [{"guid": "g", "data": {"type": "Glue", "intensity": 1}}]});
        let rotation = json!({"data": {"type": "Rotation"}, "forms": [
            {"guid": "r", "coordinateType": "DeformerLocal", "data": {"type": "Rotation"}},
            {"guid": "bare", "data": {"type": "Rotation"}}]});
        assert_eq!((editable_forms(&part).len(), editable_forms(&glue).len(), editable_forms(&rotation).len()), (1, 1, 1));
        assert_eq!(form_label(&part["forms"][0]), "p");
        assert_eq!(form_label(&rotation["forms"][0]), "r (DeformerLocal)");
        assert!(same_form(&part["forms"][0], "p", "") && !same_form(&rotation["forms"][0], "r", ""));
    }

    fn mesh_form() -> Value {
        json!({"guid": "f", "coordinateType": "Canvas", "drawOrder": 502, "multiplyColor": [1, 0.5, 0.25, 1],
            "screenColor": [0, 0, 0, 1], "data": {"type": "ArtMesh", "positions": [1, 2]}})
    }

    fn rotation_form() -> Value {
        json!({"guid": "r", "coordinateType": "DeformerLocal", "data": {"type": "Rotation", "angle": 15, "origin": [10, 20],
            "scale": 1, "reflectX": false, "reflectY": false}})
    }

    #[test]
    fn form_set_requests_name_only_the_changed_settings_of_the_kind() {
        let original = FormSettings::from_form(&mesh_form());
        assert_eq!((original.draw_order, original.multiply, original.screen), (502, [1.0, 0.5, 0.25, 1.0], [0.0, 0.0, 0.0, 1.0]));
        let mut draft = original.clone();
        draft.draw_order = 600;
        draft.multiply[0] = 0.5;
        draft.angle = 9.0;
        assert_eq!(
            form_set_request("m", "f", "Canvas", "ArtMesh", &draft, &original, 4).unwrap(),
            json!({"op": "live2d.form.set", "sourceGuid": "m", "formGuid": "f", "coordinateType": "Canvas",
                "expectedRevision": 4, "drawOrder": 600, "multiplyColor": [0.5, 0.5, 0.25]})
        );
        draft.screen = [0.1, 0.2, 0.3, 0.5];
        let request = form_set_request("m", "f", "Canvas", "ArtMesh", &draft, &original, 4).unwrap();
        assert_eq!(request["screenColor"], json!([0.1, 0.2, 0.3, 0.5]), "a changed alpha channel goes along");

        let original = FormSettings::from_form(&rotation_form());
        let mut draft = original.clone();
        (draft.angle, draft.origin, draft.reflect_x) = (-30.5, [12.0, 24.0], true);
        assert_eq!(
            form_set_request("m", "r", "DeformerLocal", "Rotation", &draft, &original, 2).unwrap(),
            json!({"op": "live2d.form.set", "sourceGuid": "m", "formGuid": "r", "coordinateType": "DeformerLocal",
                "expectedRevision": 2, "angle": -30.5, "origin": [12.0, 24.0], "reflectX": true})
        );
        draft.scale = 2.0;
        draft.reflect_y = true;
        let request = form_set_request("m", "r", "DeformerLocal", "Rotation", &draft, &original, 2).unwrap();
        assert_eq!((&request["scale"], &request["reflectY"]), (&json!(2.0), &json!(true)));

        // Part and Glue forms have no coordinate type, and a setting of another kind is not sent.
        let part = FormSettings::from_form(&json!({"guid": "p", "data": {"type": "Part"}}));
        let mut draft = part.clone();
        draft.draw_order = 700;
        assert_eq!(form_set_request("m", "p", "", "Part", &draft, &part, 1).unwrap_err(), "No Live2D form setting changed");
        draft.screen[1] = 1.0;
        assert_eq!(
            form_set_request("m", "p", "", "Part", &draft, &part, 1).unwrap(),
            json!({"op": "live2d.form.set", "sourceGuid": "m", "formGuid": "p", "expectedRevision": 1, "screenColor": [0.0, 1.0, 0.0]})
        );
        let glue = FormSettings::from_form(&json!({"guid": "g", "data": {"type": "Glue", "intensity": 1.5}}));
        let mut draft = glue.clone();
        draft.intensity = 0.25;
        assert_eq!(
            form_set_request("m", "g", "", "Glue", &draft, &glue, 3).unwrap(),
            json!({"op": "live2d.form.set", "sourceGuid": "m", "formGuid": "g", "expectedRevision": 3, "intensity": 0.25})
        );
    }

    #[test]
    fn form_set_requests_refuse_missing_identity_nothing_changed_and_non_finite_numbers() {
        let original = FormSettings::from_form(&mesh_form());
        let mut draft = original.clone();
        assert_eq!(form_set_request("m", "f", "Canvas", "ArtMesh", &draft, &original, 1).unwrap_err(), "No Live2D form setting changed");
        draft.draw_order = 1;
        assert_eq!(form_set_request("", "f", "Canvas", "ArtMesh", &draft, &original, 1).unwrap_err(), "Live2D source and form GUIDs are required");
        assert_eq!(form_set_request("m", "", "Canvas", "ArtMesh", &draft, &original, 1).unwrap_err(), "Live2D source and form GUIDs are required");
        assert_eq!(form_set_request("m", "f", "", "ArtMesh", &draft, &original, 1).unwrap_err(), "Live2D coordinate type is required");
        assert_eq!(form_set_request("m", "f", "Canvas", "Warp", &draft, &original, 1).unwrap_err(), "Warp forms have no stored settings");
        draft.multiply[2] = f64::NAN;
        assert_eq!(form_set_request("m", "f", "Canvas", "ArtMesh", &draft, &original, 1).unwrap_err(), "Live2D form settings must be finite");
    }

    #[test]
    fn source_set_requests_name_only_the_changed_settings_of_the_kind() {
        let mesh = json!({"guid": "m", "data": {"type": "ArtMesh", "colorComposition": "NORMAL", "alphaComposition": "OVER",
            "invertClippingMask": false, "clipGuids": ["a"]}});
        let original = SourceSettings::from_source(&mesh);
        assert_eq!((original.color_composition.as_str(), original.clip_guids.clone()), ("NORMAL", vec!["a".to_owned()]));
        let mut draft = original.clone();
        draft.color_composition = "SCREEN".into();
        draft.invert_clipping_mask = true;
        draft.clip_guids.push("b".into());
        draft.use_offscreen = true;
        assert_eq!(
            source_set_request("m", "ArtMesh", &draft, &original, 7).unwrap(),
            json!({"op": "live2d.source.set", "guid": "m", "expectedRevision": 7, "colorComposition": "SCREEN",
                "invertClippingMask": true, "clipGuids": ["a", "b"]}),
            "useOffscreen belongs to a Part only"
        );
        draft.clip_guids.clear();
        draft.alpha_composition = "ATOP".into();
        let request = source_set_request("m", "Part", &draft, &original, 7).unwrap();
        assert_eq!((&request["useOffscreen"], &request["clipGuids"], &request["alphaComposition"]), (&json!(true), &json!([]), &json!("ATOP")));

        assert_eq!(source_set_request("m", "ArtMesh", &original, &original, 1).unwrap_err(), "No Live2D source setting changed");
        assert_eq!(source_set_request("", "ArtMesh", &draft, &original, 1).unwrap_err(), "Live2D source GUID is required");
        assert_eq!(source_set_request("m", "Glue", &draft, &original, 1).unwrap_err(), "Glue sources have no stored settings");
        let bare = SourceSettings::from_source(&json!({"data": {"type": "Part"}}));
        assert_eq!((bare.color_composition.as_str(), bare.alpha_composition.as_str(), bare.use_offscreen), ("NORMAL", "OVER", false));
    }

    #[test]
    fn the_rotation_and_glue_fields_are_named_controls() {
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1500.0);
        let mut rotation = FormSettings::from_form(&rotation_form());
        let mut glue = FormSettings::from_form(&json!({"guid": "g", "data": {"type": "Glue", "intensity": 0.5}}));
        let mut draw = |kind: &str, draft: &mut FormSettings| {
            for _ in 0..2 {
                harness.frame(Vec::new(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| form_fields(ui, kind, draft));
                });
            }
            harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| form_fields(ui, kind, draft));
            })
        };
        let output = draw("Rotation", &mut rotation);
        for label in ["Angle", "Origin X", "Origin Y", "Scale"] {
            assert!(texts(&output).iter().any(|t| t == label), "{label}");
        }
        for label in ["Reflect X", "Reflect Y"] {
            assert!(has(&output, Role::CheckBox, label), "{label}");
        }
        assert!(assert_all_named(&output, "rotation settings") >= 6);
        assert_eq!((rotation.angle, rotation.origin, rotation.scale), (15.0, [10.0, 20.0], 1.0), "drawing leaves the draft alone");
        let output = draw("Glue", &mut glue);
        assert!(texts(&output).iter().any(|t| t == "Intensity"));
        assert!(assert_all_named(&output, "glue settings") >= 1);
        assert_eq!(glue.intensity, 0.5);
    }

    struct Rig {
        controller: EditorController,
        editor: FormEditor,
        harness: Harness,
    }

    impl Rig {
        fn frame(&mut self, events: Vec<egui::Event>) -> crate::editor_test_support::Out {
            let (editor, controller) = (&mut self.editor, &mut self.controller);
            self.harness.frame(events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, controller); });
            })
        }

        fn choose(&mut self, combo: &str, choice: &str) {
            let output = {
                let (editor, controller) = (&mut self.editor, &mut self.controller);
                self.harness.click(Role::ComboBox, combo, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, controller); });
                })
            };
            let role = output.iter().find(|(_, node)| crate::editor_test_support::name_of(&output, node).as_deref() == Some(choice))
                .map(|(_, node)| node.role()).unwrap_or_else(|| panic!("missing choice {choice}"));
            self.click(role, choice);
        }

        fn drag(&mut self, label: &str, dx: f32) {
            let output = self.draw();
            let from = crate::editor_test_support::center(&output, Role::SpinButton, label);
            let to = from + egui::vec2(dx, 0.0);
            let press = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
            self.frame(vec![egui::Event::PointerMoved(from + egui::vec2(dx/2.0, 0.0))]);
            self.frame(vec![egui::Event::PointerMoved(to)]);
            self.frame(vec![press(to, false)]);
            self.frame(Vec::new());
        }

        fn artpath(name: &str, xml: &str) -> Self {
            let mut rig = Self::new(name);
            let path = temp_dir(name).join("path.cmo3");
            std::fs::write(&path, crate::editor_test_support::caff_with_xml(xml)).unwrap();
            let opened = rig.controller.core_request(&json!({"op":"open", "path":path}));
            assert_eq!(opened["succeeded"], true, "{opened}");
            rig.editor.reset();
            rig.editor.select_source("path-guid");
            rig
        }

        fn new(name: &str) -> Self {
            let dir = temp_dir(name);
            std::fs::write(dir.join("fixture.cmo3"), cmo3_bytes()).unwrap();
            let mut controller = EditorController::new();
            let opened = controller.core_request(&json!({"op": "open", "path": dir.join("fixture.cmo3").to_str().unwrap()}));
            assert_eq!(opened["succeeded"], true, "{opened}");
            let mut harness = Harness::new();
            harness.screen = egui::vec2(900.0, 1500.0);
            Self { controller, editor: FormEditor::new(), harness }
        }

        fn draw(&mut self) -> crate::editor_test_support::Out {
            let (editor, controller) = (&mut self.editor, &mut self.controller);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor.show(ui, controller);
                });
            });
            let (editor, controller) = (&mut self.editor, &mut self.controller);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor.show(ui, controller);
                });
            })
        }

        fn click(&mut self, role: Role, label: &str) {
            let (editor, controller) = (&mut self.editor, &mut self.controller);
            self.harness.click(role, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor.show(ui, controller);
                });
            });
        }

        fn click_observing(&mut self, label: &str, mut observe: impl FnMut(&FormEditor, &EditorController)) {
            let (editor, controller) = (&mut self.editor, &mut self.controller);
            self.harness.click(Role::Button, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor.show(ui, controller);
                    observe(editor, controller);
                });
            });
        }

        fn form(&self) -> Value {
            self.controller.snapshot()["model"]["sources"][0]["forms"][0].clone()
        }

        fn revision(&self) -> u64 {
            self.controller.snapshot()["revision"].as_u64().unwrap()
        }
    }

    #[test]
    fn the_editor_lists_the_source_and_form_with_named_controls_and_loads_the_first_vertex() {
        let mut rig = Rig::new("forms-names");
        let output = rig.draw();
        assert!(texts(&output).iter().any(|text| text == "mesh [ArtMesh]"), "{:?}", texts(&output));
        for label in ["Source", "Form"] {
            assert!(has(&output, Role::ComboBox, label), "{label}");
        }
        assert!(has(&output, Role::CheckBox, "Edit opacity"));
        for label in ["Apply vertex", "Reload form"] {
            assert!(has(&output, Role::Button, label), "{label}: {:?}", texts(&output));
        }
        for label in ["Vertex index", "Source X", "Source Y"] {
            assert!(texts(&output).iter().any(|t| t == label), "{label}");
        }
        assert!(assert_all_named(&output, "form editor") >= 6);
        assert_eq!(rig.editor.position, [1.0, 2.0]);
        assert_eq!((rig.editor.opacity, rig.editor.coordinate.as_str()), (0.75, "Canvas"));
        assert!(!rig.editor.has_draft());
    }

    #[test]
    fn applying_a_vertex_or_the_opacity_is_one_guarded_undo_step() {
        let mut rig = Rig::new("forms-apply");
        rig.draw();
        let start = rig.revision();
        rig.editor.vertex = 1;
        rig.editor.loaded = false;
        rig.draw();
        assert_eq!(rig.editor.position, [3.0, 4.0], "vertex 1 of the form");
        rig.editor.position = [30.25, -40.5];
        rig.editor.dirty = true;
        rig.click(Role::Button, "Apply vertex");
        assert_eq!(rig.editor.status, "Live2D form updated", "{}", rig.editor.error);
        assert_eq!(rig.revision(), start + 1);
        assert_eq!(rig.form()["data"]["positions"], json!([1.0, 2.0, 30.25, -40.5, 5.0, 6.0]));
        assert!(!rig.editor.has_draft());
        rig.controller.core_request(&json!({"op": "undo"}));
        assert_eq!(rig.form()["data"]["positions"], json!([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), "one undo restores it");

        rig.draw();
        rig.click(Role::CheckBox, "Edit opacity");
        assert!(rig.editor.opacity_mode);
        rig.editor.opacity = 0.5;
        rig.editor.dirty = true;
        let before = rig.revision();
        rig.click(Role::Button, "Apply opacity");
        assert_eq!(rig.form()["opacity"], 0.5, "{}", rig.editor.error);
        assert_eq!(rig.revision(), before + 1);
    }

    #[test]
    fn a_change_elsewhere_makes_the_draft_a_conflict_and_the_backend_refuses_a_stale_revision() {
        let mut rig = Rig::new("forms-conflict");
        rig.draw();
        rig.editor.position = [9.0, 9.0];
        rig.editor.dirty = true;
        let loaded_at = rig.revision();
        let changed = rig.controller.core_request(&json!({"op": "live2d.form.set_vertex", "sourceGuid": "mesh-guid", "formGuid": "form-guid",
            "coordinateType": "Canvas", "vertexIndex": 2, "position": [7.0, 7.0], "expectedRevision": loaded_at}));
        assert_eq!(changed["succeeded"], true, "{changed}");
        rig.draw();
        let (dirty, conflict, error) = rig.editor.state();
        assert!(dirty && conflict, "dirty {dirty} conflict {conflict} {error}");
        assert_eq!(error, "Document changed elsewhere. Your draft is preserved; reload before applying.");
        let revision = rig.revision();
        // The Apply button is disabled for a conflict, and a request that is sent anyway is refused by the
        // backend's revision check, which marks the conflict again.
        let output = rig.draw();
        let node = crate::editor_test_support::find(&output, Role::Button, "Apply vertex").unwrap();
        assert!(node.is_disabled());
        rig.editor.conflict = false;
        assert!(!rig.editor.apply(&mut rig.controller));
        assert!(rig.editor.error.contains("revision conflict"), "{}", rig.editor.error);
        assert!(rig.editor.state().1, "the refusal marks the conflict");
        assert_eq!(rig.revision(), revision);
        rig.click(Role::Button, "Reload form");
        let (dirty, conflict, error) = rig.editor.state();
        assert!(!dirty && !conflict && error.is_empty());
        assert_eq!(rig.editor.position, [1.0, 2.0]);
    }

    #[test]
    fn stored_settings_apply_as_one_guarded_undo_step_and_a_refusal_is_shown() {
        let mut rig = Rig::new("forms-stored");
        let output = rig.draw();
        for label in ["Color composition", "Alpha composition"] {
            assert!(has(&output, Role::ComboBox, label), "{label}");
        }
        assert!(has(&output, Role::CheckBox, "Invert clipping mask"));
        for label in ["Apply form settings", "Apply source settings"] {
            assert!(has(&output, Role::Button, label), "{label}: {:?}", texts(&output));
        }
        for label in ["Draw order", "Multiply color R", "Screen color A", "Clipping masks"] {
            assert!(texts(&output).iter().any(|t| t == label), "{label}");
        }
        assert_all_named(&output, "stored settings");
        assert!(crate::editor_test_support::find(&output, Role::Button, "Apply form settings").unwrap().is_disabled());
        assert_eq!(rig.editor.stored.form.multiply, [1.0, 1.0, 1.0, 1.0]);
        assert!(!rig.editor.has_draft());

        let start = rig.revision();
        rig.editor.stored.form.multiply = [0.5, 0.25, 0.0, 1.0];
        assert!(rig.editor.has_draft());
        rig.click(Role::Button, "Apply form settings");
        assert_eq!(rig.editor.status, "Live2D settings updated", "{}", rig.editor.error);
        assert_eq!(rig.revision(), start + 1);
        assert_eq!(rig.form()["multiplyColor"], json!([0.5, 0.25, 0.0, 1.0]));
        assert!(!rig.editor.has_draft());
        rig.controller.core_request(&json!({"op": "undo"}));
        assert_eq!(rig.form()["multiplyColor"], json!([1.0, 1.0, 1.0, 1.0]), "one undo restores it");

        // The fixture stores no draw order, so the file refuses it; the draft stays and the refusal text is shown.
        rig.draw();
        let revision = rig.revision();
        rig.editor.stored.form.draw_order = 640;
        rig.click(Role::Button, "Apply form settings");
        assert!(!rig.editor.error.is_empty());
        assert!(rig.editor.status.is_empty());
        assert_eq!(rig.revision(), revision);
        assert_eq!(rig.editor.stored.form.draw_order, 640);
        assert!(!rig.editor.state().1, "a refusal that is not a revision conflict is not a conflict");
        let message = rig.editor.error.clone();
        assert!(texts(&rig.draw()).contains(&message));
    }

    #[test]
    fn a_stale_settings_draft_is_a_conflict_and_the_other_half_keeps_its_draft_after_an_apply() {
        let mut rig = Rig::new("forms-stored-conflict");
        rig.draw();
        rig.editor.stored.form.multiply[0] = 0.5;
        let loaded_at = rig.revision();
        let changed = rig.controller.core_request(&json!({"op": "live2d.form.set_vertex", "sourceGuid": "mesh-guid",
            "formGuid": "form-guid", "coordinateType": "Canvas", "vertexIndex": 0, "position": [7.0, 7.0], "expectedRevision": loaded_at}));
        assert_eq!(changed["succeeded"], true, "{changed}");
        rig.draw();
        let (_, conflict, error) = rig.editor.state();
        assert!(conflict, "{error}");
        assert_eq!(rig.editor.stored.form.multiply[0], 0.5, "the draft is preserved");
        rig.click(Role::Button, "Reload form");
        assert_eq!(rig.editor.stored.form.multiply[0], 1.0);
        assert!(!rig.editor.has_draft() && !rig.editor.state().1);

        // Applying one half rebases the other half's draft on the new revision instead of dropping it.
        rig.editor.stored.form.multiply[0] = 0.5;
        rig.editor.stored.source.invert_clipping_mask = true;
        rig.click(Role::Button, "Apply form settings");
        assert_eq!(rig.editor.status, "Live2D settings updated", "{}", rig.editor.error);
        assert!(rig.editor.stored.source.invert_clipping_mask && rig.editor.stored.is_dirty());
        assert_eq!(rig.editor.stored.revision, rig.revision());
        assert!(!rig.editor.state().1);

        // The reverse: a vertex apply must not make a dirty settings draft look stale.
        rig.editor.stored.form.multiply[0] = 0.25;
        rig.editor.position = [9.0, 9.0];
        rig.editor.dirty = true;
        rig.click(Role::Button, "Apply vertex");
        assert_eq!(rig.editor.status, "Live2D form updated", "{}", rig.editor.error);
        rig.draw();
        assert!(!rig.editor.state().1, "{}", rig.editor.error);
        assert_eq!(rig.editor.stored.form.multiply[0], 0.25);
    }

    #[test]
    fn opacity_gui_main_apply_preserves_both_settings_drafts_in_the_event_invocation() {
        let mut rig = Rig::new("forms-opacity-event-rebase");
        rig.draw();
        rig.editor.stored.form.multiply[0] = 0.25;
        rig.editor.stored.source.invert_clipping_mask = true;
        rig.editor.position = [9.0, 9.0];
        rig.editor.dirty = true;
        let form_draft = rig.editor.stored.form.clone();
        let source_draft = rig.editor.stored.source.clone();
        let form_original = rig.editor.stored.form_original.clone();
        let source_original = rig.editor.stored.source_original.clone();
        let mut applied_invocations = 0;
        let (editor, controller) = (&mut rig.editor, &mut rig.controller);
        rig.harness.click(Role::Button, "Apply vertex", |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = controller.snapshot()["revision"].as_u64().unwrap();
                let changed = editor.show(ui, controller);
                let after = controller.snapshot()["revision"].as_u64().unwrap();
                if after > before {
                    applied_invocations += 1;
                    assert!(changed);
                    assert!(!editor.conflict, "event invocation: {}", editor.error);
                    assert_eq!(editor.stored.revision, after);
                }
                assert!(editor.stored.form == form_draft && editor.stored.source == source_draft);
                assert!(editor.stored.form_original == form_original && editor.stored.source_original == source_original);
            });
        });
        assert_eq!(applied_invocations, 1);
        assert!(!rig.editor.conflict, "{}", rig.editor.error);
    }

    #[test]
    fn opacity_gui_artpath_offers_opacity_without_vertex_controls() {
        let mut rig = Rig::artpath("forms-opacity-artpath-controls", &crate::editor_test_support::artpath_cmo3_xml());
        let output = rig.draw();
        assert!(has(&output, Role::SpinButton, "Opacity"));
        assert!(has(&output, Role::Button, "Apply opacity"));
        assert!(has(&output, Role::CheckBox, "Closed"));
        for label in ["Apply vertex", "Vertex index", "Source X", "Source Y", "Edit opacity"] {
            assert!(!texts(&output).iter().any(|text| text == label), "{label}");
        }
    }

    #[test]
    fn opacity_gui_artpath_numeric_edit_history_noop_and_save_keep_other_fields() {
        for coordinate in ["Canvas", "DeformerLocal"] {
            let xml = crate::editor_test_support::artpath_cmo3_xml()
                .replace("<s xs.n='coordName'>Canvas</s>", &format!("<s xs.n='coordName'>{coordinate}</s>"));
            let mut rig = Rig::artpath(&format!("forms-opacity-history-{coordinate}"), &xml);
            rig.draw();
            let before = rig.controller.snapshot().clone();
            rig.drag("Opacity", -150.0);
            assert!(rig.editor.opacity < 0.0, "authored opacity is not clamped");
            let opacity = rig.editor.opacity;
            let mut expected = before["model"].clone();
            expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap()
                ["forms"][0]["opacity"] = json!(opacity);
            rig.click(Role::Button, "Apply opacity");
            assert!(rig.editor.error.is_empty(), "{}", rig.editor.error);
            assert_eq!(rig.editor.coordinate, coordinate);
            assert_eq!(rig.controller.snapshot()["model"], expected);
            assert_eq!(rig.controller.snapshot()["history"]["count"].as_u64().unwrap(), before["history"]["count"].as_u64().unwrap() + 1);
            rig.controller.core_request(&json!({"op":"undo"}));
            assert_eq!(rig.controller.snapshot()["model"], before["model"]);
            rig.controller.core_request(&json!({"op":"redo"}));
            assert_eq!(rig.controller.snapshot()["model"], expected);
            rig.draw();
            let unchanged = rig.controller.snapshot().clone();
            rig.drag("Opacity", 10.0);
            rig.drag("Opacity", -10.0);
            assert!(!rig.editor.has_draft());
            assert!(crate::editor_test_support::find(&rig.draw(), Role::Button, "Apply opacity").unwrap().is_disabled());
            rig.click(Role::Button, "Apply opacity");
            assert_eq!(rig.controller.snapshot(), &unchanged);
            rig.editor.opacity = 1.00005;
            rig.editor.dirty = true;
            rig.click(Role::Button, "Apply opacity");
            expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap()
                ["forms"][0]["opacity"] = json!(1.00005);
            assert_eq!(rig.controller.snapshot()["model"], expected);
            let saved = temp_dir(&format!("forms-opacity-saved-{coordinate}")).join("saved.cmo3");
            assert_eq!(rig.controller.core_request(&json!({"op":"save","path":saved}))["succeeded"], true);
            let mut reopened = EditorController::new();
            assert_eq!(reopened.core_request(&json!({"op":"open","path":saved}))["succeeded"], true);
            assert_eq!(reopened.snapshot()["model"], expected);
        }
    }

    #[test]
    fn opacity_gui_artpath_three_drafts_rebase_in_each_event_and_following_invocation() {
        for first in ["Apply opacity", "Apply form settings", "Apply source settings"] {
            let mut rig = Rig::artpath(&format!("forms-opacity-three-{first}"), &crate::editor_test_support::artpath_cmo3_xml());
            rig.draw();
            rig.drag("Opacity", -25.0);
            let opacity = rig.editor.opacity;
            rig.drag("Draw order", -10.0);
            let order = rig.editor.stored.form.draw_order;
            rig.click(Role::CheckBox, "Closed");
            let start = rig.revision();
            let mut applied = 0;
            rig.click_observing(first, |editor, controller| {
                let revision = controller.snapshot()["revision"].as_u64().unwrap();
                assert!(!editor.conflict, "{first}: {}", editor.error);
                if revision > start {
                    applied += 1;
                    assert_eq!(editor.revision, revision);
                    assert_eq!(editor.stored.revision, revision);
                    assert_eq!(editor.opacity, opacity);
                    assert_eq!(editor.stored.form.draw_order, order);
                    assert!(editor.stored.source.closed);
                    assert_eq!(editor.dirty, first != "Apply opacity");
                    assert_eq!(editor.stored.form != editor.stored.form_original, first != "Apply form settings");
                    assert_eq!(editor.stored.source != editor.stored.source_original, first != "Apply source settings");
                }
            });
            assert!(applied > 0, "the actual button must apply");
            for remaining in ["Apply opacity", "Apply form settings", "Apply source settings"] {
                if remaining != first {
                    rig.click(Role::Button, remaining);
                    assert!(rig.editor.error.is_empty(), "{}", rig.editor.error);
                }
            }
            assert_eq!(rig.revision(), start + 3);
            assert!(!rig.editor.has_draft());
        }
    }

    #[test]
    fn opacity_gui_artpath_storage_refusal_keeps_redo_and_stale_reload_keeps_metadata_available() {
        let xml = crate::editor_test_support::artpath_cmo3_xml()
            .replace("<i xs.n='drawOrder'>950</i><f xs.n='opacity'>1</f>", "<i xs.n='drawOrder'>950</i><f xs.n='opacity'><![CDATA[1]]></f>");
        let mut rig = Rig::artpath("forms-opacity-refusal", &xml);
        rig.draw();
        rig.click(Role::CheckBox, "Closed");
        rig.click(Role::Button, "Apply source settings");
        rig.controller.core_request(&json!({"op":"undo"}));
        rig.draw();
        rig.drag("Opacity", -25.0);
        let opacity = rig.editor.opacity;
        let before = rig.controller.snapshot().clone();
        rig.click(Role::Button, "Apply opacity");
        assert_eq!(rig.controller.snapshot(), &before);
        assert!(rig.editor.dirty && !rig.editor.conflict && !rig.editor.error.is_empty());
        assert_eq!(rig.editor.opacity, opacity);
        let error = rig.editor.error.clone();
        assert!(texts(&rig.draw()).contains(&error));
        rig.controller.core_request(&json!({"op":"redo"}));
        rig.draw();
        assert!(rig.editor.conflict && rig.editor.dirty);
        assert_eq!(rig.editor.opacity, opacity);
        rig.click(Role::Button, "Reload form");
        let output = rig.draw();
        assert!(!rig.editor.has_draft() && !rig.editor.conflict);
        assert_eq!(rig.editor.opacity, 1.0);
        assert!(has(&output, Role::CheckBox, "Closed"));
        rig.choose("Source", "Mesh [ArtMesh]");
        assert!(!rig.editor.opacity_mode);
        assert!(has(&rig.draw(), Role::Button, "Apply vertex"));
        rig.choose("Source", "Path [ArtPath]");
        assert!(rig.editor.opacity_mode);
        assert!(has(&rig.draw(), Role::Button, "Apply opacity"));
    }

    #[test]
    fn opacity_gui_artpath_same_guid_in_another_coordinate_is_not_edited() {
        let mut xml = crate::editor_test_support::artpath_cmo3_xml();
        let start = xml.find("<CArtPathForm>").unwrap();
        let end = start + xml[start..].find("</CArtPathForm>").unwrap() + "</CArtPathForm>".len();
        let other = xml[start..end].replace("<s xs.n='coordName'>Canvas</s>", "<s xs.n='coordName'>DeformerLocal</s>");
        xml.insert_str(end, &other);
        xml = xml.replace("<carray_list xs.n='keyforms' count='1'><CArtPathForm>", "<carray_list xs.n='keyforms' count='2'><CArtPathForm>");
        let mut rig = Rig::artpath("forms-opacity-coordinate-identity", &xml);
        rig.draw();
        rig.choose("Form", "path-form (DeformerLocal)");
        let before = rig.controller.snapshot().clone();
        rig.drag("Opacity", -25.0);
        let mut expected = before["model"].clone();
        let path = expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap();
        assert_eq!(path["forms"].as_array().unwrap().len(), 2);
        path["forms"].as_array_mut().unwrap().iter_mut().find(|f| f["coordinateType"] == "DeformerLocal").unwrap()
            ["opacity"] = json!(rig.editor.opacity);
        rig.click(Role::Button, "Apply opacity");
        assert!(rig.editor.error.is_empty(), "{}", rig.editor.error);
        assert_eq!(rig.controller.snapshot()["model"], expected);
    }

    #[test]
    fn opacity_gui_invalid_opacity_does_not_create_zero_or_disable_stored_settings() {
        let mut rig = Rig::artpath("forms-opacity-invalid-projection", &crate::editor_test_support::artpath_cmo3_xml());
        rig.draw();
        let source = rig.controller.snapshot()["model"]["sources"].as_array().unwrap().iter()
            .find(|s| s["guid"] == "path-guid").unwrap().clone();
        for value in [Value::Null, json!("bad"), json!([])] {
            let mut form = source["forms"][0].clone();
            form["opacity"] = value;
            rig.editor.opacity = 0.75;
            rig.editor.load_draft(&form, rig.revision());
            assert!(!rig.editor.loaded && !rig.editor.error.is_empty());
            assert_eq!(rig.editor.opacity, 0.75, "no invented editable zero");
            let (editor, controller) = (&mut rig.editor, &mut rig.controller);
            let revision = controller.snapshot()["revision"].as_u64().unwrap();
            let output = rig.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor.notices(ui);
                    assert!(!editor.stored_ui(ui, controller, &source, Some(&form), revision, &[], &[]));
                });
            });
            assert!(has(&output, Role::Button, "Apply source settings"));
            assert!(has(&output, Role::Button, "Apply form settings"));
            assert!(texts(&output).contains(&"This form has invalid opacity".to_owned()));
        }
    }

    #[test]
    fn artpath_stored_controls_apply_alongside_opacity_without_vertex_controls() {
        let mut rig = Rig::artpath("forms-artpath-controls", &crate::editor_test_support::artpath_cmo3_xml());
        let output = rig.draw();
        assert!(has(&output, Role::CheckBox, "Closed"));
        assert!(has(&output, Role::SpinButton, "Division count"));
        assert!(has(&output, Role::ComboBox, "Brush"));
        assert!(has(&output, Role::SpinButton, "Draw order"));
        for label in ["Apply vertex", "Use offscreen", "Color composition", "Multiply color R"] {
            assert!(!texts(&output).iter().any(|text| text == label), "{label}");
        }
        let before = rig.controller.snapshot().clone();
        rig.choose("Source", "Mesh [ArtMesh]");
        rig.choose("Source", "Path [ArtPath]");
        rig.click(Role::CheckBox, "Closed");
        rig.drag("Division count", 10.0);
        let division = rig.editor.stored.source.division_count;
        assert_ne!(division, 20);
        rig.choose("Brush", "BrushB [brush-b]");
        rig.click(Role::CheckBox, "Invert clipping mask");
        rig.click(Role::CheckBox, "Mesh");
        rig.drag("Draw order", -10.0);
        let order = rig.editor.stored.form.draw_order;
        assert_ne!(order, 950);
        rig.click(Role::Button, "Apply source settings");
        let mut expected = before["model"].clone();
        let path = expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap();
        path["data"]["closed"] = json!(true);
        path["data"]["divisionCount"] = json!(division);
        path["data"]["brushGuid"] = json!("brush-b");
        path["data"]["invertClippingMask"] = json!(true);
        path["data"]["clipGuids"] = json!(["mesh-guid"]);
        assert_eq!(rig.controller.snapshot()["model"], expected, "{}", rig.editor.error);
        assert_eq!(rig.revision(), before["revision"].as_u64().unwrap()+1);
        assert_eq!(rig.editor.stored.form.draw_order, order, "other half draft survives");
        rig.click(Role::Button, "Apply form settings");
        expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "path-guid").unwrap()["forms"][0]["drawOrder"] = json!(order);
        assert_eq!(rig.controller.snapshot()["model"], expected, "{}", rig.editor.error);
        assert_eq!(rig.revision(), before["revision"].as_u64().unwrap()+2);
        let changed = rig.controller.snapshot().clone();
        rig.click(Role::CheckBox, "Closed");
        rig.click(Role::CheckBox, "Closed");
        assert!(!rig.editor.has_draft());
        assert_eq!(rig.controller.snapshot(), &changed);
        assert!(crate::editor_test_support::find(&rig.draw(),Role::Button,"Apply source settings").unwrap().is_disabled());
        assert_eq!(rig.controller.core_request(&json!({"op":"undo"}))["succeeded"], true);
        assert_eq!(rig.controller.core_request(&json!({"op":"redo"}))["succeeded"], true);
        assert_eq!(rig.controller.snapshot()["model"], expected);
        rig.draw();
        rig.click(Role::CheckBox,"Closed");
        rig.drag("Draw order",-10.0);
        rig.click(Role::Button,"Apply form settings");
        assert!(!rig.editor.stored.source.closed && rig.editor.stored.is_dirty(),"source draft survives form apply");
        assert_eq!(rig.editor.stored.revision,rig.revision());
        rig.click(Role::Button,"Apply source settings");
        assert!(rig.editor.error.is_empty() && !rig.editor.has_draft(),"{}",rig.editor.error);
    }

    #[test]
    fn artpath_requests_send_only_supported_changed_fields_and_keep_real_coordinates() {
        let source = json!({"data":{"type":"ArtPath","closed":false,"divisionCount":0,"brushGuid":"unknown"}});
        let original = SourceSettings::from_source(&source);
        let mut draft = original.clone();
        draft.closed = true;
        draft.division_count = u32::MAX;
        draft.color_composition = "invalid".into();
        draft.alpha_composition = "invalid".into();
        draft.use_offscreen = true;
        assert_eq!(source_set_request("path", "ArtPath", &draft, &original, 7).unwrap(),
            json!({"op":"live2d.source.set","guid":"path","closed":true,"divisionCount":u32::MAX,"expectedRevision":7}));
        let original = FormSettings::default();
        let mut draft = original.clone();
        draft.draw_order = 951;
        draft.multiply[0] = f64::NAN;
        assert_eq!(form_set_request("path","form","DeformerLocal","ArtPath",&draft,&original,8).unwrap(),
            json!({"op":"live2d.form.set","sourceGuid":"path","formGuid":"form","coordinateType":"DeformerLocal","drawOrder":951,"expectedRevision":8}));
        assert!(form_set_request("path","form","","ArtPath",&draft,&original,8).is_err());
        let source = json!({"data":{"type":"ArtPath"},"forms":[
            {"guid":"same","coordinateType":"Canvas","data":{"type":"ArtPath","points":[]}},
            {"guid":"same","coordinateType":"DeformerLocal","data":{"type":"ArtPath","points":[]}},
            {"guid":"bad","coordinateType":"","data":{"type":"ArtPath"}}]});
        assert_eq!(editable_forms(&source).len(),2);
        assert!(same_form(editable_forms(&source)[1],"same","DeformerLocal"));
        assert!(!same_form(editable_forms(&source)[0],"same","DeformerLocal"));
    }

    #[test]
    fn artpath_refusal_retains_drafts_and_external_changes_conflict() {
        let xml = crate::editor_test_support::artpath_cmo3_xml()
            .replace("<b xs.n='invertClippingMask'>false</b>", "");
        let mut rig = Rig::artpath("forms-artpath-refusal", &xml);
        rig.draw();
        rig.click(Role::CheckBox, "Closed");
        rig.click(Role::CheckBox, "Invert clipping mask");
        let before = rig.controller.snapshot().clone();
        rig.click(Role::Button, "Apply source settings");
        assert_eq!(rig.controller.snapshot(), &before);
        assert!(rig.editor.stored.source.closed && rig.editor.stored.source.invert_clipping_mask);
        assert!(!rig.editor.error.is_empty() && !rig.editor.conflict);
        let message = rig.editor.error.clone();
        assert!(texts(&rig.draw()).contains(&message));
        let reply = rig.controller.core_request(&json!({"op":"live2d.rename","guid":"path-guid","name":"Changed","expectedRevision":rig.revision()}));
        assert_eq!(reply["succeeded"],true,"{reply}");
        rig.draw();
        assert!(rig.editor.conflict && rig.editor.stored.source.closed);
        rig.click(Role::Button,"Reload form");
        rig.draw();
        assert!(!rig.editor.has_draft() && !rig.editor.conflict);
    }

    #[test]
    fn artpath_without_forms_preserves_unknown_brush_and_still_edits_source() {
        let mut xml = crate::editor_test_support::artpath_cmo3_xml();
        let start = xml.find("<carray_list xs.n='keyforms' count='1'><CArtPathForm>").unwrap();
        let end = start+xml[start..].find("</CArtPathForm></carray_list>").unwrap()+"</CArtPathForm></carray_list>".len();
        xml.replace_range(start..end,"<carray_list xs.n='keyforms' count='0'/>");
        xml = xml.replace("<CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>","<CArtPathBrushGuid xs.n='brushGuid' uuid='unknown'/>");
        let mut rig = Rig::artpath("forms-artpath-noforms",&xml);
        let output = rig.draw();
        assert!(has(&output,Role::ComboBox,"Brush"));
        assert!(!has(&output,Role::SpinButton,"Draw order"));
        assert!(!has(&output,Role::Button,"Apply form settings"));
        assert_eq!(rig.editor.stored.source.brush_guid,"unknown");
        assert!(!rig.editor.has_draft());
        rig.click(Role::CheckBox,"Closed");
        rig.click(Role::Button,"Apply source settings");
        assert!(rig.editor.error.is_empty(),"{}",rig.editor.error);
        let path = rig.controller.snapshot()["model"]["sources"].as_array().unwrap().iter().find(|s| s["guid"]=="path-guid").unwrap();
        assert_eq!(path["data"]["brushGuid"],"unknown");
        assert_eq!(path["data"]["closed"],true);
        let start = xml.find("<CArtPathBrushSetting xs.n='artPathBrushesSetting'>").unwrap();
        let end = start+xml[start..].find("</CArtPathBrushSetting>").unwrap()+"</CArtPathBrushSetting>".len();
        xml.replace_range(start..end,"");
        let mut rig = Rig::artpath("forms-artpath-empty-brushes",&xml);
        let output = rig.draw();
        assert!(crate::editor_test_support::find(&output,Role::ComboBox,"Brush").unwrap().is_disabled());
        assert_eq!(rig.editor.stored.source.brush_guid,"unknown");
        rig.click(Role::CheckBox,"Closed");
        rig.click(Role::Button,"Apply source settings");
        assert!(rig.editor.error.is_empty(),"{}",rig.editor.error);
        assert_eq!(rig.editor.stored.source.brush_guid,"unknown");
    }

    #[test]
    fn invalid_artpath_projection_is_visible_without_loading_an_editable_default() {
        let mut rig = Rig::new("forms-artpath-invalid-projection");
        for data in [json!({"type":"ArtPath","closed":false,"divisionCount":4294967296_u64,"brushGuid":"b"}),
            json!({"type":"ArtPath","closed":null,"divisionCount":20,"brushGuid":"b"})]
        {
            let source = json!({"data":data});
            let (editor,controller) = (&mut rig.editor,&mut rig.controller);
            let output = rig.harness.frame(Vec::new(),|ctx| {
                egui::CentralPanel::default().show(ctx,|ui| {
                    assert!(!editor.stored_ui(ui,controller,&source,None,0,&[],&[]));
                });
            });
            assert!(texts(&output).contains(&"ArtPath stored metadata is invalid".to_owned()));
            assert!(!rig.editor.stored.loaded);
        }
    }

    #[test]
    fn a_part_without_forms_still_offers_its_source_settings() {
        let mut rig = Rig::new("forms-part");
        rig.draw();
        rig.editor.select_source("part-guid");
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t == "This source has no editable primary forms"), "{:?}", texts(&output));
        assert!(has(&output, Role::CheckBox, "Use offscreen"));
        assert!(has(&output, Role::Button, "Apply source settings"));
        assert!(!has(&output, Role::Button, "Apply form settings"));
        assert!(!has(&output, Role::Button, "Apply vertex"));
    }
}
