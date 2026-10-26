//! `animation.motionsync.*` (ANI26): key lip-sync parameter values from the mouth-shape cues an external
//! Rhubarb Lip Sync executable produces for a local WAV file, an alternative to the Cubism Editor's CRI
//! MotionSync (https://docs.live2d.com/en/cubism-editor-manual/motion-sync/ and .../motion-sync-bake/), not
//! a port of it. The host picks the analyzer with the `SOUL2D_RHUBARB` environment variable at host
//! creation (otherwise `rhubarb` on PATH); nothing here downloads or installs one, and a request can never
//! choose the program, its options or its timeout.
//!
//! `start` only analyzes. It copies the WAV once into a private temporary file for the worker.
//! Starting a job still reads the source WAV on the session thread. Analysis leaves the document,
//! history and preview unchanged. The worker owns the subprocess with captured-output limits,
//! a deadline and a cancel flag. `status` and `cancel` collect finished outcomes without changing the
//! model. `Session` drop cancels and joins workers so no analyzer child is orphaned. Only `bake` writes.
//! It requires a finished job and the same canonical WAV path and length plus CRC32 fingerprint.
//! This fingerprint detects stale input, not authenticity. `bake` keys the mapped shape values on
//! every frame `from..=to` as Linear/Forced lanes in one commit. Cues use half-open intervals
//! `[start, end)` at audio time `(frame - audio.frame) * timestep`. Frames outside the recording take
//! the rest shape `X`. Rhubarb shapes are A-H/X, not CRI A/I/U/E/O. The caller maps each shape to a value.

use crate::{animation, export, frame_step, required_string, valid_id, Session};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The mouth shapes the analyzer reports, in UI/schema order: A-H plus X, the rest shape. Rhubarb calls
/// only G, H and X its "extended shapes"; the set is not the CRI A/I/U/E/O mouth set.
pub const SHAPES: [&str; 9] = ["A", "B", "C", "D", "E", "F", "G", "H", "X"];

const MAX_WAV_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STDOUT: u64 = 8 * 1024 * 1024;
const MAX_STDERR: u64 = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(10);
const MAX_SETTLED_JOBS: usize = 4;
const MAX_DURATION: f64 = 300.0;
const MAX_CUES: usize = 100_000;
const MAX_BAKE_FRAMES: u64 = 2000;
const MAX_BAKE_KEYS: u64 = 200_000;
const MAX_MAPPINGS: usize = 32;

#[derive(Clone)]
struct Cue {
    start: f64,
    end: f64,
    shape: &'static str,
}

#[derive(Clone)]
struct Analysis {
    duration: f64,
    cues: Vec<Cue>,
}

enum Outcome {
    Finished(Analysis),
    Failed(String),
    Cancelled,
}

enum WorkerError {
    Cancelled,
    Failed(String),
}

impl From<String> for WorkerError {
    fn from(error: String) -> Self {
        WorkerError::Failed(error)
    }
}

struct Job {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Outcome>>,
    outcome: Option<Outcome>,
    path: String,
    len: u64,
    crc: u32,
    recognizer: String,
}

impl Job {
    fn settle(&mut self) {
        if self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            self.outcome = Some(match self.handle.take().expect("the handle was checked").join() {
                Ok(outcome) => outcome,
                Err(_) => Outcome::Failed("the analyzer worker panicked".into()),
            });
        }
    }

    fn running(&self) -> bool {
        self.outcome.is_none()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) struct MotionHost {
    program: String,
    deadline: Duration,
    jobs: BTreeMap<u64, Job>,
    next: u64,
}

impl Default for MotionHost {
    fn default() -> Self {
        Self {
            program: std::env::var("SOUL2D_RHUBARB")
                .ok()
                .filter(|program| !program.trim().is_empty())
                .unwrap_or_else(|| "rhubarb".into()),
            deadline: DEADLINE,
            jobs: BTreeMap::new(),
            next: 1,
        }
    }
}

impl MotionHost {
    pub(crate) fn has_background_work(&self) -> bool {
        self.jobs.values().any(|job| job.handle.as_ref().is_some_and(|handle| !handle.is_finished()))
    }
}

struct TempDir {
    dir: PathBuf,
    files: Vec<PathBuf>,
}

impl TempDir {
    fn create() -> Result<Self, String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| format!("cannot read random bytes: {error}"))?;
        let mut suffix = String::with_capacity(32);
        for byte in random {
            suffix.push_str(&format!("{byte:02x}"));
        }
        let dir = std::env::temp_dir().join(format!("soul2d-motionsync-{}-{suffix}", std::process::id()));
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        Ok(Self { dir, files: Vec::new() })
    }

    fn file(&mut self, name: &str) -> Result<(PathBuf, File), String> {
        let path = self.dir.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(&path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        self.files.push(path.clone());
        Ok((path, file))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for file in self.files.drain(..) {
            let _ = fs::remove_file(file);
        }
        let _ = fs::remove_dir(&self.dir);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn kill_reap(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_reap();
    }
}

fn read_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(cap + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > cap {
        return Err(format!("{} is larger than {cap} bytes", path.display()));
    }
    Ok(bytes)
}

fn collect_output(path: &Path, cap: u64, what: &str) -> Result<Vec<u8>, String> {
    let size = path
        .metadata()
        .map_err(|error| format!("cannot stat the analyzer's {what}: {error}"))?
        .len();
    if size > cap {
        return Err(format!("the analyzer wrote more {what} than the {cap}-byte limit"));
    }
    read_bounded(path, cap).map_err(|error| format!("cannot read the analyzer's {what}: {error}"))
}

fn read_source(path: &str) -> Result<(Vec<u8>, u64, u32), String> {
    let bytes = read_bounded(Path::new(path), MAX_WAV_BYTES)?;
    let len = bytes.len() as u64;
    let crc = crc32fast::hash(&bytes);
    Ok((bytes, len, crc))
}

fn analyze(
    program: &str,
    recognizer: &str,
    bytes: &[u8],
    cancel: Arc<AtomicBool>,
    deadline: Duration,
) -> Outcome {
    let work = || -> Result<Analysis, WorkerError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(WorkerError::Cancelled);
        }
        let mut dir = TempDir::create()?;
        let (wav, mut input) = dir.file("input.wav")?;
        input
            .write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", wav.display()))?;
        drop(input);
        let (stdout_path, stdout) = dir.file("stdout.json")?;
        let (stderr_path, stderr) = dir.file("stderr.txt")?;
        let input_arg = wav.to_string_lossy().into_owned();
        let child = Command::new(program)
            .args([
                "--recognizer",
                recognizer,
                "--exportFormat",
                "json",
                "--extendedShapes",
                "GHX",
                "--threads",
                "2",
                "--quiet",
                &input_arg,
            ])
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("cannot start the analyzer {program:?}: {error}"))?;
        let mut child = ChildGuard(Some(child));
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(WorkerError::Cancelled);
            }
            match child.0.as_mut().expect("the child is alive").try_wait() {
                Ok(Some(status)) => {
                    let stdout = collect_output(&stdout_path, MAX_STDOUT, "output")?;
                    let stderr = collect_output(&stderr_path, MAX_STDERR, "diagnostics")?;
                    if !status.success() {
                        let tail = String::from_utf8_lossy(&stderr);
                        let tail = tail.trim().chars().rev().take(2000).collect::<String>();
                        let tail: String = tail.chars().rev().collect();
                        return Err(format!("the analyzer exited with {status}: {tail}").into());
                    }
                    return parse(&stdout, &input_arg).map_err(WorkerError::Failed);
                }
                Ok(None) => {
                    if started.elapsed() > deadline {
                        return Err(format!(
                            "the analyzer did not finish within {} seconds",
                            deadline.as_secs_f64().round() as u64
                        )
                        .into());
                    }
                    for (path, cap, what) in [
                        (&stdout_path, MAX_STDOUT, "output"),
                        (&stderr_path, MAX_STDERR, "diagnostics"),
                    ] {
                        let size = path.metadata().map(|meta| meta.len()).unwrap_or(0);
                        if size > cap {
                            return Err(format!("the analyzer wrote more {what} than the limit").into());
                        }
                    }
                    std::thread::sleep(POLL);
                }
                Err(error) => return Err(format!("could not wait for the analyzer: {error}").into()),
            }
        }
    };
    match work() {
        Ok(analysis) => Outcome::Finished(analysis),
        Err(WorkerError::Cancelled) => Outcome::Cancelled,
        Err(WorkerError::Failed(error)) => Outcome::Failed(error),
    }
}

fn parse(stdout: &[u8], input: &str) -> Result<Analysis, String> {
    let value: Value = serde_json::from_slice(stdout)
        .map_err(|error| format!("the analyzer output is not JSON: {error}"))?;
    let metadata = value
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or("the analyzer output has no metadata object")?;
    let duration = metadata
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|duration| duration.is_finite() && *duration > 0.0 && *duration <= MAX_DURATION)
        .ok_or("the analyzer reported an unusable duration")?;
    match metadata.get("soundFile") {
        None => {}
        Some(sound) => {
            let sound = sound
                .as_str()
                .ok_or("the analyzer's metadata.soundFile must be a string")?;
            if sound != input {
                return Err("the analyzer reported a different sound file than its input".into());
            }
        }
    }
    let cues = value
        .get("mouthCues")
        .and_then(Value::as_array)
        .ok_or("the analyzer output has no mouthCues array")?;
    if cues.len() > MAX_CUES {
        return Err(format!("the analyzer reported more than {MAX_CUES} mouth cues"));
    }
    let mut parsed = Vec::with_capacity(cues.len());
    let mut at = 0.0;
    for (index, cue) in cues.iter().enumerate() {
        let where_ = format!("mouthCues[{index}]");
        let object = cue.as_object().ok_or_else(|| format!("{where_} must be an object"))?;
        let start = object
            .get("start")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| format!("{where_}.start must be a finite nonnegative number"))?;
        let end = object
            .get("end")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > start)
            .ok_or_else(|| format!("{where_}.end must be a finite number after start"))?;
        if start != at {
            return Err(format!("{where_} does not continue the cue before it (gap, overlap or wrong order)"));
        }
        let shape = object
            .get("value")
            .and_then(Value::as_str)
            .and_then(|value| SHAPES.iter().copied().find(|shape| *shape == value))
            .ok_or_else(|| format!("{where_}.value must be one of {}", SHAPES.join("/")))?;
        parsed.push(Cue { start, end, shape });
        at = end;
    }
    if at != duration {
        return Err("the mouth cues do not cover the whole recording duration".into());
    }
    Ok(Analysis { duration, cues: parsed })
}

fn shape_at(cues: &[Cue], mut at: usize, time: f64) -> (usize, &'static str) {
    while at < cues.len() && cues[at].end <= time {
        at += 1;
    }
    match cues.get(at) {
        Some(cue) if cue.start <= time && time < cue.end => (at, cue.shape),
        _ => (at, "X"),
    }
}

fn cue_json(cues: &[Cue]) -> Vec<Value> {
    cues.iter()
        .map(|cue| json!({"start": cue.start, "end": cue.end, "value": cue.shape}))
        .collect()
}

fn job_status(id: u64, job: &Job) -> Value {
    let mut status = json!({
        "job": id,
        "backend": "rhubarb",
        "recognizer": job.recognizer,
        "path": job.path,
    });
    match &job.outcome {
        None => status["state"] = json!("running"),
        Some(Outcome::Finished(analysis)) => {
            status["state"] = json!("finished");
            status["duration"] = json!(analysis.duration);
            status["cues"] = json!(cue_json(&analysis.cues));
        }
        Some(Outcome::Failed(error)) => {
            status["state"] = json!("failed");
            status["error"] = json!(error);
        }
        Some(Outcome::Cancelled) => status["state"] = json!("cancelled"),
    }
    status
}

fn job_id(request: &Value) -> Result<u64, String> {
    request
        .get("job")
        .and_then(Value::as_u64)
        .ok_or("job must be an integer job id".into())
}

struct Mapping {
    id: u32,
    axis: usize,
    values: [f64; 9],
}

fn leaf_parameter<'a>(model: &'a Value, id: u32) -> Result<&'a Value, String> {
    let parameter = crate::find_parameter(model, id)
        .ok_or_else(|| format!("parameter {id} was not found"))?;
    if parameter.get("groupUUID").is_some() {
        return Err(format!("parameter {id} is a parameter group"));
    }
    Ok(parameter)
}

impl Session {
    /// The analyzer executable; host code may change it, no request can. Default: `SOUL2D_RHUBARB` at host
    /// creation, otherwise `rhubarb` on PATH.
    pub fn set_motion_sync_analyzer(&mut self, program: &str) {
        self.motion_sync.program = program.to_owned();
    }

    /// Test/support hook: a shorter analysis deadline than the 120-second default.
    #[doc(hidden)]
    pub fn set_motion_sync_deadline(&mut self, deadline: Duration) {
        self.motion_sync.deadline = deadline;
    }

    /// `animation.motionsync.start {path, recognizer?}`: run the analyzer on a local WAV on a worker
    /// thread. The bytes are read once now, so the child sees a fixed input even if the file changes.
    pub(crate) fn op_motionsync_start(&mut self, request: &Value) -> Result<Value, String> {
        for key in ["program", "args", "output", "timeout"] {
            if request.get(key).is_some() {
                return Err(format!("{key} is a host setting, not a request field"));
            }
        }
        let recognizer = match request.get("recognizer") {
            None | Some(Value::Null) => "phonetic",
            Some(value) => value
                .as_str()
                .filter(|value| ["phonetic", "pocketSphinx"].contains(value))
                .ok_or("recognizer must be phonetic or pocketSphinx")?,
        }
        .to_owned();
        let path = export::wav_file(required_string(request, "path")?)?;
        let (bytes, len, crc) = read_source(&path)?;
        for job in self.motion_sync.jobs.values_mut() {
            job.settle();
        }
        if self.motion_sync.jobs.values().any(Job::running) {
            return Err("a speech analysis is already running; cancel it or wait for it".into());
        }
        while self.motion_sync.jobs.values().filter(|job| !job.running()).count() >= MAX_SETTLED_JOBS {
            let oldest = self
                .motion_sync
                .jobs
                .iter()
                .filter(|(_, job)| !job.running())
                .map(|(id, _)| *id)
                .next()
                .expect("a settled job exists");
            self.motion_sync.jobs.remove(&oldest);
        }
        let id = self.motion_sync.next;
        self.motion_sync.next = id.checked_add(1).ok_or("the job counter wrapped")?;
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = {
            let program = self.motion_sync.program.clone();
            let recognizer_worker = recognizer.clone();
            let flag = Arc::clone(&cancel);
            let deadline = self.motion_sync.deadline;
            std::thread::spawn(move || analyze(&program, &recognizer_worker, &bytes, flag, deadline))
        };
        self.motion_sync.jobs.insert(
            id,
            Job { cancel, handle: Some(handle), outcome: None, path: path.clone(), len, crc, recognizer },
        );
        Ok(json!({
            "job": id,
            "state": "running",
            "backend": "rhubarb",
            "recognizer": self.motion_sync.jobs[&id].recognizer,
            "path": path,
        }))
    }

    /// `animation.motionsync.status {job}`: never blocks; settled jobs keep their terminal state.
    pub(crate) fn op_motionsync_status(&mut self, request: &Value) -> Result<Value, String> {
        let id = job_id(request)?;
        let job = self
            .motion_sync
            .jobs
            .get_mut(&id)
            .ok_or_else(|| format!("job {id} is not known"))?;
        job.settle();
        Ok(job_status(id, job))
    }

    /// `animation.motionsync.cancel {job}`: sets the cancel flag without blocking; the worker's own
    /// thread kills and reaps the child, so the state reads "running" until the next settle collects
    /// "cancelled". A terminal job is returned unchanged.
    pub(crate) fn op_motionsync_cancel(&mut self, request: &Value) -> Result<Value, String> {
        let id = job_id(request)?;
        let job = self
            .motion_sync
            .jobs
            .get_mut(&id)
            .ok_or_else(|| format!("job {id} is not known"))?;
        job.settle();
        if job.outcome.is_none() {
            job.cancel.store(true, Ordering::SeqCst);
            job.settle();
        }
        Ok(job_status(id, job))
    }

    /// `animation.motionsync.bake {name, job, audio_index, mappings, from?, to?, extend?}`: key a finished
    /// analysis on the clip's frames in one undo step. Every refusal leaves document, history and preview
    /// untouched.
    pub(crate) fn op_motionsync_bake(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("animation.motionsync.bake")?;
        let id = job_id(request)?;
        let job = self
            .motion_sync
            .jobs
            .get_mut(&id)
            .ok_or_else(|| format!("job {id} is not known"))?;
        job.settle();
        let (path, len, crc, analysis) = match &job.outcome {
            Some(Outcome::Finished(analysis)) => {
                (job.path.clone(), job.len, job.crc, analysis.clone())
            }
            Some(Outcome::Failed(error)) => return Err(format!("job {id} failed: {error}")),
            Some(Outcome::Cancelled) => return Err(format!("job {id} was cancelled")),
            None => return Err(format!("job {id} is still running")),
        };

        let name = required_string(request, "name")?;
        let model = &self.document.model;
        let clip = model["animations"]
            .get(name)
            .ok_or_else(|| format!("animation {name:?} was not found"))?;
        let length = animation::validate_animation_header(clip)?;
        frame_step::validate(model, clip, length)?;
        if animation::freeze_frame(clip).is_some() {
            return Err("speech cannot be baked while freeze is on; turn freeze off first".into());
        }
        if clip.get("additive") == Some(&json!(true)) {
            return Err("speech cannot be baked into an additive animation".into());
        }
        let weight = clip.get("animationWeight").and_then(Value::as_f64).unwrap_or(1.0);
        if weight != 1.0 {
            return Err("speech cannot be baked into an animation whose animationWeight is not 1".into());
        }
        let timestep = clip["timestep"].as_f64().ok_or("animation timestep is missing")?;

        let audio_index = request
            .get("audio_index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or("audio_index must be an integer audio track index")?;
        let tracks = clip.get("audio").and_then(Value::as_array);
        let track = tracks
            .and_then(|tracks| tracks.get(audio_index))
            .ok_or_else(|| format!("animation {name:?} has no audio track {audio_index}"))?;
        if track.get("mute") == Some(&json!(true)) {
            return Err(format!("audio track {audio_index} of {name:?} is muted"));
        }
        let group = |of: &Value| of.get("group").and_then(Value::as_str).map(str::to_owned);
        let named = |name: &str| {
            clip.get("groups")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        };
        if let Some(group_name) = group(track) {
            let group = named(&group_name)
                .ok_or_else(|| format!("audio track {audio_index} names a group that is not in the clip"))?;
            if group.get("hidden") == Some(&json!(true)) {
                return Err(format!("audio track {audio_index} is in the hidden group {group_name:?}"));
            }
            if group.get("locked") == Some(&json!(true)) {
                return Err(format!("audio track {audio_index} is in the locked group {group_name:?}"));
            }
        }
        if let Some(model_group) = clip.get("modelGroup").and_then(Value::as_str) {
            if named(model_group).is_some_and(|group| group.get("locked") == Some(&json!(true))) {
                return Err(format!("the model is in the locked group {model_group:?}"));
            }
        }
        let track_path = track
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("audio track {audio_index} of {name:?} has no path"))?;
        let current = export::wav_file(track_path)?;
        if current != path {
            return Err(format!(
                "audio track {audio_index} names {current:?}, not the analyzed file {path:?}"
            ));
        }
        let (bytes, current_len, current_crc) = read_source(&current)?;
        if current_len != len || current_crc != crc {
            return Err("the WAV changed since the analysis; run animation.motionsync.start again".into());
        }
        drop(bytes);

        let audio_frame = track
            .get("frame")
            .and_then(Value::as_u64)
            .and_then(|frame| i32::try_from(frame).ok())
            .ok_or_else(|| format!("audio track {audio_index} of {name:?} has no frame"))?;
        let frame_field = |key: &str| -> Result<Option<i32>, String> {
            match request.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_i64()
                    .and_then(|frame| i32::try_from(frame).ok())
                    .filter(|frame| *frame >= 0)
                    .map(Some)
                    .ok_or_else(|| format!("{key} must be a nonnegative integer frame")),
            }
        };
        let from = frame_field("from")?.unwrap_or(audio_frame);
        let to = match frame_field("to")? {
            Some(to) => to,
            None => {
                let steps = (analysis.duration / timestep).ceil();
                if !steps.is_finite() || steps > (animation::MAX_ANIMATION_LENGTH - 1) as f64 {
                    return Err("the recording is too long to cover; pass an explicit to".into());
                }
                audio_frame
                    .checked_add(steps as i32)
                    .filter(|to| i64::from(*to) < animation::MAX_ANIMATION_LENGTH)
                    .ok_or("the recording ends past the supported animation length; pass an explicit to")?
            }
        };
        if from > to {
            return Err("from must not be after to".into());
        }
        if i64::from(to) >= animation::MAX_ANIMATION_LENGTH {
            return Err(format!(
                "frame {to} is past the supported animation length of {}",
                animation::MAX_ANIMATION_LENGTH
            ));
        }
        let extend = match request.get("extend") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(extend)) => *extend,
            Some(_) => return Err("extend must be a boolean".into()),
        };
        if to as usize >= length && !extend {
            return Err(format!(
                "baking to frame {to} needs the clip extended past its length {length}; pass extend true"
            ));
        }
        let new_length = if to as usize >= length { (to as usize) + 1 } else { length };
        let frames = u64::from((to - from) as u32) + 1;
        if frames > MAX_BAKE_FRAMES {
            return Err(format!("baking would cover {frames} frames; the limit is {MAX_BAKE_FRAMES}"));
        }

        let list = request
            .get("mappings")
            .and_then(Value::as_array)
            .ok_or("mappings must be a nonempty array")?;
        if list.is_empty() {
            return Err("mappings must be a nonempty array".into());
        }
        if list.len() > MAX_MAPPINGS {
            return Err(format!("at most {MAX_MAPPINGS} mappings may be baked"));
        }
        let mut mappings = Vec::with_capacity(list.len());
        let mut lanes_taken = BTreeSet::new();
        for (index, entry) in list.iter().enumerate() {
            let where_ = format!("mappings[{index}]");
            let id = entry
                .get("parameter")
                .and_then(Value::as_u64)
                .and_then(valid_id)
                .ok_or_else(|| format!("{where_}.parameter must be a parameter UUID"))?;
            let parameter = leaf_parameter(model, id)?;
            let axes = if parameter.get("is_vec2") == Some(&json!(true)) { 2 } else { 1 };
            let axis = entry
                .get("axis")
                .and_then(Value::as_u64)
                .and_then(|axis| usize::try_from(axis).ok())
                .filter(|axis| *axis < axes)
                .ok_or_else(|| format!("{where_}.axis must be a valid axis of parameter {id}"))?;
            if !lanes_taken.insert((id, axis)) {
                return Err(format!("{where_} repeats the parameter {id} axis {axis} of another mapping"));
            }
            let min = parameter
                .get("min")
                .and_then(|list| list.get(axis))
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("parameter {id}.min axis {axis} is missing"))?;
            let max = parameter
                .get("max")
                .and_then(|list| list.get(axis))
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("parameter {id}.max axis {axis} is missing"))?;
            let shapes = entry
                .get("shapes")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("{where_}.shapes must be an object with every shape value"))?;
            let mut values = [0.0; 9];
            for key in shapes.keys() {
                if !SHAPES.contains(&key.as_str()) {
                    return Err(format!("{where_}.shapes has an unknown shape {key:?}"));
                }
            }
            for (slot, shape) in SHAPES.iter().enumerate() {
                let value = shapes
                    .get(*shape)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && (*value as f32).is_finite())
                    .ok_or_else(|| {
                        format!("{where_}.shapes.{shape} must be a finite number representable as f32")
                    })?;
                if value < min || value > max {
                    return Err(format!(
                        "{where_}.shapes.{shape} {value} is outside parameter {id} axis {axis}'s range \
                         [{min}, {max}]"
                    ));
                }
                values[slot] = value;
            }
            mappings.push(Mapping { id, axis, values });
        }
        let keys = frames
            .checked_mul(mappings.len() as u64)
            .ok_or("the bake would write too many keys")?;
        if keys > MAX_BAKE_KEYS {
            return Err(format!("baking would write {keys} keys; the limit is {MAX_BAKE_KEYS}"));
        }

        let lanes = match clip.get("lanes") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value.as_array().ok_or("animation.lanes must be an array")?,
        };
        for mapping in &mappings {
            let mut seen = false;
            for lane in lanes
                .iter()
                .filter(|lane| lane["uuid"].as_u64() == Some(u64::from(mapping.id)))
            {
                let lane_axis = lane
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(|axis| usize::try_from(axis).ok())
                    .filter(|axis| *axis < axes_of(model, mapping.id))
                    .ok_or_else(|| {
                        format!("a lane of parameter {} has an axis the parameter does not have", mapping.id)
                    })?;
                if lane_axis != mapping.axis {
                    continue;
                }
                if seen {
                    return Err(format!(
                        "parameter {} has two lanes on axis {}; the bake would overwrite one",
                        mapping.id, mapping.axis
                    ));
                }
                seen = true;
                let linear = lane
                    .get("interpolation")
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("linear"));
                let forced = lane
                    .get("merge_mode")
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("forced"));
                if !linear || !forced {
                    return Err(format!(
                        "the lane of parameter {} axis {} must be Linear and Forced so baked keys draw",
                        mapping.id, mapping.axis
                    ));
                }
            }
        }
        let blink = clip
            .get("eyeBlink")
            .and_then(|track| track.get("keyframes"))
            .and_then(Value::as_array)
            .is_some_and(|keys| !keys.is_empty());
        if blink {
            let eyes: BTreeSet<u32> = animation::eye_parameters(model).collect();
            for mapping in &mappings {
                if eyes.contains(&mapping.id) {
                    return Err(format!(
                        "parameter {} is an eye parameter and the clip has blink keys, so its baked keys \
                         would not draw as mapped",
                        mapping.id
                    ));
                }
            }
        }
        for mapping in &mappings {
            for frame in from..=to {
                let remapped =
                    frame_step::parameter_frame(model, clip, mapping.id, length, f64::from(frame))?;
                if remapped != f64::from(frame) {
                    return Err(format!(
                        "frame stepping shows frame {remapped} for parameter {} on frame {frame}, so its \
                         baked keys would not draw as mapped",
                        mapping.id
                    ));
                }
            }
        }

        let mut keyed: BTreeMap<(u32, usize), Vec<(i32, f32)>> = BTreeMap::new();
        let mut at = 0;
        for frame in from..=to {
            let time = f64::from(frame - audio_frame) * timestep;
            let (cursor, shape) = shape_at(&analysis.cues, at, time);
            at = cursor;
            let slot = SHAPES.iter().position(|code| *code == shape).unwrap_or(8);
            for mapping in &mappings {
                keyed
                    .entry((mapping.id, mapping.axis))
                    .or_default()
                    .push((frame, mapping.values[slot] as f32));
            }
        }

        let mut candidate = model.clone();
        let mut written = 0_u64;
        {
            let animation = animation::animation_mut(&mut candidate, name)?;
            if new_length != length {
                animation["length"] = json!(new_length);
            }
            for ((id, axis), keys) in &keyed {
                for (frame, value) in keys {
                    animation::put_key(animation, *id, *axis, *frame, *value, new_length)?;
                    written += 1;
                }
            }
        }
        let base = self.runtime_values.clone();
        for frame in from..=to {
            let sampled = animation::sample(&candidate, name, f64::from(frame), &base, true)?;
            for mapping in &mappings {
                let drawn = sampled.get(&mapping.id).ok_or_else(|| {
                    format!("the baked animation has no value for parameter {}", mapping.id)
                })?;
                let expected = keyed[&(mapping.id, mapping.axis)][(frame - from) as usize].1;
                if (drawn[mapping.axis] - f64::from(expected)).abs() > 1e-6 * f64::from(expected).abs().max(1.0)
                {
                    return Err(format!(
                        "the baked lane of parameter {} axis {} draws {} on frame {frame} instead of \
                         {expected}; held frames, a blink track or another lane may rescale it",
                        mapping.id, mapping.axis, drawn[mapping.axis]
                    ));
                }
            }
        }
        self.commit_model(candidate)?;
        Ok(json!({
            "name": name,
            "job": id,
            "audio_index": audio_index,
            "from": from,
            "to": to,
            "frames": frames,
            "keys": written,
            "parameters": mappings.iter().map(|mapping| mapping.id).collect::<Vec<_>>(),
            "length": new_length,
        }))
    }
}

fn axes_of(model: &Value, id: u32) -> usize {
    crate::find_parameter(model, id)
        .filter(|parameter| parameter.get("groupUUID").is_none())
        .map(|parameter| if parameter.get("is_vec2") == Some(&json!(true)) { 2 } else { 1 })
        .unwrap_or(1)
}

#[cfg(test)]
mod quit_tests {
    use super::*;

    #[test]
    fn remote_quit_queries_speech_work_without_polling_joining_or_cancelling() {
        let mut session = Session::new();
        let before = session.snapshot();
        let (release, wait) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        session.motion_sync.jobs.insert(1, Job { cancel: Arc::clone(&cancel),
            handle: Some(std::thread::spawn(move || { wait.recv().unwrap(); Outcome::Cancelled })),
            outcome: None, path: String::new(), len: 0, crc: 0, recognizer: "stub".into() });
        assert!(session.has_background_work());
        assert!(!cancel.load(Ordering::SeqCst));
        assert_eq!(session.snapshot(), before);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.has_background_work() { assert!(Instant::now() < deadline); std::thread::yield_now(); }
        let job = &session.motion_sync.jobs[&1];
        assert!(job.handle.is_some() && job.outcome.is_none(), "query did not settle the worker");
        assert!(!cancel.load(Ordering::SeqCst));
    }
}
