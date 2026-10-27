//! Session operations `export.image`, `export.frames`, `export.gif` and `export.video.{start,status,cancel}`: the
//! editor's export actions as requests, so agents and the GUI share one code path.
//!
//! Rendering exports and command previews are INP/INX only. They need `Camera` nodes, `scene.evaluate` and `animation.seek`, none of
//! which exist for CMO3/CAN3 documents, so the native allowlists in `handle_inner` do not list them
//! (use `render` for the CPU preview of a native document). None of them touches history, `modified`
//! or `revision`. Codec probing is host-owned and available for every document format.
//!
//! The session owns the video jobs: it cannot move to another thread (raw GPU callbacks), so a job
//! encodes a [`crate::ExportCopy`] on its own thread, which in turn owns the ffmpeg child. The
//! session keeps the thread handle, the cancel flag and the progress counters.

use crate::export::{
    self, AudioInput, EncoderSettings, ImageExportOptions, VideoExportOptions, VideoOutput, EXPORT_CANCELLED,
};
use crate::{normalized_export_target, required_string, write_atomic, Session};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Finished jobs kept for `export.video.status`; older ones are forgotten.
const KEPT_JOBS: usize = 8;
/// `export.frames` renders on the caller's thread and cannot be cancelled, so a request is capped at
/// frames times pixels. About 480 frames of 1080p; larger runs go to `export.video.start` with an image codec.
const MAX_FRAMES_REQUEST_PIXELS: u64 = 1_000_000_000;

type JobResult = Result<usize, String>;

struct CodecProbe {
    program: String,
    handle: Option<JoinHandle<Result<Vec<export::VideoCodec>, String>>>,
    outcome: Option<Result<Vec<export::VideoCodec>, String>>,
}

impl CodecProbe {
    fn status(&mut self) -> Value {
        if self.outcome.is_none() && self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            self.outcome = Some(self.handle.take().unwrap().join()
                .unwrap_or_else(|_| Err("the codec probe panicked".into())));
        }
        match &self.outcome {
            None => json!({"state":"running","program":self.program}),
            Some(Ok(codecs)) => json!({"state":"finished","program":self.program,"codecs":codecs}),
            Some(Err(error)) => json!({"state":"failed","program":self.program,"error":error}),
        }
    }
}

struct VideoJob {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    path: String,
    handle: Option<JoinHandle<JobResult>>,
    /// Set once the thread has been joined, so repeated polls agree.
    outcome: Option<JobResult>,
}

impl Drop for VideoJob {
    /// A dropped session (or one replaced by `new`/`open` without carrying its jobs) stops the encoder.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl VideoJob {
    fn settle(&mut self) {
        if self.outcome.is_none() && self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            let joined = self.handle.take().map(JoinHandle::join);
            self.outcome = Some(match joined {
                Some(Ok(result)) => result,
                _ => Err("the video export thread panicked".into()),
            });
        }
    }

    fn status(&mut self, id: u64) -> Value {
        self.settle();
        let (state, extra) = match &self.outcome {
            None => ("running", json!({})),
            Some(Ok(frames)) => ("finished", json!({"frames": frames})),
            Some(Err(error)) if error == EXPORT_CANCELLED => ("cancelled", json!({"partialFileKept": true})),
            Some(Err(error)) => ("failed", json!({"error": error})),
        };
        let mut status = json!({
            "job": id,
            "state": state,
            "path": self.path,
            "framesDone": self.done.load(Ordering::Relaxed),
            "framesTotal": self.total.load(Ordering::Relaxed),
        });
        if let (Some(status), Some(extra)) = (status.as_object_mut(), extra.as_object()) {
            status.extend(extra.clone());
        }
        status
    }
}

/// What a [`Session`] keeps for video export. Replacing the document keeps it.
pub(crate) struct VideoHost {
    jobs: BTreeMap<u64, VideoJob>,
    next: u64,
    /// The encoder executable. Host code may change it ([`Session::set_video_encoder`]); no request can.
    program: String,
    /// Extra encoder options, split on whitespace. Host-only, like `program`: a request token that is
    /// not an option value would become another input or output file.
    options: String,
    probe: Option<CodecProbe>,
}

impl Default for VideoHost {
    fn default() -> Self {
        Self { jobs: BTreeMap::new(), next: 1, program: "ffmpeg".into(), options: String::new(), probe: None }
    }
}

/// `stem-N.ext` next to `target`, the names `export.frames` writes, with N any positive number.
fn is_frame_name_of(candidate: &Path, target: &Path) -> bool {
    let (Some(stem), Some(ext)) = (target.file_stem(), target.extension()) else { return false };
    let number = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(&*stem.to_string_lossy()))
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(&*format!(".{}", ext.to_string_lossy())));
    candidate.parent() == target.parent() && number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) struct PreparedVideoExport {
    pub(crate) consent: export::VideoConsent,
    options: VideoExportOptions,
    path: String,
}

pub(crate) enum PreparedExport {
    Video(PreparedVideoExport),
    Collection(export::CollectionPlan),
    Gltf(PreparedGltfExport),
    DepthPng(export::DepthPngPlan),
    Store(crate::StoreWritePlan),
}

pub(crate) enum PreparedGltfExport {
    Report(Value),
    Write(export::GltfPlan),
}

impl PreparedExport {
    pub(crate) fn consent(&self) -> export::ExportConsent<'_> {
        match self {
            Self::Video(video) => export::ExportConsent::Video(&video.consent),
            Self::Collection(plan) => export::ExportConsent::Collection(plan),
            Self::Gltf(PreparedGltfExport::Report(_)) => export::ExportConsent::GltfReport,
            Self::Gltf(PreparedGltfExport::Write(plan)) => export::ExportConsent::Gltf(plan),
            Self::DepthPng(plan) => export::ExportConsent::DepthPng(plan),
            Self::Store(plan) => export::ExportConsent::Store(plan),
        }
    }
}

impl PreparedVideoExport {
    pub(crate) fn resolve_program(&mut self) -> Result<(), String> {
        self.consent.command.program = export::resolve_encoder_program(&self.consent.host_program, &self.consent.directory)?;
        Ok(())
    }
}

pub(crate) fn resolve_collection_path(path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    let absolute = if path.is_absolute() { path.to_owned() } else {
        std::env::current_dir().map_err(|error| format!("could not resolve current directory: {error}"))?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {},
            Component::ParentDir => {
                if resolved.exists() && !resolved.is_dir() { return Err("collection parent must be a directory".into()) }
                resolved.pop();
            },
            Component::Normal(name) => {
                let next = resolved.join(name);
                match fs::symlink_metadata(&next) {
                    Ok(_) => resolved = fs::canonicalize(&next)
                        .map_err(|error| format!("could not resolve collection path {}: {error}", next.display()))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved = next,
                    Err(error) => return Err(format!("could not resolve collection path {}: {error}", next.display())),
                }
            },
        }
    }
    Ok(resolved)
}

fn resolve_collection_destination(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("collection output must name a file")?;
    let parent = resolve_collection_path(path.parent().unwrap_or(Path::new(".")))?;
    if parent.exists() && !parent.is_dir() { return Err("collection parent must be a directory".into()) }
    Ok(parent.join(name))
}

fn collection_file(source_alias: PathBuf, source: PathBuf, destination_alias: PathBuf) -> Result<export::CollectionFile, String> {
    Ok(export::CollectionFile { source_alias, source, destination: resolve_collection_destination(&destination_alias)?, destination_alias })
}

fn validate_collection_plan(plan: &export::CollectionPlan, open: Option<&Path>) -> Result<(), String> {
    if resolve_collection_path(&plan.directory_alias)? != plan.directory {
        return Err("collection directory changed after preparation".into());
    }
    if plan.directory.exists() && !plan.directory.is_dir() { return Err("collection directory must be a directory".into()) }
    for file in &plan.copies {
        let source = fs::canonicalize(&file.source_alias).map_err(|error| format!("collection source: {error}"))?;
        if source != file.source || !source.is_file() { return Err("collection source changed after preparation".into()) }
    }
    let open = open.map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()));
    let mut seen = BTreeSet::new();
    let outputs = plan.copies.iter().map(|file| (&file.destination_alias, &file.destination))
        .chain([(&plan.document_alias, &plan.document)]);
    for (alias, destination) in outputs {
        if resolve_collection_destination(alias)? != *destination { return Err("collection destination changed after preparation".into()) }
        if !seen.insert(destination) { return Err("collection outputs resolve to the same file".into()) }
        if open.as_ref() == Some(destination) || plan.copies.iter().any(|file| file.source == *destination) {
            return Err(format!("export.collect would overwrite its own input {}", destination.display()));
        }
        match fs::symlink_metadata(destination) {
            Ok(_) if !plan.overwrite => return Err(format!("{} already exists", destination.display())),
            Ok(meta) if !meta.is_file() => return Err(format!("{} is not a regular file", destination.display())),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(format!("could not inspect {}: {error}", destination.display())),
            _ => {},
        }
    }
    Ok(())
}

impl Session {
    /// Query running host work without joining workers or rebuilding the document snapshot.
    pub fn has_background_work(&self) -> bool {
        self.video.jobs.values().any(|job| job.handle.as_ref().is_some_and(|handle| !handle.is_finished()))
            || self.video.probe.as_ref().and_then(|probe| probe.handle.as_ref()).is_some_and(|handle| !handle.is_finished())
            || self.motion_sync.has_background_work()
    }

    /// Host-side switch of the encoder executable (tests use a stub). Requests cannot name a program.
    pub fn set_video_encoder(&mut self, program: &str) {
        self.video.program = program.to_owned();
    }

    /// Host-side extra encoder options (the editor's "Extra ffmpeg options" field), split on whitespace and
    /// passed to every later video export. Requests cannot carry options.
    pub fn set_video_ffmpeg_options(&mut self, options: &str) {
        self.video.options = options.to_owned();
    }

    /// `export.image`: one render of a camera to `<path>` with the format's extension.
    pub(crate) fn op_export_image(&mut self, request: &Value) -> Result<Value, String> {
        let options = ImageExportOptions::from_request(request)?;
        options.validate()?;
        let raw = required_string(request, "path")?;
        let named = Path::new(raw).with_extension(options.format.extension());
        // Write to the path that was checked, so a symlinked name cannot reach the open document.
        let target = normalized_export_target(raw, &named, self.file_path.as_deref(), "export.image")?;
        let done = export::export_image_to_target(self, &options, &target);
        self.history_op = "export.image".into();
        let done = done?;
        Ok(json!({"path": done.path.to_string_lossy(), "width": done.width, "height": done.height}))
    }

    /// `export.frames`: the animation as `stem-N.png` / `stem-N.tga` files next to `<path>`.
    pub(crate) fn op_export_frames(&mut self, request: &Value) -> Result<Value, String> {
        let options = VideoExportOptions::from_request(request)?;
        options.validate()?;
        let raw = required_string(request, "path")?;
        let destination = Path::new(raw);
        let target = normalized_export_target(raw, destination, self.file_path.as_deref(), "export.frames")?;
        if self.file_path.as_deref().is_some_and(|open| is_frame_name_of(open, &target)) {
            return Err("export.frames would overwrite the open document".into());
        }
        let plan = export::plan_video(self, &options);
        self.history_op = "export.frames".into();
        let plan = plan?;
        let pixels = plan.frames() as u64 * u64::from(plan.camera.width) * u64::from(plan.camera.height);
        if pixels > MAX_FRAMES_REQUEST_PIXELS {
            return Err(format!(
                "export.frames is limited to {MAX_FRAMES_REQUEST_PIXELS} pixels in total (frames x width x height); \
                 this request has {pixels}. Use export.video.start with an image codec (png, tga) for larger runs"
            ));
        }
        let paths = export::export_frame_sequence(self, &options, &target);
        let paths = paths?;
        let name = |path: Option<&std::path::PathBuf>| path.map(|p| p.to_string_lossy().into_owned());
        let reply = json!({"frames": paths.len(), "first": name(paths.first()), "last": name(paths.last())});
        Ok(with_images_left_out(reply, &self.document.model, &plan))
    }

    /// `export.gif`: the animation as one animated GIF at `<path>` with the `.gif` extension.
    pub(crate) fn op_export_gif(&mut self, request: &Value) -> Result<Value, String> {
        let options = VideoExportOptions::from_request(request)?;
        options.validate()?;
        let gif = export::GifOptions::from_request(request)?;
        let raw = required_string(request, "path")?;
        let named = Path::new(raw).with_extension("gif");
        let target = normalized_export_target(raw, &named, self.file_path.as_deref(), "export.gif")?;
        let plan = export::plan_gif(self, &options);
        self.history_op = "export.gif".into();
        let (_, plan) = plan?;
        let pixels = plan.frames() as u64 * u64::from(plan.camera.width) * u64::from(plan.camera.height);
        if pixels > MAX_FRAMES_REQUEST_PIXELS {
            return Err(format!(
                "export.gif is limited to {MAX_FRAMES_REQUEST_PIXELS} pixels in total (frames x width x height); \
                 this request has {pixels}"
            ));
        }
        let done = export::export_gif(self, &options, &gif, &target)?;
        let reply = json!({
            "path": done.path.to_string_lossy(), "frames": done.frames, "width": done.width, "height": done.height,
            "delay": done.delay,
        });
        Ok(with_images_left_out(reply, &self.document.model, &plan))
    }

    /// `export.collect`: a copy of the open INX document in `dir`, with every file its DepthDraw layers name
    /// copied to `dir/assets/` and named there by a path relative to the copy, so the folder moves as one. The
    /// open document and its own links stay as they are.
    pub(crate) fn op_export_collect(&mut self, request: &Value) -> Result<Value, String> {
        let plan = self.prepare_collection(request)?;
        self.execute_collection(plan)
    }

    pub(crate) fn prepare_collection(&self, request: &Value) -> Result<export::CollectionPlan, String> {
        if !self.document.is_inx() {
            return Err("export.collect needs an INX document; a Live2D source keeps its own links".into());
        }
        let raw = required_string(request, "dir")?;
        if raw.is_empty() { return Err("dir must not be empty".into()) }
        let overwrite = match request.get("overwrite") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(overwrite)) => *overwrite,
            Some(_) => return Err("overwrite must be a boolean".into()),
        };
        let directory_alias = PathBuf::from(raw);
        let dir = resolve_collection_path(&directory_alias)?;
        let stem = self.file_path.as_deref().and_then(Path::file_stem).map(|stem| stem.to_string_lossy().into_owned());
        let document = dir.join(format!("{}.inx", stem.as_deref().unwrap_or("model")));
        let staged = self.staged_flush()?;
        let mut model = self.model_for_save(staged.as_ref().map_or(&self.document.model, |flush| &flush.model).clone());
        // (source, copy) pairs; a source several layers name is copied once.
        let mut copies = Vec::new();
        if let Some(text) = model.get(crate::depth_session::MANIFEST_KEY).and_then(Value::as_str) {
            let mut manifest: Value =
                serde_json::from_str(text).map_err(|_| "the DepthDraw manifest of this document is not readable")?;
            let mut named: BTreeMap<PathBuf, String> = BTreeMap::new();
            // Lower case: a case-insensitive file system holds `Back.png` and `back.png` as one file.
            let mut used: BTreeSet<String> = BTreeSet::new();
            let layers = manifest.get_mut("layers").and_then(Value::as_array_mut).into_iter().flatten();
            for layer in layers.filter(|layer| layer.is_object()) {
                let source = layer["sourcePath"].as_str().unwrap_or_default().to_owned();
                if source.is_empty() {
                    continue;
                }
                let file = fs::canonicalize(self.depth_source(&source))
                    .ok()
                    .filter(|file| file.is_file())
                    .ok_or_else(|| format!("the DepthDraw source {source} was not found"))?;
                if !named.contains_key(&file) {
                    let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                    let (base, extension) = match name.rsplit_once('.') {
                        Some((base, extension)) if !base.is_empty() => (base, format!(".{extension}")),
                        _ => (name.as_str(), String::new()),
                    };
                    let mut reference = format!("assets/{name}");
                    for number in 2.. {
                        if used.insert(reference.to_lowercase()) {
                            break;
                        }
                        reference = format!("assets/{base}-{number}{extension}");
                    }
                    copies.push(collection_file(self.depth_source(&source), file.clone(), dir.join(&reference))?);
                    named.insert(file.clone(), reference);
                }
                layer["sourcePath"] = json!(named[&file]);
            }
            if !named.is_empty() {
                model[crate::depth_session::MANIFEST_KEY] = json!(manifest.to_string());
            }
        }
        let mut sequence_index = 0;
        for clip in model.get_mut("animations").and_then(Value::as_object_mut).into_iter().flat_map(|clips| clips.values_mut()) {
            for track in clip.get_mut("images").and_then(Value::as_array_mut).into_iter().flatten() {
                let Some(sequence) = track.get_mut("sequence") else { continue; };
                let folder = format!("assets/sequence-{sequence_index}");
                sequence_index += 1;
                let files = sequence.get_mut("files").and_then(Value::as_array_mut).ok_or("sequence files are missing")?;
                if files.is_empty() || files.len() > crate::animation::MAX_SEQUENCE_FILES {
                    return Err("sequence file count is invalid".into());
                }
                let mut numbers = BTreeSet::new();
                for file in files.iter_mut() {
                    let number = file["number"].as_u64().ok_or("sequence frame number is missing")?;
                    if !numbers.insert(number) { return Err("sequence has duplicate frame numbers".into()); }
                    let source = file["path"].as_str().ok_or("sequence file path is missing")?;
                    export::local_path(source).map_err(str::to_owned)?;
                    let source_alias = Path::new(source);
                    if !source_alias.is_absolute() { return Err("sequence file path must be absolute".into()); }
                    let source = fs::canonicalize(source_alias).map_err(|error| format!("sequence source: {error}"))?;
                    if !source.is_file() { return Err("sequence source must be a file".into()); }
                    let extension = source.extension().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();
                    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg") {
                        return Err("sequence source must be PNG or JPEG".into());
                    }
                    let name = format!("frame{number}.{extension}");
                    let reference = format!("{folder}/{name}");
                    copies.push(collection_file(source_alias.to_owned(), source, dir.join(&reference))?);
                    file["path"] = json!(reference);
                }
                let first_path = files[0]["path"].clone();
                sequence["directory"] = json!(folder);
                sequence["prefix"] = json!("frame");
                track["path"] = first_path;
            }
        }
        let bytes = self.document.with_model(model).encode()?;
        let plan = export::CollectionPlan {
            directory: dir, directory_alias, copies,
            document: resolve_collection_destination(&document)?, document_alias: document,
            bytes: bytes.into(), overwrite,
        };
        validate_collection_plan(&plan, self.file_path.as_deref())?;
        Ok(plan)
    }

    pub(crate) fn execute_collection(&mut self, plan: export::CollectionPlan) -> Result<Value, String> {
        validate_collection_plan(&plan, self.file_path.as_deref())?;
        // The images first and the document last, so a failed copy never leaves a document naming a missing image.
        let mut targets: Vec<(PathBuf, Option<&Path>)> =
            plan.copies.iter().map(|file| (file.destination.clone(), Some(file.source.as_path()))).collect();
        targets.push((plan.document.clone(), None));
        for (target, _) in &targets {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            }
        }
        // ponytail: after a failure the files made here are removed, but the folders stay and a file replaced
        // under overwrite stays replaced.
        let mut created: Vec<&PathBuf> = Vec::new();
        for (path, source) in &targets {
            let existed = path.exists();
            // ponytail: each image is read whole, one at a time.
            let written = match source {
                Some(source) => fs::read(source)
                    .map_err(|error| format!("could not read {}: {error}", source.display()))
                    .and_then(|data| write_atomic(path, &data, plan.overwrite)),
                None => write_atomic(path, &plan.bytes, plan.overwrite),
            };
            if let Err(error) = written {
                created.iter().for_each(|file| drop(fs::remove_file(file)));
                return Err(error);
            }
            if !existed {
                created.push(path);
            }
        }
        Ok(json!({
            "path": targets[targets.len() - 1].0.to_string_lossy(),
            "files": plan.copies.iter().map(|file| file.destination.to_string_lossy()).collect::<Vec<_>>(),
        }))
    }

    /// `export.video.start`: plans the video, then encodes it on a background thread.
    pub(crate) fn prepare_video_export(&mut self, request: &Value) -> Result<PreparedVideoExport, String> {
        if self.video.jobs.values().any(|job| job.outcome.is_none() && !job.handle.as_ref().is_some_and(JoinHandle::is_finished)) {
            return Err("a video export is already running".into());
        }
        self.plan_video_export(request)
    }

    fn plan_video_export(&self, request: &Value) -> Result<PreparedVideoExport, String> {
        let options = VideoExportOptions::from_request(request)?;
        options.validate()?;
        let raw = required_string(request, "path")?;
        let codec = match request.get("codec") {
            None | Some(Value::Null) => None,
            Some(Value::String(codec)) => Some(codec.as_str()),
            Some(_) => return Err("codec must be a string".into()),
        };
        let preset = match request.get("preset") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(export::VideoPreset::parse(name)?),
            Some(_) => return Err("preset must be a string".into()),
        };
        if request.get("ffmpeg_options").is_some() {
            return Err("ffmpeg_options cannot be set by a request; the host configures extra encoder options".into());
        }
        // ffmpeg reads the audio by its resolved absolute name, so no protocol name reaches it (and `ffmpeg_command`
        // lets it read only files). A requested file replaces the clip's audio tracks.
        let audio = match request.get("audio") {
            None | Some(Value::Null) => None,
            Some(Value::String(audio)) => {
                let path = fs::canonicalize(audio)
                    .ok()
                    .filter(|file| file.is_file())
                    .ok_or_else(|| format!("audio file {audio} was not found"))?
                    .to_string_lossy()
                    .into_owned();
                // ffmpeg reads a name with `%` as a numbered sequence of files.
                if path.contains('%') {
                    return Err(format!("audio file {audio} must not contain '%'"));
                }
                Some(vec![AudioInput { path, delay_ms: 0 }])
            }
            Some(_) => return Err("audio must be a string".into()),
        };
        let (codec, preset_options, path) = match preset {
            None => (codec.unwrap_or("auto"), "", raw.to_owned()),
            Some(_) if codec.is_some() => return Err("give codec or preset, not both".into()),
            Some(export::VideoPreset::Mp4) if options.transparency => {
                return Err("mp4 has no alpha channel; use the webm or mov preset for transparency".into());
            }
            Some(preset) => {
                let (codec, preset_options) = preset.encoder(options.transparency);
                let named = Path::new(raw).with_extension(preset.extension());
                (codec, preset_options, named.to_string_lossy().into_owned())
            }
        };
        let path = path.as_str();
        let target = normalized_export_target(raw, Path::new(path), self.file_path.as_deref(), "export.video.start")?;
        // ffmpeg reads names such as `file:x`, `tcp:host:port` and `x-%d.jpg` as protocols or patterns, so it only
        // ever gets the absolute target, and a `%` is refused.
        let target_text = target.to_string_lossy().into_owned();
        if target_text.contains('%') {
            return Err("path must not contain '%'".into());
        }
        // Image codecs write `stem-N.ext` instead of `path`.
        if self.file_path.as_deref().is_some_and(|open| is_frame_name_of(open, &target)) {
            return Err("export.video.start would overwrite the open document".into());
        }
        if !self.snapshot()["motion3"].is_null() { return Err("stop the Motion3 preview before exporting a video".into()); }
        let source = self.export_copy()?;
        let plan = export::plan_video(&mut source.clone().open()?, &options)?;
        let (audio, left_out) = match audio {
            Some(audio) => (audio, Vec::new()),
            None => export::clip_audio(&self.document.model, &plan.animation, &plan.plan, codec, &target_text),
        };
        let output = VideoOutput {
            program: self.video.program.clone(),
            codec: codec.to_owned(),
            file: target_text,
            // The host's options come last, so they can override the preset's.
            ffmpeg_options: format!("{preset_options} {}", self.video.options),
            audio,
        };
        // Reject a bad codec or output name now rather than from the thread.
        let command = export::encoder_launch_command(&self.video.program, &EncoderSettings {
            codec: output.codec.clone(),
            framerate: u32::try_from(plan.plan.fps).unwrap_or(0),
            file: output.file.clone(),
            frames: u32::try_from(plan.frames()).unwrap_or(u32::MAX),
            width: plan.camera.width,
            height: plan.camera.height,
            transparency: options.transparency,
            ffmpeg_options: output.ffmpeg_options.clone(),
            audio: output.audio.clone(),
        }, options.overwrite)?;
        export::check_video_primary_output(&command, plan.frames(), self.file_path.as_deref())?;
        let consent = export::VideoConsent {
            command, directory: std::env::current_dir().map_err(|error| format!("cannot read working directory: {error}"))?,
            plan, source, source_path: self.file_path.clone(), overwrite: options.overwrite, audio_left_out: left_out,
            host_program: self.video.program.clone(), host_options: self.video.options.clone(),
        };
        Ok(PreparedVideoExport { consent, options, path: path.to_owned() })
    }

    pub(crate) fn op_export_video_probe(&mut self) -> Result<Value, String> {
        if self.op_export_video_probe_status()["state"] == "running" {
            return Err("a codec probe is already running".into());
        }
        let program = self.video.program.clone();
        let worker_program = program.clone();
        let handle = thread::Builder::new().name("video-codec-probe".into())
            .spawn(move || export::probe_encoders(&worker_program, Duration::from_secs(5)))
            .map_err(|error| format!("cannot start codec probe: {error}"))?;
        self.video.probe = Some(CodecProbe { program: program.clone(), handle: Some(handle), outcome: None });
        Ok(json!({"state":"running","program":program}))
    }

    pub(crate) fn op_export_video_probe_status(&mut self) -> Value {
        match self.video.probe.as_mut() {
            Some(probe) => probe.status(),
            None => json!({"state":"idle"}),
        }
    }

    /// Last successful non-executing command plan; replacing the document clears it.
    pub fn video_command_preview(&self) -> &Value { &self.video_command_preview }

    pub(crate) fn op_export_video_command(&mut self, request: &Value) -> Result<Value, String> {
        let prepared = self.plan_video_export(request)?;
        let consent = prepared.consent;
        let plan = consent.plan;
        let preview = with_images_left_out(json!({"program":consent.command.program,"args":consent.command.args,
            "directory":consent.directory,"path":prepared.path,"overwrite":consent.overwrite,
            "frames":plan.frames(),"width":plan.camera.width,"height":plan.camera.height,
            "fps":plan.plan.fps,"audioLeftOut":consent.audio_left_out}), &self.document.model, &plan);
        self.video_command_preview = preview.clone();
        Ok(preview)
    }

    pub(crate) fn op_export_video_start(&mut self, request: &Value) -> Result<Value, String> {
        let prepared = self.prepare_video_export(request)?;
        self.launch_video_export(prepared)
    }

    pub(crate) fn launch_video_export(&mut self, prepared: PreparedVideoExport) -> Result<Value, String> {
        let PreparedVideoExport { consent, options, path } = prepared;
        export::check_video_primary_output(&consent.command, consent.plan.frames(), self.file_path.as_deref())?;
        let plan = consent.plan;
        let left_out = consent.audio_left_out;
        for job in self.video.jobs.values_mut() { job.settle(); }
        let cancel = Arc::new(AtomicBool::new(false));
        let (done, total) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(plan.frames())));
        let (worker_cancel, worker_done) = (Arc::clone(&cancel), Arc::clone(&done));
        let render_plan = plan.clone();
        let handle = thread::spawn(move || {
            export::check_video_primary_output(&consent.command, render_plan.frames(), consent.source_path.as_deref())?;
            let mut session = consent.source.open()?;
            export::export_video_planned(&mut session, &options, &render_plan, &consent.command,
                Some(&consent.directory), worker_cancel, &mut |frames, _| {
                    worker_done.store(frames, Ordering::Relaxed);
                })?;
            Ok(render_plan.frames())
        });
        let id = self.video.next;
        self.video.next += 1;
        self.video.jobs.insert(id, VideoJob {
            cancel, done, total, path: path.to_owned(), handle: Some(handle), outcome: None,
        });
        while self.video.jobs.len() > KEPT_JOBS {
            self.video.jobs.pop_first();
        }
        let mut reply = json!({
            "job": id, "path": path, "frames": plan.frames(), "width": plan.camera.width,
            "height": plan.camera.height, "fps": plan.plan.fps,
        });
        if !left_out.is_empty() {
            reply["audioLeftOut"] = json!(left_out);
        }
        Ok(with_images_left_out(reply, &self.document.model, &plan))
    }

    fn video_job(&mut self, request: &Value) -> Result<(u64, &mut VideoJob), String> {
        let id = request.get("job").and_then(Value::as_u64).ok_or("job must be a job id")?;
        let job = self.video.jobs.get_mut(&id).ok_or("unknown video export job")?;
        Ok((id, job))
    }

    /// `export.video.status`: never blocks; `finished`, `failed` and `cancelled` are final.
    pub(crate) fn op_export_video_status(&mut self, request: &Value) -> Result<Value, String> {
        let (id, job) = self.video_job(request)?;
        Ok(job.status(id))
    }

    /// `export.video.cancel`: asks the encode to stop (ffmpeg is closed, the partial file stays). The state
    /// turns `cancelled` once the thread has stopped; cancelling a final job changes nothing.
    pub(crate) fn op_export_video_cancel(&mut self, request: &Value) -> Result<Value, String> {
        let (id, job) = self.video_job(request)?;
        job.cancel.store(true, Ordering::Relaxed);
        Ok(job.status(id))
    }
}

/// `reply` with `imagesLeftOut`, the clip's image tracks (ANI04) the export could not draw, when there are any.
// ponytail: the check decodes each image once more (one at a time) on the request thread, before the render decodes
// them to keep; a header-only check when large backgrounds make the request slow.
fn with_images_left_out(mut reply: Value, model: &Value, plan: &export::VideoPlan) -> Value {
    let left_out = export::image_source_warnings(model, &plan.animation);
    if !left_out.is_empty() {
        reply["imagesLeftOut"] = json!(left_out);
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_quit_background_query_does_not_join_or_poll_a_worker() {
        let mut session = Session::new();
        let (release, wait) = std::sync::mpsc::channel();
        session.video.probe = Some(CodecProbe { program:"waiting-stub".into(),
            handle:Some(thread::spawn(move || { wait.recv().unwrap(); Ok(Vec::new()) })), outcome:None });
        assert!(session.has_background_work());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while session.has_background_work() { assert!(std::time::Instant::now() < deadline); thread::yield_now(); }
        let probe = session.video.probe.as_ref().unwrap();
        assert!(probe.handle.is_some() && probe.outcome.is_none(), "the query did not join or settle the worker");
        assert_eq!(session.op_export_video_probe_status()["state"], "finished");
    }

    #[test]
    fn shared_codec_probe_panic_is_a_stable_failure_and_drop_does_not_join() {
        let mut session = Session::new();
        session.video.probe = Some(CodecProbe { program:"panic-stub".into(),
            handle:Some(thread::spawn(|| panic!("probe test panic"))), outcome:None });
        while !session.video.probe.as_ref().unwrap().handle.as_ref().unwrap().is_finished() {
            thread::yield_now();
        }
        let status = session.op_export_video_probe_status();
        assert_eq!(status, json!({"state":"failed","program":"panic-stub","error":"the codec probe panicked"}));
        assert_eq!(session.op_export_video_probe_status(), status);
        let (release, wait) = std::sync::mpsc::channel();
        session.video.probe = Some(CodecProbe { program:"waiting-stub".into(),
            handle:Some(thread::spawn(move || { wait.recv().unwrap(); Ok(Vec::new()) })), outcome:None });
        assert_eq!(session.op_export_video_probe_status(), json!({"state":"running","program":"waiting-stub"}));
        drop(session);
        release.send(()).unwrap();
    }

    fn collection_fixture(name: &str) -> (Session, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("soul2d-collect-{name}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");fs::write(&source, b"external source bytes").unwrap();
        let mut session = Session::new();
        let mut model = session.document.model.clone();
        model[crate::depth_session::MANIFEST_KEY] = json!(json!({"layers":[{"sourcePath":source}]}).to_string());
        let reply = session.handle_request(&json!({"op":"new","model":model}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        (session, root, source)
    }

    #[test]
    fn collection_prepares_missing_directories_without_writing_and_executes_the_same_plan() {
        let (mut session, root, source) = collection_fixture("prepared");
        let before = session.snapshot();
        assert!(session.prepare_collection(&json!({"dir":""})).is_err());
        let output = root.join("missing/nested/project");
        let plan = session.prepare_collection(&json!({"dir":output})).unwrap();
        assert!(!root.join("missing").exists());assert_eq!(session.snapshot(), before);
        assert_eq!(plan.copies.len(), 1);
        assert_eq!(plan.copies[0].source, source.canonicalize().unwrap());
        let expected = plan.clone();
        let reply = session.execute_collection(plan).unwrap();
        assert_eq!(reply["path"], expected.document.to_string_lossy().as_ref());
        assert_eq!(fs::read(&expected.document).unwrap(), &*expected.bytes);
        assert_eq!(fs::read(&expected.copies[0].destination).unwrap(), fs::read(&source).unwrap());
        assert_eq!(session.snapshot(), before);
        let mut reopened = Session::new();
        let reply = reopened.handle_request(&json!({"op":"open","path":expected.document}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        let manifest: Value = serde_json::from_str(reopened.document.model[crate::depth_session::MANIFEST_KEY].as_str().unwrap()).unwrap();
        assert_eq!(manifest["layers"][0]["sourcePath"], "assets/source.png");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn collection_rejects_retargeted_sources_and_output_parents_without_writing() {
        use std::os::unix::fs::symlink;
        let (mut session, root, source) = collection_fixture("retarget");
        let alias = root.join("source-link.png");symlink(&source, &alias).unwrap();
        let mut model = session.document.model.clone();
        model[crate::depth_session::MANIFEST_KEY] = json!(json!({"layers":[{"sourcePath":alias}]}).to_string());
        let reply = session.handle_request(&json!({"op":"new","model":model}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        let output = root.join("out");
        let request = json!({"dir":output});
        let plan = session.prepare_collection(&request).unwrap();
        let second = root.join("second.png");fs::write(&second, b"other").unwrap();
        fs::remove_file(&alias).unwrap();symlink(&second, &alias).unwrap();
        assert_ne!(session.prepare_collection(&request).unwrap(), plan);
        assert!(session.execute_collection(plan).is_err());assert!(!output.exists());
        fs::create_dir_all(&output).unwrap();
        let first_assets = root.join("first-assets");let second_assets = root.join("second-assets");
        fs::create_dir(&first_assets).unwrap();fs::create_dir(&second_assets).unwrap();
        symlink(&first_assets, output.join("assets")).unwrap();
        let plan = session.prepare_collection(&request).unwrap();
        assert_eq!(plan.copies[0].destination.parent().unwrap(), first_assets.canonicalize().unwrap());
        fs::remove_file(output.join("assets")).unwrap();symlink(&second_assets, output.join("assets")).unwrap();
        assert!(session.execute_collection(plan).is_err());
        assert_eq!(fs::read_dir(&first_assets).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&second_assets).unwrap().count(), 0);
        assert!(!output.join("model.inx").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn collection_paths_resolve_symlinks_before_parent_components_and_reject_dangling_links() {
        use std::os::unix::fs::symlink;
        let (_, root, _) = collection_fixture("paths");
        fs::create_dir_all(root.join("physical/child")).unwrap();
        symlink(root.join("physical/child"), root.join("alias")).unwrap();
        assert_eq!(resolve_collection_path(&root.join("alias/../missing/leaf")).unwrap(), root.canonicalize().unwrap().join("physical/missing/leaf"));
        symlink(root.join("absent"), root.join("dangling")).unwrap();
        assert!(resolve_collection_path(&root.join("dangling/child")).is_err());
        assert!(resolve_collection_path(&root.join("source.png/child")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn frame_names_follow_the_destination() {
        let target = Path::new("/d/shot.png");
        assert!(is_frame_name_of(Path::new("/d/shot-12.png"), target));
        assert!(!is_frame_name_of(Path::new("/d/shot-x.png"), target));
        assert!(!is_frame_name_of(Path::new("/d/shot-.png"), target));
        assert!(!is_frame_name_of(Path::new("/e/shot-1.png"), target));
        assert!(!is_frame_name_of(Path::new("/d/shot-1.tga"), target));
    }

}

