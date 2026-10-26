//! The "Depth Draw" section of the depth tools: D's layer composition (`viewport/depth/draw`) as session requests.
//!
//! The editor never holds pixels. The panel keeps a pixel-less copy of the session's manifest (`depth.draw.manifest.get`)
//! and the last diagnostics, refreshes both when the session reports a new `depthDraw.revision` (or the document
//! changes under it), and turns each widget change into one session request. The request builders are plain
//! functions with unit tests; the egui code only calls them.

use crate::editor_mesh_tools::V2;
use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::depth_draw as dd;

type Key = (u64, u64);

/// Which gap `depth.draw.fit_z` fits the selected layer into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitGap {
    Adjacent,
    Target,
    Layers,
    Values,
}

impl FitGap {
    const ALL: [FitGap; 4] = [FitGap::Adjacent, FitGap::Target, FitGap::Layers, FitGap::Values];

    fn label(self) -> &'static str {
        match self {
            FitGap::Adjacent => "Between neighbours",
            FitGap::Target => "Target range",
            FitGap::Layers => "Between layers",
            FitGap::Values => "Explicit values",
        }
    }

    fn key(self) -> &'static str {
        match self {
            FitGap::Adjacent => "adjacent",
            FitGap::Target => "target",
            FitGap::Layers => "layers",
            FitGap::Values => "values",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FitDraft {
    pub gap: FitGap,
    pub margin: f32,
    pub back_layer: String,
    pub front_layer: String,
    pub back: f32,
    pub front: f32,
}

impl Default for FitDraft {
    fn default() -> Self {
        Self { gap: FitGap::Adjacent, margin: 0.0, back_layer: String::new(), front_layer: String::new(), back: -1.0, front: 1.0 }
    }
}

// ------------------------------------------------------------------------------------ requests

pub fn load_png_request(path: &str, id: &str) -> Result<Value, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Enter the path of a PNG file".into());
    }
    let mut request = json!({"op": "depth.draw.load_png", "path": path});
    if !id.trim().is_empty() {
        request["id"] = json!(id.trim());
    }
    Ok(request)
}

pub fn export_request(dir: &str, overwrite: bool) -> Result<Value, String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Err("Enter the export directory".into());
    }
    Ok(json!({"op": "depth.draw.export_png", "dir": dir, "overwrite": overwrite}))
}

pub fn layer_request(layer: &str, key: &str, value: Value) -> Value {
    json!({"op": "depth.draw.layer.update", "layer": layer, key: value})
}

pub fn binding_request(layer: &str, target: u32, key: &str, value: Value) -> Value {
    json!({"op": "depth.draw.binding.update", "layer": layer, "target": target, key: value})
}

pub fn display_request(key: &str, on: bool) -> Value {
    json!({"op": "depth.draw.display", key: on})
}

pub fn select_request(layer: &str) -> Value {
    json!({"op": "depth.draw.select", "layer": layer})
}

pub fn auto_bind_request() -> Value {
    json!({"op": "depth.draw.auto_bind"})
}

pub fn compose_request(target: u32) -> Value {
    json!({"op": "depth.draw.compose", "target": target})
}

pub fn apply_request(target: u32) -> Value {
    json!({"op": "depth.draw.apply", "target": target})
}

pub fn gap_fill_request(layer: &str) -> Value {
    json!({"op": "depth.draw.cleanup", "layer": layer, "kind": "gap_fill"})
}

pub fn contour_request(layer: &str, thickness: i32) -> Value {
    json!({"op": "depth.draw.cleanup", "layer": layer, "kind": "contour_repair", "thickness": thickness})
}

pub fn fit_request(layer: &str, target: u32, fit: &FitDraft) -> Result<Value, String> {
    let mut request = json!({
        "op": "depth.draw.fit_z", "layer": layer, "target": target, "gap": fit.gap.key(),
        "margin": f64::from(fit.margin),
    });
    match fit.gap {
        FitGap::Layers => {
            if fit.back_layer.is_empty() || fit.front_layer.is_empty() {
                return Err("Choose the back and the front layer".into());
            }
            request["backLayer"] = json!(fit.back_layer);
            request["frontLayer"] = json!(fit.front_layer);
        }
        FitGap::Values => {
            request["back"] = json!(f64::from(fit.back));
            request["front"] = json!(f64::from(fit.front));
        }
        FitGap::Adjacent | FitGap::Target => {}
    }
    Ok(request)
}

/// What to read after the session's DepthDraw state or the document changed.
pub fn refresh_requests(target: u32) -> Vec<Value> {
    vec![json!({"op": "depth.draw.manifest.get"}), json!({"op": "depth.draw.diagnostics", "target": target})]
}

/// The outline of each visible, enabled layer in world coordinates (D `documentToModelPoint`: the document
/// centre is the world origin), as `(layer id, corners, selected)`.
pub fn layer_planes(draw: &dd::Session) -> Vec<(String, [V2; 4], bool)> {
    let (half_w, half_h) = (f64::from(draw.document_width.max(1)) / 2.0, f64::from(draw.document_height.max(1)) / 2.0);
    draw.layers
        .iter()
        .filter(|layer| layer.visible && layer.enabled)
        .map(|layer| {
            let (min, max) = dd::layer_document_bounds(layer);
            let corner = |x: f32, y: f32| [f64::from(x) - half_w, f64::from(y) - half_h];
            (
                layer.id.clone(),
                [corner(min[0], min[1]), corner(max[0], min[1]), corner(max[0], max[1]), corner(min[0], max[1])],
                layer.id == draw.selected_layer_id,
            )
        })
        .collect()
}

// -------------------------------------------------------------------------------------- panel

pub struct DrawPanel {
    pub png_path: String,
    pub png_id: String,
    pub export_dir: String,
    pub overwrite: bool,
    pub thickness: i32,
    pub fit: FitDraft,
    /// The session's DepthDraw state without pixels, as of the last `depth.draw.manifest.get`.
    draw: dd::Session,
    /// `(has depth pixels, has normal coverage)` for each layer id.
    pixels: Vec<(String, bool, bool)>,
    diagnostics: Option<Value>,
    compose: Option<(u32, Value)>,
    /// The session key the cache was last requested for; a different key means the cache is stale.
    requested: Option<Key>,
    current: Key,
    pub status: String,
    pub failed: bool,
}

impl Default for DrawPanel {
    fn default() -> Self {
        Self {
            png_path: String::new(),
            png_id: String::new(),
            export_dir: String::new(),
            overwrite: false,
            thickness: 2,
            fit: FitDraft::default(),
            draw: dd::Session::new(),
            pixels: Vec::new(),
            diagnostics: None,
            compose: None,
            requested: None,
            current: (0, 0),
            status: String::new(),
            failed: false,
        }
    }
}

impl DrawPanel {
    /// The session's `(depthDraw.revision, document revision)` of this frame. A change makes the cache stale.
    pub fn observe(&mut self, key: Key) {
        if key != self.current {
            self.compose = None;
        }
        self.current = key;
    }

    pub fn display(&self) -> dd::DisplayOptions {
        self.draw.display
    }

    pub fn planes(&self) -> Vec<(String, [V2; 4], bool)> {
        layer_planes(&self.draw)
    }

    /// The depths and winning layer ids of the last Compose Preview of `target`, while they are current.
    pub fn composed(&self, target: u32) -> Option<(Vec<f32>, Vec<String>)> {
        let (id, result) = self.compose.as_ref()?;
        if *id != target {
            return None;
        }
        let depths = result["depths"].as_array()?.iter().map(|d| d.as_f64().unwrap_or(0.0) as f32).collect();
        let winners = result["winningLayerIds"].as_array()?.iter().map(|w| w.as_str().unwrap_or("").to_owned()).collect();
        Some((depths, winners))
    }

    fn selected_layer(&self) -> Option<&dd::Layer> {
        self.draw.layer_by_id(&self.draw.selected_layer_id)
    }

    fn has_pixels(&self, id: &str) -> bool {
        self.pixels.iter().any(|(layer, depth, _)| layer == id && *depth)
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.failed = true;
    }

    /// Takes the session's answer to a request [`Self::show`] returned.
    pub fn deliver(&mut self, request: &Value, response: &Value) {
        let op = request["op"].as_str().unwrap_or("");
        if !op.starts_with("depth.draw.") {
            return;
        }
        if response["succeeded"] != true {
            self.fail(response["message"].as_str().unwrap_or("the request failed"));
            return;
        }
        let result = &response["result"];
        match op {
            "depth.draw.manifest.get" => {
                if let Ok(draw) = dd::session_from_manifest(&result["manifest"]) {
                    self.draw = draw;
                }
                self.pixels = result["layers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|l| {
                        (
                            l["id"].as_str().unwrap_or("").to_owned(),
                            l["hasDepthPixels"] == true,
                            l["hasNormalCoverage"] == true,
                        )
                    })
                    .collect();
                return;
            }
            "depth.draw.diagnostics" => {
                self.diagnostics = Some(result.clone());
                return;
            }
            "depth.draw.compose" => {
                let target = request["target"].as_u64().and_then(|t| u32::try_from(t).ok()).unwrap_or(0);
                self.status = format!(
                    "Composed {} vertices: {} sampled, {} missing, depth {:.3} to {:.3}; Apply would change {} (layers used {}, skipped {})",
                    result["vertexCount"],
                    result["sampledVertices"],
                    result["missingVertices"],
                    result["minDepth"].as_f64().unwrap_or(0.0),
                    result["maxDepth"].as_f64().unwrap_or(0.0),
                    result["apply"]["changedVertices"],
                    result["apply"]["layersApplied"],
                    result["apply"]["layersSkipped"],
                );
                self.compose = Some((target, result.clone()));
                self.failed = false;
                return;
            }
            "depth.draw.layer.update" => {
                // Keep the widgets in step with the answer; the revision change refreshes the rest.
                if let Ok(next) = dd::session_from_manifest(&json!({"layers": [result["layer"].clone()]})) {
                    if let (Some(new), Some(old)) = (next.layers.first(), self.draw.layers.iter_mut().find(|l| l.id == result["layer"]["id"])) {
                        *old = new.clone();
                    }
                }
            }
            "depth.draw.binding.update" => {
                if let Ok(next) = dd::session_from_manifest(&json!({"bindings": [result["binding"].clone()]})) {
                    if let Some(new) = next.bindings.first() {
                        match self.draw.bindings.iter_mut().find(|b| b.layer_id == new.layer_id && b.target_grid_uuid == new.target_grid_uuid) {
                            Some(old) => *old = new.clone(),
                            None => self.draw.bindings.push(new.clone()),
                        }
                    }
                }
            }
            _ => {}
        }
        self.failed = false;
        self.status = match op {
            "depth.draw.load_png" => format!("Loaded layer {}", result["layer"]["id"].as_str().unwrap_or("")),
            "depth.draw.auto_bind" => format!("Auto bind matched {} layers", result["matched"]),
            "depth.draw.apply" => format!(
                "Applied DepthDraw: {} vertices changed, {} layers used",
                result["apply"]["changedVertices"], result["apply"]["layersApplied"]
            ),
            "depth.draw.cleanup" if result["kind"] == "gap_fill" => format!(
                "Gap fill {}: {} detected, {} filled, {} left",
                if result["changed"] == true { "changed the layer" } else { "changed nothing" },
                result["detected"]["total"], result["filled"], result["remaining"]
            ),
            "depth.draw.cleanup" => format!(
                "Contour repair {}: {} contour pixels, {} filled",
                if result["changed"] == true { "changed the layer" } else { "changed nothing" },
                result["contourPixels"], result["filledPixels"]
            ),
            "depth.draw.fit_z" if result["succeeded"] == true => format!(
                "Fit Z: scale {:.3}, offset {:.3}{}",
                result["zScale"].as_f64().unwrap_or(0.0),
                result["zOffset"].as_f64().unwrap_or(0.0),
                if result["applied"] == true { "" } else { " (not applied)" }
            ),
            "depth.draw.fit_z" => {
                self.failed = true;
                format!("Fit Z failed: {}", result["error"].as_str().unwrap_or(""))
            }
            "depth.draw.export_png" => format!("Exported {} PNGs and {}", result["exportedLayers"], result["manifest"].as_str().unwrap_or("")),
            _ => std::mem::take(&mut self.status),
        };
    }

    /// Draws the section for the grid `target` and returns the session requests to send; give each answer to
    /// [`Self::deliver`].
    pub fn show(&mut self, ui: &mut egui::Ui, target: u32) -> Vec<Value> {
        let mut out = Vec::new();
        if self.requested != Some(self.current) {
            self.requested = Some(self.current);
            out.extend(refresh_requests(target));
        }
        ui.horizontal_wrapped(|ui| {
            let caption = ui.label("PNG path");
            ui.add(egui::TextEdit::singleline(&mut self.png_path).desired_width(260.0)).labelled_by(caption.id);
            let caption = ui.label("Layer id");
            ui.add(egui::TextEdit::singleline(&mut self.png_id).desired_width(90.0))
                .labelled_by(caption.id)
                .on_hover_text("Optional. The file name by default; a saved layer of that id without pixels gets this PNG");
            if ui.button("Load PNG").on_hover_text("Load a depth PNG as a layer (its RGBA is the depth)").clicked() {
                match load_png_request(&self.png_path, &self.png_id) {
                    Ok(request) => out.push(request),
                    Err(message) => self.fail(message),
                }
            }
        });
        let selected = self.selected_layer().map(|l| l.id.clone());
        let usable = selected.as_deref().is_some_and(|id| self.has_pixels(id));
        let reason = "Select a layer that has pixels first";
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Auto Bind Layers")
                .on_hover_text("Bind each layer to the Part or deformer that matches its layer path or name")
                .clicked()
            {
                out.push(auto_bind_request());
            }
            if ui.button("Compose Preview").on_hover_text("Compose the bound layers onto this grid and show the result").clicked() {
                out.push(compose_request(target));
            }
            if ui.button("Apply DepthDraw").on_hover_text("Write the composed depths to this grid as one undo step").clicked() {
                out.push(apply_request(target));
            }
        });
        ui.horizontal_wrapped(|ui| {
            let layer = selected.clone().unwrap_or_default();
            if ui
                .add_enabled(usable, egui::Button::new("Fill Alpha-Depth Gaps"))
                .on_hover_text("Fill holes, dips and cliffs under the layer's alpha from their neighbours")
                .on_disabled_hover_text(reason)
                .clicked()
            {
                out.push(gap_fill_request(&layer));
            }
            let caption = ui.label("Contour thickness");
            ui.add(egui::DragValue::new(&mut self.thickness).range(1..=dd::MAX_CONTOUR_THICKNESS).speed(0.1))
                .labelled_by(caption.id)
                .on_hover_text("Pixels of the layer border that are re-estimated from the inside");
            if ui
                .add_enabled(usable, egui::Button::new("Repair Contour"))
                .on_hover_text("Re-estimate the border band of the layer's mask from the pixels inside it")
                .on_disabled_hover_text(reason)
                .clicked()
            {
                out.push(contour_request(&layer, self.thickness));
            }
        });
        let ids: Vec<String> = self.draw.layers.iter().map(|l| l.id.clone()).collect();
        ui.horizontal_wrapped(|ui| {
            let layer = selected.clone().unwrap_or_default();
            let gap = self.fit.gap;
            let combo = egui::ComboBox::from_id_salt("depth-draw-fit-gap").selected_text(gap.label()).show_ui(ui, |ui| {
                for choice in FitGap::ALL {
                    ui.selectable_value(&mut self.fit.gap, choice, choice.label());
                }
            });
            let caption = ui.label("Fit gap");
            combo.response.labelled_by(caption.id);
            labelled_drag(ui, "Fit margin", &mut self.fit.margin, 0.0..=2.0, 0.01, "Space kept on both sides of the gap");
            match self.fit.gap {
                FitGap::Layers => {
                    layer_combo(ui, "Back layer", &mut self.fit.back_layer, &ids);
                    layer_combo(ui, "Front layer", &mut self.fit.front_layer, &ids);
                }
                FitGap::Values => {
                    labelled_drag(ui, "Gap back", &mut self.fit.back, -4.0..=4.0, 0.01, "Depth the gap starts at");
                    labelled_drag(ui, "Gap front", &mut self.fit.front, -4.0..=4.0, 0.01, "Depth the gap ends at");
                }
                FitGap::Adjacent | FitGap::Target => {}
            }
            if ui
                .add_enabled(usable, egui::Button::new("Fit Z to Gap"))
                .on_hover_text("Scale and shift the layer's depth so its range fits the gap")
                .on_disabled_hover_text(reason)
                .clicked()
            {
                match fit_request(&layer, target, &self.fit) {
                    Ok(request) => out.push(request),
                    Err(message) => self.fail(message),
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            let caption = ui.label("Export directory");
            ui.add(egui::TextEdit::singleline(&mut self.export_dir).desired_width(260.0)).labelled_by(caption.id);
            ui.checkbox(&mut self.overwrite, "Overwrite existing files");
            if ui.button("Export Depth PNGs").on_hover_text("Write one grey PNG per layer plus a manifest").clicked() {
                match export_request(&self.export_dir, self.overwrite) {
                    Ok(request) => out.push(request),
                    Err(message) => self.fail(message),
                }
            }
        });
        self.display_ui(ui, &mut out);
        self.layers_ui(ui, target, &mut out);
        if !self.status.is_empty() {
            let color = if self.failed { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN };
            ui.colored_label(color, self.status.clone());
        }
        self.diagnostics_ui(ui);
        out
    }

    fn display_ui(&mut self, ui: &mut egui::Ui, out: &mut Vec<Value>) {
        let d = self.draw.display;
        let boxes: [(&str, &str, bool, &str); 9] = [
            ("Composite", "showComposite", d.show_composite, "Show the composed depths over the grid after Compose Preview"),
            ("Layer Planes", "showLayerPlanes", d.show_layer_planes, "Outline each layer in the viewport"),
            ("Depth Ranges", "showDepthRanges", d.show_depth_ranges, "Keep the depth range of each layer in the table"),
            ("Missing Vertices", "showMissingVertices", d.show_missing_vertices, "Mark grid vertices no layer covers"),
            ("Winning Layer", "showWinningLayer", d.show_winning_layer, "Colour each vertex by the layer that won it"),
            ("Normal Image", "showNormalImage", d.show_normal_image, "Stored; the normal image is not drawn in this editor"),
            ("Raw Depth", "showRawDepth", d.show_raw_depth, "Stored; the raw depth image is not drawn in this editor"),
            ("Coverage", "showCoverage", d.show_coverage, "Stored; the coverage samples are not drawn in this editor"),
            ("GPU Preview", "useGpuPreview", d.use_gpu_preview, "Stored; custom-radius median falls back to 3x3, composition stays on the CPU"),
        ];
        ui.horizontal_wrapped(|ui| {
            for (label, key, value, hover) in boxes {
                let mut on = value;
                if ui.checkbox(&mut on, label).on_hover_text(hover).changed() {
                    out.push(display_request(key, on));
                }
            }
        });
    }

    fn layers_ui(&mut self, ui: &mut egui::Ui, target: u32, out: &mut Vec<Value>) {
        if self.draw.layers.is_empty() {
            ui.label("No layers. Load a PNG to start.");
            return;
        }
        let layers = self.draw.layers.clone();
        let bindings = self.draw.bindings.clone();
        let selected = self.draw.selected_layer_id.clone();
        let target64 = u64::from(target);
        for layer in &layers {
            let id = layer.id.as_str();
            let title = if layer.display_name.is_empty() { id.to_owned() } else { layer.display_name.clone() };
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(selected == layer.id, title.as_str()).on_hover_text("Select this layer").clicked() {
                    out.push(select_request(id));
                }
                let (mut visible, mut enabled) = (layer.visible, layer.enabled);
                if ui.checkbox(&mut visible, format!("Visible {title}")).changed() {
                    out.push(layer_request(id, "visible", json!(visible)));
                }
                if ui.checkbox(&mut enabled, format!("Enabled {title}")).changed() {
                    out.push(layer_request(id, "enabled", json!(enabled)));
                }
                if !self.has_pixels(id) {
                    ui.label("(no pixels: load the PNG again)");
                }
            });
            egui::CollapsingHeader::new(format!("{title} settings"))
                .id_salt(("depth-draw-layer", id))
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let f = |v: f32| json!(f64::from(v));
                        let mut v = layer.z_scale;
                        if labelled_drag(ui, "Z Scale", &mut v, -8.0..=8.0, 0.01, "Multiplies the sampled depth") {
                            out.push(layer_request(id, "zScale", f(v)));
                        }
                        let mut v = layer.z_offset;
                        if labelled_drag(ui, "Z Offset", &mut v, -8.0..=8.0, 0.01, "Added to the scaled depth") {
                            out.push(layer_request(id, "zOffset", f(v)));
                        }
                        let mut v = layer.back_depth;
                        if labelled_drag(ui, "Back Depth", &mut v, -8.0..=8.0, 0.01, "Depth of the darkest pixel") {
                            out.push(layer_request(id, "backDepth", f(v)));
                        }
                        let mut v = layer.front_depth;
                        if labelled_drag(ui, "Front Depth", &mut v, -8.0..=8.0, 0.01, "Depth of the brightest pixel") {
                            out.push(layer_request(id, "frontDepth", f(v)));
                        }
                        let mut invert = layer.invert;
                        if ui.checkbox(&mut invert, "Invert").on_hover_text("Bright pixels are far").changed() {
                            out.push(layer_request(id, "invert", json!(invert)));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let mut pair = layer.xy_offset;
                        if labelled_drag(ui, "Offset X", &mut pair[0], -4096.0..=4096.0, 1.0, "Move the layer in the document")
                            | labelled_drag(ui, "Offset Y", &mut pair[1], -4096.0..=4096.0, 1.0, "Move the layer in the document")
                        {
                            out.push(layer_request(id, "xyOffset", json!([f64::from(pair[0]), f64::from(pair[1])])));
                        }
                        let mut pair = layer.xy_scale;
                        if labelled_drag(ui, "Scale X", &mut pair[0], -16.0..=16.0, 0.01, "Stretch the layer in the document")
                            | labelled_drag(ui, "Scale Y", &mut pair[1], -16.0..=16.0, 0.01, "Stretch the layer in the document")
                        {
                            out.push(layer_request(id, "xyScale", json!([f64::from(pair[0]), f64::from(pair[1])])));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let mut channel = layer.channel;
                        let combo = egui::ComboBox::from_id_salt(("depth-draw-channel", id)).selected_text(channel.name()).show_ui(ui, |ui| {
                            for choice in dd::Channel::ALL {
                                ui.selectable_value(&mut channel, choice, choice.name());
                            }
                        });
                        let caption = ui.label("Channel");
                        combo.response.labelled_by(caption.id);
                        if channel != layer.channel {
                            out.push(layer_request(id, "channel", json!(channel.name())));
                        }
                        let mut convolution = layer.convolution;
                        let combo = egui::ComboBox::from_id_salt(("depth-draw-convolution", id))
                            .selected_text(convolution.name())
                            .show_ui(ui, |ui| {
                                for choice in dd::Convolution::all() {
                                    ui.selectable_value(&mut convolution, choice, choice.name());
                                }
                            });
                        let caption = ui.label("Convolution");
                        combo.response.labelled_by(caption.id);
                        if convolution != layer.convolution {
                            out.push(layer_request(id, "convolution", json!(convolution.name())));
                        }
                        let mut radius = layer.custom_radius;
                        let caption = ui.label("Custom Radius");
                        let changed = ui
                            .add(egui::DragValue::new(&mut radius).range(dd::MIN_CUSTOM_RADIUS..=dd::MAX_CUSTOM_RADIUS).speed(0.1))
                            .labelled_by(caption.id)
                            .changed();
                        if changed {
                            out.push(layer_request(id, "customRadius", json!(radius)));
                        }
                        let mut threshold = layer.alpha_threshold;
                        if labelled_drag(ui, "Alpha Threshold", &mut threshold, 0.0..=1.0, 0.005, "Pixels at or below this alpha are missing") {
                            out.push(layer_request(id, "alphaThreshold", json!(f64::from(threshold))));
                        }
                    });
                    let binding = bindings.iter().find(|b| b.layer_id == layer.id && b.target_grid_uuid == target64);
                    ui.horizontal_wrapped(|ui| {
                        let mut bound = binding.is_some_and(|b| b.enabled);
                        if ui.checkbox(&mut bound, "Bound to this grid").on_hover_text("Compose this layer onto the grid being edited").changed() {
                            out.push(binding_request(id, target, "enabled", json!(bound)));
                        }
                        let Some(binding) = binding else { return };
                        let mut order = binding.order;
                        let caption = ui.label("Order");
                        let changed = ui
                            .add(egui::DragValue::new(&mut order).speed(0.1))
                            .labelled_by(caption.id)
                            .on_hover_text("Layers are merged from the lowest order up")
                            .changed();
                        if changed {
                            out.push(binding_request(id, target, "order", json!(order)));
                        }
                        let mut policy = binding.merge_policy;
                        let combo = egui::ComboBox::from_id_salt(("depth-draw-merge", id)).selected_text(policy.name()).show_ui(ui, |ui| {
                            for choice in [
                                dd::MergePolicy::Replace,
                                dd::MergePolicy::Frontmost,
                                dd::MergePolicy::Backmost,
                                dd::MergePolicy::Add,
                                dd::MergePolicy::KeepExistingWhereMissing,
                            ] {
                                ui.selectable_value(&mut policy, choice, choice.name());
                            }
                        });
                        let caption = ui.label("Merge");
                        combo.response.labelled_by(caption.id);
                        if policy != binding.merge_policy {
                            out.push(binding_request(id, target, "mergePolicy", json!(policy.name())));
                        }
                        let mut alpha = binding.use_normal_layer_alpha;
                        if ui.checkbox(&mut alpha, "Use normal-layer alpha").changed() {
                            out.push(binding_request(id, target, "useNormalLayerAlpha", json!(alpha)));
                        }
                        let mut threshold = binding.coverage_threshold;
                        if labelled_drag(ui, "Coverage Threshold", &mut threshold, 0.0..=1.0, 0.005, "Minimum coverage alpha") {
                            out.push(binding_request(id, target, "coverageThreshold", json!(f64::from(threshold))));
                        }
                    });
                });
        }
    }

    fn diagnostics_ui(&self, ui: &mut egui::Ui) {
        let Some(diagnostics) = &self.diagnostics else { return };
        let rows = diagnostics["rows"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        let source = &diagnostics["source"];
        ui.label(format!(
            "Layers: {} usable of {}, {} without target, {} with several targets; bindings {} of {} enabled",
            source["usableDepthLayers"], source["totalLayers"], source["layersMissingTarget"],
            source["layersWithAmbiguousBindings"], source["enabledBindings"], source["totalBindings"]
        ));
        egui::Grid::new("depth-draw-diagnostics").striped(true).show(ui, |ui| {
            for header in ["Layer", "Target", "Merge", "Sampled", "Missing", "Won", "Min", "Max", "Warnings"] {
                ui.strong(header);
            }
            ui.end_row();
            for row in &rows {
                ui.label(row["displayName"].as_str().unwrap_or(""));
                ui.label(row["targetDisplayName"].as_str().unwrap_or(""));
                ui.label(row["mergePolicy"].as_str().unwrap_or(""));
                ui.label(row["sampledVertices"].to_string());
                ui.label(row["missingVertices"].to_string());
                ui.label(row["winningVertices"].to_string());
                let range = |key: &str| {
                    if row["hasDepthRange"] == true && self.draw.display.show_depth_ranges { format!("{:.3}", row[key].as_f64().unwrap_or(0.0)) } else { "-".to_owned() }
                };
                ui.label(range("minDepth"));
                ui.label(range("maxDepth"));
                ui.label(warning_text(&row["warnings"]));
                ui.end_row();
            }
        });
    }
}

fn warning_text(warnings: &Value) -> String {
    [
        ("missingDepthPixels", "no pixels"),
        ("missingTarget", "no target"),
        ("missingCoverage", "no coverage"),
        ("mostlyMissingSamples", "mostly missing"),
    ]
    .iter()
    .filter(|(key, _)| warnings[*key] == true)
    .map(|(_, text)| *text)
    .collect::<Vec<_>>()
    .join(", ")
}

fn labelled_drag(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, speed: f64, hover: &str) -> bool {
    let caption = ui.label(label);
    ui.add(egui::DragValue::new(value).range(range).speed(speed).fixed_decimals(2))
        .labelled_by(caption.id)
        .on_hover_text(hover)
        .changed()
}

fn layer_combo(ui: &mut egui::Ui, label: &str, value: &mut String, ids: &[String]) {
    let shown = if value.is_empty() { "(choose)".to_owned() } else { value.clone() };
    let combo = egui::ComboBox::from_id_salt(("depth-draw-layer-choice", label)).selected_text(shown).show_ui(ui, |ui| {
        for id in ids {
            ui.selectable_value(value, id.clone(), id.as_str());
        }
    });
    let caption = ui.label(label);
    combo.response.labelled_by(caption.id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_built_with_the_session_field_names() {
        assert_eq!(
            load_png_request("  /tmp/a.png ", " front ").unwrap(),
            json!({"op": "depth.draw.load_png", "path": "/tmp/a.png", "id": "front"})
        );
        assert_eq!(load_png_request("/tmp/a.png", "").unwrap(), json!({"op": "depth.draw.load_png", "path": "/tmp/a.png"}));
        assert_eq!(load_png_request("  ", "x").unwrap_err(), "Enter the path of a PNG file");
        assert_eq!(
            export_request(" /tmp/out ", true).unwrap(),
            json!({"op": "depth.draw.export_png", "dir": "/tmp/out", "overwrite": true})
        );
        assert_eq!(export_request("", false).unwrap_err(), "Enter the export directory");
        assert_eq!(layer_request("mid", "zScale", json!(0.5)), json!({"op": "depth.draw.layer.update", "layer": "mid", "zScale": 0.5}));
        assert_eq!(
            binding_request("mid", 5, "mergePolicy", json!("Add")),
            json!({"op": "depth.draw.binding.update", "layer": "mid", "target": 5, "mergePolicy": "Add"})
        );
        assert_eq!(display_request("showRawDepth", true), json!({"op": "depth.draw.display", "showRawDepth": true}));
        assert_eq!(select_request("mid"), json!({"op": "depth.draw.select", "layer": "mid"}));
        assert_eq!(compose_request(5), json!({"op": "depth.draw.compose", "target": 5}));
        assert_eq!(apply_request(5), json!({"op": "depth.draw.apply", "target": 5}));
        assert_eq!(auto_bind_request(), json!({"op": "depth.draw.auto_bind"}));
        assert_eq!(gap_fill_request("mid"), json!({"op": "depth.draw.cleanup", "layer": "mid", "kind": "gap_fill"}));
        assert_eq!(
            contour_request("mid", 3),
            json!({"op": "depth.draw.cleanup", "layer": "mid", "kind": "contour_repair", "thickness": 3})
        );
        assert_eq!(
            refresh_requests(5),
            [json!({"op": "depth.draw.manifest.get"}), json!({"op": "depth.draw.diagnostics", "target": 5})]
        );
    }

    #[test]
    fn fit_requests_carry_only_what_the_chosen_gap_needs() {
        let mut fit = FitDraft { margin: 0.5, ..FitDraft::default() };
        assert_eq!(
            fit_request("mid", 5, &fit).unwrap(),
            json!({"op": "depth.draw.fit_z", "layer": "mid", "target": 5, "gap": "adjacent", "margin": 0.5})
        );
        fit.gap = FitGap::Target;
        assert_eq!(fit_request("mid", 5, &fit).unwrap()["gap"], "target");
        fit.gap = FitGap::Values;
        fit.back = 0.0;
        fit.front = 2.0;
        assert_eq!(
            fit_request("mid", 5, &fit).unwrap(),
            json!({"op": "depth.draw.fit_z", "layer": "mid", "target": 5, "gap": "values", "margin": 0.5, "back": 0.0, "front": 2.0})
        );
        fit.gap = FitGap::Layers;
        assert_eq!(fit_request("mid", 5, &fit).unwrap_err(), "Choose the back and the front layer");
        fit.back_layer = "back".into();
        fit.front_layer = "front".into();
        let request = fit_request("mid", 5, &fit).unwrap();
        assert_eq!((request["backLayer"].clone(), request["frontLayer"].clone()), (json!("back"), json!("front")));
        assert!(request.get("back").is_none());
    }

    #[test]
    fn layer_planes_are_world_rectangles_around_the_document_centre() {
        let mut draw = dd::Session::new();
        draw.document_width = 10;
        draw.document_height = 8;
        draw.layers.push(dd::Layer {
            id: "a".into(),
            bounds: dd::Rect { left: 2, top: 1, width: 4, height: 2 },
            xy_offset: [1.0, 0.0],
            xy_scale: [1.0, 2.0],
            ..dd::Layer::default()
        });
        draw.layers.push(dd::Layer { id: "hidden".into(), visible: false, ..dd::Layer::default() });
        draw.selected_layer_id = "a".into();
        // Document rectangle (3,1)-(7,5), minus the centre (5,4).
        assert_eq!(
            layer_planes(&draw),
            vec![("a".to_owned(), [[-2.0, -3.0], [2.0, -3.0], [2.0, 1.0], [-2.0, 1.0]], true)]
        );
    }

    #[test]
    fn answers_update_the_cache_and_the_status() {
        let mut panel = DrawPanel::default();
        let manifest = json!({
            "manifest": {"documentWidth": 6, "documentHeight": 6, "selectedLayerId": "mid", "layers": [
                {"id": "mid", "zScale": 1.0, "width": 6, "height": 6}], "bindings": []},
            "layers": [{"id": "mid", "hasDepthPixels": true, "hasNormalCoverage": false}],
        });
        panel.deliver(&json!({"op": "depth.draw.manifest.get"}), &json!({"succeeded": true, "result": manifest}));
        assert_eq!(panel.selected_layer().map(|l| l.id.as_str()), Some("mid"));
        assert!(panel.has_pixels("mid"));
        // A layer update patches the cached layer so a drag does not jump back.
        let update = json!({"op": "depth.draw.layer.update", "layer": "mid", "zScale": 0.5});
        let answer = json!({"succeeded": true, "result": {"layer": {"id": "mid", "zScale": 0.5, "width": 6, "height": 6}}});
        panel.deliver(&update, &answer);
        assert_eq!(panel.draw.layers[0].z_scale, 0.5);
        // A binding update adds the binding, then replaces it.
        let bind = json!({"op": "depth.draw.binding.update", "layer": "mid", "target": 5});
        let created = json!({"succeeded": true, "result": {"binding": {"layerId": "mid", "targetGridUuid": "5", "order": 1}}});
        panel.deliver(&bind, &created);
        let changed = json!({"succeeded": true, "result": {"binding": {"layerId": "mid", "targetGridUuid": "5", "order": 2}}});
        panel.deliver(&bind, &changed);
        assert_eq!(panel.draw.bindings.len(), 1);
        assert_eq!(panel.draw.bindings[0].order, 2);
        // Compose keeps the depths for the viewport until the session changes.
        let compose = json!({"succeeded": true, "result": {
            "vertexCount": 2, "sampledVertices": 1, "missingVertices": 1, "minDepth": -1.0, "maxDepth": 1.0,
            "depths": [1.0, 0.5], "winningLayerIds": ["mid", ""],
            "apply": {"changedVertices": 1, "layersApplied": 1, "layersSkipped": 0}}});
        panel.deliver(&compose_request(5), &compose);
        assert_eq!(panel.composed(5), Some((vec![1.0, 0.5], vec!["mid".to_owned(), String::new()])));
        assert_eq!(panel.composed(6), None);
        assert_eq!(
            panel.status,
            "Composed 2 vertices: 1 sampled, 1 missing, depth -1.000 to 1.000; Apply would change 1 (layers used 1, skipped 0)"
        );
        panel.observe((1, 0));
        assert_eq!(panel.composed(5), None, "a new session revision drops the preview");
        // Failures show the session's message.
        panel.deliver(&apply_request(5), &json!({"succeeded": false, "message": "node 5 was not found"}));
        assert_eq!((panel.status.as_str(), panel.failed), ("node 5 was not found", true));
        // Requests of other ops are not this panel's business.
        panel.deliver(&json!({"op": "depth.edit.apply"}), &json!({"succeeded": false, "message": "other"}));
        assert_eq!(panel.status, "node 5 was not found");
    }
}
