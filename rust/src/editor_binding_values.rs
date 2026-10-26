//! Numeric editors under the armed parameter's bindings (port of the TRS binding editor and the manual
//! deformation editor of `panels/rustcore.d`): the transform channels of one node at the closest keypoint, and
//! one offset pair per vertex. A transform field writes when its edit is committed (drag released, Enter or
//! focus left), so a drag is one undo step; the deform list is applied as one step with its button.

use eframe::egui;
use serde_json::Value;
use soul2d_core::binding_values::{self, DeformValues, TrsChannel, TrsValues};

#[derive(Clone, Debug, PartialEq)]
struct Loaded {
    parameter: u32,
    node: u32,
    key: [usize; 2],
    revision: u64,
}

struct DeformDraft {
    original: DeformValues,
    offsets: Vec<[f64; 2]>,
    /// The model changed under an edited list: it can only be reloaded.
    conflict: bool,
}

impl DeformDraft {
    fn dirty(&self) -> bool {
        self.offsets != self.original.offsets
    }
}

#[derive(Default)]
pub struct ValuesEditor {
    loaded: Option<Loaded>,
    trs: Option<Result<TrsValues, String>>,
    deform: Option<Result<DeformDraft, String>>,
    edited: [bool; 6],
    /// The revision at which Apply was sent: once the document moves past it, the list reloads as clean.
    apply_sent_at: Option<u64>,
}

fn commit(response: &egui::Response, flag: &mut bool) -> bool {
    if response.changed() {
        *flag = true;
    }
    let done = *flag && (response.drag_stopped() || response.lost_focus());
    if done {
        *flag = false;
    }
    done
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut f64, speed: f64) -> egui::Response {
    ui.horizontal(|ui| {
        let caption = ui.label(label);
        ui.add(egui::DragValue::new(value).speed(speed).max_decimals(4)).labelled_by(caption.id)
    })
    .inner
}

impl ValuesEditor {
    pub fn editing_trs(&self) -> bool {
        self.edited.iter().any(|edited| *edited)
    }

    /// An editor holding an unapplied deform list, for tests of the callers that must not drop it.
    #[cfg(test)]
    pub fn with_draft() -> Self {
        let original = DeformValues { offsets: vec![[0.0, 0.0]], is_set: false };
        Self { deform: Some(Ok(DeformDraft { offsets: vec![[1.0, 0.0]], original, conflict: false })), ..Self::default() }
    }

    /// The unapplied deform list, if any: a document replacement or save should not silently drop it.
    pub fn has_draft(&self) -> bool {
        matches!(&self.deform, Some(Ok(draft)) if draft.dirty())
    }

    fn reload(&mut self, model: &Value, at: &Loaded) {
        self.trs = Some(binding_values::read_trs(model, at.parameter, at.node, at.key));
        self.deform = Some(binding_values::read_deform(model, at.parameter, at.node, at.key).map(|original| DeformDraft {
            offsets: original.offsets.clone(),
            original,
            conflict: false,
        }));
        self.loaded = Some(at.clone());
        self.edited = [false; 6];
    }

    /// Draws the editors for `node` at `key` of `parameter` and returns the requests to run (in order).
    pub fn show(&mut self, ui: &mut egui::Ui, model: &Value, revision: u64, parameter: u32, node: u32, key: [usize; 2]) -> Vec<Value> {
        let at = Loaded { parameter, node, key, revision };
        let same_target = self.loaded.as_ref().is_some_and(|l| (l.parameter, l.node, l.key) == (parameter, node, key));
        let applied = self.apply_sent_at.is_some_and(|sent| sent != revision);
        if applied {
            self.apply_sent_at = None;
        }
        if self.loaded.as_ref() != Some(&at) {
            if same_target && self.has_draft() && !applied {
                // Keep the edited offsets; the model changed elsewhere.
                if let Some(Ok(draft)) = self.deform.as_mut() {
                    draft.conflict = true;
                }
                self.trs = Some(binding_values::read_trs(model, parameter, node, key));
                if let Some(loaded) = self.loaded.as_mut() {
                    loaded.revision = revision;
                }
            } else {
                self.reload(model, &at);
            }
        }
        let mut requests = Vec::new();
        ui.separator();
        ui.strong(format!("Binding values at key {}, {}", key[0], key[1]));
        match self.trs.clone() {
            Some(Ok(mut values)) => {
                if self.trs_fields(ui, &mut values, parameter, node, key, &mut requests) {
                    self.trs = Some(Ok(values));
                }
            }
            Some(Err(error)) => {
                ui.label(error);
            }
            None => {}
        }
        self.deform_section(ui, parameter, node, key, revision, &mut requests);
        requests
    }

    /// Returns true when a field changed, so the caller keeps the edited copy until the next reload.
    fn trs_fields(
        &mut self,
        ui: &mut egui::Ui,
        values: &mut TrsValues,
        parameter: u32,
        node: u32,
        key: [usize; 2],
        requests: &mut Vec<Value>,
    ) -> bool {
        let before = values.clone();
        let mut done = [false; 6];
        done[0] |= commit(&field(ui, "Translation X", &mut values.translation[0], 0.1), &mut self.edited[0]);
        done[0] |= commit(&field(ui, "Translation Y", &mut values.translation[1], 0.1), &mut self.edited[0]);
        done[1] |= commit(&field(ui, "Translation Z", &mut values.translation_z, 0.1), &mut self.edited[1]);
        done[2] |= commit(&field(ui, "Scale X", &mut values.scale[0], 0.01), &mut self.edited[2]);
        done[2] |= commit(&field(ui, "Scale Y", &mut values.scale[1], 0.01), &mut self.edited[2]);
        done[3] |= commit(&field(ui, "Rotation (degrees)", &mut values.rotation_degrees, 0.5), &mut self.edited[3]);
        done[4] |= commit(&field(ui, "Rotation X (degrees)", &mut values.rotation_x_degrees, 0.5), &mut self.edited[4]);
        done[5] |= commit(&field(ui, "Rotation Y (degrees)", &mut values.rotation_y_degrees, 0.5), &mut self.edited[5]);
        for (flag, channel) in done.into_iter().zip([
            TrsChannel::Translation,
            TrsChannel::TranslationZ,
            TrsChannel::Scale,
            TrsChannel::Rotation,
            TrsChannel::RotationX,
            TrsChannel::RotationY,
        ]) {
            if flag {
                match binding_values::trs_request(parameter, node, key, channel, values) {
                    Ok(request) => requests.push(request),
                    Err(error) => {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                }
            }
        }
        *values != before
    }

    fn deform_section(
        &mut self,
        ui: &mut egui::Ui,
        parameter: u32,
        node: u32,
        key: [usize; 2],
        revision: u64,
        requests: &mut Vec<Value>,
    ) {
        ui.strong("Manual deformation");
        let mut reload = false;
        match self.deform.as_mut() {
            Some(Ok(draft)) => {
                ui.label(if draft.original.is_set {
                    "An explicit manual deform key is set here."
                } else {
                    "No explicit manual key; offsets start at zero."
                });
                if draft.conflict {
                    ui.label("Draft retained; reload to discard it or resolve the changed geometry/key.");
                }
                let count = draft.offsets.len();
                egui::ScrollArea::vertical().id_salt("binding-deform-offsets").max_height(160.0).show_rows(ui, 22.0, count, |ui, rows| {
                    for index in rows {
                        ui.add_enabled_ui(!draft.conflict, |ui| {
                            ui.horizontal(|ui| {
                                let caption = ui.label(format!("Vertex {index} offset"));
                                let [x, y] = &mut draft.offsets[index];
                                ui.add(egui::DragValue::new(x).speed(0.01).max_decimals(3).prefix("x ")).labelled_by(caption.id);
                                ui.add(egui::DragValue::new(y).speed(0.01).max_decimals(3).prefix("y ")).labelled_by(caption.id);
                            });
                        });
                    }
                });
                if ui.add_enabled(draft.dirty() && !draft.conflict, egui::Button::new("Apply manual deformation")).clicked() {
                    match binding_values::deform_request(parameter, node, key, &draft.offsets) {
                        Ok(request) => {
                            requests.push(request);
                            self.apply_sent_at = Some(revision);
                        }
                        Err(error) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                    }
                }
                reload = ui.button("Reload manual deformation").clicked();
            }
            Some(Err(error)) => {
                ui.label(error.as_str());
            }
            None => {}
        }
        if reload {
            self.loaded = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::{assert_all_named, center, find, has, temp_dir, texts, Harness};
    use egui::accesskit::Role;
    use serde_json::json;
    use soul2d_core::editor_controller::EditorController;

    struct Rig {
        controller: EditorController,
        editor: ValuesEditor,
        harness: Harness,
        parameter: u32,
        requests: Vec<Value>,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let dir = temp_dir(name);
            image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255])).save(dir.join("p.png")).unwrap();
            let mut controller = EditorController::new();
            controller.core_request(&json!({"op": "new"}));
            controller.core_request(&json!({"op": "merge.images", "paths": [dir.join("p.png").to_str().unwrap()]}));
            let made = controller.core_request(&json!({"op": "parameter.create", "name": "Slide", "dimensions": 1, "min": [0, 0], "max": [1, 1]}));
            let parameter = made["result"]["uuid"].as_u64().unwrap() as u32;
            let mut harness = Harness::new();
            harness.screen = egui::vec2(900.0, 1800.0);
            Self { controller, editor: ValuesEditor::default(), harness, parameter, requests: Vec::new() }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> crate::editor_test_support::Out {
            let snapshot = self.controller.snapshot().clone();
            let revision = snapshot["revision"].as_u64().unwrap();
            let (editor, parameter) = (&mut self.editor, self.parameter);
            let mut produced = Vec::new();
            let out = self.harness.frame(events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    produced.extend(editor.show(ui, &snapshot["model"], revision, parameter, 2, [1, 0]));
                });
            });
            self.requests.extend(produced);
            out
        }

        fn run_requests(&mut self) {
            for request in std::mem::take(&mut self.requests) {
                let response = self.controller.core_request(&request);
                assert_eq!(response["succeeded"], true, "{request}: {response}");
            }
        }

        /// Drags the spin button by `dx` pixels.
        fn drag(&mut self, label: &str, dx: f32) {
            self.frame(Vec::new());
            let laid_out = self.frame(Vec::new());
            let from = center(&laid_out, Role::SpinButton, label);
            let to = from + egui::vec2(dx, 0.0);
            let press = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
            self.frame(vec![egui::Event::PointerMoved(from + egui::vec2(dx / 2.0, 0.0))]);
            self.frame(vec![egui::Event::PointerMoved(to)]);
            assert!(self.editor.editing_trs(), "the pending drag is visible before its release");
            self.frame(vec![press(to, false)]);
            self.frame(Vec::new());
            assert!(!self.editor.editing_trs(), "release has emitted the transform request");
        }
    }

    #[test]
    fn every_field_is_named_and_shows_the_keypoints_neutral_values() {
        let mut rig = Rig::new("binding-values-names");
        rig.frame(Vec::new());
        let output = rig.frame(Vec::new());
        for label in [
            "Translation X", "Translation Y", "Translation Z", "Scale X", "Scale Y", "Rotation (degrees)",
            "Rotation X (degrees)", "Rotation Y (degrees)",
        ] {
            assert!(texts(&output).iter().any(|t| t == label), "{label}: {:?}", texts(&output));
            assert!(find(&output, Role::SpinButton, label).is_some(), "{label} is a named spin button");
        }
        assert!(has(&output, Role::Button, "Apply manual deformation") && has(&output, Role::Button, "Reload manual deformation"));
        assert!(texts(&output).iter().any(|t| t == "No explicit manual key; offsets start at zero."));
        assert!(texts(&output).iter().any(|t| t == "Binding values at key 1, 0"));
        let checked = assert_all_named(&output, "binding values");
        assert!(checked >= 8 + 8 + 2, "four vertices of offsets and the eight channels: {checked}");
    }

    #[test]
    fn dragging_a_channel_writes_one_set_trs_request_when_released_and_the_value_comes_back() {
        let mut rig = Rig::new("binding-values-drag");
        rig.drag("Translation X", 40.0);
        assert_eq!(rig.requests.len(), 1, "a drag is one request: {:?}", rig.requests);
        let request = rig.requests[0].clone();
        assert_eq!((request["op"].as_str(), request["nodes"].clone(), request["key"].clone()), (Some("binding.set_trs"), json!([2]), json!([1, 0])));
        assert!(request["translation"][0].as_f64().unwrap() > 0.0, "{request}");
        assert_eq!(request["translation"][1], 0.0);
        let revision = rig.controller.snapshot()["revision"].as_u64().unwrap();
        rig.run_requests();
        assert_eq!(rig.controller.snapshot()["revision"].as_u64().unwrap(), revision + 1);
        rig.frame(Vec::new());
        let output = rig.frame(Vec::new());
        let node = find(&output, Role::SpinButton, "Translation X").unwrap();
        assert!(node.numeric_value().unwrap() > 0.0, "the stored value is shown after the reload");
        assert!(rig.requests.is_empty(), "no further request without another edit");
    }

    impl Rig {
        fn click(&mut self, label: &str) {
            let snapshot = self.controller.snapshot().clone();
            let sink = std::cell::RefCell::new(Vec::new());
            let (editor, parameter) = (&mut self.editor, self.parameter);
            self.harness.click(Role::Button, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let produced = editor.show(ui, &snapshot["model"], snapshot["revision"].as_u64().unwrap(), parameter, 2, [1, 0]);
                    sink.borrow_mut().extend(produced);
                });
            });
            self.requests.extend(sink.into_inner());
        }
    }

    #[test]
    fn the_deform_list_applies_as_one_request_and_a_foreign_change_keeps_the_edited_list() {
        let mut rig = Rig::new("binding-values-deform");
        rig.frame(Vec::new());
        let offsets = |rig: &Rig| match &rig.editor.deform {
            Some(Ok(draft)) => draft.offsets.clone(),
            other => panic!("{:?}", other.as_ref().map(|r| r.is_ok())),
        };
        assert_eq!(offsets(&rig).len(), 4);
        if let Some(Ok(draft)) = rig.editor.deform.as_mut() {
            draft.offsets[1] = [1.5, -2.0];
        }
        assert!(rig.editor.has_draft());
        let revision = rig.controller.snapshot()["revision"].as_u64().unwrap();
        rig.click("Apply manual deformation");
        assert_eq!(rig.requests.len(), 1, "one request for the whole list");
        assert_eq!(rig.requests[0]["op"], "binding.set_deform");
        assert_eq!(rig.requests[0]["values"], json!([0.0, 0.0, 1.5, -2.0, 0.0, 0.0, 0.0, 0.0]));
        rig.run_requests();
        assert_eq!(rig.controller.snapshot()["revision"].as_u64().unwrap(), revision + 1);
        rig.frame(Vec::new());
        rig.frame(Vec::new());
        assert!(!rig.editor.has_draft(), "reloaded from the document after the apply");
        assert!(matches!(&rig.editor.deform, Some(Ok(d)) if d.original.is_set && d.offsets[1] == [1.5, -2.0]));

        // An edit not applied yet survives a change made elsewhere and can only be reloaded.
        if let Some(Ok(draft)) = rig.editor.deform.as_mut() {
            draft.offsets[2] = [9.0, 9.0];
        }
        rig.controller.core_request(&json!({"op": "binding.set_deform", "parameter": rig.parameter, "nodes": [2], "key": [1, 0], "values": [0, 0, 0, 0, 0, 0, 3, 3]}));
        rig.frame(Vec::new());
        let output = rig.frame(Vec::new());
        assert!(texts(&output).iter().any(|t| t.starts_with("Draft retained; reload to discard it")), "{:?}", texts(&output));
        assert!(rig.editor.has_draft());
        let apply = find(&output, Role::Button, "Apply manual deformation").unwrap();
        assert!(apply.is_disabled(), "a conflicted list cannot be applied");
        rig.click("Reload manual deformation");
        rig.frame(Vec::new());
        assert!(!rig.editor.has_draft());
        assert!(matches!(&rig.editor.deform, Some(Ok(d)) if d.offsets[3] == [3.0, 3.0]));
        assert!(rig.requests.is_empty());
    }

    #[test]
    fn a_node_without_deform_geometry_says_so() {
        let mut rig = Rig::new("binding-values-none");
        let snapshot = rig.controller.snapshot().clone();
        let out = rig.harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Node 1 is the root Node: transform values exist, deform geometry does not.
                rig.editor.show(ui, &snapshot["model"], 1, rig.parameter, 1, [1, 0]);
            });
        });
        assert!(texts(&out).iter().any(|t| t == "Node 1 has no deform geometry"), "{:?}", texts(&out));
        assert!(!has(&out, Role::Button, "Apply manual deformation"));
    }
}
