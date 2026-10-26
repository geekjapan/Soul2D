use eframe::egui;
use serde_json::{json, Value};

#[derive(Default)]
pub struct SequencePanel {
    pub directory: String,
    pub prefix: String,
    selected: usize,
    at: Option<(String, u64, i64, usize, Option<u64>)>,
    image_frame: f64,
    interpolation: String,
    step: u32,
    group: Option<u64>,
    placement: [f64; 4],
    original: Option<(f64, String, u32, [f64; 4])>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::Harness;
    use egui::accesskit::Role;
    use soul2d_core::Session;

    #[test]
    fn remote_quit_sequence_drafts_compare_loaded_values_and_reverts() {
        let snapshot = json!({"revision":1,"model":{"param":[],"animations":{"Idle":{
            "images":[{"sequence":{},"x":0,"y":0,"scale":1,"opacity":1}]
        }}}});
        let mut panel = SequencePanel::default();
        let mut harness = Harness::new();
        harness.frame(Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui|
                panel.show(ui, &snapshot, "Idle", 0, &mut Vec::new()));
        });
        assert!(!panel.has_draft(), "opening a sequence is clean");
        let original = panel.original.clone().unwrap();
        for field in 0..7 {
            match field {
                0..=3 => panel.placement[field] += 0.25,
                4 => panel.image_frame += 1.0,
                5 => panel.interpolation = "Stepped".into(),
                _ => panel.step += 1,
            }
            assert!(panel.has_draft(), "field {field}");
            (panel.image_frame, panel.interpolation, panel.step, panel.placement) = original.clone();
            assert!(!panel.has_draft(), "reverted field {field}");
        }
    }

    fn draw<'a>(
        panel: &'a mut SequencePanel,
        session: &'a mut Session,
        frame: i64,
    ) -> impl FnMut(&egui::Context) + 'a {
        move |ctx| {
            let mut requests = Vec::new();
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.show(ui, &session.snapshot(), "Idle", frame, &mut requests)
            });
            for request in requests {
                let result = session.handle_request(&request);
                assert_eq!(result["succeeded"], true, "{request}: {result}");
            }
        }
    }

    #[test]
    fn sequence_controls_import_key_exposures_and_delete_through_session() {
        let dir = crate::scratch::scratch(
            std::env::temp_dir().join(format!("soul2d-sequence-ui-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        for number in [10, 11, 13] {
            image::RgbaImage::from_pixel(2, 2, image::Rgba([number, 0, 0, 255]))
                .save(dir.join(format!("frame{number}.png")))
                .unwrap();
        }
        let model = json!({"nodes":{"uuid":1,"name":"Root","type":"Node","children":[]},
            "param":[{"groupUUID":10,"name":"Body","children":[]}],
            "animations":{"Idle":{"length":8,"timestep":0.1,"additive":false,"animationWeight":1,
                "leadIn":-1,"leadOut":-1,"lanes":[]}}});
        let file = dir.join("fixture.inx");
        std::fs::write(
            &file,
            soul2d_core::model_import::inx_bytes(&model, &[]).unwrap(),
        )
        .unwrap();
        let mut session = Session::new();
        assert_eq!(
            session.handle_request(&json!({"op":"open","path":file}))["succeeded"],
            true
        );
        let mut panel = SequencePanel {
            directory: dir.to_string_lossy().into_owned(),
            prefix: "frame".into(),
            ..Default::default()
        };
        let mut harness = Harness::new();
        harness.screen = egui::vec2(1200.0, 1400.0);
        harness.click(
            Role::Button,
            "Add sequence",
            draw(&mut panel, &mut session, 0),
        );
        let clip = |session: &Session| session.snapshot()["model"]["animations"]["Idle"].clone();
        assert_eq!(
            clip(&session)["images"][0]["sequence"]["files"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        harness.click(
            Role::CheckBox,
            "Guide only (exclude from export)",
            draw(&mut panel, &mut session, 0),
        );
        assert_eq!(clip(&session)["images"][0]["guide"], true);
        harness.click(
            Role::CheckBox,
            "In front of model",
            draw(&mut panel, &mut session, 0),
        );
        assert_eq!(clip(&session)["images"][0]["front"], true);
        panel.placement = [12.0, -6.0, 2.0, 0.5];
        harness.click(
            Role::Button,
            "Apply image placement",
            draw(&mut panel, &mut session, 0),
        );
        let placed = clip(&session)["images"][0].clone();
        for (key, value) in [("x", 12.0), ("y", -6.0), ("scale", 2.0), ("opacity", 0.5)] {
            assert_eq!(placed[key], value);
        }
        panel.image_frame = 3.0;
        harness.click(
            Role::Button,
            "Set timing key",
            draw(&mut panel, &mut session, 0),
        );
        assert_eq!(
            clip(&session)["images"][0]["timeRemap"]["keyframes"][0]["value"],
            3.0
        );
        harness.click(Role::Button, "2 frames", draw(&mut panel, &mut session, 0));
        let before = session.snapshot()["revision"].as_u64().unwrap();
        harness.click(
            Role::Button,
            "Set frame step key",
            draw(&mut panel, &mut session, 0),
        );
        assert_eq!(session.snapshot()["revision"].as_u64().unwrap(), before + 1);
        assert_eq!(clip(&session)["frameStep"]["keyframes"][0]["value"], 2);
        harness.frame(Vec::new(), draw(&mut panel, &mut session, 5));
        panel.step = 4;
        harness.click(
            Role::Button,
            "Set frame step key",
            draw(&mut panel, &mut session, 5),
        );
        assert_eq!(
            clip(&session)["frameStep"]["keyframes"][1],
            json!({"frame":5,"value":4})
        );
        assert_eq!(
            session.handle_request(&json!({"op":"undo"}))["succeeded"],
            true
        );
        assert_eq!(
            clip(&session)["frameStep"]["keyframes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        harness.click(
            Role::ComboBox,
            "Frame step target",
            draw(&mut panel, &mut session, 5),
        );
        harness.click(Role::Button, "Body", draw(&mut panel, &mut session, 5));
        harness.click(Role::Button, "3 frames", draw(&mut panel, &mut session, 5));
        harness.click(
            Role::Button,
            "Set frame step key",
            draw(&mut panel, &mut session, 5),
        );
        assert_eq!(
            clip(&session)["frameStepGroups"]["10"]["keyframes"],
            json!([{"frame":5,"value":3}])
        );
        assert_eq!(
            session.handle_request(&json!({"op":"undo"}))["succeeded"],
            true
        );
        assert!(clip(&session)["frameStepGroups"].is_null());
        harness.click(
            Role::Button,
            "Clear time remapping",
            draw(&mut panel, &mut session, 5),
        );
        assert!(clip(&session)["images"][0]["timeRemap"].is_null());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([12, 0, 0, 255]))
            .save(dir.join("frame12.png"))
            .unwrap();
        harness.click(
            Role::Button,
            "Reload sequence",
            draw(&mut panel, &mut session, 5),
        );
        let numbers: Vec<_> = clip(&session)["images"][0]["sequence"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, [10, 11, 12, 13]);
        harness.click(
            Role::Button,
            "Delete sequence",
            draw(&mut panel, &mut session, 5),
        );
        assert!(clip(&session)["images"]
            .as_array()
            .is_none_or(Vec::is_empty));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

fn keys_with(track: &Value, frame: i64, value: Option<Value>) -> Vec<Value> {
    let mut keys: Vec<Value> = track["keyframes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|key| key["frame"].as_i64() != Some(frame))
        .cloned()
        .collect();
    if let Some(value) = value {
        keys.push(json!({"frame": frame, "value": value}));
    }
    keys.sort_by_key(|key| key["frame"].as_i64().unwrap_or_default());
    keys
}

impl SequencePanel {
    pub fn has_draft(&self) -> bool {
        self.original.as_ref().is_some_and(|(frame, interpolation, step, placement)|
            self.image_frame != *frame || self.interpolation != *interpolation
                || self.step != *step || self.placement != *placement)
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Value,
        name: &str,
        frame: i64,
        requests: &mut Vec<Value>,
    ) {
        let clip = &snapshot["model"]["animations"][name];
        let images = clip["images"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.selected = self.selected.min(images.len().saturating_sub(1));
        let track = images.get(self.selected).unwrap_or(&Value::Null);
        let timing = &track["timeRemap"];
        fn groups<'a>(list: &'a Value, out: &mut Vec<(u64, &'a str)>) {
            for item in list.as_array().into_iter().flatten() {
                if let Some(id) = item["groupUUID"].as_u64() {
                    out.push((id, item["name"].as_str().unwrap_or("Unnamed group")));
                    groups(&item["children"], out);
                }
            }
        }
        let mut choices = Vec::new();
        groups(&snapshot["model"]["param"], &mut choices);
        if self
            .group
            .is_some_and(|id| !choices.iter().any(|(group, _)| *group == id))
        {
            self.group = None;
        }
        let exposure = match self.group {
            Some(id) => &clip["frameStepGroups"][id.to_string()],
            None => &clip["frameStep"],
        };
        let at = (
            name.to_owned(),
            snapshot["revision"].as_u64().unwrap_or_default(),
            frame,
            self.selected,
            self.group,
        );
        if self.at.as_ref() != Some(&at) {
            self.image_frame = timing["keyframes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|key| key["frame"].as_i64() == Some(frame))
                .and_then(|key| key["value"].as_f64())
                .unwrap_or(frame as f64);
            self.interpolation = timing["interpolation"]
                .as_str()
                .unwrap_or("Linear")
                .to_owned();
            self.placement = [
                track["x"].as_f64().unwrap_or(0.0),
                track["y"].as_f64().unwrap_or(0.0),
                track["scale"].as_f64().unwrap_or(1.0),
                track["opacity"].as_f64().unwrap_or(1.0),
            ];
            self.step = exposure["keyframes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|key| key["frame"].as_i64().is_some_and(|at| at <= frame))
                .last()
                .and_then(|key| key["value"].as_f64())
                .unwrap_or(1.0) as u32;
            self.at = Some(at);
            self.original = Some((self.image_frame, self.interpolation.clone(), self.step, self.placement));
        }
        ui.label("Import PNG or JPEG files with numbered names from a folder.");
        ui.horizontal(|ui| {
            let label = ui.label("Sequence folder");
            ui.text_edit_singleline(&mut self.directory)
                .labelled_by(label.id);
        });
        ui.horizontal(|ui| {
            let label = ui.label("Filename prefix");
            ui.text_edit_singleline(&mut self.prefix)
                .labelled_by(label.id);
        });
        if ui
            .add_enabled(
                !self.directory.trim().is_empty(),
                egui::Button::new("Add sequence"),
            )
            .clicked()
        {
            requests.push(
                json!({"op": "animation.image.sequence", "name": name, "directory": self.directory,
                "prefix": self.prefix}),
            );
            ui.close();
        }
        if !images.is_empty() {
            egui::ComboBox::from_label("Image track")
                .selected_text(format!("{}", self.selected + 1))
                .show_ui(ui, |ui| {
                    for (index, track) in images.iter().enumerate() {
                        let label = format!(
                            "{}: {}",
                            index + 1,
                            if track["sequence"].is_object() {
                                "Sequence"
                            } else {
                                "Still"
                            }
                        );
                        if ui
                            .selectable_value(&mut self.selected, index, label)
                            .changed()
                        {
                            self.at = None;
                        }
                    }
                });
            // Apply the selection on the next frame before exposing controls for another track.
            if self.at.is_none() {
                return;
            }
            if track["sequence"].is_object() {
                ui.horizontal(|ui| {
                    if ui.button("Reload sequence").clicked() {
                        requests.push(json!({"op": "animation.image.sequence", "name": name, "index": self.selected}));
                        ui.close();
                    }
                    if ui.button("Delete sequence").clicked() {
                        requests.push(json!({"op": "animation.image.delete", "name": name, "index": self.selected}));
                        ui.close();
                    }
                });
                let mut guide = track["guide"].as_bool().unwrap_or(false);
                if ui
                    .checkbox(&mut guide, "Guide only (exclude from export)")
                    .changed()
                {
                    requests.push(json!({"op": "animation.image.set", "name": name, "index": self.selected, "guide": guide}));
                }
                let mut front = track["front"].as_bool().unwrap_or(false);
                if ui.checkbox(&mut front, "In front of model").changed() {
                    requests.push(json!({"op": "animation.image.set", "name": name, "index": self.selected, "front": front}));
                }
                ui.horizontal(|ui| {
                    for (index, label) in ["Image X", "Image Y", "Image scale", "Image opacity"]
                        .iter()
                        .enumerate()
                    {
                        let label = ui.label(*label);
                        ui.add(egui::DragValue::new(&mut self.placement[index]).speed(0.1))
                            .labelled_by(label.id);
                    }
                });
                if ui.button("Apply image placement").clicked() {
                    let [x, y, scale, opacity] = self.placement;
                    requests.push(
                        json!({"op":"animation.image.set", "name":name, "index":self.selected,
                        "x":x,"y":y,"scale":scale,"opacity":opacity}),
                    );
                }
                ui.separator();
                ui.label(format!("Time remapping on clip frame {frame}"));
                ui.horizontal(|ui| {
                    let label = ui.label("Image frame");
                    ui.add(egui::DragValue::new(&mut self.image_frame).range(0.0..=1_000_000.0))
                        .labelled_by(label.id);
                });
                egui::ComboBox::from_label("Timing interpolation")
                    .selected_text(&self.interpolation)
                    .show_ui(ui, |ui| {
                        for mode in ["Linear", "Stepped", "Nearest", "Cubic"] {
                            ui.selectable_value(&mut self.interpolation, mode.to_owned(), mode);
                        }
                    });
                let time = |keys: Value| {
                    json!({"op": "animation.image.time", "name": name, "index": self.selected,
                    "keyframes": keys, "interpolation": self.interpolation})
                };
                ui.horizontal(|ui| {
                    if ui.button("Set timing key").clicked() {
                        requests.push(time(json!(keys_with(
                            timing,
                            frame,
                            Some(json!(self.image_frame))
                        ))));
                    }
                    if ui
                        .add_enabled(
                            timing.is_object(),
                            egui::Button::new("Clear time remapping"),
                        )
                        .clicked()
                    {
                        requests.push(time(Value::Null));
                    }
                });
                for key in timing["keyframes"].as_array().into_iter().flatten() {
                    let Some(at) = key["frame"].as_i64() else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        ui.label(format!("Clip {at}: image {}", key["value"]));
                        if ui.button(format!("Delete timing key {at}")).clicked() {
                            requests.push(time(json!(keys_with(timing, at, None))));
                        }
                    });
                }
            }
        }
        ui.separator();
        ui.label(format!("Frame step on clip frame {frame}"));
        let selected = choices
            .iter()
            .find(|(id, _)| Some(*id) == self.group)
            .map_or("Whole clip", |(_, name)| *name);
        egui::ComboBox::from_label("Frame step target")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(&mut self.group, None, "Whole clip")
                    .changed()
                {
                    self.at = None;
                }
                for (id, label) in choices {
                    if ui
                        .selectable_value(&mut self.group, Some(id), label)
                        .changed()
                    {
                        self.at = None;
                    }
                }
            });
        if self.at.is_none() {
            return;
        }
        ui.label("Hold the picture for this many frames. Audio keeps its timing.");
        ui.horizontal(|ui| {
            let label = ui.label("Frames per exposure");
            ui.add(egui::DragValue::new(&mut self.step).range(1..=1_000_000))
                .labelled_by(label.id);
            for step in 1..=3 {
                if ui
                    .button(format!("{step} frame{}", if step == 1 { "" } else { "s" }))
                    .clicked()
                {
                    self.step = step;
                }
            }
        });
        let step = |keys: Value| {
            let mut request =
                json!({"op": "animation.frame_step", "name": name, "keyframes": keys});
            if let Some(id) = self.group {
                request["parameterGroup"] = json!(id);
            }
            request
        };
        ui.horizontal(|ui| {
            if ui.button("Set frame step key").clicked() {
                requests.push(step(json!(keys_with(
                    exposure,
                    frame,
                    Some(json!(self.step))
                ))));
            }
            if ui
                .add_enabled(exposure.is_object(), egui::Button::new("Clear frame step"))
                .clicked()
            {
                requests.push(step(Value::Null));
            }
        });
        for key in exposure["keyframes"].as_array().into_iter().flatten() {
            let Some(at) = key["frame"].as_i64() else {
                continue;
            };
            ui.horizontal(|ui| {
                ui.label(format!("From frame {at}: hold {} frames", key["value"]));
                if ui.button(format!("Delete frame step key {at}")).clicked() {
                    requests.push(step(json!(keys_with(exposure, at, None))));
                }
            });
        }
    }
}
