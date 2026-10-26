use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;

/// A successfully rendered review state: mutations are armed only while the document and draft
/// still match and at least one real frame has passed since this view was drawn.
#[derive(Clone, Copy)]
struct Presented {
    document_revision: u64,
    draft_revision: u64,
    frame: u64,
}

/// The INX Outline "Part split review" section: a read-only cached view of the split draft plus
/// reorder/remove/apply mutations. All heavy plan/overlay data comes from the Session; the GUI
/// owns only the selected part name, user feedback and the decoded preview texture.
#[derive(Default)]
pub(crate) struct PartSplitReview {
    /// `split.plan` result keyed by the draft revision it was read at; the Err variant is cached too.
    read: Option<(u64, Result<Value, String>)>,
    /// Preview texture for the selected part name (`None` = all parts), refreshed when either changes.
    overlay: Option<(Option<String>, egui::TextureHandle)>,
    selected: Option<String>,
    feedback: String,
    presented: Option<Presented>,
}

impl PartSplitReview {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Draw the review. Returns true only when the document changed (`split.apply`).
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, controller: &mut EditorController) -> bool {
        let snapshot = controller.snapshot();
        let draft = &snapshot["partSplit"];
        let draft_revision = draft["revision"].as_u64().unwrap_or(0);
        let document_revision = snapshot["revision"].as_u64().unwrap_or(0);
        let applied = draft["applied"].as_bool().unwrap_or(false);
        let frame = ui.ctx().cumulative_frame_nr();
        if snapshot["documentFormat"] != "inx" || draft["open"].as_bool() != Some(true) {
            self.reset();
            ui.label("Open a split source first (split.open).");
            return false;
        }

        if self.read.as_ref().map(|read| read.0) != Some(draft_revision) {
            self.overlay = None;
            self.feedback.clear();
            let result = controller.with_session(|session| {
                let response = session.handle_request(&json!({"op": "split.plan"}));
                if response["succeeded"].as_bool().unwrap_or(false) {
                    Ok(response["result"].clone())
                } else {
                    Err(response["message"].as_str().unwrap_or("split.plan failed").to_owned())
                }
            });
            self.read = Some((draft_revision, result));
        }
        let read = match &self.read {
            Some((_, read)) => read,
            None => unreachable!(),
        };

        let plan = match read {
            Ok(plan) => plan.clone(),
            Err(message) => {
                ui.label(format!("Split draft: {message}"));
                self.presented = None;
                return false;
            }
        };
        let parts = plan["parts"].as_array().cloned().unwrap_or_default();
        if let Some(selected) = &self.selected {
            if !parts.iter().any(|part| part["name"] == selected.as_str()) {
                self.selected = None;
                self.overlay = None;
            }
        }
        let source = &plan["plan"]["source"];
        ui.label(format!(
            "Source {}x{} — {} parts, {} unclaimed, {} multiply claimed",
            source["width"].as_u64().unwrap_or(0),
            source["height"].as_u64().unwrap_or(0),
            parts.len(),
            plan["unclaimedPixels"].as_u64().unwrap_or(0),
            plan["multiplyClaimedPixels"].as_u64().unwrap_or(0),
        ));
        if parts.is_empty() {
            ui.label("The draft has no parts yet.");
            self.presented = None;
            return false;
        }
        for part in &parts {
            let name = part["name"].as_str().unwrap_or("?");
            let pixels = part["pixels"].as_u64().unwrap_or(0);
            let bbox = &part["bbox"];
            let row = format!("{name}: {pixels} px at {bbox}");
            if ui
                .selectable_label(self.selected.as_deref() == Some(name), row)
                .clicked()
            {
                self.selected = Some(name.to_owned());
                self.overlay = None;
            }
        }
        if let Some((_, texture)) = &self.overlay {
            let size = texture.size_vec2();
            let fit = 220.0_f32.min(ui.available_width()).max(1.0);
            let scale = fit / size.x.max(size.y).max(1.0);
            ui.image((texture.id(), size * scale));
        }
        if !self.feedback.is_empty() {
            ui.label(self.feedback.clone());
        }

        // Arm only after this exact view has actually been displayed for a frame; the draft or
        // document changing since disarms every mutation button for one fresh frame.
        let armed = self
            .presented
            .is_some_and(|p| p.document_revision == document_revision
                && p.draft_revision == draft_revision
                && p.frame < frame);
        self.presented = Some(Presented {
            document_revision,
            draft_revision,
            frame,
        });
        if !armed {
            ui.ctx().request_repaint();
        }

        let mut action: Option<Value> = None;
        let mut preview: Option<Option<String>> = None;
        ui.horizontal(|ui| {
            if ui.button("Preview all parts").clicked() {
                preview = Some(None);
            }
            if let Some(name) = &self.selected {
                if ui.button(format!("Preview {name}")).clicked() {
                    preview = Some(Some(name.clone()));
                }
            }
        });
        if let Some(name) = self.selected.clone() {
            let index = parts.iter().position(|part| part["name"] == name).unwrap_or(0);
            ui.horizontal(|ui| {
                let mut names: Vec<Value> = parts.iter().map(|part| part["name"].clone()).collect();
                if ui.add_enabled(armed && index > 0, egui::Button::new("Move part forward")).clicked() {
                    names.swap(index, index - 1);
                    action = Some(json!({"op": "split.part.order", "names": names}));
                }
                if ui
                    .add_enabled(armed && index + 1 < parts.len(), egui::Button::new("Move part backward"))
                    .clicked()
                {
                    names.swap(index, index + 1);
                    action = Some(json!({"op": "split.part.order", "names": names}));
                }
                if ui
                    .add_enabled(armed, egui::Button::new("Remove proposed part"))
                    .clicked()
                {
                    action = Some(json!({"op": "split.part.remove", "name": name}));
                }
            });
        }
        if applied {
            ui.label("This plan is already applied — its Parts are in the document (Undo restores the state before it).");
        } else if ui
            .add_enabled(armed && parts.iter().all(|part| part["pixels"].as_u64().unwrap_or(0) > 0),
                egui::Button::new("Apply reviewed split"))
            .on_hover_text("Adds the reviewed Parts to the document in one undo step")
            .clicked()
        {
            action = Some(json!({"op": "split.apply"}));
        }

        if let Some(which) = preview {
            let name = which.clone();
            let response = controller.with_session(|session| {
                session.handle_request(&json!({"op": "split.overlay", "part": name}))
            });
            if response["succeeded"].as_bool().unwrap_or(false) {
                match self.decode_overlay(ui.ctx(), &response["result"]) {
                    Ok(texture) => self.overlay = Some((which, texture)),
                    Err(message) => self.feedback = message,
                }
            } else {
                self.feedback = response["message"].as_str().unwrap_or("overlay failed").to_owned();
            }
        }
        if let Some(request) = action {
            let document_step = request["op"] == "split.apply";
            let response = controller.core_request(&request);
            if response["succeeded"].as_bool().unwrap_or(false) {
                self.feedback.clear();
                if !document_step {
                    self.read = None;
                    self.overlay = None;
                }
                self.presented = None;
                return document_step;
            }
            self.feedback = response["message"].as_str().unwrap_or("request failed").to_owned();
            self.presented = None;
        }
        false
    }

    fn decode_overlay(&mut self, ctx: &egui::Context, result: &Value) -> Result<egui::TextureHandle, String> {
        let base64 = result["png"].as_str().ok_or("overlay returned no png")?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, base64)
            .map_err(|error| format!("overlay png: {error}"))?;
        let rgba = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|error| format!("overlay png: {error}"))?
            .to_rgba8();
        let max = ctx.input(|input| input.max_texture_side) as u32;
        let (width, height) = rgba.dimensions();
        let image = if width > max || height > max {
            let scale = max as f32 / width.max(height) as f32;
            let (w, h) = ((width as f32 * scale).max(1.0) as u32, (height as f32 * scale).max(1.0) as u32);
            image::DynamicImage::ImageRgba8(rgba)
                .resize_exact(w, h, image::imageops::FilterType::Nearest)
                .to_rgba8()
        } else {
            rgba
        };
        let (w, h) = image.dimensions();
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], image.as_raw());
        Ok(ctx.load_texture("split-review", color, egui::TextureOptions::LINEAR))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::{center, find, temp_dir, texts, Harness};
    use egui::accesskit::Role;

    struct Rig {
        controller: EditorController,
        panel: PartSplitReview,
        harness: Harness,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let dir = temp_dir(name);
            let mut image = image::RgbaImage::new(8, 8);
            for (x, _, px) in image.enumerate_pixels_mut() {
                *px = image::Rgba(if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
            }
            let path = dir.join("source.png");
            image::DynamicImage::ImageRgba8(image).save(&path).unwrap();
            let mut controller = EditorController::new();
            let opened = controller.core_request(
                &json!({"op": "split.open", "path": path.to_str().unwrap()}));
            assert_eq!(opened["succeeded"], true, "{opened}");
            Self { controller, panel: PartSplitReview::default(), harness: Harness::new() }
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

        fn click(&mut self, role: Role, label: &str) -> crate::editor_test_support::Out {
            let (panel, controller) = (&mut self.panel, &mut self.controller);
            self.harness.click(role, label, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show(ui, controller);
                });
            })
        }

        fn set(&mut self, name: &str, rect: [u32; 4]) {
            let response = self.controller.core_request(&json!({
                "op": "split.part.set", "name": name,
                "steps": [{"op": "box", "rect": rect}],
            }));
            assert_eq!(response["succeeded"], true, "{response}");
        }

        fn draft_revision(&self) -> u64 {
            self.controller.snapshot()["partSplit"]["revision"].as_u64().unwrap()
        }
    }

    fn left_right(rig: &mut Rig) {
        rig.set("Left", [0, 0, 4, 8]);
        rig.set("Right", [4, 0, 4, 8]);
    }

    #[test]
    fn the_review_shows_stats_reorders_removes_and_applies() {
        let mut rig = Rig::new("split-review");
        left_right(&mut rig);
        let output = rig.draw();
        let all = texts(&output);
        assert!(all.iter().any(|t| t.contains("8x8") && t.contains("0 unclaimed")), "{all:?}");
        assert!(all.iter().any(|t| t.contains("Left") && t.contains("32")), "{all:?}");
        rig.click(Role::Button, "Preview all parts");
        rig.click(Role::Button, "Right: 32 px at [4,0,4,8]");
        let draft = rig.draft_revision();
        rig.click(Role::Button, "Move part forward");
        let output = rig.draw();
        let names: Vec<String> = texts(&output).into_iter()
            .filter_map(|t| t.split(": ").next().map(str::to_owned)).filter(|n| ["Left", "Right"].contains(&n.as_str())).collect();
        assert_eq!(rig.controller.with_session(|s| s.handle_request(&json!({"op":"split.plan"})))["result"]["plan"]["parts"][0]["name"], "Right");
        assert_eq!(names.len(), 2, "{:?}", texts(&output));
        assert_eq!(rig.draft_revision(), draft + 1);
        let revision = rig.controller.snapshot()["revision"].as_u64().unwrap();
        rig.click(Role::Button, "Move part backward");
        let output = rig.draw();
        let names: Vec<String> = texts(&output).into_iter()
            .filter_map(|t| t.split(": ").next().map(str::to_owned)).filter(|n| ["Left", "Right"].contains(&n.as_str())).collect();
        assert_eq!(names.len(), 2);
        assert_eq!(rig.controller.with_session(|s| s.handle_request(&json!({"op":"split.plan"})))["result"]["plan"]["parts"][0]["name"], "Left");
        assert_eq!(rig.controller.snapshot()["revision"].as_u64().unwrap(), revision, "draft edits are not document steps");
        rig.click(Role::Button, "Right: 32 px at [4,0,4,8]");
        rig.click(Role::Button, "Remove proposed part");
        let output = rig.draw();
        let all = texts(&output);
        assert!(!all.iter().any(|t| t.starts_with("Right: ")), "{all:?}");
        rig.set("Right", [4, 0, 4, 8]);
        rig.draw();
        let revision = rig.controller.snapshot()["revision"].as_u64().unwrap();
        rig.click(Role::Button, "Apply reviewed split");
        let after = rig.controller.snapshot();
        assert_eq!(after["revision"].as_u64().unwrap(), revision + 1, "one document step");
        assert_eq!(after["partSplit"]["applied"], json!(true));
        let children = after["model"]["nodes"]["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        let left = children[0]["uuid"].clone();
        let response = rig.controller.core_request(&json!({"op":"node.rename", "id":left, "name":"Renamed Left"}));
        assert_eq!(response["succeeded"], true);
        let response = rig.controller.core_request(&json!({"op":"parameter.create", "name":"Unrelated", "dimensions":1}));
        assert_eq!(response["succeeded"], true);
        rig.draw();
        let output = rig.draw();
        assert!(texts(&output).iter().any(|t| t.contains("already applied")), "{:?}", texts(&output));
        assert!(find(&output, Role::Button, "Apply reviewed split").is_none());
        rig.controller.core_request(&json!({"op": "undo"}));
        rig.controller.core_request(&json!({"op": "undo"}));
        rig.controller.core_request(&json!({"op": "undo"}));
        rig.draw();
        assert_eq!(rig.controller.snapshot()["partSplit"]["applied"], json!(false));
        rig.controller.core_request(&json!({"op": "redo"}));
        rig.draw();
        assert_eq!(rig.controller.snapshot()["partSplit"]["applied"], json!(true));
        assert!(find(&rig.draw(), Role::Button, "Apply reviewed split").is_none());
    }

    #[test]
    fn a_stale_click_is_disarmed_by_one_frame() {
        let mut rig = Rig::new("split-stale");
        left_right(&mut rig);
        let laid_out = rig.draw();
        let at = center(&laid_out, Role::Button, "Apply reviewed split");
        rig.set("Left", [0, 0, 2, 8]);
        let revision = rig.controller.snapshot()["revision"].as_u64().unwrap();
        let press = |pressed| egui::Event::PointerButton {
            pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
        };
        let frame = rig.harness.ctx.cumulative_frame_nr();
        let (panel, controller) = (&mut rig.panel, &mut rig.controller);
        let mut passes = 0;
        let output = rig.harness.frame(vec![egui::Event::PointerMoved(at), press(true), press(false)], |ctx| {
            passes += 1;
            egui::CentralPanel::default().show(ctx, |ui| { panel.show(ui, controller); });
            if passes == 1 { ctx.request_discard("test same-frame draft freshness"); }
            assert_eq!(ctx.cumulative_frame_nr(), frame);
        });
        assert!(passes >= 2);
        assert!(texts(&output).iter().any(|t| t.contains("Left") && t.contains("16")), "{:?}", texts(&output));
        assert_eq!(rig.controller.snapshot()["revision"].as_u64().unwrap(), revision,
            "no document edit from a stale click");
        rig.draw();
        rig.click(Role::Button, "Apply reviewed split");
        assert_eq!(rig.controller.snapshot()["partSplit"]["applied"], json!(true));
    }

    #[test]
    fn a_different_plan_can_append_after_review() {
        let mut rig = Rig::new("split-append");
        left_right(&mut rig);
        rig.draw();
        rig.click(Role::Button, "Apply reviewed split");
        rig.set("Left", [0, 0, 2, 8]);
        rig.draw();
        rig.click(Role::Button, "Apply reviewed split");
        let children = rig.controller.snapshot()["model"]["nodes"]["children"].as_array().unwrap();
        assert_eq!(children.len(), 4, "the different plan appends even with identical names");
        assert_eq!(children[0]["name"], children[2]["name"]);
    }

    #[test]
    fn referenced_removal_is_refused_and_empty_masks_cannot_apply() {
        let mut rig = Rig::new("split-refusal");
        left_right(&mut rig);
        let response = rig.controller.core_request(&json!({"op":"split.part.set", "name":"Right",
            "steps":[{"op":"part", "name":"Left"}]}));
        assert_eq!(response["succeeded"], true, "{response}");
        rig.draw();
        rig.click(Role::Button, "Left: 32 px at [0,0,4,8]");
        let before = rig.controller.snapshot().clone();
        rig.click(Role::Button, "Remove proposed part");
        assert_eq!(rig.controller.snapshot(), &before);
        assert!(!rig.panel.feedback.is_empty());
        rig.set("Right", [0, 0, 1, 1]);
        let response = rig.controller.core_request(&json!({"op":"split.part.set", "name":"Left",
            "steps":[{"op":"box", "rect":[0,0,4,8]}, {"op":"box", "rect":[4,0,4,8], "mode":"intersect"}]}));
        assert_eq!(response["succeeded"], true, "{response}");
        rig.draw();
        let before = rig.controller.snapshot().clone();
        rig.click(Role::Button, "Apply reviewed split");
        assert_eq!(rig.controller.snapshot(), &before);
    }

    #[test]
    fn oversized_source_downsamples_for_display_only() {
        let mut rig = Rig::new("split-bigtex2");
        left_right(&mut rig);
        rig.draw();
        let result = rig.controller.with_session(|s| s.handle_request(&json!({"op":"split.overlay"})));
        rig.harness.frame(Vec::new(), |ctx| {
            let previous = ctx.input(|input| input.max_texture_side);
            ctx.input_mut(|input| input.max_texture_side = 4);
            let texture = rig.panel.decode_overlay(ctx, &result["result"]).unwrap();
            ctx.input_mut(|input| input.max_texture_side = previous);
            rig.panel.overlay = Some((None, texture));
        });
        assert!(rig.panel.overlay.is_some());
        let (_, texture) = rig.panel.overlay.as_ref().unwrap();
        assert_eq!(texture.size(), [4, 4]);
        let plan = rig.controller.with_session(|s| s.handle_request(&json!({"op":"split.plan"})));
        assert_eq!(plan["result"]["parts"][0]["pixels"], 32);
        assert_eq!(rig.controller.snapshot()["partSplit"]["width"], 8);
        rig.panel.reset();
        assert!(rig.panel.overlay.is_none(), "reset drops the texture handle");
    }
}
