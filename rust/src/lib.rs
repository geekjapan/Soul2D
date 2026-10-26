mod animation;
mod animation_bake;
mod animation_clone;
mod frame_step;
mod app_shell;
mod animation_source;
pub mod acp;
pub mod atlas;
pub mod autosave;
pub mod binding_values;
pub mod automesh;
mod authoring;
mod authoring_edit;
mod authoring_graph;
mod authoring_hierarchy;
mod authoring_forms;
mod authoring_physics;
mod authoring_keyforms;
mod authoring_scene;
mod authoring_values;
pub mod caff;
pub mod crashdump;
pub mod cv;
pub mod depth_draw;
pub mod export;
mod export_ops;
pub mod gif;
pub mod geometry;
pub mod mesh_edit;
pub mod i18n;
pub mod jpeg;
pub mod manifest;
mod manifest_edit_ops;
mod manifest_gui_entries;
pub mod window_shell;
pub mod inp_export;
pub mod kra;
pub mod pixels;
pub mod psd;
pub mod selector;
pub mod settings;
mod bindings;
mod bone_deform;
mod bone_edit;
mod deformers;
mod depth_ops;
mod depth_resample;
mod depth_session;
mod depths;
mod dirty;
mod document;
pub mod editor_controller;
mod binding_grid;
mod evaluation;
mod fit_z;
mod gpu_deform;
mod mcp;
pub mod mcp_guides;
mod mcp_aliases;
mod mesh_mirror;
mod motion3;
mod motion_pose;
pub mod motion_sync;
mod native_pose;
mod native_physics;
mod native_blend;
mod native_playback;
mod native_convert;
mod native_scene;
pub mod mcp_http;
pub mod mcp_listener;
pub mod mmd_motion;
pub mod model_import;
pub mod njc;
mod nodes;
mod object_names;
mod onion;
mod controller;
mod form;
mod parameter_edit;
mod part_split;
pub mod parameter_flip;
mod physics;
mod playlist;
mod model_template;
mod authoring_clips;
mod authoring_copy;
mod authoring_delete;
mod authoring_fields;
mod authoring_grid;
mod authoring_reparent;
mod moc3;
mod preset_store;
pub use preset_store::StoreWritePlan;
mod physics_runtime;
mod scene_deform;
mod psd_depth;
mod random_pose;
mod render;
mod gltf_export;
mod gltf_mesh;
mod gltf_rig;
mod skeleton;
mod standard_params;
#[cfg(test)]
mod conversion3d_tests;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_scratch;
mod weld;

pub use nodes::{
    properties_for, property_value, puppet_property, PropertyKind, PropertySpec, MASKED_TYPES, MASK_SOURCE_TYPES,
    PUPPET_PROPERTIES,
};
use document::Document;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const HISTORY_LIMIT: usize = 100;
/// Bounds of `history.limit.set`; the limit counts retained states, so 2 keeps one undo step.
const HISTORY_LIMIT_RANGE: std::ops::RangeInclusive<usize> = 2..=1000;
const MAX_RENDER_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_MOTION_BYTES: u64 = 16 * 1024 * 1024;
/// Largest PSD the session reads into memory (the parser's own pixel and channel limits still apply).
const MAX_PSD_BYTES: u64 = 512 * 1024 * 1024;
/// Largest Krita file the session reads (the parser's own inflated-size limits still apply).
const MAX_KRA_BYTES: u64 = 512 * 1024 * 1024;
/// Limits of `extdata.set`: the key length, the decoded payload size and the number of entries.
const MAX_EXT_KEY_BYTES: usize = 256;
const MAX_EXT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXT_ENTRIES: usize = 4096;
/// Extension entry that holds the document's MMD bone table override, as the JSON of the `boneTable` argument.
pub const MMD_BONE_TABLE_KEY: &str = "io.github.geekjapan.soul2d.MmdBoneTable";
/// Largest stored bone table that is read. Every table that `BoneTable::parse` accepts fits: 64 entries whose key
/// is `MAX_MMD_NAME_BYTES` long take about 50 KiB as JSON when every key byte is escaped.
const MAX_MMD_BONE_TABLE_BYTES: usize = 64 * 1024;
/// Work units `atlas.build` may spend (one per drawn or allocated atlas pixel).
const ATLAS_WORK_BUDGET: u64 = 1_000_000_000;
/// Decoded texture pixels `atlas.build` holds at once.
const ATLAS_TEXTURE_PIXELS: u64 = 128 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The layered file formats `import.*` and `merge.*` read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LayerFile {
    Psd,
    Kra,
}

impl LayerFile {
    fn label(self) -> &'static str {
        match self {
            Self::Psd => "PSD",
            Self::Kra => "KRA",
        }
    }

    fn max_bytes(self) -> u64 {
        match self {
            Self::Psd => MAX_PSD_BYTES,
            Self::Kra => MAX_KRA_BYTES,
        }
    }
}

struct AnimationPreview {
    timeline: PreviewTimeline,
    values: BTreeMap<u32, [f64; 2]>,
    deformations: BTreeMap<u32, Vec<[f64; 2]>>,
    /// The clip's form edits on the frame, kept apart from `deformations`, which replace a node's offsets, and from
    /// physics, which the manual says does not see them.
    forms: form::Sample,
}

/// A live recording of parameter values into an animation (issue #177), kept out of the document until it stops.
struct Recording {
    name: String,
    started: std::time::Instant,
    start_frame: i64,
    timestep: f64,
    before: BTreeMap<u32, [f64; 2]>,
    samples: Vec<(u32, i64, [f64; 2])>,
}

impl Recording {
    fn frame(&self) -> i64 {
        // The start frame holds the value before the recording, so a change lands on a later frame.
        let elapsed = (self.started.elapsed().as_secs_f64() / self.timestep) as i64;
        self.start_frame.saturating_add(elapsed.max(1))
    }
}

struct NativeMotionPreview {
    name: String,
    time: f64,
    clip_end: f64,
    values: BTreeMap<String, f64>,
    applied: Value,
    unmapped: Value,
    pose_signals: BTreeMap<String, f64>,
    /// Set when the preview plays with fades.
    fade: Option<motion3::FadeClock>,
}

struct HistoryState {
    model: Value,
    /// Texture and extension sections the document had in this state (INX only). Undo restores them
    /// together with the model, which is what lets a PSD merge replace textures in one step.
    sections: Option<Arc<document::InxSections>>,
    payloads: Option<Arc<document::NativePayloads>>,
    /// Operation that produced this state; empty for the oldest retained one.
    label: String,
    /// In-memory split applications whose created Parts are live in this model state.
    split_applications: part_split::Applications,
}

enum PreviewTimeline {
    Animation { name: String, frame: f64, time: f32 },
    Motion3 {
        name: String, time: f64, clip_end: f64, applied: Value, unmapped: Value, fade: Option<motion3::FadeClock>,
    },
    RandomPose(random_pose::RandomPose),
    Playlist { name: String, time: f64 },
}

pub struct Session {
    document: Document,
    authoring_scene: Option<authoring_scene::ParsedScene>,
    native_scene: Option<native_scene::NativeScene>,
    native_playback: native_playback::State,
    runtime_values: BTreeMap<u32, [f64; 2]>,
    native_runtime_values: BTreeMap<String, f64>,
    // shortcut: CMO3 clips are session-only; persist them when the native motion registry can round-trip.
    native_motions: BTreeMap<String, Value>,
    native_motion_preview: Option<NativeMotionPreview>,
    native_pose: Option<native_pose::Runtime>,
    native_physics: Option<native_physics::Runtime>,
    physics_runtime: physics_runtime::Runtime,
    physics_output: Option<physics_runtime::Step>,
    animation_preview: Option<AnimationPreview>,
    gpu_probe: Option<gpu_deform::Probe>,
    /// Final-pass ambient light; session-only like the D `inSceneAmbientLight` global (survives new/open).
    scene_ambient: [f64; 3],
    /// The settings directory of the host, where the user automesh presets are; survives new/open.
    config_dir: Option<PathBuf>,
    /// Background video exports and the encoder they run; survives new/open like the ambient light.
    video: export_ops::VideoHost,
    video_command_preview: Value,
    motion_sync: motion_sync::MotionHost,
    /// The loss report of the last `rig.import_gltf` that changed a document; session-only, read as a resource.
    rig_import_report: Value,
    /// The loss report of the last `export.gltf` that wrote a file; session-only, read as a resource.
    gltf_export_report: Value,
    /// The loss report of the last `mesh.import_gltf` that changed a document; session-only, read as a resource.
    mesh_import_report: Value,
    /// The mapping report of the last `rig.apply_mmd_pose` or `rig.bake_mmd_motion`, dry run or applied;
    /// session-only, read as a resource.
    mmd_report: Value,
    history: VecDeque<HistoryState>,
    history_op: String,
    history_position: usize,
    history_limit: usize,
    /// `default_form.lock.set`: refuse an edit of an explicit keyform at a parameter's default value (Cubism
    /// "Lock default forms"). Kept for one document, never saved.
    default_form_locked: bool,
    /// `view.*` (Cubism multiview): the views of the document. The active view's values are `runtime_values` and
    /// `native_runtime_values`; the slot of the active view is not read. Kept for one document, never saved.
    views: Vec<ViewSlot>,
    active_view: u32,
    /// While set, every view shows the active values and a view switch keeps them.
    views_synchronized: bool,
    saved_model: Value,
    saved_tail: Vec<u8>,
    saved_payloads: Option<Arc<document::NativePayloads>>,
    file_path: Option<PathBuf>,
    file_baseline: Option<Vec<u8>>,
    clipboard: Vec<nodes::ClipboardEntry>,
    animation_clipboard: Vec<animation::ClipboardEntry>,
    /// `cClipboardParameter` and `cClipboardBindings` of the parameter panel.
    parameter_clipboard: Option<Value>,
    binding_clipboard: Option<parameter_flip::BindingClipboard>,
    recording: Option<Recording>,
    /// Rest-mesh copy between Parts. Separate from the node clipboard. Consumed by a successful paste.
    mesh_clipboard: Option<mesh_edit::MeshClipboard>,
    revision: u64,
    preview_deformation: BTreeMap<u32, Vec<[f64; 2]>>,
    deform_cursor: Option<Vec<dirty::Cell>>,
    /// Vertex offsets for the head cursor cell that are not in the document.
    deform_partial: Vec<[f64; 2]>,
    /// DepthDraw layers, bindings and display options; outside the undo history (see `depth_session`).
    depth_draw: depth_session::State,
    /// The part split draft (source image and part plans); outside the document and its history.
    part_split: part_split::State,
    #[cfg(test)]
    pub(crate) fail_save_after_staging: bool,
}

/// Fits file-supplied playback data to a parameter range the way the Cubism Framework does in
/// `CubismModel::SetParameterValue`: clamp, or wrap when the parameter repeats.
fn native_range_value(value: f64, minimum: f64, maximum: f64, repeat: bool) -> f64 {
    let span = maximum - minimum;
    if !repeat || span <= 0.0 {
        return value.clamp(minimum, maximum);
    }
    let wrapped = if value > maximum {
        minimum + (value - maximum) % span
    } else if value < minimum {
        maximum - (minimum - value) % span
    } else {
        value
    };
    // Finite inputs near f64::MAX overflow the differences above.
    if wrapped.is_finite() { wrapped } else { value.clamp(minimum, maximum) }
}

impl Session {
    pub fn new() -> Self {
        let model = empty_model();
        Self::from_document(Document::new(model), 0, None, None)
    }

    /// Rebuilds the session around another document. What belongs to the host and to no document is kept.
    pub(crate) fn replace_document(
        &mut self,
        document: Document,
        revision: u64,
        file_path: Option<PathBuf>,
        file_baseline: Option<Vec<u8>>,
    ) {
        let (gpu_probe, scene_ambient, history_limit) = (self.gpu_probe, self.scene_ambient, self.history_limit);
        let video = std::mem::take(&mut self.video);
        let motion_sync = std::mem::take(&mut self.motion_sync);
        let config_dir = self.config_dir.take();
        *self = Self::from_document(document, revision, file_path, file_baseline);
        self.gpu_probe = gpu_probe;
        self.scene_ambient = scene_ambient;
        self.history_limit = history_limit;
        self.video = video;
        self.motion_sync = motion_sync;
        self.config_dir = config_dir;
    }

    pub fn handle_request(&mut self, request: &Value) -> Value {
        self.handle_prepared_request(request, None)
    }

    pub(crate) fn handle_prepared_request(&mut self, request: &Value, prepared: Option<export_ops::PreparedExport>) -> Value {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle_inner(request, prepared)))
        {
            Ok(result) => result,
            Err(_) => error("internal panic while handling request"),
        }
    }

    /// Handle one MCP JSON-RPC request against this session. The protocol
    /// engine lives in the library so a future single-owner GUI can drive the
    /// same `Session` without a second process or a copied tool registry.
    pub fn handle_mcp(&mut self, request: &Value) -> Option<Value> {
        self.handle_mcp_guarded(request, |_| Ok(()))
    }

    /// Like [`Session::handle_mcp`], but `guard` vets each canonical core
    /// request produced by `tools/call` before it reaches
    /// [`Session::handle_request`]. `Err(message)` answers the tool call with
    /// `isError` and the message, leaving the session untouched. Non-tool
    /// requests such as `initialize`, `resources/read`, unknown methods, and
    /// malformed envelopes never invoke the guard.
    pub fn handle_mcp_guarded(
        &mut self,
        request: &Value,
        mut guard: impl FnMut(&Value) -> Result<(), String>,
    ) -> Option<Value> {
        mcp::handle_mcp(self, request, |_, call| { guard(call)?; Ok(None) })
    }

    pub fn snapshot(&self) -> Value {
        let native_context = self.document.is_native() || self.document.is_animation();
        let native_source = self.native_model_source();
        let native_parameters = native_source
            .and_then(|source| source["parameters"].as_array())
            .cloned()
            .unwrap_or_default();
        let effective_native = self
            .effective_native_playback_values()
            .unwrap_or_else(|_| self.native_runtime_values.clone());
        let mut snapshot = json!({
            "documentFormat": self.document.format(),
            "model": self.document.model,
            "values": if native_context { json!(effective_native) }
                else { values_json(self.effective_values()) },
            "physics": self.physics_snapshot(),
            "nativePose": self.native_pose.as_ref().map(native_pose::Runtime::snapshot),
            "nativePhysics": self.native_physics.as_ref().map(native_physics::Runtime::snapshot),
            "animation": self.animation_preview.as_ref().and_then(|preview| match &preview.timeline {
                PreviewTimeline::Animation { name, frame, .. } => Some(json!({"name": name, "frame": frame})),
                _ => None,
            }),
            "playlist": self.animation_preview.as_ref().and_then(|preview| match &preview.timeline {
                PreviewTimeline::Playlist { name, time } => Some(json!({"name": name, "time": time})),
                _ => None,
            }),
            "randomPose": self.animation_preview.as_ref().and_then(|preview| match &preview.timeline {
                PreviewTimeline::RandomPose(pose) => Some(pose.summary()),
                _ => None,
            }),
            "motion3": if self.document.is_native() {
                self.native_motion_preview.as_ref().map(|preview| with_fade(json!({"name": preview.name,
                    "time": preview.time, "clipEnd": preview.clip_end,
                    "applied": preview.applied, "unmapped": preview.unmapped}), preview.fade))
            } else { self.animation_preview.as_ref().and_then(|preview| match &preview.timeline {
                PreviewTimeline::Motion3 { name, time, clip_end, applied, unmapped, fade } => Some(with_fade(json!({
                    "name": name, "time": time, "clipEnd": clip_end, "applied": applied, "unmapped": unmapped}), *fade)),
                _ => None,
            }) },
            "canUndo": self.history_position > 0,
            "canRedo": self.history_position + 1 < self.history.len(),
            "history": self.history_summary(),
            "modified": self.is_modified(),
            "revision": self.revision,
            "defaultFormLocked": self.default_form_locked,
            "views": self.views_json(),
            "deformPending": self.deform_cursor.is_some(),
            "scene": {"ambientLight": self.scene_ambient},
            "depthDraw": self.depth_draw.summary(),
            "partSplit": self.part_split.summary(self.split_applied()),
            "clipboard": nodes::clipboard_summaries(&self.clipboard),
            "animationClipboard": animation::clipboard_summaries(&self.animation_clipboard),
            "parameterClipboard": self.parameter_clipboard.as_ref()
                .map(|parameter| json!({"uuid": parameter["uuid"], "name": parameter["name"]})),
            "bindingClipboard": self.binding_clipboard.as_ref().map(parameter_flip::BindingClipboard::summary),
            "previewDeformation": preview_json(self.effective_deformations()),
        });
        if self.document.is_native() {
            snapshot["motion3Library"] = json!(self.native_motions);
        }
        if native_context {
            let animation = self.active_animation_source();
            let scenes = animation
                .and_then(|source| source["scenes"].as_array())
                .into_iter()
                .flatten()
                .map(|scene| {
                    json!({
                        "guid": scene["guid"], "name": scene["name"],
                        "fps": scene["fps"], "durationFrames": scene["durationFrames"]
                    })
                })
                .collect::<Vec<_>>();
            let model_path = if self.document.is_native() {
                self.file_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
            } else {
                self.native_playback
                    .model
                    .as_ref()
                    .map(|model| model.path.clone())
            };
            let animation_path = if self.document.is_animation() {
                self.file_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
            } else {
                self.native_playback
                    .animation
                    .as_ref()
                    .map(|animation| animation.path.clone())
            };
            let resource_guid = if self.document.is_native() {
                self.native_playback
                    .animation
                    .as_ref()
                    .map(|animation| animation.resource_guid.clone())
            } else {
                self.native_playback
                    .model
                    .as_ref()
                    .map(|model| model.resource_guid.clone())
            };
            let model_name = native_source.and_then(|source| source["name"].as_str());
            let active = self.native_playback.active.as_ref().map(|preview| {
                json!({
                    "sceneGuid": preview.scene_guid, "frame": preview.frame,
                    "clockFrame": preview.clock_frame,
                    "fps": preview.fps, "durationFrames": preview.duration_frames,
                    "poseSignals": preview.pose_signals, "partOpacities": preview.part_opacities
                })
            });
            let mut warnings = Vec::new();
            if let Some(warning) = self.part_opacity_binding_warning() {
                warnings.push(warning);
            }
            if native_source.is_some_and(|source| {
                source["sources"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["data"]["type"] == "Rotation"
                            && item["forms"].as_array().is_some_and(|forms| {
                                forms.iter().any(|form| {
                                    let data = &form["data"];
                                    data["reflectX"] == true || data["reflectY"] == true
                                })
                            })
                    })
                })
            }) {
                warnings.push(native_scene::ROTATION_UNVERIFIED);
            }
            let expressions = self.native_playback.expression_list();
            let mut playback = json!({
                "scenes": scenes, "resourceGuid": resource_guid, "modelName": model_name,
                "modelPath": model_path, "animationPath": animation_path, "active": active,
                "expressions": expressions["expressions"], "expressionName": self.native_playback.expression_name,
                "expressionWeight": expressions["weight"],
                "parameters": native_parameters, "warnings": warnings
            });
            if playback["resourceGuid"].is_null() {
                playback["resourceGuid"] = Value::Null;
            }
            snapshot["nativePlayback"] = playback;
        }
        snapshot
    }

    fn native_model_source(&self) -> Option<&Value> {
        if self.document.is_native() {
            Some(&self.document.model)
        } else {
            self.native_playback
                .model
                .as_ref()
                .map(|model| model.scene.source())
        }
    }

    fn active_animation_source(&self) -> Option<&Value> {
        if self.document.is_animation() {
            Some(&self.document.model)
        } else {
            self.native_playback
                .animation
                .as_ref()
                .map(|animation| &animation.source)
        }
    }

    fn effective_native_playback_values(&self) -> Result<BTreeMap<String, f64>, String> {
        let mut values = self.native_physics_inputs()?;
        if let Some(physics) = &self.native_physics { values.extend(physics.values.clone()); }
        Ok(values)
    }

    fn native_physics_inputs(&self) -> Result<BTreeMap<String, f64>, String> {
        let Some(source) = self.native_model_source() else {
            return Ok(BTreeMap::new());
        };
        let base = if self.document.is_native() {
            self.effective_native_values().clone()
        } else {
            self.native_runtime_values.clone()
        };
        self.native_playback.effective_values(
            &base,
            source["parameters"]
                .as_array()
                .ok_or("native parameters are missing")?,
        )
    }

    fn active_native_scene(&mut self) -> Result<&native_scene::NativeScene, String> {
        if self.document.is_native() {
            if self.native_scene.is_none() {
                let bytes = self.document.encode()?;
                let container = caff::CaffDocument::parse(&bytes)?;
                self.native_scene = Some(native_scene::NativeScene::parse(&container)?);
            }
            return self
                .native_scene
                .as_ref()
                .ok_or("native scene cache is missing".into());
        }
        self.native_playback
            .model
            .as_ref()
            .map(|model| &model.scene)
            .ok_or_else(|| "rendering an active CAN3 requires an explicitly attached CMO3".into())
    }

    fn native_render_scene(
        &mut self,
        request: &Value,
    ) -> Result<(Value, scene_deform::SceneDeform, Vec<String>), String> {
        if !self.document.is_native()
            && !(self.document.is_animation() && self.native_playback.model.is_some())
        {
            return Err(
                "native rendering requires an active CMO3 or an attached CMO3 for CAN3".into(),
            );
        }
        if request.get("deformations").is_some_and(|deformations| {
            deformations
                .as_object()
                .is_none_or(|values| !values.is_empty())
        }) {
            return Err("native rendering does not accept manual deformations".into());
        }
        let mut values = self.effective_native_playback_values()?;
        if let Some(overrides) = request.get("values") {
            let overrides = overrides
                .as_object()
                .ok_or("native render values must be an object")?;
            let source = self
                .native_model_source()
                .ok_or("native model source is missing")?;
            for (id, value) in overrides {
                let value = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("native parameter {id:?} must be finite"))?;
                let parameter = source["parameters"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|parameter| parameter["id"].as_str() == Some(id.as_str()))
                    .ok_or_else(|| format!("unknown native parameter ID {id:?}"))?;
                let min = parameter["minimum"]
                    .as_f64()
                    .ok_or("native parameter minimum is missing")?;
                let max = parameter["maximum"]
                    .as_f64()
                    .ok_or("native parameter maximum is missing")?;
                let repeat = parameter["repeat"] == true;
                if !repeat && (value < min || value > max) {
                    return Err(format!(
                        "native parameter {id:?} is outside its authored range"
                    ));
                }
                values.insert(id.clone(), native_range_value(value, min, max, repeat));
            }
        }
        let mut part_opacities = self.native_playback.active.as_ref()
            .map(|preview| preview.part_opacities.clone()).unwrap_or_default();
        if let Some(pose) = &self.native_pose { part_opacities.extend(pose.opacities.clone()); }
        let (model, mut warnings) = self.active_native_scene()?.render_model_with_parts(&values, &part_opacities)?;
        let repeats = self.native_model_source().is_some_and(|source| {
            source["parameters"].as_array().into_iter().flatten().any(|parameter| parameter["repeat"] == true)
        });
        if repeats {
            warnings.push("native repeat interpolation across the seam is approximate".into());
        }
        if let Some(warning) = self.part_opacity_binding_warning() {
            warnings.push(warning.into());
        }
        let root = model
            .get("nodes")
            .ok_or("native render model has no nodes")?;
        let mut no_path_update =
            |_: u32, _: &Value, _: bone_deform::Mat4, _: f64, _: &[[f64; 2]]| Ok(None);
        let geometry = scene_deform::evaluate_with_path_update(
            &model,
            root,
            &BTreeMap::new(),
            &mut no_path_update,
        )?;
        Ok((model, geometry, warnings))
    }

    fn part_opacity_binding_warning(&self) -> Option<&'static str> {
        let animation = self.active_animation_source()?;
        let resource_guid = self.native_resource_guid();
        let has_underscore_curves = animation["scenes"].as_array().into_iter().flatten()
            .flat_map(|scene| scene["tracks"].as_array().into_iter().flatten())
            .filter(|track| resource_guid.as_deref().is_none_or(|resource| track["resourceGuid"].as_str() == Some(resource)))
            .any(|track| track["underscorePartOpacityCurves"].as_array().is_some_and(|curves| !curves.is_empty()));
        let has_legacy_curves = animation["scenes"].as_array().is_some_and(|scenes| {
            scenes.iter().any(|scene| {
                scene["tracks"].as_array().is_some_and(|tracks| {
                    tracks.iter().any(|track| {
                        if resource_guid.as_deref().is_some_and(|resource| {
                            track["resourceGuid"].as_str() != Some(resource)
                        }) {
                            return false;
                        }
                        track["legacyPartOpacityCurves"]
                            .as_array()
                            .is_some_and(|curves| !curves.is_empty())
                            || track["skippedLegacyPartOpacityCount"]
                                .as_u64()
                                .unwrap_or(0)
                                > 0
                    })
                })
            })
        });
        if !has_legacy_curves && !has_underscore_curves {
            return None;
        }
        let (Some(model), Some(resource_guid)) = (self.native_model_source(), resource_guid) else {
            return Some("CAN3 PartOpacity binding is unavailable");
        };
        match animation_source::bindings(animation, model, &resource_guid) {
            Ok(report) if report["unboundUnderscorePartCurves"].as_array().is_some_and(|curves| !curves.is_empty()) => {
                Some("unmatched underscore-map CAN3 Part GUIDs were not evaluated")
            }
            Ok(report) if report["unboundLegacyPartCurves"].as_array().is_some_and(|curves| !curves.is_empty()) => {
                Some("unmatched legacy CAN3 Part GUIDs were not evaluated")
            }
            Ok(report) if report["unboundLegacyPartCurves"].is_array()
                && report["unboundUnderscorePartCurves"].is_array() => None,
            _ => Some("CAN3 PartOpacity binding is unavailable"),
        }
    }

    pub fn resource_parameters(&self) -> Value {
        if self.document.is_animation() {
            if let Some(model) = &self.native_playback.model {
                return Value::Array(model.scene.parameters().to_vec());
            }
        }
        self.document
            .model
            .get(if self.document.is_native() { "parameters" } else { "param" })
            .cloned()
            .unwrap_or_else(|| json!([]))
    }

    fn from_document(
        document: Document,
        revision: u64,
        file_path: Option<PathBuf>,
        file_baseline: Option<Vec<u8>>,
    ) -> Self {
        let initial_model = document.model.clone();
        let depth_draw = depth_session::State::from_model(&document.model);
        let initial_sections = document.inx_sections();
        let initial_payloads = document.native_payloads();
        let saved_model = document.model.clone();
        let saved_tail = document.tail().to_vec();
        let runtime_values = default_values(&document.model);
        let native_runtime_values = document.model["parameters"].as_array().into_iter().flatten()
            .filter_map(|parameter| {
                Some((parameter["id"].as_str()?.to_owned(), parameter["default"].as_f64()?,
                ))
            })
            .collect();
        Self {
            document,
            authoring_scene: None,
            native_scene: None,
            native_playback: native_playback::State::default(),
            runtime_values,
            native_runtime_values,
            native_motions: BTreeMap::new(),
            native_motion_preview: None,
            native_pose: None,
            native_physics: None,
            physics_runtime: physics_runtime::Runtime::default(),
            physics_output: None,
            animation_preview: None,
            gpu_probe: None,
            video: export_ops::VideoHost::default(),
            video_command_preview: Value::Null,
            motion_sync: motion_sync::MotionHost::default(),
            rig_import_report: Value::Null,
            gltf_export_report: Value::Null,
            mesh_import_report: Value::Null,
            mmd_report: Value::Null,
            scene_ambient: [1.0; 3],
            config_dir: None,
            history: VecDeque::from([HistoryState {
                model: initial_model,
                sections: initial_sections,
                payloads: initial_payloads.clone(),
                label: String::new(),
                split_applications: part_split::Applications::default(),
            }]),
            history_op: String::new(),
            history_position: 0,
            history_limit: HISTORY_LIMIT,
            default_form_locked: false,
            views: vec![ViewSlot { id: 1, ..ViewSlot::default() }],
            active_view: 1,
            views_synchronized: false,
            saved_model,
            saved_tail,
            saved_payloads: initial_payloads,
            file_path,
            file_baseline,
            clipboard: Vec::new(),
            animation_clipboard: Vec::new(),
            parameter_clipboard: None,
            binding_clipboard: None,
            recording: None,
            mesh_clipboard: None,
            revision,
            preview_deformation: BTreeMap::new(),
            deform_cursor: None,
            deform_partial: Vec::new(),
            depth_draw,
            part_split: part_split::State::default(),
            #[cfg(test)]
            fail_save_after_staging: false,
        }
    }

    fn handle_inner(&mut self, request: &Value, prepared: Option<export_ops::PreparedExport>) -> Value {
        let _gpu_scope = gpu_deform::activate_probe(self.gpu_probe);
        let Some(object) = request.as_object() else {
            return error("request must be a JSON object");
        };
        let Some(op) = object.get("op").and_then(Value::as_str) else {
            return error("request requires a string op");
        };
        self.history_op = op.to_owned();
        if self.deform_cursor.is_some() && !allows_pending_deform(op) {
            return error("a deform refresh is still pending");
        }
        if self.document.is_animation()
            && !(op == "live2d.authoring.read" && request.get("path").is_some())
            && !matches!(op,
            "new" | "open" | "import.psd" | "import.kra" | "import.inp" | "import.folder" | "snapshot" | "parameters" | "save" | "undo" | "redo"
            | "history.summary" | "history.goto" | "history.clear" | "history.limit.set"
            | "cv.analyze" | "image.process"
            | "automesh.preset.save" | "automesh.preset.delete" | "automesh.preset.list"
            | "physics.preset.save" | "physics.preset.delete" | "physics.preset.list"
            | "live2d.physics3.preset.save" | "live2d.physics3.preset.delete" | "live2d.physics3.preset.list"
            | "animation.template.delete" | "animation.template.list"
            | "export.video.status" | "export.video.cancel" | "export.video.probe" | "export.video.probe.status"
            | "animation.motionsync.start" | "animation.motionsync.status" | "animation.motionsync.cancel"
            | "live2d.animation.read" | "live2d.animation.key.set" | "live2d.animation.point.set" | "live2d.animation.point.delete" | "live2d.animation.point.add" | "live2d.animation.bindings"
            | "live2d.animation.scene.set" | "live2d.animation.scene.add"
            | "live2d.animation.handle.set" | "live2d.animation.curve_type.set"
            | "live2d.animation.key.add" | "live2d.animation.key.delete"
            | "live2d.animation.curve.add" | "live2d.animation.curve.delete"
            | "live2d.animation.track.add" | "live2d.animation.track.delete" | "live2d.animation.track.duplicate"
            | "live2d.animation.scene.delete" | "live2d.animation.resource.add" | "live2d.animation.image_resource.add"
            | "live2d.animation.image_resource.relink"
            | "live2d.authoring.inspect"
                    | "live2d.animation.attach"
                    | "live2d.model.attach"
                    | "live2d.animation.list"
                    | "live2d.animation.seek"
                    | "live2d.animation.advance"
                    | "live2d.animation.reset"
                    | "live2d.expression.import"
                    | "live2d.expression.apply"
                    | "live2d.expression.reset"
                    | "live2d.expression.list"
                    | "live2d.parameter.set"
                    | "live2d.pose.attach" | "live2d.pose.reset" | "live2d.pose.step"
                    | "live2d.physics.attach" | "live2d.physics.reset" | "live2d.physics.step"
                    | "live2d.physics.options.set" | "live2d.physics.stabilize"
                    | "render"
                    | "render.packet"
                    | "scene.geometry"
                    | "model.statistics"
                    | "texture.get"
                    | "bounds"
                    | "scene.ambient.set"
            ) {
            return error("operation is not supported for the active CAN3 animation document");
        }
        if self.document.is_native() && !matches!(op,
            "new" | "open" | "import.psd" | "import.kra" | "import.inp" | "import.folder" | "snapshot" | "parameters" | "save" | "undo" | "redo"
            | "history.summary" | "history.goto" | "history.clear" | "history.limit.set"
            | "cv.analyze" | "image.process"
            | "automesh.preset.save" | "automesh.preset.delete" | "automesh.preset.list"
            | "physics.preset.save" | "physics.preset.delete" | "physics.preset.list"
            | "live2d.physics3.preset.save" | "live2d.physics3.preset.delete" | "live2d.physics3.preset.list"
            | "animation.template.delete" | "animation.template.list"
            | "export.video.status" | "export.video.cancel" | "export.video.probe" | "export.video.probe.status"
            | "animation.motionsync.start" | "animation.motionsync.status" | "animation.motionsync.cancel"
            | "live2d.authoring.inspect" | "live2d.authoring.read" | "live2d.cdi3.export" | "live2d.physics3.export"
            | "live2d.physics3.read" | "live2d.physics3.edit" | "live2d.convert.inx"
            | "live2d.userdata3.export" | "live2d.user_data.set"
            | "view.list" | "view.add" | "view.select" | "view.remove" | "view.sync.set"
            | "live2d.animation.read" | "live2d.animation.bindings"
            | "live2d.animation.attach"
                    | "live2d.animation.list"
                    | "live2d.animation.seek"
                    | "live2d.animation.advance"
                    | "live2d.animation.reset"
                    | "live2d.expression.import"
                    | "live2d.expression.apply"
                    | "live2d.expression.reset"
                    | "live2d.expression.list"
                    | "live2d.authoring.linear_weights" | "live2d.authoring.primary_forms" | "live2d.authoring.morph_forms"
            | "live2d.form.set_vertex" | "live2d.form.set_opacity" | "live2d.rename" | "live2d.parameter.set"
            | "live2d.form.paste" | "live2d.form.set_opacities" | "live2d.form.set" | "live2d.source.set"
            | "live2d.artpath.point.set" | "live2d.artpath.point.add" | "live2d.artpath.point.delete"
            | "live2d.brush.set" | "live2d.brush.replace_image" | "live2d.reparent" | "live2d.reparent_deformer" | "live2d.delete"
            | "live2d.parameter.delete" | "live2d.set_id" | "live2d.duplicate" | "live2d.parameter.edit"
            | "live2d.keyform.add" | "live2d.keyform.delete"
            | "live2d.runtime.inspect"
            | "live2d.pose.attach" | "live2d.pose.reset" | "live2d.pose.step"
            | "live2d.physics.attach" | "live2d.physics.reset" | "live2d.physics.step"
            | "live2d.physics.options.set" | "live2d.physics.stabilize"
            | "motion3.import" | "motion3.list" | "motion3.read" | "motion3.sample" | "motion3.export"
            | "motion3.pose" | "motion3.seek" | "motion3.advance" | "motion3.reset" | "motion3.fade.set"
            | "motion3.pose.switch"
                    | "render"
                    | "render.packet"
                    | "scene.geometry"
                    | "model.statistics"
                    | "texture.get"
                    | "bounds"
                    | "scene.ambient.set"
            ) {
            return error("operation is not supported for the active Live2D document");
        }
        let result = match op {
            "new" => self.op_new(request),
            "open" => self.op_open(request),
            "import.psd" => self.op_import_model(request, LayerFile::Psd),
            "merge.psd.plan" => self.op_merge_plan(request, LayerFile::Psd),
            "merge.psd.apply" => self.op_merge_apply(request, LayerFile::Psd),
            "import.kra" => self.op_import_model(request, LayerFile::Kra),
            "merge.kra.plan" => self.op_merge_plan(request, LayerFile::Kra),
            "merge.kra.apply" => self.op_merge_apply(request, LayerFile::Kra),
            "export.inp" => self.op_export_inp(request),
            "export.image" => self.op_export_image(request),
            "export.frames" => self.op_export_frames(request),
            "export.gif" => self.op_export_gif(request),
            "export.collect" => match prepared { Some(export_ops::PreparedExport::Collection(plan)) => self.execute_collection(plan), None => self.op_export_collect(request), _ => Err("prepared export does not match operation".into()) },
            "export.video.start" => match prepared { Some(export_ops::PreparedExport::Video(video)) => self.launch_video_export(video), None => self.op_export_video_start(request), _ => Err("prepared export does not match operation".into()) },
            "export.video.status" => self.op_export_video_status(request),
            "export.video.probe" => self.op_export_video_probe(),
            "export.video.probe.status" => Ok(self.op_export_video_probe_status()),
            "export.video.command" => self.op_export_video_command(request),
            "export.video.cancel" => self.op_export_video_cancel(request),
            "animation.motionsync.start" => self.op_motionsync_start(request),
            "animation.motionsync.status" => self.op_motionsync_status(request),
            "animation.motionsync.cancel" => self.op_motionsync_cancel(request),
            "animation.motionsync.bake" => self.op_motionsync_bake(request),
            "import.inp" => self.op_import_inp(request),
            "merge.inp" => self.op_merge_inp(request),
            "import.folder" => self.op_import_folder(request),
            "merge.images" => self.op_merge_images(request),
            "tool.regenerate_node_ids" | "tool.repair" => self.op_tool_regenerate_ids(),
            "tool.generate_fake_layer_names" => self.op_tool_fake_layer_names(),
            "tool.premultiply_textures" => self.op_tool_textures(false),
            "tool.rebleed_textures" => self.op_tool_textures(true),
            "tool.import_session_data" => self.op_tool_import_session_data(request),
            "texture.set" => self.op_texture_set(request),
            "texture.export" => self.op_texture_export(request),
            "texture.replace" => self.op_texture_replace(request),
            "view.screenshot" => self.op_view_screenshot(request),
            "sketch.snapshot" => self.op_sketch_snapshot(request),
            "values.reset" => self.op_values_reset(),
            "extdata.get" => self.op_extdata_get(request),
            "extdata.set" => self.op_extdata_set(request),
            "texture.get" => self.op_texture_get(request),
            "scene.geometry" => self.op_scene_geometry(request),
            "scene.overlap" => self.op_scene_overlap(request),
            "model.statistics" => self.op_model_statistics(),
            "atlas.build" => self.op_atlas_build(request),
            "snapshot" => Ok(self.snapshot()),
            "live2d.authoring.inspect" | "live2d.authoring.read" | "live2d.animation.read" | "live2d.authoring.linear_weights" | "live2d.authoring.primary_forms" | "live2d.authoring.morph_forms" => self.op_authoring(request),
            "live2d.form.set_vertex" => self.op_authoring_set_vertex(request),
            "live2d.form.set_opacity" => self.op_authoring_set_opacity(request),
            "live2d.form.paste" => self.op_authoring_edit(request, authoring_edit::paste_form),
            "live2d.form.set_opacities" => self.op_authoring_edit(request, authoring_edit::set_opacities),
            "live2d.form.set" => self.op_authoring_edit(request, authoring_fields::set_form),
            "live2d.source.set" => self.op_authoring_edit(request, authoring_fields::set_source),
            "live2d.artpath.point.set" => self.op_authoring_edit(request, authoring_fields::set_point),
            "live2d.artpath.point.add" => self.op_authoring_edit(request, |model, request| {
                authoring_fields::add_point(model, request, &mut animation_source::new_guid)
            }),
            "live2d.artpath.point.delete" => self.op_authoring_edit(request, authoring_fields::delete_point),
            "live2d.brush.set" => self.op_authoring_edit(request, authoring_fields::set_brush),
            "live2d.brush.replace_image" => self.op_brush_replace_image(request),
            "live2d.reparent" => self.op_authoring_edit(request, authoring_reparent::reparent),
            "live2d.reparent_deformer" => self.op_authoring_edit(request, authoring_reparent::reparent_deformer),
            "live2d.delete" => self.op_authoring_edit(request, authoring_delete::delete),
            "live2d.parameter.delete" => self.op_authoring_edit(request, authoring_delete::delete_parameter),
            "live2d.rename" => self.op_authoring_rename(request),
            "live2d.set_id" => self.op_authoring_edit(request, authoring_edit::set_id),
            "live2d.duplicate" => self.op_authoring_duplicate(request),
            "live2d.parameter.edit" => self.op_authoring_edit(request, authoring_fields::set_parameter),
            "live2d.keyform.add" => self.op_authoring_edit(request, |model, request| {
                authoring_grid::add_key(model, request, &mut animation_source::new_guid)
            }),
            "live2d.keyform.delete" => self.op_authoring_edit(request, authoring_grid::delete_key),
            "live2d.cdi3.export" => self.op_runtime_json_export(request, authoring::display_info),
            "live2d.runtime.inspect" => moc3::inspect(request),
            "live2d.userdata3.export" => self.op_runtime_json_export(request, authoring::user_data),
            "live2d.user_data.set" => self.op_authoring_user_data(request),
            "live2d.physics3.export" => self.op_runtime_json_export(request, authoring::physics),
            "live2d.convert.inx" => self.op_convert_inx(request),
            "live2d.physics3.read" => self.physics_source_view(),
            "live2d.physics3.edit" => self.op_physics_source_edit(request),
            "live2d.animation.key.set" => self.op_animation_set_key(request, animation_source::set_key),
            "live2d.animation.point.set" => self.op_animation_set_key(request, animation_source::set_point),
            "live2d.animation.point.delete" => self.op_animation_set_key(request, animation_source::delete_point),
            "live2d.animation.point.add" => self.op_animation_set_key(request, animation_source::add_point),
            "live2d.animation.handle.set" => self.op_animation_set_key(request, animation_source::set_handle),
            "live2d.animation.curve_type.set" => self.op_animation_set_key(request, animation_source::set_curve_type),
            "live2d.animation.key.add" => self.op_animation_set_key(request, animation_source::add_key),
            "live2d.animation.key.delete" => self.op_animation_set_key(request, animation_source::delete_key),
            "live2d.animation.curve.add" => self.op_animation_curve(request, false),
            "live2d.animation.curve.delete" => self.op_animation_curve(request, true),
            "live2d.animation.track.add" => self.op_animation_track(request, false),
            "live2d.animation.track.delete" => self.op_animation_track(request, true),
            "live2d.animation.track.duplicate" => self.op_animation_track_duplicate(request),
            "live2d.animation.resource.add" => self.op_animation_resource(request, animation_source::ResourceKind::Model),
            "live2d.animation.image_resource.add" => self.op_animation_resource(request, animation_source::ResourceKind::Image),
            "live2d.animation.image_resource.relink" => self.op_animation_image_relink(request),
            "live2d.animation.scene.set" | "live2d.animation.scene.add" | "live2d.animation.scene.delete" => self.op_animation_scene(op, request),
            "live2d.animation.bindings" => self.op_animation_bindings(request),
            "live2d.animation.attach" => self.op_native_animation_attach(request),
            "live2d.model.attach" => self.op_native_model_attach(request),
            "live2d.animation.list" => Ok(self.op_native_animation_list()),
            "live2d.animation.seek" => self.op_native_animation_seek(request),
            "live2d.animation.advance" => self.op_native_animation_advance(request),
            "live2d.animation.reset" => Ok(self.op_native_animation_reset()),
            "live2d.expression.import" => self.op_native_expression_import(request),
            "live2d.expression.apply" => self.op_native_expression_apply(request),
            "live2d.expression.reset" => Ok(self.op_native_expression_reset()),
            "live2d.expression.list" => Ok(self.native_playback.expression_list()),
            "live2d.parameter.set" => self.op_native_parameter_set(request),
            "live2d.pose.attach" => self.op_native_pose_attach(request),
            "live2d.pose.reset" => self.op_native_pose_reset(),
            "live2d.pose.step" => self.op_native_pose_step(request),
            "live2d.physics.attach" => self.op_native_physics_attach(request),
            "live2d.physics.reset" => self.op_native_physics_reset(),
            "live2d.physics.step" => self.op_native_physics_step(request),
            "live2d.physics.options.set" => self.op_native_physics_options_set(request),
            "live2d.physics.stabilize" => self.op_native_physics_stabilize(),
            "parameters" => Ok(self.resource_parameters()),
            "atlas.pack" => atlas::plan(request),
            "cv.analyze" => cv::analyze(request),
            "image.process" => pixels::process(request),
            "nodes.list" => Ok(nodes::list_nodes(&self.document.model)),
            "node.get" => self.op_node_get(request),
            "node.create" => self.op_node_create(request),
            "physics.set" => self.op_physics_set(request),
            "physics.preset.save" | "physics.preset.delete" | "automesh.preset.save" | "automesh.preset.delete"
                | "live2d.physics3.preset.save" | "live2d.physics3.preset.delete"
                | "animation.template.save" | "animation.template.delete" => {
                let plan = match prepared {
                    Some(export_ops::PreparedExport::Store(plan)) => Ok(plan),
                    None => self.prepare_store_write(request),
                    _ => Err("prepared store does not match operation".into()),
                };
                plan.and_then(|plan| plan.execute(self.config_dir.as_deref()))
            },
            "physics.preset.list" => self.physics_presets(),
            "live2d.physics3.preset.list" => self.physics3_presets(),
            "physics.step" => self.op_physics_step(request),
            "physics.reset" => Ok(self.op_physics_reset()),
            "animation.create" | "animation.rename" | "animation.delete" | "animation.set"
                | "animation.key.set" | "animation.key.delete" | "animation.lane.set"
                | "animation.lane.delete" | "animation.marker.set" | "animation.marker.delete"
                | "animation.key.delete_all" | "animation.blink.parameters.set" | "animation.blink.set"
                | "animation.blink.key.delete" | "animation.audio.set" | "animation.audio.delete"
                | "animation.image.set" | "animation.image.delete" | "animation.image.time" | "animation.group.add" | "animation.group.set"
                | "animation.group.delete" | "animation.shy.set" | "animation.freeze.set" | "animation.bookmark.set"
                | "animation.bookmark.delete" | "animation.bookmark.copy" => self.op_animation_edit(request, op),
            "animation.template.apply" => self.op_animation_template(request),
            "animation.template.list" => self.animation_templates(),
            "animation.controller.add" => self.op_controller_add(request),
            "animation.controller.set" => self.op_controller_set(request),
            "animation.controller.delete" => self.op_controller_delete(request),
            "animation.controller.read" => self.op_controller_read(request),
            "animation.controller.move" => self.op_controller_move(request),
            "animation.controller.bake" => self.op_controller_bake(request),
            "animation.physics.bake" => self.op_animation_physics_bake(request),
            "animation.form.key" => self.op_form_key(request),
            "animation.form.delete" => self.op_form_delete(request),
            "animation.form.time" => self.op_form_time(request),
            "animation.form.read" => self.op_form_read(request),
            "animation.bookmark.parameters" => {
                required_id(request, "node").and_then(|node| animation::bookmark_parameters(&self.document.model, node))
            }
            "animation.audio.add" => self.op_animation_audio_add(request),
            "animation.image.add" => self.op_animation_image_add(request),
            "animation.image.sequence" => self.op_animation_image_sequence(request),
            "animation.image.read" => self.op_animation_image_read(request),
            "animation.frame_step" => self.op_animation_frame_step(request),
            "animation.record.start" => self.op_animation_record_start(request),
            "animation.playlist.set" | "animation.playlist.delete" | "animation.scene_group.set"
                | "animation.scene_group.delete" => self.op_playlist_edit(request, op),
            "animation.playlist.seek" => self.op_playlist_seek(request),
            "atlas.layout.set" | "atlas.layout.clear" => atlas::edit_layout(&self.document.model, request, op)
                .and_then(|candidate| self.commit_model(candidate))
                .map(|_| json!({"atlasLayout": self.document.model.get("atlasLayout")})),
            "animation.record.status" => Ok(self.animation_record_status()),
            "animation.record.stop" => self.op_animation_record_stop(request),
            "animation.key.add" => self.op_animation_key_add(request),
            "animation.key.add_all" => self.op_animation_key_add_all(request),
            "animation.key.copy" => self.op_animation_key_copy(request),
            "animation.key.paste" => self.op_animation_key_paste(request),
            "animation.sample" => self.op_animation_sample(request),
            "animation.onion" => self.op_animation_onion(request),
            "animation.trajectory" => self.op_animation_trajectory(request),
            "animation.seek" => self.op_animation_seek(request),
            "animation.advance" => self.op_animation_advance(request),
            "animation.reset" => self.op_animation_reset(),
            "random_pose.seek" => self.op_random_pose_seek(request),
            "motion3.import" => self.op_motion3_import(request),
            "motion3.list" => self.op_motion3_list(),
            "motion3.read" => self.motion3_document(request).cloned(),
            "motion3.sample" => self.op_motion3_sample(request),
            "motion3.point.set" => self.op_motion3_set_point(request),
            "motion3.fade.set" => self.op_motion3_set_fade(request),
            "motion3.pose.switch" => self.op_motion3_pose_switch(request),
            "motion3.export" => self.op_motion3_export(request),
            "motion3.bind" => self.op_motion3_bind(request),
            "motion3.bindings" => motion3_name(request).and_then(|name| motion_pose::bindings(&self.document.model, name)),
            "motion3.pose" => self.op_motion3_pose(request),
            "motion3.seek" => self.op_motion3_seek(request),
            "motion3.advance" => self.op_motion3_advance(request),
            "motion3.reset" => self.op_motion3_reset(),
            "node.insert" => self.op_node_insert(request),
            "node.move" => self.op_node_move(request),
            "node.delete" => self.op_node_delete(request),
            "node.copy" => self.op_node_copy(request),
            "node.cut" => self.op_node_cut(request),
            "node.paste" => self.op_node_paste(request),
            "node.convert" => self.op_node_convert(request),
            "deformer.define_vertices" => self.op_define_vertices(request),
            "deformer.define_grid" => self.op_define_grid(request),
            "deformer.define_mesh" => self.op_define_mesh(request),
            "mesh.mirror" => self.op_mesh_mirror(request),
            "mesh.edit" => self.op_mesh_edit(request),
            "mesh.copy" => self.op_mesh_copy(request),
            "mesh.paste" => self.op_mesh_paste(request),
            "mesh.select" => mesh_edit::select_vertices(&self.document.model, request),
            "deform.edit" => self.op_deform_edit(request),
            "automesh.generate" => self.op_automesh_generate(request),
            "automesh.preset.list" => self.automesh_presets(),
            "deform.continue" => self.op_deform_continue(),
            "deformer.set" => self.op_deformer_set(request),
            "deformer.validate" => self.op_deformer_validate(request),
            "deformer.extend_grid" => self.op_deformer_extend_grid(request),
            "deformer.apply_to_children" => self.op_deformer_apply_to_children(request),
            "deformer.create_rotation" => self.op_deformer_create_rotation(request),
            "deformer.bezier_controls" => self.op_deformer_bezier(request, false),
            "deformer.bezier_warp" => self.op_deformer_bezier(request, true),
            "deformer.generate_face_turn" => self.op_deformer_generate_face_turn(request),
            "deformer.generate_sway" => self.op_deformer_generate_sway(request),
            "deformer.generate_humanoid" => self.op_deformer_generate_humanoid(request),
            "depth.list" => self.op_depth_list(request),
            "depth.set" => self.op_depth_set(request),
            "depth.clear" => self.op_depth_clear(request),
            "depth.ops.list" => self.op_depth_ops_list(request),
            "depth.ops.set" => self.op_depth_ops_set(request),
            "depth.ops.add" => self.op_depth_ops_add(request),
            "depth.ops.update" => self.op_depth_ops_update(request),
            "depth.ops.remove" => self.op_depth_ops_remove(request),
            "depth.ops.move" => self.op_depth_ops_move(request),
            "depth.ops.clear" => self.op_depth_ops_clear(request),
            "depth.ops.apply" => self.op_depth_ops_apply(request),
            "depth.edit.apply" => self.op_depth_edit_apply(request),
            "depth.draw.manifest.get" => self.op_depth_draw_manifest_get(request),
            "depth.draw.manifest.set" => self.op_depth_draw_manifest_set(request),
            "depth.draw.load_png" => self.op_depth_draw_load_png(request),
            "depth.draw.layer.update" => self.op_depth_draw_layer_update(request),
            "depth.draw.binding.update" => self.op_depth_draw_binding_update(request),
            "depth.draw.auto_bind" => self.op_depth_draw_auto_bind(request),
            "depth.draw.compose" => self.op_depth_draw_compose(request),
            "depth.draw.apply" => self.op_depth_draw_apply(request),
            "depth.draw.cleanup" => self.op_depth_draw_cleanup(request),
            "depth.draw.fit_z" => self.op_depth_draw_fit_z(request),
            "depth.draw.export_png" => match prepared {
                Some(export_ops::PreparedExport::DepthPng(plan)) => self.execute_depth_png_export(plan),
                None => self.op_depth_draw_export_png(request),
                _ => Err("prepared export does not match operation".into()),
            },
            "depth.draw.diagnostics" => self.op_depth_draw_diagnostics(request),
            "depth.draw.display" => self.op_depth_draw_display(request),
            "depth.draw.select" => self.op_depth_draw_select(request),
            "split.open" => self.op_split_open(request),
            "split.part.set" => self.op_split_part_set(request),
            "split.part.remove" => self.op_split_part_remove(request),
            "split.part.order" => self.op_split_part_order(request),
            "split.plan" => self.op_split_plan(request),
            "split.components" => self.op_split_components(request),
            "split.mask.inspect" => self.op_split_mask_inspect(request),
            "split.overlay" => self.op_split_overlay(request),
            "split.apply" => self.op_split_apply(request),
            "split.close" => self.op_split_close(),
            "depth.import_psd" => self.op_import_psd(request),
            "depth.import_psd.plan" => self.op_import_psd_plan(request),
            "rig.add_skeleton" => self.op_add_skeleton(request),
            "rig.fit_human" => self.op_fit_human(request),
            "rig.add_standard_parameters" => self.op_add_standard_parameters(request),
            "rig.list_bones" => self.op_list_bones(request),
            "rig.overlay" => self.op_overlay(request),
            "rig.pick_bone" => self.op_pick_bone(request),
            "rig.drag_bone" => self.op_drag_bone(request),
            "rig.solve_ik" => self.op_solve_ik(request),
            "rig.apply_mmd_pose" => self.op_apply_mmd_pose(request),
            "rig.bake_mmd_motion" => self.op_bake_mmd_motion(request),
            "rig.import_gltf" => self.op_import_gltf_rig(request),
            "mesh.import_gltf" => self.op_import_gltf_mesh(request),
            "export.gltf" => match prepared {
                Some(export_ops::PreparedExport::Gltf(gltf)) => self.execute_gltf_export(gltf),
                None => self.op_export_gltf(request),
                _ => Err("prepared export does not match operation".into()),
            },
            "rig.mirror" => self.op_mirror(request),
            "rig.set_vertex_weights" => self.op_set_vertex_weights(request),
            "rig.clear_vertex_weights" => self.op_clear_vertex_weights(request),
            "rig.add_source" => self.op_add_source(request),
            "rig.remove_source" => self.op_remove_source(request),
            "rig.list_sources" => self.op_list_sources(request),
            "rig.set_source_settings" => self.op_set_source_settings(request),
            "rig.set_influence_rule" => self.op_set_influence_rule(request),
            "rig.get_influence_rule" => self.op_get_influence_rule(request),
            "rig.preview_influence" => self.op_preview_influence(request),
            "rig.preview_deform" => self.op_preview_deform(request),
            "rig.gpu_offsets" => self.op_gpu_offsets(request),
            "rig.apply_deform" => self.op_apply_deform(request),
            "rig.fit_root_z" => self.op_fit_root_z(request),
            "rig.fit_bone_z" => self.op_fit_bone_z(request),
            "rig.set_rest" => self.op_set_rest(request),
            "rig.set_constraint" => self.op_set_constraint(request),
            "node.rename" => self.op_node_rename(request),
            "object.find" => object_names::find(&self.document.model, request),
            "object.replace_name" => self.op_object_replace_name(request),
            "object.export_csv" => self.op_object_export_csv(request),
            "object.import_csv" => self.op_object_import_csv(request),
            "model.template.export" => self.op_model_template_export(request),
            "model.template.apply" => self.op_model_template_apply(request),
            "node.transform" => self.op_node_transform(request),
            "node.property.set" => self.op_node_property_set(request),
            "node.mask.edit" => self.op_node_mask_edit(request),
            "puppet.meta.set" => self.op_puppet_meta_set(request),
            "node.enabled.set" | "node.enabled.toggle" => self.op_node_enabled(request),
            "node.visibility.set" => self.op_node_visibility(request),
            "node.lock.set" => self.op_node_lock(request),
            "parameter.get" => self.op_parameter_get(request),
            "parameter.create" => self.op_parameter_create(request),
            "parameter.rename" => self.op_parameter_rename(request),
            "parameter.delete" => self.op_parameter_delete(request),
            "parameter.set_axes" => self.op_parameter_set_axes(request),
            "parameter.duplicate" => self.op_parameter_duplicate(request),
            "parameter.create_mouth" => self.op_parameter_create_mouth(request),
            "parameter.create_standard" => self.op_parameter_create_standard(request),
            "parameter.group.create" => self.op_parameter_group_create(request),
            "parameter.group.delete" => self.op_parameter_group_delete(request),
            "parameter.group.set_color" => self.op_parameter_group_set_color(request),
            "parameter.move" => self.op_parameter_move(request),
            "parameter.set_repeat" => self.op_parameter_set_repeat(request),
            "parameter.flip" => self.op_parameter_flip(request),
            "parameter.mirror" => self.op_parameter_mirror(request),
            "parameter.set_defaults" => self.op_parameter_set_defaults(request),
            "parameter.set_all_defaults" => self.op_parameter_set_all_defaults(),
            "parameter.split" => self.op_parameter_split(request),
            "binding.keypoint" => self.op_binding_keypoint(request),
            "parameter.link" => self.op_parameter_link(request),
            "parameter.unlink" => self.op_parameter_unlink(request),
            "parameter.mirrored_autofill" => self.op_parameter_mirrored_autofill(request),
            "parameter.synthesize_corners" => self.op_parameter_synthesize_corners(request),
            "parameter.copy" => self.op_parameter_copy(request),
            "parameter.paste" => self.op_parameter_paste(request),
            "parameter.duplicate_flip" => self.op_parameter_duplicate_flip(request),
            "parameter.convert_to_2d" => self.op_parameter_convert_to_2d(request),
            "binding.copy" => self.op_binding_copy(request),
            "binding.paste" => self.op_binding_paste(request),
            "binding.multi_edit" => self.op_binding_multi_edit(request),
            "binding.weight_limits.set" => self.op_binding_weight_limits(request),
            "binding.copy_to" => self.op_binding_move_to_node(request, false),
            "binding.swap_with" => self.op_binding_move_to_node(request, true),
            "binding.remove" => self.op_binding_remove(request),
            "binding.set_interpolation" => self.op_binding_set_interpolation(request),
            "undo" => self.op_history(false),
            "redo" => self.op_history(true),
            "history.summary" => Ok(self.history_summary()),
            "history.goto" => self.op_history_goto(request),
            "history.clear" => Ok(self.op_history_clear()),
            "history.limit.set" => self.op_history_limit_set(request),
            // Native documents never get here: their op list does not hold it. A new document starts unlocked.
            "default_form.lock.set" => match request["locked"].as_bool() {
                Some(locked) => {
                    self.default_form_locked = locked;
                    Ok(self.snapshot())
                }
                None => Err("locked must be a boolean".into()),
            },
            "view.list" => Ok(self.views_json()),
            "view.add" => self.op_view_add(),
            "view.select" => self.op_view_select(request),
            "view.remove" => self.op_view_remove(request),
            "view.sync.set" => match request["synchronized"].as_bool() {
                Some(synchronized) => {
                    if self.views_synchronized && !synchronized {
                        // The views part with the values they shared, and with the pose preview of those values.
                        for view in &mut self.views {
                            view.values = self.runtime_values.clone();
                            view.native_values = self.native_runtime_values.clone();
                            view.preview = Some((self.revision, self.preview_deformation.clone()));
                        }
                    }
                    self.views_synchronized = synchronized;
                    Ok(self.views_json())
                }
                None => Err("synchronized must be a boolean".into()),
            },
            "save" => self.op_save(request),
            "export" => self.op_export(),
            "binding.set_trs" => self.op_set_trs(request),
            "binding.set_deform" => self.op_set_deform(request),
            "evaluate" => self.op_evaluate(request),
            "scene.evaluate" => self.op_evaluate_scene(request),
            "value.set" => self.op_value_set(request),
            "scene.ambient.set" => self.op_scene_ambient_set(request),
            "render" => self.op_render(request),
            "render.packet" => self.op_render_packet(request),
            "bounds" => self.op_bounds(request),
            "part.set_weld" => self.op_set_weld(request),
            "part.clear_weld" => self.op_clear_weld(request),
            _ => Err(format!("unknown operation: {op}")),
        };
        match result {
            Ok(result) => json!({ "succeeded": true, "status": "ok", "result": result }),
            Err(message) => error(&message),
        }
    }

    fn op_new(&mut self, request: &Value) -> Result<Value, String> {
        let mut model = match request.get("model") {
            Some(model) => model.clone(),
            None => empty_model(),
        };
        prepare_model(&mut model)?;
        let revision = self.revision.saturating_add(1);
        self.replace_document(Document::new(model), revision, None, None);
        Ok(self.snapshot())
    }

    fn op_open(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        let mut file = File::open(path).map_err(|error| format!("could not read {path}: {error}"))?;
        if !file.metadata().map_err(|error| error.to_string())?.is_file() {
            return Err("document source must be a regular file".into());
        }
        let mut bytes = Vec::new();
        (&mut file).take(4).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        if bytes.starts_with(b"CAFF") {
            file.take(256 * 1024 * 1024 - 4 + 1).read_to_end(&mut bytes)
                .map_err(|error| format!("could not read {path}: {error}"))?;
        } else {
            file.read_to_end(&mut bytes).map_err(|error| format!("could not read {path}: {error}"))?;
        }
        let normalized = normalize_target_path(Path::new(path))?;
        let mut document = Document::parse(&bytes)?;
        if document.is_inx() {
            resolve_relative_image_paths(&mut document.model, &normalized);
            prepare_model(&mut document.model)?;
        }
        let revision = self.revision.saturating_add(1);
        // "as": keep this document's content but adopt another path (a restored autosave backup keeps
        // the project path). The saved baseline is the main file, so the document reads as modified.
        let main = match request.get("as") {
            None | Some(Value::Null) => None,
            Some(value) => Some(normalize_target_path(Path::new(value.as_str().ok_or("as must be a string")?))?),
        };
        // "untitled": keep this document's content but give it no path, as when a crash snapshot of an
        // unsaved document is restored. It reads as modified until it is saved somewhere.
        let untitled = match request.get("untitled") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or("untitled must be a boolean")?,
        };
        if untitled && main.is_some() {
            return Err("open takes either as or untitled, not both".into());
        }
        let mut saved = None;
        let (file_path, file_baseline) = match main {
            Some(main) => {
                let main_bytes = main.to_str().and_then(|main| read_bounded_file(main, 256 * 1024 * 1024, "document").ok());
                saved = Some(main_bytes.as_deref().and_then(|bytes| {
                    let mut parsed = Document::parse(bytes).ok()?;
                    if parsed.is_inx() {
                        resolve_relative_image_paths(&mut parsed.model, &main);
                        prepare_model(&mut parsed.model).ok()?;
                    }
                    // Unprojected CAN3 keys must not make a different backup look saved.
                    if document.is_animation()
                        && document.with_model(parsed.model.clone()).encode().ok().as_deref() != Some(bytes) {
                        return None;
                    }
                    Some((parsed.model.clone(), parsed.tail().to_vec(), document.native_baseline_payloads(&parsed)))
                }));
                (Some(main), main_bytes)
            }
            None if untitled => {
                saved = Some(None);
                (None, None)
            }
            None => (Some(normalized), Some(bytes)),
        };
        self.replace_document(document, revision, file_path, file_baseline);
        if let Some(saved) = saved {
            (self.saved_model, self.saved_tail, self.saved_payloads) = saved.unwrap_or((Value::Null, Vec::new(), None));
        }
        Ok(self.snapshot())
    }

    fn motion3_document(&self, request: &Value) -> Result<&Value, String> {
        let name = motion3_name(request)?;
        if self.document.is_native() {
            return self.native_motions.get(name)
                .ok_or_else(|| format!("Motion3 {name:?} was not found"));
        }
        let document = motion3_registry(&self.document.model)?
            .and_then(|motions| motions.get(name))
            .ok_or_else(|| format!("Motion3 {name:?} was not found"))?;
        motion3::validate(document)?;
        Ok(document)
    }

    fn op_authoring(&mut self, request: &Value) -> Result<Value, String> {
        if request.get("path").is_none() && matches!(request["op"].as_str(),
            Some("live2d.authoring.linear_weights" | "live2d.authoring.primary_forms" | "live2d.authoring.morph_forms")) {
            if !self.document.is_native() {
                return Err("authoring query requires a path or an active Live2D document".into());
            }
            if self.authoring_scene.is_none() {
                let container = caff::CaffDocument::parse(&self.document.encode()?)?;
                self.authoring_scene = Some(authoring_scene::parse_scene(&container)?);
            }
            let mut request = request.clone();
            let mut values = serde_json::to_value(self.effective_native_playback_values()?)
                .map_err(|error| error.to_string())?;
            if let Some(overrides) = request.get("parameterValues") {
                let overrides = overrides.as_object().ok_or("parameterValues must be an object")?;
                values.as_object_mut().unwrap().extend(overrides.clone());
            }
            request["parameterValues"] = values;
            let scene = self.authoring_scene.as_ref().unwrap();
            return match request["op"].as_str() {
                Some("live2d.authoring.linear_weights") => scene.linear_weights(&request),
                Some("live2d.authoring.primary_forms") => scene.primary_forms(&request),
                _ => scene.morph_forms(&request),
            };
        }
        let bytes = if request.get("path").is_some() {
            let path = required_string(request, "path")?;
            let file = File::open(path).map_err(|error| format!("could not open authoring file: {error}"))?;
            if !file.metadata().map_err(|error| error.to_string())?.is_file() {
                return Err("authoring source must be a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes)
                .map_err(|error| format!("could not read authoring file: {error}"))?;
            bytes
        } else {
            if !self.document.is_native() && !self.document.is_animation() {
                return Err("authoring query requires a path or an active Live2D document".into());
            }
            if request["op"] == "live2d.authoring.read" {
                return Ok(self.document.model.clone());
            }
            if request["op"] == "live2d.animation.read" && self.document.is_animation() {
                return Ok(self.document.model.clone());
            }
            self.document.encode()?
        };
        let container = caff::CaffDocument::parse(&bytes)?;
        match request["op"].as_str() {
            Some("live2d.animation.read") => animation_source::read(&container),
            Some("live2d.authoring.read") => authoring_scene::read(&container),
            Some("live2d.authoring.linear_weights") => {
                authoring_scene::linear_weights(&container, request)
            }
            Some("live2d.authoring.primary_forms") => {
                authoring_scene::primary_forms(&container, request)
            }
            Some("live2d.authoring.morph_forms") => {
                authoring_scene::morph_forms(&container, request)
            }
            _ => authoring::inspect(&container),
        }
    }

    fn op_native_animation_attach(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("CAN3 attachment requires an active CMO3 document".into());
        }
        let path = required_string(request, "path")?;
        let bytes = read_bounded_file(path, 256 * 1024 * 1024, "CAN3")?;
        let animation = Document::parse(&bytes)?;
        if !animation.is_animation() {
            return Err("animation.attach path must be a CAN3 container".into());
        }
        let animation_source = animation.model;
        let requested = optional_request_string(request, "resourceGuid")?;
        let resource_guid = native_playback::select_resource(&animation_source, requested)?;
        let source = self.document.model.clone();
        animation_source::bindings(&animation_source, &source, &resource_guid)?;
        let had_preview = self.native_playback.active.is_some();
        self.native_playback.animation = Some(native_playback::AttachedAnimation {
            source: animation_source,
            path: path.to_owned(),
            resource_guid,
        });
        if had_preview { return Ok(self.op_native_animation_reset()); }
        self.native_playback.reset_animation();
        Ok(self.snapshot())
    }

    fn op_native_model_attach(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("model.attach requires an active CAN3 document".into());
        }
        let path = required_string(request, "path")?;
        let requested = optional_request_string(request, "resourceGuid")?;
        let resource_guid = native_playback::select_resource(&self.document.model, requested)?;
        let mut runtime_warnings = Vec::new();
        let scene = if path.to_ascii_lowercase().ends_with(".model3.json") || path.to_ascii_lowercase().ends_with(".moc3") {
            // A runtime model stores IDs only: its scene takes the GUIDs the CAN3's curves use for them.
            let guids = moc3::RuntimeGuids::from_animation(&self.document.model, &resource_guid)?;
            let (scene, warnings) = moc3::attach(path, &guids)?;
            runtime_warnings = warnings;
            scene
        } else {
            let bytes = read_bounded_file(path, 256 * 1024 * 1024, "CMO3")?;
            let model = Document::parse(&bytes)?;
            if !model.is_native() {
                return Err("model.attach path must be a CMO3 container or a .model3.json".into());
            }
            let container = model
                .native_container()
                .ok_or("CMO3 container is missing")?;
            native_scene::NativeScene::parse(container)?
        };
        animation_source::bindings(&self.document.model, scene.source(), &resource_guid)?;
        let parameters = scene.parameters().to_vec();
        let path = path.to_owned();
        let defaults = native_parameter_defaults(&parameters)?;
        self.native_playback.model = Some(native_playback::AttachedModel {
            scene,
            path,
            resource_guid,
        });
        self.native_playback.reset_animation();
        self.native_playback.clear_expressions();
        self.native_runtime_values = defaults;
        self.native_pose = None;
        self.native_physics = None;
        let mut snapshot = self.snapshot();
        if !runtime_warnings.is_empty() {
            snapshot["warnings"] = json!(runtime_warnings);
        }
        Ok(snapshot)
    }

    fn op_native_animation_list(&self) -> Value {
        let animation = self.active_animation_source();
        let model = self.native_model_source();
        let scenes = animation
            .and_then(|source| source["scenes"].as_array())
            .into_iter()
            .flatten()
            .map(|scene| {
                json!({
                    "guid": scene["guid"], "name": scene["name"],
                    "fps": scene["fps"], "durationFrames": scene["durationFrames"]
                })
            })
            .collect::<Vec<_>>();
        let resource_guid = if self.document.is_native() {
            self.native_playback
                .animation
                .as_ref()
                .map(|attachment| attachment.resource_guid.clone())
        } else {
            self.native_playback
                .model
                .as_ref()
                .map(|attachment| attachment.resource_guid.clone())
        };
        let model_path = if self.document.is_native() {
            self.file_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
        } else {
            self.native_playback
                .model
                .as_ref()
                .map(|model| model.path.clone())
        };
        let animation_path = if self.document.is_animation() {
            self.file_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
        } else {
            self.native_playback
                .animation
                .as_ref()
                .map(|animation| animation.path.clone())
        };
        json!({
            "scenes": scenes, "resourceGuid": resource_guid,
            "modelName": model.and_then(|source| source["name"].as_str()),
            "modelPath": model_path, "animationPath": animation_path
        })
    }

    fn op_native_animation_seek(&mut self, request: &Value) -> Result<Value, String> {
        let scene_guid = required_string(request, "sceneGuid")?;
        let frame = request["frame"]
            .as_f64()
            .filter(|frame| frame.is_finite())
            .ok_or("frame must be a finite number")?;
        let resource_guid = self
            .native_resource_guid()
            .ok_or("no CAN3 model resource is selected")?;
        let preview = native_playback::sample(
            self.active_animation_source()
                .ok_or("no CAN3 animation is attached")?,
            self.native_model_source()
                .ok_or("CAN3 playback requires an explicitly attached CMO3")?,
            &resource_guid,
            scene_guid,
            frame,
            &self.native_runtime_values,
        )?;
        let mut physics = self.native_physics.clone();
        let mut pose = self.native_pose.clone();
        if let Some(runtime) = &mut physics { runtime.rewind(); }
        if let Some(runtime) = &mut pose { runtime.reset(); }
        if physics.is_some() || pose.is_some() {
            let elapsed = preview.clock_frame / preview.fps;
            let steps = (elapsed * 60.0).ceil();
            if !steps.is_finite() || steps > 216_000.0 {
                return Err("native runtime seek replay exceeds one hour".into());
            }
            let source = self.native_model_source().unwrap();
            let parameters = source["parameters"].as_array().ok_or("native parameters are missing")?;
            let animation = self.active_animation_source().unwrap();
            let mut previous = 0.0;
            for index in 0..=steps as u64 {
                let time = (index as f64 / 60.0).min(elapsed);
                let sampled = native_playback::sample(animation, source, &resource_guid, scene_guid,
                    time * preview.fps, &self.native_runtime_values)?;
                let values = self.native_playback.effective_values_without_animation(&sampled.values, parameters)?;
                Self::step_native_runtime(&mut physics, &mut pose, values, &sampled.pose_signals, time - previous)?;
                previous = time;
            }
        }
        self.native_playback.active = Some(preview);
        self.native_motion_preview = None;
        self.native_physics = physics;
        self.native_pose = pose;
        Ok(self.snapshot())
    }

    fn op_native_animation_advance(&mut self, request: &Value) -> Result<Value, String> {
        let dt = request["dt"]
            .as_f64()
            .filter(|dt| dt.is_finite() && *dt >= 0.0)
            .ok_or("dt must be a finite nonnegative number")?;
        let looped = request["loop"].as_bool().ok_or("loop must be a boolean")?;
        let current = self
            .native_playback
            .active
            .clone()
            .ok_or("CAN3 playback has no selected scene")?;
        let frame = native_playback::advance_frame(&current, dt, looped)?;
        let preview = native_playback::sample(
            self.active_animation_source().ok_or("no CAN3 animation is attached")?,
            self.native_model_source().ok_or("CAN3 playback requires an attached CMO3")?,
            &self.native_resource_guid().ok_or("no CAN3 resource is selected")?,
            &current.scene_guid, frame, &self.native_runtime_values)?;
        let (physics, pose) = self.ticking_expression(dt, |session| {
            let values = session.native_playback.effective_values_without_animation(&preview.values,
                session.native_model_source().unwrap()["parameters"].as_array().unwrap())?;
            session.native_runtime_step(values, &preview.pose_signals, dt)
        })?;
        self.native_playback.active = Some(preview);
        self.native_motion_preview = None;
        self.native_physics = physics;
        self.native_pose = pose;
        Ok(self.snapshot())
    }

    fn op_native_animation_reset(&mut self) -> Value {
        self.native_playback.reset_animation();
        if let Some(physics) = &mut self.native_physics { physics.rewind(); }
        if let Some(pose) = &mut self.native_pose { pose.reset(); }
        self.reset_native_pose_parameters();
        self.snapshot()
    }

    fn op_native_expression_import(&mut self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let path = required_string(request, "path")?;
        let parameters = self
            .native_model_source()
            .and_then(|source| source["parameters"].as_array())
            .cloned()
            .ok_or("expression import requires an active or attached CMO3")?;
        let bytes = read_bounded_file(path, 16 * 1024 * 1024, "expression")?;
        let result = self
            .native_playback
            .import_expression(name, path, &parameters, &bytes)?;
        Ok(result)
    }

    fn op_native_expression_apply(&mut self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let fades = request.get("fades").map(|value| value.as_bool().ok_or("fades must be a boolean"))
            .transpose()?.unwrap_or(false);
        self.native_playback.apply_expression(name, fades)?;
        Ok(self.snapshot())
    }

    fn op_native_expression_reset(&mut self) -> Value {
        self.native_playback.reset_expression();
        self.snapshot()
    }

    fn op_native_pose_attach(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        if !path.to_ascii_lowercase().ends_with(".pose3.json") {
            return Err("pose path must end in .pose3.json".into());
        }
        let bytes = read_bounded_file(path, 16 * 1024 * 1024, "pose")?;
        let document: Value = serde_json::from_slice(&bytes).map_err(|e| format!("invalid pose JSON: {e}"))?;
        let source = self.native_model_source().ok_or("pose requires an active or attached CMO3")?;
        let candidate = native_pose::Runtime::parse(&document, source)?;
        let previous = self.native_pose.replace(candidate);
        let initialized = if let Some(preview) = &self.native_motion_preview {
            let (name, time, fade) = (preview.name.clone(), preview.time, preview.fade);
            self.seek_motion3(&name, time, fade)
        } else if let Some(preview) = &self.native_playback.active {
            let seek = json!({"sceneGuid":preview.scene_guid,"frame":preview.clock_frame});
            self.op_native_animation_seek(&seek)
        } else {
            self.reset_native_pose_parameters();
            Ok(self.snapshot())
        };
        if initialized.is_err() { self.native_pose = previous; }
        initialized
    }

    fn op_native_physics_attach(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        if !path.ends_with(".physics3.json") { return Err("physics path must end in .physics3.json".into()); }
        let source = self.native_model_source().ok_or("physics requires an active or attached CMO3")?;
        let bytes = read_bounded_file(path, MAX_MOTION_BYTES, "physics")?;
        let document: Value = serde_json::from_slice(&bytes).map_err(|e| format!("invalid physics JSON: {e}"))?;
        let rig = native_physics::Rig::parse(&document, source)?;
        let previous = self.native_physics.replace(native_physics::Runtime::new(rig));
        let initialized = if let Some(preview) = &self.native_motion_preview {
            let (name, time, fade) = (preview.name.clone(), preview.time, preview.fade);
            self.seek_motion3(&name, time, fade)
        } else if let Some(preview) = &self.native_playback.active {
            let seek = json!({"sceneGuid":preview.scene_guid,"frame":preview.clock_frame});
            self.op_native_animation_seek(&seek)
        } else {
            Ok(self.snapshot())
        };
        if initialized.is_err() { self.native_physics = previous; }
        initialized
    }

    fn op_native_physics_reset(&mut self) -> Result<Value, String> {
        self.native_physics.as_mut().ok_or("no native physics is attached")?.reset();
        Ok(self.snapshot())
    }

    fn op_native_physics_options_set(&mut self, request: &Value) -> Result<Value, String> {
        self.native_physics.as_mut().ok_or("no native physics is attached")?.set_options(request)?;
        Ok(self.snapshot())
    }

    fn op_native_physics_stabilize(&mut self) -> Result<Value, String> {
        let mut physics = self.native_physics.clone().ok_or("no native physics is attached")?;
        let values = self.native_physics_inputs()?;
        physics.stabilize(&values)?;
        let mut signals = BTreeMap::new();
        if let Some(preview) = &self.native_playback.active { signals.extend(preview.pose_signals.clone()); }
        if let Some(preview) = &self.native_motion_preview { signals.extend(preview.pose_signals.clone()); }
        let mut pose = self.native_pose.clone();
        if let Some(runtime) = &mut pose { runtime.step(&signals, 0.0)?; }
        self.native_physics = Some(physics);
        self.native_pose = pose;
        Ok(self.snapshot())
    }

    /// Runs `step` with an expression's fade advanced by `dt`, so the step sees the weight after it; a failed step
    /// leaves the fade where it was.
    fn ticking_expression<T>(&mut self, dt: f64, step: impl FnOnce(&Self) -> Result<T, String>)
        -> Result<T, String> {
        let clock = self.native_playback.expression_elapsed;
        self.native_playback.tick_expression(dt);
        step(self).inspect_err(|_| self.native_playback.expression_elapsed = clock)
    }

    fn native_runtime_step(&self, values: BTreeMap<String, f64>, signals: &BTreeMap<String, f64>, dt: f64)
        -> Result<(Option<native_physics::Runtime>, Option<native_pose::Runtime>), String> {
        let mut physics = self.native_physics.clone();
        let mut pose = self.native_pose.clone();
        Self::step_native_runtime(&mut physics, &mut pose, values, signals, dt)?;
        Ok((physics, pose))
    }

    fn step_native_runtime(physics: &mut Option<native_physics::Runtime>, pose: &mut Option<native_pose::Runtime>,
        values: BTreeMap<String, f64>, signals: &BTreeMap<String, f64>, dt: f64) -> Result<(), String> {
        if let Some(runtime) = physics {
            runtime.step(&values, dt)?;
        }
        if let Some(runtime) = pose { runtime.step(signals, dt)?; }
        Ok(())
    }

    fn op_native_physics_step(&mut self, request: &Value) -> Result<Value, String> {
        let dt = request["dt"].as_f64().ok_or("physics dt must be a number")?;
        if self.native_physics.is_none() { return Err("no native physics is attached".into()); }
        if dt == 0.0 { return Ok(self.snapshot()); }
        let signals = self.native_motion_preview.as_ref().map(|p| p.pose_signals.clone())
            .or_else(|| self.native_playback.active.as_ref().map(|p| p.pose_signals.clone())).unwrap_or_default();
        let (physics, pose) = self.ticking_expression(dt, |session| {
            session.native_runtime_step(session.native_physics_inputs()?, &signals, dt)
        })?;
        self.native_physics = physics;
        self.native_pose = pose;
        Ok(self.snapshot())
    }

    fn reset_native_pose_parameters(&mut self) {
        if let Some(pose) = &self.native_pose {
            for (id, value) in &pose.signals {
                if let Some(preview) = &mut self.native_motion_preview {
                    if let Some(current) = preview.pose_signals.get_mut(id) { *current = *value; }
                }
                if let Some(preview) = &mut self.native_playback.active {
                    if let Some(current) = preview.pose_signals.get_mut(id) { *current = *value; }
                }
            }
        }
    }

    fn op_native_pose_reset(&mut self) -> Result<Value, String> {
        self.native_pose.as_mut().ok_or("no native pose is attached")?.reset();
        self.reset_native_pose_parameters();
        Ok(self.snapshot())
    }

    fn op_native_pose_step(&mut self, request: &Value) -> Result<Value, String> {
        let dt = request["dt"].as_f64().ok_or("pose dt must be a number")?;
        let mut candidate = self.native_pose.clone().ok_or("no native pose is attached")?;
        let mut signals = BTreeMap::new();
        if let Some(preview) = &self.native_playback.active { signals.extend(preview.pose_signals.clone()); }
        if let Some(preview) = &self.native_motion_preview { signals.extend(preview.pose_signals.clone()); }
        if let Some(values) = request.get("signals") {
            for (id, value) in values.as_object().ok_or("pose signals must be an object")? {
                if !candidate.signals.contains_key(id) { return Err(format!("unknown pose signal {id:?}")); }
                signals.insert(id.clone(), value.as_f64().filter(|v| v.is_finite())
                    .ok_or("pose signal must be finite")?);
            }
        }
        candidate.step(&signals, dt)?;
        self.native_pose = Some(candidate);
        Ok(self.snapshot())
    }

    fn native_resource_guid(&self) -> Option<String> {
        if self.document.is_native() {
            self.native_playback
                .animation
                .as_ref()
                .map(|animation| animation.resource_guid.clone())
        } else {
            self.native_playback
                .model
                .as_ref()
                .map(|model| model.resource_guid.clone())
        }
    }

    fn op_native_parameter_set(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() && !self.document.is_animation() {
            return Err("parameter operation requires an active Live2D document".into());
        }
        let source = self
            .native_model_source()
            .cloned()
            .ok_or("parameter operation requires an active or attached CMO3")?;
        let id = required_string(request, "parameterId")?;
        let value = request["value"].as_f64().filter(|value| value.is_finite())
            .ok_or("parameter value must be a finite number")?;
        let parameter = source["parameters"].as_array().ok_or("native parameters are missing")?
            .iter()
            .find(|parameter| parameter["id"] == id).ok_or("unknown Live2D parameter ID")?;
        let minimum = parameter["minimum"].as_f64().unwrap();
        let maximum = parameter["maximum"].as_f64().unwrap();
        let repeat = parameter["repeat"] == true;
        if !repeat && (value < minimum || value > maximum) {
            return Err("parameter value is outside its authored range".into());
        }
        self.native_runtime_values.insert(id.to_owned(), native_range_value(value, minimum, maximum, repeat));
        self.native_motion_preview = None;
        self.native_playback.reset_animation();
        if let Some(physics) = &mut self.native_physics { physics.rewind(); }
        Ok(self.snapshot())
    }

    /// A CAN3 key, handle or curve-type edit, checked against the document revision.
    fn op_animation_set_key(
        &mut self,
        request: &Value,
        edit: fn(&Value, &Value) -> Result<Value, String>,
    ) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("key editing requires an active CAN3 document".into());
        }
        let revision = request["expectedRevision"]
            .as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("CAN3 key edit revision conflict".into());
        }
        let candidate = edit(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_curve(&mut self, request: &Value, delete: bool) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("curve editing requires an active CAN3 document".into());
        }
        let revision = request["expectedRevision"]
            .as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("CAN3 curve edit revision conflict".into());
        }
        let candidate = if delete {
            animation_source::delete_curve(&self.document.model, request)?
        } else {
            required_string(request, "modelPath")?;
            let source = self.op_authoring(&json!({
                "op": "live2d.authoring.read",
                "path": request["modelPath"],
            }))?;
            animation_source::add_curve(&self.document.model,
                self.document.animation_initial().ok_or("curve editing requires an active CAN3 source")?,
                request, &source)?
        };
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_scene(&mut self, op: &str, request: &Value) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("scene editing requires an active CAN3 document".into());
        }
        let revision = request["expectedRevision"]
            .as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("CAN3 scene edit revision conflict".into());
        }
        if op == "live2d.animation.scene.delete" {
            let candidate = animation_source::delete_scene(&self.document.model, request)?;
            self.commit_model(candidate)?;
            return Ok(self.snapshot());
        }
        if op == "live2d.animation.scene.set" {
            let candidate = animation_source::set_scene(&self.document.model, request)?;
            self.commit_model(candidate)?;
            return Ok(self.snapshot());
        }
        let guid = animation_source::new_guid()?;
        let candidate = animation_source::add_scene(&self.document.model, request, &guid)?;
        self.commit_model(candidate)?;
        let mut reply = self.snapshot();
        reply["sceneGuid"] = json!(guid);
        Ok(reply)
    }

    fn op_animation_track(&mut self, request: &Value, delete: bool) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("track editing requires an active CAN3 document".into());
        }
        let revision = request["expectedRevision"]
            .as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("CAN3 track edit revision conflict".into());
        }
        let mut track_guid = None;
        let candidate = if delete {
            animation_source::delete_track(&self.document.model, request)?
        } else {
            let guid = animation_source::new_guid()?;
            track_guid = Some(guid.clone());
            animation_source::add_track(&self.document.model, request, &guid)?
        };
        self.commit_model(candidate)?;
        let mut reply = self.snapshot();
        if let Some(guid) = track_guid {
            reply["trackGuid"] = json!(guid);
        }
        Ok(reply)
    }

    fn op_animation_track_duplicate(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("track copying requires an active CAN3 document".into());
        }
        let revision = request["expectedRevision"].as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision { return Err("CAN3 track copy revision conflict".into()); }
        let guid = animation_source::new_guid()?;
        let (candidate, copy) = animation_source::duplicate_track(
            &self.document.model, request, self.document.animation_copies(), &guid)?;
        let original = self.document.clone();
        self.document.add_animation_copy(copy);
        if let Err(error) = self.commit_model(candidate) {
            self.document = original;
            return Err(error);
        }
        let mut reply = self.snapshot();
        reply["trackGuid"] = json!(guid);
        Ok(reply)
    }

    fn op_animation_resource(&mut self, request: &Value, kind: animation_source::ResourceKind) -> Result<Value, String> {
        if !self.document.is_animation() {
            return Err("resource editing requires an active CAN3 document".into());
        }
        let (op, path_key) = match kind {
            animation_source::ResourceKind::Model => ("live2d.animation.resource.add", "modelPath"),
            animation_source::ResourceKind::Image => ("live2d.animation.image_resource.add", "imagePath"),
        };
        animation_source::strict_keys(request, op, &["op", path_key, "expectedRevision"])?;
        let revision = request["expectedRevision"]
            .as_u64()
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("CAN3 resource edit revision conflict".into());
        }
        let path = required_string(request, path_key)?;
        if path.is_empty() {
            return Err(format!("{path_key} must be a nonempty string"));
        }
        if kind == animation_source::ResourceKind::Model {
            self.op_authoring(&json!({"op": "live2d.authoring.read", "path": path}))?;
        }
        let absolute = if kind == animation_source::ResourceKind::Image {
            validated_linked_image(path)?
        } else {
            std::fs::canonicalize(path).map_err(|_| format!("{path_key} cannot be resolved"))?
        };
        let absolute = absolute.to_str().ok_or_else(|| format!("{path_key} is not valid UTF-8"))?;
        let guid = animation_source::new_guid()?;
        let candidate = animation_source::add_resource(&self.document.model, kind, absolute, &guid)?;
        self.commit_model(candidate)?;
        let mut reply = self.snapshot();
        reply["resourceGuid"] = json!(guid);
        Ok(reply)
    }

    fn op_animation_image_relink(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_animation() { return Err("resource editing requires an active CAN3 document".into()); }
        animation_source::strict_keys(request, "live2d.animation.image_resource.relink",
            &["op", "resourceGuid", "imagePath", "expectedRevision"])?;
        let revision = request["expectedRevision"].as_u64().ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision { return Err("CAN3 resource edit revision conflict".into()); }
        let guid = required_string(request, "resourceGuid")?;
        let path = validated_linked_image(required_string(request, "imagePath")?)?;
        let path = path.to_str().ok_or("imagePath is not valid UTF-8")?;
        let candidate = animation_source::relink_image_resource(&self.document.model, guid, path)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_bindings(&mut self, request: &Value) -> Result<Value, String> {
        let resource_guid = required_string(request, "resourceGuid")?;
        let model_path = required_string(request, "modelPath")?;
        if resource_guid.is_empty() {
            return Err("resourceGuid must be a nonempty string".into());
        }
        if model_path.is_empty() {
            return Err("modelPath must be a nonempty string".into());
        }
        match request.get("path") {
            Some(path) if path.as_str().filter(|value| !value.is_empty()).is_none() => {
                return Err("path must be a nonempty string".into());
            }
            _ => {}
        }
        let animation = match request.get("path") {
            None => {
                if !self.document.is_animation() {
                    return Err(
                        "binding query without path requires an active CAN3 document".into(),
                    );
                }
                self.document.model.clone()
            }
            Some(path) => self.op_authoring(&json!({
                "op": "live2d.animation.read",
                "path": path,
            }))?,
        };
        let model = self.op_authoring(&json!({
            "op": "live2d.authoring.read",
            "path": model_path,
        }))?;
        animation_source::bindings(&animation, &model, resource_guid)
    }

    fn op_authoring_set_vertex(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("vertex editing requires an active Live2D document".into());
        }
        let revision = request.get("expectedRevision").and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D vertex edit revision conflict".into());
        }
        let candidate = authoring_edit::set_vertex(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_authoring_rename(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("renaming requires an active Live2D document".into());
        }
        let revision = request.get("expectedRevision").and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D rename revision conflict".into());
        }
        let candidate = authoring_edit::rename(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_authoring_duplicate(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("copying requires an active Live2D document".into());
        }
        if request.get("expectedRevision").and_then(Value::as_u64) != Some(self.revision) {
            return Err("Live2D copy revision conflict".into());
        }
        let (candidate, copy) = authoring_copy::duplicate(
            &self.document.model, request, self.document.copies(), &mut animation_source::new_guid)?;
        let guid = copy.guid().to_owned();
        // A record the model never uses is harmless, so a refused commit leaves it.
        self.document.add_copy(copy);
        self.commit_model(candidate)?;
        let mut reply = self.snapshot();
        reply["guid"] = json!(guid);
        Ok(reply)
    }

    fn op_authoring_user_data(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("user data editing requires an active Live2D document".into());
        }
        let revision = request.get("expectedRevision").and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D user data revision conflict".into());
        }
        let candidate = authoring_edit::set_user_data(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_authoring_edit(&mut self, request: &Value, edit: fn(&Value, &Value) -> Result<Value, String>)
    -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("form editing requires an active Live2D document".into());
        }
        let revision = request.get("expectedRevision").and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D form edit revision conflict".into());
        }
        let candidate = edit(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_brush_replace_image(&mut self, request: &Value) -> Result<Value, String> {
        animation_source::strict_keys(request, "live2d.brush.replace_image", &["op", "guid", "path", "expectedRevision"])?;
        if !self.document.is_native() {
            return Err("image replacement requires an active CMO3 document".into());
        }
        let revision = request["expectedRevision"].as_u64().ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision { return Err("Live2D image replacement revision conflict".into()); }
        let guid = required_string(request, "guid")?;
        let brushes = self.document.model["brushes"].as_array().ok_or("the document has no brushes")?;
        let matching: Vec<_> = brushes.iter().filter(|brush| brush["guid"] == guid).collect();
        let [brush] = matching[..] else { return Err("the brush GUID must identify exactly one brush".into()) };
        let texture = &brush["textureInput"]["texture"];
        let entry = texture["imageEntry"].as_str().ok_or("the brush has no image entry")?.to_owned();
        let dimensions = texture["imageSize"].clone();
        let textures: Vec<_> = self.document.model["textures"].as_array().into_iter().flatten().collect();
        let brush_textures: Vec<_> = brushes.iter().map(|brush| &brush["textureInput"]["texture"]).collect();
        for texture in textures.iter().chain(&brush_textures) {
            if texture["imageEntry"] == entry && texture["imageSize"] != dimensions {
                return Err("shared image references disagree about the image dimensions".into());
            }
        }
        let affected_textures = textures.iter().filter(|texture| texture["imageEntry"] == entry).count();
        let affected_brushes = brush_textures.iter().filter(|texture| texture["imageEntry"] == entry).count();
        let original = self.document.native_entry_data(&entry).ok_or("the brush image entry is missing")?;
        let old_dimensions = model_import::decode_complete_png(&entry, &original)?.dimensions();
        if dimensions != json!([old_dimensions.0, old_dimensions.1]) {
            return Err("the stored image dimensions do not match the PNG".into());
        }
        let bytes = read_bounded_file(required_string(request, "path")?, 256 * 1024 * 1024, "PNG")?;
        let new_dimensions = model_import::decode_complete_png("replacement PNG", &bytes)?.dimensions();
        if new_dimensions != old_dimensions {
            return Err("replacement PNG dimensions must match the existing image".into());
        }
        if bytes != original {
            let mut next = self.document.replacing_native_entry(&entry, bytes)?;
            if let Some(saved) = &self.saved_payloads {
                if saved == &next { next = Arc::clone(saved); }
            }
            let previous = self.document.native_payloads().unwrap();
            self.document.set_native_payloads(next);
            if let Err(error) = self.commit_model_keeping(self.document.model.clone(), true) {
                self.document.set_native_payloads(previous);
                return Err(error);
            }
        }
        let mut reply = self.snapshot();
        reply["imageEntry"] = json!(entry);
        reply["affectedBrushes"] = json!(affected_brushes);
        reply["affectedTextures"] = json!(affected_textures);
        Ok(reply)
    }

    fn op_authoring_set_opacity(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("opacity editing requires an active Live2D document".into());
        }
        let revision = request
            .get("expectedRevision")
            .and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D opacity edit revision conflict".into());
        }
        let candidate = authoring_edit::set_opacity(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    /// `live2d.physics3.read` and `resource://soul2d/document/physics-source`: the typed full-precision view of
    /// the CMO3 physics source. Read-only.
    pub(crate) fn physics_source_view(&self) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("physics reading requires an active Live2D document".into());
        }
        let container = caff::CaffDocument::parse(&self.document.encode()?)?;
        authoring_physics::view(&container, &self.document.model)
    }

    /// `live2d.physics3.edit`: one typed edit of the CMO3 physics source, shared revision and undo/redo.
    fn op_physics_source_edit(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("physics editing requires an active Live2D document".into());
        }
        let revision = request
            .get("expectedRevision")
            .and_then(Value::as_u64)
            .ok_or("expectedRevision must be an unsigned integer")?;
        if revision != self.revision {
            return Err("Live2D physics edit revision conflict".into());
        }
        let container = caff::CaffDocument::parse(&self.document.encode()?)?;
        let candidate = match request.get("preset") {
            Some(name) => {
                let kind = authoring_physics::preset_kind(request)?;
                let name = name.as_str().ok_or("preset must be a string")?;
                let dir = self.preset_dir(kind)?;
                let entry = kind.load(dir)?.remove(name).ok_or_else(|| {
                    format!("no saved {} is named {:?}", kind.noun, name.chars().take(64).collect::<String>())
                })?;
                authoring_physics::apply_preset(&container, &self.document.model, request, &entry)?
            }
            None => authoring_physics::edit(&container, &self.document.model, request)?,
        };
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_motion3_import(&mut self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        if self.native_motions.contains_key(name) ||
            (!self.document.is_native() && motion3_registry(&self.document.model)?
                .is_some_and(|motions| motions.contains_key(name))) {
            return Err(format!("Motion3 {name:?} already exists"));
        }
        let path = required_string(request, "path")?;
        let file = File::open(path).map_err(|error| format!("could not open Motion3: {error}"))?;
        if !file.metadata().map_err(|error| error.to_string())?.is_file() {
            return Err("Motion3 source must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_MOTION_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|error| format!("could not read Motion3: {error}"))?;
        if bytes.len() as u64 > MAX_MOTION_BYTES {
            return Err("Motion3 source exceeds the 16 MiB limit".into());
        }
        let document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid Motion3 JSON: {error}"))?;
        motion3::validate(&document)?;
        if self.document.is_native() {
            self.native_motions.insert(name.to_owned(), document);
            return Ok(json!({"name": name, "storage": "session"}));
        }
        let mut candidate = self.document.model.clone();
        motion3_registry_mut(&mut candidate)?.insert(name.to_owned(), document);
        self.commit_model(candidate)?;
        Ok(json!({"name": name}))
    }

    fn op_motion3_list(&self) -> Result<Value, String> {
        let mut names = Vec::new();
        if self.document.is_native() {
            for (name, document) in &self.native_motions {
                names.push(json!({"name": name, "duration": document["Meta"]["Duration"],
                    "curveCount": document["Meta"]["CurveCount"], "storage": "session"}),
                );
            }
            return Ok(json!({"motions": names}));
        }
        if let Some(motions) = motion3_registry(&self.document.model)? {
            for (name, document) in motions {
                motion3::validate(document)?;
                names.push(json!({"name": name, "duration": document["Meta"]["Duration"],
                    "curveCount": document["Meta"]["CurveCount"]}),
                );
            }
        }
        Ok(json!({"motions": names}))
    }

    fn op_motion3_sample(&self, request: &Value) -> Result<Value, String> {
        let time = finite_number(request.get("time").ok_or("time is required")?, "time")?;
        motion3::sample(self.motion3_document(request)?, time)
    }

    fn op_motion3_set_point(&mut self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        let edited = motion3::set_point(self.motion3_document(request)?, request)?;
        let mut candidate = self.document.model.clone();
        motion3_registry_mut(&mut candidate)?.insert(name.to_owned(), edited);
        self.commit_model(candidate)?;
        Ok(json!({"name": name}))
    }

    /// `motion3.fade.set`: an INX motion is edited as one undo step; a motion imported onto a CMO3 lives only in
    /// this session, like the import itself.
    fn op_motion3_set_fade(&mut self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        let edited = motion3::set_fade(self.motion3_document(request)?, request)?;
        if self.document.is_native() {
            self.native_motions.insert(name.to_owned(), edited);
        } else {
            let mut candidate = self.document.model.clone();
            motion3_registry_mut(&mut candidate)?.insert(name.to_owned(), edited);
            self.commit_model(candidate)?;
        }
        Ok(json!({"name": name}))
    }

    /// `motion3.pose.switch`: keys every part of the attached pose group that holds `part`, shown (1) for `part`
    /// and hidden (0) for the others, at `time`. Like the import, the edit lives only in this session.
    fn op_motion3_pose_switch(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("pose switching needs an active CMO3 document".into());
        }
        let name = motion3_name(request)?;
        let part = required_string(request, "part")?;
        let time = request["time"].as_f64().filter(|time| time.is_finite() && *time >= 0.0)
            .ok_or("time must be a finite nonnegative number of seconds")?;
        let group = self.native_pose.as_ref().ok_or("no native pose is attached")?.group_of(part)
            .ok_or_else(|| format!("part {part:?} is not in a group of the attached pose"))?;
        let mut edited = self.motion3_document(request)?.clone();
        for member in &group {
            edited = motion3::key_part(&edited, member, time, if member == part { 1.0 } else { 0.0 })?;
        }
        self.native_motions.insert(name.to_owned(), edited);
        Ok(json!({"name": name, "parts": group}))
    }

    /// Answers one Live2D runtime JSON document derived from the active CMO3 and, with `path`, creates it as a file.
    fn op_runtime_json_export(
        &self,
        request: &Value,
        derive: fn(&caff::CaffDocument) -> Result<Value, String>,
    ) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err(format!("{} needs an active CMO3 document", self.history_op));
        }
        let document = derive(&caff::CaffDocument::parse(&self.document.encode()?)?)?;
        if request.get("path").is_some() {
            let path = required_string(request, "path")?;
            let target = normalized_export_target(path, Path::new(path), self.file_path.as_deref(), &self.history_op)?;
            let bytes = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
            write_atomic(&target, &bytes, false)?;
        }
        Ok(document)
    }

    /// `live2d.convert.inx`: the active CMO3 as an INX model (issue #146, the approved lossy mode). The answer is
    /// the loss report of `native_convert`; with `path`, the INX is also written there.
    fn op_convert_inx(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_native() {
            return Err("live2d.convert.inx needs an active CMO3 document".into());
        }
        let mut converted = native_convert::to_inx(self.active_native_scene()?)?;
        let mut report = converted.report.clone();
        if request.get("animation").is_some() {
            let can3 = required_string(request, "animation")?;
            let animation = animation_source::read(&caff::CaffDocument::parse(&read_regular_file(can3, "animation")?)?)?;
            let resource = match request.get("resourceGuid") {
                None | Some(Value::Null) => None,
                Some(guid) => Some(guid.as_str().ok_or("resourceGuid must be a string")?),
            };
            let source = self.native_model_source().ok_or("native model source is missing")?;
            let mut model = std::mem::take(&mut converted.model);
            // `to_inx` above filled the scene cache, which lets the scene and the model source be shared borrows.
            let scene = self.native_scene.as_ref().ok_or("native scene cache is missing")?;
            let animations = native_convert::add_can3_animations(&mut model, &animation, source, resource, scene, &converted)?;
            converted.model = model;
            report["animations"] = animations;
        }
        if request.get("path").is_some() {
            let path = required_string(request, "path")?;
            let target = normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "live2d.convert.inx")?;
            validate_model(&converted.model)?;
            let bytes = model_import::inx_bytes(&converted.model, &converted.textures)?;
            write_atomic(&target, &bytes, false)?;
            report["path"] = json!(path);
        }
        Ok(report)
    }

    fn op_motion3_export(&self, request: &Value) -> Result<Value, String> {
        let document = self.motion3_document(request)?;
        let path = required_string(request, "path")?;
        let target = normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "motion3.export")?;
        let checked = match request.get("runtimeModel") {
            None | Some(Value::Null) => None,
            Some(runtime) => {
                let runtime = runtime.as_str().ok_or("runtimeModel must be a path")?;
                Some(moc3::check_motion(&moc3::inspect(&json!({"path": runtime}))?, document)?)
            }
        };
        let bytes = serde_json::to_vec_pretty(document).map_err(|error| error.to_string())?;
        write_atomic(&target, &bytes, false)?;
        Ok(match checked {
            Some(curves) => json!({"path": path, "checkedCurves": curves}),
            None => json!({"path": path}),
        })
    }

    fn op_motion3_bind(&mut self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        let bindings = request.get("bindings").ok_or("bindings is required")?;
        let preserve_unknown = request.get("preserve_unknown")
            .map(|value| value.as_bool().ok_or("preserve_unknown must be a boolean"))
            .transpose()?.unwrap_or(false);
        let candidate = motion_pose::bind(&self.document.model, self.motion3_document(request)?,
            name, bindings, preserve_unknown,
        )?;
        self.commit_model(candidate)?;
        motion_pose::bindings(&self.document.model, name)
    }

    fn motion3_pose(&self, name: &str, time: f64, fade: Option<motion3::FadeClock>)
        -> Result<motion_pose::Pose, String> {
        let document = self.motion3_document(&json!({"name": name}))?;
        motion_pose::pose(&self.document.model, document, name, time, &self.runtime_values, fade)
    }

    fn op_motion3_pose(&self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        let time = finite_number(request.get("time").ok_or("time is required")?, "time")?;
        if self.document.is_native() {
            let pose = self.native_motion_frame(name, time, None)?;
            return Ok(json!({"time": pose.time, "clipEnd": pose.clip_end, "values": pose.values,
                "applied": pose.applied, "unmapped": pose.unmapped}),
            );
        }
        let pose = self.motion3_pose(name, time, None)?;
        Ok(json!({"time": pose.time, "clipEnd": pose.clip_end, "values": values_json(&pose.values),
            "applied": pose.applied, "unmapped": pose.unmapped}),
        )
    }

    fn motion3_frame(&self, name: &str, time: f64, fade: Option<motion3::FadeClock>)
        -> Result<AnimationPreview, String> {
        let pose = self.motion3_pose(name, time, fade)?;
        let deformations = self.pose_deformations(&pose.values)?;
        Ok(AnimationPreview {
            timeline: PreviewTimeline::Motion3 { name: name.to_owned(), time: pose.time,
                clip_end: pose.clip_end, applied: pose.applied, unmapped: pose.unmapped, fade,
            },
            values: pose.values, deformations, forms: form::Sample::default(),
        })
    }

    fn op_motion3_seek(&mut self, request: &Value) -> Result<Value, String> {
        let name = motion3_name(request)?;
        let time = finite_number(request.get("time").ok_or("time is required")?, "time")?;
        let fades = request.get("fades").map(|value| value.as_bool().ok_or("fades must be a boolean"))
            .transpose()?.unwrap_or(false);
        // Playback with fades starts at the seek: the clip fades in from here.
        self.seek_motion3(name, time, fades.then_some(motion3::FadeClock { elapsed: 0.0, looping: false }))
    }

    /// Seeks with `fade`, the clock at `time`. Re-seeks after an edit pass the preview's clock to keep its fades.
    fn seek_motion3(&mut self, name: &str, time: f64, fade: Option<motion3::FadeClock>) -> Result<Value, String> {
        if self.document.is_native() {
            let preview = self.native_motion_frame(name, time, fade)?;
            let mut pose = self.native_pose.clone();
            let mut physics = self.native_physics.clone();
            if let Some(runtime) = &mut pose { runtime.reset(); }
            if let Some(runtime) = &mut physics { runtime.rewind(); }
            if pose.is_some() || physics.is_some() {
                // shortcut: Runtime seeks replay at 60 Hz; use event-aware replay for exact switch timing.
                let steps = (preview.time * 60.0).ceil() as u64;
                if steps > 216_000 { return Err("native runtime seek replay exceeds one hour".into()); }
                let parameters = self.document.model["parameters"].as_array().ok_or("native parameters are missing")?;
                let mut previous = 0.0;
                for index in 0..=steps {
                    let current = (index as f64 / 60.0).min(preview.time);
                    // The clock as it read at `current`: before it started the fade-in holds the motion at zero.
                    // ponytail: replays only the last lap, and a curve without a fade-in (or a PartOpacity pose
                    // signal) plays from clip time 0; store the clip time playback started at if warm-up must be exact.
                    let elapsed = |clock: motion3::FadeClock| clock.elapsed - (preview.time - current);
                    let clock = fade.map(|clock| motion3::FadeClock { elapsed: elapsed(clock), ..clock });
                    let frame = self.native_motion_frame(name, current, clock)?;
                    let values = self.native_playback.effective_values_without_animation(&frame.values, parameters)?;
                    Self::step_native_runtime(&mut physics, &mut pose, values, &frame.pose_signals, current - previous)?;
                    previous = current;
                }
            }
            self.native_motion_preview = Some(preview);
            self.native_pose = pose;
            self.native_physics = physics;
            self.native_playback.reset_animation();
            return Ok(self.snapshot());
        }
        let next = self.motion3_frame(name, time, fade)?;
        self.animation_preview = Some(next);
        self.reset_physics();
        Ok(self.snapshot())
    }

    fn op_motion3_advance(&mut self, request: &Value) -> Result<Value, String> {
        let dt = finite_number(request.get("dt").ok_or("dt is required")?, "dt")?;
        if dt < 0.0 { return Err("motion dt must be nonnegative".into()); }
        let looping = request.get("loop").map(|value| value.as_bool().ok_or("loop must be a boolean"))
            .transpose()?.unwrap_or(false);
        if self.document.is_native() {
            let preview = self.native_motion_preview.as_ref().ok_or("seek a motion before advancing it")?;
            let mut time = preview.time + dt;
            if !time.is_finite() { return Err("motion time is not finite".into()); }
            if looping && preview.clip_end > 0.0 { time %= preview.clip_end; }
            let fade = preview.fade.map(|clock| motion3::FadeClock { elapsed: clock.elapsed + dt, looping });
            let next = self.native_motion_frame(&preview.name, time, fade)?;
            let (physics, pose) = self.ticking_expression(dt, |session| {
                let values = session.native_playback.effective_values_without_animation(&next.values,
                    session.document.model["parameters"].as_array().unwrap())?;
                session.native_runtime_step(values, &next.pose_signals, dt)
            })?;
            self.native_motion_preview = Some(next);
            self.native_playback.reset_animation();
            self.native_physics = physics;
            self.native_pose = pose;
            return Ok(self.snapshot());
        }
        let preview = self.animation_preview.as_ref().ok_or("seek a motion before advancing it")?;
        let PreviewTimeline::Motion3 { name, time, clip_end, fade, .. } = &preview.timeline else {
            return Err("the current preview is not a Motion3 motion".into());
        };
        let mut time = time + dt;
        if !time.is_finite() { return Err("motion time is not finite".into()); }
        if looping && *clip_end > 0.0 { time %= clip_end; }
        let fade = fade.map(|clock| motion3::FadeClock { elapsed: clock.elapsed + dt, looping });
        let next = self.motion3_frame(name, time, fade)?;
        self.animation_preview = Some(next);
        self.physics_output = None;
        Ok(self.snapshot())
    }

    fn op_motion3_reset(&mut self) -> Result<Value, String> {
        if self.document.is_native() {
            self.native_motion_preview = None;
            self.native_playback.reset_animation();
            if let Some(physics) = &mut self.native_physics { physics.rewind(); }
            if let Some(pose) = &mut self.native_pose { pose.reset(); }
            self.reset_native_pose_parameters();
            return Ok(self.snapshot());
        }
        if self.animation_preview.as_ref().is_some_and(|preview| matches!(preview.timeline,
            PreviewTimeline::Animation { .. } | PreviewTimeline::RandomPose(_) | PreviewTimeline::Playlist { .. })) {
            return Err("the current preview is not a Motion3 motion".into());
        }
        self.animation_preview = None;
        self.reset_physics();
        Ok(self.snapshot())
    }

    fn op_node_get(&self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let mut node = nodes::find_node(&self.document.model, id)
            .cloned()
            .ok_or_else(|| format!("node {id} was not found"))?;
        if let Some(offsets) = self.physics_output.as_ref()
            .and_then(|output| output.path_deformations.get(&id))
            .or_else(|| self.effective_deformations().get(&id)) {
            node.as_object_mut()
                .ok_or("node must be an object")?
                .insert("deformation".into(), json!(offsets));
        }
        Ok(node)
    }

    fn op_node_create(&mut self, request: &Value) -> Result<Value, String> {
        let class_name = requested_class(request)?;
        let suffix = request.get("_suffix").and_then(Value::as_str);
        let name = request.get("name").and_then(Value::as_str);
        if request.get("bindings").is_some()
            || request.get("displayOptions").is_some()
            || request.get("templateOptions").is_some()
        {
            return Err("binding edits are not supported in Rust Core mode".into());
        }
        let parent_ids = parent_ids(request, &self.document.model)?;
        let bone = nodes::bone_rest_from_request(request)?;
        let (candidate, created) = nodes::create_nodes(
            &self.document.model,
            &parent_ids,
            class_name,
            suffix,
            name,
            bone.as_ref(),
        )?;
        self.commit_model(candidate)?;
        Ok(json!({ "created": created }))
    }

    fn effective_values(&self) -> &BTreeMap<u32, [f64; 2]> {
        self.physics_output.as_ref().map(|output| &output.parameter_values)
            .or_else(|| {
                self.animation_preview.as_ref().map(|preview| &preview.values)
            })
            .unwrap_or(&self.runtime_values)
    }

    fn effective_native_values(&self) -> &BTreeMap<String, f64> {
        self.native_motion_preview.as_ref().map(|preview| &preview.values)
            .unwrap_or(&self.native_runtime_values)
    }

    fn native_motion_frame(&self, name: &str, time: f64, fade: Option<motion3::FadeClock>)
        -> Result<NativeMotionPreview, String> {
        if !time.is_finite() || time < 0.0 {
            return Err("motion pose time must be finite and nonnegative".into());
        }
        let document = self.motion3_document(&json!({"name": name}))?;
        let clip_end = motion3::end_time(document)?;
        let time = time.min(clip_end);
        let sampled = motion3::sample(document, time)?;
        let weights = fade.map(|clock| motion3::weights(document, time, clip_end, clock)).transpose()?;
        let mut values = self.native_runtime_values.clone();
        let mut applied = Vec::new();
        let mut unmapped = Vec::new();
        let mut pose_signals = BTreeMap::new();
        for (index, curve) in sampled["curves"].as_array().unwrap().iter().enumerate() {
            let id = curve["id"].as_str().unwrap();
            if curve["target"] == "Parameter" && values.contains_key(id) {
                let raw = curve["value"].as_f64().unwrap();
                let parameter = self.document.model["parameters"].as_array().unwrap().iter()
                    .find(|parameter| parameter["id"] == id).unwrap();
                let mut value = native_range_value(raw, parameter["minimum"].as_f64().unwrap(),
                    parameter["maximum"].as_f64().unwrap(), parameter["repeat"] == true);
                let mut row = json!({"target": "Parameter", "id": id, "raw": raw, "parameter": id});
                if let Some(weight) = weights.as_ref().map(|weights| weights[index]) {
                    // The fade blends from the value the parameter holds without the motion.
                    value = values[id] + (value - values[id]) * weight;
                    row["weight"] = json!(weight);
                }
                values.insert(id.to_owned(), value);
                row["value"] = json!(value);
                applied.push(row);
            } else if curve["target"] == "PartOpacity" && self.native_pose.as_ref()
                .is_some_and(|pose| pose.signals.contains_key(id)) {
                let value = curve["value"].as_f64().unwrap();
                pose_signals.insert(id.to_owned(), value);
                applied.push(json!({"target":"PartOpacity","id":id,"value":value,"poseSignal":id}));
            } else {
                unmapped.push(curve.clone());
            }
        }
        Ok(NativeMotionPreview { name: name.to_owned(), time, clip_end, values, fade,
            applied: Value::Array(applied), unmapped: Value::Array(unmapped), pose_signals,
        })
    }

    fn effective_deformations(&self) -> &BTreeMap<u32, Vec<[f64; 2]>> {
        self.animation_preview.as_ref().map(|preview| &preview.deformations)
            .unwrap_or(&self.preview_deformation)
    }

    fn op_animation_edit(&mut self, request: &Value, op: &str) -> Result<Value, String> {
        let candidate = animation::apply(&self.document.model, request, op)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_audio_add(&mut self, request: &Value) -> Result<Value, String> {
        let mut request = request.clone();
        request["path"] = json!(export::wav_file(required_string(&request, "path")?)?);
        self.op_animation_edit(&request, "animation.audio.add")
    }

    fn op_animation_image_add(&mut self, request: &Value) -> Result<Value, String> {
        let mut request = request.clone();
        request["path"] = json!(export::image_file(required_string(&request, "path")?)?.0);
        self.op_animation_edit(&request, "animation.image.add")
    }

    fn op_animation_image_sequence(&mut self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let index = request.get("index").filter(|index| !index.is_null());
        let (directory, prefix, first_number) = if let Some(index) = index {
            let index = index.as_u64().and_then(|index| usize::try_from(index).ok())
                .ok_or("index must be a nonnegative integer")?;
            let track = self.document.model["animations"].get(name)
                .and_then(|animation| animation.get("images"))
                .and_then(Value::as_array).and_then(|images| images.get(index))
                .ok_or("image track index was not found")?;
            let sequence = track.get("sequence").filter(|sequence| sequence.is_object())
                .ok_or("image track has no sequence to reload")?;
            let directory = optional_request_string(request, "directory")?
                .or_else(|| sequence["directory"].as_str()).ok_or("sequence directory is missing")?;
            let prefix = optional_request_string(request, "prefix")?
                .or_else(|| sequence["prefix"].as_str()).ok_or("sequence prefix is missing")?;
            let first = sequence["firstNumber"].as_u64().ok_or("sequence firstNumber is missing")?;
            (directory.to_owned(), prefix.to_owned(), Some(first))
        } else {
            let directory = required_string(request, "directory")?;
            let prefix = required_string(request, "prefix")?;
            (directory.to_owned(), prefix.to_owned(), None)
        };
        let sequence = export::image_sequence(&directory, &prefix, first_number)?;
        let mut request = request.clone();
        request["sequence"] = sequence;
        self.op_animation_edit(&request, "animation.image.sequence")
    }

    fn op_animation_image_read(&self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let frame = finite_number(request.get("frame").ok_or("frame not provided")?, "frame")?;
        let animation = self.document.model["animations"].get(name)
            .ok_or_else(|| format!("animation {name:?} was not found"))?;
        let length = animation::validate_animation_header(animation)?;
        frame_step::validate(&self.document.model, animation, length)?;
        let images = animation.get("images").and_then(Value::as_array).into_iter().flatten()
            .enumerate().filter(|(_, image)| image.get("sequence").is_some_and(Value::is_object))
            .map(|(index, image)| {
                let (path, source_frame) = animation::image_path_at(image, animation, frame)?;
                let gap = path.is_none();
                Ok(json!({
                    "index": index,
                    "path": path,
                    "sourceFrame": source_frame,
                    "gap": gap,
                    "guide": image.get("guide") == Some(&json!(true)),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(json!({"name":name,"frame":frame,"images":images}))
    }

    fn op_animation_frame_step(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = frame_step::set(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_key_copy(&mut self, request: &Value) -> Result<Value, String> {
        let entries = animation::copy_keys(&self.document.model, request)?;
        let keys = animation::clipboard_summaries(&entries);
        let count = entries.len();
        self.animation_clipboard = entries;
        Ok(json!({ "count": count, "keys": keys }))
    }

    /// The values a key records: the effective values, except that while an INX clip is previewed the eye parameters
    /// take their value before the clip's eye blinking, so a key does not bake the blink into the eye lanes.
    fn key_values(&self) -> Result<BTreeMap<u32, [f64; 2]>, String> {
        let mut values = self.effective_values().clone();
        let timeline = self.animation_preview.as_ref().map(|preview| &preview.timeline);
        if let Some(PreviewTimeline::Animation { name, frame, .. }) = timeline {
            let open = animation::sample(&self.document.model, name, *frame, &self.runtime_values, false)?;
            for id in animation::eye_parameters(&self.document.model) {
                if let Some(value) = open.get(&id) {
                    values.insert(id, *value);
                }
            }
        }
        Ok(values)
    }

    fn op_animation_key_add(&mut self, request: &Value) -> Result<Value, String> {
        let parameter_id = required_id(request, "parameter")?;
        let values = self
            .key_values()?
            .get(&parameter_id)
            .copied()
            .ok_or_else(|| format!("parameter {parameter_id} has no runtime value"))?;
        let candidate = animation::add_keys(&self.document.model, request, values)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_key_add_all(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = animation::add_all_keys(&self.document.model, request, &self.key_values()?)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_key_paste(&mut self, request: &Value) -> Result<Value, String> {
        let candidate =
            animation::paste_keys(&self.document.model, request, &self.animation_clipboard)?;
        self.commit_model(candidate)?;
        Ok(self.snapshot())
    }

    fn op_animation_sample(&self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let frame = finite_number(request.get("frame").ok_or("frame not provided")?, "frame")?;
        let values = animation::sample(&self.document.model, name, frame, &self.runtime_values, true)?;
        Ok(json!({"values": values_json(&values)}))
    }

    fn op_animation_seek(&mut self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let frame = finite_number(request.get("frame").ok_or("frame not provided")?, "frame")?;
        self.animation_preview = Some(self.animation_frame(name, frame)?);
        self.reset_physics();
        Ok(self.snapshot())
    }

    fn animation_frame(&self, name: &str, frame: f64) -> Result<AnimationPreview, String> {
        let values = animation::sample(&self.document.model, name, frame, &self.runtime_values, true)?;
        let deformations = self.pose_deformations(&values)?;
        let length = self.document.model["animations"][name]["length"].as_f64()
            .ok_or("animation length is missing")?;
        let timestep = self.document.model["animations"][name]["timestep"].as_f64()
            .ok_or("animation timestep is missing")? as f32;
        let frame = frame.clamp(0.0, length);
        let forms = form::sample(&self.document.model, name, frame)?;
        Ok(AnimationPreview {
            timeline: PreviewTimeline::Animation { name: name.to_owned(), frame, time: frame as f32 * timestep },
            values, deformations, forms,
        })
    }

    fn pose_deformations(&self, values: &BTreeMap<u32, [f64; 2]>) -> Result<BTreeMap<u32, Vec<[f64; 2]>>, String> {
        evaluation::evaluate(&self.document.model, values)?;
        let mut deformations = self.preview_deformation.clone();
        for parameter in values.keys() {
            deformations.extend(dirty::preview_pose(&self.document.model, values, *parameter)?);
        }
        Ok(deformations)
    }

    fn op_animation_advance(&mut self, request: &Value) -> Result<Value, String> {
        let dt = finite_number(request.get("dt").ok_or("dt not provided")?, "dt")?;
        if dt < 0.0 || !(dt as f32).is_finite() {
            return Err("animation dt must be finite, nonnegative, and fit in f32".into());
        }
        let seconds = dt;
        let dt = dt as f32;
        let looping = request.get("loop").map(|value| value.as_bool().ok_or("loop must be a boolean"))
            .transpose()?.unwrap_or(true);
        let preview = self.animation_preview.as_ref().ok_or("seek an animation before advancing it")?;
        if let PreviewTimeline::Playlist { name, time } = &preview.timeline {
            let name = name.clone();
            let duration = playlist::duration(&self.document.model, &name)?;
            let time = if looping && duration > 0.0 { (time + seconds) % duration } else { (time + seconds).min(duration) };
            self.animation_preview = Some(self.playlist_frame(&name, time)?);
            self.physics_output = None;
            return Ok(self.snapshot());
        }
        if let PreviewTimeline::RandomPose(pose) = &preview.timeline {
            // A random pose does not loop: `loop` is accepted for the INX animation it shares this operation with.
            self.animation_preview = Some(self.random_pose_frame(pose.advanced(seconds)?)?);
            self.physics_output = None;
            return Ok(self.snapshot());
        }
        let PreviewTimeline::Animation { name, time: old_time, .. } = &preview.timeline else {
            return Err("the current preview is not an INX animation".into());
        };
        let animation = &self.document.model["animations"][name];
        let length = animation["length"].as_f64().ok_or("animation length is missing")? as f32;
        let timestep = animation["timestep"].as_f64().ok_or("animation timestep is missing")? as f32;
        let marker = |key: &str, fallback: f32| animation.get(key).and_then(Value::as_f64)
            .map(|value| value as f32).filter(|value| *value > 0.0 && *value + 1.0 < length)
            .unwrap_or(fallback);
        let mut time = old_time + dt;
        let mut frame = time / timestep;
        if !time.is_finite() || !frame.is_finite() {
            return Err("animation time is not finite".into());
        }
        if looping && frame >= marker("leadOut", length) {
            time = marker("leadIn", 0.0) * timestep;
            frame = time / timestep;
        }
        if !looping && frame.round() + 1.0 >= length {
            time = (length - 1.0) * timestep;
            frame = time / timestep;
        }
        let mut next = self.animation_frame(name, f64::from(frame))?;
        if let PreviewTimeline::Animation { time: next_time, .. } = &mut next.timeline { *next_time = time; }
        self.animation_preview = Some(next);
        self.physics_output = None;
        Ok(self.snapshot())
    }

    fn op_playlist_seek(&mut self, request: &Value) -> Result<Value, String> {
        let name = required_string(request, "name")?;
        let time = finite_number(request.get("time").ok_or("time not provided")?, "time")?;
        self.animation_preview = Some(self.playlist_frame(name, time)?);
        self.reset_physics();
        Ok(self.snapshot())
    }

    fn playlist_frame(&self, name: &str, time: f64) -> Result<AnimationPreview, String> {
        let (values, time, _) = playlist::sample(&self.document.model, name, time, &self.runtime_values)?;
        let deformations = self.pose_deformations(&values)?;
        Ok(AnimationPreview {
            timeline: PreviewTimeline::Playlist { name: name.to_owned(), time },
            values, deformations, forms: form::Sample::default(),
        })
    }

    fn op_playlist_edit(&mut self, request: &Value, op: &str) -> Result<Value, String> {
        let candidate = playlist::apply(&self.document.model, request, op)?;
        self.commit_model(candidate)?;
        Ok(json!({"playlists": self.document.model.get("playlists"), "sceneGroups": self.document.model.get("sceneGroups")}))
    }

    fn op_random_pose_seek(&mut self, request: &Value) -> Result<Value, String> {
        self.animation_preview = Some(self.random_pose_frame(random_pose::RandomPose::parse(request)?)?);
        self.reset_physics();
        Ok(self.snapshot())
    }

    /// The preview for a random pose: the session values (never changed here) with the moved parameters replaced.
    fn random_pose_frame(&self, pose: random_pose::RandomPose) -> Result<AnimationPreview, String> {
        let values = random_pose::values(&self.document.model, &self.runtime_values, &pose)?;
        let deformations = self.pose_deformations(&values)?;
        Ok(AnimationPreview { timeline: PreviewTimeline::RandomPose(pose), values, deformations, forms: form::Sample::default() })
    }

    fn op_animation_reset(&mut self) -> Result<Value, String> {
        if self.animation_preview.as_ref().is_some_and(|preview| matches!(preview.timeline, PreviewTimeline::Motion3 { .. })) {
            return Err("the current preview is not an INX animation".into());
        }
        self.animation_preview = None;
        self.reset_physics();
        Ok(self.snapshot())
    }

    fn physics_snapshot(&self) -> Value {
        json!({
            "active": self.physics_output.is_some(),
            "pathDeformation": self.physics_output.as_ref()
                .map(|output| preview_json(&output.path_deformations)).unwrap_or(json!({})),
        })
    }

    fn reset_physics(&mut self) {
        self.physics_runtime = physics_runtime::Runtime::default();
        self.physics_output = None;
        if let Some(physics) = &mut self.native_physics { physics.rewind(); }
    }

    fn op_physics_reset(&mut self) -> Value {
        self.reset_physics();
        self.snapshot()
    }

    fn op_physics_step(&mut self, request: &Value) -> Result<Value, String> {
        let dt = request.get("dt").and_then(Value::as_f64)
            .filter(|dt| dt.is_finite() && *dt >= 0.0)
            .ok_or("physics dt must be finite and nonnegative")?;
        let values = self.animation_preview.as_ref().map(|preview| &preview.values)
            .unwrap_or(&self.runtime_values);
        let deformations = self.animation_preview.as_ref().map(|preview| &preview.deformations)
            .unwrap_or(&self.preview_deformation);
        let output = self.physics_runtime.step(&self.document.model, values, deformations, dt)?;
        self.physics_output = Some(output);
        Ok(self.snapshot())
    }

    fn op_physics_set(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, ids) =
            physics::apply_with_preset(&self.document.model, self.config_dir.as_deref(), request)?;
        self.commit_model(candidate)?;
        Ok(json!({ "nodes": ids }))
    }

    /// `physics.preset.list`, and the resource of the same content.
    pub(crate) fn physics_presets(&self) -> Result<Value, String> {
        self.presets_of(&physics::PRESETS)
    }

    fn op_node_insert(&mut self, request: &Value) -> Result<Value, String> {
        let class_name = requested_class(request)?;
        let suffix = request.get("_suffix").and_then(Value::as_str);
        let ids = node_ids(request)?;
        let (candidate, created) =
            nodes::insert_nodes(&self.document.model, &ids, class_name, suffix)?;
        self.commit_model(candidate)?;
        Ok(json!({ "created": created }))
    }

    fn op_define_vertices(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let vertices = number_array(request, "vertices")?;
        let (candidate, applied) =
            deformers::define_vertices(&self.document.model, &ids, &vertices)?;
        self.commit_targets(candidate, &ids)?;
        Ok(json!({ "nodes": applied }))
    }

    fn op_define_grid(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let axis_x = number_array(request, "axisX")?;
        let axis_y = number_array(request, "axisY")?;
        let (candidate, applied) =
            deformers::define_grid(&self.document.model, &ids, &axis_x, &axis_y)?;
        self.commit_targets(candidate, &ids)?;
        Ok(json!({ "nodes": applied }))
    }

    fn op_define_mesh(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let (candidate, applied) = deformers::define_mesh(&self.document.model, &ids, request)?;
        self.commit_model(candidate)?;
        Ok(json!({ "nodes": applied }))
    }

    fn op_mesh_mirror(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = mesh_mirror::apply(&self.document.model, request)?;
        let node = request.get("node").cloned().unwrap_or(Value::Null);
        self.commit_model(candidate)?;
        Ok(json!({ "node": node }))
    }

    fn op_mesh_edit(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, summary) = mesh_edit::edit_part(&self.document.model, self.document.textures(), request)?;
        let id = summary["id"].as_u64().and_then(valid_id).ok_or("invalid id")?;
        self.commit_targets(candidate, &[id])?;
        Ok(summary)
    }

    fn op_mesh_copy(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let clipboard = mesh_edit::copy_part_mesh(&self.document.model, id)?;
        let vertices = clipboard.vertices.len();
        self.mesh_clipboard = Some(clipboard);
        Ok(json!({ "id": id, "vertices": vertices }))
    }

    fn op_mesh_paste(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let clipboard = self.mesh_clipboard.clone().ok_or("mesh clipboard is empty")?;
        let reflect = request.get("reflect").and_then(Value::as_str).unwrap_or("none");
        let (candidate, summary) = mesh_edit::paste_part_mesh(
            &self.document.model,
            self.document.textures(),
            &clipboard,
            id,
            reflect,
        )?;
        self.commit_targets(candidate, &[id])?;
        self.mesh_clipboard = None;
        Ok(summary)
    }

    fn op_deform_edit(&mut self, request: &Value) -> Result<Value, String> {
        if let Some(expected) = request.get("expectedRevision") {
            let expected = expected.as_u64().ok_or("expectedRevision must be an unsigned integer")?;
            if expected != self.revision {
                return Err("deform edit revision conflict".to_owned());
            }
        }
        if request.get("space").is_some_and(|space| !space.is_null() && space != "local") {
            return self.op_deform_edit_canvas(request);
        }
        let (candidate, summary) = mesh_edit::deform_edit(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(summary)
    }

    /// `deform.edit` translate with `space: "canvas"` (issue #182): `delta` is a canvas move of the Part's selected
    /// vertices as drawn at the current parameter values. Each vertex gets the local move that lands it there,
    /// found by Newton steps on a finite-difference Jacobian of local to canvas, so it holds under dynamic grids too.
    fn op_deform_edit_canvas(&mut self, request: &Value) -> Result<Value, String> {
        if request["space"] != "canvas" {
            return Err("space must be local or canvas".into());
        }
        if request["tool"] != "translate" {
            return Err("space canvas works with the translate tool only".into());
        }
        if ["rect", "lasso", "mirror"].iter().any(|key| request.get(*key).is_some_and(|value| !value.is_null())) {
            return Err("space canvas takes selected, not rect, lasso or mirror".into());
        }
        let id = required_id(request, "id")?;
        let delta = required_pair(request, "delta")?;
        let mut selected: Vec<usize> = serde_json::from_value(request.get("selected").cloned().unwrap_or(Value::Null))
            .ok().filter(|selected: &Vec<usize>| !selected.is_empty())
            .ok_or("space canvas needs a non-empty selected list")?;
        selected.sort_unstable();
        selected.dedup();
        let base = self.canvas_vertices(&self.document.model, id)?;
        let at = |positions: &[[f64; 2]], index: usize| positions.get(index).copied()
            .ok_or_else(|| format!("vertex index {index} is past the vertex list"));
        let targets = selected.iter().map(|&index| at(&base, index).map(|p| [p[0] + delta[0], p[1] + delta[1]]))
            .collect::<Result<Vec<_>, _>>()?;
        let scale = targets.iter().flatten().fold(1.0f64, |scale, value| scale.max(value.abs()));
        let (step, tolerance) = (1e-2, 1e-5 * scale);
        // One small request for every step: the caller's (possibly large) selected list is not copied each time.
        let mut base = request.clone();
        if let Some(object) = base.as_object_mut() {
            object.remove("selected");
            object.insert("space".into(), json!("local"));
        }
        let translate = |model: &Value, vertices: &[usize], delta: [f64; 2]| {
            let mut edit = base.clone();
            edit["selected"] = json!(vertices);
            edit["delta"] = json!(delta);
            mesh_edit::deform_edit(model, &edit).map(|(model, _)| model)
        };
        let mut candidate = self.document.model.clone();
        let mut residual = f64::INFINITY;
        for round in 0..=8 {
            let now = self.canvas_vertices(&candidate, id)?;
            let misses: Vec<[f64; 2]> = selected.iter().zip(&targets)
                .map(|(&index, target)| at(&now, index).map(|p| [target[0] - p[0], target[1] - p[1]]))
                .collect::<Result<_, _>>()?;
            residual = misses.iter().map(|miss| miss[0].hypot(miss[1])).fold(0.0, f64::max);
            if residual <= tolerance || round == 8 {
                break;
            }
            let moved_x = self.canvas_vertices(&translate(&candidate, &selected, [step, 0.0])?, id)?;
            let moved_y = self.canvas_vertices(&translate(&candidate, &selected, [0.0, step])?, id)?;
            for ((&index, miss), (dx, dy)) in selected.iter().zip(&misses).zip(selected.iter().map(|&index| {
                let (p, x, y) = (now[index], moved_x[index], moved_y[index]);
                ([(x[0] - p[0]) / step, (x[1] - p[1]) / step], [(y[0] - p[0]) / step, (y[1] - p[1]) / step])
            })) {
                let determinant = dx[0] * dy[1] - dy[0] * dx[1];
                if determinant.abs() < 1e-9 {
                    return Err(format!("vertex {index} does not follow its key at the current parameter values; \
                        set the parameter onto the key first"));
                }
                let local = [(dy[1] * miss[0] - dy[0] * miss[1]) / determinant,
                    (dx[0] * miss[1] - dx[1] * miss[0]) / determinant];
                candidate = translate(&candidate, &[index], local)?;
            }
        }
        if residual > tolerance {
            return Err(format!("the canvas move did not converge (residual {residual}); nothing was changed"));
        }
        if candidate != self.document.model {
            self.commit_model(candidate)?;
        }
        Ok(json!({"id": id, "residual": residual}))
    }

    /// The canvas positions of Part `id`'s vertices as `model` draws them at the current parameter values.
    fn canvas_vertices(&self, model: &Value, id: u32) -> Result<Vec<[f64; 2]>, String> {
        let values = self.effective_values();
        let evaluated = evaluation::evaluate(model, values)?;
        let deformations = evaluation::raster_deformations(model, values)?;
        let scene = scene_deform::evaluate_with_path_update(&evaluated, model.get("nodes").ok_or("model.nodes is missing")?,
            &deformations, &mut |_, _, _, _, _| Ok(None))?;
        let geometry = render::scene_geometry_for_scene(&evaluated, &scene, self.document.textures())?;
        let part = geometry["parts"].as_array().into_iter().flatten().find(|part| part["id"] == id)
            .ok_or_else(|| format!("node {id} is not a drawn Part"))?;
        serde_json::from_value(part["vertices"].clone()).map_err(|error| format!("canvas vertices: {error}"))
    }

    /// Where the host keeps the settings of the user. Without it the session has no presets of the user.
    pub fn set_config_dir(&mut self, dir: PathBuf) {
        self.config_dir = Some(dir);
    }

    fn preset_dir(&self, kind: &preset_store::Kind) -> Result<&Path, String> {
        let missing = || format!("this host has no settings directory to keep {}s in", kind.noun);
        self.config_dir.as_deref().ok_or_else(missing)
    }

    /// The presets of one kind the user saved: the answer of a list operation and of its resource.
    fn presets_of(&self, kind: &preset_store::Kind) -> Result<Value, String> {
        let presets = self.config_dir.as_deref().map(|dir| kind.load(dir)).transpose()?.unwrap_or_default();
        Ok(json!({ "presets": presets }))
    }

    /// `animation.template.list`, and the resource of the same content.
    pub(crate) fn animation_templates(&self) -> Result<Value, String> {
        self.presets_of(&animation::TEMPLATES)
    }

    pub(crate) fn prepare_store_write(&self, request: &Value) -> Result<StoreWritePlan, String> {
        let op = request["op"].as_str().unwrap_or_default();
        let kind = match op {
            "automesh.preset.save" | "automesh.preset.delete" => &automesh::PRESETS,
            "physics.preset.save" | "physics.preset.delete" => &physics::PRESETS,
            "live2d.physics3.preset.save" | "live2d.physics3.preset.delete" => authoring_physics::preset_kind(request)?,
            "animation.template.save" | "animation.template.delete" => &animation::TEMPLATES,
            _ => return Err("not a preset store operation".into()),
        };
        let dir = self.preset_dir(kind)?;
        let renamed = |error: String| match error.strip_prefix("name must") {
            Some(rest) => format!("template must{rest}"),
            None => error,
        };
        match op {
            "animation.template.save" => {
                let saved = animation::template_request(&self.document.model, request)?;
                let mut plan = kind.prepare_save(dir, &saved).map_err(renamed)?;
                let saved = &plan.response;
                plan.response = json!({"template": saved["name"], "entry": saved["preset"], "replaced": saved["replaced"]});
                Ok(plan)
            }
            "animation.template.delete" => {
                let named = json!({"name": request.get("template").cloned().unwrap_or(Value::Null)});
                let mut plan = kind.prepare_delete(dir, &named).map_err(renamed)?;
                plan.response = json!({"template": named["name"]});
                Ok(plan)
            }
            "physics.preset.save" => kind.prepare_save(dir, &physics::preset_request(&self.document.model, request)?),
            "live2d.physics3.preset.save" => {
                let view = match request.get("group") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(self.physics_source_view()?),
                };
                kind.prepare_save(dir, &authoring_physics::preset_request(view.as_ref(), request)?)
            }
            "automesh.preset.save" => kind.prepare_save(dir, request),
            _ => kind.prepare_delete(dir, request),
        }
    }

    fn op_animation_template(&mut self, request: &Value) -> Result<Value, String> {
        let dir = self.preset_dir(&animation::TEMPLATES)?;
        let template = request.get("template").and_then(Value::as_str).ok_or("template must be a string")?;
        let templates = animation::TEMPLATES.load(dir)?;
        let entry = templates.get(template).ok_or_else(|| format!("no saved template is named {template:?}"))?;
        let (candidate, report) = animation::apply_template(&self.document.model, request, entry)?;
        self.commit_model(candidate)?;
        Ok(report)
    }

    /// `live2d.physics3.preset.list`, and the resource of the same content: the presets of the inputs of a physics
    /// group and those of its physical model.
    pub(crate) fn physics3_presets(&self) -> Result<Value, String> {
        let inputs = self.presets_of(&authoring_physics::INPUT_PRESETS)?;
        let vertices = self.presets_of(&authoring_physics::MODEL_PRESETS)?;
        Ok(json!({ "inputs": inputs["presets"], "vertices": vertices["presets"] }))
    }

    /// `automesh.preset.list`, and the resource of the same content.
    pub(crate) fn automesh_presets(&self) -> Result<Value, String> {
        self.presets_of(&automesh::PRESETS)
    }

    fn op_automesh_generate(&mut self, request: &Value) -> Result<Value, String> {
        let request = &automesh::with_user_preset(self.config_dir.as_deref(), request)?;
        let result =
            automesh::generate_for_part(&self.document.model, self.document.textures(), request)?;
        if let Some(candidate) = result.candidate {
            let id = result.summary["id"].as_u64().and_then(valid_id).ok_or("invalid id")?;
            self.commit_targets(candidate, &[id])?;
        }
        Ok(result.summary)
    }

    /// Lists children whose rest mesh or a keyed deform form leaves the axis range of their GridDeformer, and
    /// deformers without children (issue #164).
    pub(crate) fn op_deformer_validate(&self, request: &Value) -> Result<Value, String> {
        let (evaluated, scene) = self.evaluate_scene(request)?;
        let forms = evaluation::deform_key_forms(&self.document.model)?;
        let overhanging = scene_deform::overhanging_children(&evaluated, &scene, &forms)?;
        fn empty(node: &Value, found: &mut Vec<Value>) {
            let children = node.get("children").and_then(Value::as_array);
            let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "GridDeformer" | "PathDeformer" | "MeshGroup") && children.is_none_or(Vec::is_empty) {
                found.push(json!({"id": node.get("uuid"), "name": node.get("name"), "type": kind}));
            }
            for child in children.into_iter().flatten() {
                empty(child, found);
            }
        }
        let mut empty_deformers = Vec::new();
        empty(self.document.model.get("nodes").ok_or("model.nodes is missing")?, &mut empty_deformers);
        Ok(json!({"overhanging": overhanging, "empty": empty_deformers}))
    }

    /// Adds an outer grid line on each side where a child of the GridDeformer leaves it (issue #165). define_grid
    /// carries the keyed offsets over, and the new edge takes the old edge's offsets, so the old area moves as before.
    fn op_deformer_extend_grid(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let (evaluated, scene) = self.evaluate_scene(&json!({}))?;
        let forms = evaluation::deform_key_forms(&self.document.model)?;
        let overhanging = scene_deform::overhanging_children(&evaluated, &scene, &forms)?;
        let mut candidate = self.document.model.clone();
        let mut extended = Vec::new();
        for id in &ids {
            let mut reach = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for entry in overhanging.iter().filter(|entry| entry["deformer"] == *id) {
                let bounds: [f64; 4] = serde_json::from_value(entry["bounds"].clone()).map_err(|error| error.to_string())?;
                reach = [reach[0].min(bounds[0]), reach[1].min(bounds[1]), reach[2].max(bounds[2]), reach[3].max(bounds[3])];
            }
            if let Some(next) = deformers::extend_grid(&candidate, *id, reach)? {
                candidate = next;
                let node = nodes::find_node(&candidate, *id).ok_or("extended grid was not found")?;
                extended.push(json!({"node": id, "axisX": node["grid_axis_x"], "axisY": node["grid_axis_y"]}));
            }
        }
        if !extended.is_empty() {
            self.commit_targets(candidate, &ids)?;
        }
        Ok(json!({ "extended": extended }))
    }

    /// Deletes a GridDeformer after moving its keyed deformation into its children's deform bindings and the
    /// children to its place, keeping their world transforms (issue #167). One undo step.
    fn op_deformer_apply_to_children(&mut self, request: &Value) -> Result<Value, String> {
        let [id] = node_ids(request)?[..] else {
            return Err("deformer.apply_to_children takes one GridDeformer".into());
        };
        let (evaluated, scene) = self.evaluate_scene(&json!({}))?;
        let children = scene_deform::grid_children(&evaluated, &scene, id)?;
        let mut candidate = deformers::transfer_grid_to_children(&self.document.model, id, &children)?;
        let moved: Vec<u32> = children.iter().map(|child| child.id).collect();
        if !moved.is_empty() {
            let parent = nodes::find_parent_id(&candidate, id)?.ok_or("the deformer has no parent")?;
            let index = nodes::find_node(&candidate, parent)
                .and_then(|node| node["children"].as_array()?.iter().position(|child| child["uuid"] == id));
            candidate = nodes::move_nodes(&candidate, &moved, parent, index)?;
        }
        candidate = nodes::delete_nodes(&candidate, &[id])?;
        let marks = dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "deleted": id, "children": moved }))
    }

    /// A Cubism rotation deformer on INX (issue #161): a Node marked `rotationDeformer {handleLength}`, placed at
    /// `pivot` (root space) with angle `angle` (radians, `transform.r.z`), under `parent`, with `children` moved
    /// into it keeping their world transforms. Its children turn about the pivot as the node's angle changes.
    /// Continuous creation is one call per deformer. One undo step.
    fn op_deformer_create_rotation(&mut self, request: &Value) -> Result<Value, String> {
        let pivot = crate::parse_pair(request.get("pivot").ok_or("pivot is required")?, "pivot")?;
        let angle = request.get("angle").map(|value| finite_number(value, "angle")).transpose()?.unwrap_or(0.0);
        let handle = request.get("handleLength").map(|value| finite_number(value, "handleLength")).transpose()?
            .unwrap_or(100.0);
        if handle <= 0.0 {
            return Err("handleLength must be positive".into());
        }
        let root = self.document.model["nodes"]["uuid"].as_u64().and_then(valid_id).ok_or("the model has no root")?;
        let parent = match request.get("parent") {
            Some(value) => value.as_u64().and_then(valid_id).ok_or("parent must be a node UUID")?,
            None => root,
        };
        let children = match request.get("children") {
            Some(_) => node_ids(&json!({"nodes": request["children"]}))?,
            None => Vec::new(),
        };
        let name = request.get("name").and_then(Value::as_str).unwrap_or("Rotation");
        let (mut candidate, created) = nodes::create_nodes(&self.document.model, &[root], "Node", None, Some(name), None)?;
        let id = created.first().and_then(|node| node["uuid"].as_u64()).and_then(valid_id).ok_or("no node was created")?;
        candidate = nodes::transform_node(&candidate, id, &json!({
            "translation": [pivot[0], pivot[1], 0.0], "rotation": [0.0, 0.0, angle]}))?;
        nodes::find_node_mut(&mut candidate, id).ok_or("the created node was not found")?["rotationDeformer"] =
            json!({"handleLength": handle});
        if parent != root {
            candidate = nodes::move_nodes(&candidate, &[id], parent, None)?;
        }
        if !children.is_empty() {
            candidate = nodes::move_nodes(&candidate, &children, id, None)?;
        }
        self.commit_model(candidate)?;
        Ok(json!({"node": id, "children": children}))
    }

    /// Warp Bezier handles (issue #160). `deformer.bezier_controls` reads a GridDeformer's control lattice at
    /// `parameter`'s `key` (the rest grid without them); `deformer.bezier_warp` writes the grid that `controls`
    /// give to that key as one undo step. Nothing is stored besides the key's offsets.
    fn op_deformer_bezier(&mut self, request: &Value, write: bool) -> Result<Value, String> {
        let [id] = node_ids(request)?[..] else {
            return Err("give one GridDeformer in nodes".into());
        };
        let node = nodes::find_node(&self.document.model, id).ok_or_else(|| format!("node {id} was not found"))?;
        if node["type"] != "GridDeformer" {
            return Err(format!("node {id} is not a GridDeformer"));
        }
        let divisions: [usize; 2] = serde_json::from_value(request.get("divisions").cloned().unwrap_or(Value::Null))
            .map_err(|_| "divisions must be [columns, rows] of patches")?;
        if write {
            let controls: Vec<[f64; 2]> = serde_json::from_value(request.get("controls").cloned().unwrap_or(Value::Null))
                .map_err(|_| "controls must be [x, y] points")?;
            let offsets = deformers::bezier_offsets(node, divisions, &controls)?;
            let mut candidate = self.document.model.clone();
            deformers::set_deform(&mut candidate, &json!({"parameter": request.get("parameter"), "nodes": [id],
                "key": request.get("key"), "values": offsets.iter().flatten().collect::<Vec<_>>()}))?;
            self.commit_model(candidate)?;
            return Ok(json!({"node": id, "offsets": offsets}));
        }
        let points = deformers::node_geometry_points(node).len();
        let mut offsets = vec![[0.0, 0.0]; points];
        if let Some(parameter) = request.get("parameter") {
            let parameter = parameter.as_u64().and_then(valid_id).ok_or("parameter must be a UUID")?;
            let key: [usize; 2] = serde_json::from_value(request.get("key").cloned().unwrap_or(Value::Null))
                .map_err(|_| "key must be [x, y]")?;
            let mut model = self.document.model.clone();
            let slot = parameter_edit::locate_plain(&mut model, parameter)?;
            let found = &parameter_edit::container_of(&mut model, slot)?[parameter_edit::slot_index(slot)];
            if let Some(binding) = found["bindings"].as_array().into_iter().flatten()
                .find(|binding| binding["param_name"] == "deform" && binding["node"] == id) {
                let cell: Vec<[f64; 2]> = serde_json::from_value(binding["values"][key[0]][key[1]].clone())
                    .map_err(|_| "the key is outside the binding")?;
                if cell.len() == points {
                    offsets = cell;
                }
            }
        }
        Ok(json!({"node": id, "divisions": divisions, "controls": deformers::bezier_controls(node, divisions, &offsets)?}))
    }

    /// Automatic deformer generation (issue #163) without part recognition: `roles` maps role names of
    /// `deformers::HUMANOID_ROLES` to mesh UUIDs, and the humanoid GridDeformer tree is built under `parent`
    /// (default the root) as one undo step.
    fn op_deformer_generate_humanoid(&mut self, request: &Value) -> Result<Value, String> {
        let roles: BTreeMap<String, Vec<u32>> = serde_json::from_value(request.get("roles").cloned().unwrap_or(Value::Null))
            .map_err(|_| "roles must map role names to arrays of node UUIDs".to_string())?;
        let parent = match request.get("parent") {
            Some(value) => value.as_u64().and_then(valid_id).ok_or("parent must be a node UUID")?,
            None => self.document.model["nodes"]["uuid"].as_u64().and_then(valid_id).ok_or("the model has no root")?,
        };
        let margin = request.get("margin").map(|value| finite_number(value, "margin")).transpose()?.unwrap_or(0.05);
        let divisions = match request.get("divisions") {
            Some(value) => value.as_u64().and_then(|value| usize::try_from(value).ok()).ok_or("divisions must be a count")?,
            None => 4,
        };
        let (candidate, created, not_generated) =
            deformers::generate_humanoid(&self.document.model, parent, &roles, margin, divisions)?;
        self.commit_model(candidate)?;
        Ok(json!({"created": created, "notGenerated": not_generated}))
    }

    /// Sway generation (issue #181, PAR17): three keys on the 1D `parameter`, the middle one the rest grid, written to
    /// each GridDeformer in `nodes` from `deformers::sway_offsets` on top of what its middle key holds, as one undo step. A parameter with another
    /// number of keys is refused unless `replaceKeys`, which sets its keys to 0, 0.5 and 1, as Cubism asks before
    /// it replaces them.
    fn op_deformer_generate_sway(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let parameter = required_id(request, "parameter")?;
        let number = |key: &str| request.get(key).map(|value| finite_number(value, key)).transpose();
        let fulcrum = match request.get("fulcrum") {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| "fulcrum must be top, bottom, left or right".to_string())?,
            None => deformers::Fulcrum::Top,
        };
        let sway = deformers::Sway {
            fulcrum,
            horizontal: number("horizontal")?,
            vertical: number("vertical")?.unwrap_or(0.0),
            softness: number("softness")?.unwrap_or(1.0),
            zoom: number("zoom")?.unwrap_or(0.0),
            flip: flip_flag(request)?,
        };
        let generated = self.key_grids_three(request, parameter, &ids, |node| {
            let (horizontal, keys) = deformers::sway_offsets(node, &sway)?;
            Ok((json!({"horizontal": horizontal}), keys))
        })?;
        Ok(json!({"parameter": parameter, "nodes": generated}))
    }

    fn op_deformer_generate_face_turn(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let parameter = required_id(request, "parameter")?;
        let horizontal = match request.get("axis").and_then(Value::as_str) {
            Some("x") => true,
            Some("y") => false,
            _ => return Err("axis must be x or y".into()),
        };
        let amount = request.get("amount").map(|value| finite_number(value, "amount")).transpose()?.unwrap_or(0.3);
        let flip = flip_flag(request)?;
        let generated = self.key_grids_three(request, parameter, &ids, |node| {
            Ok((json!({"amount": amount}), deformers::turn_offsets(node, horizontal, amount, flip)?))
        })?;
        Ok(json!({"parameter": parameter, "axis": if horizontal { "x" } else { "y" }, "nodes": generated}))
    }

    /// Writes low, middle and high keys of 1D `parameter` on each GridDeformer from `generate`, in one undo. The
    /// parameter is cut to three keys when `replaceKeys` is set; the middle key keeps what it holds and the low and
    /// high offsets are added on top of it.
    fn key_grids_three(
        &mut self,
        request: &Value,
        parameter: u32,
        ids: &[u32],
        generate: impl Fn(&Value) -> Result<(Value, [Vec<[f64; 2]>; 2]), String>,
    ) -> Result<Vec<Value>, String> {
        let mut candidate = self.document.model.clone();
        let found = find_parameter(&candidate, parameter).ok_or_else(|| format!("parameter {parameter} was not found"))?;
        if found["is_vec2"] == true {
            return Err("generation needs a 1D parameter".into());
        }
        let axes: [Vec<f64>; 2] = serde_json::from_value(found["axis_points"].clone())
            .map_err(|_| format!("parameter {parameter} has no axis points"))?;
        // The rest grid goes on the middle key, which must sit at the parameter's default.
        let (min, max) = (parse_pair(&found["min"], "min")?, parse_pair(&found["max"], "max")?);
        let rest_at = (parse_pair(&found["defaults"], "defaults")?[0] - min[0]) / (max[0] - min[0]);
        if !(rest_at > 0.0 && rest_at < 1.0) {
            return Err(format!("parameter {parameter} needs its default strictly between its minimum and maximum"));
        }
        if axes[0].len() != 3 || (axes[0][1] - rest_at).abs() > 1e-9 {
            if request.get("replaceKeys").and_then(Value::as_bool) != Some(true) {
                return Err(format!("parameter {parameter} needs three keys with the middle one at its default; \
                    pass replaceKeys to replace its {} keys", axes[0].len()));
            }
            parameter_edit::set_axes(&mut candidate, parameter, min, max, &[0.0, rest_at, 1.0], &axes[1])?;
        }
        let mut generated = Vec::new();
        for &id in ids {
            let node = nodes::find_node(&candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
            if node["type"] != "GridDeformer" {
                return Err(format!("node {id} is not a GridDeformer"));
            }
            let (mut summary, [mut low, mut high]) = generate(node)?;
            let rest = find_parameter(&candidate, parameter)
                .and_then(|found| found["bindings"].as_array())
                .and_then(|bindings| bindings.iter().find(|b| b["param_name"] == "deform" && b["node"] == id))
                .filter(|binding| binding["isSet"][1][0] == true)
                .and_then(|binding| serde_json::from_value::<Vec<[f64; 2]>>(binding["values"][1][0].clone()).ok())
                .filter(|rest| rest.len() == low.len())
                .unwrap_or_else(|| vec![[0.0, 0.0]; low.len()]);
            for (offsets, base) in [&mut low, &mut high].into_iter().flat_map(|key| key.iter_mut().zip(&rest)) {
                *offsets = [offsets[0] + base[0], offsets[1] + base[1]];
            }
            for (key, offsets) in [(0, &low), (1, &rest), (2, &high)] {
                deformers::set_deform(&mut candidate, &json!({"parameter": parameter, "nodes": [id], "key": [key, 0],
                    "values": offsets.iter().flatten().collect::<Vec<_>>()}))?;
            }
            summary["node"] = json!(id);
            generated.push(summary);
        }
        self.commit_model(candidate)?;
        Ok(generated)
    }

    fn op_deformer_set(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let property = required_string(request, "property")?;
        let value = request.get("value").ok_or("value is required")?;
        let candidate = deformers::set_property(&self.document.model, &ids, property, value)?;
        self.commit_model(candidate)?;
        Ok(json!({ "nodes": ids, "property": property }))
    }

    fn op_depth_list(&self, request: &Value) -> Result<Value, String> {
        depths::list_depths(&self.document.model, request)
    }

    fn op_depth_set(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depths::set_depths(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depths::list_depths(&self.document.model, request)
    }

    fn op_depth_clear(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depths::clear_depths(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depths::list_depths(&self.document.model, request)
    }

    fn op_depth_ops_list(&self, request: &Value) -> Result<Value, String> {
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_set(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::set_ops(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_add(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::add_op(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_update(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::update_op(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_remove(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::remove_op(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_move(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::move_op(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_clear(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::clear_ops(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depth_ops::list_ops(&self.document.model, request)
    }

    fn op_depth_ops_apply(&mut self, request: &Value) -> Result<Value, String> {
        let candidate = depth_ops::apply_ops(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        depths::list_depths(&self.document.model, request)
    }

    /// `depth.edit.apply`: the depth editor's Apply as ONE undo step (D `applyToTargets`). Nothing to apply
    /// is not an error and leaves history alone.
    fn op_depth_edit_apply(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, plan) = depth_ops::edit_apply(&self.document.model, request)?;
        self.commit_targets(candidate, &depth_targets(request))?;
        let mut listed = depth_ops::list_ops(&self.document.model, request)?;
        listed["depths"] = depths::list_depths(&self.document.model, request)?["depths"].clone();
        listed["plan"] = json!(plan);
        Ok(listed)
    }

    fn op_import_psd(&mut self, request: &Value) -> Result<Value, String> {
        let posed = self.posed_model()?;
        let imported = psd_depth::import_psd(
            &self.document.model,
            &posed,
            self.document.textures(),
            request,
        )?;
        self.commit_targets(imported.model, &imported.changed_grids)?;
        Ok(imported.report)
    }

    /// `depth.import_psd.plan`: the layer table, matching, per-target results, budget and D's diagnostics of
    /// the import `request` would run. Read-only.
    fn op_import_psd_plan(&mut self, request: &Value) -> Result<Value, String> {
        let posed = self.posed_model()?;
        psd_depth::plan_psd(&self.document.model, &posed, self.document.textures(), request)
    }

    /// `import.psd` / `import.kra`: a new document from a layered file, as D's `incImport!PSD` and
    /// `incImport!KRA`. Replaces the session exactly like `new` does (the editor refuses it while the
    /// current document has unsaved changes).
    fn op_import_model(&mut self, request: &Value, format: LayerFile) -> Result<Value, String> {
        let bytes = read_bounded_file(required_string(request, "path")?, format.max_bytes(), format.label())?;
        let options = model_import::ImportOptions::from_request(request, 1)?;
        let limits = psd::Limits::default();
        let imported = match format {
            LayerFile::Psd => model_import::import_psd(&bytes, &options, &limits),
            LayerFile::Kra => model_import::import_kra(&bytes, &options, &limits),
        }?;
        let mut document = Document::parse(&model_import::inx_bytes(&imported.model, &imported.textures)?)?;
        prepare_model(&mut document.model)?;
        if format == LayerFile::Psd {
            link_psd(&mut document.model, required_string(request, "path")?);
        }
        let nodes = count_nodes(&document.model["nodes"]).saturating_sub(1);
        let revision = self.revision.saturating_add(1);
        self.replace_document(document, revision, None, None);
        let mut snapshot = self.snapshot();
        snapshot["imported"] = json!({"nodes": nodes, "textures": imported.textures.len()});
        for (key, value) in imported.report {
            snapshot["imported"][key] = value;
        }
        Ok(snapshot)
    }

    /// Reads the layered file of a merge request and matches its layers to the open document's Parts.
    fn merge_plan(&self, request: &Value, format: LayerFile) -> Result<(model_import::MergePlan, String), String> {
        if !self.document.is_inx() {
            return Err(format!("{} merge needs an INP/INX document", format.label()));
        }
        let flag = |key: &str| match request.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be a boolean")),
        };
        let options = model_import::MergeOptions {
            rename_mapped: flag("renameMapped")?,
            retranslate_mapped: flag("retranslateMapped")?,
            first_id: 1,
        };
        // Without a path a PSD merge reads the PSD the document is linked to (issue #150).
        let path = match (optional_request_string(request, "path")?, format) {
            (Some(path), _) => path,
            (None, LayerFile::Psd) => self.document.model["meta"]["psdSource"].as_str()
                .ok_or("path is required: the document is not linked to a PSD")?,
            (None, LayerFile::Kra) => required_string(request, "path")?,
        };
        let bytes = read_bounded_file(path, format.max_bytes(), format.label())?;
        let (model, textures, limits) = (&self.document.model, self.model_textures(), psd::Limits::default());
        let plan = match format {
            LayerFile::Psd => model_import::merge_psd(model, &textures, &bytes, &options, &limits),
            LayerFile::Kra => model_import::merge_kra(model, &textures, &bytes, &options, &limits),
        }?;
        Ok((plan, path.to_owned()))
    }

    /// Parts linked to a layer (a stored `psdLayerPath`) whose layer is not in the file, by path or by name. Only a
    /// merge of the PSD the document is linked to (or of a first PSD into an unlinked one) can tell: Parts from
    /// another file are not in this one by design.
    fn missing_layers(&self, plan: &model_import::MergePlan, format: LayerFile, path: &str) -> Vec<(u32, String)> {
        let linked = self.document.model["meta"]["psdSource"].as_str();
        if format != LayerFile::Psd || linked.is_some_and(|linked| linked != absolute_path(path)) {
            return Vec::new();
        }
        let present = |stored: &str| plan.layers.iter().any(|layer| layer.path == stored
            || model_import::base_name(&layer.path) == model_import::base_name(stored));
        plan.unmapped_parts.iter().filter_map(|&id| {
            let stored = nodes::find_node(&self.document.model, id)?["psdLayerPath"].as_str()
                .filter(|stored| !stored.is_empty() && !present(stored))?;
            Some((id, stored.to_owned()))
        }).collect()
    }

    fn model_textures(&self) -> Vec<model_import::Texture> {
        self.document
            .textures()
            .iter()
            .map(|blob| model_import::Texture { tag: blob.tag, data: blob.data.clone() })
            .collect()
    }

    /// `merge.psd.plan` / `merge.kra.plan`: which Part each layer would replace by default. Read-only.
    fn op_merge_plan(&self, request: &Value, format: LayerFile) -> Result<Value, String> {
        let (plan, path) = self.merge_plan(request, format)?;
        let mut result = plan.to_json();
        result["source"] = json!(path);
        result["missingLayers"] = json!(self.missing_layers(&plan, format, &path).iter()
            .map(|(id, path)| json!({"part": id, "path": path})).collect::<Vec<_>>());
        Ok(result)
    }

    /// `merge.psd.apply` / `merge.kra.apply`: replace or add Parts from the layers in one undo step. The
    /// history state keeps the texture sections too, so undo and redo bring the old and new textures back
    /// with the model.
    fn op_merge_apply(&mut self, request: &Value, format: LayerFile) -> Result<Value, String> {
        let removed = match request.get("removed").map(Value::as_str) {
            None | Some(Some("keep")) => "keep",
            Some(Some(mode @ ("hide" | "remove"))) if format == LayerFile::Psd => mode,
            Some(Some("hide" | "remove")) => return Err("removed works on a PSD merge only".into()),
            _ => return Err("removed must be keep, hide or remove".into()),
        };
        let (plan, path) = self.merge_plan(request, format)?;
        let choices = plan.choices_from_request(request)?;
        let mut merged = model_import::apply_merge(&self.document.model, &self.model_textures(), &plan, &choices)?;
        // Parts whose layer left the file, unless a choice put a layer into them.
        let gone: Vec<u32> = self.missing_layers(&plan, format, &path).into_iter().map(|(id, _)| id)
            .filter(|id| !merged.replaced.contains(id)).collect();
        // A Part that has children is hidden rather than removed, so the children stay.
        let has_children = |model: &Value, id: u32| nodes::find_node(model, id)
            .is_some_and(|node| node["children"].as_array().is_some_and(|children| !children.is_empty()));
        let (hidden, deleted): (Vec<u32>, Vec<u32>) = match removed {
            "hide" => (gone.clone(), Vec::new()),
            "remove" => gone.iter().copied().partition(|id| has_children(&merged.model, *id)),
            _ => (Vec::new(), Vec::new()),
        };
        for id in &hidden {
            if let Some(node) = nodes::find_node_mut(&mut merged.model, *id) {
                node["enabled"] = json!(false);
            }
        }
        if !deleted.is_empty() {
            merged.model = nodes::delete_nodes(&merged.model, &deleted)?;
        }
        // A merge links an unlinked document; relinking a linked one to another file takes `link`.
        let relink = request.get("link").map(|link| link.as_bool().ok_or("link must be a boolean")).transpose()?;
        if format == LayerFile::Psd && relink.unwrap_or(merged.model["meta"]["psdSource"].is_null()) {
            link_psd(&mut merged.model, &path);
        }
        let blobs = merged
            .textures
            .iter()
            .map(|texture| document::TextureBlob { tag: texture.tag, data: texture.data.clone() })
            .collect::<Vec<_>>();
        let textures = blobs.len();
        let result = json!({"created": merged.created, "replaced": merged.replaced, "textures": textures,
            "missingLayers": gone, "removed": removed});
        if merged.model == self.document.model && blobs == self.document.textures() {
            return Ok(result);
        }
        let sections = self.document.sections_with_textures(blobs)?;
        self.commit_with_sections(merged.model, sections)?;
        Ok(result)
    }

    /// Records one undo step that also swaps the document's texture and extension sections. `true` keeps
    /// a step even when only the sections changed.
    fn commit_with_sections(&mut self, model: Value, sections: Arc<document::InxSections>) -> Result<(), String> {
        let previous = self.document.inx_sections();
        self.document.set_inx_sections(sections);
        if let Err(error) = self.commit_model_keeping(model, true) {
            if let Some(previous) = previous {
                self.document.set_inx_sections(previous);
            }
            return Err(error);
        }
        Ok(())
    }

    /// `atlas.build`: where D's INP export would put each Part's textures, computed on the session's
    /// current meshes and textures without changing the document.
    fn op_atlas_build(&self, request: &Value) -> Result<Value, String> {
        if !self.document.is_inx() {
            return Err("atlas.build needs an INP/INX document".into());
        }
        let settings = atlas_settings(request)?;
        let wanted = optional_ids(request, "parts")?;
        let (_, scene) = self.evaluate_scene(&json!({}))?;
        let plan = atlas_inputs(&self.document.model, self.document.textures(), &scene, &wanted)?;
        let pins = atlas::pins(&self.document.model)?;
        let parts = plan.parts.iter().map(|part| atlas::AtlasPart {
            textures: part.slots.map(|slot| slot.map(|index| &plan.images[index])),
            uvs: &part.uvs,
            bounds: part.bounds,
            pin: pins.get(&part.id).copied(),
        }).collect::<Vec<_>>();
        let mut budget = cv::WorkBudget::new(ATLAS_WORK_BUDGET);
        let build = atlas::build_atlases(&parts, &settings, &mut budget)?;
        // The packing preview: one page's albedo, at most 512 pixels a side, as a PNG.
        let preview = match request.get("preview") {
            None | Some(Value::Null) => None,
            Some(page) => {
                let page = page.as_u64().and_then(|page| usize::try_from(page).ok())
                    .filter(|page| *page < build.atlases.len()).ok_or("preview must be the index of an atlas")?;
                let albedo = build.atlases[page].slots[0].as_ref().ok_or("the atlas has no albedo page")?;
                let [width, height] = albedo.size();
                let full = image::RgbaImage::from_raw(width, height, albedo.pixels().to_vec())
                    .ok_or("the atlas page has the wrong size")?;
                let side = width.min(512);
                let small = image::imageops::resize(&full, side, side, image::imageops::FilterType::Triangle);
                let mut png = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(small).write_to(&mut png, image::ImageFormat::Png)
                    .map_err(|error| format!("could not encode the preview: {error}"))?;
                Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png.into_inner()))
            }
        };
        let placed = plan.parts.iter().zip(&build.parts).map(|(part, placement)| json!({
            "id": part.id,
            "atlas": placement.atlas,
            "mapping": placement.mapping,
            "uvs": placement.uvs.iter().map(|uv| [f64::from(uv[0]), f64::from(uv[1])]).collect::<Vec<_>>(),
        })).collect::<Vec<_>>();
        Ok(json!({
            "resolution": settings.resolution,
            "atlasCount": build.atlases.len(),
            "outputScale": f64::from(build.output_scale),
            "parts": placed,
            "skipped": plan.skipped,
            "preview": preview,
        }))
    }

    fn op_add_skeleton(&mut self, request: &Value) -> Result<Value, String> {
        // D's AddStandardDepthSkeleton takes root and scale only, and D never stored display or template options.
        if request.get("bindings").is_some()
            || request.get("displayOptions").is_some()
            || request.get("templateOptions").is_some()
        {
            return Err("rig.add_skeleton takes only root and scale; add bindings with rig.add_source".into());
        }
        let root = required_id(request, "root")?;
        let scale = match request.get("scale") {
            None | Some(Value::Null) => 1.0,
            Some(value) => finite_number(value, "scale")?,
        };
        let scale = skeleton::normalize_scale(scale)?;
        let posed = self.posed_model()?;
        let candidate = skeleton::add_standard_skeleton(&self.document.model, &posed, root, scale)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        skeleton::list_bones(&self.document.model, root)
    }

    fn posed_model(&self) -> Result<Value, String> {
        let has_parameters = self
            .document
            .model
            .get("param")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
        if !has_parameters && self.runtime_values.is_empty() {
            return Ok(self.document.model.clone());
        }
        evaluation::evaluate(&self.document.model, &self.runtime_values)
    }

    fn op_fit_human(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let posed = self.posed_model()?;
        let candidate = skeleton::fit_human(&self.document.model, &posed, root, &self.runtime_values)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(json!({ "root": root }))
    }

    fn op_add_standard_parameters(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let candidate = standard_params::add(&self.document.model, root)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(self.resource_parameters())
    }

    fn op_list_bones(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        skeleton::list_bones(&self.document.model, root)
    }

    fn op_overlay(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let selected = match request.get("selected") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(valid_id)
                    .ok_or("selected must be a bone UUID")?,
            ),
        };
        let posed = self.posed_model()?;
        bone_deform::overlay_geometry(&posed, root, selected)
    }

    fn op_pick_bone(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let point = request.get("point").ok_or("point is required")?;
        let radius = finite_number(request.get("radius").ok_or("radius is required")?, "radius")?;
        bone_deform::pick_bone(&self.posed_model()?, root, point, radius)
    }

    fn op_drag_bone(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let bone = required_id(request, "bone")?;
        bone_deform::require_rig_bone(&self.document.model, root, bone)?;
        let (translation_axes, rotation_axes) = drag_axes(request);
        let original = nodes::find_node(&self.document.model, bone)
            .ok_or_else(|| format!("node {bone} was not found"))?;
        let base_translation = bone_deform::transform_vec(original, "trans", [0.0, 0.0, 0.0]);
        let base_rotation = bone_deform::transform_vec(original, "rot", [0.0, 0.0, 0.0]);
        let mut candidate = if let Some(component) = request.get("component").and_then(Value::as_str) {
            let value = finite_number(
                request.get("value").ok_or("value is required with component")?,
                "value",
            )?;
            nodes::apply_transform_component(&self.document.model, bone, component, value)?
        } else {
            nodes::transform_node(&self.document.model, bone, request)?
        };
        bone_deform::apply_drag_pose(
            &mut candidate,
            root,
            bone,
            &self.runtime_values,
            translation_axes,
            rotation_axes,
            base_translation,
            base_rotation,
        )?;
        let result = nodes::find_node(&candidate, bone)
            .cloned()
            .ok_or_else(|| format!("node {bone} was not found"))?;
        if transform_changes_trs(request) {
            let marks: Vec<_> = dirty::mark_bone(&candidate, bone).into_iter().collect();
            if marks.is_empty() {
                self.commit_model(candidate)?;
            } else {
                self.commit_marked(candidate, marks)?;
            }
        } else {
            self.commit_model(candidate)?;
        }
        Ok(result)
    }

    fn op_solve_ik(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let effector = required_id(request, "endEffector")?;
        let target = request.get("target").ok_or("target is required")?;
        let (candidate, body) =
            bone_deform::solve_ik(&self.document.model, &self.runtime_values, root, effector, target)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(body)
    }

    /// `rig.apply_mmd_pose`: writes a VPD pose, or a VMD sampled at `frame`, into one keypoint of `parameter`
    /// (`key`, default the keypoint closest to the parameter's current value) through the `binding.set_trs` write and
    /// the same keypoint deform refresh, as one undo step. `dryRun` returns the report and changes nothing.
    fn op_apply_mmd_pose(&mut self, request: &Value) -> Result<Value, String> {
        use mmd_motion::apply::{plan, rig_bones, Pose};
        let root = required_id(request, "root")?;
        let parameter = required_id(request, "parameter")?;
        let optional = |key: &str| request.get(key).filter(|value| !value.is_null());
        let dry_run = dry_run_flag(request)?;
        let scale = translation_scale(request)?;
        let path = required_string(request, "path")?;
        let extension = Path::new(path).extension().and_then(|extension| extension.to_str());
        let is_vmd = match extension.map(str::to_ascii_lowercase).as_deref() {
            Some("vpd") => false,
            Some("vmd") => true,
            _ => return Err("path must name a .vpd or .vmd file".into()),
        };
        let frame = match (is_vmd, optional("frame")) {
            (false, None) => None,
            (false, Some(_)) => return Err("frame applies to a .vmd file only".into()),
            (true, None) => return Err("frame is required for a .vmd file".into()),
            (true, Some(value)) => Some(
                value
                    .as_f64()
                    .filter(|frame| (0.0..=f64::from(mmd_motion::MAX_FRAME)).contains(frame))
                    .ok_or("frame must be a number from 0 to 1000000")?,
            ),
        };
        bone_deform::require_rig_root(&self.document.model, root)?;
        let key = match optional("key") {
            None => bone_deform::closest_keypoint(&self.document.model, &self.runtime_values, parameter)?,
            Some(_) => key_index(request)?,
        };
        let (table, table_source, sections) = self.mmd_bone_table(request)?;
        let mut candidate = self.document.model.clone();
        evaluation::set_pose_cells(&mut candidate, parameter, key, &[])?;
        let pose = if is_vmd {
            let bytes = read_bounded_file(path, mmd_motion::MAX_VMD_BYTES as u64, "VMD")?;
            Pose::from_vmd(&mmd_motion::parse_vmd(&bytes)?, frame.unwrap_or_default(), &table)
        } else {
            let bytes = read_bounded_file(path, mmd_motion::MAX_VPD_BYTES as u64, "VPD")?;
            Pose::from_vpd(&mmd_motion::parse_vpd(&bytes)?, &table)
        };
        let mut plan = plan(&pose, &rig_bones(&candidate, root)?, scale);
        let bindings = find_parameter(&candidate, parameter).map(|parameter| parameter["bindings"].clone()).unwrap_or_default();
        let owned = |bone: u32, channel: &str| {
            bindings.as_array().is_some_and(|all| all.iter().any(|b| b["node"] == bone && b["param_name"] == channel))
        };
        plan.drop_unowned_resets(owned);
        // A zero needs no binding: a channel this parameter does not drive is zero already, and may be another's.
        let mut cells = plan.cells();
        cells.retain(|&(bone, channel, value)| value != 0.0 || owned(bone, channel));
        evaluation::set_pose_cells(&mut candidate, parameter, key, &cells)?;
        let mut report = plan.report(&pose, root, parameter, key);
        report["boneTable"] = json!(table_source);
        if !dry_run {
            let marks = dirty::mark_pose(&candidate, parameter, &plan.bones(), key);
            self.commit_marked_with_sections(candidate, marks, sections)?;
        }
        self.mmd_report = report.clone();
        Ok(report)
    }

    /// The bone table of an MMD operation and where it came from: the `boneTable` argument (`"argument"`), else the
    /// table stored with the document (`"document"`), else none (`"default"`). With `storeBoneTable` also the
    /// sections that hold the argument as the stored table (an empty table removes it), when that changes them.
    fn mmd_bone_table(
        &self,
        request: &Value,
    ) -> Result<(mmd_motion::BoneTable, &'static str, Option<Arc<document::InxSections>>), String> {
        let store = match request.get("storeBoneTable").filter(|value| !value.is_null()) {
            None => false,
            Some(value) => value.as_bool().ok_or("storeBoneTable must be a boolean")?,
        };
        let is_key = |entry: &(Vec<u8>, Vec<u8>)| entry.0 == MMD_BONE_TABLE_KEY.as_bytes();
        let Some(argument) = request.get("boneTable").filter(|value| !value.is_null()) else {
            if store {
                return Err("storeBoneTable needs boneTable".into());
            }
            let stored = if self.document.is_inx() { self.document.extension_entries()? } else { Vec::new() };
            let Some((_, payload)) = stored.into_iter().find(is_key) else {
                return Ok((mmd_motion::BoneTable::default(), "default", None));
            };
            // The stored table is file content: bounded and checked like the argument.
            let table = Some(payload)
                .filter(|payload| payload.len() <= MAX_MMD_BONE_TABLE_BYTES)
                .and_then(|payload| serde_json::from_slice::<Value>(&payload).ok())
                .ok_or_else(|| "the bone table stored with the document is too large or not JSON".to_string())
                .and_then(|value| mmd_motion::BoneTable::parse(&value))
                .map_err(|error| format!("{error} (stored with the document; pass boneTable to replace it)"))?;
            return Ok((table, "document", None));
        };
        let table = mmd_motion::BoneTable::parse(argument)?;
        if !store {
            return Ok((table, "argument", None));
        }
        let old = self.document.extension_entries()?;
        let mut entries = old.clone();
        let payload = match argument.as_object().is_some_and(|entries| !entries.is_empty()) {
            true => Some(serde_json::to_vec(argument).map_err(|error| error.to_string())?),
            false => None,
        };
        match (entries.iter().position(is_key), payload) {
            (Some(at), Some(payload)) => entries[at].1 = payload,
            (Some(at), None) => {
                entries.remove(at);
            }
            (None, Some(_)) if entries.len() >= MAX_EXT_ENTRIES => {
                return Err(format!("the document already has {MAX_EXT_ENTRIES} extension entries"));
            }
            (None, Some(payload)) => entries.push((MMD_BONE_TABLE_KEY.as_bytes().to_vec(), payload)),
            (None, None) => {}
        }
        let sections = if entries == old { None } else { Some(self.document.sections_with_extensions(&entries)?) };
        Ok((table, "argument", sections))
    }

    /// What `rig.import_gltf` and `mesh.import_gltf` ask of a request alike.
    fn gltf_import(&self, request: &Value, op: &str) -> Result<GltfImport, String> {
        let root = required_id(request, "root")?;
        let path = required_string(request, "path")?;
        let flag = |key: &str| match request.get(key).filter(|value| !value.is_null()) {
            None => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be a boolean")),
        };
        let (lossy, acknowledged, dry_run) = (flag("lossy")?, flag("acknowledgeRights")?, dry_run_flag(request)?);
        let units = request
            .get("unitsPerSourceUnit")
            .filter(|value| !value.is_null())
            .map(|value| value.as_f64().filter(|units| units.is_finite() && *units > 0.0))
            .map(|units| units.ok_or("unitsPerSourceUnit must be a finite positive number"))
            .transpose()?;
        let extension = Path::new(path).extension().and_then(|extension| extension.to_str());
        if !matches!(extension.map(str::to_ascii_lowercase).as_deref(), Some("gltf" | "glb" | "vrm")) {
            return Err("path must name a .gltf, .glb or .vrm file".into());
        }
        if !self.document.is_inx() {
            return Err(format!("{op} needs an INP/INX document"));
        }
        bone_deform::require_rig_root(&self.document.model, root)?;
        let file = File::open(path).map_err(|error| format!("could not open glTF file: {error}"))?;
        if !file.metadata().map_err(|error| error.to_string())?.is_file() {
            return Err("glTF source must be a regular file".into());
        }
        Ok(GltfImport { root, file, units, lossy, acknowledged, dry_run })
    }

    /// `rig.import_gltf`: adds the standard skeleton below an empty DepthRigRoot at the rest positions of the humanoid
    /// in a glTF 2.0 or VRM file, and keeps what the file states about rights in the extension section, as one undo
    /// step. Answers the loss report of `doc/3d-conversion-loss-map.md`. Nothing is written with `dryRun`, or while
    /// `needs` names an argument the caller has to give.
    fn op_import_gltf_rig(&mut self, request: &Value) -> Result<Value, String> {
        let GltfImport { root, file, units, lossy, acknowledged, dry_run } =
            self.gltf_import(request, "rig.import_gltf")?;
        if !bone_deform::bone_order(&self.document.model, root)?.is_empty() {
            return Err("the DepthRigRoot already has bones; fitting an existing skeleton is not implemented".into());
        }
        let imported = gltf_rig::skeleton(&gltf_rig::read_json(file)?)?;
        let mut needs = Vec::new();
        if units.is_none() {
            needs.push("unitsPerSourceUnit");
        }
        if imported.lossy() && !lossy {
            needs.push("lossy");
        }
        if imported.rights.is_some() && !acknowledged {
            needs.push("acknowledgeRights");
        }
        let mut answer = imported.report();
        answer["lossy"] = json!(lossy);
        answer["applied"] = json!(false);
        answer["needs"] = json!(needs);
        let Some(units) = units.filter(|_| needs.is_empty()) else {
            return Ok(answer);
        };
        // A dry run goes through every check before the commit.
        let scaled = |point: [f64; 3]| point.map(|axis| axis * units);
        // Half the range, so that the offset between two bones, which a bone stores, is finite too.
        let finite = |point: &[f64; 3]| scaled(*point).iter().all(|axis| axis.abs() <= f64::MAX / 2.0);
        if !imported.rests.values().all(|(head, tail)| finite(head) && finite(tail))
            || !imported.extra.iter().all(|(_, point)| finite(point))
        {
            return Err("unitsPerSourceUnit puts a bone outside the finite range".into());
        }
        if imported.rests.values().any(|(head, tail)| scaled(*head) == scaled(*tail)) {
            return Err("unitsPerSourceUnit leaves a bone without length".into());
        }
        let candidate = skeleton::add_skeleton_with_rests(&self.document.model, root, |name| {
            let (head, tail) = imported.rests[name];
            (scaled(head), scaled(tail))
        })?;
        let old = self.document.extension_entries()?;
        let mut entries = old.clone();
        // Kept rights are never replaced: what an earlier import kept may state other rights. The humanoid entry
        // describes the last import and is replaced.
        let mut replaced = false;
        let mut keep = |key: &str, value: &Value, replace: bool| -> Result<(), String> {
            let payload = serde_json::to_vec(value).map_err(|error| error.to_string())?;
            if payload.len() > MAX_EXT_PAYLOAD_BYTES {
                return Err(format!("{key} is too large to keep"));
            }
            match entries.iter().position(|entry| entry.0 == key.as_bytes()) {
                Some(at) if entries[at].1 == payload => {}
                Some(at) if replace => {
                    entries[at].1 = payload;
                    replaced = true;
                }
                Some(_) => return Err(format!("the document already keeps other data under {key}")),
                None if entries.len() >= MAX_EXT_ENTRIES => {
                    return Err(format!("the document already has {MAX_EXT_ENTRIES} extension entries"));
                }
                None => entries.push((key.as_bytes().to_vec(), payload)),
            }
            Ok(())
        };
        if let Some((key, rights)) = &imported.rights {
            keep(key, rights, false)?;
        }
        if !imported.extra.is_empty() {
            let bones: serde_json::Map<String, Value> =
                imported.extra.iter().map(|(name, point)| (name.clone(), json!(scaled(*point)))).collect();
            keep(gltf_rig::VRM_HUMANOID_KEY, &json!({ "unitsPerSourceUnit": units, "bones": bones }), true)?;
        }
        if replaced {
            let reason = "the humanoid bones an earlier import kept are replaced";
            let item = json!({ "feature": "skeleton.bone", "source": "", "outcome": "dropped", "reason": reason });
            answer["items"].as_array_mut().ok_or("the report has no items")?.push(item);
            answer["counts"]["dropped"] = json!(answer["counts"]["dropped"].as_u64().unwrap_or(0) + 1);
        }
        let sections = if entries == old { None } else { Some(self.document.sections_with_extensions(&entries)?) };
        let bones = skeleton::list_bones(&candidate, root)?["items"].take();
        if dry_run {
            return Ok(answer);
        }
        self.commit_marked_with_sections(candidate, vec![dirty::mark_root(root)], sections)?;
        answer["applied"] = json!(true);
        answer["unitsPerSourceUnit"] = json!(units);
        answer["bones"] = bones;
        self.rig_import_report = answer.clone();
        Ok(answer)
    }

    /// `mesh.import_gltf`: the mesh import prototype of issue #244. Each triangle primitive of the file becomes a
    /// Part below the document root, seen from the front, with a depth for each vertex. A skinned one is bound to
    /// the bones `rig.import_gltf` added below `root`. Answers the loss report of
    /// `doc/3d-conversion-loss-map.md`. Nothing changes with `dryRun`, while `needs` names an argument, or when the
    /// file has no primitive to import.
    fn op_import_gltf_mesh(&mut self, request: &Value) -> Result<Value, String> {
        let GltfImport { root, file, units, lossy, acknowledged, dry_run } =
            self.gltf_import(request, "mesh.import_gltf")?;
        let (text, bin) = gltf_mesh::read_file(file)?;
        let skeleton = gltf_rig::skeleton(&text)?;
        if let Some((key, rights)) = &skeleton.rights {
            // Rights are never dropped: the import that keeps them comes first, and no mesh is read before.
            let payload = serde_json::to_vec(rights).map_err(|error| error.to_string())?;
            let kept = self.document.extension_entries()?;
            if !kept.iter().any(|entry| entry.0 == key.as_bytes() && entry.1 == payload) {
                return Err("the document does not keep the rights this file states; rig.import_gltf keeps them".into());
            }
        }
        let mut imported = gltf_mesh::meshes(&text, bin, &skeleton)?;
        if let Some((key, _)) = &skeleton.rights {
            imported.report.add("rights", "", "kept", &format!("extdata:{key}"));
        }
        let bones = skeleton::bones_by_name(&self.document.model, root)?;
        let mut bound = imported.parts.iter().flat_map(|part| part.weights.keys());
        if let Some(bone) = bound.find(|bone| !bones.contains_key(**bone)) {
            return Err(format!("the DepthRigRoot has no bone {bone}; rig.import_gltf adds the bones of the file"));
        }
        let mut needs = Vec::new();
        if units.is_none() {
            needs.push("unitsPerSourceUnit");
        }
        if imported.report.lossy() && !lossy {
            needs.push("lossy");
        }
        if skeleton.rights.is_some() && !acknowledged {
            needs.push("acknowledgeRights");
        }
        // The source and the rights are those of the rig import. The items are those of the meshes.
        let mut answer = skeleton.report();
        let rights_cut = answer["truncated"].as_array().is_some_and(|cut| cut.contains(&json!("rights")));
        let fields = imported.report.fields(rights_cut);
        for key in ["counts", "items", "truncated"] {
            answer[key] = fields[key].clone();
        }
        answer["lossy"] = json!(lossy);
        answer["applied"] = json!(false);
        answer["needs"] = json!(needs);
        let Some(units) = units.filter(|_| needs.is_empty() && !imported.parts.is_empty()) else {
            return Ok(answer);
        };
        // A mesh lies on its bones only when they are the bones of this file at this scale.
        for (bone, (head, _)) in &skeleton.rests {
            let Some(id) = bones.get(*bone) else {
                continue;
            };
            let stored = nodes::find_node(&self.document.model, *id).map(|bone| &bone["restHead"]);
            let near = |axis: usize, rest: f64| (rest - head[axis] * units).abs() <= 1e-3 * units;
            let at = |axis: usize| stored.and_then(|stored| stored[axis].as_f64());
            if (0..3).any(|axis| !at(axis).is_some_and(|rest| near(axis, rest))) {
                return Err("the bones below root are not where this file and unitsPerSourceUnit put them".into());
            }
        }
        let parent = self.document.model["nodes"]["uuid"].as_u64().and_then(valid_id);
        let parent = parent.ok_or("the document has no root node")?;
        // A Part goes below the document root, so its vertices are moved to where the DepthRigRoot puts its bones.
        let world = |id: u32| bone_deform::world_matrix(&self.document.model, id);
        let frame = bone_deform::mul_mat(bone_deform::invert(world(parent)?)?, world(root)?);
        // A depth is the z of the rig, so the rig must keep the picture plane.
        let plane = [(0, 2, 0.0), (1, 2, 0.0), (2, 0, 0.0), (2, 1, 0.0), (2, 2, 1.0), (2, 3, 0.0)];
        if plane.iter().any(|(row, column, kept)| (frame[*row][*column] - kept).abs() > 1e-9) {
            return Err("the DepthRigRoot is moved or turned out of the picture plane, where no mesh is put".into());
        }
        let first_slot = self.document.textures().len();
        let mut candidate = self.document.model.clone();
        let mut made = Vec::new();
        for part in &imported.parts {
            let points: Vec<[f64; 3]> = part.vertices.iter().map(|point| point.map(|axis| axis * units)).collect();
            let placed = points.iter().map(|point| bone_deform::transform_point(frame, [point[0], point[1], 0.0]));
            let placed = placed.collect::<Result<Vec<[f64; 3]>, String>>()?;
            // The renderer refuses a vertex beyond this.
            if !points.iter().chain(&placed).flatten().all(|axis| axis.abs() <= 1e9) {
                return Err("unitsPerSourceUnit puts a vertex outside the range a Part can hold".into());
            }
            let flat = |pairs: &mut dyn Iterator<Item = [f64; 2]>| pairs.flatten().collect::<Vec<f64>>();
            // The factor of the file is linear, and a Part tint is in the encoding of its texture.
            let encoded = |linear: f64| {
                if linear <= 0.003_130_8 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                }
            };
            let node = json!({
                "name": gltf_rig::label(&part.name, 64), "type": "Part", "enabled": true,
                // The larger zsort is drawn first, so the nearer Part gets the smaller one.
                "zsort": -points.iter().map(|point| point[2]).sum::<f64>() / points.len() as f64,
                "transform": { "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0] },
                "lockToRoot": false, "pinToMesh": false, "children": [],
                "mesh": {
                    "verts": flat(&mut placed.iter().map(|point| [point[0], point[1]])),
                    "uvs": flat(&mut part.uvs.iter().copied()),
                    "indices": part.indices,
                    "origin": [0.0, 0.0],
                },
                "textures": [first_slot + part.image], "blend_mode": "Normal",
                "tint": [encoded(part.factor[0]), encoded(part.factor[1]), encoded(part.factor[2])],
                "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5,
                "opacity": part.factor[3],
            });
            let id = nodes::add_built_node(&mut candidate, parent, node)?;
            // The candidate is changed in place: a copy of the document for each bone of each Part takes minutes.
            for bone in part.weights.keys() {
                bindings::add_source_in(&mut candidate, root, id, bones[*bone])?;
            }
            if !part.weights.is_empty() {
                let rows = part.weights.iter().map(|(bone, values)| json!({ "bone": bones[*bone], "values": values }));
                bindings::set_vertex_weights_in(&mut candidate, root, id, &json!(rows.collect::<Vec<_>>()))?;
            }
            made.push((id, points));
        }
        // A stored depth is a share of the size the rig scales depth by, and the new Parts are part of that size.
        let depth_scale = bone_deform::depth_world_scale(&candidate, &candidate, root)?;
        for (id, points) in &made {
            let depths: Vec<f64> = points.iter().map(|point| point[2] / depth_scale).collect();
            depths::store_depths(&mut candidate, *id, &depths)?;
        }
        validate_model(&candidate)?;
        let parts = made.iter().zip(&imported.parts).map(|((id, points), part)| {
            json!({
                "id": id, "name": gltf_rig::label(&part.name, 64), "vertices": points.len(),
                "triangles": part.indices.len() / 3, "bones": part.weights.len(),
            })
        });
        let parts: Vec<Value> = parts.collect();
        // One mark for the rig, as binding a bone gives: a new target changes the size the rig scales the depth of
        // every target by. The refresh poses the document once for a mark, or once for each key of each parameter
        // that poses the rig while the targets of the key fit one call.
        let bound = imported.parts.iter().any(|part| !part.weights.is_empty());
        let marks = if bound { vec![dirty::mark_root(root)] } else { Vec::new() };
        // A dry run goes through every check before the commit.
        let sections = if imported.images.is_empty() {
            None
        } else {
            let mut blobs = self.document.textures().to_vec();
            blobs.extend(imported.images.into_iter().map(|data| document::TextureBlob { tag: 0, data }));
            Some(self.document.sections_with_textures(blobs)?)
        };
        if dry_run {
            return Ok(answer);
        }
        self.commit_marked_with_sections(candidate, marks, sections)?;
        answer["applied"] = json!(true);
        answer["unitsPerSourceUnit"] = json!(units);
        answer["depthScale"] = json!(depth_scale);
        answer["parts"] = json!(parts);
        self.mesh_import_report = answer.clone();
        Ok(answer)
    }

    /// The loss report of the last `mesh.import_gltf` that changed a document in this session, or null.
    pub(crate) fn mesh_import_report(&self) -> &Value {
        &self.mesh_import_report
    }

    /// `export.gltf`: writes the document as layered planes in glTF 2.0, a `.glb` or a `.gltf` with its buffer
    /// inside. Answers the loss report of `doc/3d-conversion-loss-map.md`. Nothing is written with `dryRun`, or
    /// while `needs` names an argument the caller has to give.
    fn op_export_gltf(&mut self, request: &Value) -> Result<Value, String> {
        let prepared = self.prepare_gltf_export(request)?;
        self.execute_gltf_export(prepared)
    }

    pub(crate) fn prepare_gltf_export(&self, request: &Value) -> Result<export_ops::PreparedGltfExport, String> {
        if !self.document.is_inx() {
            return Err("export.gltf needs an INP/INX document".into());
        }
        let path = required_string(request, "path")?;
        let flag = |key: &str| match request.get(key).filter(|value| !value.is_null()) {
            None => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be a boolean")),
        };
        let (overwrite, lossy, acknowledged) = (flag("overwrite")?, flag("lossy")?, flag("acknowledgeRights")?);
        let dry_run = dry_run_flag(request)?;
        let units = request
            .get("unitsPerMetre")
            .filter(|value| !value.is_null())
            .map(|value| value.as_f64().filter(|units| units.is_finite() && *units > 0.0))
            .map(|units| units.ok_or("unitsPerMetre must be a finite positive number"))
            .transpose()?;
        if path.ends_with(['/', std::path::MAIN_SEPARATOR]) {
            return Err("path must name a file".into());
        }
        let extension = Path::new(path).extension().and_then(|extension| extension.to_str());
        let binary = match extension.map(str::to_ascii_lowercase).as_deref() {
            Some("glb") => true,
            Some("gltf") => false,
            _ => return Err("path must name a .gltf or .glb file".into()),
        };
        let target = normalize_target_path(Path::new(path))?;
        if self.file_path.as_deref() == Some(target.as_path()) {
            return Err("export.gltf would overwrite the open document".into());
        }
        if !overwrite && target.exists() {
            return Err("export destination already exists".into());
        }
        // Without a scale the report is the one of a file at one canvas unit a metre, which is not written.
        let kept = self.document.extension_entries()?;
        let model = &self.document.model;
        let export = gltf_export::export(model, self.document.textures(), &kept, units.unwrap_or(1.0), binary)?;
        let mut needs = Vec::new();
        if units.is_none() {
            needs.push("unitsPerMetre");
        }
        if !lossy {
            needs.push("lossy");
        }
        if export.rights && !acknowledged {
            needs.push("acknowledgeRights");
        }
        let mut answer = export.report;
        answer["lossy"] = json!(lossy);
        answer["applied"] = json!(false);
        answer["needs"] = json!(needs);
        if !needs.is_empty() || dry_run {
            return Ok(export_ops::PreparedGltfExport::Report(answer));
        }
        Ok(export_ops::PreparedGltfExport::Write(export::GltfPlan {
            target, target_alias: PathBuf::from(path), overwrite, report: answer,
            bytes: export.bytes.into(), units: units.unwrap(),
        }))
    }

    pub(crate) fn execute_gltf_export(&mut self, prepared: export_ops::PreparedGltfExport) -> Result<Value, String> {
        let plan = match prepared {
            export_ops::PreparedGltfExport::Report(answer) => return Ok(answer),
            export_ops::PreparedGltfExport::Write(plan) => plan,
        };
        if normalize_target_path(&plan.target_alias)? != plan.target {
            return Err("glTF export destination changed after preparation".into());
        }
        if self.file_path.as_deref() == Some(plan.target.as_path()) {
            return Err("export.gltf would overwrite the open document".into());
        }
        if !plan.overwrite && plan.target.exists() { return Err("export destination already exists".into()) }
        write_atomic(&plan.target, &plan.bytes, plan.overwrite)?;
        let mut answer = plan.report;
        answer["applied"] = json!(true);
        answer["path"] = json!(plan.target.to_string_lossy());
        answer["bytes"] = json!(plan.bytes.len());
        answer["unitsPerMetre"] = json!(plan.units);
        self.gltf_export_report = answer.clone();
        Ok(answer)
    }

    /// The loss report of the last `export.gltf` that wrote a file in this session, or null.
    pub(crate) fn gltf_export_report(&self) -> &Value {
        &self.gltf_export_report
    }

    /// The loss report of the last `rig.import_gltf` that changed a document in this session, or null.
    pub(crate) fn rig_import_report(&self) -> &Value {
        &self.rig_import_report
    }

    /// The report of the last `rig.apply_mmd_pose` or `rig.bake_mmd_motion` in this session, or null.
    pub(crate) fn mmd_report(&self) -> &Value {
        &self.mmd_report
    }

    /// `rig.bake_mmd_motion`: samples a VMD at `frames` and writes sample `i` into keypoint `i` along `axis` of
    /// `parameter` (the other axis stays at `other`), with the writes of `rig.apply_mmd_pose`, as one undo step.
    /// `animation` also creates or replaces an animation whose lane moves the parameter through those keypoints at the
    /// motion's timing. `dryRun` returns the report and changes nothing.
    fn op_bake_mmd_motion(&mut self, request: &Value) -> Result<Value, String> {
        use mmd_motion::apply::{plan, rig_bones, Pose};
        let root = required_id(request, "root")?;
        let parameter_id = required_id(request, "parameter")?;
        let optional = |key: &str| request.get(key).filter(|value| !value.is_null());
        let dry_run = dry_run_flag(request)?;
        let scale = translation_scale(request)?;
        let path = required_string(request, "path")?;
        let is_vmd = Path::new(path).extension().and_then(|extension| extension.to_str());
        if !is_vmd.is_some_and(|extension| extension.eq_ignore_ascii_case("vmd")) {
            return Err("path must name a .vmd file".into());
        }
        let frames = mmd_motion::bake::parse_frames(request.get("frames").ok_or("frames is required")?)?;
        let axis = match optional("axis") {
            None => 0,
            Some(value) => value.as_u64().filter(|axis| *axis < 2).ok_or("axis must be 0 or 1")? as usize,
        };
        let other = match optional("other") {
            None => None,
            Some(value) => Some(
                value.as_u64().and_then(|index| usize::try_from(index).ok()).ok_or("other must be a keypoint index")?,
            ),
        };
        let animation_name = match optional("animation") {
            None => None,
            Some(value) => {
                Some(value.as_str().filter(|name| !name.trim().is_empty()).ok_or("animation must be a non-empty string")?)
            }
        };
        bone_deform::require_rig_root(&self.document.model, root)?;
        let parameter = find_parameter(&self.document.model, parameter_id)
            .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
        if parameter.get("groupUUID").is_some() {
            return Err(format!("parameter {parameter_id} refers to a parameter group"));
        }
        let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
        if !is_vec2 && (axis != 0 || other.is_some()) {
            return Err("a 1D parameter has axis 0 and no other".into());
        }
        let values = evaluation::parameter_key_values(parameter)?;
        let other = other.unwrap_or(0);
        if other >= values[1 - axis].len() {
            return Err("other must identify an existing parameter keypoint index".into());
        }
        if frames.len() > values[axis].len() {
            return Err(format!(
                "parameter has {} keypoints on axis {axis}, {} frames were requested",
                values[axis].len(),
                frames.len()
            ));
        }
        let keys: Vec<[usize; 2]> = (0..frames.len()).map(|i| if axis == 0 { [i, other] } else { [other, i] }).collect();
        let (table, table_source, sections) = self.mmd_bone_table(request)?;
        let morphs = match optional("morphs") {
            None => Vec::new(),
            Some(value) => mmd_motion::bake::parse_morphs(value)?,
        };
        if !morphs.is_empty() && animation_name.is_none() {
            return Err("morphs needs animation: morph values are written as lanes of the baked animation".into());
        }
        // The value range of each morph, and one lane per parameter axis.
        let mut lane_owner = HashMap::new();
        let mut ranges = Vec::with_capacity(morphs.len());
        for mapping in &morphs {
            let label = mmd_motion::retarget::name_label(&mapping.morph);
            let (id, morph_axis) = (mapping.parameter, mapping.axis);
            let target = find_parameter(&self.document.model, id)
                .filter(|target| target.get("groupUUID").is_none())
                .ok_or_else(|| format!("morphs entry \"{label}\": parameter {id} was not found or is a group"))?;
            if morph_axis == 1 && !target.get("is_vec2").and_then(Value::as_bool).unwrap_or(false) {
                return Err(format!("morphs entry \"{label}\": parameter {id} is 1D and has no axis 1"));
            }
            // Either axis: the baked poses sit at keypoint `other` of the axis the motion's lane leaves alone.
            if id == parameter_id {
                return Err(format!("morphs entry \"{label}\" drives parameter {id}, which the motion is baked on"));
            }
            if let Some(other) = lane_owner.insert((id, morph_axis), label.clone()) {
                return Err(format!(
                    "morphs entry \"{label}\" drives axis {morph_axis} of parameter {id}, which morphs entry \
                     \"{other}\" already drives"
                ));
            }
            ranges.push(match mapping.range {
                Some(range) => range,
                None => evaluation::parameter_axis_range(target, morph_axis)?,
            });
        }

        let bytes = read_bounded_file(path, mmd_motion::MAX_VMD_BYTES as u64, "VMD")?;
        let vmd = mmd_motion::parse_vmd(&bytes)?;
        let mut candidate = self.document.model.clone();
        let bones = rig_bones(&candidate, root)?;
        let (tracks, mut retarget) = (vmd.bone_tracks(), vmd.retarget_report(&table));
        retarget.unmapped_morphs.retain(|name| !morphs.iter().any(|mapping| mapping.morph == name.text));
        let poses: Vec<Pose> =
            frames.iter().map(|frame| Pose::from_tracks(&tracks, retarget.clone(), f64::from(*frame), &table)).collect();
        let mut plans: Vec<_> = poses.iter().map(|pose| plan(pose, &bones, scale)).collect();

        // A channel a later frame makes nonzero gets a binding, and cells never set are interpolated, so an earlier
        // frame that leaves it at zero must write that zero: ownership is judged over the whole motion.
        let bindings = find_parameter(&candidate, parameter_id).map(|p| p["bindings"].clone()).unwrap_or_default();
        let mut owned: HashSet<(u32, String)> = bindings
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| Some((u32::try_from(b["node"].as_u64()?).ok()?, b["param_name"].as_str()?.to_owned())))
            .collect();
        owned.extend(
            plans.iter().flat_map(|plan| plan.cells()).filter(|cell| cell.2 != 0.0).map(|cell| (cell.0, cell.1.to_owned())),
        );
        let is_owned = |bone: u32, channel: &str| owned.contains(&(bone, channel.to_owned()));
        let mut all_bones: Vec<u32> = Vec::new();
        for (plan, key) in plans.iter_mut().zip(&keys) {
            plan.drop_unowned_resets(is_owned);
            let mut cells = plan.cells();
            cells.retain(|&(bone, channel, value)| value != 0.0 || is_owned(bone, channel));
            evaluation::set_pose_cells(&mut candidate, parameter_id, *key, &cells)?;
            all_bones.extend(plan.bones());
        }
        all_bones.sort_unstable();
        all_bones.dedup();

        let mut report = mmd_motion::bake::report(&frames, &keys, &plans, &retarget);
        report.as_object_mut().expect("the report is an object").extend([
            ("root".to_owned(), json!(root)),
            ("parameter".to_owned(), json!(parameter_id)),
            ("axis".to_owned(), json!(axis)),
            ("other".to_owned(), json!(other)),
            ("keypointsWritten".to_owned(), json!(keys.len())),
            ("boneTable".to_owned(), json!(table_source)),
        ]);
        // One animation frame per VMD frame (30 fps), counted from the first sampled frame.
        let first = frames[0];
        // ponytail: a morph is sampled at `frames` like the poses, so a change between two sampled frames (a blink)
        // is lost. Keying the lane at the morph's own keys inside the range would be exact, since both are linear.
        let mut morph_lanes = Vec::new();
        let mut morph_reports = Vec::with_capacity(morphs.len());
        for (mapping, range) in morphs.iter().zip(&ranges) {
            let track = vmd.morph_track(&mapping.morph);
            let weight = |frame: &u32| mmd_motion::sample_morph(&track, f64::from(*frame));
            // f32, as the lane stores it.
            let sampled: Vec<f32> =
                frames.iter().map(|frame| (range[0] + (range[1] - range[0]) * weight(frame)) as f32).collect();
            morph_reports.push(json!({
                "morph": mapping.morph, "parameter": mapping.parameter, "axis": mapping.axis, "range": range,
                "inFile": !track.is_empty(),
                "values": if track.is_empty() { Value::Null } else { json!(sampled) },
            }));
            if !track.is_empty() {
                let lane = frames.iter().zip(sampled).map(|(frame, value)| ((frame - first) as i32, value)).collect();
                morph_lanes.push((mapping.parameter, mapping.axis, lane));
            }
        }
        report["morphs"] = json!(morph_reports);
        report["animation"] = match animation_name {
            None => Value::Null,
            Some(_) => {
                let lane: Vec<(i32, f32)> = frames
                    .iter()
                    .zip(&keys)
                    .map(|(frame, key)| (frame - first, values[axis][key[axis]]))
                    .map(|(offset, value)| (offset as i32, value as f32))
                    .collect();
                // The lane stores f32: two keypoints that round to one value would be one place on the lane.
                if lane.windows(2).any(|pair| pair[0].1 == pair[1].1) {
                    return Err("two of the keypoints have the same value in an animation lane (f32)".into());
                }
                let mut lanes = vec![(parameter_id, axis, lane)];
                lanes.append(&mut morph_lanes);
                animation::replace_with_lanes(&mut candidate, request, 1.0f32 / 30.0f32, &lanes)?
            }
        };
        if !dry_run {
            let mut marks = Vec::new();
            for key in &keys {
                let mut key_marks = dirty::mark_pose(&candidate, parameter_id, &all_bones, *key);
                key_marks.dedup();
                marks.extend(key_marks);
            }
            self.commit_marked_with_sections(candidate, marks, sections)?;
        }
        self.mmd_report = report.clone();
        Ok(report)
    }

    fn op_mirror(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let bones = request.get("bones").ok_or("bones is required")?;
        let axis = request.get("axis").ok_or("axis is required")?;
        let candidate = bone_deform::mirror_bones(
            &self.document.model,
            &self.runtime_values,
            root,
            bones,
            axis,
        )?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(json!({ "root": root }))
    }

    fn op_set_vertex_weights(&mut self, request: &Value) -> Result<Value, String> {
        let mut scrubbed = request.clone();
        if let Some(object) = scrubbed.as_object_mut() {
            object.remove("weights");
        }
        bindings::reject_weight_arrays(&scrubbed)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let weights = request.get("weights").ok_or("weights is required")?;
        let candidate = bindings::set_vertex_weights(&self.document.model, root, target, weights)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(json!({ "root": root, "target": target }))
    }

    fn op_clear_vertex_weights(&mut self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let candidate = bindings::clear_vertex_weights(&self.document.model, root, target)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(json!({ "root": root, "target": target }))
    }

    fn op_add_source(&mut self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let bone = required_id(request, "bone")?;
        let candidate = bindings::add_source(&self.document.model, root, target, bone)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        bindings::list_sources(&self.document.model, root, target)
    }

    fn op_remove_source(&mut self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let bone = required_id(request, "bone")?;
        let candidate = bindings::remove_source(&self.document.model, root, target, bone)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        bindings::list_sources(&self.document.model, root, target)
    }

    fn op_list_sources(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        bindings::list_sources(&self.document.model, root, target)
    }

    fn op_set_source_settings(&mut self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let bone = required_id(request, "bone")?;
        let settings = request.get("settings").ok_or("settings is required")?;
        let candidate =
            bindings::set_source_settings(&self.document.model, root, target, bone, settings)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        bindings::list_sources(&self.document.model, root, target)
    }

    fn op_set_influence_rule(&mut self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let rule = request.get("rule").ok_or("rule is required")?;
        let candidate = bindings::set_rule(&self.document.model, root, target, rule)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        bindings::get_rule(&self.document.model, root, target)
    }

    fn op_get_influence_rule(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        bindings::get_rule(&self.document.model, root, target)
    }

    fn op_preview_influence(&self, request: &Value) -> Result<Value, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let target = required_id(request, "target")?;
        let bone = required_id(request, "bone")?;
        bindings::preview_influence(&self.document.model, root, target, bone)
    }

    fn op_gpu_offsets(&self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let targets = optional_ids(request, "targets")?;
        let parameter = match request.get("parameter") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(valid_id)
                    .ok_or_else(|| "parameter must be a positive resource UUID".to_string())?,
            ),
        };
        bone_deform::gpu_offsets(
            &self.document.model,
            &self.runtime_values,
            root,
            &targets,
            parameter,
            &self.preview_deformation,
        )
    }

    fn op_preview_deform(&mut self, request: &Value) -> Result<Value, String> {
        let has_runtime_override = request.get("values").is_some();
        let values = if has_runtime_override {
            self.request_values(request)?
        } else {
            self.runtime_values.clone()
        };
        let mut preview = self.preview_with_runtime_overrides(request, &values)?;
        let outcome = self.deform_outcome(request, false, &values, &preview)?;
        if let Some(targets) = outcome.body.get("targets").and_then(Value::as_array) {
            for target in targets {
                let Some(id) = target
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(valid_id)
                else {
                    continue;
                };
                let offsets = target
                    .get("offsets")
                    .and_then(Value::as_array)
                    .ok_or("offsets must be an array")?;
                let mut stored = Vec::with_capacity(offsets.len());
                for offset in offsets {
                    stored.push(parse_pair(offset, "offset")?);
                }
                preview.insert(id, stored);
            }
        }
        if has_runtime_override {
            self.runtime_values = values;
        }
        self.preview_deformation = preview;
        self.animation_preview = None;
        self.reset_physics();
        Ok(outcome.body)
    }

    fn op_fit_root_z(&mut self, request: &Value) -> Result<Value, String> {
        let root = required_id(request, "root")?;
        let (candidate, body) = fit_z::fit_root(&self.document.model, &self.runtime_values, root)?;
        self.commit_marked(candidate, vec![dirty::mark_root(root)])?;
        Ok(body)
    }

    fn op_set_rest(&mut self, request: &Value) -> Result<Value, String> {
        let bone = required_id(request, "bone")?;
        let head = bone_edit::parse_vec3(
            request.get("restHead").ok_or("restHead is required")?,
            "restHead",
        )?;
        let tail = bone_edit::parse_vec3(
            request.get("restTail").ok_or("restTail is required")?,
            "restTail",
        )?;
        let roll = finite_number(
            request.get("restRoll").ok_or("restRoll is required")?,
            "restRoll",
        )?;
        let candidate = bone_edit::set_rest(&self.document.model, bone, head, tail, roll)?;
        let marks = dirty::mark_bone(&candidate, bone).into_iter().collect();
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "bone": bone }))
    }

    fn op_set_constraint(&mut self, request: &Value) -> Result<Value, String> {
        let bone = required_id(request, "bone")?;
        let patch = request.get("constraint").ok_or("constraint is required")?;
        let candidate = bone_edit::set_constraint(&self.document.model, bone, patch)?;
        let marks = dirty::mark_bone(&candidate, bone).into_iter().collect();
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "bone": bone }))
    }

    fn op_fit_bone_z(&mut self, request: &Value) -> Result<Value, String> {
        let bone = required_id(request, "bone")?;
        let (candidate, body) = fit_z::fit_bone(&self.document.model, &self.runtime_values, bone)?;
        let marks = dirty::mark_bone(&candidate, bone).into_iter().collect();
        self.commit_marked(candidate, marks)?;
        Ok(body)
    }

    fn op_apply_deform(&mut self, request: &Value) -> Result<Value, String> {
        let has_runtime_override = request.get("values").is_some();
        let values = if has_runtime_override {
            self.request_values(request)?
        } else {
            self.runtime_values.clone()
        };
        let preview = self.preview_with_runtime_overrides(request, &values)?;
        let outcome = self.deform_outcome(request, true, &values, &preview)?;
        let model = outcome.model.ok_or("No armed parameter")?;
        let model_unchanged = model == self.document.model;
        self.commit_model(model)?;
        if has_runtime_override {
            self.runtime_values = values;
            if model_unchanged {
                self.preview_deformation = preview;
            }
            self.animation_preview = None;
            self.reset_physics();
        }
        Ok(outcome.body)
    }

    fn preview_with_runtime_overrides(
        &self,
        request: &Value,
        values: &BTreeMap<u32, [f64; 2]>,
    ) -> Result<BTreeMap<u32, Vec<[f64; 2]>>, String> {
        let mut preview = self.preview_deformation.clone();
        let Some(overrides) = request.get("values").and_then(Value::as_object) else {
            return Ok(preview);
        };
        for key in overrides.keys() {
            let parameter = key
                .parse::<u32>()
                .map_err(|_| format!("invalid parameter UUID key: {key}"))?;
            for (id, offsets) in dirty::preview_pose(&self.document.model, values, parameter)? {
                preview.insert(id, offsets);
            }
        }
        Ok(preview)
    }

    fn deform_outcome(
        &self,
        request: &Value,
        apply: bool,
        values: &BTreeMap<u32, [f64; 2]>,
        preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    ) -> Result<bone_deform::DeformOutcome, String> {
        bindings::reject_weight_arrays(request)?;
        let root = required_id(request, "root")?;
        let targets = optional_ids(request, "targets")?;
        let parameter = match request.get("parameter") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(valid_id)
                    .ok_or_else(|| "parameter must be a positive resource UUID".to_string())?,
            ),
        };
        bone_deform::run(
            &self.document.model,
            values,
            root,
            &targets,
            parameter,
            apply,
            preview,
        )
    }

    fn op_node_move(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let parent = request
            .get("parent")
            .or_else(|| request.get("newParent"))
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or_else(|| "parent must be a positive resource UUID".to_string())?;
        let index = request
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let candidate = nodes::move_nodes(&self.document.model, &ids, parent, index)?;
        let moved = ids.clone();
        // D NodeMoveAction reparents and rewrites translation. It does not
        // rewrite rig bindings or regenerate "deform" for a rig node. A
        // non-rig node in the same request still marks its deform targets.
        let planar: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|id| {
                nodes::find_node(&candidate, *id)
                    .and_then(|node| node.get("type"))
                    .and_then(Value::as_str)
                    .is_none_or(|kind| kind != "DepthBone" && kind != "DepthRigRoot")
            })
            .collect();
        if planar.is_empty() {
            self.commit_model(candidate)?;
        } else {
            self.commit_targets(candidate, &planar)?;
        }
        Ok(json!({ "moved": moved, "parent": parent, "index": index }))
    }

    fn op_node_delete(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let candidate = nodes::delete_nodes(&self.document.model, &ids)?;
        let deleted = ids.clone();
        let marks = dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "deleted": deleted }))
    }

    fn op_node_copy(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let copies = nodes::copy_nodes(&self.document.model, &ids)?;
        let clipboard = nodes::clipboard_summaries(&copies);
        self.clipboard = copies;
        Ok(json!({ "count": self.clipboard.len(), "clipboard": clipboard }))
    }

    fn op_node_cut(&mut self, request: &Value) -> Result<Value, String> {
        let ids = node_ids(request)?;
        let copies = nodes::copy_nodes(&self.document.model, &ids)?;
        let deleted: Vec<u32> = copies.iter().map(|entry| entry.source_id).collect();
        let candidate = nodes::delete_nodes(&self.document.model, &deleted)?;
        let clipboard = nodes::clipboard_summaries(&copies);
        let marks = dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        self.clipboard = copies;
        Ok(json!({ "deleted": deleted, "count": self.clipboard.len(), "clipboard": clipboard }))
    }

    fn op_node_paste(&mut self, request: &Value) -> Result<Value, String> {
        let parent = request
            .get("parent")
            .or_else(|| request.get("newParent"))
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .or_else(|| node_ids(request).ok().and_then(|ids| ids.first().copied()))
            .ok_or_else(|| "paste parent is required".to_string())?;
        let index = request
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        if self.clipboard.is_empty() {
            return Err("Clipboard empty".into());
        }
        let (candidate, created) =
            nodes::paste_nodes(&self.document.model, &self.clipboard, parent, index)?;
        let marks = dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        self.clipboard.clear();
        Ok(json!({ "created": created, "parent": parent }))
    }

    fn op_node_convert(&mut self, request: &Value) -> Result<Value, String> {
        let class_name = request
            .get("className")
            .or_else(|| request.get("toType"))
            .and_then(Value::as_str)
            .ok_or("className is required")?;
        let ids = node_ids(request)?;
        let (candidate, converted) = nodes::convert_nodes(&self.document.model, &ids, class_name)?;
        let marks = dirty::marks_for_structure(&self.document.model, &candidate);
        self.commit_marked(candidate, marks)?;
        Ok(json!({ "converted": converted, "created": converted }))
    }

    fn op_node_rename(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let name = required_string(request, "name")?.to_owned();
        let candidate = nodes::rename_node(&self.document.model, id, &name)?;
        let result = nodes::find_node(&candidate, id)
            .cloned()
            .ok_or_else(|| format!("node {id} was not found"))?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    fn op_object_replace_name(&mut self, request: &Value) -> Result<Value, String> {
        let (candidate, reply) = object_names::replace(&self.document.model, request)?;
        self.commit_model(candidate)?;
        Ok(reply)
    }

    /// `object.export_csv`: writes the UUID, type and name of every node, parameter and parameter group as CSV.
    fn op_object_export_csv(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        let overwrite = match request.get("overwrite").filter(|value| !value.is_null()) {
            None => false,
            Some(value) => value.as_bool().ok_or("overwrite must be a boolean")?,
        };
        let extension = Path::new(path).extension().and_then(|extension| extension.to_str());
        if !extension.is_some_and(|extension| extension.eq_ignore_ascii_case("csv")) {
            return Err("path must name a .csv file".into());
        }
        let target = normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "object.export_csv")?;
        if !overwrite && target.exists() {
            return Err("export destination already exists".into());
        }
        let (text, rows) = object_names::export_csv(&self.document.model);
        write_atomic(&target, text.as_bytes(), overwrite)?;
        Ok(json!({"path": target.to_string_lossy(), "rows": rows, "bytes": text.len()}))
    }

    /// `model.template.export`: writes the node tree (without textures) and the parameters of the document to a
    /// model template file.
    fn op_model_template_export(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        let overwrite = match request.get("overwrite").filter(|value| !value.is_null()) {
            None => false,
            Some(value) => value.as_bool().ok_or("overwrite must be a boolean")?,
        };
        let extension = Path::new(path).extension().and_then(|extension| extension.to_str());
        if !extension.is_some_and(|extension| extension.eq_ignore_ascii_case("json")) {
            return Err("path must name a .json file".into());
        }
        let target = normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "model.template.export")?;
        if !overwrite && target.exists() {
            return Err("export destination already exists".into());
        }
        let stem = Path::new(path).file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        let name = optional_string(request, "name")?.unwrap_or(stem);
        let description = optional_string(request, "description")?.unwrap_or_default();
        let template = model_template::export(&self.document.model, name, description)?;
        let bytes = serde_json::to_vec_pretty(&template).map_err(|error| error.to_string())?;
        write_atomic(&target, &bytes, overwrite)?;
        Ok(json!({"path": target.to_string_lossy(), "name": name, "bytes": bytes.len()}))
    }

    /// `model.template.apply`: fits a model template onto the document's Parts and copies its deformers and
    /// parameters, in one undo step.
    fn op_model_template_apply(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        let bytes = read_bounded_file(path, model_template::MAX_TEMPLATE_BYTES, "model template")?;
        let template: Value = serde_json::from_slice(&bytes).map_err(|error| format!("the template is not JSON: {error}"))?;
        let by_position = match optional_string(request, "match")? {
            None | Some("name") => false,
            Some("position") => true,
            Some(_) => return Err("match must be name or position".into()),
        };
        let add_parameters = match optional_string(request, "parameters")? {
            None | Some("join") => false,
            Some("add") => true,
            Some(_) => return Err("parameters must be join or add".into()),
        };
        let scale = match request.get("scale").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(finite_number(value, "scale")?),
        };
        let offset = match request.get("offset").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(parse_pair(value, "offset")?),
        };
        let options = model_template::Options {
            by_position,
            add_parameters,
            parts: optional_ids(request, "parts")?,
            scale,
            offset,
        };
        let (candidate, report) = model_template::apply(&self.document.model, &template, &options)?;
        self.commit_model(candidate)?;
        Ok(report)
    }

    /// `object.import_csv`: renames the objects a CSV of `object.export_csv` lists, in one undo step. The text
    /// comes from the file `path` or from `csv`.
    fn op_object_import_csv(&mut self, request: &Value) -> Result<Value, String> {
        let dry_run = dry_run_flag(request)?;
        let text = match (optional_string(request, "path")?, optional_string(request, "csv")?) {
            (Some(path), None) => {
                let file = File::open(path).map_err(|error| format!("could not open the CSV file: {error}"))?;
                let mut bytes = Vec::new();
                // One byte more than the most that is read shows a larger file without reading it.
                file.take(object_names::MAX_CSV_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("could not read the CSV file: {error}"))?;
                if bytes.len() > object_names::MAX_CSV_BYTES {
                    return Err(format!("the CSV text is larger than {} bytes", object_names::MAX_CSV_BYTES));
                }
                String::from_utf8(bytes).map_err(|_| "the CSV file is not UTF-8")?
            }
            (None, Some(text)) => text.to_owned(),
            _ => return Err("give the CSV as exactly one of path and csv".into()),
        };
        let (candidate, mut reply) = object_names::import_csv(&self.document.model, &text)?;
        let applied = !dry_run && reply["renamed"].as_array().is_some_and(|renamed| !renamed.is_empty());
        if applied {
            self.commit_model(candidate)?;
        }
        reply["applied"] = json!(applied);
        Ok(reply)
    }

    fn op_node_enabled(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let change = if request["op"].as_str() == Some("node.enabled.set") {
            let enabled = request
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("enabled must be a boolean")?;
            nodes::NodeEnabledChange::Set(enabled)
        } else {
            if request.get("enabled").is_some() {
                return Err("enabled must not be supplied to node.enabled.toggle".into());
            }
            nodes::NodeEnabledChange::Toggle
        };
        let (candidate, enabled) = nodes::change_enabled(&self.document.model, id, change)?;
        self.commit_model(candidate)?;
        Ok(json!({ "uuid": id, "enabled": enabled }))
    }

    fn op_node_transform(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let candidate = if let Some(component) = request.get("component").and_then(Value::as_str) {
            let value = finite_number(
                request
                    .get("value")
                    .ok_or("value is required with component")?,
                "value",
            )?;
            nodes::apply_transform_component(&self.document.model, id, component, value)?
        } else {
            nodes::transform_node(&self.document.model, id, request)?
        };
        let result = nodes::find_node(&candidate, id)
            .cloned()
            .ok_or_else(|| format!("node {id} was not found"))?;
        let kind = result.get("type").and_then(Value::as_str);
        let rig = matches!(kind, Some("DepthBone" | "DepthRigRoot"));
        if rig && transform_changes_trs(request) {
            let marks = match kind {
                Some("DepthBone") => dirty::mark_bone(&candidate, id).into_iter().collect(),
                Some("DepthRigRoot") => vec![dirty::mark_root(id)],
                _ => Vec::new(),
            };
            if marks.is_empty() {
                self.commit_model(candidate)?;
            } else {
                self.commit_marked(candidate, marks)?;
            }
        } else if rig {
            // z-sort does not change the rig pose, so it must not refresh deform.
            self.commit_model(candidate)?;
        } else {
            self.commit_targets(candidate, &[id])?;
        }
        Ok(result)
    }

    fn op_node_property_set(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let property = required_string(request, "property")?;
        let value = request.get("value").ok_or("value is required")?;
        let (candidate, spec) = nodes::set_property(&self.document.model, id, property, value)?;
        let stored = nodes::find_node(&candidate, id)
            .and_then(|node| nodes::property_value(node, spec))
            .cloned()
            .ok_or("property was not stored")?;
        if spec.pose {
            self.commit_targets(candidate, &[id])?;
        } else {
            self.commit_model(candidate)?;
        }
        Ok(json!({ "id": id, "property": property, "value": stored }))
    }

    fn op_node_mask_edit(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let source = required_id(request, "source")?;
        let dodge = match request.get("mode").map(|mode| mode.as_str()) {
            None => false,
            Some(Some("Mask")) => false,
            Some(Some("DodgeMask")) => true,
            Some(_) => return Err("mode must be Mask or DodgeMask".into()),
        };
        let edit = match required_string(request, "action")? {
            "add" => nodes::MaskEdit::Add { source, dodge },
            "remove" => nodes::MaskEdit::Remove { source },
            "mode" if request.get("mode").is_some() => nodes::MaskEdit::SetMode { source, dodge },
            "mode" => return Err("mode is required".into()),
            "move" => {
                let index = request
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("index must be a nonnegative integer")?;
                nodes::MaskEdit::Move { source, index }
            }
            other => return Err(format!("unknown mask action {other}")),
        };
        let (candidate, masks) = nodes::edit_masks(&self.document.model, id, &edit)?;
        self.commit_model(candidate)?;
        Ok(json!({ "id": id, "masks": masks }))
    }

    fn op_puppet_meta_set(&mut self, request: &Value) -> Result<Value, String> {
        let property = required_string(request, "property")?;
        let value = request.get("value").ok_or("value is required")?;
        let candidate = nodes::set_puppet_property(&self.document.model, property, value)?;
        self.commit_model(candidate)?;
        Ok(json!({ "property": property, "value": value }))
    }

    fn op_parameter_get(&self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        find_parameter(&self.document.model, id)
            .cloned()
            .ok_or_else(|| format!("parameter {id} was not found"))
    }

    fn op_parameter_create(&mut self, request: &Value) -> Result<Value, String> {
        let dimensions = request
            .get("dimensions")
            .and_then(Value::as_u64)
            .filter(|value| *value == 1 || *value == 2)
            .ok_or("dimensions must be 1 or 2")? as usize;
        let (default_min, default_max) = if dimensions == 1 {
            ([-1.0, 0.0], [1.0, 1.0])
        } else {
            ([-1.0, -1.0], [1.0, 1.0])
        };
        let min = optional_pair(request, "min")?.unwrap_or(default_min);
        let max = optional_pair(request, "max")?.unwrap_or(default_max);
        let name = match request.get("name") {
            Some(value) => value.as_str().ok_or("name must be a string")?.to_owned(),
            None => format!("Param #{}\0", parameter_count(&self.document.model)),
        };
        check_parameter_name(&self.document.model, &name, None)?;
        let mut candidate = self.document.model.clone();
        let id = fresh_resource_id(&candidate)?;
        let axes = [
            axis_points(min[0], max[0]),
            if dimensions == 1 {
                vec![0.0]
            } else {
                axis_points(min[1], max[1])
            },
        ];
        let parameter = json!({
            "uuid": id,
            "name": name,
            "is_vec2": dimensions == 2,
            "min": min,
            "max": max,
            "defaults": [0.0, 0.0],
            "axis_points": axes,
            "merge_mode": "Additive",
            "bindings": [],
        });
        candidate
            .as_object_mut()
            .ok_or("model must be an object")?
            .entry("param")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("model param must be an array")?
            .push(parameter.clone());
        self.commit_model(candidate)?;
        self.runtime_values.insert(id, [0.0, 0.0]);
        Ok(parameter)
    }

    fn op_parameter_rename(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let name = required_string(request, "name")?.to_owned();
        check_parameter_name(&self.document.model, &name, Some(id))?;
        let mut candidate = self.document.model.clone();
        let parameter = find_parameter_mut(&mut candidate, id)
            .ok_or_else(|| format!("parameter {id} was not found"))?;
        parameter
            .as_object_mut()
            .ok_or("parameter must be an object")?
            .insert("name".into(), Value::String(name));
        let result = parameter.clone();
        self.commit_model(candidate)?;
        Ok(result)
    }

    fn op_parameter_delete(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let deleted = find_parameter(&self.document.model, id)
            .ok_or_else(|| format!("parameter {id} was not found"))?.clone();
        let mut candidate = self.document.model.clone();
        parameter_edit::delete_parameter(&mut candidate, id)?;
        self.commit_model(candidate)?;
        Ok(deleted)
    }

    fn op_parameter_set_axes(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let min = required_pair(request, "min")?;
        let max = required_pair(request, "max")?;
        let axes: [Vec<f64>; 2] = serde_json::from_value(
            request.get("axis_points").ok_or("axis_points is required")?.clone(),
        ).map_err(|_| "axis_points must contain two numeric arrays")?;
        let mut candidate = self.document.model.clone();
        parameter_edit::set_axes(&mut candidate, id, min, max, &axes[0], &axes[1])?;
        let updated = find_parameter(&candidate, id)
            .ok_or_else(|| format!("parameter {id} was not found"))?.clone();
        self.commit_model(candidate)?;
        Ok(updated)
    }

    fn op_parameter_duplicate(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let name = optional_string(request, "name")?;
        let mut candidate = self.document.model.clone();
        let new_id = fresh_resource_id(&candidate)?;
        let copy = parameter_edit::duplicate_parameter(&mut candidate, id, new_id, name)?;
        self.commit_model(candidate)?;
        Ok(copy)
    }

    /// The panel's "Add Mouth Shape": a 2D parameter with extra axis points (`AddMouthParameterCommand`).
    fn op_parameter_create_mouth(&mut self, request: &Value) -> Result<Value, String> {
        let name = match optional_string(request, "name")? {
            Some(name) => name.to_owned(),
            None => format!("Mouth #{}\0", parameter_count(&self.document.model)),
        };
        check_parameter_name(&self.document.model, &name, None)?;
        let mut candidate = self.document.model.clone();
        let id = fresh_resource_id(&candidate)?;
        let parameter = json!({
            "uuid": id,
            "name": name,
            "is_vec2": true,
            "min": [-1.0, 0.0],
            "max": [1.0, 1.0],
            "defaults": [0.0, 0.0],
            "axis_points": [[0.0, 0.25, 0.5, 0.6, 1.0], [0.0, 0.3, 0.5, 0.6, 1.0]],
            "merge_mode": "Additive",
            "bindings": [],
        });
        candidate
            .as_object_mut()
            .ok_or("model must be an object")?
            .entry("param")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("model param must be an array")?
            .push(parameter.clone());
        self.commit_model(candidate)?;
        self.runtime_values.insert(id, [0.0, 0.0]);
        Ok(parameter)
    }

    /// Add each parameter of the Cubism standard list that no parameter is named after yet.
    fn op_parameter_create_standard(&mut self, request: &Value) -> Result<Value, String> {
        // ID, minimum, default, maximum, and whether the list marks the row "to be input if necessary". From the
        // "Standard Parameter List" page of the Cubism Editor manual, read 2026-10-13. The page gives the default
        // of ParamCheek as "As appropriate"; 0 stands in for it.
        const STANDARD: &[(&str, f64, f64, f64, bool)] = &[
            ("ParamAngleX", -30.0, 0.0, 30.0, false),
            ("ParamAngleY", -30.0, 0.0, 30.0, false),
            ("ParamAngleZ", -30.0, 0.0, 30.0, false),
            ("ParamEyeLOpen", 0.0, 1.0, 1.0, false),
            ("ParamEyeLSmile", 0.0, 0.0, 1.0, false),
            ("ParamEyeROpen", 0.0, 1.0, 1.0, false),
            ("ParamEyeRSmile", 0.0, 0.0, 1.0, false),
            ("ParamEyeBallX", -1.0, 0.0, 1.0, false),
            ("ParamEyeBallY", -1.0, 0.0, 1.0, false),
            ("ParamEyeBallForm", -1.0, 0.0, 1.0, true),
            ("ParamBrowLY", -1.0, 0.0, 1.0, false),
            ("ParamBrowRY", -1.0, 0.0, 1.0, false),
            ("ParamBrowLX", -1.0, 0.0, 1.0, false),
            ("ParamBrowRX", -1.0, 0.0, 1.0, false),
            ("ParamBrowLAngle", -1.0, 0.0, 1.0, false),
            ("ParamBrowRAngle", -1.0, 0.0, 1.0, false),
            ("ParamBrowLForm", -1.0, 0.0, 1.0, false),
            ("ParamBrowRForm", -1.0, 0.0, 1.0, false),
            ("ParamMouthForm", -1.0, 0.0, 1.0, false),
            ("ParamMouthOpenY", 0.0, 0.0, 1.0, false),
            ("ParamCheek", 0.0, 0.0, 1.0, false),
            ("ParamBodyAngleX", -10.0, 0.0, 10.0, false),
            ("ParamBodyAngleY", -10.0, 0.0, 10.0, false),
            ("ParamBodyAngleZ", -10.0, 0.0, 10.0, false),
            ("ParamBreath", 0.0, 0.0, 1.0, false),
            ("ParamArmLA", -30.0, 0.0, 30.0, true),
            ("ParamArmRA", -30.0, 0.0, 30.0, true),
            ("ParamArmLB", -30.0, 0.0, 30.0, true),
            ("ParamArmRB", -30.0, 0.0, 30.0, true),
            ("ParamHandL", -10.0, 0.0, 10.0, true),
            ("ParamHandR", -10.0, 0.0, 10.0, true),
            ("ParamHairFront", -1.0, 0.0, 1.0, false),
            ("ParamHairSide", -1.0, 0.0, 1.0, false),
            ("ParamHairBack", -1.0, 0.0, 1.0, false),
            ("ParamHairFluffy", -1.0, 0.0, 1.0, true),
            ("ParamShoulderY", -10.0, 0.0, 10.0, true),
            ("ParamBustX", -1.0, 0.0, 1.0, true),
            ("ParamBustY", -1.0, 0.0, 1.0, true),
            ("ParamBaseX", -10.0, 0.0, 10.0, true),
            ("ParamBaseY", -10.0, 0.0, 10.0, true),
        ];
        let all = request.get("all").map(|value| value.as_bool().ok_or("all must be a boolean"))
            .transpose()?.unwrap_or(false);
        let mut candidate = self.document.model.clone();
        let (mut created, mut existing) = (Vec::new(), Vec::new());
        for &(name, min, default, max, if_necessary) in STANDARD {
            if if_necessary && !all {
                continue;
            }
            if check_parameter_name(&candidate, name, None).is_err() {
                // Groups share the name space. A group of that name is not the parameter.
                if !has_parameter_named(candidate["param"].as_array().map_or(&[], Vec::as_slice), name) {
                    return Err(format!("{name} is the name of a parameter group, so the parameter cannot be added"));
                }
                existing.push(name);
                continue;
            }
            let parameter = json!({
                "uuid": fresh_resource_id(&candidate)?,
                "name": name,
                "is_vec2": false,
                "min": [min, 0.0],
                "max": [max, 1.0],
                "defaults": [default, 0.0],
                "axis_points": [axis_points(min, max), [0.0]],
                "merge_mode": "Additive",
                "bindings": [],
            });
            candidate
                .as_object_mut()
                .ok_or("model must be an object")?
                .entry("param")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("model param must be an array")?
                .push(parameter);
            created.push(name);
        }
        self.commit_model(candidate)?;
        Ok(json!({"created": created, "existing": existing}))
    }

    fn op_parameter_group_create(&mut self, request: &Value) -> Result<Value, String> {
        let name = optional_string(request, "name")?;
        let members = match request.get("members") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or("members must be an array of parameter UUIDs")?
                .iter()
                .map(|id| {
                    id.as_u64()
                        .and_then(valid_id)
                        .ok_or_else(|| "members must contain nonzero u32 UUIDs".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        let mut candidate = self.document.model.clone();
        let id = fresh_resource_id(&candidate)?;
        let group = parameter_edit::create_group(&mut candidate, id, name, &members)?;
        self.commit_model(candidate)?;
        Ok(group)
    }

    fn op_parameter_group_delete(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let mut candidate = self.document.model.clone();
        let released = parameter_edit::delete_group(&mut candidate, id)?;
        if let Some(animations) = candidate.get_mut("animations").and_then(Value::as_object_mut) {
            for animation in animations.values_mut() {
                let remove_empty = if let Some(groups) = animation.get_mut("frameStepGroups").and_then(Value::as_object_mut) {
                    groups.remove(&id.to_string());
                    groups.is_empty()
                } else {
                    false
                };
                if remove_empty {
                    animation.as_object_mut().map(|animation| animation.remove("frameStepGroups"));
                }
            }
        }
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "released": released}))
    }

    fn op_parameter_group_set_color(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let color: [f64; 3] = serde_json::from_value(request.get("color").ok_or("color is required")?.clone())
            .map_err(|_| "color must be three numbers")?;
        let mut candidate = self.document.model.clone();
        parameter_edit::set_group_color(&mut candidate, id, color)?;
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "color": color}))
    }

    fn op_parameter_move(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let group = match request.get("group") {
            None | Some(Value::Null) => None,
            Some(_) => Some(required_id(request, "group")?),
        };
        let index = match request.get("index") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("index must be a nonnegative integer")?,
            ),
        };
        let mut candidate = self.document.model.clone();
        parameter_edit::move_parameter(&mut candidate, id, group, index)?;
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "group": group}))
    }

    fn op_parameter_flip(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let axis = axis_arg(request, 0)?;
        let mut candidate = self.document.model.clone();
        parameter_edit::flip_axis(&mut candidate, id, axis)?;
        let result = find_parameter(&candidate, id).cloned().ok_or("parameter vanished")?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    fn op_parameter_mirror(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let axis = axis_arg(request, 0)?;
        let mut candidate = self.document.model.clone();
        parameter_edit::mirror_all(&mut candidate, id, axis)?;
        let result = find_parameter(&candidate, id).cloned().ok_or("parameter vanished")?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    /// Without `value`, the session's current value of the parameter becomes its default.
    /// Joins the two ends of a 1D parameter into a loop (`true`) or opens it again (`false`, which removes the key).
    fn op_parameter_set_repeat(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let repeat = request
            .get("repeat")
            .and_then(Value::as_bool)
            .ok_or("repeat must be a boolean")?;
        let mut candidate = self.document.model.clone();
        let parameter = find_parameter_mut(&mut candidate, id)
            .ok_or_else(|| format!("parameter {id} was not found"))?;
        if parameter.get("groupUUID").is_some() {
            return Err(format!("{id} is a parameter group; only a 1D parameter can repeat"));
        }
        if parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false) {
            return Err(format!("parameter {id} is 2D; only a 1D parameter can repeat"));
        }
        let object = parameter.as_object_mut().ok_or("parameter must be an object")?;
        if repeat {
            object.insert("repeat".into(), json!(true));
        } else {
            object.remove("repeat");
        }
        evaluation::refill_parameter_bindings(parameter)?;
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "repeat": repeat}))
    }

    fn op_parameter_set_defaults(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let value = match request.get("value") {
            Some(_) => required_pair(request, "value")?,
            None => *self
                .runtime_values
                .get(&id)
                .ok_or_else(|| format!("parameter {id} was not found"))?,
        };
        let value = checked_parameter_value(&self.document.model, id, value)?;
        let mut candidate = self.document.model.clone();
        parameter_edit::set_defaults(&mut candidate, id, value)?;
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "defaults": value}))
    }

    /// Cubism's "set all current values as defaults": every parameter starts where the session has it now.
    fn op_parameter_set_all_defaults(&mut self) -> Result<Value, String> {
        let defaults = default_values(&self.document.model);
        let mut candidate = self.document.model.clone();
        let mut changed = 0;
        for (&id, default) in &defaults {
            let Some(&value) = self.runtime_values.get(&id).filter(|value| *value != default) else { continue };
            let value = checked_parameter_value(&candidate, id, value)?;
            parameter_edit::set_defaults(&mut candidate, id, value)?;
            changed += 1;
        }
        self.commit_model(candidate)?;
        Ok(json!({"parameters": defaults.len(), "changed": changed}))
    }

    fn op_parameter_split(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let nodes = request
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or("nodes must be an array of node UUIDs")?
            .iter()
            .map(|id| {
                id.as_u64()
                    .and_then(valid_id)
                    .ok_or_else(|| "nodes must contain nonzero u32 UUIDs".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidate = self.document.model.clone();
        let new_id = fresh_resource_id(&candidate)?;
        let split = parameter_edit::split_parameter(&mut candidate, id, new_id, &nodes)?;
        self.commit_model(candidate)?;
        Ok(split)
    }

    fn op_binding_keypoint(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let key = key_arg(request)?;
        let action = required_string(request, "action")?;
        let only = binding_keys(request, false)?;
        let mut candidate = self.document.model.clone();
        let mut skipped = Vec::new();
        let edited = match action {
            "flip_deform" | "symmetrize_deform" => {
                let (edited, unsupported) =
                    parameter_flip::flip_deform(&mut candidate, id, key, action == "symmetrize_deform", only.as_deref())?;
                skipped = unsupported;
                edited
            }
            "set_from_mirror_horizontal" | "set_from_mirror_vertical" | "set_from_mirror_diagonal"
            | "set_from_mirror_1d" => {
                let mode = match action {
                    "set_from_mirror_horizontal" => parameter_flip::MirrorSource::Horizontal,
                    "set_from_mirror_vertical" => parameter_flip::MirrorSource::Vertical,
                    "set_from_mirror_diagonal" => parameter_flip::MirrorSource::Diagonal,
                    _ => parameter_flip::MirrorSource::OneD,
                };
                let pairs = self.flip_pairs()?;
                parameter_flip::set_from_mirror(&mut candidate, &pairs, id, key, mode, only.as_deref())?
            }
            _ => parameter_edit::edit_keypoint(&mut candidate, id, key, action, only.as_deref())?,
        };
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "key": key, "action": action, "edited": edited, "skipped": skipped}))
    }

    /// The flip pairs of the open document (none for documents without extension data).
    fn flip_pairs(&self) -> Result<Vec<parameter_flip::Pair>, String> {
        if !self.document.is_inx() {
            return Ok(Vec::new());
        }
        parameter_flip::read_pairs(&self.document.extension_entries()?, &self.document.model)
    }

    fn op_parameter_link(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let axis = axis_arg(request, 0)?;
        let target = required_id(request, "target")?;
        let target_axis = axis_named(request, "target_axis", 0)?;
        let mut candidate = self.document.model.clone();
        let binding = parameter_flip::link(&mut candidate, id, axis, target, target_axis)?;
        self.commit_model(candidate)?;
        Ok(binding)
    }

    fn op_parameter_unlink(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let target = required_id(request, "target")?;
        let target_axis = axis_named(request, "target_axis", 0)?;
        let mut candidate = self.document.model.clone();
        let removed = parameter_flip::unlink(&mut candidate, id, target, target_axis)?;
        self.commit_model(candidate)?;
        Ok(json!({"id": id, "target": target, "removed": removed}))
    }

    fn op_parameter_mirrored_autofill(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let direction = request
            .get("direction")
            .and_then(Value::as_u64)
            .and_then(|direction| u8::try_from(direction).ok())
            .ok_or("direction must be 1, 2, 3 or 4")?;
        let pairs = self.flip_pairs()?;
        let mut candidate = self.document.model.clone();
        let result = parameter_flip::mirrored_autofill(&mut candidate, &pairs, id, direction)?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    fn op_parameter_synthesize_corners(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let mut candidate = self.document.model.clone();
        let result = parameter_flip::synthesize_corners(&mut candidate, id)?;
        self.commit_model(candidate)?;
        Ok(result)
    }

    /// `CopyParameterCommand`: the clipboard holds a copy; the document is untouched.
    fn op_parameter_copy(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let parameter = find_parameter(&self.document.model, id)
            .filter(|parameter| parameter.get("groupUUID").is_none())
            .ok_or_else(|| format!("parameter {id} was not found"))?
            .clone();
        let name = parameter["name"].clone();
        self.parameter_clipboard = Some(parameter);
        Ok(json!({"copied": id, "name": name}))
    }

    /// `PasteParameterCommand` / `PasteParameterWithFlipCommand`; the clipboard is used up.
    fn op_parameter_paste(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let flip = match request.get("flip") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or("flip must be a boolean")?,
        };
        let source = self.parameter_clipboard.clone().ok_or("the parameter clipboard is empty")?;
        let pairs = self.flip_pairs()?;
        let mut candidate = self.document.model.clone();
        let result = parameter_flip::paste_parameter(&mut candidate, &pairs, id, &source, if flip { 0 } else { 2 })?;
        self.commit_model(candidate)?;
        self.parameter_clipboard = None;
        Ok(result)
    }

    fn op_parameter_duplicate_flip(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let pairs = self.flip_pairs()?;
        let mut candidate = self.document.model.clone();
        let new_id = fresh_resource_id(&candidate)?;
        let copy = parameter_flip::duplicate_with_flip(&mut candidate, &pairs, id, new_id)?;
        self.commit_model(candidate)?;
        Ok(copy)
    }

    fn op_parameter_convert_to_2d(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let mut candidate = self.document.model.clone();
        let converted = parameter_flip::convert_to_2d(&mut candidate, id)?;
        self.commit_model(candidate)?;
        Ok(converted)
    }

    fn op_binding_copy(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let key = key_arg(request)?;
        let only = binding_keys(request, false)?;
        let mut scratch = self.document.model.clone();
        let clipboard = parameter_flip::copy_bindings(&mut scratch, id, key, only.as_deref())?;
        let summary = clipboard.summary();
        self.binding_clipboard = Some(clipboard);
        Ok(summary)
    }

    fn op_binding_paste(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let key = key_arg(request)?;
        let only = binding_keys(request, false)?;
        let clipboard = self.binding_clipboard.clone().ok_or("the binding clipboard is empty")?;
        let mut candidate = self.document.model.clone();
        let mut options = parameter_flip::PasteOptions::default();
        if let Some(ratio) = request.get("ratio") {
            options.ratio = ratio.as_f64().ok_or("ratio must be a number")?;
        }
        if let Some(invert) = request.get("invert") {
            let axis = match invert.get("axis").and_then(Value::as_str) {
                Some("vertical") => 0,
                Some("horizontal") => 1,
                _ => return Err("invert.axis must be vertical or horizontal".into()),
            };
            let origin = invert.get("origin").map(|value| value.as_f64().filter(|value| value.is_finite())
                .ok_or("invert.origin must be a finite number")).transpose()?;
            options.invert = Some((axis, origin));
        }
        let pasted = parameter_flip::paste_bindings(&mut candidate, &clipboard, id, key, only.as_deref(), options)?;
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "key": key, "pasted": pasted}))
    }

    /// "Copy to" and "Swap with": the selected bindings' keypoint goes to / is exchanged with `target`.
    fn op_binding_move_to_node(&mut self, request: &Value, swap: bool) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let key = key_arg(request)?;
        let only = binding_keys(request, true)?.unwrap_or_default();
        let target = required_id(request, "target")?;
        let mut candidate = self.document.model.clone();
        let moved = parameter_flip::move_to_node(&mut candidate, id, key, &only, target, swap)?;
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "key": key, "target": target, "bindings": moved}))
    }

    fn op_binding_multi_edit(&mut self, request: &Value) -> Result<Value, String> {
        let nodes = node_ids(request)?;
        let channel = required_string(request, "channel")?;
        let value = match request.get("value") {
            Some(Value::Array(_)) => number_array(request, "value")?,
            Some(value) => vec![value.as_f64().ok_or("value must be a number or an array of three")?],
            None => return Err("value is required".into()),
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("overwrite");
        let parameters = match request.get("parameters") {
            None => None,
            Some(list) => Some(list.as_array().ok_or("parameters must be an array")?.iter()
                .map(|id| id.as_u64().and_then(valid_id).ok_or("parameters must hold parameter UUIDs"))
                .collect::<Result<Vec<u32>, _>>()?),
        };
        let mut candidate = self.document.model.clone();
        let changed = parameter_flip::multi_edit(&mut candidate, &nodes, channel, &value, method, parameters.as_deref())?;
        if changed > 0 {
            self.commit_model(candidate)?;
        }
        Ok(json!({"changed": changed}))
    }

    /// Sets the blend-shape weight limits of one deform binding (issue #185); an empty list removes them.
    fn op_binding_weight_limits(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let node = required_id(request, "node")?;
        let limits = request.get("limits").and_then(Value::as_array).ok_or("limits must be an array")?;
        for limit in limits {
            let driver = limit.get("parameter").and_then(Value::as_u64).and_then(valid_id)
                .ok_or("each limit needs a parameter UUID")?;
            if driver == id || !parameter_edit::parameter_defaults(&self.document.model).contains_key(&driver) {
                return Err(format!("limit parameter {driver} must be another existing parameter"));
            }
            evaluation::parse_weight_points(limit.get("points"))?;
        }
        let mut candidate = self.document.model.clone();
        let slot = parameter_edit::locate_plain(&mut candidate, id)?;
        let parameter = &mut parameter_edit::container_of(&mut candidate, slot)?[parameter_edit::slot_index(slot)];
        let binding = parameter["bindings"].as_array_mut().into_iter().flatten()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node)
            .ok_or_else(|| format!("parameter {id} has no deform binding on node {node}"))?;
        let object = binding.as_object_mut().ok_or("binding must be an object")?;
        if limits.is_empty() {
            object.remove("weightLimits");
        } else {
            object.insert("weightLimits".into(), json!(limits));
        }
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "node": node, "limits": limits}))
    }

    fn op_binding_remove(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let keys = binding_keys(request, true)?.unwrap_or_default();
        let mut candidate = self.document.model.clone();
        let removed = parameter_edit::remove_bindings(&mut candidate, id, &keys)?;
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "removed": removed}))
    }

    fn op_binding_set_interpolation(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "parameter")?;
        let mode = required_string(request, "mode")?;
        let keys = binding_keys(request, true)?.unwrap_or_default();
        let mut candidate = self.document.model.clone();
        let changed = parameter_edit::set_interpolation(&mut candidate, id, &keys, mode)?;
        self.commit_model(candidate)?;
        Ok(json!({"parameter": id, "mode": mode, "changed": changed}))
    }

    fn op_history(&mut self, redo: bool) -> Result<Value, String> {
        let next = if redo {
            self.history_position
                .checked_add(1)
                .filter(|next| *next < self.history.len())
                .ok_or("nothing to redo")?
        } else {
            self.history_position
                .checked_sub(1)
                .ok_or("nothing to undo")?
        };
        self.restore_history_index(next)
    }

    fn history_summary(&self) -> Value {
        json!({
            "position": self.history_position,
            "count": self.history.len().saturating_sub(1),
            "limit": self.history_limit,
            "revision": self.revision,
            "states": (0..self.history.len()).map(|index| json!({
                "index": index,
                "label": self.history[index].label,
                "current": index == self.history_position,
                "applied": index <= self.history_position,
            })).collect::<Vec<_>>(),
        })
    }

    fn op_history_goto(&mut self, request: &Value) -> Result<Value, String> {
        let index = request["index"]
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .ok_or("history.goto requires a nonnegative integer index")?;
        let expected = request["expectedRevision"]
            .as_u64()
            .ok_or("history.goto requires an integer expectedRevision")?;
        if expected != self.revision {
            return Err(format!(
                "history.goto expected revision {expected}, but the session is at {}",
                self.revision
            ));
        }
        if index >= self.history.len() {
            return Err("history.goto index is outside the retained states".to_owned());
        }
        if index == self.history_position {
            return Ok(self.snapshot());
        }
        self.restore_history_index(index)
    }

    /// Set how many states the history retains. Shrinking drops the oldest states first and then the newest redo
    /// states, never the current one; the document is untouched.
    fn op_history_limit_set(&mut self, request: &Value) -> Result<Value, String> {
        let limit = request["limit"].as_u64().and_then(|limit| usize::try_from(limit).ok())
            .filter(|limit| HISTORY_LIMIT_RANGE.contains(limit))
            .ok_or_else(|| format!("history.limit.set requires an integer limit from {} to {}",
                HISTORY_LIMIT_RANGE.start(), HISTORY_LIMIT_RANGE.end()))?;
        self.history_limit = limit;
        let excess = self.history.len().saturating_sub(limit);
        if excess > 0 {
            let oldest = excess.min(self.history_position);
            self.history.drain(..oldest);
            self.history_position -= oldest;
            self.history.truncate(limit);
            // Retained indexes changed, so pending history.goto requests must not match.
            self.revision = self.revision.saturating_add(1);
        }
        Ok(self.snapshot())
    }

    /// Forget every retained state except the current one; the document is untouched.
    fn op_history_clear(&mut self) -> Value {
        if self.history.len() > 1 {
            self.history.drain(..self.history_position);
            self.history.truncate(1);
            self.history[0].label.clear();
            self.history_position = 0;
            // Retained indexes changed, so pending history.goto requests must not match.
            self.revision = self.revision.saturating_add(1);
        }
        self.snapshot()
    }

    fn restore_history_index(&mut self, next: usize) -> Result<Value, String> {
        let native_motion_state = self
            .native_motion_preview
            .as_ref()
            .map(|preview| (preview.name.clone(), preview.time, preview.fade));
        self.deform_cursor = None;
        self.deform_partial.clear();
        let model = self.history[next].model.clone();
        if let Some(sections) = self.history[next].sections.clone() {
            self.document.set_inx_sections(sections);
        }
        if let Some(payloads) = self.history[next].payloads.clone() {
            self.document.set_native_payloads(payloads);
        }
        self.preview_deformation.clear();
        self.animation_preview = None;
        self.native_motion_preview = None;
        self.native_playback.reset_animation();
        self.reset_physics();
        if let Some(pose) = &mut self.native_pose { pose.reset(); }
        self.document.model = model;
        self.authoring_scene = None;
        self.native_scene = None;
        self.history_position = next;
        self.revision = self.revision.saturating_add(1);
        self.sync_runtime_values();
        if self.document.is_native() {
            self.sync_native_playback_values();
            if let Some((name, time, fade)) = native_motion_state {
                let _ = self.seek_motion3(&name, time, fade);
            }
        }
        Ok(self.snapshot())
    }

    fn op_deform_continue(&mut self) -> Result<Value, String> {
        let Some(cells) = self.deform_cursor.clone() else {
            return Ok(json!({ "pending": false }));
        };
        let flush = dirty::continue_cells(
            &self.document.model,
            &self.runtime_values,
            &cells,
            self.deform_partial.clone(),
            Some(dirty::ALL_KEYPOINTS_BUDGET),
        )?;
        self.rewrite_current(flush.model)?;
        self.store_deform_cursor(flush.pending, flush.partial);
        for (id, offsets) in flush.preview {
            self.preview_deformation.insert(id, offsets);
        }
        Ok(json!({ "pending": self.deform_cursor.is_some() }))
    }

    fn op_save(&mut self, request: &Value) -> Result<Value, String> {
        let path = required_string(request, "path")?;
        let native_extension = if self.document.is_animation() { "can3" } else { "cmo3" };
        if !self.document.is_inx() && !Path::new(path).extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(native_extension)) {
            return Err(format!("native Live2D save requires a .{native_extension} destination"));
        }
        let mark_saved = request
            .get("mark_saved")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let target = normalize_target_path(Path::new(path))?;
        match fs::read(&target) {
            Ok(current) if self.file_path.as_deref() == Some(target.as_path()) => {
                if self.file_baseline.as_deref() != Some(current.as_slice()) {
                    return Err(
                        "save refused because the file changed since it was opened or saved".into(),
                    );
                }
            }
            Ok(_) => return Err("save destination already exists".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.file_path.as_deref() == Some(target.as_path())
                    && self.file_baseline.is_some()
                {
                    return Err("save refused because the opened file was removed".into());
                }
            }
            Err(error) => return Err(format!("could not inspect save destination: {error}")),
        }
        let staged = self.staged_flush()?;
        let written = self.model_for_save(staged.as_ref().map_or(&self.document.model, |flush| &flush.model).clone());
        let bytes = self.document.with_model(written).encode()?;
        #[cfg(test)]
        if self.fail_save_after_staging {
            self.fail_save_after_staging = false;
            return Err("could not sync temporary save file: injected failure".into());
        }
        write_atomic(&target, &bytes, true)?;
        if let Some(flush) = staged {
            self.rewrite_current(flush.model)?;
            self.store_deform_cursor(Vec::new(), Vec::new());
            for (id, offsets) in flush.preview {
                self.preview_deformation.insert(id, offsets);
            }
        }
        if mark_saved {
            self.file_path = Some(target);
            self.file_baseline = Some(bytes);
            self.saved_model = self.document.model.clone();
            self.saved_tail = self.document.tail().to_vec();
            self.saved_payloads = self.document.native_payloads();
            self.depth_draw.unsaved = false;
        } else if self.file_path.as_deref() == Some(target.as_path()) {
            self.file_baseline = Some(bytes);
        }
        Ok(json!({ "path": path }))
    }

    /// The model a file write must contain when a deform refresh is still pending: the refresh run to the
    /// end on a copy, `None` when nothing is pending.
    fn staged_flush(&self) -> Result<Option<dirty::Flush>, String> {
        let Some(cells) = self.deform_cursor.clone() else {
            return Ok(None);
        };
        let flush = dirty::continue_cells(
            &self.document.model,
            &self.runtime_values,
            &cells,
            self.deform_partial.clone(),
            None,
        )?;
        if !flush.pending.is_empty() {
            return Err("deform refresh did not finish".into());
        }
        Ok(Some(flush))
    }

    /// `export.inp`: D's INP export of the open INX document to `<path>` with the `.inp` extension. The
    /// session keeps its history, modified state and document path.
    fn op_export_inp(&self, request: &Value) -> Result<Value, String> {
        if !self.document.is_inx() {
            return Err("export.inp needs an INP/INX document".into());
        }
        let path = required_string(request, "path")?;
        let overwrite = match request.get("overwrite") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or("overwrite must be a boolean")?,
        };
        let options = inp_export::InpExportOptions::from_request(request)?;
        let named = Path::new(path).with_extension("inp");
        let target = normalized_export_target(path, &named, self.file_path.as_deref(), "export.inp")?;
        if !overwrite && target.exists() {
            return Err("export destination already exists".into());
        }
        let staged = self.staged_flush()?;
        let model = staged.as_ref().map_or(&self.document.model, |flush| &flush.model);
        let export = inp_export::export_inp_report(
            model,
            &self.model_textures(),
            self.document.extension_bytes()?,
            &options,
        )?;
        write_atomic(&target, &export.bytes, overwrite)?;
        Ok(json!({
            "path": named.to_string_lossy(),
            "bytes": export.bytes.len(),
            "atlases": export.atlas_count,
            "outputScale": f64::from(export.output_scale),
            "prunedNodes": export.pruned_nodes,
            "skippedParts": export.skipped_parts,
            "unbakedDeformers": export.unbaked_deformers,
        }))
    }

    fn extension_key(request: &Value) -> Result<&str, String> {
        let key = required_string(request, "key")?;
        if key.is_empty() || key.len() > MAX_EXT_KEY_BYTES {
            return Err(format!("key must be 1 to {MAX_EXT_KEY_BYTES} bytes"));
        }
        Ok(key)
    }

    /// `extdata.get`: one named payload of the INX extension section.
    fn op_extdata_get(&self, request: &Value) -> Result<Value, String> {
        if !self.document.is_inx() {
            return Err("extdata.get needs an INP/INX document".into());
        }
        let key = Self::extension_key(request)?;
        let entries = self.document.extension_entries()?;
        let payload = entries.iter().find(|(name, _)| name == key.as_bytes()).map(|(_, payload)| payload);
        Ok(json!({
            "key": key,
            "present": payload.is_some(),
            "base64": payload.map(|payload| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload)),
        }))
    }

    /// `extdata.set`: store (`base64` string) or remove (`base64: null`) one named payload of the
    /// extension section as one undo step. A request that changes nothing records nothing.
    fn op_extdata_set(&mut self, request: &Value) -> Result<Value, String> {
        if !self.document.is_inx() {
            return Err("extdata.set needs an INP/INX document".into());
        }
        let key = Self::extension_key(request)?;
        let payload = match request.get("base64") {
            Some(Value::Null) => None,
            Some(Value::String(text)) if text.len() <= MAX_EXT_PAYLOAD_BYTES / 3 * 4 + 4 => Some(
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text)
                    .map_err(|error| format!("base64 is not valid: {error}"))?,
            ),
            Some(Value::String(_)) => return Err("base64 payload is too large".into()),
            Some(_) => return Err("base64 must be a string or null".into()),
            None => return Err("base64 is required (null removes the key)".into()),
        };
        if payload.as_ref().is_some_and(|payload| payload.len() > MAX_EXT_PAYLOAD_BYTES) {
            return Err("base64 payload is too large".into());
        }
        let old = self.document.extension_entries()?;
        let mut entries = old.clone();
        let at = entries.iter().position(|(name, _)| name == key.as_bytes());
        match (at, &payload) {
            (Some(at), Some(payload)) => entries[at].1 = payload.clone(),
            (Some(at), None) => {
                entries.remove(at);
            }
            (None, Some(_)) if entries.len() >= MAX_EXT_ENTRIES => {
                return Err(format!("the document already has {MAX_EXT_ENTRIES} extension entries"));
            }
            (None, Some(payload)) => entries.push((key.as_bytes().to_vec(), payload.clone())),
            (None, None) => {}
        }
        let result = json!({
            "key": key,
            "present": payload.is_some(),
            "bytes": payload.as_ref().map_or(0, Vec::len),
            "changed": entries != old,
        });
        if entries != old {
            let sections = self.document.sections_with_extensions(&entries)?;
            self.commit_with_sections(self.document.model.clone(), sections)?;
        }
        Ok(result)
    }

    /// `texture.get`: the stored bytes of one texture slot with its decoded size.
    fn op_texture_get(&mut self, request: &Value) -> Result<Value, String> {
        let slot = request
            .get("slot")
            .and_then(Value::as_u64)
            .and_then(|slot| usize::try_from(slot).ok())
            .ok_or("slot must be a nonnegative integer")?;
        let native = self.document.is_native() || (self.document.is_animation() && self.native_playback.model.is_some());
        let textures = if native { self.active_native_scene()?.textures() } else { self.document.textures() };
        let blob = textures
            .get(slot)
            .ok_or_else(|| format!("texture slot {slot} is out of range ({} textures)", textures.len()))?;
        let format = match blob.tag {
            0 => "png",
            1 => "tga",
            tag => return Err(format!("texture slot {slot} has unsupported image tag {tag}")),
        };
        let [width, height] = render::texture_size(blob, slot)?;
        Ok(json!({
            "slot": slot,
            "width": width,
            "height": height,
            "format": format,
            "base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &blob.data),
        }))
    }

    /// `model.statistics`: counts over the tree that is rendered. An offscreen pass is a `Composite` or
    /// `DynamicComposite` node; a CMO3 Part with offscreen drawing becomes one in the native render model.
    fn op_model_statistics(&mut self) -> Result<Value, String> {
        let native_model;
        let model = if self.document.is_native()
            || (self.document.is_animation() && self.native_playback.model.is_some())
        {
            native_model = self.native_render_scene(&json!({}))?.0;
            &native_model
        } else {
            &self.document.model
        };
        let (nodes, mask_references, offscreen_depth) =
            render::packet_scene_limits(model.get("nodes").ok_or("model.nodes is missing")?)?;
        Ok(json!({
            "nodes": nodes,
            "maskReferences": mask_references,
            "maxOffscreenDepth": offscreen_depth,
            "offscreenDepthLimit": render::MAX_COMPOSITE_DEPTH,
        }))
    }

    /// `scene.geometry`: the drawn Parts in world space in painter order, evaluated like `render.packet`
    /// but without texture pixels and without the limits that belong to drawing them.
    fn op_scene_geometry(&mut self, request: &Value) -> Result<Value, String> {
        if self.document.is_native()
            || (self.document.is_animation() && self.native_playback.model.is_some())
        {
            let (evaluated, geometry, warnings) = self.native_render_scene(request)?;
            let mut result = render::scene_geometry_for_scene(
                &evaluated,
                &geometry,
                self.active_native_scene()?.textures(),
            )?;
            result["warnings"] = json!(warnings);
            return Ok(result);
        }
        render::packet_scene_limits(self.document.model.get("nodes").ok_or("model.nodes is missing")?)?;
        let (evaluated, scene) = self.evaluate_scene(request)?;
        let mut result = render::scene_geometry_for_scene(&evaluated, &scene, self.document.textures())?;
        // The editor viewport does not pick a Part that carries this.
        let locked = nodes::locked_ids(&self.document.model);
        for part in result["parts"].as_array_mut().into_iter().flatten() {
            if part["id"].as_u64().and_then(|id| u32::try_from(id).ok()).is_some_and(|id| locked.contains(&id)) {
                part["locked"] = json!(true);
            }
        }
        Ok(result)
    }

    /// Drawn ArtMeshes with an advanced blend (every mesh with `all`) whose own triangles overlap in the current
    /// pose, which advanced blending can draw twice and flicker (issue #197).
    fn op_scene_overlap(&mut self, request: &Value) -> Result<Value, String> {
        if self.document.is_native() || self.document.is_animation() {
            return Err("scene.overlap reads INX documents only".into());
        }
        let all = request.get("all").and_then(Value::as_bool).unwrap_or(false);
        let mut geometry_request = request.clone();
        if let Some(object) = geometry_request.as_object_mut() {
            object.remove("all");
        }
        let geometry = self.op_scene_geometry(&geometry_request)?;
        let mut meshes = Vec::new();
        for part in geometry["parts"].as_array().into_iter().flatten() {
            let node = part["id"].as_u64().and_then(|id| u32::try_from(id).ok())
                .and_then(|id| nodes::find_node(&self.document.model, id));
            let blend = node.and_then(|node| node["blend_mode"].as_str()).unwrap_or("Normal");
            if !all && !node.is_some_and(advanced_blend) {
                continue;
            }
            let vertices: Vec<[f64; 2]> = serde_json::from_value(part["vertices"].clone())
                .map_err(|error| format!("scene vertices: {error}"))?;
            let indices: Vec<u32> = serde_json::from_value(part["indices"].clone())
                .map_err(|error| format!("scene indices: {error}"))?;
            let pairs = render::overlapping_triangles(&vertices, &indices);
            if !pairs.is_empty() {
                meshes.push(json!({"id": part["id"], "name": node.map(|node| node["name"].clone()), "blendMode": blend,
                    "pairs": pairs.len(), "triangles": pairs[..pairs.len().min(16)]}));
            }
        }
        Ok(json!({"meshes": meshes}))
    }

    fn op_export(&self) -> Result<Value, String> {
        // Save keeps rig nodes. Export drops DepthBone and DepthRigRoot.
        let exported = self.document.with_model(depths::model_for_export(&self.document.model)?);
        let bytes = exported.encode()?;
        Ok(json!({ "bytes": bytes }))
    }

    fn op_set_trs(&mut self, request: &Value) -> Result<Value, String> {
        let mut candidate = self.document.model.clone();
        let result = evaluation::set_trs(&mut candidate, request)?;
        validate_model(&candidate)?;
        let parameter = required_id(request, "parameter")?;
        let nodes = node_ids(request)?;
        let key = key_index(request)?;
        let marks = dirty::mark_pose(&candidate, parameter, &nodes, key);
        self.commit_marked(candidate, marks)?;
        Ok(result)
    }

    fn op_set_deform(&mut self, request: &Value) -> Result<Value, String> {
        let mut candidate = self.document.model.clone();
        let updated = deformers::set_deform(&mut candidate, request)?;
        self.commit_model(candidate)?;
        Ok(json!({"updated": updated}))
    }

    fn op_evaluate(&self, request: &Value) -> Result<Value, String> {
        let values = self.request_values(request)?;
        evaluation::evaluate(&self.document.model, &values)
    }

    fn op_scene_ambient_set(&mut self, request: &Value) -> Result<Value, String> {
        let color = request
            .get("color")
            .and_then(Value::as_array)
            .filter(|color| color.len() == 3)
            .and_then(|color| {
                let mut out = [0.0; 3];
                for (slot, value) in out.iter_mut().zip(color) {
                    *slot = value.as_f64().filter(|v| (0.0..=1.0).contains(v))?;
                }
                Some(out)
            })
            .ok_or("color must be three finite numbers in 0..=1")?;
        self.scene_ambient = color;
        Ok(json!({ "ambientLight": color }))
    }

    fn op_value_set(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_id(request, "id")?;
        let value = required_pair(request, "value")?;
        let value = checked_parameter_value(&self.document.model, id, value)?;
        let previous = self.runtime_values.insert(id, value);
        if let Err(error) = self.refresh_pose_preview(id) {
            match previous {
                Some(value) => {
                    self.runtime_values.insert(id, value);
                }
                None => {
                    self.runtime_values.remove(&id);
                }
            }
            return Err(error);
        }
        self.physics_output = None;
        self.animation_preview = None;
        if let Some(recording) = &mut self.recording {
            recording.samples.push((id, recording.frame(), value));
        }
        Ok(json!({ "id": id, "value": value }))
    }

    fn op_animation_record_start(&mut self, request: &Value) -> Result<Value, String> {
        if let Some(recording) = &self.recording {
            return Err(format!("already recording into animation {:?}", recording.name));
        }
        let name = required_string(request, "name")?;
        let animation = self.document.model["animations"].get(name)
            .ok_or_else(|| format!("animation {name:?} was not found"))?;
        let length = animation::validate_animation_header(animation)?;
        let timestep =
            animation["timestep"].as_f64().filter(|step| *step > 0.0).ok_or("animation.timestep must be positive")?;
        let start_frame = match request.get("frame") {
            Some(frame) => frame.as_u64().filter(|frame| *frame < length as u64)
                .ok_or_else(|| format!("frame must be between 0 and {}", length.saturating_sub(1)))? as i64,
            None => 0,
        };
        self.recording = Some(Recording {
            name: name.to_owned(),
            started: std::time::Instant::now(),
            start_frame,
            timestep,
            before: self.runtime_values.clone(),
            samples: Vec::new(),
        });
        Ok(json!({"name": name, "frame": start_frame}))
    }

    fn animation_record_status(&self) -> Value {
        match &self.recording {
            None => json!({"recording": false}),
            Some(recording) => json!({"recording": true, "name": recording.name, "startFrame": recording.start_frame,
                "frame": recording.frame(), "samples": recording.samples.len()}),
        }
    }

    /// Ends the recording and, unless `discard`, keys every recorded parameter in one undo: its value before the
    /// recording on the start frame, then the last value set within each later frame.
    fn op_animation_record_stop(&mut self, request: &Value) -> Result<Value, String> {
        let recording = self.recording.take().ok_or("not recording")?;
        if request.get("discard").and_then(Value::as_bool) == Some(true) {
            return Ok(json!({"name": recording.name, "discarded": recording.samples.len()}));
        }
        // A parameter deleted while recording drops its samples; any other failure keeps the take for a retry.
        let mut keys = BTreeMap::new();
        let mut missing = 0;
        for &(id, frame, value) in &recording.samples {
            let Some(parameter) = find_parameter(&self.document.model, id) else {
                missing += 1;
                continue;
            };
            let before = match recording.before.get(&id) {
                Some(value) => *value,
                None => match parse_pair(&parameter["defaults"], "defaults") {
                    Ok(value) => value,
                    Err(error) => {
                        self.recording = Some(recording);
                        return Err(error);
                    }
                },
            };
            keys.entry((id, recording.start_frame)).or_insert(before);
            keys.insert((id, frame), value);
        }
        let parameters = keys.keys().map(|(id, _)| id).collect::<HashSet<_>>().len();
        let (candidate, written, dropped) = match animation::record_keys(&self.document.model, &recording.name, &keys) {
            Ok(result) => result,
            Err(error) => {
                self.recording = Some(recording);
                return Err(error);
            }
        };
        if written > 0 {
            if let Err(error) = self.commit_model(candidate) {
                self.recording = Some(recording);
                return Err(error);
            }
        }
        Ok(json!({"name": recording.name, "parameters": parameters, "keys": written, "dropped": dropped + missing}))
    }

    fn op_evaluate_scene(&self, request: &Value) -> Result<Value, String> {
        let (_, scene) = self.evaluate_scene(request)?;
        Ok(json!({"worldMatrices": scene.world_matrices, "localOffsets": scene.local_offsets,
            "worldOffsets": scene.world_offsets}))
    }

    fn evaluate_scene(&self, request: &Value,
    ) -> Result<(Value, scene_deform::SceneDeform), String> {
        let values = self.request_values(request)?;
        let evaluated = evaluation::evaluate(&self.document.model, &values)?;
        let mut deformations = evaluation::raster_deformations(&self.document.model, &values)?;
        evaluation::overlay_preview(&mut deformations, if request.get("values").is_some() {
            &self.preview_deformation
        } else {
            self.effective_deformations()
        },
        );
        if let Some(input) = request.get("deformations") {
            let overrides: BTreeMap<u32, Vec<[f64; 2]>> = serde_json::from_value(input.clone())
                .map_err(|error| format!("invalid scene deformations: {error}"))?;
            for (id, offsets) in overrides {
                if nodes::find_node(&evaluated, id).is_none() {
                    return Err(format!("scene deformation node {id} was not found"));
                }
                deformations.insert(id, offsets);
            }
        }
        let own = request.get("values").is_none() && request.get("deformations").is_none();
        let physics = self.physics_output.as_ref().filter(|_| own);
        let forms = self.animation_preview.as_ref().map(|preview| &preview.forms).filter(|_| own);
        if let Some(forms) = forms {
            forms.add_local(&mut deformations);
        }
        let mut scene = scene_deform::evaluate_with_path_update(
            &evaluated,
            self.document.model.get("nodes").ok_or("model.nodes is missing")?,
            &deformations,
            &mut |id, _, _, _, _| {
                Ok(physics.and_then(|output| output.path_preprocess.get(&id)).cloned())
            },
        )?;
        if let Some(forms) = forms {
            forms.add_world(&mut scene);
        }
        Ok((evaluated, scene))
    }

    fn op_render(&mut self, request: &Value) -> Result<Value, String> {
        let width = request
            .get("width")
            .and_then(Value::as_u64)
            .filter(|width| *width > 0 && *width <= u32::MAX as u64)
            .ok_or("width must be a positive u32")? as u32;
        let height = request
            .get("height")
            .and_then(Value::as_u64)
            .filter(|height| *height > 0 && *height <= u32::MAX as u64)
            .ok_or("height must be a positive u32")? as u32;
        if u64::from(width) * u64::from(height) > MAX_RENDER_PIXELS {
            return Err("render dimensions exceed the pixel limit".into());
        }
        let bounds = required_quad(request, "bounds")?;
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            return Err("bounds must have increasing min and max coordinates".into());
        }
        let target = match request.get("path") {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) => Some(normalized_export_target(path, Path::new(path), self.file_path.as_deref(), "render")?),
            Some(_) => return Err("path must be a string".into()),
        };
        if self.document.is_native()
            || (self.document.is_animation() && self.native_playback.model.is_some())
        {
            if ["solo", "solo_cover"].iter().any(|key| request.get(*key).is_some_and(|value| !value.is_null())) {
                return Err("solo needs an INX document".into());
            }
            let (evaluated, geometry, warnings) = self.native_render_scene(request)?;
            let ambient = self.scene_ambient.map(|c| c as f32);
            let png = render::render_png_lit(
                &evaluated,
                &geometry,
                self.active_native_scene()?.textures(),
                width,
                height,
                bounds,
                ambient,
            )?;
            let mut result = json!({
                "width": width, "height": height,
                "png": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png),
                "warnings": warnings
            });
            if let Some(target) = &target {
                write_atomic(target, &png, true)?;
                result["path"] = request["path"].clone();
            }
            return Ok(result);
        }
        let solo = match request.get("solo") {
            None | Some(Value::Null) => None,
            Some(solo) => Some(solo_nodes(self.document.model.get("nodes").ok_or("model.nodes is missing")?, solo)?),
        };
        let cover = match request.get("solo_cover") {
            None | Some(Value::Null) => None,
            Some(_) if solo.is_none() => return Err("solo_cover needs solo".into()),
            Some(cover) => Some(
                cover
                    .as_array()
                    .and_then(|channels| {
                        channels.iter().map(|c| u8::try_from(c.as_u64()?).ok()).collect::<Option<Vec<u8>>>()
                    })
                    .and_then(|channels| <[u8; 4]>::try_from(channels).ok())
                    .ok_or("solo_cover must be [r, g, b, a], each an integer from 0 to 255")?,
            ),
        };
        let (evaluated, scene) = self.evaluate_scene(request)?;
        let ambient = self.scene_ambient.map(|c| c as f32);
        let textures = self.document.textures();
        let png = match &solo {
            Some(only) => {
                render::render_png_solo(&evaluated, &scene, textures, width, height, bounds, ambient, only, cover)?
            }
            None => render::render_png_lit(&evaluated, &scene, textures, width, height, bounds, ambient)?,
        };
        let mut result = json!({ "width": width, "height": height, "png": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png) });
        if let Some(target) = &target {
            write_atomic(target, &png, true)?;
            result["path"] = request["path"].clone();
        }
        Ok(result)
    }

    fn op_render_packet(&mut self, request: &Value) -> Result<Value, String> {
        if self.document.is_native()
            || (self.document.is_animation() && self.native_playback.model.is_some())
        {
            let (evaluated, geometry, warnings) = self.native_render_scene(request)?;
            let mut packet = render::render_packet_for_scene(
                &evaluated,
                &geometry,
                self.active_native_scene()?.textures(),
            )?;
            packet["warnings"] = json!(warnings);
            return Ok(packet);
        }
        render::packet_scene_limits(self.document.model.get("nodes").ok_or("model.nodes is missing")?,
        )?;
        let (evaluated, scene) = self.evaluate_scene(request)?;
        render::render_packet_for_scene(&evaluated, &scene, self.document.textures())
    }

    fn op_set_weld(&mut self, request: &Value) -> Result<Value, String> {
        let part = Self::request_id(request, "part")?;
        let target = Self::request_id(request, "target")?;
        // Without indices each vertex welds to the nearest target vertex within 4 units, as D's inspector did.
        let indices = match request.get("indices").filter(|value| !value.is_null()) {
            None => {
                let indices = weld::nearest_indices(&self.document.model, part, target, 4.0)?;
                if indices.iter().all(|index| *index == -1) {
                    return Err("no vertex of the part is within 4 units of a target vertex".into());
                }
                indices
            }
            Some(indices) => indices
                .as_array()
                .ok_or("weld indices must be an array")?
                .iter()
                .map(|value| value.as_i64().ok_or_else(|| "weld index must be an integer".to_string()))
                .collect::<Result<Vec<_>, _>>()?,
        };
        let weight = match request.get("weight").filter(|value| !value.is_null()) {
            None => 0.5,
            Some(weight) => weight.as_f64().filter(|weight| weight.is_finite()).ok_or("weld weight must be finite")?,
        };
        let candidate = weld::set_weld(&self.document.model, part, target, &indices, weight)?;
        self.commit_model(candidate)?;
        Ok(json!({ "part": part, "target": target, "indices": indices }))
    }

    fn op_clear_weld(&mut self, request: &Value) -> Result<Value, String> {
        let part = Self::request_id(request, "part")?;
        let target = Self::request_id(request, "target")?;
        let candidate = weld::clear_weld(&self.document.model, part, target)?;
        self.commit_model(candidate)?;
        Ok(json!({ "part": part, "target": target }))
    }

    fn request_id(request: &Value, key: &str) -> Result<u32, String> {
        request
            .get(key)
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or_else(|| format!("{key} must be a positive resource UUID"))
    }

    fn op_bounds(&mut self, request: &Value) -> Result<Value, String> {
        if self.document.is_native()
            || (self.document.is_animation() && self.native_playback.model.is_some())
        {
            let (evaluated, geometry, warnings) = self.native_render_scene(request)?;
            return Ok(json!({
                "bounds": render::scene_bounds_for_scene(&evaluated, &geometry)?,
                "warnings": warnings
            }));
        }
        let (evaluated, scene) = self.evaluate_scene(request)?;
        Ok(json!({"bounds": render::scene_bounds_for_scene(&evaluated, &scene)?}))
    }

    fn request_values(&self, request: &Value) -> Result<BTreeMap<u32, [f64; 2]>, String> {
        let mut values = if request.get("values").is_some() {
            self.runtime_values.clone()
        } else {
            self.effective_values().clone()
        };
        let Some(input) = request.get("values") else {
            return Ok(values);
        };
        let object = input
            .as_object()
            .ok_or("values must be an object keyed by parameter UUID")?;
        for (key, raw) in object {
            let id = key
                .parse::<u32>()
                .map_err(|_| format!("invalid parameter UUID key: {key}"))?;
            let value = parse_pair(raw, "parameter value")?;
            let value = checked_parameter_value(&self.document.model, id, value)?;
            values.insert(id, value);
        }
        Ok(values)
    }

    fn refresh_pose_preview(&mut self, parameter: u32) -> Result<(), String> {
        let preview = dirty::preview_pose(&self.document.model, &self.runtime_values, parameter)?;
        for (id, offsets) in preview {
            self.preview_deformation.insert(id, offsets);
        }
        Ok(())
    }

    fn commit_marked(&mut self, candidate: Value, marks: Vec<dirty::Mark>) -> Result<(), String> {
        self.commit_marked_keeping(candidate, marks, false)
    }

    /// `commit_marked` that also swaps in `sections` (changed extension data) as part of the same undo step.
    fn commit_marked_with_sections(
        &mut self,
        candidate: Value,
        marks: Vec<dirty::Mark>,
        sections: Option<Arc<document::InxSections>>,
    ) -> Result<(), String> {
        let Some(sections) = sections else {
            return self.commit_marked(candidate, marks);
        };
        let previous = self.document.inx_sections();
        self.document.set_inx_sections(sections);
        // The step is kept even when the model is unchanged: the sections changed.
        let result = self.commit_marked_keeping(candidate, marks, true);
        if let (Err(_), Some(previous)) = (&result, previous) {
            self.document.set_inx_sections(previous);
        }
        result
    }

    fn commit_marked_keeping(&mut self, candidate: Value, marks: Vec<dirty::Mark>, keep: bool) -> Result<(), String> {
        let flush = dirty::flush(&candidate, &self.runtime_values, &marks)?;
        let keep = keep || !flush.pending.is_empty();
        self.commit_model_keeping(flush.model, keep)?;
        self.store_deform_cursor(flush.pending, flush.partial);
        for (id, offsets) in flush.preview {
            self.preview_deformation.insert(id, offsets);
        }
        Ok(())
    }

    fn rewrite_current(&mut self, candidate: Value) -> Result<(), String> {
        validate_model(&candidate)?;
        while self.history.len() > self.history_position + 1 {
            self.history.pop_back();
        }
        if self.history.is_empty() {
            self.history.push_back(HistoryState {
                model: candidate.clone(),
                sections: self.document.inx_sections(),
                payloads: self.document.native_payloads(),
                label: String::new(),
                split_applications: part_split::Applications::default(),
            });
            self.history_position = 0;
        } else {
            self.history[self.history_position].split_applications =
                self.history[self.history_position].split_applications.carried_into(&candidate);
            self.history[self.history_position].model = candidate.clone();
        }
        let changed = self.document.model != candidate;
        self.document.model = candidate;
        if changed {
            self.authoring_scene = None;
            self.animation_preview = None;
            self.reset_physics();
            self.revision = self.revision.saturating_add(1);
            self.sync_runtime_values();
        }
        Ok(())
    }

    fn commit_targets(&mut self, candidate: Value, targets: &[u32]) -> Result<(), String> {
        let mut marks = Vec::new();
        for target in targets {
            marks.extend(dirty::mark_target(&candidate, *target));
        }
        self.commit_marked(candidate, marks)
    }

    fn commit_model(&mut self, candidate: Value) -> Result<(), String> {
        self.commit_model_keeping(candidate, false)
    }

    fn store_deform_cursor(&mut self, pending: Vec<dirty::Cell>, partial: Vec<[f64; 2]>) {
        if pending.is_empty() {
            self.deform_cursor = None;
            self.deform_partial.clear();
        } else {
            self.deform_cursor = Some(pending);
            self.deform_partial = partial;
        }
    }

    fn commit_model_keeping(&mut self, candidate: Value, keep_duplicate: bool,
    ) -> Result<(), String> {
        if candidate == self.document.model && !keep_duplicate {
            return Ok(());
        }
        if self.default_form_locked {
            if let Some((parameter, node, name)) = changed_default_form(&self.document.model, &candidate) {
                return Err(format!(
                    "the default forms are locked: this edit changes the {name} keyform of node {node} at the default \
                     value of parameter {parameter}; turn the lock off with default_form_lock_set or the \
                     \"Lock default forms\" check box"
                ));
            }
        }
        if let Some(refusal) = animation::frozen_change(&self.document.model, &candidate) {
            return Err(refusal);
        }
        self.document.validate_candidate(&candidate)?;
        let payload_changed = self.history[self.history_position].payloads != self.document.native_payloads();
        let native_motion_state = self
            .native_motion_preview
            .as_ref()
            .map(|preview| (preview.name.clone(), preview.time, preview.fade));
        self.preview_deformation.clear();
        self.animation_preview = None;
        self.native_motion_preview = None;
        self.native_playback.reset_animation();
        self.reset_physics();
        if let Some(pose) = &mut self.native_pose { pose.reset(); }
        while self.history.len() > self.history_position + 1 {
            self.history.pop_back();
        }
        self.history.push_back(HistoryState {
            model: candidate.clone(),
            sections: self.document.inx_sections(),
            payloads: self.document.native_payloads(),
            label: self.history_op.clone(),
            split_applications: self.history[self.history_position]
                .split_applications
                .carried_into(&candidate),
        });
        if self.history.len() > self.history_limit {
            self.history.pop_front();
        }
        self.history_position = self.history.len() - 1;
        if self.document.model != candidate || payload_changed {
            self.authoring_scene = None;
            self.native_scene = None;
        }
        self.document.model = candidate;
        self.revision = self.revision.saturating_add(1);
        self.sync_runtime_values();
        if self.document.is_native() {
            self.sync_native_playback_values();
            if let Some((name, time, fade)) = native_motion_state {
                let _ = self.seek_motion3(&name, time, fade);
            }
        }
        Ok(())
    }

    fn sync_native_playback_values(&mut self) {
        let Some(parameters) = self.document.model["parameters"].as_array().cloned() else {
            self.native_runtime_values.clear();
            self.native_playback.retain_parameters(&[]);
            return;
        };
        self.native_playback.retain_parameters(&parameters);
        let Ok(defaults) = native_parameter_defaults(&parameters) else {
            return;
        };
        let ranges = parameters
            .iter()
            .filter_map(|parameter| {
                Some((
                    parameter["id"].as_str()?,
                    (
                        parameter["minimum"].as_f64()?,
                        parameter["maximum"].as_f64()?,
                    ),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        self.native_runtime_values.retain(|id, value| {
            ranges.get(id.as_str()).is_some_and(|(minimum, maximum)| {
                value.is_finite() && minimum <= maximum && *value >= *minimum && *value <= *maximum
            })
        });
        for (id, value) in defaults {
            self.native_runtime_values.entry(id).or_insert(value);
        }
    }

    fn views_json(&self) -> Value {
        json!({
            "views": self.views.iter().map(|view| json!({"id": view.id, "active": view.id == self.active_view}))
                .collect::<Vec<_>>(),
            "active": self.active_view,
            "synchronized": self.views_synchronized,
        })
    }

    fn op_view_add(&mut self) -> Result<Value, String> {
        if self.views.len() >= MAX_VIEWS {
            return Err(format!("a document has at most {MAX_VIEWS} views"));
        }
        let id = self.views.iter().map(|view| view.id).max().unwrap_or(0) + 1;
        // A new view shows the default forms as an opened document does; `activate` fills in every default.
        self.views.push(ViewSlot { id, preview: Some((self.revision, BTreeMap::new())), ..ViewSlot::default() });
        self.activate_view(id)?;
        Ok(self.views_json())
    }

    fn op_view_select(&mut self, request: &Value) -> Result<Value, String> {
        self.activate_view(required_view(request)?)?;
        Ok(self.views_json())
    }

    fn op_view_remove(&mut self, request: &Value) -> Result<Value, String> {
        let id = required_view(request)?;
        let index = self.views.iter().position(|view| view.id == id).ok_or_else(|| format!("view {id} was not found"))?;
        if self.views.len() == 1 {
            return Err("the last view cannot be removed".into());
        }
        if id == self.active_view {
            let next = self.views[if index + 1 < self.views.len() { index + 1 } else { index - 1 }].id;
            self.activate_view(next)?;
        }
        self.views.remove(index);
        Ok(self.views_json())
    }

    /// Makes `id` the active view: the values of the view that was active go to its slot and the values of `id`
    /// come out of its slot, fitted to the parameters the document has now (a removed parameter is dropped, and a
    /// value outside its range now falls back to the default, as after an edit).
    fn activate_view(&mut self, id: u32) -> Result<(), String> {
        let index = self.views.iter().position(|view| view.id == id).ok_or_else(|| format!("view {id} was not found"))?;
        if id == self.active_view || self.views_synchronized {
            self.active_view = id;
            return Ok(());
        }
        let mut values = self.views[index].values.clone();
        let defaults = default_values(&self.document.model);
        values.retain(|id, value| {
            checked_parameter_value(&self.document.model, *id, *value).map(|checked| *value = checked).is_ok()
        });
        for (id, value) in defaults {
            values.entry(id).or_insert(value);
        }
        // The Depth Bone pose preview the view left with, unless an edit came since; then the one `value.set` makes.
        let preview = match &self.views[index].preview {
            Some((revision, preview)) if *revision == self.revision => preview.clone(),
            _ if self.document.is_inx() => {
                let mut preview = BTreeMap::new();
                for parameter in values.keys() {
                    preview.extend(dirty::preview_pose(&self.document.model, &values, *parameter)?);
                }
                preview
            }
            _ => BTreeMap::new(),
        };
        self.views[index].preview = None;
        let leaving = self.active_view;
        let revision = self.revision;
        let slot = self.views.iter_mut().find(|view| view.id == leaving).ok_or("the active view is missing")?;
        slot.preview = Some((revision, std::mem::take(&mut self.preview_deformation)));
        slot.values = std::mem::replace(&mut self.runtime_values, values);
        slot.native_values = std::mem::take(&mut self.native_runtime_values);
        self.native_runtime_values = std::mem::take(&mut self.views[index].native_values);
        self.views[index].values.clear();
        if self.document.is_native() {
            self.sync_native_playback_values();
            self.native_motion_preview = None;
            self.native_playback.reset_animation();
        }
        self.preview_deformation = preview;
        self.reset_physics();
        self.animation_preview = None;
        self.active_view = id;
        Ok(())
    }

    fn sync_runtime_values(&mut self) {
        let defaults = default_values(&self.document.model);
        // The other views drop a removed parameter now: a parameter made later can take its id.
        for view in &mut self.views {
            view.values.retain(|id, _| defaults.contains_key(id));
        }
        self.runtime_values.retain(|id, value| {
            defaults.contains_key(id)
                && checked_parameter_value(&self.document.model, *id, *value).map(|checked| *value = checked).is_ok()
        });
        for (id, value) in defaults {
            self.runtime_values.entry(id).or_insert(value);
        }
    }

    fn is_modified(&self) -> bool {
        self.document.model != self.saved_model
            || self.document.tail() != self.saved_tail
            || self.document.native_payloads() != self.saved_payloads
            || self.depth_draw.unsaved
    }
}

/// One view of `view.*`: the parameter values it shows while another view is active. UI03 (solo per view) would
/// add its solo set here.
#[derive(Clone, Default)]
struct ViewSlot {
    id: u32,
    values: BTreeMap<u32, [f64; 2]>,
    native_values: BTreeMap<String, f64>,
    /// The Depth Bone pose preview the view had, and the revision it belongs to.
    preview: Option<(u64, BTreeMap<u32, Vec<[f64; 2]>>)>,
}

// ponytail: a fixed cap on views per document; raise it when a layout needs more.
const MAX_VIEWS: usize = 16;

fn required_view(request: &Value) -> Result<u32, String> {
    request["id"].as_u64().and_then(|id| u32::try_from(id).ok()).ok_or_else(|| "id must be a view id".into())
}

/// A `Send` copy of what a render needs from a session: the encoded document, the manual parameter
/// values and the ambient light. [`Session`] holds raw GPU callbacks and cannot cross threads, so a
/// background export builds its own session from this with [`ExportCopy::open`].
#[derive(Clone, Debug, PartialEq)]
pub struct ExportCopy {
    bytes: Arc<[u8]>,
    values: BTreeMap<u32, [f64; 2]>,
    ambient: [f64; 3],
}

impl ExportCopy {
    /// The session to render with; it has no history, file path or animation preview.
    pub fn open(self) -> Result<Session, String> {
        let mut document = Document::parse(&self.bytes)?;
        prepare_model(&mut document.model)?;
        let mut session = Session::from_document(document, 0, None, None);
        session.runtime_values = self.values;
        session.sync_runtime_values();
        session.scene_ambient = self.ambient;
        Ok(session)
    }
}

impl Session {
    /// Copies the open INP/INX document for rendering on another thread (see [`ExportCopy`]).
    pub fn export_copy(&self) -> Result<ExportCopy, String> {
        if !self.document.is_inx() {
            return Err("only INP/INX documents can be copied for export".into());
        }
        if self.deform_cursor.is_some() {
            return Err("a deform refresh is still pending".into());
        }
        Ok(ExportCopy {
            bytes: self.document.encode()?.into(),
            values: self.runtime_values.clone(),
            ambient: self.scene_ambient,
        })
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod gpu_host_tests {
    use super::*;

    unsafe extern "C" fn reject_probe(
        _: *mut c_void, _: *const gpu_deform::GpuProbePacket, _: *mut f32,
        _: usize, _: *mut c_char, _: usize,
    ) -> i32 { 1 }

    fn ffi_request(session: *mut Session, request: Value) -> Value {
        let input = CString::new(request.to_string()).unwrap();
        let output = vsoul_session_request(session, input.as_ptr());
        assert!(!output.is_null());
        let parsed = serde_json::from_slice(unsafe { CStr::from_ptr(output) }.to_bytes()).unwrap();
        vsoul_string_free(output);
        parsed
    }

    struct ReentrantProbe {
        session: *mut Session,
        rejected: bool,
    }

    unsafe extern "C" fn reenter_probe(
        user: *mut c_void, _: *const gpu_deform::GpuProbePacket, _: *mut f32,
        _: usize, _: *mut c_char, _: usize,
    ) -> i32 {
        let context = &mut *user.cast::<ReentrantProbe>();
        let read = ffi_request(context.session, json!({"op":"snapshot"}));
        let replace = ffi_request(context.session, json!({"op":"new"}));
        let setter = vsoul_session_set_gpu_probe(context.session, std::ptr::null_mut(), None);
        vsoul_session_free(context.session);
        context.rejected = read["succeeded"] == false && replace["succeeded"] == false && setter == -2;
        1
    }

    #[test]
    fn callback_cannot_reenter_replace_or_free_the_active_session() {
        let session = vsoul_session_new();
        let create = |request| {
            let response = ffi_request(session, request);
            assert_eq!(response["succeeded"], true, "{response}");
            response["result"]["created"][0]["uuid"].as_u64().unwrap()
        };
        let rig = create(json!({"op":"node.create","className":"DepthRigRoot"}));
        let bone = create(json!({"op":"node.create","className":"DepthBone","parents":[rig],
            "boneId":"Test","restHead":[0,0,0],"restTail":[0,10,0]}),
        );
        let grid = create(json!({"op":"node.create","className":"GridDeformer"}));
        let added = ffi_request(session, json!({"op":"rig.add_source","root":rig,"target":grid,"bone":bone}),
        );
        assert_eq!(added["succeeded"], true, "{added}");
        let before = ffi_request(session, json!({"op":"snapshot"}));
        let mut context = ReentrantProbe { session, rejected: false,
        };
        assert_eq!(vsoul_session_set_gpu_probe(session,
            (&mut context as *mut ReentrantProbe).cast(), Some(reenter_probe)), 0);
        let compared = ffi_request(session, json!({"op":"rig.gpu_offsets","root":rig}));
        assert!(context.rejected);
        assert_eq!(compared["succeeded"], true, "{compared}");
        assert_eq!(compared["result"]["matched"], false);
        assert_eq!(ffi_request(session, json!({"op":"snapshot"})), before);
        vsoul_session_free(session);
    }

    #[test]
    fn host_registration_survives_document_replacement_and_stays_session_local() {
        let mut session = Session::new();
        let other = Session::new();
        assert_eq!(vsoul_session_set_gpu_probe(&mut session, std::ptr::null_mut(), Some(reject_probe)), 0);
        assert_eq!(session.handle_request(&json!({"op":"new"}))["succeeded"], true);
        assert!(session.gpu_probe.is_some());
        assert!(other.gpu_probe.is_none());
        let path = std::env::temp_dir().join(format!("vsoul-gpu-host-{}-{}.inx",
            std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        fs::write(&path, Document::new(empty_model()).encode().unwrap()).unwrap();
        let opened = session.handle_request(&json!({"op":"open","path":path}));
        fs::remove_file(path).unwrap();
        assert_eq!(opened["succeeded"], true, "{opened}");
        assert!(session.gpu_probe.is_some());
        assert_eq!(vsoul_session_set_gpu_probe(&mut session, std::ptr::null_mut(), None), 0);
        assert!(session.gpu_probe.is_none());
        assert_eq!(vsoul_session_set_gpu_probe(std::ptr::null_mut(), std::ptr::null_mut(), None), -1);
    }
}

#[cfg(test)]
mod rig_runtime_override_tests {
    use super::*;

    fn result(session: &mut Session, request: Value) -> Value {
        let response = session.handle_request(&request);
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    fn create(session: &mut Session, class_name: &str, parents: &[u64]) -> u32 {
        let mut request = json!({"op":"node.create", "className":class_name, "parents":parents});
        if class_name == "DepthBone" {
            request["restHead"] = json!([0.0, 0.0, 0.0]);
            request["restTail"] = json!([0.0, 10.0, 0.0]);
            request["boneId"] = json!("Test");
        }
        let created = result(session, request);
        created["created"][0]["uuid"].as_u64().unwrap() as u32
    }

    #[test]
    fn rig_preview_commits_runtime_overrides_only_after_success() {
        let mut session = Session::new();
        let parameter = result(
            &mut session,
            json!({"op":"parameter.create","dimensions":2,"name":"Pose"}),
        )["uuid"]
            .as_u64()
            .unwrap();
        let physics = create(&mut session, "SimplePhysics", &[1]);
        let rig = create(&mut session, "DepthRigRoot", &[1]);
        let bone = create(&mut session, "DepthBone", &[u64::from(rig)]);
        let grid = create(&mut session, "GridDeformer", &[1]);
        let invalid = create(&mut session, "Node", &[1]);
        result(
            &mut session,
            json!({"op":"deformer.define_grid","nodes":[grid],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        result(
            &mut session,
            json!({"op":"rig.add_source","root":rig,"target":grid,"bone":bone}),
        );
        result(
            &mut session,
            json!({"op":"binding.set_trs","parameter":parameter,"nodes":[bone],"key":[1,0],"rotation_y":0.5}),
        );
        result(
            &mut session,
            json!({"op":"physics.set","node":physics,"param":parameter}),
        );
        result(&mut session, json!({"op":"animation.create","name":"Idle"}));
        result(&mut session, json!({"op":"animation.seek","name":"Idle","frame":0}),
        );
        result(&mut session, json!({"op":"physics.step","dt":0.01}));

        let before = session.snapshot();
        assert_eq!(before["animation"]["name"], "Idle");
        assert_eq!(before["physics"]["active"], true);
        let mut values = Map::new();
        values.insert(parameter.to_string(), json!([1.0, 0.0]));
        let request = json!({
            "op":"rig.preview_deform", "root":rig, "parameter":parameter,
            "targets":[invalid], "values":values,
        });
        let rejected = session.handle_request(&request);
        assert_eq!(rejected["succeeded"], false);
        assert_eq!(session.snapshot(), before);

        let mut successful = request;
        successful["targets"] = json!([grid]);
        result(&mut session, successful);
        let after = session.snapshot();
        assert_eq!(after["values"][parameter.to_string()], json!([1.0, 0.0]));
        assert_eq!(after["animation"], Value::Null);
        assert_eq!(after["physics"]["active"], false);
        assert_eq!(after["revision"], before["revision"]);
        assert_eq!(after["canUndo"], before["canUndo"]);
    }
}

#[no_mangle]
pub extern "C" fn vsoul_session_new() -> *mut Session {
    std::panic::catch_unwind(|| Box::into_raw(Box::new(Session::new())))
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn vsoul_session_set_gpu_probe(
    session: *mut Session,
    user: *mut c_void,
    callback: Option<gpu_deform::GpuProbeCallback>,
) -> i32 {
    if gpu_deform::callback_is_active() {
        return -2;
    }
    if session.is_null() {
        return -1;
    }
    unsafe {
        (*session).gpu_probe = callback.map(|callback| gpu_deform::Probe { user, callback });
    }
    0
}

#[no_mangle]
pub extern "C" fn vsoul_session_free(session: *mut Session) {
    if gpu_deform::callback_is_active() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !session.is_null() {
            unsafe {
                drop(Box::from_raw(session));
            }
        }
    }));
}

#[no_mangle]
pub extern "C" fn vsoul_session_request(
    session: *mut Session,
    request: *const c_char,
) -> *mut c_char {
    let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if gpu_deform::callback_is_active() {
            return error("session access from a GPU callback is not allowed");
        }
        if session.is_null() || request.is_null() {
            return error("session and request pointers must be non-null");
        }
        let parsed = unsafe { CStr::from_ptr(request) }
            .to_str()
            .map_err(|error| format!("request is not UTF-8: {error}"))
            .and_then(|text| {
                serde_json::from_str::<Value>(text)
                    .map_err(|error| format!("invalid JSON: {error}"))
            });
        match parsed {
            Ok(request) => {
                let mut response = unsafe { (&mut *session).handle_request(&request) };
                if request.get("d_compatible").and_then(Value::as_bool) == Some(true) {
                    project_d_numbers(&mut response);
                }
                response
            }
            Err(message) => error(&message),
        }
    }))
    .unwrap_or_else(|_| error("internal panic at FFI boundary"));
    CString::new(response.to_string())
        .unwrap_or_else(|_| CString::new("{}").unwrap())
        .into_raw()
}

#[no_mangle]
pub extern "C" fn vsoul_string_free(value: *mut c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !value.is_null() {
            unsafe {
                drop(CString::from_raw(value));
            }
        }
    }));
}

fn project_d_numbers(value: &mut Value) {
    match value {
        Value::Number(number) if number.as_i64().is_none() && number.as_u64().is_none() => {
            let text = number.to_string();
            *value = if text.contains(['.', 'e', 'E']) {
                number
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .map(Value::from)
                    .unwrap_or(Value::String(text))
            } else {
                Value::String(text)
            };
        }
        Value::Array(values) => values.iter_mut().for_each(project_d_numbers),
        Value::Object(values) => values.values_mut().for_each(project_d_numbers),
        _ => {}
    }
}

fn empty_model() -> Value {
    json!({
        "meta": { "name": "", "version": "1.0-alpha" },
        "physics": {},
        "nodes": {
            "uuid": 1,
            "name": "Root",
            "type": "Node",
            "enabled": true,
            "zsort": 0.0,
            "transform": { "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0] },
            "lockToRoot": false,
            "pinToMesh": false,
        },
        "param": [],
        "automation": [],
        "animations": {},
    })
}

fn error(message: &str) -> Value {
    json!({ "succeeded": false, "status": "error", "message": message, "result": null })
}

fn motion3_name(request: &Value) -> Result<&str, String> {
    let name = required_string(request, "name")?;
    if name.trim().is_empty() || name.len() > 1024 {
        return Err("Motion3 name must be nonempty and at most 1024 bytes".into());
    }
    Ok(name)
}

fn motion3_registry(model: &Value) -> Result<Option<&Map<String, Value>>, String> {
    let Some(live2d) = model.get("live2d") else { return Ok(None);
    };
    let live2d = live2d.as_object().ok_or("model.live2d must be an object")?;
    live2d.get("motions").map(|motions| {
            motions.as_object()
        .ok_or_else(|| "model.live2d.motions must be an object".into())
        }).transpose()
}

fn motion3_registry_mut(model: &mut Value) -> Result<&mut Map<String, Value>, String> {
    let live2d = model.as_object_mut().ok_or("model must be an object")?
        .entry("live2d").or_insert_with(|| json!({})).as_object_mut()
        .ok_or("model.live2d must be an object")?;
    live2d.entry("motions").or_insert_with(|| json!({})).as_object_mut()
        .ok_or_else(|| "model.live2d.motions must be an object".into())
}

fn required_string<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn optional_request_string<'a>(request: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match request.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn count_nodes(node: &Value) -> usize {
    1 + node["children"].as_array().map_or(0, |children| children.iter().map(count_nodes).sum())
}

/// The `atlas.build` options with D's export window defaults.
fn atlas_settings(request: &Value) -> Result<atlas::AtlasSettings, String> {
    let defaults = atlas::AtlasSettings::default();
    let integer = |key: &str, default: i64| match request.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value.as_i64().ok_or_else(|| format!("{key} must be an integer")),
    };
    let resolution = u32::try_from(integer("resolution", i64::from(defaults.resolution))?)
        .map_err(|_| "resolution must be a positive integer")?;
    let padding = i32::try_from(integer("padding", i64::from(defaults.padding))?)
        .map_err(|_| "padding must be a 32-bit integer")?;
    let scale = match request.get("scale") {
        None | Some(Value::Null) => defaults.scale,
        Some(value) => finite_number(value, "scale")? as f32,
    };
    let non_linear_scaling = match request.get("nonLinearScaling") {
        None | Some(Value::Null) => defaults.non_linear_scaling,
        Some(value) => value.as_bool().ok_or("nonLinearScaling must be a boolean")?,
    };
    Ok(atlas::AtlasSettings { resolution, padding, scale, non_linear_scaling })
}

struct AtlasInput {
    id: u32,
    /// Indices into [`AtlasInputs::images`]: albedo, emissive, bump.
    slots: [Option<usize>; 3],
    uvs: Vec<[f32; 2]>,
    /// World `[left, top, right, bottom]`.
    bounds: [f32; 4],
}

struct AtlasInputs {
    images: Vec<pixels::PixelImage>,
    parts: Vec<AtlasInput>,
    skipped: Vec<Value>,
}

fn pair_list(value: &Value) -> Option<Vec<[f64; 2]>> {
    let items = value.as_array()?;
    if items.first().is_some_and(Value::is_array) {
        items.iter().map(|pair| Some([pair.get(0)?.as_f64()?, pair.get(1)?.as_f64()?])).collect()
    } else if items.len() % 2 == 0 {
        items.chunks(2).map(|pair| Some([pair[0].as_f64()?, pair[1].as_f64()?])).collect()
    } else {
        None
    }
}

/// Vertices and UVs of a Part: the renderer's `mesh.verts` when present, else the top-level arrays.
fn part_mesh(node: &Value) -> Option<(Vec<[f64; 2]>, Vec<[f64; 2]>)> {
    let mesh = node.get("mesh");
    let (verts, uvs) = match mesh.and_then(|mesh| mesh.get("verts")) {
        Some(verts) => (verts, mesh.and_then(|mesh| mesh.get("uvs")).or_else(|| node.get("uvs"))?),
        None => (node.get("vertices")?, node.get("uvs")?),
    };
    let (verts, uvs) = (pair_list(verts)?, pair_list(uvs)?);
    (!uvs.is_empty() && uvs.len() == verts.len()).then_some((verts, uvs))
}

fn decode_atlas_texture(blob: &document::TextureBlob, slot: usize) -> Result<pixels::PixelImage, String> {
    let format = match blob.tag {
        0 => image::ImageFormat::Png,
        1 => image::ImageFormat::Tga,
        tag => return Err(format!("texture slot {slot} has unsupported image tag {tag}")),
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(blob.data.as_slice()), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("could not decode texture slot {slot}: {error}"))?
        .into_rgba8();
    pixels::PixelImage::from_pixels(decoded.width(), decoded.height(), pixels::PixelFormat::Rgba, decoded.into_raw())
}

/// Collects what [`atlas::build_atlases`] needs from the Parts of `model`. Without `wanted`, Parts that
/// are disabled (or sit under a disabled node) are skipped like D's "Prune unused nodes" export, and so
/// are Parts without an albedo texture or mesh; naming a Part asks for exactly that Part and fails if
/// it cannot be placed.
fn atlas_inputs(
    model: &Value,
    textures: &[document::TextureBlob],
    scene: &scene_deform::SceneDeform,
    wanted: &[u32],
) -> Result<AtlasInputs, String> {
    fn walk<'a>(node: &'a Value, enabled: bool, out: &mut Vec<(&'a Value, bool)>) {
        let enabled = enabled && node.get("enabled").and_then(Value::as_bool) != Some(false);
        if node.get("type").and_then(Value::as_str) == Some("Part") {
            out.push((node, enabled));
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            walk(child, enabled, out);
        }
    }
    let mut found = Vec::new();
    walk(model.get("nodes").ok_or("model is missing nodes")?, true, &mut found);
    if let Some(missing) = wanted.iter().find(|id| !found.iter().any(|(node, _)| node["uuid"] == **id)) {
        return Err(format!("node {missing} is not a Part"));
    }
    let mut inputs = AtlasInputs { images: Vec::new(), parts: Vec::new(), skipped: Vec::new() };
    let mut decoded: BTreeMap<usize, usize> = BTreeMap::new();
    let mut pixel_total = 0u64;
    for (node, enabled) in found {
        let id = node["uuid"].as_u64().and_then(valid_id).ok_or("Part is missing its uuid")?;
        let explicit = wanted.contains(&id);
        if !wanted.is_empty() && !explicit {
            continue;
        }
        let skip = |inputs: &mut AtlasInputs, reason: &str| {
            if explicit {
                Err(format!("Part {id} cannot be atlased: {reason}"))
            } else {
                inputs.skipped.push(json!({"id": id, "reason": reason}));
                Ok(())
            }
        };
        if !enabled && !explicit {
            skip(&mut inputs, "disabled")?;
            continue;
        }
        let Some((verts, uvs)) = part_mesh(node) else {
            skip(&mut inputs, "no mesh")?;
            continue;
        };
        let mut slots = [None; 3];
        for (position, entry) in node.get("textures").and_then(Value::as_array).into_iter().flatten().take(3).enumerate() {
            let Some(slot) = entry.as_u64().filter(|slot| *slot != u64::from(u32::MAX)) else { continue };
            let slot = usize::try_from(slot).ok().filter(|slot| *slot < textures.len())
                .ok_or_else(|| format!("Part {id} uses texture slot {slot} that is not in the file"))?;
            slots[position] = Some(match decoded.get(&slot) {
                Some(index) => *index,
                None => {
                    let image = decode_atlas_texture(&textures[slot], slot)?;
                    let [width, height] = image.size();
                    pixel_total += u64::from(width) * u64::from(height);
                    if pixel_total > ATLAS_TEXTURE_PIXELS {
                        return Err("the Part textures are too large to atlas".into());
                    }
                    inputs.images.push(image);
                    decoded.insert(slot, inputs.images.len() - 1);
                    inputs.images.len() - 1
                }
            });
        }
        if slots[0].is_none() {
            skip(&mut inputs, "no albedo texture")?;
            continue;
        }
        let matrix = scene.world_matrices.get(&id);
        let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for [x, y] in &verts {
            let (wx, wy) = match matrix {
                Some(m) => (m[0][0] * x + m[0][1] * y + m[0][3], m[1][0] * x + m[1][1] * y + m[1][3]),
                None => (*x, *y),
            };
            bounds = [bounds[0].min(wx), bounds[1].min(wy), bounds[2].max(wx), bounds[3].max(wy)];
        }
        inputs.parts.push(AtlasInput {
            id,
            slots,
            uvs: uvs.iter().map(|uv| [uv[0] as f32, uv[1] as f32]).collect(),
            bounds: bounds.map(|value| value as f32),
        });
    }
    if inputs.parts.is_empty() {
        return Err("no Part with an albedo texture to atlas".into());
    }
    Ok(inputs)
}

fn flip_flag(request: &Value) -> Result<bool, String> {
    request.get("flip").filter(|flip| !flip.is_null()).map_or(Ok(false), |flip| flip.as_bool().ok_or("flip must be a boolean".into()))
}

fn validated_linked_image(path: &str) -> Result<std::path::PathBuf, String> {
    if path.is_empty() { return Err("imagePath must be a nonempty string".into()); }
    let absolute = std::fs::canonicalize(path).map_err(|_| "imagePath cannot be resolved")?;
    let name = absolute.to_str().ok_or("imagePath is not valid UTF-8")?;
    let bytes = read_bounded_file(name, 256 * 1024 * 1024, "image")?;
    match absolute.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => { model_import::decode_complete_png(name, &bytes)?; }
        Some("jpg" | "jpeg") => {
            jpeg::check_complete(&bytes)?;
            model_import::decode_rgba(name, None, &bytes)?;
        }
        _ => return Err("imagePath must name a static PNG or JPEG".into()),
    }
    Ok(absolute)
}

fn read_bounded_file(path: &str, limit: u64, kind: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| {
        if std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            format!("{kind} source must be a regular file")
        } else {
            format!("could not open {kind} file: {error}")
        }
    })?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("{kind} source must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {kind} file: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{kind} source exceeds the size limit"));
    }
    Ok(bytes)
}

fn native_parameter_defaults(parameters: &[Value]) -> Result<BTreeMap<String, f64>, String> {
    let mut values = BTreeMap::new();
    for parameter in parameters {
        let id = parameter["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("native parameter ID is missing")?;
        let value = parameter["default"]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("native parameter {id:?} has no finite default"))?;
        if values.insert(id.to_owned(), value).is_some() {
            return Err(format!("native parameter ID {id:?} is duplicated"));
        }
    }
    Ok(values)
}

fn required_id(request: &Value, key: &str) -> Result<u32, String> {
    let value = request
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be a positive resource UUID"))?;
    valid_id(value).ok_or_else(|| format!("{key} must be a nonzero u32 resource UUID"))
}

pub(crate) fn valid_id(value: u64) -> Option<u32> {
    u32::try_from(value)
        .ok()
        .filter(|id| *id != 0 && *id != u32::MAX)
}

fn optional_string<'a>(request: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_str().map(Some).ok_or_else(|| format!("{key} must be a string")),
    }
}

fn axis_arg(request: &Value, default: usize) -> Result<usize, String> {
    match request.get("axis") {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|axis| *axis <= 1)
            .map(|axis| axis as usize)
            .ok_or_else(|| "axis must be 0 or 1".to_string()),
    }
}

fn axis_named(request: &Value, name: &str, default: usize) -> Result<usize, String> {
    match request.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|axis| *axis <= 1)
            .map(|axis| axis as usize)
            .ok_or_else(|| format!("{name} must be 0 or 1")),
    }
}

/// The arguments `rig.import_gltf` and `mesh.import_gltf` share: the DepthRigRoot, the open file and the answers
/// the loss report may ask for.
struct GltfImport {
    root: u32,
    file: File,
    units: Option<f64>,
    lossy: bool,
    acknowledged: bool,
    dry_run: bool,
}

/// `dryRun`: an optional boolean, false when absent.
fn dry_run_flag(request: &Value) -> Result<bool, String> {
    match request.get("dryRun").filter(|value| !value.is_null()) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| "dryRun must be a boolean".into()),
    }
}

/// `translationScale`: an optional finite positive number (rig units per MMD unit).
fn translation_scale(request: &Value) -> Result<Option<f64>, String> {
    request
        .get("translationScale")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_f64()
                .filter(|scale| scale.is_finite() && *scale > 0.0)
                .ok_or_else(|| "translationScale must be a positive number".to_string())
        })
        .transpose()
}

fn key_arg(request: &Value) -> Result<[usize; 2], String> {
    let key = request
        .get("key")
        .and_then(Value::as_array)
        .filter(|key| key.len() == 2)
        .ok_or("key must be two keypoint indexes")?;
    let index = |value: &Value| {
        value
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| "key must contain nonnegative integers".to_string())
    };
    Ok([index(&key[0])?, index(&key[1])?])
}

/// `bindings: [{node, param_name}]`; `None` when absent and not required.
fn binding_keys(request: &Value, required: bool) -> Result<Option<Vec<parameter_edit::BindingKey>>, String> {
    let Some(list) = request.get("bindings") else {
        return if required { Err("bindings is required".into()) } else { Ok(None) };
    };
    list.as_array()
        .ok_or("bindings must be an array")?
        .iter()
        .map(|entry| {
            let node = entry.get("node").and_then(Value::as_u64).and_then(valid_id)
                .ok_or("binding node must be a nonzero u32 UUID")?;
            let channel = entry.get("param_name").and_then(Value::as_str)
                .ok_or("binding param_name must be a string")?;
            Ok((node, channel.to_owned()))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

fn optional_pair(request: &Value, key: &str) -> Result<Option<[f64; 2]>, String> {
    request
        .get(key)
        .map(|value| parse_pair(value, key))
        .transpose()
}

fn required_pair(request: &Value, key: &str) -> Result<[f64; 2], String> {
    request
        .get(key)
        .ok_or_else(|| format!("{key} is required"))
        .and_then(|value| parse_pair(value, key))
}

/// The `solo` of a render request: the nodes it names and every node below them. Each must be a node of `nodes`.
fn solo_nodes(nodes: &Value, solo: &Value) -> Result<HashSet<u64>, String> {
    fn walk(node: &Value, chosen: &HashSet<u64>, inside: bool, out: &mut HashSet<u64>, seen: &mut HashSet<u64>) {
        let id = node["uuid"].as_u64();
        let chosen_here = id.is_some_and(|id| chosen.contains(&id));
        if let Some(id) = id.filter(|_| chosen_here) {
            seen.insert(id);
        }
        if let Some(id) = id.filter(|_| inside || chosen_here) {
            out.insert(id);
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, chosen, inside || chosen_here, out, seen);
        }
    }
    let list = solo.as_array().ok_or("solo must be an array of node UUIDs")?;
    let chosen = list
        .iter()
        .map(|id| id.as_u64().ok_or("solo must be an array of node UUIDs"))
        .collect::<Result<HashSet<u64>, _>>()?;
    let (mut out, mut seen) = (HashSet::new(), HashSet::new());
    walk(nodes, &chosen, false, &mut out, &mut seen);
    if let Some(missing) = chosen.iter().filter(|id| !seen.contains(id)).min() {
        return Err(format!("solo names no node with UUID {missing}"));
    }
    Ok(out)
}

fn required_quad(request: &Value, key: &str) -> Result<[f64; 4], String> {
    let values = request
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key} must be an array of four numbers"))?;
    if values.len() != 4 {
        return Err(format!("{key} must be an array of four numbers"));
    }
    let values: Vec<f64> = values
        .iter()
        .map(|value| finite_number(value, key))
        .collect::<Result<_, _>>()?;
    Ok([values[0], values[1], values[2], values[3]])
}

pub(crate) fn parse_pair(value: &Value, name: &str) -> Result<[f64; 2], String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must contain two finite numbers"))?;
    if values.len() != 2 {
        return Err(format!("{name} must contain two finite numbers"));
    }
    Ok([
        finite_number(&values[0], name)?,
        finite_number(&values[1], name)?,
    ])
}

pub(crate) fn finite_number(value: &Value, name: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{name} must contain finite numbers"))
}

fn axis_points(min: f64, max: f64) -> Vec<f64> {
    if min + max == 0.0 {
        vec![0.0, 0.5, 1.0]
    } else {
        vec![0.0, 1.0]
    }
}

fn parameter_count(model: &Value) -> usize {
    fn count(list: &[Value]) -> usize {
        list.iter()
            .map(|item| {
                item.get("children")
                    .and_then(Value::as_array)
                    .map_or(1, |children| count(children))
            })
            .sum()
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .map_or(0, |list| count(list))
}

fn number_array(request: &Value, key: &str) -> Result<Vec<f64>, String> {
    let Some(value) = request.get(key) else {
        return Err(format!("{key} not provided"));
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of numbers"))?;
    values
        .iter()
        .map(|value| finite_number(value, key))
        .collect()
}

fn preview_json(values: &BTreeMap<u32, Vec<[f64; 2]>>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut object = Map::new();
    for (id, offsets) in values {
        object.insert(id.to_string(), json!(offsets));
    }
    Value::Object(object)
}

fn optional_ids(request: &Value, key: &str) -> Result<Vec<u32>, String> {
    let Some(value) = request.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of node UUIDs"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(valid_id)
                .ok_or_else(|| format!("{key} must contain nonzero u32 UUIDs"))
        })
        .collect()
}

fn drag_axes(request: &Value) -> (Option<[bool; 3]>, Option<[bool; 3]>) {
    if let Some(component) = request.get("component").and_then(Value::as_str) {
        return match component {
            "translationX" => (Some([true, false, false]), None),
            "translationY" => (Some([false, true, false]), None),
            "translationZ" => (Some([false, false, true]), None),
            "rotationX" => (None, Some([true, false, false])),
            "rotationY" => (None, Some([false, true, false])),
            "rotationZ" => (None, Some([false, false, true])),
            _ => (None, None),
        };
    }
    let Some(object) = request.as_object() else {
        return (None, None);
    };
    (
        object.contains_key("translation").then_some([true, true, true]),
        object.contains_key("rotation").then_some([true, true, true]),
    )
}

fn transform_changes_trs(request: &Value) -> bool {
    if let Some(component) = request.get("component").and_then(Value::as_str) {
        return matches!(
            component,
            "translationX"
                | "translationY"
                | "translationZ"
                | "rotationX"
                | "rotationY"
                | "rotationZ"
                | "scaleX"
                | "scaleY"
        );
    }
    request
        .as_object()
        .is_some_and(|object| {
            object.contains_key("translation")
                || object.contains_key("rotation")
                || object.contains_key("scale")
        })
}

fn allows_pending_deform(op: &str) -> bool {
    matches!(
        op,
        "snapshot"
            | "atlas.pack"
            | "atlas.build"
            | "merge.psd.plan"
            | "merge.kra.plan"
            | "export.inp"
            | "export.video.status"
            | "export.video.probe"
            | "export.video.probe.status"
            | "export.video.cancel"
            | "animation.motionsync.start"
            | "animation.motionsync.status"
            | "animation.motionsync.cancel"
            | "extdata.get"
            | "texture.get"
            | "scene.geometry"
            | "model.statistics"
            | "cv.analyze"
            | "image.process"
            | "automesh.preset.save"
            | "automesh.preset.delete"
            | "automesh.preset.list"
            | "physics.preset.save"
            | "physics.preset.delete"
            | "physics.preset.list"
            | "live2d.physics3.preset.save"
            | "live2d.physics3.preset.delete"
            | "live2d.physics3.preset.list"
            | "animation.template.save"
            | "animation.template.delete"
            | "animation.template.list"
            | "live2d.authoring.inspect"
            | "live2d.authoring.read"
            | "live2d.cdi3.export"
            | "live2d.physics3.export"
            | "live2d.physics3.read"
            | "live2d.convert.inx"
            | "live2d.userdata3.export"
            | "live2d.animation.read"
            | "live2d.animation.bindings"
            | "live2d.authoring.linear_weights"
            | "live2d.authoring.primary_forms"
            | "live2d.authoring.morph_forms"
            | "parameters"
            | "nodes.list"
            | "node.get"
            | "object.find"
            | "parameter.get"
            | "depth.list"
            | "depth.ops.list"
            | "rig.list_bones"
            | "rig.list_sources"
            | "rig.get_influence_rule"
            | "rig.preview_influence"
            | "rig.preview_deform"
            | "rig.gpu_offsets"
            | "rig.overlay"
            | "rig.pick_bone"
            | "evaluate"
            | "scene.evaluate"
            | "render.packet"
            | "motion3.list"
            | "motion3.read"
            | "motion3.sample"
            | "motion3.export"
            | "motion3.bindings"
            | "motion3.pose"
            | "render"
            | "scene.ambient.set"
            | "bounds"
            | "undo"
            | "redo"
            | "history.summary"
            | "history.goto"
            | "history.clear"
            | "history.limit.set"
            | "save"
            | "deform.continue"
    )
}

fn depth_targets(request: &Value) -> Vec<u32> {
    if let Some(id) = request
        .get("target")
        .or_else(|| request.get("id"))
        .and_then(Value::as_u64)
        .and_then(valid_id)
    {
        return vec![id];
    }
    request
        .get("nodes")
        .or_else(|| request.get("ids"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|value| value.as_u64().and_then(valid_id))
                .collect()
        })
        .unwrap_or_default()
}

fn key_index(request: &Value) -> Result<[usize; 2], String> {
    let key = request
        .get("key")
        .and_then(Value::as_array)
        .ok_or("request.key must identify an existing parameter keypoint index")?;
    if key.len() != 2 {
        return Err("request.key must identify an existing parameter keypoint index".into());
    }
    let x = key[0]
        .as_u64()
        .ok_or("request.key must identify an existing parameter keypoint index")?
        as usize;
    let y = key[1]
        .as_u64()
        .ok_or("request.key must identify an existing parameter keypoint index")?
        as usize;
    Ok([x, y])
}

fn node_ids(request: &Value) -> Result<Vec<u32>, String> {
    if let Some(id) = request.get("id").and_then(Value::as_u64) {
        return Ok(vec![required_id(&json!({ "id": id }), "id")?]);
    }
    let key = if request.get("ids").is_some() {
        "ids"
    } else {
        "nodes"
    };
    if let Some(values) = request.get(key) {
        let values = values
            .as_array()
            .ok_or_else(|| format!("{key} must be an array of node UUIDs"))?;
        return values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(valid_id)
                    .ok_or_else(|| format!("{key} must contain nonzero u32 UUIDs"))
            })
            .collect();
    }
    Err("node UUIDs are required".into())
}

fn parent_ids(request: &Value, model: &Value) -> Result<Vec<u32>, String> {
    if request.get("parent").is_some() {
        return Ok(vec![required_id(request, "parent")?]);
    }
    if let Some(parents) = request.get("parents") {
        let parents = parents
            .as_array()
            .ok_or("parents must be an array of node UUIDs")?;
        return parents
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(valid_id)
                    .ok_or_else(|| "parents must contain nonzero u32 UUIDs".to_string())
            })
            .collect();
    }
    Ok(vec![required_id(&model["nodes"], "uuid")?])
}

/// A blend beyond Cubism's legacy Normal, Add and Multiply with plain over alpha (issue #197).
fn advanced_blend(node: &Value) -> bool {
    // As the renderer reads them: names in any case, and the legacy ADD ignores the alpha composition.
    match node["blend_mode"].as_str().unwrap_or("Normal").to_ascii_lowercase().as_str() {
        "cubism53" => match node["nativeColorBlend"].as_str() {
            Some("ADD") => false,
            Some("NORMAL") => node["nativeAlphaBlend"] != "OVER",
            _ => true,
        },
        mode => {
            !matches!(mode, "normal" | "multiply" | "lineardodge")
                || node.get("alphaBlend").is_some_and(|alpha| alpha != "Over")
        }
    }
}

/// Records the PSD a document was imported or merged from as `meta.psdSource`, so a later merge can relink to it.
fn link_psd(model: &mut Value, path: &str) {
    if !model["meta"].is_object() {
        model["meta"] = json!({});
    }
    model["meta"]["psdSource"] = json!(absolute_path(path));
}

/// `path` made absolute against the working directory, so a stored link does not depend on where the host starts.
fn absolute_path(path: &str) -> String {
    std::path::absolute(path).map_or_else(|_| path.to_owned(), |path| path.to_string_lossy().into_owned())
}

fn find_parameter(model: &Value, id: u32) -> Option<&Value> {
    let list = model.get("param")?.as_array()?;
    fn find(list: &[Value], id: u32) -> Option<&Value> {
        for item in list {
            if item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
                || item.get("groupUUID").and_then(Value::as_u64) == Some(u64::from(id))
            {
                return Some(item);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                if let Some(found) = find(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    find(list, id)
}

fn find_parameter_mut(model: &mut Value, id: u32) -> Option<&mut Value> {
    let list = model.get_mut("param")?.as_array_mut()?;
    fn find(list: &mut [Value], id: u32) -> Option<&mut Value> {
        for item in list {
            let matched = item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id))
                || item.get("groupUUID").and_then(Value::as_u64) == Some(u64::from(id));
            if matched {
                return Some(item);
            }
            if let Some(children) = item.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = find(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    find(list, id)
}

/// Whether a plain parameter, at any depth, has this name.
fn has_parameter_named(list: &[Value], name: &str) -> bool {
    list.iter().any(|item| {
        (item.get("groupUUID").is_none() && item.get("name").and_then(Value::as_str) == Some(name))
            || item.get("children").and_then(Value::as_array).is_some_and(|children| has_parameter_named(children, name))
    })
}

fn check_parameter_name(model: &Value, name: &str, except_id: Option<u32>) -> Result<(), String> {
    fn check(list: &[Value], name: &str, except_id: Option<u32>) -> bool {
        list.iter().any(|item| {
            let id = item
                .get("uuid")
                .or_else(|| item.get("groupUUID"))
                .and_then(Value::as_u64);
            (except_id.is_none_or(|except| id != Some(u64::from(except)))
                && item.get("name").and_then(Value::as_str) == Some(name))
                || item
                    .get("children")
                    .and_then(Value::as_array)
                    .is_some_and(|children| check(children, name, except_id))
        })
    }
    if model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|list| check(list, name, except_id))
    {
        return Err("Name already taken".into());
    }
    Ok(())
}

/// Resolve linked images from an INX against the document that owns the links.
fn resolve_relative_image_paths(model: &mut Value, document_path: &Path) {
    let parent = document_path.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |value: &mut Value| {
        let Some(path) = value.as_str() else { return };
        let path_ref = Path::new(path);
        if !path_ref.is_absolute() && !path.contains("://") {
            *value = json!(parent.join(path_ref).to_string_lossy().into_owned());
        }
    };
    for animation in model.get_mut("animations").and_then(Value::as_object_mut).into_iter().flat_map(|animations| animations.values_mut()) {
        for image in animation.get_mut("images").and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(track) = image.as_object_mut() {
                if let Some(path) = track.get_mut("path") {
                    resolve(path);
                }
                if let Some(sequence) = track.get_mut("sequence").and_then(Value::as_object_mut) {
                    if let Some(directory) = sequence.get_mut("directory") {
                        resolve(directory);
                    }
                    for file in sequence.get_mut("files").and_then(Value::as_array_mut).into_iter().flatten() {
                        if let Some(path) = file.get_mut("path") {
                            resolve(path);
                        }
                    }
                }
            }
        }
    }
}

fn prepare_model(model: &mut Value) -> Result<(), String> {
    if model.get("param").is_some_and(Value::is_null) {
        model["param"] = json!([]);
    }
    if let Some(animations) = model.get_mut("animations").and_then(Value::as_object_mut) {
        for animation in animations.values_mut() {
            if animation.get("animationWeight").and_then(Value::as_str) == Some("\"nan\"") {
                animation["animationWeight"] = json!(1.0);
            }
        }
    }
    normalize_link_channels(model);
    validate_model(model)?;
    crate::deformers::ensure_part_arrays(model);
    Ok(())
}

/// A parameter-link binding stores "X" / "Y"; older files hold the axis index 0 / 1 instead.
fn normalize_link_channels(model: &mut Value) {
    fn walk(list: &mut [Value]) {
        for parameter in list {
            if let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) {
                for binding in bindings {
                    if let Some(axis) = binding.get("param_name").filter(|name| name.is_number()).and_then(|_| evaluation::link_axis(binding)) {
                        binding["param_name"] = json!(if axis == 0 { "X" } else { "Y" });
                    }
                }
            }
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                walk(children);
            }
        }
    }
    if let Some(list) = model.get_mut("param").and_then(Value::as_array_mut) {
        walk(list);
    }
}

/// The first keyform set at a parameter's default value in `current` that `candidate` changes or unsets, as
/// (parameter, node, binding name). Only a keyform that exists is locked: a new binding, a key set for the first time
/// and a removed binding are not edits of it. Inochi2D fills a key that is not set from the keys that are, so an
/// edit elsewhere can still move such a key. A parameter whose range or default changed is a parameter edit and is
/// skipped. When only its axis points change (a flip mirrors them, `set_axes` adds or drops points), the keyform at
/// the old default key is compared with the one at the new default key; a default left between keypoints loses its
/// keyform, like an unset key. So is the deform binding of a node whose own data changed (its mesh, grid or depths): the edit
/// re-expresses those offsets on the new geometry. A default between keypoints has no keyform. Numbers are compared
/// as numbers, since the file may hold `0` where an edit writes back `0.0`.
fn changed_default_form(current: &Value, candidate: &Value) -> Option<(u64, u64, String)> {
    fn leaves<'a>(list: &'a Value, out: &mut BTreeMap<u64, &'a Value>) {
        for item in list.as_array().into_iter().flatten() {
            if let (None, Some(id)) = (item.get("groupUUID"), item["uuid"].as_u64()) {
                out.insert(id, item);
            }
            leaves(&item["children"], out);
        }
    }
    let (mut before, mut after) = (BTreeMap::new(), BTreeMap::new());
    leaves(&current["param"], &mut before);
    leaves(&candidate["param"], &mut after);
    for (id, old) in before {
        let Some(parameter) = after.get(&id) else { continue };
        if ["min", "max", "defaults"].iter().any(|key| !same_numbers(&old[key], &parameter[key])) {
            continue;
        }
        let Some([x, y]) = default_keypoint(old) else { continue };
        let moved = default_keypoint(parameter);
        for was in old["bindings"].as_array().into_iter().flatten() {
            if was["isSet"][x][y].as_bool() != Some(true) {
                continue;
            }
            let (node, name) = (&was["node"], &was["param_name"]);
            let now = parameter["bindings"].as_array().into_iter().flatten()
                .find(|binding| binding["node"] == *node && binding["param_name"] == *name);
            let changed = |now: &Value| {
                moved.is_none_or(|[nx, ny]| {
                    now["isSet"][nx][ny].as_bool() != Some(true)
                        || !same_numbers(&now["values"][nx][ny], &was["values"][x][y])
                })
            };
            let reshaped = || {
                let id = node.as_u64().and_then(|id| u32::try_from(id).ok()).unwrap_or(0);
                let [before, after] = [current, candidate].map(|model| {
                    nodes::find_node(model, id).and_then(Value::as_object).map(|node| {
                        node.iter().filter(|(key, _)| *key != "children").collect::<BTreeMap<_, _>>()
                    })
                });
                *name == "deform" && before != after
            };
            if now.is_some_and(changed) && !reshaped() {
                return Some((id, node.as_u64().unwrap_or(0), name.as_str().unwrap_or_default().to_owned()));
            }
        }
    }
    None
}

/// Equal numbers at the same places, whatever their text.
fn same_numbers(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_numbers(a, b)),
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0),
            _ => a == b,
        },
    }
}

/// The keypoint at a parameter's default value, when the default sits on one on both axes. A 1D parameter is
/// evaluated on its one Y point, whatever its Y default.
fn default_keypoint(parameter: &Value) -> Option<[usize; 2]> {
    let mut key = [0; 2];
    let axes = if parameter["is_vec2"] == true { 2 } else { 1 };
    for (axis, slot) in key.iter_mut().enumerate().take(axes) {
        let [min, max, default] = ["min", "max", "defaults"].map(|field| parameter[field][axis].as_f64());
        let (min, max, default) = (min?, max?, default?);
        let position = if max == min { 0.0 } else { (default - min) / (max - min) };
        *slot = parameter["axis_points"][axis].as_array()?.iter()
            // f32 precision: a file may store an axis point as 0.33333334.
            .position(|point| point.as_f64().is_some_and(|point| (point - position).abs() <= 1e-6))?;
    }
    Some(key)
}

pub(crate) fn validate_model(model: &Value) -> Result<(), String> {
    let object = model.as_object().ok_or("model must be a JSON object")?;
    let mut ids = HashSet::new();
    let nodes = object.get("nodes").ok_or("model is missing nodes")?;
    validate_node(nodes, &mut ids)?;
    if let Some(parameters) = object.get("param") {
        let list = parameters
            .as_array()
            .ok_or("model param must be an array")?;
        validate_parameter_list(list, &mut ids)?;
    }
    Ok(())
}

fn validate_node(node: &Value, ids: &mut HashSet<u32>) -> Result<(), String> {
    let object = node.as_object().ok_or("node must be an object")?;
    insert_id(object.get("uuid").ok_or("node is missing uuid")?, ids)?;
    if let Some(children) = object.get("children") {
        let children = children
            .as_array()
            .ok_or("node children must be an array")?;
        for child in children {
            validate_node(child, ids)?;
        }
    }
    Ok(())
}

fn validate_parameter_list(list: &[Value], ids: &mut HashSet<u32>) -> Result<(), String> {
    for parameter in list {
        let object = parameter.as_object().ok_or("parameter must be an object")?;
        if let Some(group_id) = object.get("groupUUID") {
            insert_id(group_id, ids)?;
            if let Some(children) = object.get("children") {
                let children = children
                    .as_array()
                    .ok_or("parameter group children must be an array")?;
                validate_parameter_list(children, ids)?;
            }
            continue;
        }
        insert_id(object.get("uuid").ok_or("parameter is missing uuid")?, ids)?;
        validate_parameter_shape(parameter)?;
    }
    Ok(())
}

fn insert_id(value: &Value, ids: &mut HashSet<u32>) -> Result<(), String> {
    let raw = value
        .as_u64()
        .ok_or("resource UUID must be a nonzero u32")?;
    let id = valid_id(raw).ok_or("resource UUID must be a nonzero u32")?;
    if !ids.insert(id) {
        return Err(format!("duplicate resource UUID {id}"));
    }
    Ok(())
}

fn validate_parameter_shape(parameter: &Value) -> Result<(), String> {
    let is_vec2 = match parameter.get("is_vec2") {
        Some(value) => value
            .as_bool()
            .ok_or("parameter is_vec2 must be a boolean")?,
        None => false,
    };
    let min = parameter
        .get("min")
        .map(|value| parse_pair(value, "parameter min"))
        .transpose()?
        .unwrap_or([0.0, 0.0]);
    let max = parameter
        .get("max")
        .map(|value| parse_pair(value, "parameter max"))
        .transpose()?
        .unwrap_or([1.0, 1.0]);
    if (0..2).any(|axis| min[axis] > max[axis]) {
        return Err("parameter min must not exceed max".into());
    }
    // A value is placed on the range by its width (evaluation, repeat, the depth rig), so the width has to be a number.
    if (0..2).any(|axis| !(max[axis] - min[axis]).is_finite()) {
        return Err("parameter range is wider than a float can measure".into());
    }
    if let Some(defaults) = parameter.get("defaults") {
        parse_pair(defaults, "parameter defaults")?;
    }
    if let Some(repeat) = parameter.get("repeat") {
        repeat.as_bool().ok_or("parameter repeat must be a boolean")?;
    }
    if let Some(axes) = parameter.get("axis_points") {
        let axes = axes
            .as_array()
            .ok_or("parameter axis_points must contain two axis arrays")?;
        if axes.len() != 2 {
            return Err("parameter axis_points must contain two axis arrays".into());
        }
        validate_axis(&axes[0], true)?;
        validate_axis(&axes[1], is_vec2)?;
    }
    Ok(())
}

fn validate_axis(axis: &Value, active: bool) -> Result<(), String> {
    let axis = axis.as_array().ok_or("parameter axis must be an array")?;
    if !active {
        if axis.len() != 1 || finite_number(&axis[0], "parameter axis")? != 0.0 {
            return Err("1D parameter Y axis must be [0]".into());
        }
        return Ok(());
    }
    if axis.len() < 2 {
        return Err("active parameter axis needs endpoints 0 and 1".into());
    }
    let values: Vec<f64> = axis
        .iter()
        .map(|value| finite_number(value, "parameter axis"))
        .collect::<Result<_, _>>()?;
    if values[0] != 0.0
        || *values.last().unwrap() != 1.0
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("parameter axis values must increase from 0 to 1".into());
    }
    Ok(())
}

fn resource_ids(model: &Value, ids: &mut HashSet<u32>) {
    fn nodes(node: &Value, ids: &mut HashSet<u32>) {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            ids.insert(id);
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                nodes(child, ids);
            }
        }
    }
    fn parameters(list: &[Value], ids: &mut HashSet<u32>) {
        for item in list {
            if let Some(id) = item
                .get("uuid")
                .or_else(|| item.get("groupUUID"))
                .and_then(Value::as_u64)
                .and_then(valid_id)
            {
                ids.insert(id);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                parameters(children, ids);
            }
        }
    }
    if let Some(root) = model.get("nodes") {
        nodes(root, ids);
    }
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        parameters(list, ids);
    }
}

pub(crate) fn fresh_resource_id(model: &Value) -> Result<u32, String> {
    let mut ids = HashSet::new();
    resource_ids(model, &mut ids);
    (1..u32::MAX)
        .find(|id| !ids.contains(id))
        .ok_or_else(|| "no unused resource UUID remains".into())
}

/// The class a node-creating request names. Absent means `Node`; a value that is not a string is refused,
/// so a call that names a class never gets a `Node` in its place.
fn requested_class(request: &Value) -> Result<&str, String> {
    match request.get("className").or_else(|| request.get("class_name")) {
        None => Ok("Node"),
        Some(class) => class.as_str().ok_or_else(|| "className must be a string".to_string()),
    }
}

/// The value to use for a value a caller supplied: inside the range, or an error. On a repeating 1D parameter any
/// finite X value is accepted and wrapped into `[min, max)`.
fn checked_parameter_value(model: &Value, id: u32, value: [f64; 2]) -> Result<[f64; 2], String> {
    let parameter =
        find_parameter(model, id).ok_or_else(|| format!("parameter {id} was not found"))?;
    let min = parameter
        .get("min")
        .map(|value| parse_pair(value, "parameter min"))
        .transpose()?
        .unwrap_or([0.0, 0.0]);
    let max = parameter
        .get("max")
        .map(|value| parse_pair(value, "parameter max"))
        .transpose()?
        .unwrap_or([1.0, 1.0]);
    let mut value = value;
    if evaluation::parameter_repeats(parameter) && max[0] > min[0] {
        let wrapped = min[0] + (value[0] - min[0]).rem_euclid(max[0] - min[0]);
        if !wrapped.is_finite() {
            return Err(format!("value for parameter {id} is too large to wrap into its range"));
        }
        // Rounding can land exactly on the end that is the same point as the start.
        value[0] = if wrapped >= max[0] { min[0] } else { wrapped };
    }
    if (0..2).any(|axis| value[axis] < min[axis] || value[axis] > max[axis]) {
        return Err(format!("value for parameter {id} is outside its range"));
    }
    Ok(value)
}

fn default_values(model: &Value) -> BTreeMap<u32, [f64; 2]> {
    fn collect(list: &[Value], values: &mut BTreeMap<u32, [f64; 2]>) {
        for item in list {
            if let Some(id) = item.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
                let value = item
                    .get("defaults")
                    .and_then(|raw| parse_pair(raw, "defaults").ok())
                    .unwrap_or([0.0, 0.0]);
                values.insert(id, value);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                collect(children, values);
            }
        }
    }
    let mut values = BTreeMap::new();
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        collect(list, &mut values);
    }
    // A session starts a repeating parameter where `value.set` would put the same value.
    for (id, value) in &mut values {
        *value = checked_parameter_value(model, *id, *value).unwrap_or(*value);
    }
    values
}

/// A motion preview's state with its fade clock, when it plays with fades.
fn with_fade(mut state: Value, fade: Option<motion3::FadeClock>) -> Value {
    if let Some(clock) = fade {
        state["fade"] = json!({"elapsed": clock.elapsed, "looping": clock.looping});
    }
    state
}

fn values_json(values: &BTreeMap<u32, [f64; 2]>) -> Value {
    let mut result = Map::new();
    for (id, value) in values {
        result.insert(id.to_string(), json!(value));
    }
    Value::Object(result)
}

/// Resolve an export destination without creating it, refusing the current document path.
pub fn normalized_export_target(raw: &str, named: &Path, current_path: Option<&Path>, op: &str) -> Result<PathBuf, String> {
    // Path drops a trailing separator, so check the original request text.
    if raw.is_empty() {
        return Err("File path is required".into());
    }
    if raw.ends_with(['/', std::path::MAIN_SEPARATOR]) {
        return Err("path must name a file".into());
    }
    let target = normalize_target_path(named)?;
    if current_path == Some(target.as_path()) {
        return Err(format!("{op} would overwrite the open document"));
    }
    Ok(target)
}

/// Resolve a file destination without creating it, so hosts can request approval before writing.
pub fn normalize_target_path(path: &Path) -> Result<PathBuf, String> {
    let filename = path.file_name().ok_or("path must name a file")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("could not resolve parent directory: {error}"))?;
    let target = parent.join(filename);
    if target.exists() {
        fs::canonicalize(&target).map_err(|error| format!("could not resolve file path: {error}"))
    } else {
        Ok(target)
    }
}

/// Creates `target` from `temporary` without ever replacing a file. `link` is the hard link; on a filesystem
/// without hard links (FAT, exFAT, some network mounts) it fails for another reason than `AlreadyExists`, and
/// the file is then created exclusively and written in place, which is not atomic but still never replaces.
fn publish_new(
    temporary: &Path,
    target: &Path,
    bytes: &[u8],
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match link(temporary, target) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(target)?;
            if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = fs::remove_file(target);
                return Err(error);
            }
            Ok(())
        }
        other => other,
    }
}

/// The bytes of a regular file named by a request, up to the authoring container limit.
fn read_regular_file(path: &str, what: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| {
        if std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            format!("{what} source must be a regular file")
        } else {
            format!("could not open {what} file: {error}")
        }
    })?;
    if !file.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err(format!("{what} source must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {what} file: {error}"))?;
    Ok(bytes)
}

pub(crate) fn write_atomic(target: &Path, bytes: &[u8], replace_existing: bool) -> Result<(), String> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let filename = target
        .file_name()
        .ok_or("path must name a file")?
        .to_string_lossy();
    let mut last_error = None;
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{filename}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("could not create temporary save file: {error}")),
        };
        let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("could not sync temporary save file: {error}"));
        }
        let publish = if replace_existing {
            fs::rename(&temporary, target)
        } else {
            publish_new(&temporary, target, bytes, |from, to| fs::hard_link(from, to))
        };
        match publish {
            Ok(()) => {
                if !replace_existing {
                    let _ = fs::remove_file(&temporary);
                }
                #[cfg(unix)]
                if let Ok(directory) = File::open(parent) {
                    let _ = directory.sync_all();
                }
                return Ok(());
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                if !replace_existing {
                    return Err(format!("could not atomically create export file: {error}"));
                }
                last_error = Some(error);
            }
        }
    }
    Err(format!(
        "could not atomically save file: {}",
        last_error.map_or_else(
            || "temporary filename collisions".into(),
            |error| error.to_string()
        )
    ))
}

#[cfg(test)]
mod native_range_tests {
    use super::native_range_value;

    #[test]
    fn file_values_clamp_or_wrap_like_the_cubism_framework() {
        assert_eq!(native_range_value(1.0, -1.0, 0.0, false), 0.0);
        assert_eq!(native_range_value(-0.5, 0.0, 1.0, false), 0.0);
        assert_eq!(native_range_value(0.25, 0.0, 1.0, false), 0.25);
        assert_eq!(native_range_value(370.0, 0.0, 360.0, true), 10.0);
        assert_eq!(native_range_value(-10.0, 0.0, 360.0, true), 350.0);
        assert_eq!(native_range_value(5.0, 2.0, 2.0, true), 2.0);
        assert_eq!(native_range_value(1.7e308, -1.7e308, -1e308, true), -1e308);
    }
}

#[cfg(test)]
mod publish_new_tests {
    use super::publish_new;
    use std::path::Path;
    use std::{fs, io};

    fn unsupported(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    fn exists(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::AlreadyExists))
    }

    fn directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("soul2d-publish-new-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        crate::test_scratch::scratch(path)
    }

    #[test]
    fn falls_back_to_exclusive_create_when_hard_links_are_unsupported() {
        let dir = directory();
        let (temporary, target) = (dir.join("a.tmp"), dir.join("a.png"));
        let _ = fs::remove_file(&target);
        publish_new(&temporary, &target, b"new", unsupported).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        // The fallback still never replaces an existing file.
        let error = publish_new(&temporary, &target, b"other", unsupported).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"new");
        // An existing target reported by the link itself is final and is not retried.
        let fresh = dir.join("b.png");
        let _ = fs::remove_file(&fresh);
        assert_eq!(publish_new(&temporary, &fresh, b"x", exists).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(!fresh.exists());
    }
}
