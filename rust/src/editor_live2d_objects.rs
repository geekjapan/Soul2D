use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;

fn objects(model: &Value) -> Vec<(&Value, bool)> {
    let list = |name: &str, parameter: bool| {
        model[name].as_array().into_iter().flatten().map(move |entry| (entry, parameter)).collect::<Vec<_>>()
    };
    [list("sources", false), list("parameters", true)].concat()
}

fn label(entry: &Value, parameter: bool) -> String {
    let name = entry["name"].as_str().filter(|name| !name.is_empty()).or_else(|| entry["id"].as_str()).unwrap_or("?");
    let kind = if parameter { "Parameter" } else { entry["data"]["type"].as_str().unwrap_or("Source") };
    format!("{name} [{kind}]")
}

struct FieldDraft<T> {
    value: T,
    original: T,
}

impl<T: Clone + PartialEq> FieldDraft<T> {
    fn new(value: T) -> Self { Self { original: value.clone(), value } }
    fn dirty(&self) -> bool { self.value != self.original }
    fn accept(&mut self, fresh: T, applied: bool) {
        if applied || !self.dirty() {
            *self = Self::new(fresh);
        }
    }
}

fn accept_optional<T: Clone + PartialEq>(
    current: &mut Option<FieldDraft<T>>, fresh: Option<FieldDraft<T>>, applied: bool,
) {
    if let Some(fresh) = fresh {
        if let Some(current) = current {
            current.accept(fresh.value, applied);
        } else {
            *current = Some(fresh);
        }
    } else if applied || !current.as_ref().is_some_and(FieldDraft::dirty) {
        *current = None;
    }
}

enum ObjectDetails {
    Source {
        kind: String,
        parent: Option<FieldDraft<String>>,
        rotation: Option<FieldDraft<Option<String>>>,
    },
    Parameter { range: FieldDraft<[f64; 3]> },
}

struct LoadedObject {
    guid: String,
    revision: u64,
    name: FieldDraft<String>,
    id: FieldDraft<String>,
    details: ObjectDetails,
}

#[derive(Clone, Copy)]
enum ObjectAction {
    ApplyName, ApplyId, Move, Retarget, ApplyRange, Duplicate, AddKey, DeleteKey, Delete,
}

enum Observation<'a> {
    Current(&'a Value),
    Stale(&'a Value),
    Missing,
}

impl LoadedObject {
    fn from_entry(entry: &Value, parameter: bool, revision: u64) -> Self {
        let kind = entry["data"]["type"].as_str().unwrap_or_default().to_owned();
        let details = if parameter {
            ObjectDetails::Parameter {
                range: FieldDraft::new(["minimum", "maximum", "default"].map(|key| entry[key].as_f64().unwrap_or_default())),
            }
        } else {
            ObjectDetails::Source {
                rotation: (kind == "ArtMesh").then(|| FieldDraft::new(
                    entry["targetDeformerGuid"].as_str().filter(|guid| !guid.is_empty()).map(str::to_owned))),
                parent: entry["parentPartGuid"].as_str().map(|parent| FieldDraft::new(parent.to_owned())),
                kind,
            }
        };
        Self {
            guid: entry["guid"].as_str().unwrap_or_default().to_owned(), revision,
            name: FieldDraft::new(entry["name"].as_str().unwrap_or_default().to_owned()),
            id: FieldDraft::new(entry["id"].as_str().unwrap_or_default().to_owned()), details,
        }
    }

    fn parameter(&self) -> bool { matches!(self.details, ObjectDetails::Parameter { .. }) }

    fn matches(&self, entry: &Value, parameter: bool) -> bool {
        entry["guid"].as_str() == Some(self.guid.as_str()) && match &self.details {
            ObjectDetails::Parameter { .. } => parameter,
            ObjectDetails::Source { kind, .. } => !parameter && entry["data"]["type"].as_str().unwrap_or_default() == kind,
        }
    }

    fn find<'a>(&self, model: &'a Value) -> Option<&'a Value> {
        objects(model).into_iter().find(|(entry, parameter)| self.matches(entry, *parameter)).map(|(entry, _)| entry)
    }

    fn observe<'a>(&self, snapshot: &'a Value) -> Observation<'a> {
        match self.find(&snapshot["model"]) {
            Some(entry) if snapshot["revision"].as_u64() == Some(self.revision) => Observation::Current(entry),
            Some(entry) => Observation::Stale(entry),
            None => Observation::Missing,
        }
    }

    fn has_draft(&self) -> bool {
        self.name.dirty() || self.id.dirty() || match &self.details {
            ObjectDetails::Parameter { range } => range.dirty(),
            ObjectDetails::Source { parent, rotation, .. } =>
                parent.as_ref().is_some_and(FieldDraft::dirty) || rotation.as_ref().is_some_and(FieldDraft::dirty),
        }
    }

    fn accept(&mut self, action: ObjectAction, entry: &Value, revision: u64) {
        let fresh = Self::from_entry(entry, self.parameter(), revision);
        self.name.accept(fresh.name.value, matches!(action, ObjectAction::ApplyName));
        self.id.accept(fresh.id.value, matches!(action, ObjectAction::ApplyId));
        match (&mut self.details, fresh.details) {
            (ObjectDetails::Parameter { range }, ObjectDetails::Parameter { range: fresh }) =>
                range.accept(fresh.value, matches!(action, ObjectAction::ApplyRange)),
            (ObjectDetails::Source { parent, rotation, .. }, ObjectDetails::Source { parent: new_parent, rotation: new_rotation, .. }) => {
                accept_optional(parent, new_parent, matches!(action, ObjectAction::Move));
                accept_optional(rotation, new_rotation, matches!(action, ObjectAction::Retarget));
            }
            _ => unreachable!("accepted object kind was checked before incorporation"),
        }
        self.revision = revision;
    }
}

#[derive(Default)]
pub struct ObjectEditor {
    loaded: Option<LoadedObject>,
    axis: String,
    key: f64,
    status: String,
    error: String,
}

impl ObjectEditor {
    pub fn new() -> Self { Self::default() }
    pub fn reset(&mut self) { *self = Self::default(); }
    pub fn has_draft(&self) -> bool { self.loaded.as_ref().is_some_and(LoadedObject::has_draft) }

    fn normalize_axis(&mut self, entry: &Value) {
        let axes = entry["grid"]["axes"].as_array().map(Vec::as_slice).unwrap_or_default();
        if !axes.iter().any(|axis| axis["parameterGuid"] == self.axis.as_str()) {
            self.axis = axes.first().and_then(|axis| axis["parameterGuid"].as_str()).unwrap_or_default().to_owned();
        }
    }

    fn load(&mut self, entry: &Value, parameter: bool, revision: u64) {
        self.loaded = Some(LoadedObject::from_entry(entry, parameter, revision));
        self.normalize_axis(entry);
    }

    fn reconcile(&mut self, snapshot: &Value) {
        if self.has_draft() || self.loaded.as_ref().is_some_and(|loaded| matches!(loaded.observe(snapshot), Observation::Current(_))) {
            return;
        }
        let entries = objects(&snapshot["model"]);
        let selected = self.loaded.as_ref().and_then(|loaded| entries.iter().copied()
            .find(|(entry, parameter)| loaded.matches(entry, *parameter))).or_else(|| entries.first().copied());
        if let Some((entry, parameter)) = selected {
            if self.loaded.is_some() { self.error.clear(); self.status.clear(); }
            self.load(entry, parameter, snapshot["revision"].as_u64().unwrap_or(0));
        } else {
            self.loaded = None;
            self.axis.clear();
        }
    }

    fn reload_discard(&mut self, snapshot: &Value) {
        let entries = objects(&snapshot["model"]);
        let selected = self.loaded.as_ref().and_then(|loaded| entries.iter().copied()
            .find(|(entry, parameter)| loaded.matches(entry, *parameter))).or_else(|| entries.first().copied());
        if let Some((entry, parameter)) = selected {
            self.load(entry, parameter, snapshot["revision"].as_u64().unwrap_or(0));
        } else {
            self.loaded = None;
            self.axis.clear();
        }
        self.error.clear();
        self.status.clear();
    }

    fn request(&self, action: ObjectAction) -> Result<Value, String> {
        let loaded = self.loaded.as_ref().ok_or("No Live2D object is loaded")?;
        let mut request = match action {
            ObjectAction::ApplyName => json!({"op":"live2d.rename","guid":loaded.guid,"name":loaded.name.value}),
            ObjectAction::ApplyId => json!({"op":"live2d.set_id","guid":loaded.guid,"id":loaded.id.value}),
            ObjectAction::Move => {
                let ObjectDetails::Source { parent: Some(parent), .. } = &loaded.details else { return Err("This object has no editable Parent Part".into()) };
                json!({"op":"live2d.reparent","guid":loaded.guid,"parent":parent.value})
            }
            ObjectAction::Retarget => {
                let ObjectDetails::Source { rotation: Some(rotation), .. } = &loaded.details else { return Err("This object has no Rotation destination".into()) };
                json!({"op":"live2d.reparent_deformer","sourceGuid":loaded.guid,"targetDeformerGuid":rotation.value})
            }
            ObjectAction::ApplyRange => {
                let ObjectDetails::Parameter { range } = &loaded.details else { return Err("This object is not a parameter".into()) };
                if !range.value.iter().all(|value| value.is_finite()) { return Err("Parameter range values must be finite".into()); }
                let [minimum, maximum, default] = range.value;
                json!({"op":"live2d.parameter.edit","guid":loaded.guid,"minimum":minimum,"maximum":maximum,"default":default})
            }
            ObjectAction::Duplicate => json!({"op":"live2d.duplicate","guid":loaded.guid}),
            ObjectAction::AddKey | ObjectAction::DeleteKey => json!({
                "op":if matches!(action,ObjectAction::AddKey) {"live2d.keyform.add"} else {"live2d.keyform.delete"},
                "sourceGuid":loaded.guid,"parameterGuid":self.axis,"key":self.key}),
            ObjectAction::Delete => json!({
                "op":if loaded.parameter() {"live2d.parameter.delete"} else {"live2d.delete"},"guid":loaded.guid}),
        };
        request["expectedRevision"] = json!(loaded.revision);
        Ok(request)
    }

    fn dispatch(&mut self, action: ObjectAction, controller: &mut EditorController) -> bool {
        let Some(loaded) = &self.loaded else { return false };
        if !matches!(loaded.observe(controller.snapshot()), Observation::Current(_)) {
            self.error = "Object changed elsewhere. Your input is preserved; reload and discard before applying.".into();
            return false;
        }
        if matches!(action, ObjectAction::Delete) && loaded.has_draft() {
            self.error = "Apply or explicitly discard all object edits before deleting the object.".into();
            return false;
        }
        let before = loaded.revision;
        let request = match self.request(action) {
            Ok(request) => request,
            Err(message) => { self.error = message; self.status.clear(); return false; }
        };
        let response = controller.core_request(&request);
        if response["succeeded"] != true {
            self.status.clear();
            self.error = response["message"].as_str().unwrap_or("Live2D object edit failed").to_owned();
            return false;
        }
        self.error.clear();
        self.status = if matches!(action, ObjectAction::Retarget) && controller.snapshot()["revision"] == before {
            "Rotation target unchanged"
        } else { "Live2D object updated" }.into();
        if matches!(action, ObjectAction::Delete) {
            self.loaded = None;
        } else {
            let snapshot = controller.snapshot().clone();
            let entry = self.loaded.as_ref().and_then(|loaded| loaded.find(&snapshot["model"]));
            if let Some(entry) = entry {
                self.loaded.as_mut().unwrap().accept(action, entry, snapshot["revision"].as_u64().unwrap_or(before));
                self.normalize_axis(entry);
            } else {
                self.error = "The action succeeded but its object is unavailable. Pending input is preserved.".into();
            }
        }
        true
    }

    pub fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController) -> bool {
        let snapshot = controller.snapshot().clone();
        self.reconcile(&snapshot);
        let model = &snapshot["model"];
        let revision = snapshot["revision"].as_u64().unwrap_or(0);
        let entries = objects(model);
        let Some(loaded) = &self.loaded else {
            ui.label("No sources or parameters are available");
            return false;
        };
        let selected = loaded.find(model);
        let shown = selected.map(|entry| label(entry, loaded.parameter()))
            .unwrap_or_else(|| format!("Unavailable object [{}]", loaded.guid));
        let mut chosen = None;
        ui.add_enabled_ui(!loaded.has_draft(), |ui| {
            egui::ComboBox::from_label("Object").selected_text(shown).show_ui(ui, |ui| {
                for &(entry, parameter) in &entries {
                    if ui.selectable_label(loaded.matches(entry, parameter), label(entry, parameter)).clicked() {
                        chosen = Some((entry, parameter));
                    }
                }
            });
        });
        if let Some((entry, parameter)) = chosen {
            if !self.loaded.as_ref().unwrap().matches(entry, parameter) {
                self.load(entry, parameter, revision);
                self.error.clear(); self.status.clear();
            }
        }
        let loaded = self.loaded.as_mut().unwrap();
        let (current, selected) = match loaded.observe(&snapshot) {
            Observation::Current(entry) => (true, Some(entry)),
            Observation::Stale(entry) => (false, Some(entry)),
            Observation::Missing => (false, None),
        };
        if !current {
            ui.colored_label(egui::Color32::LIGHT_RED, format!(
                "Object {} changed or is unavailable. Input from revision {} is preserved; reload and discard before applying.",
                loaded.guid, loaded.revision));
        }
        let guid = loaded.guid.clone();
        let mut action = None;
        for (caption, field, button, apply) in [
            ("Name", &mut loaded.name.value, "Apply name", ObjectAction::ApplyName),
            ("ID", &mut loaded.id.value, "Apply ID", ObjectAction::ApplyId),
        ] {
            ui.horizontal(|ui| {
                let text = ui.label(caption);
                ui.text_edit_singleline(field).labelled_by(text.id);
                if ui.add_enabled(current, egui::Button::new(button)).clicked() { action = Some(apply); }
            });
        }
        let sources = model["sources"].as_array().map(Vec::as_slice).unwrap_or_default();
        match &mut loaded.details {
            ObjectDetails::Parameter { range } => {
                for (caption, value) in ["Minimum", "Maximum", "Default"].into_iter().zip(range.value.iter_mut()) {
                    ui.horizontal(|ui| {
                        let text = ui.label(caption);
                        ui.add(egui::DragValue::new(value).speed(0.1).max_decimals(4)).labelled_by(text.id);
                    });
                }
                if ui.add_enabled(current, egui::Button::new("Apply range")).clicked() { action = Some(ObjectAction::ApplyRange); }
            }
            ObjectDetails::Source { parent, rotation, .. } => {
                if let Some(parent) = parent {
                    let parts: Vec<&Value> = sources.iter().filter(|source| source["data"]["type"] == "Part" && source["guid"] != guid.as_str()).collect();
                    let shown = parts.iter().find(|part| part["guid"] == parent.value.as_str())
                        .map_or_else(|| format!("Unavailable: {}", parent.value), |part| label(part, false));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_label("Parent Part").selected_text(shown).show_ui(ui, |ui| {
                            for part in &parts {
                                ui.selectable_value(&mut parent.value, part["guid"].as_str().unwrap_or_default().to_owned(), label(part, false));
                            }
                        });
                        if ui.add_enabled(current, egui::Button::new("Move")).clicked() { action = Some(ObjectAction::Move); }
                    });
                }
                if let Some(rotation) = rotation {
                    let shown = selected.map(|entry| {
                        entry["targetDeformerGuid"].as_str().filter(|guid| !guid.is_empty()).map(|guid| {
                            sources.iter().find(|source| source["guid"] == guid)
                                .map_or_else(|| format!("Unlisted target: {guid}"), |source| label(source, false))
                        }).unwrap_or_else(|| "None".into())
                    }).unwrap_or_else(|| "Unavailable".into());
                    ui.label(format!("Current target: {shown}"));
                    let rotations: Vec<&Value> = sources.iter().filter(|source| source["data"]["type"] == "Rotation"
                        && source["guid"].as_str().is_some_and(|guid| !guid.is_empty())).collect();
                    let rotation_label = |source: &Value| {
                        let text = label(source, false);
                        if rotations.iter().any(|other| other["guid"] != source["guid"] && label(other, false) == text) {
                            format!("{text} ({})", source["guid"].as_str().unwrap())
                        } else { text }
                    };
                    let shown = rotation.value.as_deref().map(|guid| {
                        rotations.iter().find(|source| source["guid"] == guid)
                            .map_or_else(|| format!("Unavailable: {guid}"), |source| rotation_label(source))
                    }).unwrap_or_else(|| "Choose a Rotation".into());
                    egui::ComboBox::from_label("Rotation destination").selected_text(shown).show_ui(ui, |ui| {
                        for source in &rotations {
                            ui.selectable_value(&mut rotation.value, Some(source["guid"].as_str().unwrap().to_owned()), rotation_label(source))
                                .on_hover_text(source["guid"].as_str().unwrap());
                        }
                    });
                    let chosen = rotations.iter().any(|source| source["guid"].as_str() == rotation.value.as_deref());
                    if ui.add_enabled(current && chosen, egui::Button::new("Reparent to Rotation")).clicked() { action = Some(ObjectAction::Retarget); }
                    if rotations.is_empty() { ui.label("No Rotation destinations are available"); }
                    ui.label("Static edits only. Eligibility is checked when applied; unsupported chains are refused without changes.");
                }
            }
        }
        let axes = selected.and_then(|entry| entry["grid"]["axes"].as_array()).map(Vec::as_slice).unwrap_or_default();
        if !loaded.parameter() && !axes.is_empty() {
            let name = |guid: &str| model["parameters"].as_array().into_iter().flatten()
                .find(|parameter| parameter["guid"] == guid).and_then(|parameter| parameter["id"].as_str()).unwrap_or(guid).to_owned();
            let keys = axes.iter().find(|axis| axis["parameterGuid"] == self.axis.as_str())
                .and_then(|axis| axis["keys"].as_array()).map(|keys| keys.iter().map(Value::to_string).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            egui::ComboBox::from_label("Key axis").selected_text(format!("{} [{keys}]", name(&self.axis))).show_ui(ui, |ui| {
                for axis in axes {
                    let guid = axis["parameterGuid"].as_str().unwrap_or_default();
                    ui.selectable_value(&mut self.axis, guid.to_owned(), name(guid));
                }
            });
            ui.horizontal(|ui| {
                let text = ui.label("Key");
                ui.add(egui::DragValue::new(&mut self.key).speed(0.1).max_decimals(4)).labelled_by(text.id);
                for (button, selected) in [("Add key", ObjectAction::AddKey), ("Delete key", ObjectAction::DeleteKey)] {
                    if ui.add_enabled(current, egui::Button::new(button)).clicked() { action = Some(selected); }
                }
            });
        }
        if ui.add_enabled(current, egui::Button::new("Duplicate")).clicked() { action = Some(ObjectAction::Duplicate); }
        if ui.add_enabled(current && !loaded.has_draft(), egui::Button::new("Delete"))
            .on_hover_text("Delete an object nothing else uses; Undo restores it").clicked() { action = Some(ObjectAction::Delete); }
        if loaded.has_draft() { ui.label("Apply or discard all object edits before selecting another object or deleting it."); }
        if !self.error.is_empty() { ui.colored_label(egui::Color32::LIGHT_RED, self.error.as_str()); }
        if !self.status.is_empty() { ui.label(self.status.as_str()); }
        ui.label("Reload discards Name, ID, Parent Part, Rotation destination and parameter range edits.");
        if ui.button("Reload and discard all object edits").clicked() {
            self.reload_discard(&snapshot);
            return false;
        }
        action.is_some_and(|action| self.dispatch(action, controller))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_quit_object_drafts_compare_loaded_authored_values_and_reverts() {
        let mut editor = super::ObjectEditor::new();
        editor.load(&serde_json::json!({"guid":"test","name":"Name","id":"ID","parentPartGuid":"parent",
            "targetDeformerGuid":"rotation","data":{"type":"ArtMesh"}}), false, 1);
        assert!(!editor.has_draft());
        draft_mut(&mut editor).name.value.push(' '); assert!(editor.has_draft());
        draft_mut(&mut editor).name.value = draft(&editor).name.original.clone();
        draft_mut(&mut editor).id.value.push('x'); assert!(editor.has_draft());
        draft_mut(&mut editor).id.value = draft(&editor).id.original.clone();
        parent_mut(&mut editor).value.push('x'); assert!(editor.has_draft());
        parent_mut(&mut editor).value = parent(&editor).original.clone();
        rotation_mut(&mut editor).value = Some("other-guid".into()); assert!(editor.has_draft());
        rotation_mut(&mut editor).value = rotation(&editor).original.clone();
        assert!(!editor.has_draft());
        editor.load(&json!({"guid":"parameter","name":"Name","id":"ID","minimum":-1,"maximum":1,"default":0}),true,1);
        for index in 0..3 {
            range_mut(&mut editor).value[index] += 0.5; assert!(editor.has_draft());
            let original = range_mut(&mut editor).original; range_mut(&mut editor).value = original;
        }
        assert!(!editor.has_draft());
        draft_mut(&mut editor).name.value.push('x'); draft_mut(&mut editor).revision += 1;
        assert!(editor.has_draft(), "invalidating a read is not applying another field's draft");
        editor.reset(); assert!(!editor.has_draft());
    }

    use super::*;
    use crate::editor_test_support::{assert_all_named, cmo3_bytes, has, keyed_cmo3_bytes, temp_dir, two_part_cmo3_bytes, Harness};
    use egui::accesskit::Role;

    fn draft(editor: &ObjectEditor) -> &LoadedObject { editor.loaded.as_ref().unwrap() }
    fn draft_mut(editor: &mut ObjectEditor) -> &mut LoadedObject { editor.loaded.as_mut().unwrap() }
    fn parent(editor: &ObjectEditor) -> &FieldDraft<String> {
        let ObjectDetails::Source { parent:Some(parent),.. } = &draft(editor).details else { panic!("expected Parent Part") }; parent
    }
    fn parent_mut(editor: &mut ObjectEditor) -> &mut FieldDraft<String> {
        let ObjectDetails::Source { parent:Some(parent),.. } = &mut draft_mut(editor).details else { panic!("expected Parent Part") }; parent
    }
    fn rotation(editor: &ObjectEditor) -> &FieldDraft<Option<String>> {
        let ObjectDetails::Source { rotation:Some(rotation),.. } = &draft(editor).details else { panic!("expected Rotation destination") }; rotation
    }
    fn rotation_mut(editor: &mut ObjectEditor) -> &mut FieldDraft<Option<String>> {
        let ObjectDetails::Source { rotation:Some(rotation),.. } = &mut draft_mut(editor).details else { panic!("expected Rotation destination") }; rotation
    }
    fn range_mut(editor: &mut ObjectEditor) -> &mut FieldDraft<[f64;3]> {
        let ObjectDetails::Parameter { range } = &mut draft_mut(editor).details else { panic!("expected parameter range") }; range
    }

    fn pairs(editor: &ObjectEditor) -> Value {
        let loaded = draft(editor);
        let mut out = json!({"name":{"value":loaded.name.value,"original":loaded.name.original},
            "id":{"value":loaded.id.value,"original":loaded.id.original}});
        match &loaded.details {
            ObjectDetails::Parameter { range } => out["range"] = json!({"value":range.value,"original":range.original}),
            ObjectDetails::Source { parent,rotation,.. } => {
                if let Some(parent) = parent { out["parent"] = json!({"value":parent.value,"original":parent.original}); }
                if let Some(rotation) = rotation { out["rotation"] = json!({"value":rotation.value,"original":rotation.original}); }
            }
        }
        out
    }

    fn text(harness: &mut Harness, editor: &mut ObjectEditor, controller: &mut EditorController, label: &str, value: &str) {
        harness.text(label,value,|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,controller); });
        });
    }

    fn drag_number(harness: &mut Harness, editor: &mut ObjectEditor, controller: &mut EditorController, label: &str, dx: f32) {
        let output = harness.frame(Vec::new(),|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,controller); });
        });
        let from = crate::editor_test_support::center(&output,Role::SpinButton,label);
        let press = |pos,pressed| egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE};
        for events in [vec![egui::Event::PointerMoved(from),press(from,true)],
            vec![egui::Event::PointerMoved(from+egui::vec2(dx/2.0,0.0))],vec![egui::Event::PointerMoved(from+egui::vec2(dx,0.0))],
            vec![press(from+egui::vec2(dx,0.0),false)],Vec::new()] {
            harness.frame(events,|ctx| {
                egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,controller); });
            });
        }
    }

    fn open(name: &str) -> (EditorController, ObjectEditor, Harness) {
        let dir = temp_dir(name);
        std::fs::write(dir.join("fixture.cmo3"), cmo3_bytes()).unwrap();
        let mut controller = EditorController::new();
        let opened = controller.core_request(&json!({"op": "open", "path": dir.join("fixture.cmo3").to_str().unwrap()}));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1200.0);
        (controller, ObjectEditor::new(), harness)
    }

    fn click(harness: &mut Harness, editor: &mut ObjectEditor, controller: &mut EditorController, label: &str) {
        harness.click(Role::Button, label, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, controller);
            });
        });
    }

    fn choose(harness: &mut Harness, editor: &mut ObjectEditor, controller: &mut EditorController, combo: &str, choice: &str) {
        use crate::editor_test_support::name_of;
        let output = harness.click(Role::ComboBox, combo, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, controller); });
        });
        let role = output.iter().find(|(_, node)| name_of(&output, node).as_deref() == Some(choice))
            .map(|(_, node)| node.role()).unwrap_or_else(|| panic!("missing choice {choice}"));
        harness.click(role, choice, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, controller); });
        });
    }

    fn open_rotation(name: &str, xml: &str) -> (EditorController, ObjectEditor, Harness, std::path::PathBuf) {
        let directory = temp_dir(name);
        let path = directory.join("fixture.cmo3");
        std::fs::write(&path, crate::editor_test_support::caff_with_xml(xml)).unwrap();
        let mut controller = EditorController::new();
        let opened = controller.core_request(&json!({"op": "open", "path": path}));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1200.0);
        (controller, ObjectEditor::new(), harness, path)
    }

    #[test]
    fn object_drafts_applying_one_named_field_keeps_the_other_input() {
        for first in ["Apply name", "Apply ID"] {
            let (mut controller, mut editor, mut harness, _) = open_rotation(
                &format!("objects-sibling-{first}"), &crate::editor_test_support::artpath_cmo3_xml());
            for (field, value) in [("Name","Unapplied mesh name"), ("ID","ArtMeshFace")] {
                harness.text(field,value,|ctx| {
                    egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
                });
            }
            let before = controller.snapshot().clone();
            assert!(editor.has_draft());
            click(&mut harness,&mut editor,&mut controller,first);
            assert!(editor.error.is_empty(),"{}",editor.error);
            assert_eq!(controller.snapshot()["revision"].as_u64().unwrap(),before["revision"].as_u64().unwrap()+1);
            assert_eq!(draft(&editor).name.value,"Unapplied mesh name","applying ID must preserve pending Name input");
            assert_eq!(draft(&editor).id.value,"ArtMeshFace","applying Name must preserve pending ID input");
            assert!(editor.has_draft(),"the other field has not been applied");
            let output = harness.frame(Vec::new(),|ctx| {
                egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
            });
            let sibling = if first == "Apply name" {"ID"} else {"Name"};
            let expected = if first == "Apply name" {"ArtMeshFace"} else {"Unapplied mesh name"};
            assert_eq!(crate::editor_test_support::find(&output,Role::TextInput,sibling).unwrap().value(),Some(expected));
        }
    }

    #[test]
    fn object_drafts_external_revision_keeps_actual_named_input_until_explicit_resolution() {
        let (mut controller, mut editor, mut harness, _) = open_rotation(
            "objects-external-draft",&crate::editor_test_support::artpath_cmo3_xml());
        harness.text("Name","Pending local name",|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
        });
        let loaded = draft(&editor).revision;
        let response = controller.core_request(&json!({"op":"live2d.set_id","guid":"mesh-guid","id":"RemoteID",
            "expectedRevision":controller.snapshot()["revision"]}));
        assert_eq!(response["succeeded"],true,"{response}");
        let before = controller.snapshot().clone();
        harness.frame(Vec::new(),|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
        });
        assert_eq!(draft(&editor).name.value,"Pending local name","an external revision must not erase unsent input");
        assert!(editor.has_draft());
        assert_eq!(draft(&editor).revision,loaded,"an external revision cannot silently authorize stale input");
        assert_eq!(controller.snapshot(),&before);
    }

    #[test]
    fn object_drafts_all_applicable_groups_preserve_siblings_and_originals_in_each_frame_and_reopen() {
        for parameter in [false,true] {
            let actions = if parameter {vec![("Apply name","name"),("Apply ID","id"),("Apply range","range")]} else {
                vec![("Apply name","name"),("Apply ID","id"),("Move","parent"),("Reparent to Rotation","rotation")]};
            for &(first,applied) in &actions {
                let xml = if parameter {
                    crate::editor_test_support::artpath_cmo3_xml().replace("<CParameterSource>",
                        "<CParameterSource><s xs.n='name'>Parameter</s>")
                } else {
                    include_str!("../tests/fixtures/rotation-reparent.xml").replace("<CArtMeshGuid xs.n='guid' uuid='mesh-guid'/>",
                        "<s xs.n='localName'>Mesh</s><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/>")
                };
                let (mut controller,mut editor,mut harness,path) = open_rotation(&format!("object-pairs-{parameter}-{first}"),&xml);
                if parameter { choose(&mut harness,&mut editor,&mut controller,"Object","Parameter [Parameter]"); }
                let before = controller.snapshot().clone();
                text(&mut harness,&mut editor,&mut controller,"Name","Changed display name");
                text(&mut harness,&mut editor,&mut controller,"ID",if parameter {"ParamChanged"} else {"ArtMeshChanged"});
                if parameter {
                    for (name,dx) in [("Minimum",-10.0),("Maximum",10.0),("Default",10.0)] {
                        drag_number(&mut harness,&mut editor,&mut controller,name,dx);
                    }
                    let range = range_mut(&mut editor);
                    assert!(range.value.iter().zip(range.original).all(|(value,original)|*value != original));
                } else {
                    choose(&mut harness,&mut editor,&mut controller,"Parent Part","Arm [Part]");
                    choose(&mut harness,&mut editor,&mut controller,"Rotation destination","NewChild [Rotation]");
                }
                let pending = pairs(&editor); let guid = draft(&editor).guid.clone();
                let start = controller.snapshot()["revision"].as_u64().unwrap(); let mut observed = false;
                assert_eq!(controller.snapshot(),&before);
                harness.click(Role::Button,first,|ctx| {
                    egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
                    if controller.snapshot()["revision"].as_u64() == Some(start+1) {
                        observed = true;
                        assert_eq!(draft(&editor).guid,guid);
                        assert_eq!(draft(&editor).revision,start+1);
                        let actual = pairs(&editor);
                        assert_eq!(actual[applied]["value"],actual[applied]["original"]);
                        for &(_,sibling) in &actions {
                            if sibling != applied { assert_eq!(actual[sibling],pending[sibling],"{first} loses {sibling}"); }
                        }
                    }
                });
                assert!(observed,"{first}: {}",editor.error);
                for &(remaining,_) in &actions {
                    if remaining != first { click(&mut harness,&mut editor,&mut controller,remaining); }
                    assert!(editor.error.is_empty(),"{remaining}: {}",editor.error);
                }
                assert!(!editor.has_draft());
                assert_eq!(controller.snapshot()["revision"].as_u64(),Some(start+actions.len() as u64));
                assert_eq!(controller.snapshot()["history"]["count"].as_u64().unwrap(),
                    before["history"]["count"].as_u64().unwrap()+actions.len() as u64);
                let expected = controller.snapshot()["model"].clone();
                for _ in &actions { assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"],true); }
                assert_eq!(controller.snapshot()["model"],before["model"]);
                for _ in &actions { assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"],true); }
                assert_eq!(controller.snapshot()["model"],expected);
                let saved = path.with_file_name("saved.cmo3");
                assert_eq!(controller.core_request(&json!({"op":"save","path":saved}))["succeeded"],true);
                let mut reopened = EditorController::new();
                assert_eq!(reopened.core_request(&json!({"op":"open","path":saved}))["succeeded"],true);
                assert_eq!(reopened.snapshot()["model"],expected);
                let old = soul2d_core::caff::CaffDocument::parse(&std::fs::read(path).unwrap()).unwrap();
                let new = soul2d_core::caff::CaffDocument::parse(&std::fs::read(saved).unwrap()).unwrap();
                for entry in old.entries().iter().filter(|entry|entry.alias != "main_xml") {
                    assert_eq!(new.entry_data(&entry.name),old.entry_data(&entry.name));
                }
            }
        }
    }

    #[test]
    fn object_drafts_structural_actions_keep_the_original_owner_and_pending_metadata() {
        let dir = temp_dir("objects-structural-pending"); let path = dir.join("fixture.cmo3");
        let keyed = soul2d_core::caff::CaffDocument::parse(&keyed_cmo3_bytes()).unwrap();
        let xml = String::from_utf8(keyed.main_xml()).unwrap().replace("<CPartId xs.n='id' idstr='root'/>",
            "<CPartId xs.n='id' idstr='root'/><carray_list xs.n='_childGuids' count='1'><CArtMeshGuid uuid='mesh-guid'/></carray_list>");
        std::fs::write(&path,crate::editor_test_support::caff_with_xml(&xml)).unwrap();
        let mut controller = EditorController::new();
        assert_eq!(controller.core_request(&json!({"op":"open","path":path}))["succeeded"],true);
        let mut editor = ObjectEditor::new(); let mut harness = Harness::new(); harness.screen = egui::vec2(900.0,1600.0);
        text(&mut harness,&mut editor,&mut controller,"Name","Retained source input");
        text(&mut harness,&mut editor,&mut controller,"ID","RetainedID");
        let before = pairs(&editor); let guid = draft(&editor).guid.clone();
        for (action,key) in [("Duplicate",0.0),("Add key",0.25),("Delete key",0.25)] {
            editor.key = key;
            let revision = controller.snapshot()["revision"].as_u64().unwrap();
            click(&mut harness,&mut editor,&mut controller,action);
            assert!(editor.error.is_empty(),"{action}: {}",editor.error);
            assert_eq!(draft(&editor).guid,guid);
            assert_eq!(pairs(&editor),before);
            assert_eq!(draft(&editor).revision,revision+1);
        }
        let before = controller.snapshot().clone();
        click(&mut harness,&mut editor,&mut controller,"Delete");
        assert_eq!(controller.snapshot(),&before,"Delete cannot erase pending metadata");
        click(&mut harness,&mut editor,&mut controller,"Reload and discard all object edits");
        assert_eq!(controller.snapshot(),&before); assert!(!editor.has_draft());
        let sources = before["model"]["sources"].as_array().unwrap();
        assert!(sources.iter().any(|source|source["guid"] != guid && source["id"] == "mesh_copy"));
        assert!(sources.iter().all(|source|source["name"] != "Retained source input" && source["id"] != "RetainedID"));
    }

    #[test]
    fn object_drafts_refusal_and_no_op_keep_input_originals_and_the_redo_branch() {
        let xml = crate::editor_test_support::artpath_cmo3_xml().replace("<s xs.n='localName'>Mesh</s>","");
        let (mut controller,mut editor,mut harness,path) = open_rotation("objects-refusal-noop",&xml);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(controller.core_request(&json!({"op":"live2d.set_id","guid":"mesh-guid","id":"StoredID",
            "expectedRevision":controller.snapshot()["revision"]}))["succeeded"],true);
        assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"],true);
        text(&mut harness,&mut editor,&mut controller,"Name","Pending name");
        let pending = pairs(&editor); let before = controller.snapshot().clone();
        click(&mut harness,&mut editor,&mut controller,"Apply name");
        assert!(!editor.error.is_empty()); assert_eq!(pairs(&editor),pending);
        assert_eq!(controller.snapshot(),&before);
        let error = editor.error.clone();
        let output = harness.frame(Vec::new(),|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
        });
        assert!(crate::editor_test_support::texts(&output).contains(&error));
        click(&mut harness,&mut editor,&mut controller,"Apply ID");
        assert!(editor.error.is_empty()); assert_eq!(pairs(&editor),pending);
        assert_eq!(controller.snapshot(),&before,"no-op must not clear unrelated input or Redo");
        assert_eq!(std::fs::read(path).unwrap(),bytes);
        assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"],true);
        let before = controller.snapshot().clone();
        click(&mut harness,&mut editor,&mut controller,"Apply name");
        assert_eq!(pairs(&editor),pending); assert_eq!(controller.snapshot(),&before);
    }

    #[test]
    fn object_drafts_missing_and_empty_targets_keep_visible_input_until_explicit_discard() {
        for empty in [false,true] {
            let (mut controller,mut editor,mut harness,_) = open_rotation(&format!("objects-missing-{empty}"),
                &crate::editor_test_support::artpath_cmo3_xml());
            text(&mut harness,&mut editor,&mut controller,"Name","Pending absent object");
            let pending = pairs(&editor); let revision = draft(&editor).revision;
            let request = if empty {json!({"op":"new"})} else {json!({"op":"live2d.delete","guid":"mesh-guid",
                "expectedRevision":controller.snapshot()["revision"]})};
            let response = controller.core_request(&request); assert_eq!(response["succeeded"],true,"{response}");
            let before = controller.snapshot().clone();
            let output = harness.frame(Vec::new(),|ctx| {
                egui::CentralPanel::default().show(ctx,|ui| { editor.show(ui,&mut controller); });
            });
            assert_eq!(pairs(&editor),pending); assert_eq!(draft(&editor).guid,"mesh-guid");
            assert_eq!(draft(&editor).revision,revision); assert!(editor.has_draft());
            assert_eq!(crate::editor_test_support::find(&output,Role::TextInput,"Name").unwrap().value(),Some("Pending absent object"));
            assert!(crate::editor_test_support::find(&output,Role::Button,"Duplicate").unwrap().is_disabled());
            assert!(crate::editor_test_support::find(&output,Role::ComboBox,"Object").unwrap().is_disabled());
            click(&mut harness,&mut editor,&mut controller,"Reload and discard all object edits");
            assert!(!editor.has_draft()); assert_eq!(controller.snapshot(),&before);
            if empty {assert!(editor.loaded.is_none());} else {assert_ne!(draft(&editor).guid,"mesh-guid");}
        }
    }

    #[test]
    fn object_drafts_nonfinite_range_refuses_the_whole_group_before_json() {
        let (mut controller,mut editor,mut harness) = open("objects-nonfinite-range");
        choose(&mut harness,&mut editor,&mut controller,"Object","ParamTest [Parameter]");
        range_mut(&mut editor).value = [f64::NAN,2.5,0.5];
        let before = controller.snapshot().clone();
        assert!(!editor.dispatch(ObjectAction::ApplyRange,&mut controller));
        assert!(editor.error.contains("finite")); assert_eq!(controller.snapshot(),&before);
        assert!(range_mut(&mut editor).value[0].is_nan());
        assert_eq!(range_mut(&mut editor).value[1..],[2.5,0.5]); assert!(editor.has_draft());
    }

    #[test]
    fn rotation_destination_controls_preserve_the_model_except_target_and_positions_with_one_undo() {
        let (mut controller, mut editor, mut harness, _path) = open_rotation("object-rotation-success", include_str!("../tests/fixtures/rotation-reparent.xml"));
        let before = controller.snapshot().clone();
        choose(&mut harness, &mut editor, &mut controller, "Object", "Old [Rotation]");
        choose(&mut harness, &mut editor, &mut controller, "Object", "mesh [ArtMesh]");
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        assert_eq!(controller.snapshot(), &before, "choosing a destination is only a draft");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        let changed = controller.snapshot().clone();
        let mut expected = before["model"].clone();
        let mesh = expected["sources"].as_array_mut().unwrap().iter_mut().find(|s| s["guid"] == "mesh-guid").unwrap();
        mesh["targetDeformerGuid"] = json!("new-child-guid");
        let positions = changed["model"]["sources"].as_array().unwrap().iter().find(|s| s["guid"] == "mesh-guid")
            .unwrap()["forms"][0]["data"]["positions"].clone();
        for (actual, expected) in positions.as_array().unwrap().iter().zip([-45.25, 37.25, -47.25, 39.25, -49.25, 41.25]) {
            assert!((actual.as_f64().unwrap() - expected).abs() < 1e-12, "positions={positions}, target={}, selected={}, error={}",
                changed["model"]["sources"][0]["targetDeformerGuid"], draft(&editor).guid, editor.error);
        }
        mesh["forms"][0]["data"]["positions"] = positions;
        assert_eq!(changed["model"], expected, "{}", editor.error);
        assert_eq!(changed["revision"].as_u64().unwrap(), before["revision"].as_u64().unwrap() + 1);
        assert_eq!(changed["history"]["count"].as_u64().unwrap(), before["history"]["count"].as_u64().unwrap() + 1);
        assert_eq!(controller.core_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], before["model"]);
        assert_eq!(controller.core_request(&json!({"op": "redo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], changed["model"]);
        let no_op = controller.snapshot().clone();
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &no_op);
        assert_eq!(editor.status, "Rotation target unchanged");
    }

    #[test]
    fn rotation_refusal_keeps_the_redo_branch_and_current_target_remains_a_no_op() {
        let xml = include_str!("../tests/fixtures/rotation-reparent.xml").replace(
            "<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>",
            "<CDeformerGuid xs.n='targetDeformerGuid' xs.id='target-slot' xs.ref='old-guid-def'/>");
        let (mut controller, mut editor, mut harness, path) = open_rotation("object-rotation-refusal", &xml);
        let bytes = std::fs::read(&path).unwrap();
        let edited = controller.core_request(&json!({"op": "live2d.form.set_vertex", "sourceGuid": "mesh-guid",
            "formGuid": "form-guid", "coordinateType": "DeformerLocal", "vertexIndex": 0,
            "position": [7.0, 8.0], "expectedRevision": controller.snapshot()["revision"]}));
        assert_eq!(edited["succeeded"], true, "{edited}");
        let changed = controller.snapshot()["model"].clone();
        assert_eq!(controller.core_request(&json!({"op": "undo"}))["succeeded"], true);
        let before = controller.snapshot().clone();
        assert_eq!(before["canRedo"], true);
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before);
        assert!(!editor.error.is_empty());
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
        });
        assert!(crate::editor_test_support::texts(&output).iter().any(|text| text == &editor.error));
        assert!(editor.status.is_empty());
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "Old [Rotation]");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before);
        assert!(editor.error.is_empty());
        assert_eq!(editor.status, "Rotation target unchanged");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(controller.core_request(&json!({"op": "redo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], changed);
    }

    #[test]
    fn rotation_destinations_with_matching_labels_show_their_distinct_guids() {
        let xml = include_str!("../tests/fixtures/rotation-reparent.xml").replace("idstr='NewChild'", "idstr='Old'");
        let (mut controller, mut editor, mut harness, _path) = open_rotation("object-rotation-duplicate-labels", &xml);
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "Old [Rotation] (new-child-guid)");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert!(editor.error.is_empty(), "{}", editor.error);
        assert_eq!(controller.snapshot()["model"]["sources"][0]["targetDeformerGuid"], "new-child-guid");
    }

    #[test]
    fn rotation_draft_is_retained_on_external_revision_until_explicit_reload_and_reset() {
        let (mut controller, mut editor, mut harness, _path) = open_rotation("object-rotation-reload", include_str!("../tests/fixtures/rotation-reparent.xml"));
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        let renamed = controller.core_request(&json!({"op": "live2d.set_id", "guid": "mesh-guid", "id": "renamed",
            "expectedRevision": controller.snapshot()["revision"]}));
        assert_eq!(renamed["succeeded"], true, "{renamed}");
        let before = controller.snapshot().clone();
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before, "stale pending destination must not apply");
        assert_eq!(rotation(&editor).value,Some("new-child-guid".into()));
        assert!(editor.has_draft());
        click(&mut harness,&mut editor,&mut controller,"Reload and discard all object edits");
        assert!(!editor.has_draft());
        choose(&mut harness, &mut editor, &mut controller, "Object", "NewChild [Rotation]");
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
        });
        assert!(!has(&output, Role::ComboBox, "Rotation destination"));
        assert!(!has(&output, Role::Button, "Reparent to Rotation"));
        choose(&mut harness, &mut editor, &mut controller, "Object", "renamed [ArtMesh]");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before, "object reload uses its actual target");
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        let (other_controller, _, _, _) = open_rotation("object-rotation-reset", include_str!("../tests/fixtures/rotation-reparent.xml"));
        controller = other_controller;
        editor.reset();
        let before = controller.snapshot().clone();
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before, "replacement/reset clears the previous destination");
    }

    #[test]
    fn rotation_empty_choices_are_disabled_and_parameters_have_no_rotation_action() {
        let (mut controller, mut editor, mut harness) = open("object-rotation-empty");
        let before = controller.snapshot().clone();
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
        });
        assert!(crate::editor_test_support::find(&output, Role::Button, "Reparent to Rotation").unwrap().is_disabled());
        assert!(crate::editor_test_support::texts(&output).iter().any(|text| text == "No Rotation destinations are available"));
        assert_all_named(&output, "rotation controls");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before);
        choose(&mut harness, &mut editor, &mut controller, "Object", "ParamTest [Parameter]");
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
        });
        assert!(!has(&output, Role::Button, "Reparent to Rotation"));
        assert!(has(&output, Role::Button, "Apply range"));
    }

    #[test]
    fn rotation_without_a_rotation_target_requires_an_explicit_choice_and_shows_the_backend_refusal() {
        let xml = include_str!("../tests/fixtures/rotation-reparent.xml").replace(
            "<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>", "<null xs.n='targetDeformerGuid'/>");
        let (mut controller, mut editor, mut harness, _path) = open_rotation("object-rotation-no-target", &xml);
        let before = controller.snapshot().clone();
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
        });
        let text = crate::editor_test_support::texts(&output);
        assert!(text.iter().any(|text| text == "Current target: None"), "{text:?}");
        assert!(text.iter().any(|text| text == "Choose a Rotation"));
        assert!(crate::editor_test_support::find(&output, Role::Button, "Reparent to Rotation").unwrap().is_disabled());
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before);
        assert!(!editor.error.is_empty());
    }

    #[test]
    fn rotation_current_non_rotation_and_unlisted_targets_are_distinct_from_the_destination_draft() {
        for (name, warp, shown) in [("object-rotation-warp-target", true, "Old [Warp]"),
            ("object-rotation-root-target", false, "Unlisted target: 71fae776-e218-4aee-873e-78e8ac0cb48a")]
        {
            let mut xml = include_str!("../tests/fixtures/rotation-reparent.xml").to_owned();
            if warp {
                let start = xml.find("<CRotationDeformerSource xs.id='old'>").unwrap();
                let end = start + xml[start..].find("</CRotationDeformerSource>").unwrap() + "</CRotationDeformerSource>".len();
                xml.replace_range(start..end, "<CWarpDeformerSource xs.id='old'><CWarpGuid xs.n='guid' uuid='old-guid'/>\
                    <CWarpId xs.n='id' idstr='Old'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/>\
                    <i xs.n='col'>1</i><i xs.n='row'>1</i><carray_list xs.n='keyforms' count='1'><CWarpDeformerForm>\
                    <CFormGuid xs.n='guid' uuid='old-form'/><CWarpDeformerSource xs.n='_source' xs.ref='old'/>\
                    <CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>\
                    <float-array xs.n='positions' count='8'>0 0 1 0 0 1 1 1</float-array>\
                    </CWarpDeformerForm></carray_list></CWarpDeformerSource>");
                xml = xml.replace("<CRotationDeformerSource xs.ref='old'/>", "<CWarpDeformerSource xs.ref='old'/>");
            } else {
                xml = xml.replace("<CDeformerGuid xs.n='targetDeformerGuid' xs.ref='old-guid-def'/>",
                    "<CDeformerGuid xs.n='targetDeformerGuid' uuid='71fae776-e218-4aee-873e-78e8ac0cb48a'/>");
            }
            let (mut controller, mut editor, mut harness, _path) = open_rotation(name, &xml);
            let before = controller.snapshot().clone();
            let output = harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
            });
            let texts = crate::editor_test_support::texts(&output);
            assert!(texts.iter().any(|text| text == &format!("Current target: {shown}")), "{texts:?}");
            let target = if warp { "old-guid" } else { "71fae776-e218-4aee-873e-78e8ac0cb48a" };
            assert!(texts.iter().any(|text| text == &format!("Unavailable: {target}")));
            assert!(!editor.has_draft(),"a stored unavailable destination is not a user edit");
            assert!(crate::editor_test_support::find(&output, Role::Button, "Reparent to Rotation").unwrap().is_disabled());
            choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
            let output = harness.frame(Vec::new(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| { editor.show(ui, &mut controller); });
            });
            assert!(crate::editor_test_support::texts(&output).iter().any(|text| text == &format!("Current target: {shown}")));
            click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
            assert_eq!(controller.snapshot(), &before);
            assert!(!editor.error.is_empty());
        }
    }

    #[test]
    fn a_key_is_added_and_deleted_on_the_chosen_axis() {
        let dir = temp_dir("object-keys");
        std::fs::write(dir.join("fixture.cmo3"), keyed_cmo3_bytes()).unwrap();
        let mut controller = EditorController::new();
        let opened = controller.core_request(&json!({"op": "open", "path": dir.join("fixture.cmo3").to_str().unwrap()}));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let mut editor = ObjectEditor::new();
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1200.0);
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut controller);
            });
        });
        let keys = |controller: &EditorController| controller.snapshot()["model"]["sources"][0]["grid"]["axes"][0]["keys"].clone();
        assert!(has(&output, Role::ComboBox, "Key axis"));
        assert!(has(&output, Role::Button, "Add key") && has(&output, Role::Button, "Delete key"));
        editor.key = 0.25;
        click(&mut harness, &mut editor, &mut controller, "Add key");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(keys(&controller), json!([-1.0, 0.25, 1.0]));
        editor.key = -1.0;
        click(&mut harness, &mut editor, &mut controller, "Delete key");
        assert_eq!(keys(&controller), json!([0.25, 1.0]), "{}", editor.error);
    }

    #[test]
    fn the_buttons_change_the_id_refuse_with_the_backend_message_and_copy_and_delete_a_parameter() {
        let (mut controller, mut editor, mut harness) = open("objects");
        let output = harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut controller);
            });
        });
        assert!(has(&output, Role::ComboBox, "Object"));
        for button in ["Apply name", "Apply ID", "Move", "Duplicate", "Delete"] {
            assert!(has(&output, Role::Button, button), "{button}");
        }
        assert!(assert_all_named(&output, "object editor") >= 6);
        assert_eq!((draft(&editor).guid.as_str(), draft(&editor).id.value.as_str()), ("mesh-guid", "mesh"));

        let start = controller.snapshot()["revision"].as_u64().unwrap();
        draft_mut(&mut editor).id.value = "ArtMeshFace".into();
        click(&mut harness, &mut editor, &mut controller, "Apply ID");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(controller.snapshot()["model"]["sources"][0]["id"], "ArtMeshFace");
        assert_eq!(controller.snapshot()["revision"], start + 1);

        draft_mut(&mut editor).name.value = "Face".into();
        click(&mut harness, &mut editor, &mut controller, "Apply name");
        assert!(editor.error.contains("localName"), "{}", editor.error);
        assert_eq!(controller.snapshot()["revision"], start + 1);

        click(&mut harness,&mut editor,&mut controller,"Reload and discard all object edits");
        choose(&mut harness,&mut editor,&mut controller,"Object","ParamTest [Parameter]");
        click(&mut harness, &mut editor, &mut controller, "Duplicate");
        assert_eq!(controller.snapshot()["model"]["parameters"][1]["id"], "ParamTest_copy", "{}", editor.error);
        controller.core_request(&json!({"op": "undo"}));
        harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut controller);
            });
        });
        range_mut(&mut editor).value = [-1.0, 2.0, 0.5];
        click(&mut harness, &mut editor, &mut controller, "Apply range");
        let edited = &controller.snapshot()["model"]["parameters"][0];
        assert_eq!((edited["maximum"].as_f64(), edited["default"].as_f64()), (Some(2.0), Some(0.5)), "{}", editor.error);
        controller.core_request(&json!({"op": "undo"}));
        click(&mut harness,&mut editor,&mut controller,"Reload and discard all object edits");
        click(&mut harness, &mut editor, &mut controller, "Delete");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(controller.snapshot()["model"]["parameters"], json!([]));
        controller.core_request(&json!({"op": "undo"}));
        assert_eq!(controller.snapshot()["model"]["parameters"][0]["id"], "ParamTest", "one undo restores it");
    }

    #[test]
    fn move_sends_the_chosen_part_with_the_loaded_revision_and_one_undo_puts_the_source_back() {
        let dir = temp_dir("object-move");
        std::fs::write(dir.join("fixture.cmo3"), two_part_cmo3_bytes()).unwrap();
        let mut controller = EditorController::new();
        let opened = controller.core_request(&json!({"op": "open", "path": dir.join("fixture.cmo3").to_str().unwrap()}));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let mut editor = ObjectEditor::new();
        let mut harness = Harness::new();
        harness.screen = egui::vec2(900.0, 1200.0);
        harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut controller);
            });
        });
        assert_eq!((draft(&editor).guid.as_str(), parent(&editor).value.as_str()), ("mesh-guid", "part-guid"));
        let start = controller.snapshot()["revision"].as_u64().unwrap();
        let source = |controller: &EditorController, guid: &str| {
            let sources = controller.snapshot()["model"]["sources"].clone();
            sources.as_array().unwrap().iter().find(|source| source["guid"] == guid).unwrap().clone()
        };
        parent_mut(&mut editor).value = "arm-guid".into();
        click(&mut harness, &mut editor, &mut controller, "Move");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(controller.snapshot()["revision"], start + 1);
        assert_eq!(source(&controller, "mesh-guid")["parentPartGuid"], "arm-guid");
        assert_eq!(source(&controller, "arm-guid")["data"]["childGuids"], json!(["mesh-guid"]));
        assert_eq!(source(&controller, "part-guid")["data"]["childGuids"], json!(["arm-guid"]));
        assert_eq!((draft(&editor).guid.as_str(), parent(&editor).value.as_str()), ("mesh-guid", "arm-guid"));

        controller.core_request(&json!({"op": "undo"}));
        assert_eq!(source(&controller, "mesh-guid")["parentPartGuid"], "part-guid");
        assert_eq!(source(&controller, "part-guid")["data"]["childGuids"], json!(["mesh-guid", "arm-guid"]));
    }
}
