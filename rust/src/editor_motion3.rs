//! The Live2D Motion3 panel (port of `panels/rustcore_motion3.d`): import, list, select, export, reload,
//! curve and point drafts, parameter bindings, sample / pose readouts and the preview transport, all over the
//! `motion3.*` session operations.
//!
//! Drafts follow D's rules: an edit is held in the panel until Apply; a draft is a conflict when the document's
//! motion (or bindings) no longer equal what the draft was loaded from, and then it can only be reloaded or
//! discarded. While a draft is open, switching motions, exporting and replacing the document are refused
//! (`has_draft`). A native CMO3 holds its clips in the session only (`motion3Library`): those get the import,
//! export and transport controls but no point or binding editing, as in D.

use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;

// ------------------------------------------------------------------------------------------ pure helpers

/// One logical point of a curve: the flat `Segments` array decoded as D's `curvePoints` does.
#[derive(Clone, Debug, PartialEq)]
pub struct PointInfo {
    pub index: usize,
    pub time: f64,
    pub value: f64,
    pub kind: String,
}

pub fn curves(motion: &Value) -> &[Value] {
    motion["Curves"].as_array().map_or(&[], Vec::as_slice)
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

/// Logical points of curve `curve`: the initial point, then per segment a linear / stepped / inverse-stepped
/// endpoint (tags 0, 2, 3) or two Bezier controls and an endpoint (tag 1).
pub fn curve_points(motion: &Value, curve: usize) -> Result<Vec<PointInfo>, String> {
    let curve = curves(motion).get(curve).ok_or("Motion3 curve index is out of range")?;
    let flat = curve["Segments"].as_array().ok_or("Motion3 curve segments are malformed")?;
    if flat.len() < 2 {
        return Err("Motion3 curve has no initial point".into());
    }
    let mut points = Vec::new();
    let mut add = |time_at: usize, value_at: usize, kind: String| -> Result<(), String> {
        let time = flat.get(time_at).and_then(number);
        let value = flat.get(value_at).and_then(number);
        let (Some(time), Some(value)) = (time, value) else {
            return Err("Motion3 point is truncated or nonnumeric".into());
        };
        points.push(PointInfo { index: points.len(), time, value, kind });
        Ok(())
    };
    add(0, 1, "Initial point".into())?;
    let (mut at, mut segment) = (2, 0);
    while at < flat.len() {
        let tag = number(&flat[at]).ok_or("Motion3 segment tag is not numeric")?;
        let prefix = format!("Segment {segment}: ");
        if tag == 0.0 {
            add(at + 1, at + 2, format!("{prefix}Linear endpoint"))?;
            at += 3;
        } else if tag == 1.0 {
            add(at + 1, at + 2, format!("{prefix}Bezier control 1"))?;
            add(at + 3, at + 4, format!("{prefix}Bezier control 2"))?;
            add(at + 5, at + 6, format!("{prefix}Bezier endpoint"))?;
            at += 7;
        } else if tag == 2.0 {
            add(at + 1, at + 2, format!("{prefix}Stepped endpoint"))?;
            at += 3;
        } else if tag == 3.0 {
            add(at + 1, at + 2, format!("{prefix}Inverse-stepped endpoint"))?;
            at += 3;
        } else {
            return Err("Motion3 segment type is unsupported".into());
        }
        segment += 1;
    }
    Ok(points)
}

/// D `effectiveClipEnd`: the larger of the metadata duration and every curve's last point time.
pub fn effective_clip_end(motion: &Value) -> f64 {
    let mut end = number(&motion["Meta"]["Duration"]).unwrap_or(0.0);
    for index in 0..curves(motion).len() {
        if let Some(last) = curve_points(motion, index).ok().and_then(|points| points.last().map(|p| p.time)) {
            end = end.max(last);
        }
    }
    end
}

fn live2d_registry<'a>(snapshot: &'a Value, key: &str) -> Option<&'a Value> {
    snapshot["model"]["live2d"].get(key).filter(|value| value.is_object())
}

/// `true` for a native CMO3 document, whose clips live in the session only.
pub fn is_native(snapshot: &Value) -> bool {
    snapshot.get("motion3Library").is_some_and(Value::is_object)
}

pub fn motion_names(snapshot: &Value) -> Vec<String> {
    let registry = if is_native(snapshot) { snapshot.get("motion3Library") } else { live2d_registry(snapshot, "motions") };
    let mut names: Vec<String> = registry.and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
}

/// The document's current copy of a motion.
pub fn stored_motion<'a>(snapshot: &'a Value, name: &str) -> Option<&'a Value> {
    let registry = if is_native(snapshot) { snapshot.get("motion3Library") } else { live2d_registry(snapshot, "motions") };
    registry?.get(name)
}

/// The document's binding rows of a motion (empty when none).
pub fn stored_bindings(snapshot: &Value, name: &str) -> Value {
    live2d_registry(snapshot, "motionBindings")
        .and_then(|rows| rows.get(name))
        .filter(|rows| rows.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// A parameter a Motion3 curve can be mapped to.
#[derive(Clone, Debug, PartialEq)]
pub struct LeafParameter {
    pub uuid: u32,
    pub name: String,
    pub is_vec2: bool,
}

/// Non-group parameters of the model, in tree order (D `flattenLeafParameters`).
pub fn leaf_parameters(snapshot: &Value) -> Vec<LeafParameter> {
    fn walk(list: &Value, out: &mut Vec<LeafParameter>) {
        for item in list.as_array().into_iter().flatten() {
            if item.get("groupUUID").is_none() {
                if let Some(uuid) = item["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()) {
                    out.push(LeafParameter {
                        uuid,
                        name: item["name"].as_str().unwrap_or("Parameter").to_owned(),
                        is_vec2: item["is_vec2"].as_bool() == Some(true),
                    });
                }
            }
            walk(&item["children"], out);
        }
    }
    let mut out = Vec::new();
    walk(&snapshot["model"]["param"], &mut out);
    out
}

pub fn binding_for<'a>(rows: &'a Value, curve_id: &str) -> Option<&'a Value> {
    rows.as_array()?.iter().find(|row| row["id"].as_str() == Some(curve_id))
}

pub fn is_parameter_curve(motion: &Value, curve_id: &str) -> bool {
    curves(motion).iter().any(|curve| curve["Target"] == "Parameter" && curve["Id"].as_str() == Some(curve_id))
}

/// Checks a mapping the way D's `ngRustCoreMotion3SetBindingDraft` does before it edits the draft.
pub fn check_binding(
    motion: &Value,
    parameters: &[LeafParameter],
    curve_id: &str,
    parameter: u32,
    axis: i64,
) -> Result<(), String> {
    if curve_id.is_empty() || !is_parameter_curve(motion, curve_id) {
        return Err("Select a Parameter curve with an external Motion3 ID.".into());
    }
    let Some(found) = parameters.iter().find(|candidate| candidate.uuid == parameter).filter(|_| parameter != 0) else {
        return Err("Choose an existing parameter UUID.".into());
    };
    if axis != 0 && axis != 1 || axis == 1 && !found.is_vec2 {
        return Err(if found.is_vec2 { "Axis must be X or Y." } else { "This 1D parameter only supports X." }.into());
    }
    Ok(())
}

/// The binding rows after adding, replacing (`choice`) or removing (`None`) the row of `curve_id`.
pub fn bindings_with(rows: &Value, curve_id: &str, choice: Option<(u32, i64)>) -> Result<Value, String> {
    let mut list = match rows {
        Value::Array(list) => list.clone(),
        Value::Null => Vec::new(),
        _ => return Err("Reload bindings before editing this Motion3 mapping.".into()),
    };
    let found = list.iter().position(|row| row["id"].as_str() == Some(curve_id));
    match (choice, found) {
        (None, Some(at)) => {
            list.remove(at);
        }
        (None, None) => {}
        (Some((parameter, axis)), Some(at)) => {
            let row = list[at].as_object_mut().ok_or("The matching binding row is malformed; reload before editing.")?;
            row.insert("parameter".into(), json!(parameter));
            row.insert("axis".into(), json!(axis));
        }
        (Some((parameter, axis)), None) => list.push(json!({"id": curve_id, "parameter": parameter, "axis": axis})),
    }
    Ok(Value::Array(list))
}

fn curve_title(curve: &Value, index: usize) -> String {
    format!(
        "{index}. {} / {}",
        curve["Target"].as_str().unwrap_or("Unknown"),
        curve["Id"].as_str().unwrap_or("(missing ID)")
    )
}

fn point_title(point: &PointInfo) -> String {
    format!("#{} {}  t={}  v={}", point.index, point.kind, point.time, point.value)
}

fn row_text(row: &Value, applied: bool) -> String {
    let number = |key: &str| number(&row[key]).map_or_else(|| "?".to_owned(), |n| n.to_string());
    if applied {
        if let Some(parameter) = row["parameter"].as_str() {
            return format!("{parameter} = {}", number("value"));
        }
        let axis = if row["axis"].as_i64() == Some(1) { "Y" } else { "X" };
        format!(
            "{} -> {} {axis}, raw {}, value {}",
            row["id"].as_str().unwrap_or(""),
            row["parameter"],
            number("raw"),
            row["value"]
        )
    } else {
        format!("{} / {} = {}", row["target"].as_str().unwrap_or(""), row["id"].as_str().unwrap_or(""), number("value"))
    }
}

// ------------------------------------------------------------------------------------------ state

#[derive(Clone, Debug, PartialEq)]
struct PointDraft {
    curve: usize,
    number: usize,
    time: f64,
    value: f64,
    original_time: f64,
    original_value: f64,
    kind: String,
    /// The motion this draft was loaded from; a different document motion makes the draft a conflict.
    baseline: Value,
    conflict: bool,
}

impl PointDraft {
    fn dirty(&self) -> bool {
        self.time != self.original_time || self.value != self.original_value
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BindingDraft {
    baseline: Value,
    rows: Value,
    conflict: bool,
}

impl BindingDraft {
    fn dirty(&self) -> bool {
        self.rows != self.baseline
    }
}

/// What the panel changed this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The session changed (preview, document): the editor should re-render.
    pub changed: bool,
    /// A preview started playing: the editor starts its frame clock.
    pub play_started: bool,
}

#[derive(Default)]
pub struct Motion3Panel {
    selected: String,
    motion: Option<Value>,
    missing: bool,
    curve: usize,
    point_index: usize,
    point: Option<PointDraft>,
    bindings: Option<BindingDraft>,
    bind_parameter: u32,
    bind_axis: i64,
    seek_draft: f64,
    last_sample: Option<Value>,
    last_pose: Option<Value>,
    import_name: String,
    import_path: String,
    export_path: String,
    error: String,
    status: String,
}

#[derive(Debug)]
enum Click {
    Import,
    Refresh,
    Select(String),
    Export,
    Reload,
    Curve(usize),
    Point(usize),
    ApplyPoint,
    ReloadPoint,
    DiscardPoint,
    BindParameter(u32),
    BindAxis(i64),
    AddBinding,
    RemoveBinding,
    ApplyBindings,
    ReloadBindings,
    DiscardBindings,
    Sample,
    Pose,
    Seek,
    PlayPause,
    Reset,
}

fn call(controller: &mut EditorController, request: Value) -> Result<Value, String> {
    let response = controller.core_request(&request);
    if response["succeeded"].as_bool() == Some(true) {
        Ok(response["result"].clone())
    } else {
        Err(response["message"].as_str().unwrap_or("Motion3 command failed").to_owned())
    }
}

impl Motion3Panel {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new document replaced the old one: nothing selected, no drafts.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// True while a point or binding draft is open: the editor must not replace the document or save.
    pub fn has_draft(&self) -> bool {
        self.point.as_ref().is_some_and(PointDraft::dirty) || self.bindings.as_ref().is_some_and(BindingDraft::dirty)
    }

    #[cfg(test)]
    pub fn selected(&self) -> &str {
        &self.selected
    }

    #[cfg(test)]
    pub fn select_for_test(&mut self, name: &str) {
        self.selected = name.to_owned();
    }

    fn clear_selection(&mut self) {
        let keep = (self.import_name.clone(), self.import_path.clone(), self.export_path.clone());
        *self = Self::default();
        (self.import_name, self.import_path, self.export_path) = keep;
    }

    fn load_point(&mut self, motion: &Value, curve: usize, number: usize) -> Result<(), String> {
        let points = curve_points(motion, curve)?;
        let picked = points.get(number).ok_or("Motion3 point index is out of range.")?;
        self.curve = curve;
        self.point_index = number;
        self.point = Some(PointDraft {
            curve,
            number,
            time: picked.time,
            value: picked.value,
            original_time: picked.time,
            original_value: picked.value,
            kind: picked.kind.clone(),
            baseline: motion.clone(),
            conflict: false,
        });
        Ok(())
    }

    /// Follows the document: reloads clean state when the motion changed, flags drafts that went stale.
    pub fn sync(&mut self, snapshot: &Value) {
        if self.selected.is_empty() {
            return;
        }
        let current = stored_motion(snapshot, &self.selected).cloned();
        self.missing = current.is_none();
        let (point_dirty, bindings_dirty) =
            (self.point.as_ref().is_some_and(PointDraft::dirty), self.bindings.as_ref().is_some_and(BindingDraft::dirty));
        let Some(current) = current else {
            if point_dirty || bindings_dirty {
                self.error = "The selected Motion3 was removed; discard its retained draft before switching.".into();
                if let Some(point) = self.point.as_mut().filter(|_| point_dirty) {
                    point.conflict = true;
                }
                if let Some(bindings) = self.bindings.as_mut().filter(|_| bindings_dirty) {
                    bindings.conflict = true;
                }
            } else {
                self.clear_selection();
            }
            return;
        };
        if let Some(point) = self.point.as_mut().filter(|_| point_dirty) {
            point.conflict = point.baseline != current;
        } else if self.motion.as_ref() != Some(&current) {
            let (curve, number) = (self.curve, self.point_index);
            self.point = None;
            if self.load_point(&current, curve, number).is_err() && self.load_point(&current, 0, 0).is_err() {
                self.point = None;
            }
            self.motion = Some(current);
        }
        if is_native(snapshot) {
            return;
        }
        let rows = stored_bindings(snapshot, &self.selected);
        match self.bindings.as_mut() {
            Some(draft) if draft.dirty() => draft.conflict = draft.baseline != rows,
            _ => self.bindings = Some(BindingDraft { baseline: rows.clone(), rows, conflict: false }),
        }
    }

    // -------------------------------------------------------------------------------------- actions

    fn perform(&mut self, click: Click, controller: &mut EditorController, snapshot: &Value, outcome: &mut Outcome) {
        self.error.clear();
        let result = self.perform_inner(click, controller, snapshot, outcome);
        match result {
            Ok(status) => self.status = status,
            Err(error) => {
                self.status.clear();
                self.error = error;
            }
        }
    }

    fn perform_inner(
        &mut self,
        click: Click,
        controller: &mut EditorController,
        snapshot: &Value,
        outcome: &mut Outcome,
    ) -> Result<String, String> {
        let name = self.selected.clone();
        match click {
            Click::Import => {
                if self.has_draft() {
                    return Err("Apply or discard Motion3 drafts before importing.".into());
                }
                let new_name = self.import_name.trim().to_owned();
                call(controller, json!({"op": "motion3.import", "name": new_name, "path": self.import_path.trim()}))?;
                let motion = call(controller, json!({"op": "motion3.read", "name": new_name}))?;
                controller.pause();
                self.clear_selection();
                self.selected = new_name;
                self.seek_draft = 0.0;
                if !is_native(snapshot) {
                    let _ = self.load_point(&motion, 0, 0);
                }
                self.motion = Some(motion);
                outcome.changed = true;
                Ok("Motion3 imported.".into())
            }
            Click::Refresh => {
                let listing = call(controller, json!({"op": "motion3.list"}))?;
                let count = listing["motions"].as_array().ok_or("Motion3 list response is malformed")?.len();
                Ok(format!("Motion3 list refreshed ({count})."))
            }
            Click::Select(picked) => {
                if self.has_draft() {
                    return Err("Apply or discard Motion3 drafts before switching motions.".into());
                }
                if stored_motion(snapshot, &picked).is_none() {
                    return Err("Select an existing Motion3 motion.".into());
                }
                let motion = call(controller, json!({"op": "motion3.read", "name": picked}))?;
                controller.pause();
                self.clear_selection();
                self.selected = picked;
                self.seek_draft = 0.0;
                if !is_native(snapshot) {
                    let _ = self.load_point(&motion, 0, 0);
                }
                self.motion = Some(motion);
                Ok("Motion3 selected. Seek or play explicitly to change the preview.".into())
            }
            Click::Export => {
                call(controller, json!({"op": "motion3.export", "name": name, "path": self.export_path.trim()}))?;
                Ok("Motion3 exported.".into())
            }
            Click::Reload => {
                let motion = call(controller, json!({"op": "motion3.read", "name": name}))?;
                let (curve, number) = (self.curve, self.point_index);
                self.point = None;
                self.bindings = None;
                if self.load_point(&motion, curve, number).is_err() {
                    let _ = self.load_point(&motion, 0, 0);
                }
                self.motion = Some(motion);
                if !is_native(snapshot) {
                    let rows = call(controller, json!({"op": "motion3.bindings", "name": name}))?;
                    self.bindings = Some(BindingDraft { baseline: rows.clone(), rows, conflict: false });
                }
                Ok("Motion3 reloaded.".into())
            }
            Click::Curve(index) => {
                let motion = self.motion.clone().ok_or("Select a Motion3 first.")?;
                self.load_point(&motion, index, 0)?;
                Ok(String::new())
            }
            Click::Point(index) => {
                let motion = self.motion.clone().ok_or("Select a Motion3 first.")?;
                let curve = self.curve;
                self.load_point(&motion, curve, index)?;
                Ok(String::new())
            }
            Click::ApplyPoint => self.apply_point(controller, snapshot, outcome),
            Click::ReloadPoint => {
                let motion = stored_motion(snapshot, &name).cloned().ok_or("The selected Motion3 was removed.")?;
                let (curve, number) = (self.curve, self.point_index);
                self.point = None;
                self.load_point(&motion, curve, number)?;
                Ok("Motion3 point reloaded.".into())
            }
            Click::DiscardPoint => {
                let (curve, number) = (self.curve, self.point_index);
                self.point = None;
                if let Some(motion) = stored_motion(snapshot, &name).cloned() {
                    self.load_point(&motion, curve, number)?;
                }
                Ok(String::new())
            }
            Click::BindParameter(id) => {
                self.bind_parameter = id;
                self.bind_axis = 0;
                Ok(String::new())
            }
            Click::BindAxis(axis) => {
                self.bind_axis = axis;
                Ok(String::new())
            }
            Click::AddBinding | Click::RemoveBinding => {
                let remove = matches!(click, Click::RemoveBinding);
                let motion = self.motion.clone().ok_or("Select a Motion3 first.")?;
                let curve_id = curves(&motion).get(self.curve).and_then(|c| c["Id"].as_str()).unwrap_or_default().to_owned();
                let draft = self.bindings.as_ref().filter(|d| !d.conflict).ok_or("Reload bindings before editing this Motion3 mapping.")?;
                let choice = if remove {
                    None
                } else {
                    check_binding(&motion, &leaf_parameters(snapshot), &curve_id, self.bind_parameter, self.bind_axis)?;
                    Some((self.bind_parameter, self.bind_axis))
                };
                let rows = bindings_with(&draft.rows, &curve_id, choice)?;
                if let Some(draft) = self.bindings.as_mut() {
                    draft.rows = rows;
                }
                controller.pause();
                Ok(String::new())
            }
            Click::ApplyBindings => {
                let draft = self.bindings.clone().filter(BindingDraft::dirty).ok_or("There are no binding changes to apply.")?;
                if draft.conflict || draft.baseline != stored_bindings(snapshot, &name) {
                    if let Some(own) = self.bindings.as_mut() {
                        own.conflict = true;
                    }
                    return Err("Reload the Motion3 bindings before applying this stale draft.".into());
                }
                let applied = call(
                    controller,
                    json!({"op": "motion3.bind", "name": name, "bindings": draft.rows, "preserve_unknown": true}),
                )?;
                self.bindings = Some(BindingDraft { baseline: applied.clone(), rows: applied, conflict: false });
                outcome.changed = true;
                Ok("Motion3 bindings applied.".into())
            }
            Click::ReloadBindings | Click::DiscardBindings => {
                let rows = if self.missing { json!([]) } else { call(controller, json!({"op": "motion3.bindings", "name": name}))? };
                self.bindings = Some(BindingDraft { baseline: rows.clone(), rows, conflict: false });
                self.bind_parameter = 0;
                self.bind_axis = 0;
                Ok(String::new())
            }
            Click::Sample => {
                self.last_sample = Some(call(controller, json!({"op": "motion3.sample", "name": name, "time": self.seek_draft}))?);
                Ok("Motion3 curves sampled.".into())
            }
            Click::Pose => {
                self.last_pose = Some(call(controller, json!({"op": "motion3.pose", "name": name, "time": self.seek_draft}))?);
                Ok("Motion3 mapped pose evaluated.".into())
            }
            Click::Seek => {
                controller.pause();
                call(controller, json!({"op": "motion3.seek", "name": name, "time": self.seek_draft}))?;
                outcome.changed = true;
                Ok("Motion3 preview positioned.".into())
            }
            Click::PlayPause => {
                if controller.playing() {
                    controller.pause();
                    return Ok("Motion3 playback paused.".into());
                }
                if self.has_draft() {
                    return Err("Apply or discard Motion3 drafts before playing.".into());
                }
                let started = controller.play_motion3(&name);
                if started["succeeded"].as_bool() != Some(true) {
                    return Err(started["message"].as_str().unwrap_or("Motion3 playback failed").to_owned());
                }
                outcome.changed = true;
                outcome.play_started = true;
                Ok("Motion3 playback started.".into())
            }
            Click::Reset => {
                controller.pause();
                if snapshot["motion3"]["name"].as_str() == Some(name.as_str()) {
                    call(controller, json!({"op": "motion3.reset"}))?;
                    self.seek_draft = 0.0;
                    self.last_pose = None;
                    outcome.changed = true;
                }
                Ok("Motion3 preview reset.".into())
            }
        }
    }

    fn apply_point(&mut self, controller: &mut EditorController, snapshot: &Value, outcome: &mut Outcome) -> Result<String, String> {
        let draft = self.point.clone().filter(PointDraft::dirty).ok_or("There are no point changes to apply.")?;
        let name = self.selected.clone();
        if draft.conflict || stored_motion(snapshot, &name) != Some(&draft.baseline) {
            if let Some(own) = self.point.as_mut() {
                own.conflict = true;
            }
            return Err("Reload this Motion3 point before applying the stale draft.".into());
        }
        if !draft.time.is_finite() || !draft.value.is_finite() {
            return Err("Point time and value must be finite numbers.".into());
        }
        let mut request = json!({"op": "motion3.point.set", "name": name, "curve": draft.curve, "point": draft.number});
        if draft.time != draft.original_time {
            request["time"] = json!(draft.time);
        }
        if draft.value != draft.original_value {
            request["value"] = json!(draft.value);
        }
        call(controller, request)?;
        controller.pause();
        let motion = call(controller, json!({"op": "motion3.read", "name": name}))?;
        self.load_point(&motion, draft.curve, draft.number)?;
        self.motion = Some(motion);
        outcome.changed = true;
        Ok("Motion3 point applied.".into())
    }

    // -------------------------------------------------------------------------------------- drawing

    /// Draws the panel. `looping` is the editor's shared Loop flag for previews.
    pub fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController, looping: &mut bool) -> Outcome {
        let snapshot = controller.snapshot().clone();
        self.sync(&snapshot);
        let mut outcome = Outcome::default();
        let busy = snapshot["deformPending"].as_bool() == Some(true);
        let native = is_native(&snapshot);
        let mut clicks: Vec<Click> = Vec::new();
        ui.heading("Live2D Motion3");
        if native {
            ui.label("Session-only clips: export separately before opening another document.");
        }
        ui.add_enabled_ui(!busy, |ui| {
            self.import_row(ui, &mut clicks);
            let names = motion_names(&snapshot);
            self.select_row(ui, &names, &mut clicks);
            if !self.error.is_empty() {
                ui.colored_label(egui::Color32::LIGHT_RED, self.error.as_str());
            }
            if !self.status.is_empty() {
                ui.label(self.status.as_str());
            }
            let Some(motion) = self.motion.clone().filter(|_| !self.selected.is_empty()) else {
                ui.label(if names.is_empty() { "No Motion3 motions are imported." } else { "Select a Motion3 to inspect its curves." });
                return;
            };
            ui.label(format!("Selected: {}", self.selected));
            self.export_row(ui, &mut clicks);
            if !native {
                self.curve_section(ui, &motion, &snapshot, &mut clicks);
            }
            self.transport(ui, &snapshot, controller.playing(), looping, &mut clicks);
            self.diagnostics(ui, &snapshot);
        });
        for click in clicks {
            self.perform(click, controller, &snapshot, &mut outcome);
        }
        outcome
    }

    fn import_row(&mut self, ui: &mut egui::Ui, clicks: &mut Vec<Click>) {
        text_row(ui, "Motion3 name", &mut self.import_name);
        text_row(ui, "Motion3 JSON path", &mut self.import_path);
        let ready = !self.import_name.trim().is_empty() && !self.import_path.trim().is_empty() && !self.has_draft();
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("Import Motion3")).clicked() {
                clicks.push(Click::Import);
            }
            if ui.button("Refresh Motion3 list").clicked() {
                clicks.push(Click::Refresh);
            }
        });
    }

    fn select_row(&mut self, ui: &mut egui::Ui, names: &[String], clicks: &mut Vec<Click>) {
        let label = if self.selected.is_empty() { "Select Motion3" } else { self.selected.as_str() };
        ui.add_enabled_ui(!self.has_draft(), |ui| {
            egui::ComboBox::from_label("Motion3").selected_text(label).show_ui(ui, |ui| {
                for name in names {
                    if ui.selectable_label(*name == self.selected, name.as_str()).clicked() {
                        clicks.push(Click::Select(name.clone()));
                    }
                }
            });
        });
        if self.has_draft() {
            ui.label("Apply or discard drafts before changing the Motion3 selection.");
        }
    }

    fn export_row(&mut self, ui: &mut egui::Ui, clicks: &mut Vec<Click>) {
        text_row(ui, "Export path", &mut self.export_path);
        if self.has_draft() {
            ui.label("Apply or discard point and binding drafts before exporting Motion3.");
        }
        ui.horizontal(|ui| {
            let ready = !self.missing && !self.has_draft() && !self.export_path.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Export Motion3")).clicked() {
                clicks.push(Click::Export);
            }
            if ui.add_enabled(!self.missing && !self.has_draft(), egui::Button::new("Reload Motion3")).clicked() {
                clicks.push(Click::Reload);
            }
        });
    }

    fn curve_section(&mut self, ui: &mut egui::Ui, motion: &Value, snapshot: &Value, clicks: &mut Vec<Click>) {
        let list = curves(motion);
        if list.is_empty() {
            ui.label("This motion has no curves.");
            return;
        }
        let curve = self.curve.min(list.len() - 1);
        let drafting = self.has_draft();
        ui.add_enabled_ui(!self.missing && !drafting, |ui| {
            egui::ComboBox::from_label("Curve").selected_text(curve_title(&list[curve], curve)).show_ui(ui, |ui| {
                for (index, item) in list.iter().enumerate() {
                    if ui.selectable_label(index == curve, curve_title(item, index)).clicked() {
                        clicks.push(Click::Curve(index));
                    }
                }
            });
            if let Ok(points) = curve_points(motion, curve) {
                let shown = self.point.as_ref().map_or_else(|| "Select logical point".to_owned(), |p| {
                    point_title(&PointInfo { index: p.number, time: p.original_time, value: p.original_value, kind: p.kind.clone() })
                });
                egui::ComboBox::from_label("Logical point").selected_text(shown).show_ui(ui, |ui| {
                    for point in &points {
                        if ui.selectable_label(point.index == self.point_index, point_title(point)).clicked() {
                            clicks.push(Click::Point(point.index));
                        }
                    }
                });
            }
        });
        ui.label(format!(
            "Target: {}    Curve ID: {}",
            list[curve]["Target"].as_str().unwrap_or(""),
            list[curve]["Id"].as_str().unwrap_or("")
        ));
        if let Some(point) = self.point.as_mut() {
            ui.label(format!("Logical point {}: {}", point.number, point.kind));
            let editable = !self.missing && !point.conflict;
            ui.add_enabled_ui(editable, |ui| {
                number_row(ui, "Time (seconds)", &mut point.time);
                number_row(ui, "Value", &mut point.value);
            });
            let (dirty, conflict) = (point.dirty(), point.conflict);
            ui.horizontal(|ui| {
                if ui.add_enabled(dirty && !conflict && !self.missing, egui::Button::new("Apply point")).clicked() {
                    clicks.push(Click::ApplyPoint);
                }
                let reload = if conflict { "Reload conflicted point" } else { "Reload point" };
                if ui.add_enabled(!self.missing, egui::Button::new(reload)).clicked() {
                    clicks.push(Click::ReloadPoint);
                }
                if ui.button("Discard point draft").clicked() {
                    clicks.push(Click::DiscardPoint);
                }
            });
            if conflict {
                ui.label("This point changed in the document. Reload before applying your draft.");
            }
        }
        self.binding_section(ui, &list[curve], snapshot, clicks);
    }

    fn binding_section(&mut self, ui: &mut egui::Ui, curve: &Value, snapshot: &Value, clicks: &mut Vec<Click>) {
        if curve["Target"] != "Parameter" {
            ui.label("Only Parameter curves can be mapped to INX parameters.");
            return;
        }
        let curve_id = curve["Id"].as_str().unwrap_or_default().to_owned();
        let Some(draft) = self.bindings.clone() else {
            if ui.button("Reload bindings").clicked() {
                clicks.push(Click::ReloadBindings);
            }
            return;
        };
        let existing = binding_for(&draft.rows, &curve_id).cloned();
        if let Some(row) = &existing {
            // The selector follows the stored mapping until the user picks something else.
            if self.bind_parameter == 0 {
                self.bind_parameter = row["parameter"].as_u64().and_then(|id| u32::try_from(id).ok()).unwrap_or(0);
                self.bind_axis = row["axis"].as_i64().unwrap_or(0);
            }
        }
        ui.separator();
        ui.label(format!("Parameter mapping for {curve_id}"));
        let parameters = leaf_parameters(snapshot);
        let chosen = parameters.iter().find(|p| p.uuid == self.bind_parameter);
        let shown = chosen.map_or_else(|| "Select parameter UUID".to_owned(), |p| format!("{} ({})", p.name, p.uuid));
        let locked = draft.conflict || draft.dirty();
        ui.add_enabled_ui(!locked, |ui| {
            egui::ComboBox::from_label("INX parameter").selected_text(shown).show_ui(ui, |ui| {
                for parameter in &parameters {
                    if ui.selectable_label(parameter.uuid == self.bind_parameter, format!("{} ({})", parameter.name, parameter.uuid)).clicked() {
                        clicks.push(Click::BindParameter(parameter.uuid));
                    }
                }
            });
            if chosen.is_some_and(|p| p.is_vec2) {
                egui::ComboBox::from_label("Axis").selected_text(if self.bind_axis == 1 { "Y" } else { "X" }).show_ui(ui, |ui| {
                    for (axis, label) in [(0, "X"), (1, "Y")] {
                        if ui.selectable_label(self.bind_axis == axis, label).clicked() {
                            clicks.push(Click::BindAxis(axis));
                        }
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            let add = if existing.is_some() { "Update binding draft" } else { "Add binding draft" };
            if ui.add_enabled(!draft.conflict && self.bind_parameter != 0, egui::Button::new(add)).clicked() {
                clicks.push(Click::AddBinding);
            }
            if ui.add_enabled(!draft.conflict && existing.is_some(), egui::Button::new("Remove binding draft")).clicked() {
                clicks.push(Click::RemoveBinding);
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(draft.dirty() && !draft.conflict, egui::Button::new("Apply bindings")).clicked() {
                clicks.push(Click::ApplyBindings);
            }
            let reload = if draft.conflict { "Reload conflicted bindings" } else { "Reload bindings" };
            if ui.add_enabled(!self.missing, egui::Button::new(reload)).clicked() {
                clicks.push(Click::ReloadBindings);
            }
            if ui.button("Discard binding draft").clicked() {
                clicks.push(Click::DiscardBindings);
            }
        });
        if draft.dirty() {
            ui.label("Unapplied parameter mappings are retained as a draft.");
        }
        if draft.conflict {
            ui.label("Bindings changed in the document. Reload before applying this draft.");
        }
        match draft.rows.as_array().filter(|rows| !rows.is_empty()) {
            None => {
                ui.label("No parameter curves are mapped yet.");
            }
            Some(rows) => {
                ui.label("Current mapping drafts:");
                for row in rows {
                    ui.label(format!(
                        "{} -> {}{}",
                        row["id"].as_str().unwrap_or("(unknown curve)"),
                        row["parameter"],
                        if row["axis"].as_i64() == Some(1) { " Y" } else { " X" }
                    ));
                }
            }
        }
    }

    fn transport(&mut self, ui: &mut egui::Ui, snapshot: &Value, playing: bool, looping: &mut bool, clicks: &mut Vec<Click>) {
        ui.separator();
        let active = &snapshot["motion3"];
        let current = active["name"].as_str() == Some(self.selected.as_str());
        if current {
            // The editor's preview moves on its own while playing; the field follows it until edited.
            if let Some(time) = active["time"].as_f64().filter(|_| playing) {
                self.seek_draft = time;
            }
        }
        ui.add_enabled_ui(!self.missing, |ui| {
            number_row(ui, "Preview time (seconds)", &mut self.seek_draft);
            if !self.seek_draft.is_finite() {
                ui.colored_label(egui::Color32::LIGHT_RED, "Preview time must be finite.");
            }
            let finite = self.seek_draft.is_finite();
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(finite, egui::Button::new("Sample curves")).clicked() {
                    clicks.push(Click::Sample);
                }
                if ui.add_enabled(finite, egui::Button::new("Evaluate mapped pose")).clicked() {
                    clicks.push(Click::Pose);
                }
                if ui.add_enabled(finite, egui::Button::new("Seek preview")).clicked() {
                    clicks.push(Click::Seek);
                }
                if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
                    clicks.push(Click::PlayPause);
                }
                ui.checkbox(looping, "Loop");
                if ui.button("Stop / reset").clicked() {
                    clicks.push(Click::Reset);
                }
            });
        });
        if current {
            ui.label(format!(
                "Preview {} at {:.6} / {:.6} seconds",
                self.selected,
                active["time"].as_f64().unwrap_or(0.0),
                active["clipEnd"].as_f64().unwrap_or(0.0)
            ));
        } else {
            let length = self.motion.as_ref().map_or(0.0, effective_clip_end);
            ui.label(format!("No active Motion3 preview for {} (clip length {length:.6} seconds).", self.selected));
        }
    }

    fn diagnostics(&mut self, ui: &mut egui::Ui, snapshot: &Value) {
        if let Some(curves) = self.last_sample.as_ref().and_then(|s| s["curves"].as_array()) {
            ui.separator();
            ui.label("Sampled curve values");
            for curve in curves {
                ui.label(row_text(curve, false));
            }
        }
        let active = &snapshot["motion3"];
        let report = if active["name"].as_str() == Some(self.selected.as_str()) { Some(active) } else { self.last_pose.as_ref() };
        let Some(report) = report.filter(|report| report.is_object()) else { return };
        ui.separator();
        for (title, key, applied) in [("Applied parameter curves", "applied", true), ("Unmapped curves", "unmapped", false)] {
            let Some(rows) = report[key].as_array() else { continue };
            ui.label(title);
            if rows.is_empty() {
                ui.label("None");
            }
            for row in rows {
                ui.label(row_text(row, applied));
            }
        }
    }
}

fn text_row(ui: &mut egui::Ui, label: &str, text: &mut String) {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.text_edit_singleline(text).labelled_by(caption.id);
    });
}

fn number_row(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.add(egui::DragValue::new(value).speed(0.01).max_decimals(6)).labelled_by(caption.id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::{assert_all_named, has, temp_dir, texts, Harness};
    use egui::accesskit::Role;

    fn fixture() -> Value {
        json!({"Version":3,"Meta":{"Duration":2,"Fps":30,"Loop":true,
            "AreBeziersRestricted":false,"CurveCount":4,"TotalSegmentCount":6,"TotalPointCount":12,
            "FadeInTime":-1,"UserDataCount":1,"TotalUserDataSize":1},
            "Curves":[
                {"Target":"Parameter","Id":"Linear","Segments":[0,2,0,1,6]},
                {"Target":"Parameter","Id":"Bezier","Segments":[0,0,1,0.2,2,0.9,-1,1,1]},
                {"Target":"Model","Id":"Opacity","Segments":[0,10,2,1,20,0,2,30]},
                {"Target":"PartOpacity","Id":"Arm","Segments":[0,10,3,1,20,2,2,30]}],
            "UserData":[{"Time":0.75,"Value":"a"}]})
    }

    fn point(index: usize, time: f64, value: f64, kind: &str) -> PointInfo {
        PointInfo { index, time, value, kind: kind.to_owned() }
    }

    #[test]
    fn segments_decode_into_logical_points_like_d() {
        let motion = fixture();
        assert_eq!(
            curve_points(&motion, 0).unwrap(),
            [point(0, 0.0, 2.0, "Initial point"), point(1, 1.0, 6.0, "Segment 0: Linear endpoint")]
        );
        assert_eq!(
            curve_points(&motion, 1).unwrap(),
            [
                point(0, 0.0, 0.0, "Initial point"),
                point(1, 0.2, 2.0, "Segment 0: Bezier control 1"),
                point(2, 0.9, -1.0, "Segment 0: Bezier control 2"),
                point(3, 1.0, 1.0, "Segment 0: Bezier endpoint"),
            ]
        );
        assert_eq!(
            curve_points(&motion, 2).unwrap(),
            [
                point(0, 0.0, 10.0, "Initial point"),
                point(1, 1.0, 20.0, "Segment 0: Stepped endpoint"),
                point(2, 2.0, 30.0, "Segment 1: Linear endpoint"),
            ]
        );
        assert_eq!(
            curve_points(&motion, 3).unwrap()[1..],
            [point(1, 1.0, 20.0, "Segment 0: Inverse-stepped endpoint"), point(2, 2.0, 30.0, "Segment 1: Stepped endpoint")]
        );
        let broken = |segments: Value| json!({"Curves": [{"Target": "Parameter", "Id": "X", "Segments": segments}]});
        assert_eq!(curve_points(&broken(json!([0])), 0).unwrap_err(), "Motion3 curve has no initial point");
        assert_eq!(curve_points(&broken(json!([0, 1, 0, 1])), 0).unwrap_err(), "Motion3 point is truncated or nonnumeric");
        assert_eq!(curve_points(&broken(json!([0, 1, 7, 1, 1])), 0).unwrap_err(), "Motion3 segment type is unsupported");
        assert_eq!(curve_points(&broken(json!([0, 1, "a", 1, 1])), 0).unwrap_err(), "Motion3 segment tag is not numeric");
        assert_eq!(curve_points(&motion, 9).unwrap_err(), "Motion3 curve index is out of range");
        assert_eq!(curve_points(&json!({"Curves": [{"Id": "X"}]}), 0).unwrap_err(), "Motion3 curve segments are malformed");
    }

    #[test]
    fn the_clip_end_is_the_larger_of_the_duration_and_the_last_point() {
        assert_eq!(effective_clip_end(&fixture()), 2.0);
        let mut short = fixture();
        short["Meta"]["Duration"] = json!(0.5);
        assert_eq!(effective_clip_end(&short), 2.0);
        short["Meta"]["Duration"] = json!(9);
        assert_eq!(effective_clip_end(&short), 9.0);
        assert_eq!(effective_clip_end(&json!({})), 0.0);
    }

    fn parameters() -> Vec<LeafParameter> {
        vec![
            LeafParameter { uuid: 17, name: "Move".into(), is_vec2: true },
            LeafParameter { uuid: 18, name: "Blink".into(), is_vec2: false },
        ]
    }

    #[test]
    fn binding_choices_are_checked_and_edit_the_rows() {
        let motion = fixture();
        assert_eq!(check_binding(&motion, &parameters(), "Linear", 17, 1), Ok(()));
        assert_eq!(check_binding(&motion, &parameters(), "Linear", 18, 0), Ok(()));
        let err = |id: &str, parameter: u32, axis: i64| check_binding(&motion, &parameters(), id, parameter, axis).unwrap_err();
        assert_eq!(err("Opacity", 17, 0), "Select a Parameter curve with an external Motion3 ID.");
        assert_eq!(err("", 17, 0), "Select a Parameter curve with an external Motion3 ID.");
        assert_eq!(err("Linear", 0, 0), "Choose an existing parameter UUID.");
        assert_eq!(err("Linear", 99, 0), "Choose an existing parameter UUID.");
        assert_eq!(err("Linear", 18, 1), "This 1D parameter only supports X.");
        assert_eq!(err("Linear", 17, 2), "Axis must be X or Y.");

        let rows = json!([{"id": "Bezier", "parameter": 18, "axis": 0, "future": true}]);
        let added = bindings_with(&rows, "Linear", Some((17, 1))).unwrap();
        assert_eq!(added, json!([{"id": "Bezier", "parameter": 18, "axis": 0, "future": true}, {"id": "Linear", "parameter": 17, "axis": 1}]));
        let replaced = bindings_with(&added, "Bezier", Some((17, 0))).unwrap();
        assert_eq!(replaced[0], json!({"id": "Bezier", "parameter": 17, "axis": 0, "future": true}), "unknown fields are kept");
        let removed = bindings_with(&replaced, "Linear", None).unwrap();
        assert_eq!(removed, json!([{"id": "Bezier", "parameter": 17, "axis": 0, "future": true}]));
        assert_eq!(bindings_with(&removed, "Missing", None).unwrap(), removed);
        assert_eq!(bindings_with(&json!({}), "Linear", None).unwrap_err(), "Reload bindings before editing this Motion3 mapping.");
        assert_eq!(bindings_with(&json!([1]), "Linear", Some((17, 0))).unwrap(), json!([1, {"id": "Linear", "parameter": 17, "axis": 0}]));
        assert_eq!(binding_for(&removed, "Bezier").unwrap()["parameter"], 17);
    }

    // ---- the panel on a real controller

    struct Rig {
        controller: EditorController,
        panel: Motion3Panel,
        harness: Harness,
        looping: bool,
        dir: std::path::PathBuf,
        parameter: u64,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let dir = temp_dir(name);
            let mut controller = EditorController::new();
            let made = controller.core_request(&json!({"op": "parameter.create", "name": "Move", "dimensions": 2, "min": [-10, -10], "max": [10, 10]}));
            assert_eq!(made["succeeded"], true, "{made}");
            let parameter = made["result"]["uuid"].as_u64().unwrap();
            std::fs::write(dir.join("wave.motion3.json"), fixture().to_string()).unwrap();
            let mut harness = Harness::new();
            harness.screen = egui::vec2(900.0, 5000.0);
            Self { controller, panel: Motion3Panel::new(), harness, looping: false, dir, parameter }
        }

        fn draw(&mut self) -> crate::editor_test_support::Out {
            let (panel, controller, looping) = (&mut self.panel, &mut self.controller, &mut self.looping);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        panel.show(ui, controller, looping);
                    });
                });
            });
            let (panel, controller, looping) = (&mut self.panel, &mut self.controller, &mut self.looping);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        panel.show(ui, controller, looping);
                    });
                });
            })
        }

        fn click(&mut self, role: Role, label: &str) -> crate::editor_test_support::Out {
            let (panel, controller, looping) = (&mut self.panel, &mut self.controller, &mut self.looping);
            self.harness.click(role, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        panel.show(ui, controller, looping);
                    });
                });
            })
        }

        fn import(&mut self) {
            self.panel.import_name = "Wave".into();
            self.panel.import_path = self.dir.join("wave.motion3.json").display().to_string();
            self.click(Role::Button, "Import Motion3");
        }

        fn motion(&mut self) -> Value {
            let response = self.controller.core_request(&json!({"op": "motion3.read", "name": "Wave"}));
            response["result"].clone()
        }

        fn snapshot(&self) -> Value {
            self.controller.snapshot().clone()
        }
    }

    #[test]
    fn every_control_of_the_panel_has_a_name_in_each_state() {
        let mut rig = Rig::new("motion3-names");
        let output = rig.draw();
        assert!(has(&output, Role::Button, "Import Motion3") && has(&output, Role::Button, "Refresh Motion3 list"));
        assert!(has(&output, Role::TextInput, "Motion3 name") && has(&output, Role::TextInput, "Motion3 JSON path"));
        assert!(texts(&output).iter().any(|t| t == "No Motion3 motions are imported."));
        assert_all_named(&output, "empty panel");

        rig.import();
        assert_eq!(rig.panel.selected(), "Wave", "{}", rig.panel.error);
        let output = rig.draw();
        for label in [
            "Export Motion3", "Reload Motion3", "Apply point", "Reload point", "Discard point draft", "Add binding draft",
            "Remove binding draft", "Apply bindings", "Reload bindings", "Discard binding draft", "Sample curves",
            "Evaluate mapped pose", "Seek preview", "Play", "Stop / reset",
        ] {
            assert!(has(&output, Role::Button, label), "{label} in {:?}", texts(&output));
        }
        assert!(has(&output, Role::CheckBox, "Loop"));
        for label in ["Motion3", "Curve", "Logical point", "INX parameter"] {
            assert!(has(&output, Role::ComboBox, label), "{label}");
        }
        for label in ["Export path", "Time (seconds)", "Value", "Preview time (seconds)"] {
            assert!(texts(&output).iter().any(|t| t == label), "{label}");
        }
        let checked = assert_all_named(&output, "selected motion");
        assert!(checked >= 20, "{checked}");
    }

    #[test]
    fn importing_selecting_and_listing_work_and_a_bad_path_reports_without_changing_anything() {
        let mut rig = Rig::new("motion3-import");
        rig.panel.import_name = "Wave".into();
        rig.panel.import_path = rig.dir.join("missing.json").display().to_string();
        let before = rig.snapshot();
        rig.click(Role::Button, "Import Motion3");
        assert!(rig.panel.error.contains("could not open Motion3"), "{}", rig.panel.error);
        assert_eq!(rig.snapshot(), before);
        rig.import();
        assert_eq!(rig.panel.status, "Motion3 imported.");
        assert_eq!(motion_names(&rig.snapshot()), ["Wave"]);
        rig.click(Role::Button, "Refresh Motion3 list");
        assert_eq!(rig.panel.status, "Motion3 list refreshed (1).");
        // Importing the same name again is refused by the session.
        rig.click(Role::Button, "Import Motion3");
        assert!(rig.panel.error.contains("already exists"), "{}", rig.panel.error);
    }

    #[test]
    fn a_point_draft_applies_as_one_undo_step_blocks_switching_and_detects_conflicts() {
        let mut rig = Rig::new("motion3-point");
        rig.import();
        rig.draw();
        let draft = rig.panel.point.as_ref().unwrap();
        assert_eq!((draft.time, draft.value, draft.kind.as_str()), (0.0, 2.0, "Initial point"));
        assert!(!rig.panel.has_draft());
        // Move to the endpoint of the Linear curve and change it.
        let motion = rig.motion();
        rig.panel.load_point(&motion, 0, 1).unwrap();
        rig.panel.point.as_mut().unwrap().value = 9.5;
        assert!(rig.panel.has_draft());
        let revision = rig.snapshot()["revision"].clone();
        rig.click(Role::Button, "Export Motion3");
        assert_eq!(rig.snapshot()["revision"], revision);
        rig.panel.export_path = rig.dir.join("out.json").display().to_string();
        rig.draw();
        // Export is not offered while a draft is open.
        assert!(!rig.dir.join("out.json").exists());

        rig.click(Role::Button, "Apply point");
        assert_eq!(rig.panel.status, "Motion3 point applied.", "{}", rig.panel.error);
        assert!(!rig.panel.has_draft());
        assert_eq!(rig.motion()["Curves"][0]["Segments"], json!([0, 2, 0, 1, 9.5]));
        assert_eq!(rig.snapshot()["revision"].as_u64().unwrap(), revision.as_u64().unwrap() + 1, "one revision per Apply");
        rig.controller.core_request(&json!({"op": "undo"}));
        assert_eq!(rig.motion()["Curves"][0]["Segments"], json!([0, 2, 0, 1, 6]), "one undo restores it");

        // A change made elsewhere turns an open draft into a conflict that cannot be applied.
        rig.draw();
        let motion = rig.motion();
        rig.panel.load_point(&motion, 0, 1).unwrap();
        rig.panel.point.as_mut().unwrap().time = 1.5;
        rig.controller.core_request(&json!({"op": "motion3.point.set", "name": "Wave", "curve": 0, "point": 1, "value": 7.0}));
        rig.draw();
        assert!(rig.panel.point.as_ref().unwrap().conflict);
        let revision = rig.snapshot()["revision"].clone();
        rig.click(Role::Button, "Apply point");
        assert!(rig.panel.error.is_empty() || rig.panel.error.contains("stale"), "{}", rig.panel.error);
        assert_eq!(rig.snapshot()["revision"], revision, "the stale draft was not applied");
        rig.click(Role::Button, "Reload conflicted point");
        let reloaded = rig.panel.point.as_ref().unwrap();
        assert!(!reloaded.conflict && !rig.panel.has_draft());
        assert_eq!((reloaded.time, reloaded.value), (1.0, 7.0), "the document's point is back in the draft");

        // Non-finite input is refused at Apply.
        rig.panel.point.as_mut().unwrap().value = f64::NAN;
        let revision = rig.snapshot()["revision"].clone();
        rig.click(Role::Button, "Apply point");
        assert_eq!(rig.panel.error, "Point time and value must be finite numbers.");
        assert_eq!(rig.snapshot()["revision"], revision);
        rig.click(Role::Button, "Discard point draft");
        assert!(!rig.panel.has_draft());
    }

    #[test]
    fn bindings_are_drafted_applied_and_conflicts_are_caught() {
        let mut rig = Rig::new("motion3-bindings");
        rig.import();
        rig.draw();
        // Choose the Linear curve and the Move parameter on its Y axis.
        rig.panel.bind_parameter = rig.parameter as u32;
        rig.panel.bind_axis = 1;
        rig.click(Role::Button, "Add binding draft");
        assert_eq!(
            rig.panel.bindings.as_ref().unwrap().rows,
            json!([{"id": "Linear", "parameter": rig.parameter, "axis": 1}])
        );
        assert!(rig.panel.has_draft(), "an unapplied mapping is a draft");
        let revision = rig.snapshot()["revision"].clone();
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t.starts_with("Linear -> ") && t.ends_with(" Y")), "{:?}", texts(&output));
        assert_eq!(rig.snapshot()["revision"], revision, "a draft changes nothing in the document");

        rig.click(Role::Button, "Apply bindings");
        assert_eq!(rig.panel.status, "Motion3 bindings applied.", "{}", rig.panel.error);
        assert!(!rig.panel.has_draft());
        assert_eq!(rig.snapshot()["model"]["live2d"]["motionBindings"]["Wave"], json!([{"id": "Linear", "parameter": rig.parameter, "axis": 1}]));
        assert_eq!(rig.snapshot()["revision"].as_u64().unwrap(), revision.as_u64().unwrap() + 1);

        // Remove draft, then a change elsewhere conflicts with it.
        rig.draw();
        rig.click(Role::Button, "Remove binding draft");
        assert_eq!(rig.panel.bindings.as_ref().unwrap().rows, json!([]));
        rig.controller.core_request(&json!({"op": "motion3.bind", "name": "Wave", "bindings": [{"id": "Linear", "parameter": rig.parameter, "axis": 0}]}));
        rig.draw();
        assert!(rig.panel.bindings.as_ref().unwrap().conflict);
        let revision = rig.snapshot()["revision"].clone();
        rig.click(Role::Button, "Apply bindings");
        assert_eq!(rig.snapshot()["revision"], revision);
        rig.click(Role::Button, "Reload conflicted bindings");
        assert!(!rig.panel.has_draft());
        assert_eq!(rig.panel.bindings.as_ref().unwrap().rows, json!([{"id": "Linear", "parameter": rig.parameter, "axis": 0}]));
        // A 2D parameter's axis choice and an unknown parameter are refused before any draft is made.
        rig.panel.bind_parameter = 4242;
        rig.click(Role::Button, "Update binding draft");
        assert_eq!(rig.panel.error, "Choose an existing parameter UUID.");
        assert!(!rig.panel.has_draft());
        // The Opacity curve is not a Parameter curve: no mapping is offered for it.
        let motion = rig.motion();
        rig.panel.load_point(&motion, 2, 0).unwrap();
        rig.panel.curve = 2;
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t == "Only Parameter curves can be mapped to INX parameters."));
    }

    #[test]
    fn sampling_pose_seek_play_and_export_use_the_session_ops() {
        let mut rig = Rig::new("motion3-transport");
        rig.import();
        rig.controller.core_request(&json!({"op": "motion3.bind", "name": "Wave", "bindings": [{"id": "Linear", "parameter": rig.parameter, "axis": 0}]}));
        rig.draw();
        rig.panel.seek_draft = 0.5;
        rig.click(Role::Button, "Sample curves");
        assert_eq!(rig.panel.status, "Motion3 curves sampled.");
        let sample = rig.panel.last_sample.clone().unwrap();
        let linear = sample["curves"].as_array().unwrap().iter().find(|c| c["id"] == "Linear").unwrap();
        assert_eq!(linear["value"], 4.0, "halfway between 2 and 6");
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t == "Parameter / Linear = 4"), "{:?}", texts(&output));

        rig.click(Role::Button, "Evaluate mapped pose");
        assert_eq!(rig.panel.status, "Motion3 mapped pose evaluated.", "{}", rig.panel.error);
        assert_eq!(rig.panel.last_pose.as_ref().unwrap()["time"], 0.5);
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t.starts_with("Linear -> ")), "{:?}", texts(&output));
        assert!(texts(&output).iter().any(|t| t == "Applied parameter curves") && texts(&output).iter().any(|t| t == "Unmapped curves"));

        let revision = rig.snapshot()["revision"].clone();
        rig.click(Role::Button, "Seek preview");
        assert_eq!(rig.snapshot()["motion3"]["name"], "Wave");
        assert_eq!(rig.snapshot()["motion3"]["time"], 0.5);
        assert_eq!(rig.snapshot()["revision"], revision, "the preview is not an edit");
        rig.draw();
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t.starts_with("Preview Wave at 0.500000 / 2.000000")), "{:?}", texts(&output));

        // Play starts the controller's clock and its tick advances the INX Motion3 preview.
        let output = rig.click(Role::Button, "Play");
        assert!(rig.controller.playing(), "{}", rig.panel.error);
        assert!(has(&output, Role::Button, "Pause"));
        rig.controller.tick(0.25, false).expect("a tick");
        assert!((rig.snapshot()["motion3"]["time"].as_f64().unwrap() - 0.6).abs() < 1e-9, "the tick caps dt at 0.1");
        rig.click(Role::Button, "Pause");
        assert!(!rig.controller.playing());
        rig.click(Role::Button, "Stop / reset");
        assert!(rig.snapshot()["motion3"].is_null());

        let target = rig.dir.join("exported.motion3.json");
        rig.panel.export_path = target.display().to_string();
        rig.click(Role::Button, "Export Motion3");
        assert_eq!(rig.panel.status, "Motion3 exported.");
        let exported: Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(exported["Curves"][0]["Id"], "Linear");
        // Reload re-reads the motion from the document.
        rig.click(Role::Button, "Reload Motion3");
        assert_eq!(rig.panel.status, "Motion3 reloaded.");
    }

    #[test]
    fn a_playback_tick_caps_its_dt_and_steps_assigned_inx_physics() {
        let mut rig = Rig::new("motion3-tick");
        rig.import();
        rig.controller.core_request(&json!({"op": "motion3.bind", "name": "Wave", "bindings": [{"id": "Linear", "parameter": rig.parameter, "axis": 0}]}));
        rig.draw();
        let physics = rig.controller.core_request(&json!({"op": "node.create", "className": "SimplePhysics", "parents": [1]}));
        let node = physics["result"]["created"][0]["uuid"].as_u64().unwrap();
        let set = rig.controller.core_request(&json!({"op": "physics.set", "node": node, "param": rig.parameter}));
        assert_eq!(set["succeeded"], true, "{set}");
        rig.click(Role::Button, "Seek preview");
        rig.click(Role::Button, "Play");
        assert!(rig.controller.playing(), "{}", rig.panel.error);
        assert_eq!(rig.snapshot()["physics"]["active"], false, "seeking resets physics");
        let start = rig.snapshot()["motion3"]["time"].as_f64().unwrap();
        // A stalled frame advances the clip by the cap only, and assigned INX physics is stepped in the tick.
        let tick = rig.controller.tick(1.0, false).expect("a tick");
        assert_eq!(tick["succeeded"], true, "{tick}");
        let moved = rig.snapshot()["motion3"]["time"].as_f64().unwrap() - start;
        assert!((moved - 0.1).abs() < 1e-9, "{moved}");
        assert_eq!(rig.snapshot()["physics"]["active"], true, "advance alone would have dropped the physics result");
        assert!(rig.controller.take_physics_stepped());
        assert!(!rig.controller.take_physics_stepped(), "the flag is consumed once");
        // With "Enable physics" off the tick leaves physics alone.
        rig.controller.playback_physics = false;
        assert_eq!(rig.controller.tick(0.05, false).expect("a tick")["succeeded"], true);
        assert!(!rig.controller.take_physics_stepped());
        assert_eq!(rig.snapshot()["physics"]["active"], false);
        // The request itself keeps the dt it is given.
        let before = rig.snapshot()["motion3"]["time"].as_f64().unwrap();
        let advanced = rig.controller.core_request(&json!({"op": "motion3.advance", "dt": 0.4}));
        assert_eq!(advanced["succeeded"], true, "{advanced}");
        assert!((rig.snapshot()["motion3"]["time"].as_f64().unwrap() - before - 0.4).abs() < 1e-9);
    }

    #[test]
    fn a_removed_motion_keeps_a_dirty_draft_and_a_new_document_resets_the_panel() {
        let mut rig = Rig::new("motion3-reset");
        rig.import();
        rig.draw();
        rig.panel.point.as_mut().unwrap().value = 3.0;
        // A different document (new) takes the motion away; the editor refuses the replacement while a draft is open.
        rig.controller.allow_replace_unsaved(true);
        assert_eq!(rig.controller.core_request(&json!({"op": "new"}))["succeeded"], true);
        rig.draw();
        assert!(rig.panel.has_draft(), "the draft is retained");
        assert!(rig.panel.point.as_ref().unwrap().conflict);
        assert_eq!(rig.panel.error, "The selected Motion3 was removed; discard its retained draft before switching.");
        rig.click(Role::Button, "Discard point draft");
        rig.draw();
        assert_eq!(rig.panel.selected(), "", "a clean panel drops the vanished selection");
        rig.import();
        rig.panel.reset();
        assert_eq!(rig.panel.selected(), "");
        assert!(!rig.panel.has_draft());
    }
}

