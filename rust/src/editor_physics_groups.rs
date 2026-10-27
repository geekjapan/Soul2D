//! The CMO3 physics group editor of the outline (PHY01, issue #236): the typed view `live2d.physics3.read`
//! returns, rendered as editable rows. Every commit is one `live2d.physics3.edit` request guarded by
//! `expectedRevision`, so a document changed elsewhere is refused instead of overwritten. A DragValue keeps a
//! draft in `drag` until it is let go, as the timeline's value fields do, so a drag stays one undo step.
//! The first group can be created only in an existing editable empty settings list.
//! Each group also has a Presets section for the per-user presets of its inputs or pendulum vertices: Save and
//! Delete write the settings directory (not the document, so no `expectedRevision` and no undo step); Apply is a
//! guarded `live2d.physics3.edit` with `preset`.

use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;

#[derive(Default)]
pub struct PhysicsGroups {
    /// The `live2d.physics3.read` answer and the revision it was read at; a change refetches.
    view: Option<Value>,
    error: Option<String>,
    revision: u64,
    read: bool,
    /// The DragValue being edited: its field key and current value until the field is let go.
    drag: Option<(String, f64, f64)>,
    /// Name text drafts, one slot per group of the current view.
    names: Vec<Option<String>>,
    /// The `live2d.physics3.preset.list` answer, `{inputs: {name: ..}, vertices: {name: ..}}`; read once and again
    /// after each save or delete, since a preset write does not move the revision.
    presets: Value,
    presets_read: bool,
    /// The Presets section: the list it acts on (false inputs, true vertices) and the name typed for it.
    preset_vertices: bool,
    preset_name: String,
    /// A preset save or delete the section asked for; it carries no `expectedRevision`, so it skips `pending`.
    preset_request: Option<Value>,
}

/// One DragValue that commits only when let go, keeping the typed value in `drag` while it is edited.
fn drag_field(
    ui: &mut egui::Ui,
    drag: &mut Option<(String, f64, f64)>,
    pending: &mut Option<Value>,
    enabled: bool,
    key: String,
    label: &str,
    stored: f64,
    range: std::ops::RangeInclusive<f64>,
    integer: bool,
    request: impl FnOnce(f64) -> Value,
) {
    let mut value = drag
        .as_ref()
        .filter(|(held, _, _)| *held == key)
        .map_or(stored, |(_, held, _)| *held);
    let label = ui.label(label);
    let field = egui::DragValue::new(&mut value).speed(if integer { 1.0 } else { 0.01 }).range(range);
    let field = if integer { field.min_decimals(0).max_decimals(0) } else { field };
    let response = ui.add_enabled(enabled, field).labelled_by(label.id);
    if response.changed() {
        *drag = Some((key.clone(), value, stored));
    }
    if response.drag_stopped() || response.lost_focus() {
        if let Some((held, _, _)) = drag.take() {
            if held == key && value != stored && pending.is_none() {
                *pending = Some(request(value));
            }
        }
    }
}

/// A parameter picker for `source`/`destination`: the ID goes to the request, the name is what is shown.
fn parameter_combo(
    ui: &mut egui::Ui,
    pending: &mut Option<Value>,
    enabled: bool,
    salt: &str,
    label: &str,
    current: &str,
    parameters: &[(String, String)],
    request: impl Fn(String) -> Value,
) {
    let label_widget = ui.label(label);
    egui::ComboBox::from_id_salt(salt)
        .selected_text(
            parameters
                .iter()
                .find(|(id, _)| id == current)
                .map(|(id, name)| format!("{name} ({id})"))
                .unwrap_or_else(|| current.to_owned()),
        )
        .show_ui(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                for (id, name) in parameters {
                    if ui
                        .selectable_label(id == current, format!("{name} ({id})"))
                        .clicked()
                        && pending.is_none()
                    {
                        *pending = Some(request(id.clone()));
                    }
                }
            });
        })
        .response
        .labelled_by(label_widget.id);
}

fn row_enabled(view: &Value, group: &Value, item: Option<&Value>) -> bool {
    view["editable"].as_bool() == Some(true)
        && group["editable"].as_bool() == Some(true)
        && item.is_none_or(|item| item["editable"].as_bool() == Some(true))
}

impl PhysicsGroups {
    pub fn has_draft(&self) -> bool {
        self.drag.as_ref().is_some_and(|(_, value, original)| value != original)
            || self.names.iter().enumerate().any(|(group, draft)| draft.as_ref().is_some_and(|draft|
                draft != self.view.as_ref().and_then(|view| view["groups"][group]["name"].as_str()).unwrap_or("")))
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Renders the panel; answers true when a request changed the document (the preview wants a redraw).
    pub fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController) -> bool {
        let revision = controller.snapshot()["revision"].as_u64().unwrap_or(0);
        if !self.read || self.revision != revision {
            self.read = true;
            self.revision = revision;
            self.drag = None;
            let response = controller.with_session(|session| {
                session.handle_request(&json!({"op": "live2d.physics3.read"}))
            });
            if response["succeeded"].as_bool() == Some(true) {
                let view = response["result"].clone();
                self.names = vec![None; view["groups"].as_array().map_or(0, Vec::len)];
                self.view = Some(view);
                self.error = None;
            } else {
                self.view = None;
                self.error = Some(
                    response["message"].as_str().unwrap_or("physics read failed").to_owned(),
                );
            }
        }
        if !self.presets_read {
            self.read_presets(controller);
        }
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error.clone());
        }
        let Some(view) = self.view.clone() else {
            if self.error.is_none() {
                ui.label("Physics settings unavailable");
            }
            return false;
        };
        if view["present"].as_bool() != Some(true) {
            ui.label("No physics settings in this CMO3");
            return false;
        }
        let parameters: Vec<(String, String)> = controller.snapshot()["model"]["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|parameter| {
                Some((
                    parameter["id"].as_str()?.to_owned(),
                    parameter["name"].as_str().unwrap_or("").to_owned(),
                ))
            })
            .collect();
        let mut pending = None;
        if ui.button("Make physics settings independent")
            .on_hover_text("Copy the complete physics settings. Preserve original shared definitions. One undo step; already independent settings stay unchanged.")
            .clicked()
        {
            pending = Some(json!({"op":"live2d.physics3.edit", "detach":true}));
        }
        self.set_row(ui, &view, &mut pending);
        if view["groups"].as_array().is_some_and(Vec::is_empty)
            && ui.add_enabled(view["editable"] == true, egui::Button::new("Create first group")).clicked()
            && pending.is_none()
        {
            pending = Some(json!({"op":"live2d.physics3.edit", "insert":0, "value":{
                "name":"Physics", "normalization":{
                    "position":{"min":-10.0,"default":0.0,"max":10.0},
                    "angle":{"min":-30.0,"default":0.0,"max":30.0}},
                "inputs":[], "outputs":[], "vertices":[]}}));
        }
        for (g, group) in view["groups"].as_array().into_iter().flatten().enumerate() {
            self.group_ui(ui, g, group, &view, &parameters, &mut pending);
        }
        if let Some(request) = self.preset_request.take() {
            let response = controller.core_request(&request);
            self.error = (response["succeeded"].as_bool() != Some(true)).then(|| {
                response["message"].as_str().unwrap_or("physics preset request failed").to_owned()
            });
            self.read_presets(controller);
        }
        let Some(mut request) = pending else {
            return false;
        };
        request["expectedRevision"] = json!(revision);
        let response = controller.core_request(&request);
        if response["succeeded"].as_bool() == Some(true) {
            self.error = None;
            true
        } else {
            self.error =
                Some(response["message"].as_str().unwrap_or("physics edit failed").to_owned());
            false
        }
    }

    /// Reads the saved presets; a refused list (an invalid store file) leaves none and shows its message.
    fn read_presets(&mut self, controller: &mut EditorController) {
        self.presets_read = true;
        let response = controller.with_session(|session| {
            session.handle_request(&json!({"op": "live2d.physics3.preset.list"}))
        });
        if response["succeeded"].as_bool() == Some(true) {
            self.presets = response["result"].clone();
        } else {
            self.presets = Value::Null;
            self.error =
                Some(response["message"].as_str().unwrap_or("physics presets cannot be read").to_owned());
        }
    }

    fn set_row(&mut self, ui: &mut egui::Ui, view: &Value, pending: &mut Option<Value>) {
        let enabled = view["editable"].as_bool() == Some(true);
        ui.horizontal(|ui| {
            match view["fps"].as_u64() {
                Some(fps) => {
                    drag_field(ui, &mut self.drag, pending, enabled, "fps".to_owned(), "FPS",
                        fps as f64, 1.0..=240.0, true, |value| {
                            json!({"op": "live2d.physics3.edit", "field": "fps",
                                "value": value as u64})
                        });
                    if ui
                        .add_enabled(enabled, egui::Button::new("Clear FPS"))
                        .clicked()
                        && pending.is_none()
                    {
                        *pending = Some(json!({"op": "live2d.physics3.edit", "field": "fps",
                            "value": null}));
                    }
                }
                None => {
                    ui.label("FPS unset");
                    if ui
                        .add_enabled(enabled, egui::Button::new("Set FPS to 60"))
                        .clicked()
                        && pending.is_none()
                    {
                        *pending = Some(json!({"op": "live2d.physics3.edit", "field": "fps",
                            "value": 60}));
                    }
                }
            }
        });
    }

    fn group_ui(
        &mut self,
        ui: &mut egui::Ui,
        g: usize,
        group: &Value,
        view: &Value,
        parameters: &[(String, String)],
        pending: &mut Option<Value>,
    ) {
        let name = group["name"].as_str().unwrap_or("");
        let editable = row_enabled(view, group, None);
        egui::CollapsingHeader::new(format!("{name} (group {g})"))
            .id_salt(("physics-group", g))
            .show(ui, |ui| {
                ui.push_id(("physics-group", g), |ui| {
                    if !editable {
                        ui.label((group["shared"].as_bool() == Some(true))
                            .then_some("Shared storage: read only")
                            .unwrap_or("Read only"));
                    }
                    ui.horizontal(|ui| {
                        let label = ui.label("Name");
                        let draft = self
                            .names
                            .get_mut(g)
                            .map(|slot| slot.get_or_insert_with(|| name.to_owned()));
                        if let Some(draft) = draft {
                            let response = ui
                                .add_enabled(editable, egui::TextEdit::singleline(draft))
                                .labelled_by(label.id);
                            if response.lost_focus()
                                && *draft != name
                                && pending.is_none()
                            {
                                *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                                    "field": "name", "value": *draft}));
                            }
                        }
                        if ui
                            .add_enabled(editable, egui::Button::new(format!("Copy group {g}")))
                            .on_hover_text("Adds a copy of this group at the end; one undo step")
                            .clicked()
                            && pending.is_none()
                        {
                            *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                                "duplicate": true}));
                        }
                        if ui
                            .add_enabled(editable, egui::Button::new(format!("Delete group {g}")))
                            .on_hover_text("Removing a group is one undo step")
                            .clicked()
                            && pending.is_none()
                        {
                            *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                                "remove": true}));
                        }
                    });
                    ui.horizontal(|ui| {
                        for (part, part_label) in
                            [("position", "Position"), ("angle", "Angle")]
                        {
                            for bound in ["min", "default", "max"] {
                                let stored = group["normalization"][part][bound]
                                    .as_f64()
                                    .unwrap_or(0.0);
                                let key = format!("{g}.{part}.{bound}");
                                drag_field(ui, &mut self.drag, pending, editable, key,
                                    &format!("{part_label} {bound}"), stored,
                                    -1e6..=1e6, false, |value| {
                                        json!({"op": "live2d.physics3.edit", "group": g,
                                            "field": format!("normalization.{part}.{bound}"),
                                            "value": value})
                                    });
                            }
                        }
                    });
                    self.list_ui(ui, g, group, "inputs", "input", view, parameters, pending);
                    self.list_ui(ui, g, group, "outputs", "output", view, parameters, pending);
                    self.list_ui(ui, g, group, "vertices", "vertex", view, parameters, pending);
                    self.preset_ui(ui, g, editable, pending);
                });
            });
    }

    fn preset_ui(&mut self, ui: &mut egui::Ui, g: usize, editable: bool, pending: &mut Option<Value>) {
        let shown = ui.collapsing("Presets", |ui| {
            ui.push_id(("physics-presets", g), |ui| {
                let list = if self.preset_vertices { "vertices" } else { "inputs" };
                ui.horizontal(|ui| {
                    for (vertices, choice) in [(false, "inputs"), (true, "vertices")] {
                        let text = format!("Group {g}: preset {choice}");
                        if ui.add(egui::Button::selectable(self.preset_vertices == vertices, text)).clicked() {
                            self.preset_vertices = vertices;
                        }
                    }
                });
                let saved: Vec<String> =
                    self.presets[list].as_object().into_iter().flatten().map(|(name, _)| name.clone()).collect();
                for name in &saved {
                    let text = format!("Group {g}: use {list} preset {name}");
                    if ui.add(egui::Button::selectable(*name == self.preset_name, text)).clicked() {
                        self.preset_name.clone_from(name);
                    }
                }
                if saved.is_empty() {
                    ui.label(format!("No saved {list} preset"));
                }
                ui.horizontal(|ui| {
                    let label = ui.label("Preset name");
                    ui.text_edit_singleline(&mut self.preset_name).labelled_by(label.id);
                });
                let name = self.preset_name.trim().to_owned();
                let exists = saved.contains(&name);
                ui.horizontal(|ui| {
                    let verb = if exists { "replace" } else { "save" };
                    let save = egui::Button::new(format!("Group {g}: {verb} preset"));
                    if ui
                        .add_enabled(!name.is_empty(), save)
                        .on_hover_text("Keeps this group's list under the name for this user; not an undo step")
                        .clicked()
                        && self.preset_request.is_none()
                    {
                        self.preset_request = Some(json!({"op": "live2d.physics3.preset.save", "list": list,
                            "name": name, "group": g, "overwrite": exists}));
                    }
                    if ui
                        .add_enabled(editable && exists, egui::Button::new(format!("Group {g}: apply preset")))
                        .on_hover_text("Makes this group's list the saved preset; one undo step")
                        .clicked()
                        && pending.is_none()
                    {
                        *pending = Some(json!({"op": "live2d.physics3.edit", "group": g, "list": list,
                            "preset": name}));
                    }
                    if ui
                        .add_enabled(exists, egui::Button::new(format!("Group {g}: delete preset")))
                        .on_hover_text("Removes the saved preset; this cannot be undone")
                        .clicked()
                        && self.preset_request.is_none()
                    {
                        self.preset_request =
                            Some(json!({"op": "live2d.physics3.preset.delete", "list": list, "name": name}));
                    }
                });
            });
        });
        // The store is per user and other writers (agent, MCP, another editor) do not move the revision.
        if shown.header_response.clicked() {
            self.presets_read = false;
        }
    }

    fn list_ui(
        &mut self,
        ui: &mut egui::Ui,
        g: usize,
        group: &Value,
        list: &str,
        kind: &str,
        view: &Value,
        parameters: &[(String, String)],
        pending: &mut Option<Value>,
    ) {
        let items = group[list].as_array().cloned().unwrap_or_default();
        let vertex_count = group["vertices"].as_array().map_or(0, Vec::len);
        let editable = row_enabled(view, group, None);
        ui.collapsing(format!("{} {} ({})", kind, list, items.len()), |ui| {
            ui.push_id(("physics-list", g, list), |ui| {
                for (i, item) in items.iter().enumerate() {
                    let enabled = row_enabled(view, group, Some(item));
                    ui.push_id(("physics-item", g, list, i), |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("{kind} {i}"));
                            match list {
                                "inputs" | "outputs" => self.link_ui(
                                    ui, g, list, i, item, enabled, vertex_count, parameters, pending,
                                ),
                                _ => self.vertex_ui(ui, g, i, item, enabled, pending),
                            }
                            if ui
                                .add_enabled(enabled, egui::Button::new(
                                    format!("Group {g}: remove {kind} {i}")))
                                .clicked()
                                && pending.is_none()
                            {
                                *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                                    "list": list, "index": i, "remove": true}));
                            }
                        });
                    });
                }
                if items.is_empty() {
                    ui.label(format!("No {list}"));
                }
                let vertex_count = group["vertices"].as_array().map_or(0, Vec::len);
                let first = parameters.first().map(|(id, _)| id.clone());
                let add = match (list, first) {
                    ("inputs", Some(source)) => Some(json!({"source": source, "type": "X",
                        "scale": 10.0, "weight": 100.0, "reflect": false})),
                    ("outputs", Some(destination)) if vertex_count > 1 =>
                        Some(json!({"destination": destination, "vertexIndex": 1,
                            "scale": 10.0, "weight": 100.0, "reflect": false})),
                    ("vertices", _) => Some(json!({"x": 0.0, "y": 0.0, "mobility": 1.0,
                        "delay": 0.5, "acceleration": 1.0, "radius": 1.0})),
                    _ => None,
                };
                if let Some(add) = add {
                    if ui
                        .add_enabled(editable, egui::Button::new(format!("Group {g}: add {kind}")))
                        .clicked()
                        && pending.is_none()
                    {
                        *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                            "list": list, "insert": items.len(), "value": add}));
                    }
                }
            });
        });
    }

    fn link_ui(
        &mut self,
        ui: &mut egui::Ui,
        g: usize,
        list: &str,
        i: usize,
        item: &Value,
        enabled: bool,
        vertex_count: usize,
        parameters: &[(String, String)],
        pending: &mut Option<Value>,
    ) {
        let field = if list == "inputs" { "source" } else { "destination" };
        let current = item[field].as_str().unwrap_or("").to_owned();
        parameter_combo(ui, pending, enabled, &format!("{g}-{list}-{i}-{field}"), field,
            &current, parameters, |id| {
                json!({"op": "live2d.physics3.edit", "group": g, "list": list, "index": i,
                    "field": field, "value": id})
            });
        if list == "inputs" {
            let kind = item["type"].as_str().unwrap_or("");
            let label = ui.label("type");
            egui::ComboBox::from_id_salt(("input-type", g, i))
                .selected_text(if kind.is_empty() { "unknown" } else { kind })
                .show_ui(ui, |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        for choice in ["X", "Angle"] {
                            if ui.selectable_label(kind == choice, choice).clicked()
                                && pending.is_none()
                            {
                                *pending = Some(json!({"op": "live2d.physics3.edit", "group": g,
                                    "list": "inputs", "index": i, "field": "type",
                                    "value": choice}));
                            }
                        }
                    });
                })
                .response
                .labelled_by(label.id);
        } else {
            ui.label("type Angle");
        }
        for (field, range) in [("weight", 0.0..=100.0), ("scale", -1e6..=1e6)] {
            let stored = item[field].as_f64().unwrap_or(0.0);
            let key = format!("{g}.{list}.{i}.{field}");
            drag_field(ui, &mut self.drag, pending, enabled, key, field, stored, range, false,
                |value| json!({"op": "live2d.physics3.edit", "group": g, "list": list,
                    "index": i, "field": field, "value": value}));
        }
        if list == "outputs" {
            let stored = item["vertexIndex"].as_u64().unwrap_or(1) as f64;
            let key = format!("{g}.outputs.{i}.vertexIndex");
            drag_field(ui, &mut self.drag, pending, enabled, key, "vertexIndex", stored,
                1.0..=(vertex_count as f64 - 1.0).max(1.0), true,
                |value| json!({"op": "live2d.physics3.edit", "group": g,
                    "list": "outputs", "index": i, "field": "vertexIndex",
                    "value": value as u64}));
        }
        let mut reflect = item["reflect"].as_bool() == Some(true);
        if ui
            .add_enabled(enabled, egui::Checkbox::new(&mut reflect, "reflect"))
            .changed()
            && pending.is_none()
        {
            *pending = Some(json!({"op": "live2d.physics3.edit", "group": g, "list": list,
                "index": i, "field": "reflect", "value": reflect}));
        }
    }

    fn vertex_ui(
        &mut self,
        ui: &mut egui::Ui,
        g: usize,
        i: usize,
        item: &Value,
        enabled: bool,
        pending: &mut Option<Value>,
    ) {
        for (field, range) in [
            ("x", -1e6..=1e6),
            ("y", -1e6..=1e6),
            ("mobility", 0.0..=1.0),
            ("delay", 0.0..=1e6),
            ("acceleration", 0.0..=1e6),
            ("radius", 0.0..=1e6),
        ] {
            let stored = item[field].as_f64().unwrap_or(0.0);
            let key = format!("{g}.vertices.{i}.{field}");
            drag_field(ui, &mut self.drag, pending, enabled, key, field, stored, range, false,
                |value| json!({"op": "live2d.physics3.edit", "group": g, "list": "vertices",
                    "index": i, "field": field, "value": value}));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_quit_physics_drafts_ignore_opened_rows_and_store_operands() {
        let mut groups = super::PhysicsGroups::default();
        groups.view = Some(serde_json::json!({"groups":[{"name":"Group"}]}));
        groups.names = vec![Some("Group".into())];
        groups.preset_name = "Store operand".into();
        assert!(!groups.has_draft());
        groups.names[0] = Some("Group ".into()); assert!(groups.has_draft());
        groups.names[0] = Some("Group".into()); assert!(!groups.has_draft());
        groups.drag = Some(("fps".into(), 61.0, 60.0)); assert!(groups.has_draft());
        groups.drag = Some(("fps".into(), 60.0, 60.0)); assert!(!groups.has_draft());
        groups.reset(); assert!(!groups.has_draft());
    }

    use super::*;
    use crate::editor_test_support::{
        assert_all_named, cmo3_physics_bytes, find, has, temp_dir, texts, Harness,
    };
    use egui::accesskit::Role;

    struct Rig {
        controller: EditorController,
        panel: PhysicsGroups,
        harness: Harness,
    }

    impl Rig {
        fn new(name: &str, bytes: Vec<u8>) -> Self {
            let dir = temp_dir(name);
            std::fs::write(dir.join("fixture.cmo3"), bytes).unwrap();
            let mut controller = EditorController::new();
            let opened = controller.core_request(
                &json!({"op": "open", "path": dir.join("fixture.cmo3").to_str().unwrap()}));
            assert_eq!(opened["succeeded"], true, "{opened}");
            let mut harness = Harness::new();
            harness.screen = egui::vec2(1100.0, 1800.0);
            Self { controller, panel: PhysicsGroups::default(), harness }
        }

        fn draw(&mut self) -> crate::editor_test_support::Out {
            let (panel, controller) = (&mut self.panel, &mut self.controller);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show(ui, controller);
                });
            });
            let (panel, controller) = (&mut self.panel, &mut self.controller);
            self.harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show(ui, controller);
                });
            })
        }

        fn click(&mut self, role: Role, label: &str) {
            let (panel, controller) = (&mut self.panel, &mut self.controller);
            self.harness.click(role, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show(ui, controller);
                });
            });
        }

        fn send(&mut self, events: Vec<egui::Event>) {
            let (panel, controller) = (&mut self.panel, &mut self.controller);
            self.harness.frame(events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show(ui, controller);
                });
            });
        }

        fn view(&self) -> Value {
            self.panel.view.clone().unwrap()
        }

        fn revision(&self) -> u64 {
            self.controller.snapshot()["revision"].as_u64().unwrap()
        }
    }

    #[test]
    fn the_panel_explicitly_detaches_shared_physics_in_one_undo_step() {
        let container = soul2d_core::caff::CaffDocument::parse(&cmo3_physics_bytes()).unwrap();
        let xml = String::from_utf8(container.main_xml()).unwrap()
            .replacen("<CPhysicsVertex>", "<CPhysicsVertex xs.id='shared-physics-vertex'>", 1)
            .replace("</root>", "<CPhysicsVertex xs.ref='shared-physics-vertex'/></root>");
        let mut rig = Rig::new("physics-detach", container.replace_main_xml(xml.as_bytes()).unwrap());
        let revision = rig.revision();
        let before = rig.controller.snapshot()["model"].clone();
        rig.draw();
        assert_eq!(rig.view()["groups"][0]["vertices"][0]["shared"], true);
        rig.click(Role::Button, "Make physics settings independent");
        rig.draw();
        assert_eq!(rig.revision(), revision + 1, "{:?}", rig.panel.error);
        assert_eq!(rig.view()["groups"][0]["vertices"][0]["shared"], false);
        let detached = rig.controller.snapshot().clone();
        rig.click(Role::Button, "Make physics settings independent");
        assert_eq!(rig.controller.snapshot(), &detached);
        rig.controller.core_request(&json!({"op":"undo"}));
        assert_eq!(rig.controller.snapshot()["model"], before);
        rig.draw();
        assert_eq!(rig.view()["groups"][0]["vertices"][0]["shared"], true);
    }

    #[test]
    fn the_panel_creates_the_first_group_with_shared_history() {
        let mut rig = Rig::new("physics-first-group", cmo3_physics_bytes());
        let revision = rig.revision();
        let removed = rig.controller.core_request(&json!({"op":"live2d.physics3.edit",
            "expectedRevision":revision,"group":0,"remove":true}));
        assert_eq!(removed["succeeded"], true, "{removed}");
        let before = rig.controller.snapshot()["model"].clone();
        let revision = rig.revision();
        let output = rig.draw();
        assert!(has(&output, Role::Button, "Create first group"));
        rig.click(Role::Button, "Create first group");
        assert_eq!(rig.revision(), revision + 1);
        assert_eq!(rig.view()["groups"][0]["name"], "Physics");
        rig.controller.core_request(&json!({"op":"undo"}));
        assert_eq!(rig.controller.snapshot()["model"], before);
        rig.draw();
        assert_eq!(rig.view()["groups"], json!([]));
        rig.controller.core_request(&json!({"op":"redo"}));
        rig.draw();
        assert_eq!(rig.view()["groups"][0]["name"], "Physics");
    }

    #[test]
    fn the_panel_lists_the_group_fields_with_named_controls() {
        let mut rig = Rig::new("physics-panel", cmo3_physics_bytes());
        rig.click(Role::Button, "Sway (group 0)");
        rig.click(Role::Button, "input inputs (1)");
        rig.click(Role::Button, "output outputs (1)");
        rig.click(Role::Button, "vertex vertices (2)");
        let output = rig.draw();
        assert_eq!(rig.view()["present"], true);
        assert_eq!(rig.view()["groups"][0]["name"], "Sway");
        for label in ["FPS", "Name", "Position min", "Angle max", "vertexIndex", "reflect"] {
            assert!(texts(&output).iter().any(|text| text == label), "{label}: {:?}", texts(&output));
        }
        assert!(has(&output, Role::Button, "Delete group 0"));
        assert!(has(&output, Role::Button, "Copy group 0"));
        assert!(has(&output, Role::Button, "Group 0: add vertex"));
        rig.click(Role::Button, "Copy group 0");
        assert_eq!(rig.view()["groups"].as_array().unwrap().len(), 2, "{:?}", rig.panel.error);
        assert_eq!(rig.view()["groups"][1]["name"], "Sway");
        assert!(assert_all_named(&output, "physics groups") >= 10);
    }

    #[test]
    fn clearing_the_rate_and_removing_a_vertex_are_guarded_edits() {
        let mut rig = Rig::new("physics-edits", cmo3_physics_bytes());
        rig.draw();
        let start = rig.revision();
        rig.click(Role::Button, "Clear FPS");
        assert_eq!(rig.revision(), start + 1);
        assert!(rig.view()["fps"].is_null(), "{}", rig.view());
        rig.controller.core_request(&json!({"op": "undo"}));
        rig.draw();
        assert_eq!(rig.view()["fps"], 60);
        rig.click(Role::Button, "Sway (group 0)");
        rig.click(Role::Button, "vertex vertices (2)");
        rig.click(Role::Button, "Group 0: remove vertex 1");
        let error = rig.panel.error.clone().unwrap_or_default();
        assert!(error.contains("at least two vertices"), "{error}");
        assert_eq!(rig.view()["groups"][0]["vertices"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_panel_edit_survives_a_save_and_reopen() {
        let name = "physics-save";
        let dir = temp_dir(name);
        let source = dir.join("fixture.cmo3");
        std::fs::write(&source, cmo3_physics_bytes()).unwrap();
        let mut rig = Rig::new(name, cmo3_physics_bytes());
        rig.click(Role::Button, "Sway (group 0)");
        rig.click(Role::Button, "input inputs (1)");
        rig.click(Role::Button, "Group 0: add input");
        let view = rig.view();
        assert_eq!(view["groups"][0]["inputs"].as_array().unwrap().len(), 2);
        assert_eq!(view["groups"][0]["inputs"][1]["source"], "ParamTest");
        let saved = rig.controller.core_request(&json!({"op": "save", "path": source.to_str().unwrap()}));
        assert_eq!(saved["succeeded"], true, "{saved}");
        drop(rig);
        let mut rig = Rig::new(name, std::fs::read(&source).unwrap());
        rig.draw();
        assert_eq!(rig.view()["groups"][0]["inputs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_document_without_physics_says_so() {
        let mut rig = Rig::new("physics-empty", crate::editor_test_support::cmo3_bytes());
        let output = rig.draw();
        assert!(texts(&output).iter().any(|text| text == "No physics settings in this CMO3"),
            "{:?}", texts(&output));
    }

    #[test]
    fn reset_forces_a_reread_at_the_same_revision() {
        let mut rig = Rig::new("physics-reset", cmo3_physics_bytes());
        rig.draw();
        assert_eq!(rig.view()["fps"], 60);
        rig.panel.error = Some("stale".into());
        rig.panel.names = vec![Some("Stale".into())];
        rig.panel.drag = Some(("fps".into(), 1.0, 60.0));
        rig.panel.reset();
        assert!(rig.panel.view.is_none());
        assert!(rig.panel.error.is_none());
        assert!(rig.panel.drag.is_none());
        assert!(rig.panel.names.is_empty());
        assert!(!rig.panel.read);
        assert_eq!(rig.panel.revision, 0);
        rig.draw();
        assert_eq!(rig.view()["fps"], 60);
        assert_eq!(rig.view()["groups"][0]["name"], "Sway");
        assert!(rig.panel.error.is_none());
        assert!(rig.panel.drag.is_none());
    }

    fn open_presets(rig: &mut Rig) {
        rig.click(Role::Button, "Sway (group 0)");
        rig.click(Role::Button, "Presets");
    }

    #[test]
    fn a_group_list_is_saved_applied_replaced_and_deleted_as_a_preset() {
        let mut rig = Rig::new("physics-presets", cmo3_physics_bytes());
        let settings = temp_dir("physics-presets-settings");
        rig.controller.with_session(|session| session.set_config_dir(settings.clone()));
        open_presets(&mut rig);
        let output = rig.draw();
        assert!(texts(&output).iter().any(|text| text == "No saved inputs preset"), "{:?}", texts(&output));
        assert!(find(&output, Role::Button, "Group 0: save preset").unwrap().is_disabled(), "no name yet");
        let outside = json!({"op": "live2d.physics3.preset.save", "list": "inputs", "name": "Outside", "group": 0});
        assert_eq!(rig.controller.core_request(&outside)["succeeded"], true);
        rig.click(Role::Button, "Presets");
        rig.click(Role::Button, "Presets");
        let output = rig.draw();
        assert!(find(&output, Role::Button, "Group 0: use inputs preset Outside").is_some(), "reopening re-reads");
        let weight = rig.view()["groups"][0]["inputs"][0]["weight"].as_f64().unwrap();
        rig.panel.preset_name = "Calm".into();
        let start = rig.revision();
        rig.click(Role::Button, "Group 0: save preset");
        assert!(rig.panel.error.is_none(), "{:?}", rig.panel.error);
        assert!(settings.join("physics3-input-presets.json").is_file());
        assert_eq!(rig.panel.presets["inputs"]["Calm"]["items"][0]["weight"], weight);
        assert_eq!(rig.revision(), start, "a preset save is no document change");
        let other = if weight > 1.0 { weight - 1.0 } else { weight + 1.0 };
        let edit = json!({"op": "live2d.physics3.edit", "expectedRevision": start, "group": 0, "list": "inputs",
            "index": 0, "field": "weight", "value": other});
        assert_eq!(rig.controller.core_request(&edit)["succeeded"], true);
        rig.click(Role::Button, "Group 0: apply preset");
        assert!(rig.panel.error.is_none(), "{:?}", rig.panel.error);
        assert_eq!(rig.revision(), start + 2);
        assert_eq!(rig.view()["groups"][0]["inputs"][0]["weight"].as_f64().unwrap(), weight);
        rig.click(Role::Button, "Group 0: replace preset");
        assert!(rig.panel.error.is_none(), "{:?}", rig.panel.error);
        rig.click(Role::Button, "Group 0: preset vertices");
        assert!(!rig.panel.presets["vertices"].as_object().is_some_and(|saved| saved.contains_key("Calm")));
        rig.click(Role::Button, "Group 0: save preset");
        assert!(settings.join("physics3-model-presets.json").is_file());
        assert_eq!(rig.panel.presets["vertices"]["Calm"]["items"].as_array().unwrap().len(), 2);
        rig.click(Role::Button, "Group 0: delete preset");
        assert!(rig.panel.presets["vertices"].as_object().unwrap().is_empty());
        assert!(rig.panel.presets["inputs"]["Calm"].is_object(), "the other kind is kept");
    }

    #[test]
    fn a_preset_without_a_settings_directory_or_a_saved_name_is_refused_in_the_panel() {
        let mut rig = Rig::new("physics-presets-refused", cmo3_physics_bytes());
        open_presets(&mut rig);
        let output = rig.draw();
        for label in ["Group 0: apply preset", "Group 0: delete preset"] {
            assert!(find(&output, Role::Button, label).unwrap().is_disabled(), "{label}");
        }
        rig.panel.preset_name = "Calm".into();
        rig.click(Role::Button, "Group 0: save preset");
        let error = rig.panel.error.clone().unwrap_or_default();
        assert!(error.contains("settings directory"), "{error}");
    }

    #[test]
    fn dragging_the_rate_commits_once_on_release() {
        let mut rig = Rig::new("physics-fps-drag", cmo3_physics_bytes());
        rig.draw();
        let start = rig.revision();
        let at = crate::editor_test_support::center(&rig.draw(), Role::SpinButton, "FPS");
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        rig.send(vec![egui::Event::PointerMoved(at), press(true)]);
        rig.send(vec![egui::Event::PointerMoved(at + egui::vec2(4.0, 0.0))]);
        rig.send(vec![egui::Event::PointerMoved(at + egui::vec2(8.0, 0.0))]);
        assert_eq!(rig.revision(), start, "the drag has not committed");
        rig.send(vec![press(false)]);
        rig.draw();
        assert_eq!(rig.revision(), start + 1);
        assert_ne!(rig.view()["fps"], 60);
        rig.controller.core_request(&json!({"op": "undo"}));
        rig.draw();
        assert_eq!(rig.view()["fps"], 60);
    }
}
