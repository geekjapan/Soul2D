//! Rolling autosave backups and crash-recovery marker, ported from D `io/autosave.d`
//! and `windows/autosave.d`. No clock is read here: callers pass [`Now`].
//!
//! Layout: `<config>/autosaves/<stem>-<crc32 of the resolved path>/<stem>_<unix secs>.<ext>` plus an empty
//! `soul2d-project.lock` that exists from the first autosave until a clean exit or manual save.

use crate::Session;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const LOCKFILE: &str = "soul2d-project.lock";
const EXTENSIONS: [&str; 3] = ["inx", "cmo3", "can3"];

/// User settings. Construct with [`Settings::new`] so D's clamps apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub max_backups: u32,
}

impl Settings {
    /// D clamps the interval to 1..=1440 minutes and the file limit to 1..=1000.
    pub fn new(enabled: bool, interval_minutes: u32, max_backups: u32) -> Self {
        Self {
            enabled,
            interval_minutes: interval_minutes.clamp(1, 1440),
            max_backups: max_backups.clamp(1, 1000),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new(true, 5, 3)
    }
}

/// Caller-supplied time. `mono_secs` drives the interval; `unix_secs` names backup files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub mono_secs: u64,
    pub unix_secs: u64,
}

/// What the timer needs to know about the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocState {
    pub revision: u64,
    pub depth_revision: u64,
    pub modified: bool,
}

impl DocState {
    /// Read `revision`/`modified` from a session snapshot the frame already holds
    /// (`Session::snapshot` rebuilds the whole model JSON, so do not call it just for this).
    pub fn from_snapshot(snapshot: &Value) -> Result<Self, String> {
        Ok(Self {
            revision: snapshot["revision"].as_u64().ok_or("snapshot has no revision")?,
            depth_revision: snapshot["depthDraw"]["revision"].as_u64().ok_or("snapshot has no DepthDraw revision")?,
            modified: snapshot["modified"].as_bool().ok_or("snapshot has no modified flag")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub stamp: u64,
}

pub struct Autosave {
    config_dir: PathBuf,
    project: Option<PathBuf>,
    timer_start: u64,
    last_revision: Option<(u64, u64)>,
}

impl Autosave {
    pub fn new(config_dir: impl Into<PathBuf>, now: Now) -> Self {
        Self { config_dir: config_dir.into(), project: None, timer_start: now.mono_secs, last_revision: None }
    }

    /// Call when a project is created (`None`) or opened/saved-as (`Some(main file)`).
    /// Mirrors D `incNewProject`: the previous project's crash marker is released and the
    /// timer restarts.
    pub fn set_project(&mut self, project: Option<PathBuf>, now: Now) {
        self.release_lock();
        self.project = project;
        self.timer_start = now.mono_secs;
        self.last_revision = None;
    }

    /// True when D's `incCheckAutosave` would fire (plus the modified/revision gate the brief adds).
    pub fn is_due(&self, settings: &Settings, doc: DocState, now: Now) -> bool {
        self.project.is_some()
            && settings.enabled
            && doc.modified
            && self.last_revision != Some((doc.revision, doc.depth_revision))
            && now.mono_secs.saturating_sub(self.timer_start) / 60 >= u64::from(settings.interval_minutes)
    }

    /// Call once per frame. Returns the backup path when one was written. The timer restarts
    /// after an attempt even if it failed, so a broken disk is retried once per interval, not per frame.
    pub fn tick(
        &mut self,
        settings: &Settings,
        doc: DocState,
        now: Now,
        write: impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        if !self.is_due(settings, doc, now) {
            return Ok(None);
        }
        let result = self.autosave_now(settings, now, write);
        self.timer_start = now.mono_secs;
        if result.is_ok() {
            self.last_revision = Some((doc.revision, doc.depth_revision));
        }
        result.map(Some)
    }

    /// D `incAutosaveProject`: write a new backup, drop the oldest ones, then raise the crash marker.
    pub fn autosave_now(
        &mut self,
        settings: &Settings,
        now: Now,
        mut write: impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        let project = self.project.clone().ok_or("no project path to autosave")?;
        let dir = autosave_dir(&self.config_dir, &project)?;
        fs::create_dir_all(&dir).map_err(|e| format!("could not create autosave directory: {e}"))?;
        fs::write(dir.join(OWNER_FILE), resolved_project(&project).as_os_str().as_encoded_bytes())
            .map_err(|e| format!("could not record the autosave owner: {e}"))?;
        record_identity(&dir, &project);
        let mut backups = list_in(&dir, &project)?; // newest first
        // Strictly newer than every kept backup, so order survives a clock that went backwards
        // and two saves in one second never collide.
        let mut stamp = backups.first().map_or(now.unix_secs, |b| now.unix_secs.max(b.stamp.saturating_add(1)));
        let stem = stem_of(&project)?;
        let ext = extension_of(&project);
        let mut path = dir.join(format!("{stem}_{stamp}.{ext}"));
        while path.exists() {
            stamp += 1;
            path = dir.join(format!("{stem}_{stamp}.{ext}"));
        }
        write(&path)?;
        // The marker goes up as soon as the backup exists: a failed prune below must not hide it.
        fs::write(dir.join(LOCKFILE), b"").map_err(|e| format!("could not create autosave lock: {e}"))?;
        // Prune only after the new backup exists, so a failed write never costs a recovery point.
        while backups.len() + 1 > (settings.max_backups as usize).max(1) {
            let oldest = backups.pop().ok_or("autosave rotation underflow")?;
            fs::remove_file(&oldest.path).map_err(|e| format!("could not remove old autosave: {e}"))?;
        }
        Ok(path)
    }

    /// D `incReleaseLockfile`. Call on clean exit and after every successful manual save.
    pub fn release_lock(&self) {
        if let Some(project) = &self.project {
            if let Ok(dir) = autosave_dir(&self.config_dir, project) {
                let _ = fs::remove_file(dir.join(LOCKFILE));
                // A save replaces the project file; keep the recorded identity pointing at the new one.
                if dir.is_dir() {
                    record_identity(&dir, project);
                }
            }
        }
    }
}

/// D `incCheckLockfile`: true when a previous session left its marker, i.e. exited uncleanly
/// after autosaving. Check before opening `project` and show the restore/discard prompt if so.
pub fn unclean_exit(config_dir: &Path, project: &Path) -> bool {
    adopt_moved(config_dir, project);
    autosave_dir(config_dir, project).is_ok_and(|dir| dir.join(LOCKFILE).exists())
}

/// Recoverable backups of `project`, newest first.
pub fn list_backups(config_dir: &Path, project: &Path) -> Result<Vec<Backup>, String> {
    adopt_moved(config_dir, project);
    let dir = autosave_dir(config_dir, project)?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    list_in(&dir, project)
}

/// Dialog "Restore": open the newest backup in `session` under the project's path (`open` with `as`), so
/// the document reads as modified and `save` writes over `project`. The project file is only read here.
pub fn restore(session: &mut Session, config_dir: &Path, project: &Path) -> Result<Backup, String> {
    let newest = list_backups(config_dir, project)?
        .into_iter()
        .next()
        .ok_or("no autosave backup to restore")?;
    let path = newest.path.to_str().ok_or("backup path is not valid UTF-8")?;
    let main = project.to_str().ok_or("project path is not valid UTF-8")?;
    let response = session.handle_request(&json!({"op": "open", "path": path, "as": main}));
    if response["succeeded"] != true {
        return Err(response["message"].as_str().unwrap_or("could not open backup").to_owned());
    }
    Ok(newest)
}

/// Dialog "Discard": delete this project's backups and its crash marker. The project file is untouched.
/// Returns how many backups were deleted.
pub fn discard(config_dir: &Path, project: &Path) -> Result<usize, String> {
    let backups = list_backups(config_dir, project)?;
    for backup in &backups {
        fs::remove_file(&backup.path).map_err(|e| format!("could not delete autosave: {e}"))?;
    }
    let _ = fs::remove_file(autosave_dir(config_dir, project)?.join(LOCKFILE));
    Ok(backups.len())
}

/// `write` callback for a real session: saves to `path` with `mark_saved: false`, which leaves the
/// document's file path, baseline and modified state alone.
pub fn save_via_session(session: &mut Session, path: &Path) -> Result<(), String> {
    let path = path.to_str().ok_or("backup path is not valid UTF-8")?;
    let response = session.handle_request(&json!({"op": "save", "path": path, "mark_saved": false}));
    if response["succeeded"] == true {
        Ok(())
    } else {
        Err(response["message"].as_str().unwrap_or("autosave failed").to_owned())
    }
}

fn stem_of(project: &Path) -> Result<String, String> {
    project
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .ok_or_else(|| "project path must name a file".to_owned())
}

fn extension_of(project: &Path) -> &'static str {
    let ext = project.extension().and_then(|e| e.to_str()).unwrap_or("");
    EXTENSIONS.into_iter().find(|known| known.eq_ignore_ascii_case(ext)).unwrap_or("inx")
}

const OWNER_FILE: &str = "soul2d-project.path";

/// One spelling for one project: the real path when the file exists (so case and symlink aliases agree),
/// otherwise the resolved directory plus the given name (the file may be gone when a backup is restored).
fn resolved_project(project: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(project) {
        return real;
    }
    let parent = project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    match (fs::canonicalize(parent), project.file_name()) {
        (Ok(parent), Some(name)) => parent.join(name),
        _ => project.to_path_buf(),
    }
}

/// The backup directory of one project. D keyed it by the file stem alone, so two projects with the same
/// file name shared backups and crash markers. The checksum of the full path keeps them apart, and the
/// owner file written beside the backups settles checksum collisions: a directory owned by another path is
/// skipped and the next numbered one is used.
pub fn autosave_dir(config_dir: &Path, project: &Path) -> Result<PathBuf, String> {
    let resolved = resolved_project(project);
    let owner = resolved.as_os_str().as_encoded_bytes();
    let base = format!("{}-{:08x}", stem_of(&resolved)?, crc32fast::hash(owner));
    let root = config_dir.join("autosaves");
    // ponytail: linear probe; collisions need two paths with one stem and one CRC32, so one step is the norm.
    for attempt in 0..64u32 {
        let dir = if attempt == 0 { root.join(&base) } else { root.join(format!("{base}-{attempt}")) };
        match fs::read(dir.join(OWNER_FILE)) {
            Ok(recorded) if recorded != owner => continue,
            _ => return Ok(dir),
        }
    }
    Err("could not find a free autosave directory".into())
}

const IDENTITY_FILE: &str = "soul2d-project.id";

/// What stays the same when a file is moved or renamed on one filesystem and differs for any other file:
/// device, inode, length and modification time.
// ponytail: Unix only; std has no stable file id on Windows, so a moved project is not followed there.
fn file_identity(project: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(project).ok().filter(|meta| meta.is_file())?;
        Some(format!("{}:{}:{}:{}.{}", meta.dev(), meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec()))
    }
    #[cfg(not(unix))]
    {
        let _ = project;
        None
    }
}

fn record_identity(dir: &Path, project: &Path) {
    if let Some(identity) = file_identity(project) {
        let _ = fs::write(dir.join(IDENTITY_FILE), identity);
    }
}

/// A project moved or renamed outside the editor resolves to a new directory key. When that directory does
/// not exist yet, take over the directory that recorded this very file under a path that is now gone, so
/// its backups and crash marker follow the project. A copy, or another file with the same name, never
/// matches.
fn adopt_moved(config_dir: &Path, project: &Path) {
    let (Ok(own), Some(identity)) = (autosave_dir(config_dir, project), file_identity(project)) else { return };
    if own.exists() {
        return;
    }
    let resolved = resolved_project(project);
    for entry in fs::read_dir(config_dir.join("autosaves")).into_iter().flatten().flatten() {
        let dir = entry.path();
        let recorded = fs::read(dir.join(IDENTITY_FILE)).ok();
        let owner_gone = fs::read(dir.join(OWNER_FILE))
            .ok()
            .and_then(|owner| String::from_utf8(owner).ok())
            .is_some_and(|owner| !Path::new(&owner).exists());
        if recorded.as_deref() == Some(identity.as_bytes()) && owner_gone && fs::rename(&dir, &own).is_ok() {
            let _ = fs::write(own.join(OWNER_FILE), resolved.as_os_str().as_encoded_bytes());
            return;
        }
    }
}

/// Files `<anything>_<digits>.<known ext>` in `dir`, newest first. `project` itself is never listed,
/// so rotation cannot delete the user's file even if it lives in the autosave directory.
fn list_in(dir: &Path, project: &Path) -> Result<Vec<Backup>, String> {
    let mut backups = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("could not read autosave directory: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
        ) else {
            continue;
        };
        let Some(stamp) = stem.rsplit_once('_').and_then(|(_, digits)| digits.parse::<u64>().ok()) else {
            continue;
        };
        if EXTENSIONS.contains(&ext) && path.is_file() && path != project {
            backups.push(Backup { path, stamp });
        }
    }
    backups.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| b.path.cmp(&a.path)));
    Ok(backups)
}

// ------------------------------------------------------------------------------------------ recent autosaves

use crate::settings::Settings as Store;

const PREV_AUTOSAVES: &str = "prev_autosaves";
const PREV_MAINPATHS: &str = "prev_autosave_mainpaths";
const PREV_LIMIT: usize = 10;

/// D `AutosaveRecord`: a backup file and the project it belongs to (empty when unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutosaveRecord {
    pub autosave: String,
    pub mainsave: String,
}

/// D `incGetPrevAutosaves`: the two parallel settings lists; a missing main path reads as empty.
pub fn prev_autosaves(store: &Store) -> Vec<AutosaveRecord> {
    let autosaves = store.get::<Vec<String>>(PREV_AUTOSAVES).unwrap_or_default();
    let mains = store.get::<Vec<String>>(PREV_MAINPATHS).unwrap_or_default();
    autosaves
        .into_iter()
        .enumerate()
        .map(|(i, autosave)| AutosaveRecord { autosave, mainsave: mains.get(i).cloned().unwrap_or_default() })
        .collect()
}

fn store_records(store: &mut Store, records: &[AutosaveRecord]) -> Result<(), String> {
    store.set(PREV_AUTOSAVES, records.iter().map(|r| r.autosave.clone()).collect::<Vec<_>>())?;
    store.set(PREV_MAINPATHS, records.iter().map(|r| r.mainsave.clone()).collect::<Vec<_>>())
}

/// D `incAddPrevAutosave`: the backup moves to the front (an earlier record of the same file is dropped) and
/// ten records are kept.
pub fn add_prev_autosave(store: &mut Store, autosave: &str, mainsave: &str) -> Result<(), String> {
    let mut records = prev_autosaves(store);
    records.retain(|record| record.autosave != autosave);
    records.insert(0, AutosaveRecord { autosave: autosave.to_owned(), mainsave: mainsave.to_owned() });
    records.truncate(PREV_LIMIT);
    store_records(store, &records)
}

/// D `incPruneAutosaveList`: records whose backup file is gone are dropped. Returns how many.
pub fn prune_prev_autosaves(store: &mut Store) -> Result<usize, String> {
    let records = prev_autosaves(store);
    let kept: Vec<AutosaveRecord> = records.iter().filter(|record| Path::new(&record.autosave).exists()).cloned().collect();
    let dropped = records.len() - kept.len();
    if dropped > 0 || store.contains(PREV_AUTOSAVES) {
        store_records(store, &kept)?;
    }
    Ok(dropped)
}

/// The `open` request of the File > Recent > Autosaves entry (D `incOpenProject(main, backup)`): the backup's
/// content under the project's path, so Save writes the project. A record without a project opens the
/// backup as an untitled document.
pub fn open_record_request(record: &AutosaveRecord) -> Value {
    if record.mainsave.is_empty() {
        json!({"op": "open", "path": record.autosave, "untitled": true})
    } else {
        json!({"op": "open", "path": record.autosave, "as": record.mainsave})
    }
}

// ------------------------------------------------------------------------------------------ untitled recovery

const RECOVERY_DIR: &str = "RustRecovery";
const RECOVERY_PREFIX: &str = "rust-recovery-";
/// D writes a snapshot at most every 30 seconds while the document is modified.
pub const RECOVERY_INTERVAL_SECS: u64 = 30;

/// D `isRecoveryPath`: `rust-recovery-<something>.inx|.cmo3|.can3`.
pub fn is_recovery_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    name.len() > RECOVERY_PREFIX.len() + ext.len() + 1 && name.starts_with(RECOVERY_PREFIX) && EXTENSIONS.contains(&ext)
}

pub fn recovery_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(RECOVERY_DIR)
}

/// Snapshots left behind by earlier sessions, newest name first (D sorts the paths descending). Only regular
/// files count: a symbolic link with a snapshot's name would make a restore open whatever it points to.
pub fn list_recovery(config_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = recovery_dir(config_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("could not read the recovery directory: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) && is_recovery_path(&path) {
            files.push(path);
        }
    }
    files.sort_by(|a, b| b.cmp(a));
    Ok(files)
}

/// The snapshot of `listing` whose file name is `id`. An id is only ever compared with the names in the
/// listing, so no caller text becomes a path.
pub fn recovery_named<'a>(listing: &'a [PathBuf], id: &str) -> Result<&'a Path, String> {
    let named = |path: &&PathBuf| path.file_name().and_then(|name| name.to_str()) == Some(id);
    listing.iter().find(named).map(PathBuf::as_path).ok_or_else(|| format!("there is no recovery snapshot {id:?}"))
}

/// Deletes one snapshot; refuses anything that is not a recovery file inside the recovery directory.
pub fn delete_recovery(config_dir: &Path, path: &Path) -> Result<(), String> {
    if !is_recovery_path(path) || path.parent() != Some(recovery_dir(config_dir).as_path()) {
        return Err("not a recovery snapshot".into());
    }
    fs::remove_file(path).map_err(|e| format!("could not delete {}: {e}", path.display()))
}

/// The rolling crash snapshot of a document that has no path yet (D `autosaveRecovery`). Documents with a
/// path use [`Autosave`] instead. One snapshot is kept: each new one replaces the previous.
pub struct Recovery {
    dir: PathBuf,
    current: Option<PathBuf>,
    last_attempt: u64,
}

impl Recovery {
    pub fn new(config_dir: &Path, now: Now) -> Self {
        Self { dir: recovery_dir(config_dir), current: None, last_attempt: now.mono_secs }
    }

    /// True when a snapshot is due: the document is modified and 30 seconds passed since the last attempt.
    pub fn is_due(&self, modified: bool, now: Now) -> bool {
        modified && now.mono_secs.saturating_sub(self.last_attempt) >= RECOVERY_INTERVAL_SECS
    }

    /// Writes a snapshot with `write` when one is due. The attempt time moves even when writing fails.
    pub fn tick(
        &mut self,
        modified: bool,
        extension: &str,
        process_id: u32,
        now: Now,
        write: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        if !self.is_due(modified, now) {
            return Ok(None);
        }
        self.last_attempt = now.mono_secs;
        if !EXTENSIONS.contains(&extension) {
            return Err(format!("no recovery format for .{extension}"));
        }
        fs::create_dir_all(&self.dir).map_err(|e| format!("Recovery snapshots unavailable: {e}"))?;
        let stem = format!("{RECOVERY_PREFIX}{}-{process_id}", u128::from(now.unix_secs) * 10_000_000);
        let mut path = self.dir.join(format!("{stem}.{extension}"));
        let mut suffix = 1;
        while path.exists() {
            path = self.dir.join(format!("{stem}-{suffix}.{extension}"));
            suffix += 1;
        }
        write(&path).map_err(|e| format!("Recovery save failed: {e}"))?;
        if let Some(previous) = self.current.replace(path.clone()) {
            let _ = fs::remove_file(previous);
        }
        Ok(Some(path))
    }

    /// The document was saved or closed cleanly: this session's snapshot is no longer needed.
    pub fn release(&mut self, now: Now) {
        if let Some(path) = self.current.take() {
            let _ = fs::remove_file(path);
        }
        self.last_attempt = now.mono_secs;
    }

    /// The document was replaced: stop tracking the snapshot (it stays on disk, as in D) and restart the timer.
    pub fn forget(&mut self, now: Now) {
        self.current = None;
        self.last_attempt = now.mono_secs;
    }

    /// A restored snapshot becomes this session's snapshot: the next one replaces it and a save deletes it.
    pub fn adopt(&mut self, path: PathBuf) {
        if is_recovery_path(&path) && path.parent() == Some(self.dir.as_path()) {
            self.current = Some(path);
        }
    }

    pub fn current(&self) -> Option<&Path> {
        self.current.as_deref()
    }
}

/// `open` request that restores a snapshot as an untitled, modified document.
pub fn recovery_open_request(path: &Path) -> Result<Value, String> {
    let text = path.to_str().ok_or("recovery path is not valid UTF-8")?;
    Ok(json!({"op": "open", "path": text, "untitled": true}))
}
