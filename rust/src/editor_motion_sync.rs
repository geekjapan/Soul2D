
use eframe::egui;
use serde_json::{json, Map, Value};
use soul2d_core::motion_sync::SHAPES;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(200);

const OPENNESS: [f64; 9] = [0.0, 0.25, 0.6, 1.0, 0.6, 0.25, 0.2, 0.6, 0.0];

struct MappingRow {
    parameter: Option<u32>,
    axis: usize,
    values: [f64; 9],
}

#[derive(Clone)]
struct ParamChoice {
    id: u32,
    name: String,
    axes: usize,
    min: [f64; 2],
    max: [f64; 2],
    default: [f64; 2],
}

fn leaf_parameters(snapshot: &Value) -> Vec<ParamChoice> {
    fn walk(list: &Value, out: &mut Vec<ParamChoice>) {
        for item in list.as_array().into_iter().flatten() {
            if item.get("groupUUID").is_some() {
                walk(&item["children"], out);
            } else if let Some(id) = item["uuid"].as_u64() {
                let axes = if item["is_vec2"].as_bool() == Some(true) { 2 } else { 1 };
                let pair = |key: &str, empty: f64| {
                    let mut values = [empty; 2];
                    for axis in 0..axes {
                        values[axis] = item[key][axis].as_f64().unwrap_or(empty);
                    }
                    values
                };
                out.push(ParamChoice {
                    id: id as u32,
                    name: item["name"].as_str().unwrap_or("Unnamed").to_owned(),
                    axes,
                    min: pair("min", 0.0),
                    max: pair("max", 0.0),
                    default: pair("defaults", 0.0),
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(&snapshot["model"]["param"], &mut out);
    out
}

pub struct MotionSyncPanel {
    audio_index: usize,
    recognizer: String,
    job: Option<u64>,
    status: Value,
    outgoing: VecDeque<Value>,
    last_op: String,
    last_poll: Option<Instant>,
    report: String,
    mappings: Vec<MappingRow>,
    custom_range: bool,
    from: i64,
    to: i64,
    extend: bool,
}

impl Default for MappingRow {
    fn default() -> Self {
        Self { parameter: None, axis: 0, values: [0.0; 9] }
    }
}

impl Default for MotionSyncPanel {
    fn default() -> Self {
        Self {
            audio_index: 0,
            recognizer: "phonetic".into(),
            job: None,
            status: Value::Null,
            outgoing: VecDeque::new(),
            last_op: String::new(),
            last_poll: None,
            report: String::new(),
            mappings: Vec::new(),
            custom_range: false,
            from: 0,
            to: 0,
            extend: false,
        }
    }
}

impl MotionSyncPanel {
    pub fn take_request(&mut self) -> Option<Value> {
        if let Some(request) = self.outgoing.pop_front() {
            self.last_op = request["op"].as_str().unwrap_or_default().to_owned();
            return Some(request);
        }
        if self.job.is_some()
            && self.status["state"] == "running"
            && self.last_poll.is_none_or(|at| at.elapsed() >= POLL)
        {
            self.last_poll = Some(Instant::now());
            self.last_op = "animation.motionsync.status".into();
            return Some(json!({"op": "animation.motionsync.status", "job": self.job.unwrap()}));
        }
        None
    }

    pub fn running(&self) -> bool {
        self.job.is_some() && self.status["state"] == "running"
    }

    pub fn report(&self) -> &str {
        &self.report
    }

    pub fn apply_reply(&mut self, reply: &Value) -> bool {
        if reply["succeeded"] != true {
            self.report = reply["message"].as_str().unwrap_or("the request failed").to_owned();
            if self.last_op == "animation.motionsync.status" {
                self.job = None;
                self.last_poll = None;
                self.status = json!({"state": "failed", "error": self.report.clone()});
            }
            return false;
        }
        let result = &reply["result"];
        match self.last_op.as_str() {
            "animation.motionsync.bake" => {
                self.report = format!(
                    "Baked speech keys: {} keys on {} frames ({}..={})",
                    result["keys"], result["frames"], result["from"], result["to"]
                );
                true
            }
            _ => {
                if result.get("job").is_some() {
                    if self.last_op == "animation.motionsync.start" {
                        self.job = result["job"].as_u64();
                        self.last_poll = None;
                    }
                    if result["state"] != "running" {
                        self.last_poll = None;
                    }
                    self.status = result.clone();
                    match result["state"].as_str() {
                        Some("finished") => {
                            let cues = result["cues"].as_array().map_or(0, Vec::len);
                            self.report = format!(
                                "Analysis finished: {} mouth cues over {} seconds",
                                cues, result["duration"]
                            );
                        }
                        Some("failed") => {
                            self.report = format!(
                                "Analysis failed: {}",
                                result["error"].as_str().unwrap_or("unknown error")
                            );
                        }
                        Some("cancelled") => self.report = "Analysis cancelled".into(),
                        _ => self.report = "Analyzing...".into(),
                    }
                }
                false
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Value, name: &str) {
        let clip = &snapshot["model"]["animations"][name];
        let tracks = clip["audio"].as_array().map(Vec::as_slice).unwrap_or_default();
        ui.label("Analyze a clip's WAV track with the Rhubarb analyzer and bake mouth-shape keys.");
        if tracks.is_empty() {
            ui.label("This clip has no audio track; add a WAV first (animation.audio.add).");
        } else {
            self.audio_index = self.audio_index.min(tracks.len() - 1);
            // Submenus, since a combo box's popup would close the menu the panel sits in.
            ui.menu_button(format!("Audio track {}", self.audio_index + 1), |ui| {
                for (index, track) in tracks.iter().enumerate() {
                    let path = track["path"].as_str().unwrap_or("?");
                    let file = path.rsplit('/').next().unwrap_or(path);
                    let muted = if track["mute"].as_bool() == Some(true) { " (muted)" } else { "" };
                    ui.selectable_value(&mut self.audio_index, index, format!("{}: {}{}", index + 1, file, muted));
                }
            });
            let track = &tracks[self.audio_index];
            ui.menu_button(format!("Recognizer {}", self.recognizer), |ui| {
                ui.selectable_value(&mut self.recognizer, "phonetic".into(), "phonetic (non-English, less precise)");
                ui.selectable_value(&mut self.recognizer, "pocketSphinx".into(), "pocketSphinx (English)");
            });
            let analyzing = self.running();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!analyzing, egui::Button::new("Analyze WAV"))
                    .on_hover_text("Run the analyzer on the track's WAV; the document does not change")
                    .clicked()
                {
                    self.job = None;
                    self.status = Value::Null;
                    self.outgoing.push_back(json!({
                        "op": "animation.motionsync.start",
                        "path": track["path"].as_str().unwrap_or_default(),
                        "recognizer": self.recognizer,
                    }));
                }
                if analyzing && ui.button("Cancel analysis").clicked() {
                    if let Some(job) = self.job {
                        self.outgoing.push_back(json!({"op": "animation.motionsync.cancel", "job": job}));
                    }
                }
            });
            match self.status["state"].as_str() {
                Some("running") => {
                    ui.label("Analyzing...");
                    ui.ctx().request_repaint_after(POLL);
                }
                Some("failed") => {
                    ui.label(format!("Analysis failed: {}", self.status["error"].as_str().unwrap_or("unknown error")));
                }
                Some("finished") => {
                    let cues = self.status["cues"].as_array().map_or(0, Vec::len);
                    let duration = &self.status["duration"];
                    ui.label(format!("Analysis ready: {cues} cues over {duration} seconds"));
                }
                Some("cancelled") => {
                    ui.label("Analysis cancelled.");
                }
                _ => {}
            }
        }

        let parameters = leaf_parameters(snapshot);
        ui.separator();
        ui.label("Shape mapping: the value each Rhubarb mouth shape (A-H/X, not CRI visemes) keys on a parameter axis.");
        let mut remove = None;
        for (row_index, row) in self.mappings.iter_mut().enumerate() {
            egui::CollapsingHeader::new(format!("Mapping {}", row_index + 1)).default_open(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let selected = row
                        .parameter
                        .and_then(|id| parameters.iter().find(|param| param.id == id))
                        .map_or("Choose".to_owned(), |param| param.name.clone());
                    ui.menu_button(format!("Mapping {} parameter: {}", row_index + 1, selected), |ui| {
                        for param in &parameters {
                            if ui.selectable_value(&mut row.parameter, Some(param.id), &param.name).changed() {
                                row.axis = 0;
                                row.values = [param.default[0]; 9];
                            }
                        }
                    });
                    let axes = row
                        .parameter
                        .and_then(|id| parameters.iter().find(|param| param.id == id))
                        .map_or(1, |param| param.axes);
                    row.axis = row.axis.min(axes - 1);
                    ui.menu_button(format!("Mapping {} axis: {}", row_index + 1, row.axis), |ui| {
                        for axis in 0..axes {
                            ui.selectable_value(&mut row.axis, axis, format!("{axis}"));
                        }
                    });
                    if let Some(param) = row.parameter.and_then(|id| parameters.iter().find(|p| p.id == id)) {
                        if ui
                            .button(format!("Openness preset {}", row_index + 1))
                            .on_hover_text("Fill closed-to-wide values over the axis range; editable")
                            .clicked()
                        {
                            let (min, max) = (param.min[row.axis], param.max[row.axis]);
                            for (slot, value) in row.values.iter_mut().enumerate() {
                                *value = min + (max - min) * OPENNESS[slot];
                            }
                        }
                    }
                    if ui.button(format!("Remove mapping {}", row_index + 1)).clicked() {
                        remove = Some(row_index);
                    }
                });
                egui::Grid::new(format!("motionsync-shapes-{row_index}")).show(ui, |ui| {
                    for (slot, shape) in SHAPES.iter().enumerate() {
                        let label = ui.label(format!("Shape {shape}"));
                        ui.add(egui::DragValue::new(&mut row.values[slot]).speed(0.05)).labelled_by(label.id);
                        if slot % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            });
        }
        if let Some(index) = remove {
            self.mappings.remove(index);
        }
        let more = !parameters.is_empty() && self.mappings.len() < 32;
        if ui.add_enabled(more, egui::Button::new("Add mapping")).clicked() {
            self.mappings.push(MappingRow::default());
        }

        ui.separator();
        ui.checkbox(&mut self.custom_range, "Custom frame range")
            .on_hover_text("Default: the whole recording, from the audio track's frame");
        if self.custom_range {
            ui.horizontal(|ui| {
                let label = ui.label("From frame");
                ui.add(egui::DragValue::new(&mut self.from).range(0..=1_000_000)).labelled_by(label.id);
                let label = ui.label("To frame");
                ui.add(egui::DragValue::new(&mut self.to).range(0..=1_000_000)).labelled_by(label.id);
            });
        }
        ui.checkbox(&mut self.extend, "Extend clip to reach the last frame")
            .on_hover_text("Lengthen the clip when the bake range runs past its end");
        let mut taken = std::collections::BTreeSet::new();
        let complete = !self.mappings.is_empty()
            && self.mappings.iter().all(|row| {
                let axis_ok = row
                    .parameter
                    .and_then(|id| parameters.iter().find(|param| param.id == id))
                    .is_some_and(|param| row.axis < param.axes);
                axis_ok && taken.insert((row.parameter.unwrap(), row.axis))
            });
        let ready =
            self.status["state"] == "finished" && self.job.is_some() && complete;
        if ui
            .add_enabled(ready, egui::Button::new("Bake speech keys"))
            .on_hover_text("Key the analyzed mouth shapes on the mapped axes in one undo step")
            .clicked()
        {
            let mappings: Vec<Value> = self
                .mappings
                .iter()
                .map(|row| {
                    let mut shapes = Map::new();
                    for (slot, shape) in SHAPES.iter().enumerate() {
                        shapes.insert((*shape).to_owned(), json!(row.values[slot]));
                    }
                    json!({"parameter": row.parameter.unwrap(), "axis": row.axis, "shapes": shapes})
                })
                .collect();
            let mut request = json!({
                "op": "animation.motionsync.bake",
                "name": name,
                "job": self.job.unwrap(),
                "audio_index": self.audio_index,
                "mappings": mappings,
                "extend": self.extend,
            });
            if self.custom_range {
                request["from"] = json!(self.from);
                request["to"] = json!(self.to);
            }
            self.outgoing.push_back(request);
        }
        if !self.report.is_empty() {
            ui.label(&self.report);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::editor_test_support::Harness;
    use egui::accesskit::Role;
    use soul2d_core::Session;
    use std::os::unix::fs::PermissionsExt;

    fn stub(dir: &std::path::Path) -> String {
        let path = dir.join("fake-rhubarb");
        std::fs::write(
            &path,
            "#!/bin/sh\nfor last in \"$@\"; do :; done\ncat <<JSON\n\
             {\"metadata\":{\"soundFile\":\"$last\",\"duration\":0.5},\"mouthCues\":[\
             {\"start\":0.0,\"end\":0.125,\"value\":\"X\"},\
             {\"start\":0.125,\"end\":0.25,\"value\":\"D\"},\
             {\"start\":0.25,\"end\":0.375,\"value\":\"F\"},\
             {\"start\":0.375,\"end\":0.5,\"value\":\"X\"}]}\nJSON\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn draw<'a>(panel: &'a mut MotionSyncPanel, session: &'a mut Session) -> impl FnMut(&egui::Context) + 'a {
        move |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| panel.show(ui, &session.snapshot(), "Talk"));
            while let Some(request) = panel.take_request() {
                let reply = session.handle_request(&request);
                panel.apply_reply(&reply);
            }
        }
    }

    fn settle(panel: &mut MotionSyncPanel, session: &mut Session, harness: &mut Harness) {
        for _ in 0..50 {
            harness.frame(Vec::new(), draw(panel, session));
            if panel.status["state"] != "running" {
                return;
            }
            std::thread::sleep(POLL + Duration::from_millis(30));
        }
        panic!("the analysis did not settle");
    }

    #[test]
    fn bake_speech_keys_from_the_panel_in_one_undo() {
        let dir = crate::scratch::scratch(std::env::temp_dir().join(format!(
            "soul2d-motionsync-ui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("voice.wav");
        std::fs::write(&wav, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
        let model = json!({
            "meta": {"name": "", "version": "1.0-alpha"},
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": {"trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0]},
                "lockToRoot": false, "pinToMesh": false, "children": []},
            "param": [
                {"uuid": 10, "name": "Mouth", "is_vec2": false, "min": [0.0,0.0], "max": [1.0,0.0],
                 "defaults": [0.0,0.0], "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive",
                 "bindings": []},
                {"uuid": 11, "name": "Jaw", "is_vec2": true, "min": [-1.0,-1.0], "max": [1.0,1.0],
                 "defaults": [0.0,0.0], "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive",
                 "bindings": []},
            ],
            "animations": {"Talk": {"length": 8, "timestep": 0.125, "additive": false,
                "animationWeight": 1.0, "leadIn": -1, "leadOut": -1, "lanes": [],
                "audio": [{"path": wav.to_string_lossy(), "frame": 2}]}},
        });
        let file = dir.join("fixture.inx");
        std::fs::write(&file, soul2d_core::model_import::inx_bytes(&model, &[]).unwrap()).unwrap();
        let mut session = Session::new();
        assert_eq!(session.handle_request(&json!({"op": "open", "path": file}))["succeeded"], true);
        session.set_motion_sync_analyzer(&stub(&dir));

        let mut panel = MotionSyncPanel::default();
        let mut harness = Harness::new();
        harness.screen = egui::vec2(1200.0, 1400.0);
        harness.click(Role::Button, "Analyze WAV", draw(&mut panel, &mut session));
        settle(&mut panel, &mut session, &mut harness);
        assert_eq!(panel.status["state"], "finished");
        assert_eq!(panel.status["cues"].as_array().unwrap().len(), 4);

        harness.click(Role::Button, "Add mapping", draw(&mut panel, &mut session));
        harness.click(Role::Button, "Mapping 1 parameter:", draw(&mut panel, &mut session));
        harness.click(Role::Button, "Mouth", draw(&mut panel, &mut session));
        harness.click(Role::Button, "Openness preset 1", draw(&mut panel, &mut session));
        assert_eq!(panel.mappings[0].values[3], 1.0, "D is the wide-open slot of the preset");

        harness.click(Role::Button, "Bake speech keys", draw(&mut panel, &mut session));
        let clip = |session: &Session| session.snapshot()["model"]["animations"]["Talk"].clone();
        let lane = clip(&session)["lanes"].as_array().unwrap().iter()
            .find(|lane| lane["uuid"] == 10 && lane["target"] == 0)
            .cloned()
            .expect("the mapped lane exists");
        assert_eq!(lane["interpolation"], "Linear");
        assert_eq!(lane["merge_mode"], "Forced");
        let keys = lane["keyframes"].as_array().unwrap();
        let frames: Vec<i64> = keys.iter().map(|key| key["frame"].as_i64().unwrap()).collect();
        let values: Vec<f64> = keys.iter().map(|key| key["value"].as_f64().unwrap()).collect();
        assert_eq!(frames, [2, 3, 4, 5, 6]);
        assert_eq!(values, [0.0, 1.0, 0.25, 0.0, 0.0]);
        assert!(panel.report().starts_with("Baked speech keys: 5 keys"), "{}", panel.report());

        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert!(clip(&session)["lanes"].as_array().is_none_or(Vec::is_empty), "one undo removed the lane");
        assert_eq!(session.handle_request(&json!({"op": "redo"}))["succeeded"], true);
        assert_eq!(clip(&session)["lanes"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
