//! The object panel of a CMO3 document: pick one source (ArtMesh, Part, deformer, Glue, ArtPath) or parameter,
//! then rename it, change its ID, move it to another Part, retarget a static ArtMesh to a Rotation,
//! change a parameter's range, add or delete a key of a
//! keyform grid, copy it or delete it. Each button is one request guarded by the
//! revision the fields were loaded at, so a document that changed since is refused instead of overwritten.

use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;

/// A source or parameter of the model, with the list it came from.
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

#[derive(Default)]
pub struct ObjectEditor {
    guid: String,
    /// The revision the fields were loaded at; every request sends it as `expectedRevision`.
    revision: Option<u64>,
    name: String,
    id: String,
    parent: String,
    rotation_destination: Option<String>,
    /// Minimum, maximum and default of a parameter.
    range: [f64; 3],
    original: Option<(String, String, String, Option<String>, [f64; 3])>,
    /// The parameter GUID of the chosen keyform axis, and the key to add or delete on it.
    axis: String,
    key: f64,
    status: String,
    error: String,
}

impl ObjectEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A different document: forget the selection.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn has_draft(&self) -> bool {
        self.original.as_ref().is_some_and(|(name, id, parent, destination, range)|
            &self.name != name || &self.id != id || &self.parent != parent
                || &self.rotation_destination != destination || &self.range != range)
    }

    fn load(&mut self, entry: &Value, revision: u64) {
        self.guid = entry["guid"].as_str().unwrap_or_default().to_owned();
        self.name = entry["name"].as_str().unwrap_or_default().to_owned();
        self.id = entry["id"].as_str().unwrap_or_default().to_owned();
        self.parent = entry["parentPartGuid"].as_str().unwrap_or_default().to_owned();
        self.rotation_destination = entry["targetDeformerGuid"].as_str().filter(|guid| !guid.is_empty()).map(str::to_owned);
        self.range = ["minimum", "maximum", "default"].map(|key| entry[key].as_f64().unwrap_or_default());
        self.original = Some((self.name.clone(), self.id.clone(), self.parent.clone(), self.rotation_destination.clone(), self.range));
        let axes = entry["grid"]["axes"].as_array().cloned().unwrap_or_default();
        if !axes.iter().any(|axis| axis["parameterGuid"] == self.axis.as_str()) {
            self.axis = axes.first().and_then(|axis| axis["parameterGuid"].as_str()).unwrap_or_default().to_owned();
        }
        self.revision = Some(revision);
    }

    /// Returns true when an accepted request requires the panel and preview to refresh, including no-ops.
    pub fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController) -> bool {
        let snapshot = controller.snapshot().clone();
        let model = &snapshot["model"];
        let revision = snapshot["revision"].as_u64().unwrap_or(0);
        let entries = objects(model);
        let Some(&(first, first_parameter)) = entries.first() else {
            ui.label("No sources or parameters are available");
            return false;
        };
        let (mut selected, mut parameter) = entries.iter().copied().find(|(entry, _)| entry["guid"] == self.guid.as_str())
            .unwrap_or((first, first_parameter));
        if self.revision != Some(revision) || selected["guid"] != self.guid.as_str() {
            self.load(selected, revision);
        }
        egui::ComboBox::from_label("Object").selected_text(label(selected, parameter)).show_ui(ui, |ui| {
            for &(entry, is_parameter) in &entries {
                if ui.selectable_label(entry["guid"] == self.guid.as_str(), label(entry, is_parameter)).clicked() {
                    (selected, parameter) = (entry, is_parameter);
                }
            }
        });
        if selected["guid"] != self.guid.as_str() {
            self.load(selected, revision);
            self.error.clear();
        }
        let guid = self.guid.clone();
        let mut request = None;
        for (caption, field, button, op, key) in [
            ("Name", &mut self.name, "Apply name", "live2d.rename", "name"),
            ("ID", &mut self.id, "Apply ID", "live2d.set_id", "id"),
        ] {
            ui.horizontal(|ui| {
                let text = ui.label(caption);
                ui.text_edit_singleline(field).labelled_by(text.id);
                if ui.button(button).clicked() {
                    request = Some(json!({"op": op, "guid": guid, key: field.as_str()}));
                }
            });
        }
        if parameter {
            for (caption, value) in ["Minimum", "Maximum", "Default"].into_iter().zip(self.range.iter_mut()) {
                ui.horizontal(|ui| {
                    let text = ui.label(caption);
                    ui.add(egui::DragValue::new(value).speed(0.1).max_decimals(4)).labelled_by(text.id);
                });
            }
            if ui.button("Apply range").clicked() {
                let [minimum, maximum, default] = self.range;
                request = Some(json!({"op": "live2d.parameter.edit", "guid": guid,
                    "minimum": minimum, "maximum": maximum, "default": default}));
            }
        }
        if !parameter && selected["parentPartGuid"].is_string() {
            let parts: Vec<&Value> = model["sources"].as_array().into_iter().flatten()
                .filter(|source| source["data"]["type"] == "Part" && source["guid"] != guid.as_str()).collect();
            let shown = parts.iter().find(|part| part["guid"] == self.parent.as_str()).map_or("?".into(), |part| label(part, false));
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("Parent Part").selected_text(shown).show_ui(ui, |ui| {
                    for part in &parts {
                        let part_guid = part["guid"].as_str().unwrap_or_default();
                        ui.selectable_value(&mut self.parent, part_guid.to_owned(), label(part, false));
                    }
                });
                if ui.button("Move").clicked() {
                    request = Some(json!({"op": "live2d.reparent", "guid": guid, "parent": self.parent}));
                }
            });
        }
        if !parameter && selected["data"]["type"] == "ArtMesh" {
            let sources = model["sources"].as_array().map(Vec::as_slice).unwrap_or_default();
            let current = selected["targetDeformerGuid"].as_str().filter(|guid| !guid.is_empty());
            let shown = current.map(|guid| sources.iter().find(|source| source["guid"] == guid)
                .map_or_else(|| format!("Unlisted target: {guid}"), |source| label(source, false)))
                .unwrap_or_else(|| "None".into());
            ui.label(format!("Current target: {shown}"));
            let rotations: Vec<&Value> = sources.iter().filter(|source| source["data"]["type"] == "Rotation"
                && source["guid"].as_str().is_some_and(|guid| !guid.is_empty())).collect();
            let rotation_label = |source: &Value| {
                let text = label(source, false);
                if rotations.iter().any(|other| other["guid"] != source["guid"] && label(other, false) == text) {
                    format!("{text} ({})", source["guid"].as_str().unwrap())
                } else {
                    text
                }
            };
            let destination = rotations.iter().find(|source| source["guid"].as_str() == self.rotation_destination.as_deref());
            let shown = destination.map_or_else(|| "Choose a Rotation".into(), |source| rotation_label(source));
            egui::ComboBox::from_label("Rotation destination").selected_text(shown).show_ui(ui, |ui| {
                for source in &rotations {
                    let guid = source["guid"].as_str().unwrap();
                    ui.selectable_value(&mut self.rotation_destination, Some(guid.to_owned()), rotation_label(source))
                        .on_hover_text(guid);
                }
            });
            let chosen = rotations.iter().any(|source| source["guid"].as_str() == self.rotation_destination.as_deref());
            if ui.add_enabled(chosen, egui::Button::new("Reparent to Rotation")).clicked() {
                request = Some(json!({"op": "live2d.reparent_deformer", "sourceGuid": guid,
                    "targetDeformerGuid": self.rotation_destination}));
            }
            if rotations.is_empty() {
                ui.label("No Rotation destinations are available");
            }
            ui.label("Static edits only. Eligibility is checked when applied; unsupported chains are refused without changes.");
        }
        let axes = selected["grid"]["axes"].as_array().cloned().unwrap_or_default();
        if !parameter && !axes.is_empty() {
            let name = |guid: &str| model["parameters"].as_array().into_iter().flatten()
                .find(|p| p["guid"] == guid).and_then(|p| p["id"].as_str()).unwrap_or(guid).to_owned();
            let keys = axes.iter().find(|axis| axis["parameterGuid"] == self.axis.as_str())
                .and_then(|axis| axis["keys"].as_array()).map(|keys| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            egui::ComboBox::from_label("Key axis").selected_text(format!("{} [{keys}]", name(&self.axis))).show_ui(ui, |ui| {
                for axis in &axes {
                    let guid = axis["parameterGuid"].as_str().unwrap_or_default();
                    ui.selectable_value(&mut self.axis, guid.to_owned(), name(guid));
                }
            });
            ui.horizontal(|ui| {
                let text = ui.label("Key");
                ui.add(egui::DragValue::new(&mut self.key).speed(0.1).max_decimals(4)).labelled_by(text.id);
                for (button, op) in [("Add key", "live2d.keyform.add"), ("Delete key", "live2d.keyform.delete")] {
                    if ui.button(button).clicked() {
                        request = Some(json!({"op": op, "sourceGuid": guid, "parameterGuid": self.axis, "key": self.key}));
                    }
                }
            });
        }
        if ui.button("Duplicate").clicked() {
            request = Some(json!({"op": "live2d.duplicate", "guid": guid}));
        }
        if ui.button("Delete").on_hover_text("Delete an object nothing else uses; Undo restores it").clicked() {
            let op = if parameter { "live2d.parameter.delete" } else { "live2d.delete" };
            request = Some(json!({"op": op, "guid": guid}));
        }
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, self.error.as_str());
        }
        if !self.status.is_empty() {
            ui.label(self.status.as_str());
        }
        let Some(mut request) = request else { return false };
        request["expectedRevision"] = json!(self.revision.unwrap_or(revision));
        let response = controller.core_request(&request);
        if response["succeeded"] == true {
            self.error.clear();
            self.status = if request["op"] == "live2d.reparent_deformer" && controller.snapshot()["revision"] == revision {
                "Rotation target unchanged"
            } else {
                "Live2D object updated"
            }.into();
            self.revision = None;
            true
        } else {
            self.status.clear();
            self.error = response["message"].as_str().unwrap_or("Live2D object edit failed").to_owned();
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_quit_object_drafts_compare_loaded_authored_values_and_reverts() {
        let mut editor = super::ObjectEditor::new();
        editor.load(&serde_json::json!({"guid":"test","name":"Name","id":"ID","parentPartGuid":"parent",
            "targetDeformerGuid":"rotation","minimum":-1,"maximum":1,"default":0}), 1);
        assert!(!editor.has_draft());
        let original = (editor.name.clone(), editor.id.clone(), editor.parent.clone(), editor.rotation_destination.clone(), editor.range);
        editor.name.push(' '); assert!(editor.has_draft()); editor.name = original.0;
        editor.id.push('x'); assert!(editor.has_draft()); editor.id = original.1;
        editor.parent.push('x'); assert!(editor.has_draft()); editor.parent = original.2;
        editor.rotation_destination = Some("other-guid".into()); assert!(editor.has_draft()); editor.rotation_destination = original.3;
        for index in 0..3 { editor.range[index] += 0.5; assert!(editor.has_draft()); editor.range = original.4; }
        assert!(!editor.has_draft());
        editor.name.push('x'); editor.revision = None;
        assert!(editor.has_draft(), "invalidating a read is not applying another field's draft");
        editor.reset(); assert!(!editor.has_draft());
    }

    use super::*;
    use crate::editor_test_support::{assert_all_named, cmo3_bytes, has, keyed_cmo3_bytes, temp_dir, two_part_cmo3_bytes, Harness};
    use egui::accesskit::Role;

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
                changed["model"]["sources"][0]["targetDeformerGuid"], editor.guid, editor.error);
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
    fn rotation_draft_reloads_on_revision_selection_and_document_reset() {
        let (mut controller, mut editor, mut harness, _path) = open_rotation("object-rotation-reload", include_str!("../tests/fixtures/rotation-reparent.xml"));
        choose(&mut harness, &mut editor, &mut controller, "Rotation destination", "NewChild [Rotation]");
        let renamed = controller.core_request(&json!({"op": "live2d.set_id", "guid": "mesh-guid", "id": "renamed",
            "expectedRevision": controller.snapshot()["revision"]}));
        assert_eq!(renamed["succeeded"], true, "{renamed}");
        let before = controller.snapshot().clone();
        click(&mut harness, &mut editor, &mut controller, "Reparent to Rotation");
        assert_eq!(controller.snapshot(), &before, "revision reload discards the pending destination");
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
            assert!(texts.iter().any(|text| text == "Choose a Rotation"));
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
        assert_eq!((editor.guid.as_str(), editor.id.as_str()), ("mesh-guid", "mesh"));

        let start = controller.snapshot()["revision"].as_u64().unwrap();
        editor.id = "ArtMeshFace".into();
        click(&mut harness, &mut editor, &mut controller, "Apply ID");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(controller.snapshot()["model"]["sources"][0]["id"], "ArtMeshFace");
        assert_eq!(controller.snapshot()["revision"], start + 1);

        // The fixture stores no name for the ArtMesh, so the backend refuses and the panel shows why.
        editor.name = "Face".into();
        click(&mut harness, &mut editor, &mut controller, "Apply name");
        assert!(editor.error.contains("localName"), "{}", editor.error);
        assert_eq!(controller.snapshot()["revision"], start + 1);

        editor.guid = "parameter-guid".into();
        editor.revision = None;
        click(&mut harness, &mut editor, &mut controller, "Duplicate");
        assert_eq!(controller.snapshot()["model"]["parameters"][1]["id"], "ParamTest_copy", "{}", editor.error);
        controller.core_request(&json!({"op": "undo"}));
        harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                editor.show(ui, &mut controller);
            });
        });
        editor.range = [-1.0, 2.0, 0.5];
        click(&mut harness, &mut editor, &mut controller, "Apply range");
        let edited = &controller.snapshot()["model"]["parameters"][0];
        assert_eq!((edited["maximum"].as_f64(), edited["default"].as_f64()), (Some(2.0), Some(0.5)), "{}", editor.error);
        controller.core_request(&json!({"op": "undo"}));
        editor.revision = None;
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
        assert_eq!((editor.guid.as_str(), editor.parent.as_str()), ("mesh-guid", "part-guid"));
        let start = controller.snapshot()["revision"].as_u64().unwrap();
        let source = |controller: &EditorController, guid: &str| {
            let sources = controller.snapshot()["model"]["sources"].clone();
            sources.as_array().unwrap().iter().find(|source| source["guid"] == guid).unwrap().clone()
        };
        editor.parent = "arm-guid".into();
        click(&mut harness, &mut editor, &mut controller, "Move");
        assert_eq!(editor.status, "Live2D object updated", "{}", editor.error);
        assert_eq!(controller.snapshot()["revision"], start + 1);
        assert_eq!(source(&controller, "mesh-guid")["parentPartGuid"], "arm-guid");
        assert_eq!(source(&controller, "arm-guid")["data"]["childGuids"], json!(["mesh-guid"]));
        assert_eq!(source(&controller, "part-guid")["data"]["childGuids"], json!(["arm-guid"]));
        // The panel reloaded at the new revision and keeps the source selected under its new Part.
        assert_eq!((editor.guid.as_str(), editor.parent.as_str()), ("mesh-guid", "arm-guid"));

        controller.core_request(&json!({"op": "undo"}));
        assert_eq!(source(&controller, "mesh-guid")["parentPartGuid"], "part-guid");
        assert_eq!(source(&controller, "part-guid")["data"]["childGuids"], json!(["mesh-guid", "arm-guid"]));
    }
}
